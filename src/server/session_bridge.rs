use crate::protocol::{self, ServerMsg};
use retach::screen::{Screen, RenderCache, TerminalEmulator};
use crate::session::{SessionManager, SessionHandles};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::debug;

use super::session_setup::{setup_session, ConnectRequest};
use super::session_relay::{screen_to_client, client_to_pty};

// Lock ordering (to prevent deadlocks):
//
//   manager (tokio::Mutex)
//     → screen (StdMutex)
//       → master (StdMutex)
//     → pty_writer (StdMutex)  [try_lock in reader loop to avoid deadlock]
//     → dims (StdMutex)
//
// The persistent reader loop (session.rs) uses try_lock on pty_writer
// because client_to_pty may hold it during a blocking write while the
// child process waits for a DA response that the reader needs to deliver.

/// Minimum interval between consecutive screen renders to the client.
/// 16ms ≈ 60fps — fast enough for smooth animation (progress bars, htop)
/// while preventing CPU waste from rendering every PTY read (1000s/sec).
pub(super) const RENDER_THROTTLE: std::time::Duration = std::time::Duration::from_millis(16);

/// Estimated per-line bincode overhead: 8 bytes for Vec length prefix +
/// ~8 bytes for enum variant tag and alignment padding.
const BINCODE_LINE_OVERHEAD: usize = 16;

/// Prepend passthrough escape sequences to the rendered screen data so they
/// are sent as a single `ScreenUpdate` write.  This avoids the intermediate
/// `flush()` that `Passthrough` messages trigger on the client, which can cause
/// rendering glitches in terminals like Blink (e.g. `\e[3J` clearing the
/// viewport before the new screen content arrives).
pub(super) fn prepend_passthrough(passthrough: Vec<Vec<u8>>, render_data: Vec<u8>) -> Vec<u8> {
    if passthrough.is_empty() {
        return render_data;
    }
    let total: usize = passthrough.iter().map(|c| c.len()).sum::<usize>() + render_data.len();
    let mut combined = Vec::with_capacity(total);
    for chunk in passthrough {
        combined.extend_from_slice(&chunk);
    }
    combined.extend_from_slice(&render_data);
    combined
}

/// Lock a `StdMutex` and convert poisoning into `anyhow::Error`.
pub(super) fn lock_mutex<'a, T>(mutex: &'a StdMutex<T>, label: &str) -> anyhow::Result<std::sync::MutexGuard<'a, T>> {
    mutex.lock().map_err(|e| anyhow::anyhow!("{} mutex poisoned: {}", label, e))
}

/// Render the screen and send the update to the client.
pub(super) async fn render_and_send(
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
        let mut screen = lock_mutex(&handles.screen, "screen")?;
        // Skip history injection when in alt screen (e.g. htop, vim).
        // The scrollback is from the main screen and not relevant while the
        // alt screen app is running.  Re-injecting it on every reconnect
        // would accumulate duplicate lines in the outer terminal's scrollback.
        let hist = if screen.in_alt_screen() {
            Vec::new()
        } else {
            screen.get_history()
        };
        let notifications = screen.take_queued_notifications();
        let mut render_data = Vec::new();
        // Prepend queued notifications so the terminal processes them on reconnect
        for notif in notifications {
            render_data.extend_from_slice(&notif);
        }
        // After the client writes history lines with \r\n, up to `rows - 1`
        // lines remain on the visible screen (the final \r\n already scrolled
        // one line off, leaving the cursor on a blank bottom row).  Prepend
        // newlines to flush them into the real terminal's scrollback buffer
        // before the screen clear erases them.
        if !hist.is_empty() {
            // Position cursor at the bottom row first so that each \n
            // reliably triggers one scroll, regardless of initial cursor position.
            use retach::screen::write_u16;
            render_data.extend_from_slice(b"\x1b[");
            write_u16(&mut render_data, screen.rows());
            render_data.extend_from_slice(b";1H");
            // 1-row terminal: 0 newlines — nothing to flush.
            render_data.extend(std::iter::repeat_n(b'\n', screen.rows().saturating_sub(1) as usize));
        }
        render_data.extend_from_slice(&screen.render(true, &mut render_cache));
        let screen_msg = protocol::encode(&ServerMsg::ScreenUpdate(render_data))?;
        (hist, screen_msg)
    };

    if !hist_chunks.is_empty() {
        let mut chunk = Vec::new();
        let mut chunk_size = 0;
        // Leave headroom for bincode framing (length prefix, enum tags)
        let size_limit = protocol::codec::MAX_FRAME_SIZE / 2;

        for line in hist_chunks {
            let line_size = line.len() + BINCODE_LINE_OVERHEAD;
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
        screen.take_pending_scrollback();
        screen.take_passthrough();
    }

    Ok(render_cache)
}

/// Bridge a connected client to a session, relaying screen updates and client input bidirectionally.
pub(super) async fn handle_session(
    mut stream: tokio::net::UnixStream,
    manager: Arc<Mutex<SessionManager>>,
    req: ConnectRequest,
) -> anyhow::Result<()> {
    let setup = setup_session(&mut stream, &manager, &req.name, req.history, req.cols, req.rows, req.mode).await?;
    // Manager lock dropped — not held during I/O

    // ClientGuard clears has_client on drop (unless evicted).
    // Keep it alive until this function returns.
    let _client_guard = setup.client_guard;

    let (reader, mut writer) = stream.into_split();

    let render_cache = send_initial_state(&setup.handles, setup.is_new_session, &mut writer).await?;

    let refresh_notify = Arc::new(tokio::sync::Notify::new());

    // Ensure the screen_to_client relay doesn't miss notifications that fired
    // between send_initial_state draining pending data and the first notified()
    // poll.  A spurious wakeup is harmless — it just triggers a no-op render.
    setup.handles.screen_notify.notify_one();

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
            r??;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use retach::screen::{Screen, RenderCache, TerminalEmulator};

    #[test]
    fn prepend_passthrough_empty() {
        let render = b"render-data".to_vec();
        let result = prepend_passthrough(vec![], render.clone());
        assert_eq!(result, render);
    }

    #[test]
    fn prepend_passthrough_single() {
        let pt = vec![b"\x1b[3J".to_vec()];
        let render = b"\x1b[?2026hcontent\x1b[?2026l".to_vec();
        let result = prepend_passthrough(pt, render);
        assert_eq!(&result[..4], b"\x1b[3J");
        assert_eq!(&result[4..], b"\x1b[?2026hcontent\x1b[?2026l");
    }

    #[test]
    fn prepend_passthrough_multiple() {
        let pt = vec![vec![0x07], b"\x1b[3J".to_vec()];
        let render = b"screen".to_vec();
        let result = prepend_passthrough(pt, render);
        assert_eq!(result, b"\x07\x1b[3Jscreen");
    }

    /// ED mode 3 passthrough is prepended to the render buffer,
    /// ensuring the terminal processes clear + redraw atomically.
    #[test]
    fn ed3_included_in_screen_update() {
        let mut screen = Screen::new(80, 24, 100);
        screen.process(b"hello world");
        screen.process(b"\x1b[3J");

        let passthrough = screen.take_passthrough();
        assert_eq!(passthrough.len(), 1);
        assert_eq!(passthrough[0], b"\x1b[3J");

        let mut cache = RenderCache::new();
        let render_data = screen.render(true, &mut cache);

        let combined = prepend_passthrough(passthrough, render_data.clone());
        assert!(combined.starts_with(b"\x1b[3J"), "passthrough should prefix screen data");
        assert_eq!(&combined[4..], &render_data[..]);
    }
}
