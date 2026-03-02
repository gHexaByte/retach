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

    /// Bincode deserialization or I/O error.
    #[error("deserialization failed: {0}")]
    Deserialize(#[from] bincode::Error),
}

/// Maximum frame size: 16 MiB (fix C2 — prevents OOM from malicious/corrupt frames)
pub const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// Length-prefixed message encoding.
/// Uses u32::try_from to prevent silent truncation (fix C4).
pub fn encode(msg: &impl Serialize) -> Result<Vec<u8>, ProtocolError> {
    let data = bincode::serialize(msg)?;
    let len = u32::try_from(data.len()).map_err(|_| ProtocolError::EncodeTooLarge { size: data.len() })?;
    let mut buf = Vec::with_capacity(4 + data.len());
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&data);
    Ok(buf)
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
pub async fn read_one_message<T: DeserializeOwned>(
    reader: &mut (impl AsyncReadExt + Unpin),
) -> Result<T, ProtocolError> {
    let mut buf = vec![0u8; 65536];
    let mut read_buf = Vec::new();
    loop {
        let n = reader.read(&mut buf).await.map_err(|e| {
            ProtocolError::Deserialize(bincode::Error::from(
                std::io::Error::new(std::io::ErrorKind::UnexpectedEof, e),
            ))
        })?;
        if n == 0 {
            return Err(ProtocolError::Deserialize(bincode::Error::from(
                std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "connection closed"),
            )));
        }
        read_buf.extend_from_slice(&buf[..n]);
        if let Some((data, _)) = decode_frame(&read_buf)? {
            return Ok(bincode::deserialize(data)?);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::messages::{ClientMsg, ServerMsg, SessionInfo};

    #[test]
    fn encode_decode_round_trip() {
        let msg = ClientMsg::Connect {
            name: "test".into(),
            history: 1000,
            cols: 80,
            rows: 24,
        };
        let encoded = encode(&msg).unwrap();
        let (data, consumed) = decode_frame(&encoded).unwrap().unwrap();
        assert_eq!(consumed, encoded.len());
        let decoded: ClientMsg = bincode::deserialize(data).unwrap();
        match decoded {
            ClientMsg::Connect { name, history, cols, rows } => {
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
        let decoded: ServerMsg = bincode::deserialize(data).unwrap();
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
        let _: ClientMsg = bincode::deserialize(data1).unwrap();
        let (data2, _) = decode_frame(&buf[consumed1..]).unwrap().unwrap();
        let _: ClientMsg = bincode::deserialize(data2).unwrap();
    }
}
