//! Client-side logic for connecting to the retach server, managing sessions, and relaying I/O.

pub mod raw_mode;
pub mod server_launcher;

use crate::protocol::{self, ClientMsg, ServerMsg, FrameReader, read_one_message};
use std::io::{self, BufWriter, Read, Write};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

use raw_mode::RawMode;
use server_launcher::ensure_server_running;

/// Detach key: Ctrl+\ (0x1c).
const DETACH_KEY: u8 = 0x1c;
/// Focus-in event: ESC [ I
const FOCUS_IN: u8 = b'I';
/// Focus-out event: ESC [ O
const FOCUS_OUT: u8 = b'O';

/// RAII guard that removes the custom panic hook on drop.
struct PanicHookGuard;

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        let _ = std::panic::take_hook();
    }
}

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
            stdout.flush()?;
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
        other => {
            tracing::debug!("ignoring unexpected server message: {:?}", std::mem::discriminant(other));
        }
    }
    Ok(DispatchResult::Continue)
}

fn get_terminal_size() -> (u16, u16) {
    if let Some(size) = terminal_size::terminal_size() {
        (size.0 .0, size.1 .0)
    } else {
        (crate::session::DEFAULT_COLS, crate::session::DEFAULT_ROWS)
    }
}

type SocketWriter = std::sync::Arc<tokio::sync::Mutex<tokio::net::unix::OwnedWriteHalf>>;

