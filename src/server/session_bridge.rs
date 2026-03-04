use crate::protocol::{self, ClientMsg, ServerMsg, FrameReader};
use crate::screen::{Screen, RenderCache};
use crate::session::SessionManager;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use crate::session::{DEFAULT_COLS, DEFAULT_ROWS};
use tracing::{debug, warn};

/// Minimum interval between consecutive screen renders to the client.
const RENDER_THROTTLE: std::time::Duration = std::time::Duration::from_millis(16);

/// Lock a `StdMutex` and convert poisoning into `anyhow::Error`.
fn lock_mutex<'a, T>(mutex: &'a StdMutex<T>, label: &str) -> anyhow::Result<std::sync::MutexGuard<'a, T>> {
    mutex.lock().map_err(|e| anyhow::anyhow!("{} mutex poisoned: {}", label, e))
}

/// Resize the PTY master and the virtual screen to the given dimensions.
fn resize_pty(
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

/// Shared session handles, passed to relay loops by Arc-clone.
#[derive(Clone)]
struct SessionHandles {
    screen: Arc<StdMutex<Screen>>,
    pty_writer: Arc<StdMutex<Box<dyn Write + Send>>>,
    master: Arc<StdMutex<Box<dyn portable_pty::MasterPty + Send>>>,
    dims: Arc<StdMutex<(u16, u16)>>,
    screen_notify: Arc<tokio::sync::Notify>,
    has_client: Arc<AtomicBool>,
    reader_alive: Arc<AtomicBool>,
    name: String,
}

/// Handles returned from `setup_session`, containing everything needed for the I/O loops.
struct SessionSetup {
    handles: SessionHandles,
    is_new_session: bool,
    evict_rx: tokio::sync::watch::Receiver<bool>,
}

/// Parameters for a session connection request.
pub struct ConnectRequest {
    pub name: String,
    pub history: usize,
    pub cols: u16,
    pub rows: u16,
    pub leftover: Vec<u8>,
    pub mode: crate::protocol::ConnectMode,
}

/// Acquire or create the session, set up eviction, resize, and extract handles.
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

    // Mark client as connected before eviction so the reader thread doesn't
    // discard data intended for the new client.
    session.has_client.store(true, Ordering::Release);

    // Evict previous client if any.
    if let Some(old_tx) = session.evict_tx.take() {
        debug!(session = %name, "evicting previous client");
        let _ = old_tx.send(false);
    }

    // Create eviction channel for this client
    let (evict_tx, evict_rx) = tokio::sync::watch::channel(true);
    session.evict_tx = Some(evict_tx);

    // Resize existing session to match the connecting client's terminal size
    let (cur_cols, cur_rows) = match session.dims.lock() {
        Ok(d) => *d,
        Err(e) => {
            warn!(session = %name, error = %e, "dims mutex poisoned during reattach");
            (DEFAULT_COLS, DEFAULT_ROWS)
        }
    };
    if !is_new {
        let master = session.pty.master_arc();
        let screen = session.screen.clone();
        if cur_cols != cols || cur_rows != rows {
            debug!(
                session = %name,
                old_cols = cur_cols, old_rows = cur_rows,
                new_cols = cols, new_rows = rows,
                "resizing session for reattach"
            );
            if let Err(e) = resize_pty(&master, &screen, cols, rows) {
                warn!(session = %name, error = %e, "failed to resize on reattach");
            } else {
                match session.dims.lock() {
                    Ok(mut dims) => *dims = crate::screen::grid::sanitize_dimensions(cols, rows),
                    Err(e) => warn!(session = %name, error = %e, "dims mutex poisoned during resize"),
                }
            }
        } else {
            // Same dimensions: send SIGWINCH as a safety net.  The persistent
            // reader keeps the VTE parser in sync, but SIGWINCH still helps
            // apps that cache display state internally (e.g. htop) to do a
            // full redraw.
            debug!(session = %name, "sending SIGWINCH for reattach (same dimensions)");
            let (cols, rows) = crate::screen::grid::sanitize_dimensions(cols, rows);
            if let Err(e) = lock_mutex(&master, "master").and_then(|m| {
                m.resize(portable_pty::PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                }).map_err(|e| anyhow::anyhow!("{}", e))
            }) {
                warn!(session = %name, error = %e, "failed to send SIGWINCH on reattach");
            }
        }
    }

    let handles = SessionHandles {
        screen: session.screen.clone(),
        pty_writer: session.pty.writer.clone(),
        master: session.pty.master_arc(),
        dims: session.dims.clone(),
        screen_notify: session.screen_notify.clone(),
        has_client: session.has_client.clone(),
        reader_alive: session.reader_alive.clone(),
        name: name.to_string(),
    };

    Ok(SessionSetup { handles, is_new_session: is_new, evict_rx })
}

