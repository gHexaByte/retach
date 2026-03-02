use crate::protocol::{self, ClientMsg, ServerMsg, READ_BUF_SIZE};
use crate::screen::{Screen, RenderCache};
use crate::session::SessionManager;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tracing::{debug, warn};

/// Lock a `StdMutex` and convert poisoning into `anyhow::Error`.
fn lock_mutex<'a, T>(mutex: &'a StdMutex<T>, label: &str) -> anyhow::Result<std::sync::MutexGuard<'a, T>> {
    mutex.lock().map_err(|e| anyhow::anyhow!("{} mutex poisoned: {}", label, e))
}

/// Per-session I/O handles extracted from SessionManager.
/// Cheap Arc clones — no global lock needed during I/O.
pub struct SessionIo {
    pub pty_writer: Arc<StdMutex<Box<dyn Write + Send>>>,
    pub screen: Arc<StdMutex<Screen>>,
}

impl SessionIo {
    /// Check whether the PTY child process is still running.
    pub fn is_alive(child: &Arc<StdMutex<Box<dyn portable_pty::Child + Send + Sync>>>) -> bool {
        match child.lock() {
            Ok(mut c) => c.try_wait().ok().flatten().is_none(),
            Err(e) => {
                warn!(error = %e, "child mutex poisoned in is_alive");
                false
            }
        }
    }

