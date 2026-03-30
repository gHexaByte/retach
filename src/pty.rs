use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

/// Shared PTY writer handle.
pub type SharedPtyWriter = Arc<Mutex<Box<dyn Write + Send>>>;
/// Shared master PTY handle.
pub type SharedMasterPty = Arc<Mutex<Box<dyn MasterPty + Send>>>;
/// Shared child process handle.
pub type SharedChild = Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>;

/// Wrapper around a pseudo-terminal with shared access to the master, writer, and child process.
pub struct Pty {
    writer: SharedPtyWriter,
    master: SharedMasterPty,
    child: SharedChild,
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

    /// Return a shared reference to the PTY writer.
    pub fn writer_arc(&self) -> SharedPtyWriter {
        self.writer.clone()
    }

    /// Return a shared reference to the child process.
    pub fn child_arc(&self) -> SharedChild {
        self.child.clone()
    }

    /// Return a shared reference to the master PTY (used for reading output and resizing).
    pub fn master_arc(&self) -> SharedMasterPty {
        self.master.clone()
    }

    /// Check if the child process is still alive.
    /// Uses `try_lock()` to avoid blocking Tokio workers.
    pub fn is_child_alive(&self) -> bool {
        match self.child.try_lock() {
            Ok(mut c) => c.try_wait().ok().flatten().is_none(),
            Err(std::sync::TryLockError::WouldBlock) => true,
            Err(std::sync::TryLockError::Poisoned(e)) => {
                tracing::warn!(error = %e, "child mutex poisoned in is_alive");
                false
            }
        }
    }

    /// Clone the PTY reader for use by the persistent reader thread.
    pub fn clone_reader(&self) -> anyhow::Result<Box<dyn Read + Send>> {
        let master = self.master.lock()
            .map_err(|e| anyhow::anyhow!("master mutex poisoned: {}", e))?;
        master.try_clone_reader().map_err(|e| anyhow::anyhow!("failed to clone PTY reader: {}", e))
    }
}
