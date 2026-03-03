use crate::pty::Pty;
use crate::screen::Screen;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Check if a PTY child process is still alive.
/// Uses `try_lock()` to avoid blocking Tokio workers when called from async tasks.
pub fn is_child_alive(child: &Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>) -> bool {
    match child.try_lock() {
        Ok(mut c) => c.try_wait().ok().flatten().is_none(),
        Err(std::sync::TryLockError::WouldBlock) => true, // assume alive if contended
        Err(std::sync::TryLockError::Poisoned(e)) => {
            tracing::warn!(error = %e, "child mutex poisoned in is_alive");
            false
        }
    }
}

/// A single terminal session backed by a PTY and a virtual screen.
pub struct Session {
    pub name: String,
    pub pty: Pty,
    pub screen: Arc<Mutex<Screen>>,
    pub dims: Arc<Mutex<(u16, u16)>>,
    /// When a client is attached, holds the sender side of a watch channel.
    /// Sending `false` evicts the active client. Replaced on each new attach.
    pub evict_tx: Option<tokio::sync::watch::Sender<bool>>,
    /// Wakes the client relay when new PTY data has been processed.
    pub screen_notify: Arc<tokio::sync::Notify>,
    /// Whether a client is currently connected (used by reader to decide draining).
    pub has_client: Arc<AtomicBool>,
    /// Set to false when the persistent reader thread detects PTY EOF.
    pub reader_alive: Arc<AtomicBool>,
    /// Handle for the persistent PTY reader thread (joined on Drop).
    reader_handle: Option<std::thread::JoinHandle<()>>,
}

impl Session {
    /// Create a new session, spawning a shell in a PTY of the given size.
    pub fn new(name: String, cols: u16, rows: u16, history: usize) -> anyhow::Result<Self> {
        let pty = Pty::spawn(cols, rows)?;
        let screen = Arc::new(Mutex::new(Screen::new(cols, rows, history)));
        let dims = Arc::new(Mutex::new((cols, rows)));
        let screen_notify = Arc::new(tokio::sync::Notify::new());
        let has_client = Arc::new(AtomicBool::new(false));
        let reader_alive = Arc::new(AtomicBool::new(true));

        // Spawn the persistent PTY reader thread.
        let pty_reader = pty.clone_reader()?;
        let pty_writer = pty.writer.clone();
        let reader_handle = {
            let screen = screen.clone();
            let notify = screen_notify.clone();
            let has_client = has_client.clone();
            let reader_alive = reader_alive.clone();
            let thread_name = format!("pty-reader-{}", name);
            std::thread::Builder::new()
                .name(thread_name)
                .spawn(move || {
                    persistent_reader_loop(
                        pty_reader, screen, pty_writer, notify, has_client, reader_alive,
                    );
                })?
        };

        Ok(Self {
            name, pty, screen, dims, evict_tx: None,
            screen_notify, has_client, reader_alive,
            reader_handle: Some(reader_handle),
        })
    }

    /// Check if the session's child process is still alive.
    pub fn is_alive(&self) -> bool {
        is_child_alive(&self.pty.child_arc())
    }

    /// Get the child process PID (if available).
    /// Uses `try_lock()` to avoid blocking; returns `None` on contention or poison.
    pub fn child_pid(&self) -> Option<u32> {
        self.pty.child_arc().try_lock().ok().and_then(|c| c.process_id())
    }
}

/// Persistent PTY reader loop, runs for the entire session lifetime.
/// Reads PTY output, feeds it through the screen's VTE parser, and notifies
/// any connected client of new data.
fn persistent_reader_loop(
    mut reader: Box<dyn Read + Send>,
    screen: Arc<Mutex<Screen>>,
    pty_writer: Arc<Mutex<Box<dyn Write + Send>>>,
    notify: Arc<tokio::sync::Notify>,
    has_client: Arc<AtomicBool>,
    reader_alive: Arc<AtomicBool>,
) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                tracing::debug!("persistent pty reader: EOF");
                break;
            }
            Ok(n) => {
                let responses = {
                    let mut scr = match screen.lock() {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(error = %e, "screen mutex poisoned in reader loop");
                            break;
                        }
                    };
                    scr.process(&buf[..n]);
                    let responses = scr.take_responses();
                    // When no client is connected, drain pending data to prevent
                    // unbounded growth. The data is already in the main scrollback.
                    if !has_client.load(Ordering::Acquire) {
                        let _ = scr.take_pending_scrollback();
                        let _ = scr.take_passthrough();
                    }
                    responses
                };

                // Write PTY responses (DA, DSR replies) outside the screen lock.
                if !responses.is_empty() {
                    if let Ok(mut w) = pty_writer.lock() {
                        for response in &responses {
                            if let Err(e) = w.write_all(response) {
                                tracing::warn!(error = %e, "failed to write response to PTY in reader loop");
                                break;
                            }
                        }
                        let _ = w.flush();
                    }
                }

                notify.notify_one();
            }
            Err(e) => {
                tracing::debug!(error = %e, "persistent pty reader: read error");
                break;
            }
        }
    }
    reader_alive.store(false, Ordering::Release);
    notify.notify_one(); // wake client to detect reader death
}

