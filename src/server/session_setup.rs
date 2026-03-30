//! Session acquisition and preparation: connect/create a session, resize the PTY,
//! and return all handles needed for the I/O relay loops.

use crate::protocol::{self, ServerMsg};
use crate::session::{SessionHandles, SessionManager, ClientGuard, DEFAULT_COLS, DEFAULT_ROWS};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use super::session_bridge::lock_mutex;

/// Handles returned from `setup_session`, containing everything needed for the I/O loops.
pub(super) struct SessionSetup {
    pub(super) handles: SessionHandles,
    pub(super) is_new_session: bool,
    pub(super) evict_rx: tokio::sync::watch::Receiver<bool>,
    pub(super) client_guard: ClientGuard,
}

/// Parameters for a session connection request.
pub(super) struct ConnectRequest {
    pub(super) name: String,
    pub(super) history: usize,
    pub(super) cols: u16,
    pub(super) rows: u16,
    pub(super) leftover: Vec<u8>,
    pub(super) mode: crate::protocol::ConnectMode,
}

/// Resize the PTY master and the virtual screen to the given dimensions.
/// Acquires the screen lock first (cheaper, no side effects) so that
/// if it fails, the PTY master is not left at a mismatched size.
pub(super) fn resize_pty(
    master: &crate::pty::SharedMasterPty,
    screen: &crate::session::SharedScreen,
    cols: u16,
    rows: u16,
) -> anyhow::Result<()> {
    let retach::screen::TerminalSize { cols, rows } = retach::screen::sanitize_dimensions(cols, rows);
    let mut scr = lock_mutex(screen, "screen")?;
    let m = lock_mutex(master, "master")?;
    m.resize(portable_pty::PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    scr.resize(cols, rows);
    Ok(())
}

/// Resize the PTY + screen and update stored dimensions, or send SIGWINCH if same size.
/// Runs blocking operations on spawn_blocking.
pub(super) async fn resize_or_sigwinch(
    master: &crate::pty::SharedMasterPty,
    screen: &crate::session::SharedScreen,
    dims: &Arc<StdMutex<retach::screen::TerminalSize>>,
    cols: u16,
    rows: u16,
    current_dims: retach::screen::TerminalSize,
    session_name: &str,
) -> anyhow::Result<()> {
    let master = master.clone();
    let screen = screen.clone();
    let dims = dims.clone();
    let name = session_name.to_string();

    if current_dims.cols != cols || current_dims.rows != rows {
        debug!(
            session = %session_name,
            old_cols = current_dims.cols, old_rows = current_dims.rows,
            new_cols = cols, new_rows = rows,
            "resizing session"
        );
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            resize_pty(&master, &screen, cols, rows)?;
            match dims.lock() {
                Ok(mut d) => *d = retach::screen::sanitize_dimensions(cols, rows),
                Err(e) => warn!(session = %name, error = %e, "dims mutex poisoned during resize"),
            }
            Ok(())
        }).await??;
    } else {
        debug!(session = %session_name, "sending SIGWINCH (same dimensions)");
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let sanitized = retach::screen::sanitize_dimensions(cols, rows);
            let m = lock_mutex(&master, "master")?;
            m.resize(portable_pty::PtySize {
                rows: sanitized.rows,
                cols: sanitized.cols,
                pixel_width: 0,
                pixel_height: 0,
            }).map_err(|e| anyhow::anyhow!("{}", e))
        }).await??;
    }
    Ok(())
}

/// Send an error message to the client and return the error for the caller to propagate.
async fn send_error_to_client(
    stream: &mut tokio::net::UnixStream,
    msg: String,
) -> anyhow::Error {
    if let Ok(resp) = protocol::encode(&ServerMsg::Error(msg.clone())) {
        let _ = stream.write_all(&resp).await;
    }
    anyhow::anyhow!("{}", msg)
}