/// Send Connected message, scrollback history, and initial screen state.
/// Returns the render_cache for subsequent incremental renders.
async fn send_initial_state(
    handles: &SessionHandles,
    is_new_session: bool,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
) -> anyhow::Result<RenderCache> {
    let connected = protocol::encode(&ServerMsg::Connected { name: handles.name.clone(), new_session: is_new_session })?;
    writer.write_all(&connected).await?;

    let mut render_cache = RenderCache::new();
    let (hist_chunks, screen_msg) = {
        let screen = lock_mutex(&handles.screen, "screen")?;
        // Skip history injection when in alt screen (e.g. htop, vim).
        // The scrollback is from the main screen and not relevant while the
        // alt screen app is running.  Re-injecting it on every reconnect
        // would accumulate duplicate lines in the outer terminal's scrollback.
        let hist = if screen.in_alt_screen() {
            Vec::new()
        } else {
            screen.get_history()
        };
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

    // Drain stale pending scrollback so the screen→client loop starts clean.
    {
        let mut screen = lock_mutex(&handles.screen, "screen")?;
        let _ = screen.take_pending_scrollback();
        let _ = screen.take_passthrough();
    }

    Ok(render_cache)
}

/// Screen → client relay loop: waits for the persistent reader to signal new
/// data, then renders and sends updates to the client.
async fn screen_to_client(
    h: SessionHandles,
    mut render_cache: RenderCache,
    refresh_notify: Arc<tokio::sync::Notify>,
    mut evict_rx: tokio::sync::watch::Receiver<bool>,
    mut writer: tokio::net::unix::OwnedWriteHalf,
) -> anyhow::Result<()> {
    use std::pin::pin;
    use std::time::Duration;
    use tokio::time::Instant;

    // If the reader is already dead (child exited before we connected),
    // send final state and SessionEnded immediately.
    if !h.reader_alive.load(Ordering::Acquire) {
        render_and_send(&h.screen, &mut render_cache, &mut writer, true).await?;
        let msg = protocol::encode(&ServerMsg::SessionEnded)?;
        writer.write_all(&msg).await?;
        h.has_client.store(false, Ordering::Release);
        return Ok(());
    }

    let mut throttle_sleep = pin!(tokio::time::sleep(Duration::ZERO));
    let mut pending_render = false;

    loop {
        tokio::select! {
            _ = h.screen_notify.notified() => {
                if !h.reader_alive.load(Ordering::Acquire) {
                    // Reader exited (PTY EOF). Do a final render + send SessionEnded.
                    let (scrollback_lines, passthrough) = {
                        let mut scr = lock_mutex(&h.screen, "screen")?;
                        (scr.take_pending_scrollback(), scr.take_passthrough())
                    };
                    for chunk in passthrough {
                        let msg = protocol::encode(&ServerMsg::Passthrough(chunk))?;
                        writer.write_all(&msg).await?;
                    }
                    if !scrollback_lines.is_empty() {
                        let update = lock_mutex(&h.screen, "screen")?
                            .render_with_scrollback(&scrollback_lines, &mut render_cache);
                        let msg = protocol::encode(&ServerMsg::ScreenUpdate(update))?;
                        writer.write_all(&msg).await?;
                    } else {
                        render_and_send(&h.screen, &mut render_cache, &mut writer, false).await?;
                    }
                    let msg = protocol::encode(&ServerMsg::SessionEnded)?;
                    writer.write_all(&msg).await?;
                    break;
                }
                pending_render = true;
                throttle_sleep.as_mut().reset(Instant::now() + RENDER_THROTTLE);
            }
            _ = &mut throttle_sleep, if pending_render => {
                let (scrollback_lines, passthrough) = {
                    let mut scr = lock_mutex(&h.screen, "screen")?;
                    (scr.take_pending_scrollback(), scr.take_passthrough())
                };

                for chunk in passthrough {
                    let msg = protocol::encode(&ServerMsg::Passthrough(chunk))?;
                    writer.write_all(&msg).await?;
                }

                if !scrollback_lines.is_empty() {
                    let update = lock_mutex(&h.screen, "screen")?
                        .render_with_scrollback(&scrollback_lines, &mut render_cache);
                    let msg = protocol::encode(&ServerMsg::ScreenUpdate(update))?;
                    writer.write_all(&msg).await?;
                } else {
                    render_and_send(&h.screen, &mut render_cache, &mut writer, false).await?;
                }
                pending_render = false;
            }
            _ = refresh_notify.notified() => {
                render_and_send(&h.screen, &mut render_cache, &mut writer, true).await?;
            }
            _ = evict_rx.changed() => {
                debug!(session = %h.name, "client evicted by new connection");
                let msg = protocol::encode(&ServerMsg::Error("evicted by new client".into()))?;
                let _ = writer.write_all(&msg).await;
                break;
            }
        }
    }
    // Only clear has_client if we were NOT evicted.  When evicted, the new
    // client already set has_client=true and clearing it here would race with
    // the new connection, causing the persistent reader to drain pending data.
    // evict_rx initial value is `true`; eviction sends `false`.
    if *evict_rx.borrow() {
        h.has_client.store(false, Ordering::Release);
    }
    Ok(())
}

/// Client → PTY relay loop: reads client messages and dispatches them.
async fn client_to_pty(
    h: SessionHandles,
    mut sock_reader: tokio::net::unix::OwnedReadHalf,
    refresh_notify: Arc<tokio::sync::Notify>,
    leftover: Vec<u8>,
) -> anyhow::Result<()> {
    let mut frames = FrameReader::with_leftover(leftover);

    loop {
        if !frames.fill_from(&mut sock_reader).await? {
            debug!(session = %h.name, "client socket closed");
            break;
        }
        while let Some(msg) = frames.decode_next::<ClientMsg>()? {
            match msg {
                ClientMsg::Input(input) => {
                    let pw = h.pty_writer.clone();
                    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                        let mut w = lock_mutex(&pw, "pty_writer")?;
                        w.write_all(&input)?;
                        w.flush()?;
                        Ok(())
                    }).await??;
                }
                ClientMsg::Resize { cols, rows } => {
                    let master_clone = h.master.clone();
                    let screen_clone = h.screen.clone();
                    let dims_clone = h.dims.clone();
                    let name_clone = h.name.clone();
                    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                        resize_pty(&master_clone, &screen_clone, cols, rows)?;
                        match dims_clone.lock() {
                            Ok(mut dims) => *dims = crate::screen::grid::sanitize_dimensions(cols, rows),
                            Err(e) => warn!(session = %name_clone, error = %e, "dims mutex poisoned during client resize"),
                        }
                        Ok(())
                    }).await??;
                }
                ClientMsg::RefreshScreen => {
                    refresh_notify.notify_one();
                }
                ClientMsg::Detach => {
                    debug!(session = %h.name, "client detached");
                    return Ok(());
                }
                other => {
                    debug!(session = %h.name, "ignoring unexpected client message: {:?}", std::mem::discriminant(&other));
                }
            }
        }
    }
    Ok(())
}

