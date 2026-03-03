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
    let mut frames = FrameReader::new();
    loop {
        if !frames.fill_from(reader).await? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed",
            ).into());
        }
        if let Some(msg) = frames.decode_next()? {
            return Ok(msg);
        }
    }
}

/// Buffered frame reader for the length-prefixed protocol.
///
/// Handles read buffering, overflow protection, and frame decoding.
/// Eliminates duplicated read-decode-drain loops across client and server code.
pub struct FrameReader {
    read_buf: Vec<u8>,
    tmp_buf: Vec<u8>,
}

impl FrameReader {
    /// Create a new reader with empty buffers.
    pub fn new() -> Self {
        Self {
            read_buf: Vec::new(),
            tmp_buf: vec![0u8; READ_BUF_SIZE],
        }
    }

    /// Create a new reader pre-loaded with leftover bytes from a previous read.
    pub fn with_leftover(leftover: Vec<u8>) -> Self {
        Self {
            read_buf: leftover,
            tmp_buf: vec![0u8; READ_BUF_SIZE],
        }
    }

    /// Read from the async reader into internal buffer.
    /// Returns `Ok(true)` if data was read, `Ok(false)` on EOF.
    pub async fn fill_from<R: AsyncReadExt + Unpin>(
        &mut self,
        reader: &mut R,
    ) -> Result<bool, ProtocolError> {
        let n = reader.read(&mut self.tmp_buf).await?;
        if n == 0 {
            return Ok(false);
        }
        self.read_buf.extend_from_slice(&self.tmp_buf[..n]);
        if self.read_buf.len() > MAX_FRAME_SIZE + 4 {
            return Err(ProtocolError::FrameTooLarge {
                size: self.read_buf.len(),
                max: MAX_FRAME_SIZE,
            });
        }
        Ok(true)
    }

    /// Decode and remove the next complete message from the buffer.
    /// Returns `Ok(None)` if no complete frame is available yet.
    pub fn decode_next<T: DeserializeOwned>(&mut self) -> Result<Option<T>, ProtocolError> {
        match decode_frame(&self.read_buf)? {
            Some((data, consumed)) => {
                let msg: T = decode(data)?;
                self.read_buf.drain(..consumed);
                Ok(Some(msg))
            }
            None => Ok(None),
        }
    }

