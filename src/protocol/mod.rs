//! Length-prefixed binary protocol for client-server communication over Unix sockets.
//! Messages are serialized with bincode and framed with a 4-byte little-endian length prefix.

pub mod messages;
pub mod codec;

pub use messages::{ClientMsg, ServerMsg, SessionInfo};
pub use codec::{encode, decode_frame, read_one_message};