/// Bridge a connected client to a session, relaying screen updates and client input bidirectionally.
pub async fn handle_session(
    mut stream: tokio::net::UnixStream,
    manager: Arc<Mutex<SessionManager>>,
    req: ConnectRequest,
) -> anyhow::Result<()> {
    let setup = setup_session(&mut stream, &manager, &req.name, req.history, req.cols, req.rows, req.mode).await?;
    // Manager lock dropped — not held during I/O

    let has_client = setup.handles.has_client.clone();
    let (reader, mut writer) = stream.into_split();

    let render_cache = match send_initial_state(&setup.handles, setup.is_new_session, &mut writer).await {
        Ok(cache) => cache,
        Err(e) => {
            has_client.store(false, Ordering::Release);
            return Err(e);
        }
    };

    let refresh_notify = Arc::new(tokio::sync::Notify::new());
    let evict_rx_local = setup.evict_rx.clone();

    let mut screen_to_client_task = tokio::spawn(screen_to_client(
        setup.handles.clone(),
        render_cache,
        refresh_notify.clone(),
        setup.evict_rx,
        writer,
    ));

    let mut client_to_pty_task = tokio::spawn(client_to_pty(
        setup.handles,
        reader,
        refresh_notify,
        req.leftover,
    ));

    tokio::select! {
        r = &mut screen_to_client_task => {
            debug!("screen_to_client finished: {:?}", r.as_ref().map(|r| r.as_ref().map(|_| "ok")));
            client_to_pty_task.abort();
            r??;
        }
        r = &mut client_to_pty_task => {
            debug!("client_to_pty finished: {:?}", r.as_ref().map(|r| r.as_ref().map(|_| "ok")));
            screen_to_client_task.abort();
            // Only clear has_client if we were NOT evicted.  When evicted, the
            // new client already set has_client=true and clearing it here would
            // race with the new connection.
            if *evict_rx_local.borrow() {
                has_client.store(false, Ordering::Release);
            }
            r??;
        }
    }

    Ok(())
}
