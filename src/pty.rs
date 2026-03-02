use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::Write;
use std::sync::{Arc, Mutex};

/// Wrapper around a pseudo-terminal with shared access to the master, writer, and child process.
pub struct Pty {
    /// Shared write handle for sending input to the PTY.
    pub writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
}

impl Pty {
    /// Spawn a new shell process in a PTY with the given dimensions.
    pub fn spawn(cols: u16, rows: u16) -> anyhow::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new_default_prog();
        cmd.env("TERM", "xterm-256color");
        let child = pair.slave.spawn_command(cmd)?;
        let writer = pair.master.take_writer()?;

        Ok(Self {
            writer: Arc::new(Mutex::new(writer)),
            master: Arc::new(Mutex::new(pair.master)),
            child: Arc::new(Mutex::new(child)),
        })
    }

    /// Return a shared reference to the child process.
    pub fn child_arc(&self) -> Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>> {
        self.child.clone()
    }

    /// Return a shared reference to the master PTY (used for reading output and resizing).
    pub fn master_arc(&self) -> Arc<Mutex<Box<dyn MasterPty + Send>>> {
        self.master.clone()
    }
}
