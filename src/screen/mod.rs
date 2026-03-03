//! VTE-based terminal screen emulator with scrollback history.
//! Processes escape sequences and maintains a grid of styled cells.

pub mod style;
pub mod cell;
pub mod grid;
pub mod performer;
pub mod render;

use std::collections::VecDeque;
use vte::Parser;

use cell::Cell;
use grid::Grid;
use performer::ScreenPerformer;
use render::render_screen;
pub use render::RenderCache;
use style::Style;

/// Full cursor state saved by DECSC (ESC 7) / CSI s / mode 1048.
#[derive(Copy, Clone)]
pub struct SavedCursor {
    pub x: u16,
    pub y: u16,
    pub style: Style,
    pub g0_charset: grid::Charset,
    pub g1_charset: grid::Charset,
    pub active_charset: grid::ActiveCharset,
    pub autowrap_mode: bool,
}

/// Non-grid state that the performer needs mutable access to.
/// Grouped to reduce borrow count in ScreenPerformer.
pub struct ScreenState {
    pub current_style: Style,
    pub in_alt_screen: bool,
    pub saved_grid: Option<std::collections::VecDeque<Vec<Cell>>>,
    pub saved_cursor_state: Option<SavedCursor>,
    pub saved_modes: Option<grid::TerminalModes>,
    pub pending_responses: Vec<Vec<u8>>,
    pub pending_passthrough: Vec<Vec<u8>>,
    pub title: String,
    pub last_printed_char: char,
}

impl Default for ScreenState {
    fn default() -> Self {
        Self {
            current_style: Style::default(),
            in_alt_screen: false,
            saved_grid: None,
            saved_cursor_state: None,
            saved_modes: None,
            pending_responses: Vec::new(),
            pending_passthrough: Vec::new(),
            title: String::new(),
            last_printed_char: ' ',
        }
    }
}

/// Terminal screen emulator that processes VTE escape sequences into a cell grid.
pub struct Screen {
    pub grid: Grid,
    state: ScreenState,
    scrollback: VecDeque<Vec<Cell>>,
    scrollback_limit: usize,
    pending_scrollback: VecDeque<Vec<Cell>>,
    parser: Parser,
}

impl Screen {
    /// Create a screen with the given dimensions and scrollback line limit.
    pub fn new(cols: u16, rows: u16, scrollback_limit: usize) -> Self {
        Self {
            grid: Grid::new(cols, rows),
            state: ScreenState::default(),
            scrollback: VecDeque::new(),
            scrollback_limit,
            pending_scrollback: VecDeque::new(),
            parser: Parser::new(),
        }
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
            scrollback: &mut self.scrollback,
            scrollback_limit: self.scrollback_limit,
            pending_scrollback: &mut self.pending_scrollback,
        };
        for &byte in bytes {
            self.parser.advance(&mut performer, byte);
        }
    }

    /// Take pending responses that need to be written back to PTY stdin
    pub fn take_responses(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.state.pending_responses)
    }

    /// Take pending OSC passthrough sequences to forward to the outer terminal.
    pub fn take_passthrough(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.state.pending_passthrough)
    }

    /// Drain and return scrollback lines added since the last call, rendered as ANSI bytes.
    pub fn take_pending_scrollback(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.pending_scrollback)
            .into_iter()
            .map(|row| render::render_line(&row))
            .collect()
    }

    /// Return all accumulated scrollback lines as rendered ANSI bytes.
    pub fn get_history(&self) -> Vec<Vec<u8>> {
        self.scrollback.iter().map(|row| render::render_line(row)).collect()
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

    /// Resize the grid to new dimensions, restoring scrollback lines on vertical expand.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let old_rows = self.grid.rows;

        // Restore scrollback lines when growing vertically (not in alt screen)
        if !self.state.in_alt_screen && rows > old_rows {
            let grow = (rows - old_rows) as usize;
            let restore_count = grow.min(self.scrollback.len());
            // Pop from the end of scrollback (most recent lines) and insert at top of grid
            for _ in 0..restore_count {
                if let Some(row) = self.scrollback.pop_back() {
                    self.grid.cells.push_front(row);
                }
            }
            self.grid.cursor_y += restore_count as u16;
        }

        self.grid.resize(cols, rows);
    }
}

#[cfg(test)]
mod history_boundary_tests;
#[cfg(test)]
mod tests_screen;
#[cfg(test)]
mod tests_reattach;
#[cfg(test)]
mod tests_resize;
