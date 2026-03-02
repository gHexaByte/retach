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
    unsafe { nix::libc::signal(nix::libc::SIGHUP, nix::libc::SIG_IGN); }

    let path = socket_path();
    let _ = std::fs::remove_file(&path);

    let listener = UnixListener::bind(&path)?;
    info!(path = ?path, "server listening");

    let manager = Arc::new(Mutex::new(SessionManager::new()));

    // Dead session cleanup task (fix I2)
    let cleanup_manager = manager.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            let mut mgr = cleanup_manager.lock().await;
            mgr.cleanup_dead_sessions();
        }
    });

    loop {
        let (stream, _) = listener.accept().await?;
        let manager = manager.clone();
        tokio::spawn(async move {
            if let Err(e) = client_handler::handle_client(stream, manager).await {
                warn!(error = %e, "client error");
            }
        });
    }
}