    /// Resize the PTY master and the virtual screen to the given dimensions.
    pub fn resize(
        master: &Arc<StdMutex<Box<dyn portable_pty::MasterPty + Send>>>,
        screen: &Arc<StdMutex<Screen>>,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<()> {
        let (cols, rows) = crate::screen::grid::sanitize_dimensions(cols, rows);
        let m = lock_mutex(master, "master")?;
        m.resize(portable_pty::PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        lock_mutex(screen, "screen")?.resize(cols, rows);
        Ok(())
    }
}

/// Render the screen and send the update to the client.
async fn render_and_send(
    screen: &Arc<StdMutex<Screen>>,
    cache: &mut RenderCache,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    full: bool,
) -> anyhow::Result<()> {
    let update = lock_mutex(screen, "screen")?.render(full, cache);
    let msg = protocol::encode(&ServerMsg::ScreenUpdate(update))?;
    writer.write_all(&msg).await?;
    Ok(())
}

/// Write terminal responses (DSR, DA, etc.) back to PTY stdin.
fn write_pty_responses(
    pty_writer: &Arc<StdMutex<Box<dyn Write + Send>>>,
    responses: &[Vec<u8>],
    session_name: &str,
) {
    if responses.is_empty() {
        return;
    }
    match pty_writer.lock() {
        Ok(mut w) => {
            for response in responses {
                if let Err(e) = w.write_all(response) {
                    warn!(session = %session_name, error = %e, "failed to write response to PTY");
                    break;
                }
            }
            if let Err(e) = w.flush() {
                warn!(session = %session_name, error = %e, "failed to flush PTY response");
            }
        }
        Err(e) => {
            warn!(session = %session_name, error = %e, "PTY writer mutex poisoned for responses");
        }
    }
}

/// Send encoded scrollback lines to the client.
async fn send_scrollback(
    lines: Vec<Vec<u8>>,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
) -> anyhow::Result<()> {
    for line in lines {
        let msg = protocol::encode(&ServerMsg::ScrollbackLine(line))?;
        writer.write_all(&msg).await?;
    }
    Ok(())
}

/// Handles returned from `setup_session`, containing everything needed for the I/O loops.
struct SessionSetup {
    io: SessionIo,
    child_arc: Arc<StdMutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    master_arc: Arc<StdMutex<Box<dyn portable_pty::MasterPty + Send>>>,
    pty_reader: Box<dyn Read + Send>,
    is_new_session: bool,
    evict_rx: tokio::sync::watch::Receiver<bool>,
    dims_arc: Arc<StdMutex<(u16, u16)>>,
    reader_exited: Arc<tokio::sync::Notify>,
}

/// Acquire or create the session, set up eviction, resize, and clone PTY handles.
/// Returns all handles needed for the I/O loops, or sends an error to the client.
async fn setup_session(
    stream: &mut tokio::net::UnixStream,
    manager: &Arc<Mutex<SessionManager>>,
    name: &str,
    history: usize,
    cols: u16,
    rows: u16,
    mode: crate::protocol::ConnectMode,
) -> anyhow::Result<SessionSetup> {
    let mut mgr = manager.lock().await;

    use crate::protocol::ConnectMode;
    let (session, is_new) = match mode {
        ConnectMode::CreateOrAttach => {
            match mgr.get_or_create(name, cols, rows, history) {
                Ok(s) => s,
                Err(e) => {
                    let resp = protocol::encode(&ServerMsg::Error(format!("{}", e)))?;
                    stream.write_all(&resp).await?;
                    return Err(e);
                }
            }
        }
        ConnectMode::CreateOnly => {
            if mgr.get(name).is_some() {
                let resp = protocol::encode(&ServerMsg::Error(format!("session '{}' already exists", name)))?;
                stream.write_all(&resp).await?;
                anyhow::bail!("session '{}' already exists", name);
            }
            if let Err(e) = mgr.create(name.to_string(), cols, rows, history) {
                let resp = protocol::encode(&ServerMsg::Error(format!("{}", e)))?;
                stream.write_all(&resp).await?;
                return Err(e);
            }
            (mgr.get(name).unwrap(), true)
        }
        ConnectMode::AttachOnly => {
            match mgr.get(name) {
                Some(s) => (s, false),
                None => {
                    let resp = protocol::encode(&ServerMsg::Error(format!("session '{}' not found", name)))?;
                    stream.write_all(&resp).await?;
                    anyhow::bail!("session '{}' not found", name);
                }
            }
        }
    };

    // Evict previous client if one is attached
    if let Some(old_tx) = session.evict_tx.take() {
        debug!(session = %name, "evicting previous client");
        let _ = old_tx.send(false);
        // Wait for the old connection's PTY reader thread to exit so we don't
        // have two readers racing for PTY data from the same master fd.
        let reader_exited = session.reader_exited.clone();
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            reader_exited.notified(),
        ).await;
    }

    // Create eviction channel for this client
    let (evict_tx, evict_rx) = tokio::sync::watch::channel(true);
    session.evict_tx = Some(evict_tx);

    // Resize existing session to match the connecting client's terminal size
    let (cur_cols, cur_rows) = session.dims.lock().map(|d| *d).unwrap_or((80, 24));
    if !is_new && (cur_cols != cols || cur_rows != rows) {
        debug!(
            session = %name,
            old_cols = cur_cols, old_rows = cur_rows,
            new_cols = cols, new_rows = rows,
            "resizing session for reattach"
        );
        let master = session.pty.master_arc();
        let screen = session.screen.clone();
        if let Err(e) = SessionIo::resize(&master, &screen, cols, rows) {
            warn!(session = %name, error = %e, "failed to resize on reattach");
        } else if let Ok(mut dims) = session.dims.lock() {
            *dims = crate::screen::grid::sanitize_dimensions(cols, rows);
        }
    }

    let io = SessionIo {
        pty_writer: session.pty.writer.clone(),
        screen: session.screen.clone(),
    };
    let child_arc = session.pty.child_arc();
    let master_arc = session.pty.master_arc();
    let dims_arc = session.dims.clone();

    // Clone a fresh PTY reader for this connection.
    let pty_reader: Box<dyn Read + Send> = match lock_mutex(&master_arc, "master")
        .and_then(|m| m.try_clone_reader().map_err(|e| anyhow::anyhow!("{}", e)))
    {
        Ok(r) => r,
        Err(e) => {
            let removed = if is_new {
                warn!(session = %name, error = %e, "setup failed, removing new session");
                mgr.remove(name)
            } else {
                None
            };
            drop(mgr); // release async Mutex before Session::Drop
            if let Some(session) = removed {
                tokio::task::spawn_blocking(move || drop(session));
            }
            return Err(e);
        }
    };

    let reader_exited = session.reader_exited.clone();

    Ok(SessionSetup { io, child_arc, master_arc, pty_reader, is_new_session: is_new, evict_rx, dims_arc, reader_exited })
}

/// Send Connected message, scrollback history, and initial screen state.
/// Returns the render_cache for subsequent incremental renders.
async fn send_initial_state(
    io: &SessionIo,
    name: &str,
    is_new_session: bool,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
) -> anyhow::Result<RenderCache> {
    let connected = protocol::encode(&ServerMsg::Connected { name: name.to_string(), new_session: is_new_session })?;
    writer.write_all(&connected).await?;

    let mut render_cache = RenderCache::new();
    let (hist_chunks, screen_msg) = {
        let screen = lock_mutex(&io.screen, "screen")?;
        let hist = screen.get_history();
        let screen_msg = protocol::encode(&ServerMsg::ScreenUpdate(screen.render(true, &mut render_cache)))?;
        (hist, screen_msg)
    };

    if !hist_chunks.is_empty() {
        let mut chunk = Vec::new();
        let mut chunk_size = 0;
        let size_limit = protocol::codec::MAX_FRAME_SIZE / 2;

        for line in hist_chunks {
            let line_size = line.len() + 16;
            if chunk_size + line_size > size_limit && !chunk.is_empty() {
                let msg = protocol::encode(&ServerMsg::History(std::mem::take(&mut chunk)))?;
                writer.write_all(&msg).await?;
                chunk_size = 0;
            }
            chunk_size += line_size;
            chunk.push(line);
        }
        if !chunk.is_empty() {
            let msg = protocol::encode(&ServerMsg::History(chunk))?;
            writer.write_all(&msg).await?;
        }
    }
    writer.write_all(&screen_msg).await?;

    // Drain stale pending scrollback so the PTY→client loop starts clean.
    {
        let mut screen = lock_mutex(&io.screen, "screen")?;
        let _ = screen.take_pending_scrollback();
    }

    Ok(render_cache)
}

/// PTY → client relay loop: reads PTY output, processes it through the screen,
/// and sends rendered updates to the client.
#[allow(clippy::too_many_arguments)]
async fn pty_to_client(
    pty_reader: Box<dyn Read + Send>,
    screen: Arc<StdMutex<Screen>>,
    pty_writer: Arc<StdMutex<Box<dyn Write + Send>>>,
    child_arc: Arc<StdMutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    mut render_cache: RenderCache,
    refresh_notify: Arc<tokio::sync::Notify>,
    mut evict_rx: tokio::sync::watch::Receiver<bool>,
    mut writer: tokio::net::unix::OwnedWriteHalf,
    session_name: String,
    reader_exited: Arc<tokio::sync::Notify>,
) -> anyhow::Result<()> {
    use tokio::sync::mpsc;

    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);

