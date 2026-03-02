//! Daemon server that manages sessions and accepts client connections over a Unix socket.

pub mod socket;
pub mod client_handler;
pub mod session_bridge;

use crate::session::SessionManager;
use std::sync::Arc;
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tracing::{info, warn};

pub use socket::socket_path;

/// Start the daemon server: bind the Unix socket, spawn the cleanup task, and accept clients.
pub async fn run_server() -> anyhow::Result<()> {
    // Ignore SIGHUP so SSH disconnects don't kill us
    use nix::sys::signal::{signal, SigHandler, Signal};
    // SAFETY: SIG_IGN is async-signal-safe for SIGHUP.
    unsafe { signal(Signal::SIGHUP, SigHandler::SigIgn) }
        .map_err(|e| anyhow::anyhow!("failed to ignore SIGHUP: {}", e))?;

    let path = socket_path();
    let _ = std::fs::remove_file(&path);

    let listener = UnixListener::bind(&path)?;
    info!(path = ?path, "server listening");

    // RAII guard to clean up socket file on exit
    let _socket_guard = SocketGuard(path.clone());

    let manager = Arc::new(Mutex::new(SessionManager::new()));

    // Dead session cleanup task — drops dead sessions outside the lock
    let cleanup_manager = manager.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            let dead_sessions = {
                let mut mgr = cleanup_manager.lock().await;
                mgr.take_dead_sessions()
            };
            // Drop dead sessions on spawn_blocking (their Drop calls blocking kill+wait)
            if !dead_sessions.is_empty() {
                tokio::task::spawn_blocking(move || drop(dead_sessions));
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
