//! Client-side logic for connecting to the retach server, managing sessions, and relaying I/O.

pub mod raw_mode;
pub mod server_launcher;

use crate::protocol::{self, ClientMsg, ServerMsg, read_one_message};
use std::io::{self, Read, Write};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use raw_mode::RawMode;
use server_launcher::ensure_server_running;

fn get_terminal_size() -> (u16, u16) {
    if let Some((w, h)) = term_size::dimensions() {
        (w as u16, h as u16)
    } else {
        (80, 24)
    }
}

/// Connect to (or create) a named session and enter interactive raw-mode I/O.
pub async fn connect(name: &str, history: usize) -> anyhow::Result<()> {
    ensure_server_running().await?;

    let mut stream = UnixStream::connect(crate::server::socket_path()).await?;

    let (cols, rows) = get_terminal_size();
    let msg = protocol::encode(&ClientMsg::Connect {
        name: name.to_string(),
        history,
        cols,
        rows,
    })?;
    stream.write_all(&msg).await?;

    // Wait for Connected/Error before entering raw mode so errors display correctly.
    // Read manually to preserve leftover bytes (read_one_message drops them).
    let mut initial_buf = vec![0u8; 65536];
    let mut leftover = Vec::new();
    loop {
        let n = stream.read(&mut initial_buf).await?;
        if n == 0 {
            eprintln!("[retach: server closed connection]");
            return Ok(());
        }
        leftover.extend_from_slice(&initial_buf[..n]);
        if let Some((data, consumed)) = protocol::decode_frame(&leftover)? {
            let msg: ServerMsg = bincode::deserialize(data)?;
            leftover.drain(..consumed);
            match msg {
                ServerMsg::Connected { name: ref session_name, new_session } => {
                    if new_session {
                        eprintln!("[retach: new session '{}' (detach: Ctrl+\\)]", session_name);
                    } else {
                        eprintln!("[retach: reattached to '{}' (detach: Ctrl+\\)]", session_name);
                    }
                    break;
                }
                ServerMsg::Error(e) => {
                    eprintln!("[retach error: {}]", e);
                    return Ok(());
                }
                _ => {
                    eprintln!("[retach: unexpected response from server]");
                    return Ok(());
                }
            }
        }
    }

    let _raw = RawMode::enter()?;

    let (mut sock_reader, sock_writer) = stream.into_split();
    let sock_writer = std::sync::Arc::new(tokio::sync::Mutex::new(sock_writer));

    // SIGWINCH handler
    let mut sigwinch =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
    let sw = sock_writer.clone();
    tokio::spawn(async move {
        while sigwinch.recv().await.is_some() {
            let (cols, rows) = get_terminal_size();
            if let Ok(msg) = protocol::encode(&ClientMsg::Resize { cols, rows }) {
                let mut w = sw.lock().await;
                if let Err(e) = w.write_all(&msg).await {
                    tracing::debug!(error = %e, "failed to send resize");
                    break;
                }
            }
        }
    });

    // Stdin → socket
    let sw2 = sock_writer.clone();
    let stdin_task = tokio::spawn(async move {
        loop {
            let result = tokio::task::spawn_blocking(|| {
                let mut buf = [0u8; 1024];
                let n = io::stdin().read(&mut buf)?;
                Ok::<_, io::Error>((buf, n))
            })
            .await;

            match result {
                Ok(Ok((_buf, 0))) => break,
                Ok(Ok((buf, n))) => {
                    // Check for detach key (Ctrl+\, byte 0x1c)
                    if let Some(pos) = buf[..n].iter().position(|&b| b == 0x1c) {
                        // Fix I4: send bytes before the detach key
                        if pos > 0 {
                            if let Ok(msg) = protocol::encode(&ClientMsg::Input(buf[..pos].to_vec())) {
                                let mut w = sw2.lock().await;
                                w.write_all(&msg).await?;
                            }
                        }
                        if let Ok(msg) = protocol::encode(&ClientMsg::Detach) {
                            let mut w = sw2.lock().await;
                            w.write_all(&msg).await?;
                        }
                        break;
                    }
                    let msg = protocol::encode(&ClientMsg::Input(buf[..n].to_vec()))?;
                    let mut w = sw2.lock().await;
                    w.write_all(&msg).await?;
                }
                Ok(Err(e)) => return Err(anyhow::Error::from(e)),
                Err(e) => return Err(anyhow::Error::from(e)),
            }
        }
        Ok::<_, anyhow::Error>(())
    });

    // Socket → stdout
    let socket_task = tokio::spawn(async move {
        let mut buf = vec![0u8; 65536];
        let mut read_buf = leftover; // preserve bytes read after Connected message
        let mut stdout = io::stdout();

        // Process any complete frames already in the leftover buffer
        while let Some((data, consumed)) = protocol::decode_frame(&read_buf)? {
            let msg: ServerMsg = bincode::deserialize(data)?;
            read_buf.drain(..consumed);

            match &msg {
                ServerMsg::ScrollbackLine(line) => {
                    stdout.write_all(line)?;
                    stdout.write_all(b"\r\n")?;
                    stdout.flush()?;
                }
                ServerMsg::ScreenUpdate(data) => {
                    stdout.write_all(data)?;
                    stdout.flush()?;
                }
                ServerMsg::History(lines) => {
                    for line in lines {
                        stdout.write_all(line)?;
                        stdout.write_all(b"\r\n")?;
                    }
                    stdout.flush()?;
                }
                ServerMsg::SessionEnded => {
                    eprintln!("[retach: session ended]");
                    return Ok(());
                }
                ServerMsg::Error(e) => {
                    eprintln!("[retach error: {}]", e);
                    return Ok(());
                }
                _ => {}
            }
        }

        loop {
            let n = sock_reader.read(&mut buf).await?;
            if n == 0 {
                eprintln!("[retach: detached]");
                break;
            }
            read_buf.extend_from_slice(&buf[..n]);

            while let Some((data, consumed)) = protocol::decode_frame(&read_buf)? {
                let msg: ServerMsg = bincode::deserialize(data)?;
                read_buf.drain(..consumed);

                match msg {
                    ServerMsg::ScrollbackLine(line) => {
                        stdout.write_all(&line)?;
                        stdout.write_all(b"\r\n")?;
                        stdout.flush()?;
                    }
                    ServerMsg::ScreenUpdate(data) => {
                        stdout.write_all(&data)?;
                        stdout.flush()?;
                    }
                    ServerMsg::History(lines) => {
                        for line in lines {
                            stdout.write_all(&line)?;
                            stdout.write_all(b"\r\n")?;
                        }
                        stdout.flush()?;
                    }
                    ServerMsg::Connected { .. } => {
                        // Already handled before entering raw mode
                    }
                    ServerMsg::SessionEnded => {
                        eprintln!("[retach: session ended]");
                        return Ok(());
                    }
                    ServerMsg::Error(e) => {
                        eprintln!("[retach error: {}]", e);
                        return Ok(());
                    }
                    ServerMsg::SessionList(_) | ServerMsg::SessionKilled { .. } => {}
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    });

    tokio::select! {
        r = stdin_task => {
            if let Ok(Err(e)) = r {
                tracing::debug!(error = %e, "stdin task error");
            }
        }
        r = socket_task => {
            if let Ok(Err(e)) = r {
                tracing::warn!(error = %e, "socket task error");
                eprintln!("[retach error: {}]", e);
            }
        }
    }

    Ok(())
}

/// Query the server for active sessions and print them to stdout.
pub async fn list_sessions() -> anyhow::Result<()> {
    let path = crate::server::socket_path();
    if !path.exists() {
        println!("No active sessions");
        return Ok(());
    }

    let mut stream = UnixStream::connect(&path).await?;
    let msg = protocol::encode(&ClientMsg::ListSessions)?;
    stream.write_all(&msg).await?;

    let resp: ServerMsg = read_one_message(&mut stream).await?;
    if let ServerMsg::SessionList(sessions) = resp {
        if sessions.is_empty() {
            println!("No active sessions");
        } else {
            for s in sessions {
                println!("{} ({}x{})", s.name, s.cols, s.rows);
            }
        }
    }
    Ok(())
}

/// Ask the server to terminate the named session.
pub async fn kill_session(name: &str) -> anyhow::Result<()> {
    let path = crate::server::socket_path();
    if !path.exists() {
        anyhow::bail!("server not running");
    }

    let mut stream = UnixStream::connect(&path).await?;
    let msg = protocol::encode(&ClientMsg::KillSession {
        name: name.to_string(),
    })?;
    stream.write_all(&msg).await?;

    let resp: ServerMsg = read_one_message(&mut stream).await?;
    match resp {
        ServerMsg::SessionKilled { name } => println!("killed session '{}'", name),
        ServerMsg::Error(e) => println!("error: {}", e),
        _ => {}
    }
    Ok(())
}