    // Each connection gets its own cloned reader fd.
    tokio::task::spawn_blocking(move || {
        let mut reader: Box<dyn Read + Send> = pty_reader;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => { debug!("pty reader EOF"); break; }
                Ok(n) => {
                    // Use try_send so the thread exits promptly when the
                    // receiver is dropped (on eviction), instead of blocking.
                    match tx.try_send(buf[..n].to_vec()) {
                        Ok(()) => {}
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                            debug!("pty reader channel closed");
                            break;
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                            // Channel full — use blocking_send as back-pressure.
                            if tx.blocking_send(buf[..n].to_vec()).is_err() {
                                debug!("pty reader channel closed");
                                break;
                            }
                        }
                    }
                }
                Err(e) => { debug!(error = %e, "pty read error"); break; }
            }
        }
        debug!("pty reader thread exiting");
        // Signal that this reader is done so the next connection can safely
        // clone a new reader without racing for PTY data.
        reader_exited.notify_one();
    });

    let is_alive = || SessionIo::is_alive(&child_arc);
    let min_interval = std::time::Duration::from_millis(16);
    let mut last_render = std::time::Instant::now() - min_interval;
    let mut screen_dirty = false;

    loop {
        let data = if screen_dirty {
            let remaining = min_interval.saturating_sub(last_render.elapsed());
            if remaining.is_zero() {
                render_and_send(&screen, &mut render_cache, &mut writer, false).await?;
                last_render = std::time::Instant::now();
                screen_dirty = false;
                continue;
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Some(bytes)) => bytes,
                Ok(None) => {
                    debug!(session = %session_name, alive = is_alive(), "pty channel closed");
                    if screen_dirty {
                        render_and_send(&screen, &mut render_cache, &mut writer, false).await?;
                    }
                    if !is_alive() {
                        let msg = protocol::encode(&ServerMsg::SessionEnded)?;
                        writer.write_all(&msg).await?;
                    }
                    break;
                }
                Err(_) => {
                    render_and_send(&screen, &mut render_cache, &mut writer, false).await?;
                    last_render = std::time::Instant::now();
                    screen_dirty = false;
                    continue;
                }
            }
        } else {
            tokio::select! {
                data = rx.recv() => {
                    match data {
                        Some(bytes) => bytes,
                        None => {
                            debug!(session = %session_name, alive = is_alive(), "pty channel closed (idle)");
                            if !is_alive() {
                                let msg = protocol::encode(&ServerMsg::SessionEnded)?;
                                writer.write_all(&msg).await?;
                            }
                            break;
                        }
                    }
                }
                _ = refresh_notify.notified() => {
                    render_and_send(&screen, &mut render_cache, &mut writer, true).await?;
                    last_render = std::time::Instant::now();
                    continue;
                }
                _ = evict_rx.changed() => {
                    debug!(session = %session_name, "client evicted by new connection");
                    let msg = protocol::encode(&ServerMsg::Error("evicted by new client".into()))?;
                    let _ = writer.write_all(&msg).await;
                    break;
                }
            }
        };

        let (scrollback_lines, responses) = {
            let mut scr = lock_mutex(&screen, "screen")?;
            scr.process(&data);
            (scr.take_pending_scrollback(), scr.take_responses())
        };

        write_pty_responses(&pty_writer, &responses, &session_name);
        let had_scrollback = !scrollback_lines.is_empty();
        send_scrollback(scrollback_lines, &mut writer).await?;

        if had_scrollback {
            render_cache.invalidate();
        }

        screen_dirty = true;

        if last_render.elapsed() >= min_interval {
            render_and_send(&screen, &mut render_cache, &mut writer, false).await?;
            last_render = std::time::Instant::now();
            screen_dirty = false;
        }

        if !is_alive() {
            debug!(session = %session_name, "child process died, draining remaining output");
            let mut had_drain_scrollback = false;
            while let Ok(remaining) = rx.try_recv() {
                let lines = {
                    let mut scr = lock_mutex(&screen, "screen")?;
                    scr.process(&remaining);
                    scr.take_pending_scrollback()
                };
                if !lines.is_empty() { had_drain_scrollback = true; }
                send_scrollback(lines, &mut writer).await?;
            }
            if had_drain_scrollback {
                render_cache.invalidate();
            }
            render_and_send(&screen, &mut render_cache, &mut writer, false).await?;
            let msg = protocol::encode(&ServerMsg::SessionEnded)?;
            writer.write_all(&msg).await?;
            break;
        }
    }
    Ok(())
}

