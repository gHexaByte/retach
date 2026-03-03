//! Client-side logic for connecting to the retach server, managing sessions, and relaying I/O.

pub mod raw_mode;
pub mod server_launcher;

use crate::protocol::{self, ClientMsg, ServerMsg, read_one_message, READ_BUF_SIZE};
use std::io::{self, BufWriter, Read, Write};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use raw_mode::RawMode;
use server_launcher::ensure_server_running;

/// Result of dispatching a server message to stdout.
enum DispatchResult {
    Continue,
    Done,
}

/// Write a single ServerMsg to stdout. Returns `Done` for terminal messages.
fn dispatch_server_msg(msg: &ServerMsg, stdout: &mut impl Write) -> io::Result<DispatchResult> {
    match msg {
        ServerMsg::ScrollbackLine(line) => {
            stdout.write_all(line)?;
            stdout.write_all(b"\r\n")?;
        }
        ServerMsg::ScreenUpdate(data) => {
            stdout.write_all(data)?;
        }
        ServerMsg::Passthrough(data) => {
            stdout.write_all(data)?;
        }
        ServerMsg::History(lines) => {
            for line in lines {
                stdout.write_all(line)?;
                stdout.write_all(b"\r\n")?;
            }
        }
        ServerMsg::SessionEnded => {
            stdout.flush()?;
            eprintln!("[retach: session ended]");
            return Ok(DispatchResult::Done);
        }
        ServerMsg::Error(e) => {
            stdout.flush()?;
            eprintln!("[retach error: {}]", e);
            return Ok(DispatchResult::Done);
        }
        _ => {}
    }
    Ok(DispatchResult::Continue)
}

fn get_terminal_size() -> (u16, u16) {
    if let Some((w, h)) = term_size::dimensions() {
        (w as u16, h as u16)
    } else {
        (80, 24)
    }
}

type SocketWriter = std::sync::Arc<tokio::sync::Mutex<tokio::net::unix::OwnedWriteHalf>>;

/// Stdin → socket relay: reads stdin, handles detach key and focus events,
/// and forwards input to the server.
async fn run_stdin_to_socket(sw: SocketWriter) -> anyhow::Result<()> {
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
                    if pos > 0 {
                        if let Ok(msg) = protocol::encode(&ClientMsg::Input(buf[..pos].to_vec())) {
                            let mut w = sw.lock().await;
                            w.write_all(&msg).await?;
                        }
                    }
                    if let Ok(msg) = protocol::encode(&ClientMsg::Detach) {
                        let mut w = sw.lock().await;
                        w.write_all(&msg).await?;
                    }
                    break;
                }
                // Filter focus-in (ESC [ I) and focus-out (ESC [ O) sequences.
                let mut filtered = Vec::with_capacity(n);
                let raw = &buf[..n];
                let mut i = 0;
                while i < raw.len() {
                    if i + 2 < raw.len() && raw[i] == 0x1b && raw[i + 1] == 0x5b {
                        if raw[i + 2] == 0x49 {
                            if let Ok(msg) = protocol::encode(&ClientMsg::RefreshScreen) {
                                let mut w = sw.lock().await;
                                let _ = w.write_all(&msg).await;
                            }
                            i += 3;
                            continue;
                        } else if raw[i + 2] == 0x4f {
                            i += 3;
                            continue;
                        }
                    }
                    filtered.push(raw[i]);
                    i += 1;
                }
                if !filtered.is_empty() {
                    let msg = protocol::encode(&ClientMsg::Input(filtered))?;
                    let mut w = sw.lock().await;
                    w.write_all(&msg).await?;
                }
            }
            Ok(Err(e)) => return Err(anyhow::Error::from(e)),
            Err(e) => return Err(anyhow::Error::from(e)),
        }
    }
    Ok(())
}

