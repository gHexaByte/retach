use crate::pty::Pty;
use retach::screen::{Screen, TerminalEmulator, TerminalSize};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Default terminal dimensions when actual size is unavailable.
pub const DEFAULT_COLS: u16 = 80;
pub const DEFAULT_ROWS: u16 = 24;

/// RAII guard that clears `has_client` flag when dropped, unless evicted.
///
/// When a new client evicts the current one, it sends `false` on the eviction
/// channel. The guard checks this on Drop: if evicted, it skips clearing
/// has_client (the new client already set it to true).
pub struct ClientGuard {
    has_client: Arc<AtomicBool>,
    evict_rx: tokio::sync::watch::Receiver<bool>,
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        // evict_rx initial value is `true` (not evicted).
        // Eviction sends `false`.
        if *self.evict_rx.borrow() {
            self.has_client.store(false, Ordering::Release);
        }
    }
}

/// Session names are displayed in CLI output and stored as filesystem-friendly identifiers.
/// 128 bytes is generous for human-readable names while preventing abuse.
const MAX_SESSION_NAME_LEN: usize = 128;

/// PTY read buffer size. 4 KiB matches the typical pipe buffer granularity on
/// Linux/macOS and balances syscall overhead against latency.
const PTY_READ_BUF_SIZE: usize = 4096;

/// Maximum queued DA/DSR responses when pty_writer is contended. 64 is far
/// beyond normal usage (typically 1-2 responses in flight). Overflow means a
/// deadlock-like condition where the client holds pty_writer indefinitely.
const MAX_DEFERRED: usize = 64;

/// Shared screen handle.
pub type SharedScreen = Arc<Mutex<Screen>>;

/// Shared handles for the client relay tasks.
/// Created by `Session::connect()`.
#[derive(Clone)]
pub struct SessionHandles {
    pub screen: SharedScreen,
    pub pty_writer: crate::pty::SharedPtyWriter,
    pub master: crate::pty::SharedMasterPty,
    pub dims: Arc<Mutex<TerminalSize>>,
    pub screen_notify: Arc<tokio::sync::Notify>,
    pub reader_alive: Arc<AtomicBool>,
    pub name: String,
}

/// A single terminal session backed by a PTY and a virtual screen.
pub struct Session {
    pub(crate) name: String,
    pub(crate) pty: Pty,
    pub(crate) screen: SharedScreen,
    pub(crate) dims: Arc<Mutex<TerminalSize>>,
    /// When a client is attached, holds the sender side of a watch channel.
    /// Sending `false` evicts the active client. Replaced on each new attach.
    evict_tx: Option<tokio::sync::watch::Sender<bool>>,
    /// Wakes the client relay when new PTY data has been processed.
    screen_notify: Arc<tokio::sync::Notify>,
    /// Whether a client is currently connected (used by reader to decide draining).
    has_client: Arc<AtomicBool>,
    /// Set to false when the persistent reader thread detects PTY EOF.
    reader_alive: Arc<AtomicBool>,
    /// Handle for the persistent PTY reader thread (joined on Drop).
    reader_handle: Option<std::thread::JoinHandle<()>>,
}

