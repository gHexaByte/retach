use bincode::Options;
use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;
use tokio::io::AsyncReadExt;

/// Error type for protocol encoding/decoding failures.
#[derive(Error, Debug)]
pub enum ProtocolError {
    /// Received frame exceeds [`MAX_FRAME_SIZE`].
    #[error("frame too large: {size} bytes (max {max})")]
    FrameTooLarge { size: usize, max: usize },

    /// Serialized message is too large to fit in a u32 length prefix.
    #[error("message too large to encode: {size} bytes exceeds u32 max")]
    EncodeTooLarge { size: usize },

    /// Bincode deserialization error.
    #[error("deserialization failed: {0}")]
    Deserialize(#[from] bincode::Error),

    /// I/O error during read/write.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Maximum frame size: 16 MiB (fix C2 — prevents OOM from malicious/corrupt frames)
pub const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// Default read buffer size used across client, server, and codec.
pub const READ_BUF_SIZE: usize = 65536;

/// Bincode configuration with size limit matching MAX_FRAME_SIZE.
/// Prevents OOM from malicious frames where a Vec length prefix claims huge allocations.
/// NOTE: uses `DefaultOptions` fixint encoding — NOT compatible with top-level
/// `bincode::serialize/deserialize` (which use varint for collection lengths).
/// All encode/decode paths must use this config consistently.
pub fn bincode_config() -> impl Options + Copy {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_FRAME_SIZE as u64)
}

/// Length-prefixed message encoding.
/// Uses u32::try_from to prevent silent truncation (fix C4).
pub fn encode(msg: &impl Serialize) -> Result<Vec<u8>, ProtocolError> {
    let data = bincode_config().serialize(msg)?;
    let len = u32::try_from(data.len()).map_err(|_| ProtocolError::EncodeTooLarge { size: data.len() })?;
    let mut buf = Vec::with_capacity(4 + data.len());
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&data);
    Ok(buf)
}

/// Deserialize a bincode-encoded message from raw bytes.
pub fn decode<T: DeserializeOwned>(data: &[u8]) -> Result<T, ProtocolError> {
    Ok(bincode_config().deserialize(data)?)
}

/// Decode a length-prefixed frame from a buffer.
/// Returns (message_bytes, bytes_consumed) or an error.
/// Returns Ok(None) if the buffer is incomplete.
pub fn decode_frame(buf: &[u8]) -> Result<Option<(&[u8], usize)>, ProtocolError> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len > MAX_FRAME_SIZE {
        return Err(ProtocolError::FrameTooLarge { size: len, max: MAX_FRAME_SIZE });
    }
    if buf.len() < 4 + len {
        return Ok(None);
    }
    Ok(Some((&buf[4..4 + len], 4 + len)))
}

