//! Trait abstractions for terminal emulation and rendering.

use super::cell::Row;
use super::grid::{CursorShape, TerminalModes};
use super::style::{Style, StyleId};

/// A terminal emulator that processes byte streams and maintains a cell grid.
///
/// This is the primary abstraction for headless terminal emulation.
/// Feed bytes via [`process`](Self::process), then read the grid state
/// via row iterators, cursor position, and style resolution.
pub trait TerminalEmulator {
    /// Feed raw bytes (from SSH, PTY, etc.) through the VTE parser.
    fn process(&mut self, bytes: &[u8]);

    /// Resize the terminal grid to new dimensions.
    fn resize(&mut self, cols: u16, rows: u16);

    /// Number of columns in the terminal grid.
    fn cols(&self) -> u16;

    /// Number of visible rows in the terminal grid.
    fn rows(&self) -> u16;

    /// Iterate over visible rows (the current screen content).
    fn visible_rows(&self) -> Box<dyn Iterator<Item = &Row> + '_>;

    /// Iterate over scrollback rows (history above the visible screen).
    fn scrollback_rows(&self) -> Box<dyn Iterator<Item = &Row> + '_>;

    /// Number of scrollback rows currently stored.
    fn scrollback_len(&self) -> usize;

    /// Current cursor position as `(x, y)`, both 0-based.
    fn cursor_position(&self) -> (u16, u16);

    /// Whether the cursor is currently visible (DECTCEM).
    fn cursor_visible(&self) -> bool;

    /// Resolve a cell's interned style ID to a full [`Style`].
    fn resolve_style(&self, id: StyleId) -> Style;

    /// Whether the terminal is in alternate screen mode (e.g. vim, htop).
    fn in_alt_screen(&self) -> bool;

    /// Take pending responses that should be written back to the PTY/SSH stdin
    /// (e.g. DA, DSR query replies).
    fn take_responses(&mut self) -> Vec<Vec<u8>>;

    /// Current window title (set by OSC 0/2).
    fn title(&self) -> &str;

    /// DECSCUSR cursor shape.
    fn cursor_shape(&self) -> CursorShape;

    /// Current scroll region as `(top, bottom)`, both 0-based.
    fn scroll_region(&self) -> (u16, u16);

    /// Terminal mode flags (autowrap, mouse, charset, etc.).
    fn modes(&self) -> &TerminalModes;

    /// Take pending OSC passthrough sequences to forward to the outer terminal.
    fn take_passthrough(&mut self) -> Vec<Vec<u8>>;

    /// Drain queued desktop notifications (OSC 9/777/99).
    fn take_queued_notifications(&mut self) -> Vec<Vec<u8>>;
}

/// A rendering strategy that produces output from terminal emulator state.
///
/// The associated `Output` type allows different renderers to produce
/// different formats: `Vec<u8>` for ANSI sequences, `()` for direct
/// widget painting, or a custom draw command list.
pub trait TerminalRenderer {
    /// The output type produced by rendering.
    type Output;

    /// Render the current emulator state.
    ///
    /// When `full` is true, perform a complete redraw ignoring any cached state.
    /// When false, perform an incremental update based on what changed.
    fn render(&mut self, emulator: &dyn TerminalEmulator, full: bool) -> Self::Output;
}