/// Stdin → socket relay: reads stdin, handles detach key and focus events,
/// and forwards input to the server.
async fn run_stdin_to_socket(sw: SocketWriter) -> anyhow::Result<()> {
    // Carry buffer for ESC sequences split across read() boundaries.
    // Can hold up to 2 bytes: either a lone `\x1b` or `\x1b[`.
    let mut carry: Vec<u8> = Vec::with_capacity(2);

    loop {
        let result = tokio::task::spawn_blocking(|| {
            let mut buf = [0u8; 1024];
            let n = io::stdin().read(&mut buf)?;
            Ok::<_, io::Error>((buf, n))
        })
        .await;

        match result {
            Ok(Ok((_buf, 0))) => {
                // Flush any carry bytes on stdin EOF
                if !carry.is_empty() {
                    let msg = protocol::encode(&ClientMsg::Input(std::mem::take(&mut carry)))?;
                    let mut w = sw.lock().await;
                    w.write_all(&msg).await?;
                }
                break;
            }
            Ok(Ok((buf, n))) => {
                // Prepend any carry bytes from previous iteration.
                let raw: Vec<u8> = if carry.is_empty() {
                    buf[..n].to_vec()
                } else {
                    let mut combined = std::mem::take(&mut carry);
                    combined.extend_from_slice(&buf[..n]);
                    combined
                };

                // Check for detach key (Ctrl+\, byte 0x1c)
                if let Some(pos) = raw.iter().position(|&b| b == DETACH_KEY) {
                    let mut w = sw.lock().await;
                    if pos > 0 {
                        if let Ok(msg) = protocol::encode(&ClientMsg::Input(raw[..pos].to_vec())) {
                            w.write_all(&msg).await?;
                        }
                    }
                    if let Ok(msg) = protocol::encode(&ClientMsg::Detach) {
                        w.write_all(&msg).await?;
                    }
                    drop(w);
                    break;
                }
                // Filter focus-in (ESC [ I) and focus-out (ESC [ O) sequences.
                let mut filtered = Vec::with_capacity(raw.len());
                let mut i = 0;
                while i < raw.len() {
                    if raw[i] == 0x1b {
                        if i + 1 < raw.len() {
                            if raw[i + 1] == b'[' {
                                if i + 2 < raw.len() {
                                    if raw[i + 2] == FOCUS_IN {
                                        // Focus-in: send refresh instead
                                        if let Ok(msg) = protocol::encode(&ClientMsg::RefreshScreen) {
                                            let mut w = sw.lock().await;
                                            let _ = w.write_all(&msg).await;
                                        }
                                        i += 3;
                                        continue;
                                    } else if raw[i + 2] == FOCUS_OUT {
                                        // Focus-out: drop silently
                                        i += 3;
                                        continue;
                                    }
                                    // ESC [ <other> — not a focus event, pass through
                                } else {
                                    // ESC [ at end of buffer — valid prefix, carry
                                    carry.extend_from_slice(&raw[i..]);
                                    break;
                                }
                            } else {
                                // ESC followed by non-[, pass both through immediately
                                filtered.push(raw[i]);
                                i += 1;
                                continue;
                            }
                        } else {
                            // Lone ESC at end of buffer — carry
                            carry.push(0x1b);
                            break;
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
    let mut frames = FrameReader::with_leftover(leftover);
    let mut stdout = BufWriter::new(io::stdout());

    // Process any complete frames already in the leftover buffer
    while let Some(msg) = frames.decode_next::<ServerMsg>()? {
        if matches!(dispatch_server_msg(&msg, &mut stdout)?, DispatchResult::Done) {
            return Ok(());
        }
    }
    stdout.flush()?;

    loop {
        if !frames.fill_from(&mut sock_reader).await? {
            eprintln!("[retach: detached]");
            break;
        }
        while let Some(msg) = frames.decode_next::<ServerMsg>()? {
            if matches!(dispatch_server_msg(&msg, &mut stdout)?, DispatchResult::Done) {
                return Ok(());
            }
        }
        stdout.flush()?;
    }
    Ok(())
}

/// Connect to (or create) a named session and enter interactive raw-mode I/O.
pub async fn connect(name: &str, history: usize, mode: crate::protocol::ConnectMode) -> anyhow::Result<()> {
    ensure_server_running().await?;

    let mut stream = UnixStream::connect(crate::server::socket_path()?).await?;

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
    let mut frames = FrameReader::new();
    loop {
        if !frames.fill_from(&mut stream).await? {
            eprintln!("[retach: server closed connection]");
            return Ok(());
        }
        if let Some(msg) = frames.decode_next::<ServerMsg>()? {
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
    let leftover = frames.into_leftover();

    // Install panic hook to restore terminal even if we panic while in raw mode.
    // The guard ensures the hook is removed on all exit paths (including early returns).
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        raw_mode::emergency_restore();
        cleanup_terminal();
        prev_hook(info);
    }));
    let _hook_guard = PanicHookGuard;

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
                if let Err(e) = w.write_all(&msg).await {
                    tracing::debug!(error = %e, "failed to send refresh after resize");
                    break;
                }
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
    drop(_hook_guard);

    cleanup_terminal();

    Ok(())
}

/// Reset terminal state after detach/disconnect so the user's shell isn't left
/// with hidden cursor, mouse capture, bracketed paste, etc.
fn cleanup_terminal() {
    let mut stdout = io::stdout();
    let _ = stdout.write_all(concat!(
        "\x1b[?25h",   // show cursor
        "\x1b[?7h",    // re-enable auto-wrap
        "\x1b[?1l",    // normal cursor keys (DECCKM reset)
        "\x1b[?2004l", // disable bracketed paste
        "\x1b[?1000l", // disable mouse click tracking
        "\x1b[?1002l", // disable mouse button tracking
        "\x1b[?1003l", // disable mouse any-event tracking
        "\x1b[?1005l", // disable UTF-8 mouse encoding
        "\x1b[?1006l", // disable SGR mouse encoding
        "\x1b[?1004l", // disable focus reporting
        "\x1b[?2026l", // disable synchronized output
        "\x1b>",       // normal keypad mode (DECKPNM)
        "\x1b[0 q",    // default cursor shape
        "\x1b[0m",     // reset all SGR attributes
    ).as_bytes());
    let _ = stdout.flush();
}

/// Query the server for active sessions and print them to stdout.
pub async fn list_sessions() -> anyhow::Result<()> {
    let path = crate::server::socket_path()?;
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
    let path = crate::server::socket_path()?;
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