impl Drop for Session {
    fn drop(&mut self) {
        // Use lock() (blocking) — callers must ensure Session is dropped on
        // spawn_blocking or outside the Tokio runtime to avoid blocking workers.
        if let Ok(mut child) = self.pty.child_arc().lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // Evict any connected client
        if let Some(tx) = self.evict_tx.take() {
            let _ = tx.send(false);
        }
        // Wait for the reader thread to exit (it will see EOF after child kill).
        if let Some(handle) = self.reader_handle.take() {
            let _ = handle.join();
        }
    }
}

/// Registry of named sessions with create, lookup, and cleanup operations.
pub struct SessionManager {
    sessions: HashMap<String, Session>,
}

impl SessionManager {
    /// Create an empty session manager.
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
        }
    }

    /// Create a new session with the given name, failing if it already exists.
    pub fn create(&mut self, name: String, cols: u16, rows: u16, history: usize) -> anyhow::Result<()> {
        if self.sessions.contains_key(&name) {
            anyhow::bail!("session '{}' already exists", name);
        }
        let session = Session::new(name.clone(), cols, rows, history)?;
        self.sessions.insert(name, session);
        Ok(())
    }

    /// Get existing session or create a new one.
    /// Returns (session, is_new).
    pub fn get_or_create(&mut self, name: &str, cols: u16, rows: u16, history: usize) -> anyhow::Result<(&mut Session, bool)> {
        let is_new = if !self.sessions.contains_key(name) {
            let c = if cols > 0 { cols } else { 80 };
            let r = if rows > 0 { rows } else { 24 };
            tracing::debug!(session = %name, cols = c, rows = r, "creating new session");
            self.create(name.to_string(), c, r, history)?;
            true
        } else {
            tracing::debug!(session = %name, "reattaching to existing session");
            false
        };
        Ok((self.sessions.get_mut(name).unwrap(), is_new))
    }

    /// Get an existing session by name.
    pub fn get(&mut self, name: &str) -> Option<&mut Session> {
        self.sessions.get_mut(name)
    }

    /// Remove and return a session by name, or `None` if not found.
    pub fn remove(&mut self, name: &str) -> Option<Session> {
        self.sessions.remove(name)
    }

    /// Return metadata for all active sessions.
    pub fn list(&self) -> Vec<crate::protocol::SessionInfo> {
        self.sessions.values().map(|s| {
            let (cols, rows) = match s.dims.lock() {
                Ok(d) => *d,
                Err(e) => {
                    tracing::warn!(session = %s.name, error = %e, "dims mutex poisoned in list");
                    (80, 24)
                }
            };
            crate::protocol::SessionInfo {
                name: s.name.clone(),
                pid: s.child_pid().unwrap_or(0),
                cols,
                rows,
            }
        }).collect()
    }

    /// Remove dead sessions and return them for cleanup outside the lock.
    pub fn take_dead_sessions(&mut self) -> Vec<Session> {
        let dead: Vec<String> = self.sessions.iter()
            .filter(|(_, s)| !s.is_alive())
            .map(|(name, s)| {
                let status = s.pty.child_arc().lock().ok()
                    .and_then(|mut c| c.try_wait().ok().flatten());
                tracing::info!(
                    session = %name,
                    exit_status = ?status,
                    "cleaning up dead session"
                );
                name.clone()
            })
            .collect();
        dead.into_iter().filter_map(|name| self.sessions.remove(&name)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::Ordering;

    /// Helper: collect visible grid rows as trimmed strings.
    fn screen_lines(screen: &crate::screen::Screen) -> Vec<String> {
        screen.grid.cells.iter().map(|row| {
            let s: String = row.iter().map(|c| c.c).collect();
            s.trim_end().to_string()
        }).collect()
    }

    /// Helper: collect scrollback history as plain text (ANSI stripped).
    fn history_texts(screen: &crate::screen::Screen) -> Vec<String> {
        screen.get_history().iter().map(|b| {
            let s = String::from_utf8_lossy(b);
            let mut out = String::new();
            let mut in_esc = false;
            for ch in s.chars() {
                if in_esc {
                    if ch.is_ascii_alphabetic() || ch == 'm' { in_esc = false; }
                    continue;
                }
                if ch == '\x1b' { in_esc = true; continue; }
                if ch >= ' ' { out.push(ch); }
            }
            out.trim_end().to_string()
        }).collect()
    }

    /// Poll the screen until a predicate is satisfied or timeout expires.
    fn wait_for_screen(
        screen: &Arc<Mutex<crate::screen::Screen>>,
        timeout: std::time::Duration,
        pred: impl Fn(&crate::screen::Screen) -> bool,
    ) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if let Ok(scr) = screen.lock() {
                if pred(&scr) { return true; }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }

    /// Persistent reader processes PTY output while no client is connected.
    ///
    /// Simulates: client opens session running `sleep 2 && echo MARKER`,
    /// disconnects immediately, reconnects after the command completes,
    /// and finds MARKER in the screen or scrollback.
    #[test]
    fn persistent_reader_captures_output_without_client() {
        let session = Session::new("test-persistent".into(), 80, 24, 1000).unwrap();

        // No client connected — persistent reader is running with has_client=false.
        assert!(!session.has_client.load(Ordering::Acquire));
        assert!(session.reader_alive.load(Ordering::Acquire));

        // Write a command that produces output after a short delay.
        // Use a unique marker so we can find it unambiguously.
        {
            let mut w = session.pty.writer.lock().unwrap();
            w.write_all(b"sleep 1 && echo PERSISTENT_READER_OK\n").unwrap();
            w.flush().unwrap();
        }

        // Wait for the marker to appear in the screen (up to 5s).
        let found = wait_for_screen(&session.screen, std::time::Duration::from_secs(5), |scr| {
            let lines = screen_lines(scr);
            let hist = history_texts(scr);
            lines.iter().chain(hist.iter()).any(|l| l.contains("PERSISTENT_READER_OK"))
        });

        assert!(found, "persistent reader should capture PTY output even with no client connected");

        // Reader should still be alive (shell is still running).
        assert!(session.reader_alive.load(Ordering::Acquire));
    }

    /// After the child process exits, a reconnecting client sees the final
    /// output and reader_alive is false.
    #[test]
    fn persistent_reader_detects_child_exit() {
        let session = Session::new("test-exit".into(), 80, 24, 1000).unwrap();

        // Tell the shell to print a marker and exit.
        {
            let mut w = session.pty.writer.lock().unwrap();
            w.write_all(b"echo GOODBYE && exit\n").unwrap();
            w.flush().unwrap();
        }

        // Wait for reader_alive to become false (child exited, PTY EOF).
        let exited = wait_for_screen(&session.screen, std::time::Duration::from_secs(5), |_| {
            !session.reader_alive.load(Ordering::Acquire)
        });
        assert!(exited, "reader_alive should become false after child exits");

        // The marker should be visible in the screen or scrollback.
        let scr = session.screen.lock().unwrap();
        let lines = screen_lines(&scr);
        let hist = history_texts(&scr);
        let found = lines.iter().chain(hist.iter()).any(|l| l.contains("GOODBYE"));
        assert!(found, "final output should be captured before reader exits");
    }

    #[test]
    fn session_manager_create_and_list() {
        let mut mgr = SessionManager::new();
        mgr.create("test1".into(), 80, 24, 1000).unwrap();
        let list = mgr.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "test1");
    }

    #[test]
    fn session_manager_duplicate_create_fails() {
        let mut mgr = SessionManager::new();
        mgr.create("test".into(), 80, 24, 1000).unwrap();
        assert!(mgr.create("test".into(), 80, 24, 1000).is_err());
    }

    #[test]
    fn session_manager_get_or_create() {
        let mut mgr = SessionManager::new();
        let (session, is_new) = mgr.get_or_create("test", 80, 24, 1000).unwrap();
        assert_eq!(session.name, "test");
        assert!(is_new);
        // Should return existing session
        let (session, is_new) = mgr.get_or_create("test", 80, 24, 1000).unwrap();
        assert_eq!(session.name, "test");
        assert!(!is_new);
        assert_eq!(mgr.list().len(), 1);
    }

    #[test]
    fn session_manager_remove() {
        let mut mgr = SessionManager::new();
        mgr.create("test".into(), 80, 24, 1000).unwrap();
        assert!(mgr.remove("test").is_some());
        assert!(mgr.remove("test").is_none());
        assert_eq!(mgr.list().len(), 0);
    }

    #[test]
    fn session_manager_get_or_create_zero_dimensions() {
        let mut mgr = SessionManager::new();
        let (session, is_new) = mgr.get_or_create("test", 0, 0, 1000).unwrap();
        // Should clamp to 80x24 defaults
        let (cols, rows) = *session.dims.lock().unwrap();
        assert_eq!(cols, 80);
        assert_eq!(rows, 24);
        assert!(is_new);
    }
}
