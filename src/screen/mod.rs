//! VTE-based terminal screen emulator with scrollback history.
//! Processes escape sequences and maintains a grid of styled cells.

pub(crate) mod style;
pub(crate) mod cell;
pub(crate) mod grid;
pub(crate) mod performer;
pub(crate) mod grid_mutator;
pub(crate) mod render;
pub mod traits;

use std::collections::VecDeque;
use vte::Parser;

use grid::Grid;
use performer::ScreenPerformer;
use render::render_screen;
pub use cell::{Cell, Row};
pub use style::{Style, StyleId, Color, UnderlineStyle};
pub use grid::{CursorShape, TerminalSize, sanitize_dimensions};
pub use grid::{TerminalModes, MouseModes, MouseEncoding, Charset, ActiveCharset};
pub use style::write_u16;
pub use render::RenderCache;
pub use render::AnsiRenderer;
pub use traits::{TerminalEmulator, TerminalRenderer};

/// Full cursor state saved by DECSC (ESC 7) / CSI s / mode 1048.
#[derive(Copy, Clone)]
pub(super) struct SavedCursor {
    pub(super) x: u16,
    pub(super) y: u16,
    pub(super) style: Style,
    pub(super) g0_charset: grid::Charset,
    pub(super) g1_charset: grid::Charset,
    pub(super) active_charset: grid::ActiveCharset,
    pub(super) autowrap_mode: bool,
    pub(super) origin_mode: bool,
    /// VT220 "last column flag": deferred autowrap is pending.
    pub(super) wrap_pending: bool,
}

/// Maximum responses/passthrough entries buffered per process() call.
/// 1024 is a safety cap — normal output produces 0-2 responses (DA, DSR).
/// Pathological PTY output (e.g. 1000 DSR queries in one write) is truncated.
const MAX_PENDING: usize = 1024;

/// Notifications (OSC 9/777) queued for replay on reconnect. 50 prevents
/// a disconnected session from accumulating megabytes of stale notifications
/// while still preserving recent ones for the reconnecting client.
const MAX_QUEUED_NOTIFICATIONS: usize = 50;

/// Non-grid state that the performer needs mutable access to.
/// Grouped to reduce borrow count in ScreenPerformer.
pub(super) struct ScreenState {
    pub(super) current_style: Style,
    pub(super) in_alt_screen: bool,
    pub(super) saved_grid: Option<grid::SavedGrid>,
    pub(super) saved_cursor_state: Option<SavedCursor>,
    pub(super) saved_modes: Option<grid::TerminalModes>,
    /// Scroll region saved when entering alt screen; restored on exit.
    pub(super) saved_scroll_region: Option<(u16, u16)>,
    pub(super) pending_responses: Vec<Vec<u8>>,
    pub(super) pending_passthrough: Vec<Vec<u8>>,
    pub(super) queued_notifications: VecDeque<Vec<u8>>,
    pub(super) title: String,
    pub(super) title_stack: Vec<String>,
    pub(super) last_printed_char: char,
}

impl ScreenState {
    /// Push a PTY response (DA, DSR) with bounded growth.
    pub fn push_response(&mut self, data: Vec<u8>) {
        if self.pending_responses.len() < MAX_PENDING {
            self.pending_responses.push(data);
        } else {
            tracing::debug!("pending_responses full, dropping response");
        }
    }

    /// Push a passthrough sequence (bell, OSC, etc.) with bounded growth.
    pub fn push_passthrough(&mut self, data: Vec<u8>) {
        if self.pending_passthrough.len() < MAX_PENDING {
            self.pending_passthrough.push(data);
        }
    }

    /// Queue a text notification (OSC 9/777/99) for delivery or replay.
    /// Always enqueues; the consumer (relay or reconnect handler) drains.
    /// Oldest notifications are dropped when the queue is full.
    pub fn push_notification(&mut self, data: Vec<u8>) {
        if self.queued_notifications.len() >= MAX_QUEUED_NOTIFICATIONS {
            self.queued_notifications.pop_front();
        }
        self.queued_notifications.push_back(data);
    }
}

impl Default for ScreenState {
    fn default() -> Self {
        Self {
            current_style: Style::default(),
            in_alt_screen: false,
            saved_grid: None,
            saved_cursor_state: None,
            saved_modes: None,
            saved_scroll_region: None,
            pending_responses: Vec::new(),
            pending_passthrough: Vec::new(),
            queued_notifications: VecDeque::new(),
            title: String::new(),
            title_stack: Vec::new(),
            last_printed_char: ' ',
        }
    }
}

