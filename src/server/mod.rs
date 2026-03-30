//! Daemon server that manages sessions and accepts client connections over a Unix socket.

pub mod socket;
pub mod client_handler;
pub mod session_bridge;
mod session_setup;
mod session_relay;

use crate::session::SessionManager;
use std::sync::Arc;
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tracing::{info, warn};

pub use socket::socket_path;

/// Drop a value on a blocking thread with a timeout. Used for Session drops
/// which call blocking kill()+wait() on child processes. The 5s timeout
/// prevents hangs when grandchild processes keep the PTY alive after kill.
pub(super) async fn drop_blocking_with_timeout<T: Send + 'static>(value: T, label: &str) {
    let label = label.to_string();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::task::spawn_blocking(move || drop(value)),
    ).await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(join_err)) => warn!(%label, error = %join_err, "drop task panicked"),
        Err(_) => warn!(%label, "timed out dropping value on blocking thread"),
    }
}

/// Interval between dead session cleanup sweeps. 30s balances responsiveness
/// (dead sessions freed within half a minute) against lock contention overhead.
const CLEANUP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Start the daemon server: bind the Unix socket, spawn the cleanup task, and accept clients.
pub async fn run_server() -> anyhow::Result<()> {
    // Ignore SIGHUP so SSH disconnects don't kill us
    use nix::sys::signal::{signal, SigHandler, Signal};
    // SAFETY: SIG_IGN is async-signal-safe for SIGHUP.
    unsafe { signal(Signal::SIGHUP, SigHandler::SigIgn) }
        .map_err(|e| anyhow::anyhow!("failed to ignore SIGHUP: {}", e))?;

    let path = socket_path()?;
    // Only remove socket if it's stale (no server is listening).
    // This prevents a second server from yanking the socket out from under
    // an already-running server.
    //
    // TOCTOU note: there is a small race window between remove_file() and
    // bind() below where another process could create a socket at the same
    // path. This is acceptable because:
    //   1. retach is a single-user tool — concurrent server starts are rare
    //   2. If a race occurs, bind() fails with EADDRINUSE and the user retries
    //   3. Using O_EXCL or flock() would add complexity for a near-zero-probability scenario
    if path.exists() {
        match tokio::net::UnixStream::connect(&path).await {
            Ok(_) => {
                anyhow::bail!("another server is already running on {:?}", path);
            }
            Err(_) => {
                // Stale socket — safe to remove
                if let Err(e) = std::fs::remove_file(&path) {
                    warn!(path = ?path, error = %e, "failed to remove stale socket");
                }
            }
        }
    }

    let listener = UnixListener::bind(&path)?;
    info!(path = ?path, "server listening");

    // RAII guard to clean up socket file on exit
    let _socket_guard = SocketGuard(path.clone());

    let manager = Arc::new(Mutex::new(SessionManager::new()));

    // Dead session cleanup task — drops dead sessions outside the lock
    let cleanup_manager = manager.clone();
    let cleanup_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(CLEANUP_INTERVAL);
        loop {
            interval.tick().await;
            let dead_sessions = {
                let mut mgr = cleanup_manager.lock().await;
                mgr.take_dead_sessions()
            };
            if !dead_sessions.is_empty() {
                drop_blocking_with_timeout(dead_sessions, "dead session cleanup").await;
            }
        }
    });

    // Graceful shutdown via signals
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _)) => {
                        let manager = manager.clone();
                        tokio::spawn(async move {
                            if let Err(e) = client_handler::handle_client(stream, manager).await {
                                warn!(error = %e, "client error");
                            }
                        });
                    }
                    Err(e) => {
                        warn!(error = %e, "accept failed, retrying");
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
            }
            _ = sigterm.recv() => {
                info!("received SIGTERM, shutting down");
                break;
            }
            _ = sigint.recv() => {
                info!("received SIGINT, shutting down");
                break;
            }
        }
    }

    // Cancel the cleanup task before draining sessions
    cleanup_handle.abort();
    let _ = cleanup_handle.await;

    // Explicitly drop all sessions on a blocking thread with a timeout,
    // so server shutdown doesn't hang if child processes are unresponsive.
    let all_sessions: Vec<crate::session::Session> = {
        let mut mgr = manager.lock().await;
        mgr.drain_all()
    };
    if !all_sessions.is_empty() {
        info!(count = all_sessions.len(), "cleaning up sessions on shutdown");
        drop_blocking_with_timeout(all_sessions, "shutdown session cleanup").await;
    }

    Ok(())
    // _socket_guard drops here, removing socket file
}

/// RAII guard that removes the socket file on drop.
struct SocketGuard(std::path::PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
