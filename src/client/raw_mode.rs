use nix::sys::termios;
use std::io;
use std::os::fd::BorrowedFd;
use std::os::unix::io::AsRawFd;
use std::sync::{Mutex as StdMutex, OnceLock};

/// Global storage for original termios so we can restore even from panic hooks
/// or signal handlers where the `RawMode` RAII guard may not run `Drop`.
static ORIGINAL_TERMIOS: OnceLock<StdMutex<Option<(i32, termios::Termios)>>> = OnceLock::new();

fn get_or_init_global() -> &'static StdMutex<Option<(i32, termios::Termios)>> {
    ORIGINAL_TERMIOS.get_or_init(|| StdMutex::new(None))
}

/// Best-effort restoration of terminal mode from the global backup.
/// Safe to call from panic hooks and signal handlers.
pub fn emergency_restore() {
    let lock = get_or_init_global();
    if let Ok(guard) = lock.lock() {
        if let Some((fd, ref original)) = *guard {
            // SAFETY: fd is stdin (fd 0), which remains valid for the process
            // lifetime. The global is only populated while RawMode is active.
            let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
            let _ = termios::tcsetattr(borrowed, termios::SetArg::TCSANOW, original);
        }
    }
}

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

        // Store in global BEFORE entering raw mode so emergency_restore()
        // can recover even if we panic between tcsetattr and return.
        if let Ok(mut guard) = get_or_init_global().lock() {
            *guard = Some((fd, original.clone()));
        }

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

        // Clear the global — the RAII guard is being dropped so the backup would be stale
        if let Ok(mut guard) = get_or_init_global().lock() {
            *guard = None;
        }
    }
}