/// Terminal screen emulator that processes VTE escape sequences into a cell grid.
pub struct Screen {
    pub(super) grid: Grid,
    pub(super) state: ScreenState,
    parser: Parser,
}

impl Screen {
    /// Create a screen with the given dimensions and scrollback line limit.
    pub fn new(cols: u16, rows: u16, scrollback_limit: usize) -> Self {
        Self {
            grid: Grid::new(cols, rows, scrollback_limit),
            state: ScreenState::default(),
            parser: Parser::new(),
        }
    }

    /// Borrow the underlying grid (read-only).
    #[cfg(test)]
    pub(crate) fn grid(&self) -> &Grid {
        &self.grid
    }

    /// Current window title (OSC 0/2).
    #[cfg(test)]
    pub(crate) fn title(&self) -> &str {
        &self.state.title
    }

    /// Current SGR style.
    #[cfg(test)]
    pub(crate) fn current_style(&self) -> style::Style {
        self.state.current_style
    }

    /// Number of visible rows in the grid.
    pub fn rows(&self) -> u16 {
        self.grid.rows()
    }

    /// Whether the screen is currently in alternate screen mode.
    pub fn in_alt_screen(&self) -> bool {
        self.state.in_alt_screen
    }

    /// Feed raw bytes through the VTE parser, updating the grid and state.
    pub fn process(&mut self, bytes: &[u8]) {
        let mut performer = ScreenPerformer {
            grid: &mut self.grid,
            state: &mut self.state,
        };
        for &byte in bytes {
            self.parser.advance(&mut performer, byte);
        }
    }

    /// Take pending responses that need to be written back to PTY stdin
    pub fn take_responses(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.state.pending_responses)
    }

    /// Drain and return scrollback lines added since the last call, rendered as ANSI bytes.
    pub fn take_pending_scrollback(&mut self) -> Vec<Vec<u8>> {
        let start = self.grid.pending_start();
        let count = self.grid.pending_scrollback_count();
        self.grid.set_pending_start(self.grid.scrollback_len());
        self.grid.scrollback_rows().skip(start).take(count)
            .map(|row| render::render_line(row, self.grid.style_table()))
            .collect()
    }

    /// Return all accumulated scrollback lines as rendered ANSI bytes.
    pub fn get_history(&self) -> Vec<Vec<u8>> {
        self.grid.scrollback_rows()
            .map(|row| render::render_line(row, self.grid.style_table()))
            .collect()
    }

    /// Render the current grid as ANSI output. Pass `full: true` for a full redraw.
    pub fn render(&self, full: bool, cache: &mut RenderCache) -> Vec<u8> {
        render_screen(&self.grid, &self.state.title, full, cache)
    }

    /// Render the screen with scrollback lines included in one atomic output.
    ///
    /// Scrollback lines are injected into the real terminal's native scrollback
    /// buffer (cursor positioned at the bottom so `\r\n` scrolls), followed by
    /// a full screen redraw.  Everything is inside a single synchronized-output
    /// block to prevent flicker.
    pub fn render_with_scrollback(&self, scrollback: &[Vec<u8>], cache: &mut RenderCache) -> Vec<u8> {
        render::render_screen_with_scrollback(&self.grid, &self.state.title, scrollback, cache)
    }

    /// Take pending scrollback, passthrough, notifications, and render in a
    /// single lock hold.  Returns `(render_data, passthrough)`.
    /// Notifications are consumed here so they are delivered exactly once.
    pub fn take_and_render(&mut self, cache: &mut RenderCache) -> (Vec<u8>, Vec<Vec<u8>>) {
        let scrollback_lines = self.take_pending_scrollback();
        let mut passthrough = self.take_passthrough();
        // Drain notifications into passthrough — they are OSC sequences that
        // the terminal should process, delivered exactly once via the relay.
        passthrough.extend(self.state.queued_notifications.drain(..));
        let render_data = if !scrollback_lines.is_empty() {
            self.render_with_scrollback(&scrollback_lines, cache)
        } else {
            self.render(false, cache)
        };
        (render_data, passthrough)
    }

    /// Look up the resolved style for a visible cell. Test convenience.
    #[cfg(test)]
    pub(crate) fn cell_style(&self, row: usize, col: usize) -> style::Style {
        self.grid.style_table().get(self.grid.visible_row(row)[col].style_id)
    }

    /// Character in a visible cell. Test convenience.
    #[cfg(test)]
    pub(crate) fn cell_char(&self, row: usize, col: usize) -> char {
        self.grid.visible_row(row)[col].c
    }

    /// Display width of a visible cell. Test convenience.
    #[cfg(test)]
    pub(crate) fn cell_width(&self, row: usize, col: usize) -> u8 {
        self.grid.visible_row(row)[col].width
    }

    /// Compact the style table by scanning all cells for live style IDs
    /// and reclaiming unused slots.
    #[cfg(test)]
    pub fn compact_styles(&mut self) {
        compact_styles(&mut self.grid, self.state.saved_grid.as_ref());
    }

    /// Resize the grid to new dimensions, restoring scrollback lines on vertical expand.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let old_rows = self.grid.rows();

        // Restore scrollback lines when growing vertically (not in alt screen).
        // With unified buffer, scrollback rows are already in cells — just move the boundary.
        if !self.state.in_alt_screen && rows > old_rows {
            let grow = (rows - old_rows) as usize;
            let restore_count = grow.min(self.grid.scrollback_len());
            self.grid.restore_scrollback(restore_count);
            self.grid.set_cursor_y_unclamped(self.grid.cursor_y().saturating_add(
                u16::try_from(restore_count).unwrap_or(u16::MAX)
            ));
        }

        self.grid.resize(cols, rows);
    }
}

