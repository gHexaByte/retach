use crate::protocol::{self, ClientMsg, ServerMsg, FrameReader};
use crate::screen::{Screen, RenderCache};
use crate::session::{SessionManager, is_child_alive};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::io::AsyncWriteExt;
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
/// Runs on a blocking thread to avoid stalling Tokio workers when the PTY buffer is full.
async fn write_pty_responses(
    pty_writer: &Arc<StdMutex<Box<dyn Write + Send>>>,
    responses: Vec<Vec<u8>>,
    session_name: String,
) {
    if responses.is_empty() {
        return;
    }
    let pw = pty_writer.clone();
    let _ = tokio::task::spawn_blocking(move || {
        match pw.lock() {
            Ok(mut w) => {
                for response in &responses {
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
    }).await;
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
    reader_exited: Arc<tokio::sync::Semaphore>,
}

/// Acquire or create the session, set up eviction, resize, and clone PTY handles.
/// Returns all handles needed for the I/O loops, or sends an error to the client.
///
/// Lock strategy: the SessionManager lock is released before waiting for the
/// old reader thread to exit (up to 2s), then re-acquired for the rest of setup.
/// This prevents blocking all other clients during the wait.
async fn setup_session(
    stream: &mut tokio::net::UnixStream,
    manager: &Arc<Mutex<SessionManager>>,
    name: &str,
    history: usize,
    cols: u16,
    rows: u16,
    mode: crate::protocol::ConnectMode,
) -> anyhow::Result<SessionSetup> {
    // Phase 1: Acquire lock, find/create session, start eviction (fast).
    let (had_eviction, reader_exited_sem) = {
        let mut mgr = manager.lock().await;

        use crate::protocol::ConnectMode;
        let (session, _is_new) = match mode {
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

        // Send eviction signal while holding the lock (fast, non-blocking).
        // Clone the semaphore Arc so we can wait outside the lock.
        if let Some(old_tx) = session.evict_tx.take() {
            debug!(session = %name, "evicting previous client");
            let _ = old_tx.send(false);
            (true, Some(session.reader_exited.clone()))
        } else {
            (false, None)
        }
        // mgr lock released here
    };

    // Phase 2: Wait for old reader to exit WITHOUT holding the manager lock.
    if let Some(reader_exited) = reader_exited_sem {
        let wait_result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            reader_exited.acquire(),
        ).await;
        match wait_result {
            Ok(Ok(permit)) => permit.forget(), // consume the permit
            Ok(Err(_)) => {
                let resp = protocol::encode(&ServerMsg::Error("internal error: reader semaphore closed".into()))?;
                stream.write_all(&resp).await?;
                anyhow::bail!("reader_exited semaphore closed for session '{}'", name);
            }
            Err(_) => {
                let resp = protocol::encode(&ServerMsg::Error("previous connection still draining, try again".into()))?;
                stream.write_all(&resp).await?;
                anyhow::bail!("timed out waiting for previous PTY reader to exit for session '{}'", name);
            }
        };
    }

    // Phase 3: Re-acquire lock for resize, handle cloning, etc.
    let mut mgr = manager.lock().await;

    // Session must still exist (could have been killed while we waited).
    let session = match mgr.get(name) {
        Some(s) => s,
        None => {
            let resp = protocol::encode(&ServerMsg::Error(format!("session '{}' was removed during reconnect", name)))?;
            stream.write_all(&resp).await?;
            anyhow::bail!("session '{}' was removed during reconnect", name);
        }
    };

    let is_new = !had_eviction && session.evict_tx.is_none();

    // Create eviction channel for this client
    let (evict_tx, evict_rx) = tokio::sync::watch::channel(true);
    session.evict_tx = Some(evict_tx);

    // Resize existing session to match the connecting client's terminal size
    let (cur_cols, cur_rows) = match session.dims.lock() {
        Ok(d) => *d,
        Err(e) => {
            warn!(session = %name, error = %e, "dims mutex poisoned during reattach");
            (80, 24)
        }
    };
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
        } else {
            match session.dims.lock() {
                Ok(mut dims) => *dims = crate::screen::grid::sanitize_dimensions(cols, rows),
                Err(e) => warn!(session = %name, error = %e, "dims mutex poisoned during resize"),
            }
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
        let mut render_data = Vec::new();
        // After the client writes history lines with \r\n, up to `rows - 1`
        // lines remain on the visible screen (the final \r\n already scrolled
        // one line off, leaving the cursor on a blank bottom row).  Prepend
        // newlines to flush them into the real terminal's scrollback buffer
        // before the screen clear erases them.
        if !hist.is_empty() {
            // Position cursor at the bottom row first so that each \n
            // reliably triggers one scroll, regardless of initial cursor position.
            use crate::screen::style::write_u16;
            render_data.extend_from_slice(b"\x1b[");
            write_u16(&mut render_data, screen.grid.rows);
            render_data.extend_from_slice(b";1H");
            render_data.extend(std::iter::repeat_n(b'\n', screen.grid.rows.saturating_sub(1) as usize));
        }
        render_data.extend_from_slice(&screen.render(true, &mut render_cache));
        let screen_msg = protocol::encode(&ServerMsg::ScreenUpdate(render_data))?;
        (hist, screen_msg)
    };

    if !hist_chunks.is_empty() {
        let mut chunk = Vec::new();
        let mut chunk_size = 0;
        let size_limit = protocol::codec::MAX_FRAME_SIZE / 2;

        for line in hist_chunks {
            // Estimate per-line bincode overhead: 8 bytes for Vec length prefix +
            // ~8 bytes for enum variant tag and alignment padding.
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
    reader_exited: Arc<tokio::sync::Semaphore>,
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
                    let data = buf[..n].to_vec();
                    match tx.try_send(data) {
                        Ok(()) => {}
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                            debug!("pty reader channel closed");
                            break;
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Full(data)) => {
                            // Channel full — use blocking_send as back-pressure.
                            if tx.blocking_send(data).is_err() {
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
        reader_exited.add_permits(1);
    });

    let is_alive = || is_child_alive(&child_arc);
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

        let (scrollback_lines, responses, passthrough) = {
            let mut scr = lock_mutex(&screen, "screen")?;
            scr.process(&data);
            (scr.take_pending_scrollback(), scr.take_responses(), scr.take_passthrough())
        };

        write_pty_responses(&pty_writer, responses, session_name.clone()).await;

        for chunk in passthrough {
            let msg = protocol::encode(&ServerMsg::Passthrough(chunk))?;
            writer.write_all(&msg).await?;
        }

        if !scrollback_lines.is_empty() {
            // Scrollback lines require an immediate atomic render: cursor
            // positioning + scrollback injection + full screen redraw, all
            // inside one synchronized-output block.
            let update = lock_mutex(&screen, "screen")?
                .render_with_scrollback(&scrollback_lines, &mut render_cache);
            let msg = protocol::encode(&ServerMsg::ScreenUpdate(update))?;
            writer.write_all(&msg).await?;
            last_render = std::time::Instant::now();
            screen_dirty = false;
        } else {
            screen_dirty = true;
            if last_render.elapsed() >= min_interval {
                render_and_send(&screen, &mut render_cache, &mut writer, false).await?;
                last_render = std::time::Instant::now();
                screen_dirty = false;
            }
        }

        if !is_alive() {
            debug!(session = %session_name, "child process died, draining remaining output");
            drain_and_close(&mut rx, &screen, &mut render_cache, &mut writer).await?;
            break;
        }
    }
    Ok(())
}

/// Drain remaining PTY output after child process death, render final state, and send SessionEnded.
async fn drain_and_close(
    rx: &mut tokio::sync::mpsc::Receiver<Vec<u8>>,
    screen: &Arc<StdMutex<Screen>>,
    render_cache: &mut RenderCache,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
) -> anyhow::Result<()> {
    let mut all_drain_scrollback = Vec::new();
    while let Ok(remaining) = rx.try_recv() {
        let (lines, passthrough) = {
            let mut scr = lock_mutex(screen, "screen")?;
            scr.process(&remaining);
            (scr.take_pending_scrollback(), scr.take_passthrough())
        };
        all_drain_scrollback.extend(lines);
        for chunk in passthrough {
            let msg = protocol::encode(&ServerMsg::Passthrough(chunk))?;
            writer.write_all(&msg).await?;
        }
    }
    if !all_drain_scrollback.is_empty() {
        let update = lock_mutex(screen, "screen")?
            .render_with_scrollback(&all_drain_scrollback, render_cache);
        let msg = protocol::encode(&ServerMsg::ScreenUpdate(update))?;
        writer.write_all(&msg).await?;
    } else {
        render_and_send(screen, render_cache, writer, false).await?;
    }
    let msg = protocol::encode(&ServerMsg::SessionEnded)?;
    writer.write_all(&msg).await?;
    Ok(())
}

/// Client → PTY relay loop: reads client messages and dispatches them.
#[allow(clippy::too_many_arguments)]
async fn client_to_pty(
    mut sock_reader: tokio::net::unix::OwnedReadHalf,
    pty_writer: Arc<StdMutex<Box<dyn Write + Send>>>,
    master_arc: Arc<StdMutex<Box<dyn portable_pty::MasterPty + Send>>>,
    screen: Arc<StdMutex<Screen>>,
    dims_arc: Arc<StdMutex<(u16, u16)>>,
    refresh_notify: Arc<tokio::sync::Notify>,
    leftover: Vec<u8>,
    name: String,
) -> anyhow::Result<()> {
    let mut frames = FrameReader::with_leftover(leftover);

    loop {
        if !frames.fill_from(&mut sock_reader).await? {
            debug!(session = %name, "client socket closed");
            break;
        }
        while let Some(msg) = frames.decode_next::<ClientMsg>()? {
            match msg {
                ClientMsg::Input(input) => {
                    let pw = pty_writer.clone();
                    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                        let mut w = lock_mutex(&pw, "pty_writer")?;
                        w.write_all(&input)?;
                        w.flush()?;
                        Ok(())
                    }).await??;
                }
                ClientMsg::Resize { cols, rows } => {
                    SessionIo::resize(&master_arc, &screen, cols, rows)?;
                    match dims_arc.lock() {
                        Ok(mut dims) => *dims = crate::screen::grid::sanitize_dimensions(cols, rows),
                        Err(e) => warn!(session = %name, error = %e, "dims mutex poisoned during client resize"),
                    }
                }
                ClientMsg::RefreshScreen => {
                    refresh_notify.notify_one();
                }
                ClientMsg::Detach => {
                    debug!(session = %name, "client detached");
                    return Ok(());
                }
                other => {
                    debug!(session = %name, "ignoring unexpected client message: {:?}", std::mem::discriminant(&other));
                }
            }
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