    /// Consume the reader and return any unprocessed bytes.
    pub fn into_leftover(self) -> Vec<u8> {
        self.read_buf
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

    #[tokio::test]
    async fn read_one_message_rejects_buffer_overflow() {
        // Send a valid header claiming MAX_FRAME_SIZE bytes, then flood with junk.
        // read_one_message should reject when read_buf exceeds MAX_FRAME_SIZE + 4.
        let (mut write_half, mut read_half) = tokio::io::duplex(65536);
        use tokio::io::AsyncWriteExt;

        let len_bytes = (MAX_FRAME_SIZE as u32).to_be_bytes();
        write_half.write_all(&len_bytes).await.unwrap();
        // Write MAX_FRAME_SIZE + 1024 bytes of junk (exceeds the frame + header)
        let junk = vec![0u8; MAX_FRAME_SIZE + 1024];
        tokio::spawn(async move {
            let _ = write_half.write_all(&junk).await;
        });

        let result: Result<ClientMsg, _> = read_one_message(&mut read_half).await;
        assert!(result.is_err(), "should reject oversized buffer accumulation");
    }

    #[test]
    fn decode_frame_zero_length() {
        // A frame claiming 0 bytes should be decodable (returns 0 bytes of data)
        let mut buf = Vec::new();
        buf.extend_from_slice(&0u32.to_be_bytes());
        let result = decode_frame(&buf).unwrap();
        assert!(result.is_some());
        let (data, consumed) = result.unwrap();
        assert_eq!(data.len(), 0);
        assert_eq!(consumed, 4);
    }

    #[test]
    fn decode_frame_empty_buffer() {
        let result = decode_frame(&[]).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn decode_frame_1_byte_buffer() {
        let result = decode_frame(&[0x00]).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn decode_frame_3_byte_buffer() {
        let result = decode_frame(&[0x00, 0x00, 0x00]).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn frame_reader_with_leftover() {
        let msg = ClientMsg::Detach;
        let encoded = encode(&msg).unwrap();
        let mut reader = FrameReader::with_leftover(encoded);
        let result: Option<ClientMsg> = reader.decode_next().unwrap();
        assert!(result.is_some());
        match result.unwrap() {
            ClientMsg::Detach => {}
            other => panic!("expected Detach, got {:?}", other),
        }
    }

    #[test]
    fn frame_reader_multiple_messages_in_buffer() {
        let msg1 = ClientMsg::Detach;
        let msg2 = ClientMsg::ListSessions;
        let msg3 = ClientMsg::RefreshScreen;
        let mut buf = encode(&msg1).unwrap();
        buf.extend_from_slice(&encode(&msg2).unwrap());
        buf.extend_from_slice(&encode(&msg3).unwrap());

        let mut reader = FrameReader::with_leftover(buf);
        let r1: Option<ClientMsg> = reader.decode_next().unwrap();
        assert!(matches!(r1, Some(ClientMsg::Detach)));
        let r2: Option<ClientMsg> = reader.decode_next().unwrap();
        assert!(matches!(r2, Some(ClientMsg::ListSessions)));
        let r3: Option<ClientMsg> = reader.decode_next().unwrap();
        assert!(matches!(r3, Some(ClientMsg::RefreshScreen)));
        let r4: Option<ClientMsg> = reader.decode_next().unwrap();
        assert!(r4.is_none());
    }

    #[test]
    fn frame_reader_leftover_after_decode() {
        let msg = ClientMsg::Detach;
        let mut buf = encode(&msg).unwrap();
        buf.extend_from_slice(&[0xDE, 0xAD]); // trailing junk

        let mut reader = FrameReader::with_leftover(buf);
        let _: ClientMsg = reader.decode_next().unwrap().unwrap();
        let leftover = reader.into_leftover();
        assert_eq!(leftover, &[0xDE, 0xAD]);
    }

    #[test]
    fn encode_all_client_msg_variants() {
        // Ensure all ClientMsg variants can be encoded and decoded
        let messages: Vec<ClientMsg> = vec![
            ClientMsg::Input(vec![0x61, 0x62]),
            ClientMsg::Resize { cols: 120, rows: 40 },
            ClientMsg::Detach,
            ClientMsg::ListSessions,
            ClientMsg::Connect {
                name: "test".into(),
                history: 500,
                cols: 80,
                rows: 24,
                mode: ConnectMode::CreateOrAttach,
            },
            ClientMsg::KillSession { name: "kill-me".into() },
            ClientMsg::RefreshScreen,
        ];
        for msg in &messages {
            let encoded = encode(msg).unwrap();
            let (data, _) = decode_frame(&encoded).unwrap().unwrap();
            let _decoded: ClientMsg = decode(data).unwrap();
        }
    }

    #[test]
    fn encode_all_server_msg_variants() {
        let messages: Vec<ServerMsg> = vec![
            ServerMsg::ScrollbackLine(vec![0x41]),
            ServerMsg::ScreenUpdate(vec![0x1b, 0x5b, 0x48]),
            ServerMsg::History(vec![vec![0x41], vec![0x42]]),
            ServerMsg::SessionList(vec![SessionInfo {
                name: "s1".into(), pid: 42, cols: 80, rows: 24,
            }]),
            ServerMsg::SessionEnded,
            ServerMsg::Error("test error".into()),
            ServerMsg::Connected { name: "test".into(), new_session: true },
            ServerMsg::SessionKilled { name: "dead".into() },
            ServerMsg::Passthrough(vec![0x07]),
        ];
        for msg in &messages {
            let encoded = encode(msg).unwrap();
            let (data, _) = decode_frame(&encoded).unwrap().unwrap();
            let _decoded: ServerMsg = decode(data).unwrap();
        }
    }

    #[test]
    fn encode_empty_collections() {
        // Empty vectors, empty strings
        let msg = ServerMsg::History(vec![]);
        let encoded = encode(&msg).unwrap();
        let (data, _) = decode_frame(&encoded).unwrap().unwrap();
        let decoded: ServerMsg = decode(data).unwrap();
        match decoded {
            ServerMsg::History(lines) => assert!(lines.is_empty()),
            other => panic!("expected History, got {:?}", other),
        }

        let msg = ServerMsg::SessionList(vec![]);
        let encoded = encode(&msg).unwrap();
        let (data, _) = decode_frame(&encoded).unwrap().unwrap();
        let decoded: ServerMsg = decode(data).unwrap();
        match decoded {
            ServerMsg::SessionList(list) => assert!(list.is_empty()),
            other => panic!("expected SessionList, got {:?}", other),
        }
    }

    #[test]
    fn encode_large_input() {
        // Large input message (64KB)
        let data = vec![0x41u8; 65536];
        let msg = ClientMsg::Input(data.clone());
        let encoded = encode(&msg).unwrap();
        let (frame_data, _) = decode_frame(&encoded).unwrap().unwrap();
        let decoded: ClientMsg = decode(frame_data).unwrap();
        match decoded {
            ClientMsg::Input(d) => assert_eq!(d.len(), 65536),
            other => panic!("expected Input, got {:?}", other),
        }
    }
}