impl traits::TerminalEmulator for Screen {
    fn process(&mut self, bytes: &[u8]) {
        self.process(bytes);
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.resize(cols, rows);
    }

    fn cols(&self) -> u16 {
        self.grid.cols()
    }

    fn rows(&self) -> u16 {
        self.grid.rows()
    }

    fn visible_rows(&self) -> Box<dyn Iterator<Item = &cell::Row> + '_> {
        Box::new(self.grid.visible_rows())
    }

    fn scrollback_rows(&self) -> Box<dyn Iterator<Item = &cell::Row> + '_> {
        Box::new(self.grid.scrollback_rows())
    }

    fn scrollback_len(&self) -> usize {
        self.grid.scrollback_len()
    }

    fn cursor_position(&self) -> (u16, u16) {
        self.grid.cursor_pos()
    }

    fn cursor_visible(&self) -> bool {
        self.grid.cursor_visible()
    }

    fn resolve_style(&self, id: style::StyleId) -> style::Style {
        self.grid.style_table().get(id)
    }

    fn in_alt_screen(&self) -> bool {
        self.state.in_alt_screen
    }

    fn take_responses(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.state.pending_responses)
    }

    fn title(&self) -> &str {
        &self.state.title
    }

    fn cursor_shape(&self) -> grid::CursorShape {
        self.grid.modes().cursor_shape
    }

    fn scroll_region(&self) -> (u16, u16) {
        self.grid.scroll_region()
    }

    fn modes(&self) -> &grid::TerminalModes {
        self.grid.modes()
    }

    fn take_passthrough(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.state.pending_passthrough)
    }

    fn take_queued_notifications(&mut self) -> Vec<Vec<u8>> {
        self.state.queued_notifications.drain(..).collect()
    }
}

/// Scan all cells in the grid (scrollback + visible) and saved_grid,
/// then reclaim style table slots not referenced by any cell.
pub(crate) fn compact_styles(grid: &mut Grid, saved_grid: Option<&grid::SavedGrid>) {
    let cap = grid.style_table().capacity();
    if cap <= 1 { return; }

    let mut live = vec![false; cap];
    live[0] = true; // default style is always live

    for row in grid.scrollback_rows().chain(grid.visible_rows()) {
        for cell in row.iter() {
            let id = cell.style_id.index();
            if id < cap { live[id] = true; }
        }
    }

    if let Some(saved) = saved_grid {
        for row in saved.visible_rows() {
            for cell in row.iter() {
                let id = cell.style_id.index();
                if id < cap { live[id] = true; }
            }
        }
    }

    grid.style_table_mut().reclaim(&live);
}

#[cfg(test)]
mod tests_traits {
    use super::*;
    use super::traits::TerminalEmulator;

