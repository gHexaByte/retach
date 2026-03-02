use tokio::net::UnixStream;

/// Spawn the daemon server process if it is not already listening, then wait for it to be ready.
pub async fn ensure_server_running() -> anyhow::Result<()> {
    let path = crate::server::socket_path();
    if UnixStream::connect(&path).await.is_ok() {
        return Ok(());
    }

    let exe = std::env::current_exe()?;
    let log_path = crate::server::socket_path().with_file_name("retach.log");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    use std::os::unix::process::CommandExt;
    unsafe {
        std::process::Command::new(exe)
            .arg("server")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::from(log_file))
            .pre_exec(|| {
                // Create new session: detach from controlling terminal
                // and process group so SIGHUP from SSH disconnect won't kill us
                nix::libc::setsid();
                Ok(())
            })
            .spawn()?;
    }

    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if UnixStream::connect(&path).await.is_ok() {
            return Ok(());
        }
    }

    anyhow::bail!("failed to start server");
}
