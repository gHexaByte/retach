use crate::pty::Pty;
use crate::screen::Screen;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A single terminal session backed by a PTY and a virtual screen.
pub struct Session {
    pub name: String,
    pub pty: Pty,
    pub screen: Arc<Mutex<Screen>>,
    pub dims: Arc<Mutex<(u16, u16)>>,
    /// When a client is attached, holds the sender side of a watch channel.
    /// Sending `false` evicts the active client. Replaced on each new attach.
    pub evict_tx: Option<tokio::sync::watch::Sender<bool>>,
    /// Signaled when the previous connection's PTY reader thread exits.
    /// Used to prevent two reader threads from racing for PTY data on reconnect.
    pub reader_exited: Arc<tokio::sync::Notify>,
}

impl Session {
    /// Create a new session, spawning a shell in a PTY of the given size.
    pub fn new(name: String, cols: u16, rows: u16, history: usize) -> anyhow::Result<Self> {
        let pty = Pty::spawn(cols, rows)?;
        let screen = Arc::new(Mutex::new(Screen::new(cols, rows, history)));
        let dims = Arc::new(Mutex::new((cols, rows)));
        let reader_exited = Arc::new(tokio::sync::Notify::new());
        // Pre-signal so the first connection doesn't wait for a non-existent reader.
        reader_exited.notify_one();
        Ok(Self { name, pty, screen, dims, evict_tx: None, reader_exited })
    }

    /// Check if the session's child process is still alive
    pub fn is_alive(&self) -> bool {
        match self.pty.child_arc().lock() {
            Ok(mut child) => child.try_wait().ok().flatten().is_none(),
            Err(_) => false,
        }
    }

    /// Get the child process PID (if available)
    pub fn child_pid(&self) -> Option<u32> {
        self.pty.child_arc().lock().ok().and_then(|c| c.process_id())
    }
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
            let (cols, rows) = s.dims.lock().map(|d| *d).unwrap_or((80, 24));
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