/// Read exactly one message from an async reader, handling buffering.
/// Eliminates duplicated read-loop code in list/kill operations.
///
/// **Note:** Any bytes received after the first complete frame are discarded.
/// This is safe for request-response patterns (list/kill) where only one
/// response is expected, but must not be used when multiple messages may arrive.
pub async fn read_one_message<T: DeserializeOwned>(
    reader: &mut (impl AsyncReadExt + Unpin),
) -> Result<T, ProtocolError> {
    let mut buf = vec![0u8; READ_BUF_SIZE];
    let mut read_buf = Vec::new();
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed",
            ).into());
        }
        read_buf.extend_from_slice(&buf[..n]);
        if let Some((data, _)) = decode_frame(&read_buf)? {
            return decode(data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::messages::{ClientMsg, ConnectMode, ServerMsg, SessionInfo};

    #[test]
    fn encode_decode_round_trip() {
        let msg = ClientMsg::Connect {
            name: "test".into(),
            history: 1000,
            cols: 80,
            rows: 24,
            mode: ConnectMode::CreateOrAttach,
        };
        let encoded = encode(&msg).unwrap();
        let (data, consumed) = decode_frame(&encoded).unwrap().unwrap();
        assert_eq!(consumed, encoded.len());
        let decoded: ClientMsg = decode(data).unwrap();
        match decoded {
            ClientMsg::Connect { name, history, cols, rows, .. } => {
                assert_eq!(name, "test");
                assert_eq!(history, 1000);
                assert_eq!(cols, 80);
                assert_eq!(rows, 24);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn encode_decode_server_msg() {
        let msg = ServerMsg::SessionList(vec![
            SessionInfo { name: "s1".into(), pid: 123, cols: 80, rows: 24 },
        ]);
        let encoded = encode(&msg).unwrap();
        let (data, _) = decode_frame(&encoded).unwrap().unwrap();
        let decoded: ServerMsg = decode(data).unwrap();
        match decoded {
            ServerMsg::SessionList(list) => {
                assert_eq!(list.len(), 1);
                assert_eq!(list[0].name, "s1");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn decode_incomplete_frame() {
        let msg = ClientMsg::Detach;
        let encoded = encode(&msg).unwrap();
        // Only give partial data
        let result = decode_frame(&encoded[..3]).unwrap();
        assert!(result.is_none());
        // Give header but not full body
        let result = decode_frame(&encoded[..encoded.len() - 1]).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn decode_rejects_oversized_frame() {
        // Craft a header claiming a huge frame
        let len_bytes = ((MAX_FRAME_SIZE + 1) as u32).to_be_bytes();
        let mut buf = Vec::new();
        buf.extend_from_slice(&len_bytes);
        buf.extend_from_slice(&[0u8; 100]);
        let result = decode_frame(&buf);
        assert!(result.is_err());
        match result.unwrap_err() {
            ProtocolError::FrameTooLarge { size, max } => {
                assert_eq!(size, MAX_FRAME_SIZE + 1);
                assert_eq!(max, MAX_FRAME_SIZE);
            }
            other => panic!("expected FrameTooLarge, got {:?}", other),
        }
    }

    #[test]
    fn decode_accepts_max_size_frame() {
        // A frame exactly at MAX_FRAME_SIZE should be accepted (if buffer is large enough)
        let len_bytes = (MAX_FRAME_SIZE as u32).to_be_bytes();
        let mut buf = Vec::new();
        buf.extend_from_slice(&len_bytes);
        // Don't actually allocate MAX_FRAME_SIZE — just check header passes
        let result = decode_frame(&buf).unwrap();
        // Should be None (incomplete), not an error
        assert!(result.is_none());
    }

    #[test]
    fn encode_multiple_decode_sequential() {
        let msg1 = ClientMsg::Detach;
        let msg2 = ClientMsg::ListSessions;
        let mut buf = encode(&msg1).unwrap();
        buf.extend_from_slice(&encode(&msg2).unwrap());

        let (data1, consumed1) = decode_frame(&buf).unwrap().unwrap();
        let _: ClientMsg = decode(data1).unwrap();
        let (data2, _) = decode_frame(&buf[consumed1..]).unwrap().unwrap();
        let _: ClientMsg = decode(data2).unwrap();
    }

    #[tokio::test]
    async fn read_one_message_success() {
        let msg = ClientMsg::Detach;
        let encoded = encode(&msg).unwrap();
        let (mut write_half, mut read_half) = tokio::io::duplex(65536);
        use tokio::io::AsyncWriteExt;
        write_half.write_all(&encoded).await.unwrap();
        drop(write_half); // close writer so reader sees EOF after data
        let result: ClientMsg = read_one_message(&mut read_half).await.unwrap();
        match result {
            ClientMsg::Detach => {} // expected
            other => panic!("expected Detach, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn read_one_message_connection_closed() {
        // An empty duplex stream (writer dropped immediately) should return an error.
        let (write_half, mut read_half) = tokio::io::duplex(65536);
        drop(write_half);
        let result: Result<ClientMsg, _> = read_one_message(&mut read_half).await;
        assert!(result.is_err(), "expected error on empty stream");
        match result.unwrap_err() {
            ProtocolError::Io(e) => {
                assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof);
            }
            other => panic!("expected Io error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn read_one_message_server_msg() {
        let msg = ServerMsg::Connected {
            name: "my-session".into(),
            new_session: true,
        };
        let encoded = encode(&msg).unwrap();
        let (mut write_half, mut read_half) = tokio::io::duplex(65536);
        use tokio::io::AsyncWriteExt;
        write_half.write_all(&encoded).await.unwrap();
        drop(write_half);
        let result: ServerMsg = read_one_message(&mut read_half).await.unwrap();
        match result {
            ServerMsg::Connected { name, new_session } => {
                assert_eq!(name, "my-session");
                assert!(new_session);
            }
            other => panic!("expected Connected, got {:?}", other),
        }
    }
}
