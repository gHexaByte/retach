use tokio::net::UnixStream;

/// Spawn the daemon server process if it is not already listening, then wait for it to be ready.
///
/// Uses a lockfile with `flock(LOCK_EX | LOCK_NB)` to prevent two clients from
/// racing to spawn a server simultaneously (TOCTOU on the socket check).
pub async fn ensure_server_running() -> anyhow::Result<()> {
    let path = crate::server::socket_path()?;
    if UnixStream::connect(&path).await.is_ok() {
        return Ok(());
    }

    // Acquire exclusive lock to serialize server spawning across clients.
    let lock_path = crate::server::socket::lock_path()?;
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)?;
    let _lock_guard = match nix::fcntl::Flock::lock(lock_file, nix::fcntl::FlockArg::LockExclusiveNonblock) {
        Ok(guard) => guard, // we hold the lock (released on drop)
        Err((_, nix::errno::Errno::EWOULDBLOCK)) => {
            // Another client is already spawning the server — wait for it to appear.
            for _ in 0..50 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if UnixStream::connect(&path).await.is_ok() {
                    return Ok(());
                }
            }
            anyhow::bail!("timed out waiting for another client to start server");
        }
        Err((_, e)) => anyhow::bail!("failed to acquire startup lock: {}", e),
    };

    // Double-check: another server may have started between our first check and
    // acquiring the lock.
    if UnixStream::connect(&path).await.is_ok() {
        // _lock_guard drops here, releasing the flock
        return Ok(());
    }

    let exe = std::env::current_exe()?;
    let log_path = path.with_file_name("retach.log");
    let log_file_stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    use std::os::unix::process::CommandExt;
    unsafe {
        let mut child = std::process::Command::new(exe)
            .arg("server")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::from(log_file_stderr))
            .pre_exec(|| {
                // Create new session: detach from controlling terminal
                // and process group so SIGHUP from SSH disconnect won't kill us
                if nix::libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            })
            .spawn()?;
        // Reap the child in a background thread to avoid zombie processes
        std::thread::spawn(move || { let _ = child.wait(); });
    }

    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if UnixStream::connect(&path).await.is_ok() {
            // _lock_guard drops here, releasing the flock
            return Ok(());
        }
    }

    // _lock_guard drops here, releasing the flock
    anyhow::bail!("failed to start server");
}
