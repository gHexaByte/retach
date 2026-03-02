use nix::sys::termios;
use std::io;
use std::os::fd::BorrowedFd;
use std::os::unix::io::AsRawFd;

/// RAII guard for raw terminal mode. Restores original termios on drop.
pub struct RawMode {
    original: termios::Termios,
    fd: i32,
}

impl RawMode {
    pub fn enter() -> anyhow::Result<Self> {
        let fd = io::stdin().as_raw_fd();
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let original = termios::tcgetattr(borrowed)?;
        let mut raw = original.clone();
        termios::cfmakeraw(&mut raw);
        termios::tcsetattr(borrowed, termios::SetArg::TCSANOW, &raw)?;
        Ok(Self { original, fd })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let borrowed = unsafe { BorrowedFd::borrow_raw(self.fd) };
        if let Err(e) = termios::tcsetattr(borrowed, termios::SetArg::TCSANOW, &self.original) {
            tracing::warn!(error = %e, "failed to restore terminal mode");
        }
    }
}
