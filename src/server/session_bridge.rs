use crate::protocol::{self, ClientMsg, ServerMsg};
use crate::screen::Screen;
use crate::session::SessionManager;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tracing::{debug, warn};

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
        let m = master.lock().map_err(|e| anyhow::anyhow!("master mutex poisoned: {}", e))?;
        m.resize(portable_pty::PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        screen
            .lock()
            .map_err(|e| anyhow::anyhow!("screen mutex poisoned: {}", e))?
            .resize(cols, rows);
        Ok(())
    }
}

/// Bridge a connected client to a session, relaying PTY output and client input bidirectionally.
pub async fn handle_session(
    mut stream: tokio::net::UnixStream,
    manager: Arc<Mutex<SessionManager>>,
    name: String,
    history: usize,
    cols: u16,
    rows: u16,
    leftover: Vec<u8>,
) -> anyhow::Result<()> {
    let (io_handles, child_arc, master_arc, pty_reader, is_new_session) = {
        let mut mgr = manager.lock().await;
        let (session, is_new) = match mgr.get_or_create(&name, cols, rows, history) {
            Ok(s) => s,
            Err(e) => {
                // Send error to client before closing connection
                let resp = protocol::encode(&ServerMsg::Error(format!("{}", e)))?;
                stream.write_all(&resp).await?;
                return Err(e);
            }
        };

        // Resize existing session to match the connecting client's terminal size
        if !is_new && (session.cols != cols || session.rows != rows) {
            debug!(
                session = %name,
                old_cols = session.cols, old_rows = session.rows,
                new_cols = cols, new_rows = rows,
                "resizing session for reattach"
            );
            let master = session.pty.master_arc();
            let screen = session.screen.clone();
            if let Err(e) = SessionIo::resize(&master, &screen, cols, rows) {
                warn!(session = %name, error = %e, "failed to resize on reattach");
                // Non-fatal: continue with old dimensions
            } else {
                session.cols = cols;
                session.rows = rows;
            }
        }

        let io_handles = SessionIo {
            pty_writer: session.pty.writer.clone(),
            screen: session.screen.clone(),
        };

        let child = session.pty.child_arc();
        let master = session.pty.master_arc();

        // Clone a fresh PTY reader for this connection.
        // Each connection gets its own reader fd — the old connection's
        // blocking reader thread will exit naturally when its channel closes.
        let pty_reader: Box<dyn Read + Send> = master.lock()
            .map_err(|e| anyhow::anyhow!("master mutex poisoned: {}", e))?
            .try_clone_reader()?;

        (io_handles, child, master, pty_reader, is_new)
    };
    // Manager lock dropped — not held during I/O

    let (mut reader, mut writer) = stream.into_split();

    // Send Connected
    let connected = protocol::encode(&ServerMsg::Connected { name: name.clone(), new_session: is_new_session })?;
    writer.write_all(&connected).await?;

    // Send history + current screen
    let (hist_msg, screen_msg) = {
        let screen = io_handles.screen.lock()
            .map_err(|e| anyhow::anyhow!("screen mutex poisoned: {}", e))?;
        let hist = screen.get_history();
        let hist_msg = if !hist.is_empty() {
            Some(protocol::encode(&ServerMsg::History(hist))?)
        } else {
            None
        };
        let screen_msg = protocol::encode(&ServerMsg::ScreenUpdate(screen.render(true)))?;
        (hist_msg, screen_msg)
    };
    if let Some(msg) = hist_msg {
        writer.write_all(&msg).await?;
    }
    writer.write_all(&screen_msg).await?;

    let pty_writer_for_responses = io_handles.pty_writer.clone();
    let screen_for_read = io_handles.screen.clone();
    let child_for_alive = child_arc.clone();
    let session_name = name.clone();

    // PTY → client
    let mut pty_to_client = tokio::spawn(async move {
        use tokio::sync::mpsc;

        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);

        // Each connection gets its own cloned reader fd.
        // When this connection ends, the channel closes, blocking_send fails,
        // and the thread exits — dropping this reader clone.
        tokio::task::spawn_blocking(move || {
            let mut reader = pty_reader;
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => { debug!("pty reader EOF"); break; }
                    Ok(n) => {
                        if tx.blocking_send(buf[..n].to_vec()).is_err() {
                            debug!("pty reader channel closed");
                            break;
                        }
                    }
                    Err(e) => { debug!(error = %e, "pty read error"); break; }
                }
            }
            debug!("pty reader thread exiting");
        });

        let is_alive = || SessionIo::is_alive(&child_for_alive);
        let min_interval = std::time::Duration::from_millis(16);
        let mut last_render = std::time::Instant::now() - min_interval;
        let mut screen_dirty = false;

        loop {
            let data = if screen_dirty {
                let remaining = min_interval.saturating_sub(last_render.elapsed());
                if remaining.is_zero() {
                    let screen_update = screen_for_read.lock()
                        .map_err(|e| anyhow::anyhow!("screen mutex poisoned: {}", e))?
                        .render(false);
                    let msg = protocol::encode(&ServerMsg::ScreenUpdate(screen_update))?;
                    writer.write_all(&msg).await?;
                    last_render = std::time::Instant::now();
                    screen_dirty = false;
                    continue;
                }
                match tokio::time::timeout(remaining, rx.recv()).await {
                    Ok(Some(bytes)) => bytes,
                    Ok(None) => {
                        debug!(session = %session_name, alive = is_alive(), "pty channel closed");
                        if screen_dirty {
                            let screen_update = screen_for_read.lock()
                                .map_err(|e| anyhow::anyhow!("screen mutex poisoned: {}", e))?
                                .render(false);
                            let msg = protocol::encode(&ServerMsg::ScreenUpdate(screen_update))?;
                            writer.write_all(&msg).await?;
                        }
                        if !is_alive() {
                            let msg = protocol::encode(&ServerMsg::SessionEnded)?;
                            writer.write_all(&msg).await?;
                        }
                        break;
                    }
                    Err(_) => {
                        let screen_update = screen_for_read.lock()
                            .map_err(|e| anyhow::anyhow!("screen mutex poisoned: {}", e))?
                            .render(false);
                        let msg = protocol::encode(&ServerMsg::ScreenUpdate(screen_update))?;
                        writer.write_all(&msg).await?;
                        last_render = std::time::Instant::now();
                        screen_dirty = false;
                        continue;
                    }
                }
            } else {
                match rx.recv().await {
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
            };

            let (scrollback_lines, responses) = {
                let mut screen = screen_for_read.lock()
                    .map_err(|e| anyhow::anyhow!("screen mutex poisoned: {}", e))?;
                screen.process(&data);
                (screen.take_pending_scrollback(), screen.take_responses())
            };

            // Write responses (DSR, DA, etc.) back to PTY stdin
            if !responses.is_empty() {
                match pty_writer_for_responses.lock() {
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
            }

            for line in scrollback_lines {
                let msg = protocol::encode(&ServerMsg::ScrollbackLine(line))?;
                writer.write_all(&msg).await?;
            }

            screen_dirty = true;

            if last_render.elapsed() >= min_interval {
                let screen_update = screen_for_read.lock()
                    .map_err(|e| anyhow::anyhow!("screen mutex poisoned: {}", e))?
                    .render(false);
                let msg = protocol::encode(&ServerMsg::ScreenUpdate(screen_update))?;
                writer.write_all(&msg).await?;
                last_render = std::time::Instant::now();
                screen_dirty = false;
            }

            if !is_alive() {
                debug!(session = %session_name, "child process died, draining remaining output");
                // Drain any remaining data from the channel before sending SessionEnded
                while let Ok(remaining) = rx.try_recv() {
                    let (scrollback_lines, _responses) = {
                        let mut screen = screen_for_read.lock()
                            .map_err(|e| anyhow::anyhow!("screen mutex poisoned: {}", e))?;
                        screen.process(&remaining);
                        (screen.take_pending_scrollback(), screen.take_responses())
                    };
                    for line in scrollback_lines {
                        let msg = protocol::encode(&ServerMsg::ScrollbackLine(line))?;
                        writer.write_all(&msg).await?;
                    }
                }
                // Final render
                let screen_update = screen_for_read.lock()
                    .map_err(|e| anyhow::anyhow!("screen mutex poisoned: {}", e))?
                    .render(false);
                let msg = protocol::encode(&ServerMsg::ScreenUpdate(screen_update))?;
                writer.write_all(&msg).await?;
                let msg = protocol::encode(&ServerMsg::SessionEnded)?;
                writer.write_all(&msg).await?;
                break;
            }
        }
        Ok::<_, anyhow::Error>(())
    });

    let pty_writer = io_handles.pty_writer;
    let master_for_resize = master_arc;
    let screen_for_resize = io_handles.screen.clone();

    // Client → PTY
    let mut client_to_pty = tokio::spawn(async move {
        let mut buf = vec![0u8; 65536];
        let mut read_buf = leftover;

        loop {
            let n = reader.read(&mut buf).await?;
            if n == 0 {
                debug!(session = %name, "client socket closed");
                break;
            }
            read_buf.extend_from_slice(&buf[..n]);

            while let Some((data, consumed)) = protocol::decode_frame(&read_buf)? {
                let msg: ClientMsg = bincode::deserialize(data)?;
                read_buf.drain(..consumed);

                match msg {
                    ClientMsg::Input(input) => {
                        let mut w = pty_writer.lock()
                            .map_err(|e| anyhow::anyhow!("pty writer mutex poisoned: {}", e))?;
                        w.write_all(&input)?;
                        w.flush()?;
                    }
                    ClientMsg::Resize { cols, rows } => {
                        SessionIo::resize(&master_for_resize, &screen_for_resize, cols, rows)?;
                    }
                    ClientMsg::Detach => {
                        debug!(session = %name, "client detached");
                        return Ok::<_, anyhow::Error>(());
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    });

    tokio::select! {
        r = &mut pty_to_client => {
            debug!("pty_to_client finished: {:?}", r.as_ref().map(|r| r.as_ref().map(|_| "ok")));
            client_to_pty.abort();
            r??;
        }
        r = &mut client_to_pty => {
            debug!("client_to_pty finished: {:?}", r.as_ref().map(|r| r.as_ref().map(|_| "ok")));
            // Don't abort pty_to_client — let it finish naturally so the
            // blocking reader thread can return the PTY reader to the pool.
            // It will exit when it tries to write to the closed socket.
            drop(pty_to_client);
            r??;
        }
    }

    Ok(())
}
