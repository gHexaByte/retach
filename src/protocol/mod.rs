//! Length-prefixed binary protocol for client-server communication over Unix sockets.
//! Messages are serialized with bincode and framed with a 4-byte big-endian length prefix.

pub mod messages;
pub mod codec;

pub use messages::{ClientMsg, ConnectMode, ServerMsg, SessionInfo};
pub use codec::{encode, decode, decode_frame, read_one_message, READ_BUF_SIZE};