impl Session {
    /// Create a new session, spawning a shell in a PTY of the given size.
    pub fn new(name: String, cols: u16, rows: u16, history: usize) -> anyhow::Result<Self> {
        let pty = Pty::spawn(cols, rows)?;
        let screen = Arc::new(Mutex::new(Screen::new(cols, rows, history)));
        let dims = Arc::new(Mutex::new(TerminalSize { cols, rows }));
        let screen_notify = Arc::new(tokio::sync::Notify::new());
        let has_client = Arc::new(AtomicBool::new(false));
        let reader_alive = Arc::new(AtomicBool::new(true));

        // Spawn the persistent PTY reader thread.
        let pty_reader = pty.clone_reader()?;
        let pty_writer = pty.writer_arc();
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

    /// Check if the session is still alive.
    /// A session is dead if the reader thread exited (PTY EOF or panic)
    /// or if the child process has terminated.
    pub fn is_alive(&self) -> bool {
        self.reader_alive.load(Ordering::Acquire) && self.pty.is_child_alive()
    }

    /// Get the child process PID (if available).
    /// Uses `try_lock()` to avoid blocking; returns `None` on contention or poison.
    pub fn child_pid(&self) -> Option<u32> {
        self.pty.child_arc().try_lock().ok().and_then(|c| c.process_id())
    }

    /// Mark a client as connected, evicting any previous client.
    ///
    /// Returns a `ClientGuard` (clears `has_client` on drop), shared handles
    /// for the relay tasks, and an eviction watch receiver.
    ///
    /// **Must be called under the SessionManager lock** to prevent races.
    pub fn connect(&mut self) -> (ClientGuard, SessionHandles, tokio::sync::watch::Receiver<bool>) {
        // Set has_client BEFORE evicting old client, so the persistent reader
        // doesn't discard data intended for the new client.
        self.has_client.store(true, Ordering::Release);

        // Evict previous client if any
        if let Some(old_tx) = self.evict_tx.take() {
            tracing::debug!(session = %self.name, "evicting previous client");
            if old_tx.send(false).is_err() {
                tracing::debug!(session = %self.name, "evict channel: previous client already disconnected");
            }
        }

        // Create new eviction channel for this client
        let (evict_tx, evict_rx) = tokio::sync::watch::channel(true);
        self.evict_tx = Some(evict_tx);

        let guard = ClientGuard {
            has_client: self.has_client.clone(),
            evict_rx: evict_rx.clone(),
        };

        let handles = SessionHandles {
            screen: self.screen.clone(),
            pty_writer: self.pty.writer_arc(),
            master: self.pty.master_arc(),
            dims: self.dims.clone(),
            screen_notify: self.screen_notify.clone(),
            reader_alive: self.reader_alive.clone(),
            name: self.name.clone(),
        };

        (guard, handles, evict_rx)
    }

    /// Disconnect the current client (used by KillSession).
    /// Drops evict_tx so the connected client sees RecvError.
    pub fn disconnect(&mut self) {
        drop(self.evict_tx.take());
    }

    /// Test accessor: whether a client is connected.
    #[cfg(test)]
    pub(crate) fn has_client(&self) -> bool {
        self.has_client.load(Ordering::Acquire)
    }

    /// Test accessor: whether the reader thread is alive.
    #[cfg(test)]
    pub(crate) fn reader_alive(&self) -> bool {
        self.reader_alive.load(Ordering::Acquire)
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
    let mut buf = [0u8; PTY_READ_BUF_SIZE];
    let mut deferred_responses: VecDeque<Vec<u8>> = VecDeque::new();
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                tracing::debug!("persistent pty reader: EOF");
                break;
            }
            Ok(n) => {
                let mut responses = {
                    let mut scr = match screen.lock() {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::error!(error = %e, "screen mutex poisoned in reader loop, terminating");
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

                // Prepend any deferred responses from previous iterations.
                if !deferred_responses.is_empty() {
                    let mut all: Vec<Vec<u8>> = deferred_responses.drain(..).collect();
                    all.append(&mut responses);
                    responses = all;
                }

                // Write PTY responses (DA, DSR replies) outside the screen lock.
                // Use try_lock to avoid deadlock: client_to_pty may hold
                // pty_writer during a blocking write_all while waiting for the
                // child to read — if the child is waiting for this DA response,
                // a blocking lock() here would deadlock.
                if !responses.is_empty() {
                    match pty_writer.try_lock() {
                        Ok(mut w) => {
                            for response in &responses {
                                if let Err(e) = w.write_all(response) {
                                    tracing::warn!(error = %e, "failed to write response to PTY in reader loop");
                                    break;
                                }
                            }
                            if let Err(e) = w.flush() {
                                tracing::warn!(error = %e, "failed to flush PTY writer in reader loop");
                            }
                        }
                        Err(_) => {
                            tracing::debug!("pty_writer contended, deferring {} DA/DSR response(s)", responses.len());
                            for resp in responses {
                                if deferred_responses.len() >= MAX_DEFERRED {
                                    tracing::warn!(queue_len = MAX_DEFERRED, "deferred DA/DSR response queue full, dropping oldest response");
                                    deferred_responses.pop_front();
                                }
                                deferred_responses.push_back(resp);
                            }
                        }
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
        // Use try_lock() to avoid blocking the async runtime if the child lock
        // is contended. In practice it's never contended during drop because
        // the persistent reader has already exited (or will exit after kill).
        match self.pty.child_arc().try_lock() {
            Ok(mut child) => {
                if let Err(e) = child.kill() {
                    tracing::debug!(error = %e, session = %self.name, "child already exited before kill");
                }
                if let Err(e) = child.wait() {
                    tracing::debug!(error = %e, session = %self.name, "child already reaped before wait");
                }
            }
            Err(_) => {
                tracing::warn!(session = %self.name, "child mutex contended during drop, skipping kill/wait — detaching reader thread");
                // Can't kill the child, so the PTY reader thread will block on read
                // forever. Detach it (take + drop the JoinHandle) instead of joining,
                // and skip eviction since the session is being abandoned.
                self.reader_handle.take();
                return;
            }
        }
        // Evict any connected client
        if let Some(tx) = self.evict_tx.take() {
            if tx.send(false).is_err() {
                tracing::debug!(session = %self.name, "evict channel: client already disconnected during drop");
            }
        }
        // Wait for the reader thread to exit (it will see EOF after child kill).
        if let Some(handle) = self.reader_handle.take() {
            if handle.join().is_err() {
                tracing::warn!(session = %self.name, "PTY reader thread panicked during join");
            }
        }
    }
}

/// Validate a session name: max 128 bytes, only `[a-zA-Z0-9_-.]`.
pub fn validate_session_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        anyhow::bail!("session name cannot be empty");
    }
    if name.len() > MAX_SESSION_NAME_LEN {
        anyhow::bail!("session name too long (max {} bytes)", MAX_SESSION_NAME_LEN);
    }
    if let Some(ch) = name.chars().find(|c| !matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' | '.')) {
        anyhow::bail!("invalid character '{}' in session name (allowed: a-zA-Z0-9_-.)", ch);
    }
    Ok(())
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
    /// Zero dimensions are clamped to defaults.
    pub fn create(&mut self, name: String, cols: u16, rows: u16, history: usize) -> anyhow::Result<()> {
        validate_session_name(&name)?;
        if self.sessions.contains_key(&name) {
            anyhow::bail!("session '{}' already exists", name);
        }
        let c = if cols > 0 { cols } else { DEFAULT_COLS };
        let r = if rows > 0 { rows } else { DEFAULT_ROWS };
        let session = Session::new(name.clone(), c, r, history)?;
        self.sessions.insert(name, session);
        Ok(())
    }

    /// Get existing session or create a new one.
    /// Returns (session, is_new).
    pub fn get_or_create(&mut self, name: &str, cols: u16, rows: u16, history: usize) -> anyhow::Result<(&mut Session, bool)> {
        validate_session_name(name)?;
        use std::collections::hash_map::Entry;
        match self.sessions.entry(name.to_string()) {
            Entry::Occupied(e) => {
                tracing::debug!(session = %name, "reattaching to existing session");
                Ok((e.into_mut(), false))
            }
            Entry::Vacant(e) => {
                let c = if cols > 0 { cols } else { DEFAULT_COLS };
                let r = if rows > 0 { rows } else { DEFAULT_ROWS };
                tracing::debug!(session = %name, cols = c, rows = r, "creating new session");
                let session = Session::new(name.to_string(), c, r, history)?;
                Ok((e.insert(session), true))
            }
        }
    }

    /// Get an existing session by name (read-only).
    pub fn get(&self, name: &str) -> Option<&Session> {
        self.sessions.get(name)
    }

    /// Get an existing session by name (mutable).
    pub fn get_mut(&mut self, name: &str) -> Option<&mut Session> {
        self.sessions.get_mut(name)
    }

    /// Remove and return a session by name, or `None` if not found.
    pub fn remove(&mut self, name: &str) -> Option<Session> {
        self.sessions.remove(name)
    }

    /// Return metadata for all active sessions.
    pub fn list(&self) -> Vec<crate::protocol::SessionInfo> {
        self.sessions.values().map(|s| {
            let dims = match s.dims.lock() {
                Ok(d) => *d,
                Err(e) => {
                    tracing::warn!(session = %s.name, error = %e, "dims mutex poisoned in list");
                    TerminalSize { cols: DEFAULT_COLS, rows: DEFAULT_ROWS }
                }
            };
            crate::protocol::SessionInfo {
                name: s.name.clone(),
                pid: s.child_pid().unwrap_or(0),
                cols: dims.cols,
                rows: dims.rows,
            }
        }).collect()
    }

    /// Remove and return all sessions (for graceful shutdown).
    pub fn drain_all(&mut self) -> Vec<Session> {
        self.sessions.drain().map(|(_, s)| s).collect()
    }

    /// Remove dead sessions and return them for cleanup outside the lock.
    pub fn take_dead_sessions(&mut self) -> Vec<Session> {
        let dead: Vec<String> = self.sessions.iter()
            .filter(|(_, s)| !s.is_alive())
            .map(|(name, s)| {
                // Use try_lock to avoid blocking a Tokio worker thread
                // (this is called from an async cleanup task).
                let status = s.pty.child_arc().try_lock().ok()
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
    fn screen_lines(screen: &retach::screen::Screen) -> Vec<String> {
        use retach::screen::TerminalEmulator;
        screen.visible_rows().map(|row| {
            let s: String = row.iter().map(|c| c.c).collect();
            s.trim_end().to_string()
        }).collect()
    }

    /// Helper: collect scrollback history as plain text (ANSI stripped).
    fn history_texts(screen: &retach::screen::Screen) -> Vec<String> {
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
        screen: &Arc<Mutex<retach::screen::Screen>>,
        timeout: std::time::Duration,
        pred: impl Fn(&retach::screen::Screen) -> bool,
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
        assert!(!session.has_client());
        assert!(session.reader_alive());

        // Write a command that produces output after a short delay.
        // Use a unique marker so we can find it unambiguously.
        {
            let writer = session.pty.writer_arc();
            let mut w = writer.lock().unwrap();
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
        assert!(session.reader_alive());
    }

    /// After the child process exits, a reconnecting client sees the final
    /// output and reader_alive is false.
    #[test]
    fn persistent_reader_detects_child_exit() {
        let session = Session::new("test-exit".into(), 80, 24, 1000).unwrap();

        // Tell the shell to print a marker and exit.
        {
            let writer = session.pty.writer_arc();
            let mut w = writer.lock().unwrap();
            w.write_all(b"echo GOODBYE && exit\n").unwrap();
            w.flush().unwrap();
        }

        // Wait for reader_alive to become false (child exited, PTY EOF).
        let exited = wait_for_screen(&session.screen, std::time::Duration::from_secs(5), |_| {
            !session.reader_alive()
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
    fn deferred_responses_bounded() {
        assert!(MAX_DEFERRED > 0 && MAX_DEFERRED <= 128);
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
        let dims = *session.dims.lock().unwrap();
        assert_eq!(dims.cols, 80);
        assert_eq!(dims.rows, 24);
        assert!(is_new);
    }

    #[test]
    fn validate_session_name_valid() {
        assert!(validate_session_name("my-session.1_OK").is_ok());
        assert!(validate_session_name("a").is_ok());
        assert!(validate_session_name(&"x".repeat(128)).is_ok());
    }

    #[test]
    fn validate_session_name_empty() {
        let err = validate_session_name("").unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn validate_session_name_too_long() {
        let err = validate_session_name(&"x".repeat(129)).unwrap_err();
        assert!(err.to_string().contains("too long"));
    }

    #[test]
    fn validate_session_name_invalid_chars() {
        assert!(validate_session_name("foo/bar").is_err());
        assert!(validate_session_name("foo bar").is_err());
        assert!(validate_session_name("foo\0bar").is_err());
        assert!(validate_session_name("../escape").is_err());
    }

    #[test]
    fn session_manager_rejects_invalid_names() {
        let mut mgr = SessionManager::new();
        assert!(mgr.create("bad/name".into(), 80, 24, 1000).is_err());
        assert!(mgr.get_or_create("bad name", 80, 24, 1000).is_err());
    }

    #[test]
    fn take_dead_sessions_returns_dead() {
        let mut mgr = SessionManager::new();
        mgr.create("alive".into(), 80, 24, 100).unwrap();
        mgr.create("doomed".into(), 80, 24, 100).unwrap();

        // Kill the doomed session's child process
        {
            let session = mgr.get_mut("doomed").unwrap();
            let child_arc = session.pty.child_arc();
            let mut child = child_arc.lock().unwrap();
            child.kill().ok();
            child.wait().ok();
        }

        // Give the reader thread a moment to detect EOF
        std::thread::sleep(std::time::Duration::from_millis(200));

        let dead = mgr.take_dead_sessions();
        let dead_names: Vec<&str> = dead.iter().map(|s| s.name.as_str()).collect();
        assert!(dead_names.contains(&"doomed"), "dead list should contain 'doomed': {:?}", dead_names);
        assert!(!dead_names.contains(&"alive"), "dead list should not contain 'alive': {:?}", dead_names);

        // Only 'alive' should remain
        assert_eq!(mgr.list().len(), 1);
        assert_eq!(mgr.list()[0].name, "alive");
    }

    #[test]
    fn take_dead_sessions_empty_when_all_alive() {
        let mut mgr = SessionManager::new();
        mgr.create("s1".into(), 80, 24, 100).unwrap();
        mgr.create("s2".into(), 80, 24, 100).unwrap();

        let dead = mgr.take_dead_sessions();
        assert!(dead.is_empty(), "no sessions should be dead: {:?}", dead.iter().map(|s| &s.name).collect::<Vec<_>>());
        assert_eq!(mgr.list().len(), 2);
    }

    #[test]
    fn client_guard_clears_has_client_on_drop() {
        let has_client = Arc::new(AtomicBool::new(true));
        let (_evict_tx, evict_rx) = tokio::sync::watch::channel(true);
        {
            let _guard = ClientGuard {
                has_client: has_client.clone(),
                evict_rx,
            };
            assert!(has_client.load(Ordering::Acquire));
        }
        assert!(!has_client.load(Ordering::Acquire));
    }

    #[test]
    fn client_guard_skips_clear_when_evicted() {
        let has_client = Arc::new(AtomicBool::new(true));
        let (evict_tx, evict_rx) = tokio::sync::watch::channel(true);
        {
            let _guard = ClientGuard {
                has_client: has_client.clone(),
                evict_rx,
            };
            let _ = evict_tx.send(false);
        }
        assert!(has_client.load(Ordering::Acquire));
    }
}