/// Acquire or create the session, set up eviction, resize, and extract handles.
/// Returns all handles needed for the I/O loops, or sends an error to the client.
pub(super) async fn setup_session(
    stream: &mut tokio::net::UnixStream,
    manager: &Arc<Mutex<SessionManager>>,
    name: &str,
    history: usize,
    cols: u16,
    rows: u16,
    mode: crate::protocol::ConnectMode,
) -> anyhow::Result<SessionSetup> {
    let mut mgr = manager.lock().await;

    use crate::protocol::ConnectMode;
    let (session, is_new) = match mode {
        ConnectMode::CreateOrAttach => {
            match mgr.get_or_create(name, cols, rows, history) {
                Ok(s) => s,
                Err(e) => {
                    return Err(send_error_to_client(stream, format!("{}", e)).await);
                }
            }
        }
        ConnectMode::CreateOnly => {
            if mgr.get(name).is_some() {
                return Err(send_error_to_client(stream, format!("session '{}' already exists", name)).await);
            }
            if let Err(e) = mgr.create(name.to_string(), cols, rows, history) {
                return Err(send_error_to_client(stream, format!("{}", e)).await);
            }
            match mgr.get_mut(name) {
                Some(s) => (s, true),
                None => {
                    return Err(send_error_to_client(stream, "session disappeared after creation".into()).await);
                }
            }
        }
        ConnectMode::AttachOnly => {
            match mgr.get_mut(name) {
                Some(s) => (s, false),
                None => {
                    return Err(send_error_to_client(stream, format!("session '{}' not found", name)).await);
                }
            }
        }
    };

    let (client_guard, handles, evict_rx) = session.connect();

    // Drop the manager lock before resize — resize_or_sigwinch acquires screen/master
    // locks and runs spawn_blocking, so holding the manager lock here would block all
    // other session operations (list, kill, new connections) for the duration.
    drop(mgr);

    // Resize existing session to match the connecting client's terminal size.
    // If resize fails, the client_guard drops when we return Err, automatically
    // clearing has_client (unless evicted in the meantime).
    if !is_new {
        let cur_dims = match handles.dims.lock() {
            Ok(d) => *d,
            Err(e) => {
                warn!(session = %name, error = %e, "dims mutex poisoned during reattach");
                retach::screen::TerminalSize { cols: DEFAULT_COLS, rows: DEFAULT_ROWS }
            }
        };
        if let Err(e) = resize_or_sigwinch(&handles.master, &handles.screen, &handles.dims, cols, rows, cur_dims, &handles.name).await {
            warn!(session = %name, error = %e, "failed to resize/SIGWINCH on reattach");
            anyhow::bail!("failed to resize/SIGWINCH on reattach to '{}'", name);
        }
    }

    Ok(SessionSetup { handles, is_new_session: is_new, evict_rx, client_guard })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionManager;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[tokio::test]
    async fn setup_creates_new_session() {
        let manager = Arc::new(Mutex::new(SessionManager::new()));
        let (_client, mut server) = tokio::net::UnixStream::pair().unwrap();

        let result = setup_session(
            &mut server,
            &manager,
            "test-new",
            100,
            80, 24,
            crate::protocol::ConnectMode::CreateOrAttach,
        ).await;

        assert!(result.is_ok(), "setup_session failed: {:?}", result.err());
        let setup = result.unwrap();
        assert!(setup.is_new_session);
        assert_eq!(setup.handles.name, "test-new");
    }

    #[tokio::test]
    async fn setup_reattaches_existing_session() {
        let manager = Arc::new(Mutex::new(SessionManager::new()));

        // Create session first
        {
            let (_client, mut server) = tokio::net::UnixStream::pair().unwrap();
            let setup = setup_session(
                &mut server, &manager, "test-reattach", 100, 80, 24,
                crate::protocol::ConnectMode::CreateOrAttach,
            ).await.unwrap();
            assert!(setup.is_new_session);
            // Guard dropped -- has_client cleared
        }

        // Reattach
        {
            let (_client, mut server) = tokio::net::UnixStream::pair().unwrap();
            let setup = setup_session(
                &mut server, &manager, "test-reattach", 100, 80, 24,
                crate::protocol::ConnectMode::CreateOrAttach,
            ).await.unwrap();
            assert!(!setup.is_new_session);
        }
    }

    #[tokio::test]
    async fn setup_create_only_fails_for_existing() {
        let manager = Arc::new(Mutex::new(SessionManager::new()));

        // Create session
        {
            let (_client, mut server) = tokio::net::UnixStream::pair().unwrap();
            setup_session(
                &mut server, &manager, "test-create-only", 100, 80, 24,
                crate::protocol::ConnectMode::CreateOrAttach,
            ).await.unwrap();
        }

        // Try CreateOnly -- should fail (session exists)
        let (_client, mut server) = tokio::net::UnixStream::pair().unwrap();
        let result = setup_session(
            &mut server, &manager, "test-create-only", 100, 80, 24,
            crate::protocol::ConnectMode::CreateOnly,
        ).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn setup_attach_only_fails_for_missing() {
        let manager = Arc::new(Mutex::new(SessionManager::new()));
        let (_client, mut server) = tokio::net::UnixStream::pair().unwrap();

        let result = setup_session(
            &mut server, &manager, "nonexistent", 100, 80, 24,
            crate::protocol::ConnectMode::AttachOnly,
        ).await;
        assert!(result.is_err());
    }
}
