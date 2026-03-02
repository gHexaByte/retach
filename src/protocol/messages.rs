use serde::{Deserialize, Serialize};

/// Message sent from a client to the server.
#[derive(Serialize, Deserialize, Debug)]
pub enum ClientMsg {
    /// Keyboard input from client
    Input(Vec<u8>),
    /// Terminal resized
    Resize { cols: u16, rows: u16 },
    /// Client wants to detach
    Detach,
    /// Request session list
    ListSessions,
    /// Create or attach to session
    Connect { name: String, history: usize, cols: u16, rows: u16 },
    /// Kill a session
    KillSession { name: String },
}

/// Message sent from the server to a client.
#[derive(Serialize, Deserialize, Debug)]
pub enum ServerMsg {
    /// Scrollback line (passthrough to terminal)
    ScrollbackLine(Vec<u8>),
    /// Full screen redraw (ANSI bytes)
    ScreenUpdate(Vec<u8>),
    /// Scrollback history on reattach
    History(Vec<Vec<u8>>),
    /// Session list response
    SessionList(Vec<SessionInfo>),
    /// Session ended (shell exited)
    SessionEnded,
    /// Error
    Error(String),
    /// Connected successfully
    Connected { name: String, new_session: bool },
    /// Session killed successfully
    SessionKilled { name: String },
}

/// Snapshot of a session's metadata, used in list responses.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SessionInfo {
    pub name: String,
    pub pid: u32,
    pub cols: u16,
    pub rows: u16,
}
