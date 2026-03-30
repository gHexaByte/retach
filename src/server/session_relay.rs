//! Client relay loops: screen-to-client rendering and client-to-PTY input forwarding.

use crate::protocol::{self, ClientMsg, ServerMsg, FrameReader};
use crate::session::SessionHandles;
use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tracing::debug;

use super::session_bridge::{lock_mutex, prepend_passthrough, render_and_send, RENDER_THROTTLE};
use super::session_setup::resize_pty;

/// Screen -> client relay loop: waits for the persistent reader to signal new
/// data, then renders and sends updates to the client.
pub(super) async fn screen_to_client(
    h: SessionHandles,
    mut render_cache: retach::screen::RenderCache,
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
        return Ok(());
    }

    let mut throttle_sleep = pin!(tokio::time::sleep(Duration::ZERO));
    let mut pending_render = false;

    loop {
        tokio::select! {
            _ = h.screen_notify.notified() => {
                if !h.reader_alive.load(Ordering::Acquire) {
                    // Reader exited (PTY EOF). Do a final render + send SessionEnded.
                    let (render_data, passthrough) = lock_mutex(&h.screen, "screen")?
                        .take_and_render(&mut render_cache);
                    let update = prepend_passthrough(passthrough, render_data);
                    let msg = protocol::encode(&ServerMsg::ScreenUpdate(update))?;
                    writer.write_all(&msg).await?;
                    let msg = protocol::encode(&ServerMsg::SessionEnded)?;
                    writer.write_all(&msg).await?;
                    break;
                }
                pending_render = true;
                throttle_sleep.as_mut().reset(Instant::now() + RENDER_THROTTLE);
            }
            _ = &mut throttle_sleep, if pending_render => {
                let (render_data, passthrough) = lock_mutex(&h.screen, "screen")?
                    .take_and_render(&mut render_cache);
                // Prepend passthrough sequences (e.g. \e[3J) to the screen
                // update so the terminal processes them in a single write.
                // Sending \e[3J as a separate Passthrough message with flush()
                // before ScreenUpdate causes rendering glitches in Blink — the
                // terminal clears the viewport before the new content arrives.
                let update = prepend_passthrough(passthrough, render_data);
                // Skip sending empty updates (no rows dirty, no mode/cursor/title
                // changes). This prevents no-op sync blocks that cause flicker
                // on terminals without DEC 2026 support (e.g. xterm.js).
                if !update.is_empty() {
                    let msg = protocol::encode(&ServerMsg::ScreenUpdate(update))?;
                    writer.write_all(&msg).await?;
                }
                pending_render = false;
            }
            _ = refresh_notify.notified() => {
                render_and_send(&h.screen, &mut render_cache, &mut writer, true).await?;
            }
            result = evict_rx.changed() => {
                match result {
                    Ok(()) => {
                        debug!(session = %h.name, "client evicted by new connection");
                        let msg = protocol::encode(&ServerMsg::Error("evicted by new client".into()))?;
                        if let Err(e) = writer.write_all(&msg).await {
                            debug!(session = %h.name, error = %e, "failed to send eviction notice to client");
                        }
                    }
                    Err(_) => {
                        // Sender dropped — session was killed via KillSession
                        debug!(session = %h.name, "session killed while client connected");
                        let msg = protocol::encode(&ServerMsg::SessionEnded)?;
                        if let Err(e) = writer.write_all(&msg).await {
                            debug!(session = %h.name, error = %e, "failed to send session-ended to killed client");
                        }
                    }
                }
                break;
            }
        }
    }
    // has_client cleanup is handled by ClientGuard in handle_session
    Ok(())
}

/// Client -> PTY relay loop: reads client messages and dispatches them.
pub(super) async fn client_to_pty(
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
                            Ok(mut d) => *d = retach::screen::sanitize_dimensions(cols, rows),
                            Err(e) => tracing::warn!(session = %name_clone, error = %e, "dims mutex poisoned during client resize"),
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
                // Connect, ListSessions, KillSession are handled in client_handler
                // before the session bridge loop — they never reach here.
                ClientMsg::Connect { .. } | ClientMsg::ListSessions | ClientMsg::KillSession { .. } => {
                    tracing::debug!("ignoring unexpected client message in session relay");
                }
            }
        }
    }
    Ok(())
}