    #[test]
    fn screen_implements_terminal_emulator() {
        let mut screen = Screen::new(80, 24, 100);

        // Test process + visible_rows
        TerminalEmulator::process(&mut screen, b"Hello");
        let rows: Vec<&cell::Row> = TerminalEmulator::visible_rows(&screen).collect();
        assert_eq!(rows.len(), 24);
        assert_eq!(rows[0][0].c, 'H');
        assert_eq!(rows[0][4].c, 'o');

        // Test dimensions
        assert_eq!(TerminalEmulator::cols(&screen), 80);
        assert_eq!(TerminalEmulator::rows(&screen), 24);

        // Test cursor
        assert_eq!(TerminalEmulator::cursor_position(&screen), (5, 0));
        assert!(TerminalEmulator::cursor_visible(&screen));

        // Test resolve_style
        let style = TerminalEmulator::resolve_style(&screen, rows[0][0].style_id);
        assert!(style.is_default());

        // Test alt screen
        assert!(!TerminalEmulator::in_alt_screen(&screen));

        // Test title
        assert_eq!(TerminalEmulator::title(&screen), "");

        // Test scrollback
        assert_eq!(TerminalEmulator::scrollback_len(&screen), 0);
        assert_eq!(TerminalEmulator::scrollback_rows(&screen).count(), 0);

        // Test take_responses
        assert!(TerminalEmulator::take_responses(&mut screen).is_empty());
    }

    #[test]
    fn screen_as_dyn_terminal_emulator() {
        let mut screen = Screen::new(40, 10, 50);
        let emu: &mut dyn TerminalEmulator = &mut screen;
        emu.process(b"test");
        assert_eq!(emu.cols(), 40);
        assert_eq!(emu.rows(), 10);
        let rows: Vec<_> = emu.visible_rows().collect();
        assert_eq!(rows[0][0].c, 't');
    }

    #[test]
    fn ansi_renderer_implements_terminal_renderer() {
        use super::render::AnsiRenderer;
        use super::traits::TerminalRenderer;

        let mut screen = Screen::new(10, 3, 0);
        screen.process(b"Hi");

        let mut renderer = AnsiRenderer::new();
        let output = renderer.render(&screen, true);
        let text = String::from_utf8_lossy(&output);
        assert!(text.contains("Hi"), "render output should contain 'Hi', got: {text}");
    }

    #[test]
    fn ansi_renderer_clears_title_when_empty() {
        // Bug 3: AnsiRenderer should emit a title-clearing OSC when the
        // title was previously set and is now empty.
        use super::render::AnsiRenderer;
        use super::traits::TerminalRenderer;

        let mut screen = Screen::new(10, 3, 0);
        // Set a title
        screen.process(b"\x1b]2;Hello\x07");
        assert_eq!(screen.title(), "Hello");

        let mut renderer = AnsiRenderer::new();
        // First render — should contain the title
        let output = renderer.render(&screen, true);
        let text = String::from_utf8_lossy(&output);
        assert!(text.contains("\x1b]2;Hello\x07"),
            "first render should contain title OSC");

        // Clear the title
        screen.process(b"\x1b]2;\x07");
        assert_eq!(screen.title(), "");

        // Second render — should emit an empty-title OSC to clear it
        let output = renderer.render(&screen, true);
        let text = String::from_utf8_lossy(&output);
        assert!(text.contains("\x1b]2;\x07"),
            "render should emit title-clearing OSC when title becomes empty, \
             got: {text}");
    }
}

#[cfg(test)]
pub(super) mod test_helpers {
    use super::*;

    /// Strip ANSI escape sequences, returning only printable text.
    pub fn strip_ansi(bytes: &[u8]) -> String {
        let s = String::from_utf8_lossy(bytes);
        let mut out = String::new();
        let mut in_esc = false;
        for ch in s.chars() {
            if in_esc {
                if ch.is_ascii_alphabetic() {
                    in_esc = false;
                }
                continue;
            }
            if ch == '\x1b' {
                in_esc = true;
                continue;
            }
            if ch >= ' ' {
                out.push(ch);
            }
        }
        out.trim_end().to_string()
    }

    /// Collect visible grid rows as trimmed strings.
    pub fn screen_lines(screen: &Screen) -> Vec<String> {
        screen.grid.visible_rows().map(|row| {
            let s: String = row.iter().map(|c| c.c).collect();
            s.trim_end().to_string()
        }).collect()
    }

    /// Collect scrollback history as trimmed text strings (ANSI stripped).
    pub fn history_texts(screen: &Screen) -> Vec<String> {
        screen.get_history().iter().map(|b| strip_ansi(b)).collect()
    }
}

#[cfg(test)]
mod history_boundary_tests;
#[cfg(test)]
mod tests_large_updates;
#[cfg(test)]
mod tests_screen;
#[cfg(test)]
mod tests_reattach;
#[cfg(test)]
mod tests_resize;
#[cfg(test)]
mod tests_reconnect_scrollback;
#[cfg(test)]
mod tests_live_scrollback;
#[cfg(test)]
mod tests_progress_bar_scrollback;