/// Socket → stdout relay: reads server messages, dispatches them to stdout.
async fn run_socket_to_stdout(
    mut sock_reader: tokio::net::unix::OwnedReadHalf,
    leftover: Vec<u8>,
) -> anyhow::Result<()> {
    let mut buf = vec![0u8; READ_BUF_SIZE];
    let mut read_buf = leftover;
    let mut stdout = BufWriter::new(io::stdout());

    // Process any complete frames already in the leftover buffer
    let mut cursor = 0;
    while let Some((data, consumed)) = protocol::decode_frame(&read_buf[cursor..])? {
        let msg: ServerMsg = crate::protocol::decode(data)?;
        cursor += consumed;
        if matches!(dispatch_server_msg(&msg, &mut stdout)?, DispatchResult::Done) {
            return Ok(());
        }
    }
    if cursor > 0 {
        read_buf.drain(..cursor);
    }
    stdout.flush()?;

    loop {
        let n = sock_reader.read(&mut buf).await?;
        if n == 0 {
            eprintln!("[retach: detached]");
            break;
        }
        read_buf.extend_from_slice(&buf[..n]);

        let mut cursor = 0;
        while let Some((data, consumed)) = protocol::decode_frame(&read_buf[cursor..])? {
            let msg: ServerMsg = crate::protocol::decode(data)?;
            cursor += consumed;
            if matches!(dispatch_server_msg(&msg, &mut stdout)?, DispatchResult::Done) {
                return Ok(());
            }
        }
        if cursor > 0 {
            read_buf.drain(..cursor);
        }
        stdout.flush()?;
    }
    Ok(())
}

/// Connect to (or create) a named session and enter interactive raw-mode I/O.
pub async fn connect(name: &str, history: usize, mode: crate::protocol::ConnectMode) -> anyhow::Result<()> {
    ensure_server_running().await?;

    let mut stream = UnixStream::connect(crate::server::socket_path()).await?;

    let (cols, rows) = get_terminal_size();
    let msg = protocol::encode(&ClientMsg::Connect {
        name: name.to_string(),
        history,
        cols,
        rows,
        mode,
    })?;
    stream.write_all(&msg).await?;

    // Wait for Connected/Error before entering raw mode so errors display correctly.
    let mut initial_buf = vec![0u8; READ_BUF_SIZE];
    let mut leftover = Vec::new();
    loop {
        let n = stream.read(&mut initial_buf).await?;
        if n == 0 {
            eprintln!("[retach: server closed connection]");
            return Ok(());
        }
        leftover.extend_from_slice(&initial_buf[..n]);
        if let Some((data, consumed)) = protocol::decode_frame(&leftover)? {
            let msg: ServerMsg = crate::protocol::decode(data)?;
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

    // Install panic hook to restore terminal even if we panic while in raw mode.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        raw_mode::emergency_restore();
        cleanup_terminal();
        prev_hook(info);
    }));

    let _raw = RawMode::enter()?;

    let (sock_reader, sock_writer) = stream.into_split();
    let sock_writer = std::sync::Arc::new(tokio::sync::Mutex::new(sock_writer));

    // SIGWINCH handler
    let mut sigwinch =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
    let sw = sock_writer.clone();
    let sigwinch_handle = tokio::spawn(async move {
        while sigwinch.recv().await.is_some() {
            let (cols, rows) = get_terminal_size();
            let mut w = sw.lock().await;
            if let Ok(msg) = protocol::encode(&ClientMsg::Resize { cols, rows }) {
                if let Err(e) = w.write_all(&msg).await {
                    tracing::debug!(error = %e, "failed to send resize");
                    break;
                }
            }
            // Always request full refresh after SIGWINCH — the real terminal
            // may have lost its display buffer (e.g. app minimized/restored,
            // SSH connection resumed). The server invalidates its render cache
            // and redraws all rows.
            if let Ok(msg) = protocol::encode(&ClientMsg::RefreshScreen) {
                let _ = w.write_all(&msg).await;
            }
        }
    });

    let stdin_task = tokio::spawn(run_stdin_to_socket(sock_writer.clone()));
    let socket_task = tokio::spawn(run_socket_to_stdout(sock_reader, leftover));

    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

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
        _ = sigint.recv() => {
            tracing::debug!("received SIGINT, detaching");
        }
        _ = sigterm.recv() => {
            tracing::debug!("received SIGTERM, detaching");
        }
    }

    sigwinch_handle.abort();
    let _ = std::panic::take_hook();

    cleanup_terminal();

    Ok(())
}

/// Reset terminal state after detach/disconnect so the user's shell isn't left
/// with hidden cursor, mouse capture, bracketed paste, etc.
fn cleanup_terminal() {
    let mut stdout = io::stdout();
    let _ = stdout.write_all(
        b"\x1b[?25h\x1b[?7h\x1b[?1l\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1004l\x1b[?2026l\x1b>\x1b[0 q\x1b[0m",
    );
    let _ = stdout.flush();
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
