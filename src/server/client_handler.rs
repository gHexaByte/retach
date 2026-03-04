use crate::protocol::{self, ClientMsg, ServerMsg, FrameReader};
use crate::session::SessionManager;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use std::sync::Arc;
use tracing::info;

use super::session_bridge::{handle_session, ConnectRequest};

/// Read initial message with a timeout to prevent idle connections from leaking resources.
const INITIAL_MSG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Dispatch a single client connection by reading its first message and routing accordingly.
pub async fn handle_client(
    mut stream: tokio::net::UnixStream,
    manager: Arc<Mutex<SessionManager>>,
) -> anyhow::Result<()> {
    let mut frames = FrameReader::new();
    let deadline = tokio::time::Instant::now() + INITIAL_MSG_TIMEOUT;

    loop {
        match tokio::time::timeout_at(deadline, frames.fill_from(&mut stream)).await {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => return Ok(()),
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                tracing::debug!("client timed out waiting for initial message");
                return Ok(());
            }
        }

        if let Some(msg) = frames.decode_next::<ClientMsg>()? {
            match msg {
                ClientMsg::Connect { name, history, cols, rows, mode } => {
                    if let Err(e) = crate::session::validate_session_name(&name) {
                        let resp = protocol::encode(&ServerMsg::Error(format!("{}", e)))?;
                        stream.write_all(&resp).await?;
                        return Ok(());
                    }
                    return handle_session(stream, manager, ConnectRequest {
                        name, history, cols, rows,
                        leftover: frames.into_leftover(),
                        mode,
                    }).await;
                }
                ClientMsg::ListSessions => {
                    let list = manager.lock().await.list();
                    let resp = protocol::encode(&ServerMsg::SessionList(list))?;
                    stream.write_all(&resp).await?;
                    return Ok(());
                }
                ClientMsg::KillSession { name } => {
                    if let Err(e) = crate::session::validate_session_name(&name) {
                        let resp = protocol::encode(&ServerMsg::Error(format!("{}", e)))?;
                        stream.write_all(&resp).await?;
                        return Ok(());
                    }
                    let removed = {
                        let mut mgr = manager.lock().await;
                        mgr.remove(&name)
                    };
                    // Session dropped outside the async Mutex via spawn_blocking
                    if let Some(session) = removed {
                        tokio::task::spawn_blocking(move || drop(session));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{self, ClientMsg, ServerMsg, FrameReader};
    use crate::session::SessionManager;
    use std::sync::Arc;
    use tokio::io::AsyncWriteExt;
    use tokio::sync::Mutex;

    /// Helper: read a full response from a UnixStream and deserialize it as a ServerMsg.
    async fn read_response(stream: &mut tokio::net::UnixStream) -> ServerMsg {
        let mut reader = FrameReader::new();
        loop {
            assert!(reader.fill_from(stream).await.expect("read failed"),
                "connection closed before a full response was received");
            if let Some(msg) = reader.decode_next::<ServerMsg>().expect("decode error") {
                return msg;
            }
        }
    }

    #[tokio::test]
    async fn list_sessions_empty() {
        let (client_stream, server_stream) = tokio::net::UnixStream::pair().unwrap();
        let manager = Arc::new(Mutex::new(SessionManager::new()));

        // Send ListSessions from client side
        let msg = protocol::encode(&ClientMsg::ListSessions).unwrap();
        let mut client_stream = client_stream;
        client_stream.write_all(&msg).await.unwrap();

        // Spawn handle_client on the server side
        let handle = tokio::spawn(handle_client(server_stream, manager));

        // Read response on the client side
        let response = read_response(&mut client_stream).await;
        match response {
            ServerMsg::SessionList(list) => {
                assert!(list.is_empty(), "expected empty session list, got {:?}", list);
            }
            other => panic!("expected SessionList, got {:?}", other),
        }

        // handle_client should complete successfully
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn kill_nonexistent_session() {
        let (client_stream, server_stream) = tokio::net::UnixStream::pair().unwrap();
        let manager = Arc::new(Mutex::new(SessionManager::new()));

        let msg = protocol::encode(&ClientMsg::KillSession {
            name: "no-such-session".into(),
        })
        .unwrap();
        let mut client_stream = client_stream;
        client_stream.write_all(&msg).await.unwrap();

        let handle = tokio::spawn(handle_client(server_stream, manager));

        let response = read_response(&mut client_stream).await;
        match response {
            ServerMsg::Error(err_msg) => {
                assert!(
                    err_msg.contains("no-such-session"),
                    "error message should mention session name, got: {}",
                    err_msg
                );
            }
            other => panic!("expected Error, got {:?}", other),
        }

        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn unexpected_message_returns_error() {
        let (client_stream, server_stream) = tokio::net::UnixStream::pair().unwrap();
        let manager = Arc::new(Mutex::new(SessionManager::new()));

        // Send an Input message, which is not a valid initial message
        let msg = protocol::encode(&ClientMsg::Input(b"hello".to_vec())).unwrap();
        let mut client_stream = client_stream;
        client_stream.write_all(&msg).await.unwrap();

        let handle = tokio::spawn(handle_client(server_stream, manager));

        let response = read_response(&mut client_stream).await;
        match response {
            ServerMsg::Error(err_msg) => {
                assert!(
                    err_msg.contains("expected Connect, ListSessions, or KillSession"),
                    "unexpected error message: {}",
                    err_msg
                );
            }
            other => panic!("expected Error, got {:?}", other),
        }

        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn client_disconnect_before_message() {
        let (client_stream, server_stream) = tokio::net::UnixStream::pair().unwrap();
        let manager = Arc::new(Mutex::new(SessionManager::new()));

        // Drop the client side immediately to simulate a disconnect
        drop(client_stream);

        // handle_client should return Ok(()) when the client disconnects before sending anything
        let result = handle_client(server_stream, manager).await;
        assert!(result.is_ok(), "expected Ok(()), got {:?}", result);
    }
}
