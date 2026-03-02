use crate::protocol::{self, ClientMsg, ServerMsg};
use crate::session::SessionManager;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use std::sync::Arc;
use tracing::info;

use super::session_bridge::handle_session;

/// Read initial message with a timeout to prevent idle connections from leaking resources.
const INITIAL_MSG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Dispatch a single client connection by reading its first message and routing accordingly.
pub async fn handle_client(
    mut stream: tokio::net::UnixStream,
    manager: Arc<Mutex<SessionManager>>,
) -> anyhow::Result<()> {
    let mut buf = vec![0u8; 65536];
    let mut read_buf = Vec::new();

    let deadline = tokio::time::Instant::now() + INITIAL_MSG_TIMEOUT;

    loop {
        let n = match tokio::time::timeout_at(deadline, stream.read(&mut buf)).await {
            Ok(result) => result?,
            Err(_) => {
                tracing::debug!("client timed out waiting for initial message");
                return Ok(());
            }
        };
        if n == 0 {
            return Ok(());
        }
        read_buf.extend_from_slice(&buf[..n]);

        if let Some((data, consumed)) = protocol::decode_frame(&read_buf)? {
            let msg: ClientMsg = bincode::deserialize(data)?;
            read_buf.drain(..consumed);

            match msg {
                ClientMsg::Connect { name, history, cols, rows } => {
                    match handle_session(stream, manager, name, history, cols, rows, read_buf).await {
                        Ok(()) => return Ok(()),
                        Err(e) => {
                            // stream was moved into handle_session; error is logged by caller
                            return Err(e);
                        }
                    }
                }
                ClientMsg::ListSessions => {
                    let list = manager.lock().await.list();
                    let resp = protocol::encode(&ServerMsg::SessionList(list))?;
                    stream.write_all(&resp).await?;
                    return Ok(());
                }
                ClientMsg::KillSession { name } => {
                    let mut mgr = manager.lock().await;
                    if mgr.remove(&name).is_some() {
                        info!(session = %name, "session killed");
                        let resp = protocol::encode(&ServerMsg::SessionKilled { name })?;
                        stream.write_all(&resp).await?;
                    } else {
                        let resp = protocol::encode(&ServerMsg::Error(format!(
                            "session '{}' not found",
                            name
                        )))?;
                        stream.write_all(&resp).await?;
                    }
                    return Ok(());
                }
                _ => {
                    let resp = protocol::encode(&ServerMsg::Error(
                        "expected Connect, ListSessions, or KillSession".into(),
                    ))?;
                    stream.write_all(&resp).await?;
                    return Ok(());
                }
            }
        }
    }
}
