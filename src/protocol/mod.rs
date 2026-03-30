//! Length-prefixed binary protocol for client-server communication over Unix sockets.
//! Messages are serialized with bincode and framed with a 4-byte big-endian length prefix.

pub mod messages;
pub mod codec;

pub use messages::{ClientMsg, ConnectMode, ServerMsg, SessionInfo};
pub use codec::{encode, read_one_message, FrameReader};

#[cfg(test)]
mod tests_history_protocol;