/// Client → PTY relay loop: reads client messages and dispatches them.
#[allow(clippy::too_many_arguments)]
async fn client_to_pty(
    mut reader: tokio::net::unix::OwnedReadHalf,
    pty_writer: Arc<StdMutex<Box<dyn Write + Send>>>,
    master_arc: Arc<StdMutex<Box<dyn portable_pty::MasterPty + Send>>>,
    screen: Arc<StdMutex<Screen>>,
    dims_arc: Arc<StdMutex<(u16, u16)>>,
    refresh_notify: Arc<tokio::sync::Notify>,
    leftover: Vec<u8>,
    name: String,
) -> anyhow::Result<()> {
    let mut buf = vec![0u8; READ_BUF_SIZE];
    let mut read_buf = leftover;

    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            debug!(session = %name, "client socket closed");
            break;
        }
        read_buf.extend_from_slice(&buf[..n]);

        let mut cursor = 0;
        while let Some((data, consumed)) = protocol::decode_frame(&read_buf[cursor..])? {
            let msg: ClientMsg = crate::protocol::decode(data)?;
            cursor += consumed;

            match msg {
                ClientMsg::Input(input) => {
                    let mut w = lock_mutex(&pty_writer, "pty_writer")?;
                    w.write_all(&input)?;
                    w.flush()?;
                }
                ClientMsg::Resize { cols, rows } => {
                    SessionIo::resize(&master_arc, &screen, cols, rows)?;
                    if let Ok(mut dims) = dims_arc.lock() {
                        *dims = crate::screen::grid::sanitize_dimensions(cols, rows);
                    }
                }
                ClientMsg::RefreshScreen => {
                    refresh_notify.notify_one();
                }
                ClientMsg::Detach => {
                    debug!(session = %name, "client detached");
                    return Ok(());
                }
                _ => {}
            }
        }
        if cursor > 0 {
            read_buf.drain(..cursor);
        }
    }
    Ok(())
}

/// Bridge a connected client to a session, relaying PTY output and client input bidirectionally.
#[allow(clippy::too_many_arguments)]
pub async fn handle_session(
    mut stream: tokio::net::UnixStream,
    manager: Arc<Mutex<SessionManager>>,
    name: String,
    history: usize,
    cols: u16,
    rows: u16,
    leftover: Vec<u8>,
    mode: crate::protocol::ConnectMode,
) -> anyhow::Result<()> {
    let setup = setup_session(&mut stream, &manager, &name, history, cols, rows, mode).await?;
    // Manager lock dropped — not held during I/O

    let (reader, mut writer) = stream.into_split();

    let render_cache = send_initial_state(&setup.io, &name, setup.is_new_session, &mut writer).await?;

    let refresh_notify = Arc::new(tokio::sync::Notify::new());

    let mut pty_to_client_task = tokio::spawn(pty_to_client(
        setup.pty_reader,
        setup.io.screen.clone(),
        setup.io.pty_writer.clone(),
        setup.child_arc,
        render_cache,
        refresh_notify.clone(),
        setup.evict_rx,
        writer,
        name.clone(),
        setup.reader_exited,
    ));

    let mut client_to_pty_task = tokio::spawn(client_to_pty(
        reader,
        setup.io.pty_writer,
        setup.master_arc,
        setup.io.screen,
        setup.dims_arc,
        refresh_notify,
        leftover,
        name,
    ));

    tokio::select! {
        r = &mut pty_to_client_task => {
            debug!("pty_to_client finished: {:?}", r.as_ref().map(|r| r.as_ref().map(|_| "ok")));
            client_to_pty_task.abort();
            r??;
        }
        r = &mut client_to_pty_task => {
            debug!("client_to_pty finished: {:?}", r.as_ref().map(|r| r.as_ref().map(|_| "ok")));
            pty_to_client_task.abort();
            r??;
        }
    }

    Ok(())
}
