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
use style::Style;

/// Full cursor state saved by DECSC (ESC 7) / CSI s / mode 1048.
#[derive(Clone)]
pub struct SavedCursor {
    pub x: u16,
    pub y: u16,
    pub style: Style,
    pub g0_charset: u8,
    pub g1_charset: u8,
    pub active_charset: u8,
    pub autowrap_mode: bool,
}

/// Non-grid state that the performer needs mutable access to.
/// Grouped to reduce borrow count in ScreenPerformer.
pub struct ScreenState {
    pub current_style: Style,
    pub in_alt_screen: bool,
    pub saved_grid: Option<Vec<Vec<Cell>>>,
    pub saved_cursor: Option<(u16, u16)>,
    pub saved_cursor_state: Option<SavedCursor>,
    pub saved_modes: Option<grid::TerminalModes>,
    pub pending_responses: Vec<Vec<u8>>,
    pub title: String,
    pub last_printed_char: char,
}

impl Default for ScreenState {
    fn default() -> Self {
        Self {
            current_style: Style::default(),
            in_alt_screen: false,
            saved_grid: None,
            saved_cursor: None,
            saved_cursor_state: None,
            saved_modes: None,
            pending_responses: Vec::new(),
            title: String::new(),
            last_printed_char: ' ',
        }
    }
}

/// Terminal screen emulator that processes VTE escape sequences into a cell grid.
pub struct Screen {
    pub grid: Grid,
    state: ScreenState,
    scrollback: VecDeque<Vec<u8>>,
    scrollback_limit: usize,
    pending_scrollback: Vec<Vec<u8>>,
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
            pending_scrollback: Vec::new(),
            parser: Parser::new(),
        }
    }

    /// Feed raw bytes through the VTE parser, updating the grid and state.
    pub fn process(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            let mut performer = ScreenPerformer {
                grid: &mut self.grid,
                state: &mut self.state,
                scrollback: &mut self.scrollback,
                scrollback_limit: self.scrollback_limit,
                pending_scrollback: &mut self.pending_scrollback,
            };
            self.parser.advance(&mut performer, byte);
        }
    }

    /// Take pending responses that need to be written back to PTY stdin
    pub fn take_responses(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.state.pending_responses)
    }

    /// Drain and return scrollback lines added since the last call.
    pub fn take_pending_scrollback(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.pending_scrollback)
    }

    /// Return all accumulated scrollback lines as rendered ANSI bytes.
    pub fn get_history(&self) -> Vec<Vec<u8>> {
        self.scrollback.iter().cloned().collect()
    }

    /// Render the current grid as ANSI output. Pass `full: true` for a full redraw.
    pub fn render(&self, full: bool) -> Vec<u8> {
        render_screen(&self.grid, &self.state.title, full)
    }

    /// Resize the grid to new dimensions, clamping cursor and resetting scroll region.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.grid.resize(cols, rows);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_screen_save_restore() {
        let mut screen = Screen::new(10, 3, 100);

        // Write "Hello" on the main screen
        screen.process(b"Hello");
        assert_eq!(screen.grid.cells[0][0].c, 'H');
        assert_eq!(screen.grid.cells[0][4].c, 'o');

        // Enter alt screen (CSI ?1049h)
        screen.process(b"\x1b[?1049h");
        assert!(screen.state.in_alt_screen);
        // Alt screen should be cleared
        assert_eq!(screen.grid.cells[0][0].c, ' ');

        // Write something on alt screen
        screen.process(b"Alt");
        assert_eq!(screen.grid.cells[0][0].c, 'A');

        // Leave alt screen (CSI ?1049l) — should restore main buffer (fix S7)
        screen.process(b"\x1b[?1049l");
        assert!(!screen.state.in_alt_screen);
        assert_eq!(screen.grid.cells[0][0].c, 'H');
        assert_eq!(screen.grid.cells[0][4].c, 'o');
    }

    #[test]
    fn scrollback_on_scroll() {
        let mut screen = Screen::new(10, 3, 100);
        // Fill 3 rows and scroll
        screen.process(b"Line1\r\nLine2\r\nLine3\r\nLine4");
        let scrollback = screen.take_pending_scrollback();
        assert!(!scrollback.is_empty());
        // First scrolled line should contain "Line1"
        let first = String::from_utf8_lossy(&scrollback[0]);
        assert!(first.contains("Line1"), "scrollback should contain Line1, got: {}", first);
    }

    #[test]
    fn no_scrollback_in_alt_screen() {
        let mut screen = Screen::new(10, 3, 100);
        screen.process(b"\x1b[?1049h"); // enter alt screen
        screen.process(b"A\r\nB\r\nC\r\nD"); // scroll in alt
        let scrollback = screen.take_pending_scrollback();
        assert!(scrollback.is_empty(), "alt screen should not generate scrollback");
    }

    #[test]
    fn history_preserved_across_sessions() {
        let mut screen = Screen::new(10, 3, 100);
        screen.process(b"A\r\nB\r\nC\r\nD\r\nE");
        let _ = screen.take_pending_scrollback();
        let history = screen.get_history();
        assert!(!history.is_empty());
    }

    #[test]
    fn deferred_wrap_cr_stays_on_same_line() {
        // Simulates zsh PROMPT_SP: fill line to end, CR, overwrite
        let mut screen = Screen::new(5, 3, 100);
        // Write exactly 5 chars to fill the line
        screen.process(b"%    ");
        // wrap_pending should be set, cursor stays on row 0
        assert!(screen.grid.wrap_pending);
        assert_eq!(screen.grid.cursor_y, 0);
        // CR should clear wrap_pending and go to column 0 of SAME row
        screen.process(b"\r");
        assert!(!screen.grid.wrap_pending);
        assert_eq!(screen.grid.cursor_x, 0);
        assert_eq!(screen.grid.cursor_y, 0);
        // Space overwrites the '%'
        screen.process(b" ");
        assert_eq!(screen.grid.cells[0][0].c, ' ');
    }

    #[test]
    fn deferred_wrap_next_print_wraps() {
        let mut screen = Screen::new(5, 3, 100);
        // Fill line
        screen.process(b"ABCDE");
        assert!(screen.grid.wrap_pending);
        assert_eq!(screen.grid.cursor_y, 0);
        // Next char triggers actual wrap
        screen.process(b"F");
        assert_eq!(screen.grid.cursor_y, 1);
        assert_eq!(screen.grid.cells[1][0].c, 'F');
    }

    // --- New tests for escape sequence completeness ---

    #[test]
    fn dsr_cursor_position_report() {
        let mut screen = Screen::new(80, 24, 100);
        screen.process(b"\x1b[5;10H"); // move to row 5, col 10
        screen.process(b"\x1b[6n");     // request CPR
        let responses = screen.take_responses();
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0], b"\x1b[5;10R");
    }

    #[test]
    fn da1_primary_device_attributes() {
        let mut screen = Screen::new(80, 24, 100);
        screen.process(b"\x1b[c");
        let responses = screen.take_responses();
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0], b"\x1b[?62;c");
    }

    #[test]
    fn da2_secondary_device_attributes() {
        let mut screen = Screen::new(80, 24, 100);
        screen.process(b"\x1b[>c");
        let responses = screen.take_responses();
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0], b"\x1b[>0;10;1c");
    }

    #[test]
    fn dec_line_drawing_charset() {
        let mut screen = Screen::new(80, 24, 100);
        screen.process(b"\x1b(0");  // switch G0 to line drawing
        screen.process(b"lqk");    // should produce box-drawing chars
        assert_eq!(screen.grid.cells[0][0].c, '\u{250C}'); // ┌
        assert_eq!(screen.grid.cells[0][1].c, '\u{2500}'); // ─
        assert_eq!(screen.grid.cells[0][2].c, '\u{2510}'); // ┐
        // Switch back to ASCII
        screen.process(b"\x1b(B");
        screen.process(b"l");
        assert_eq!(screen.grid.cells[0][3].c, 'l'); // plain ASCII 'l'
    }

    #[test]
    fn rep_repeats_last_char() {
        let mut screen = Screen::new(80, 24, 100);
        screen.process(b"A\x1b[3b"); // print A, then repeat 3 times
        assert_eq!(screen.grid.cells[0][0].c, 'A');
        assert_eq!(screen.grid.cells[0][1].c, 'A');
        assert_eq!(screen.grid.cells[0][2].c, 'A');
        assert_eq!(screen.grid.cells[0][3].c, 'A');
    }

    #[test]
    fn wide_character_occupies_two_cells() {
        let mut screen = Screen::new(80, 24, 100);
        screen.process("你".as_bytes());
        assert_eq!(screen.grid.cells[0][0].c, '你');
        assert_eq!(screen.grid.cells[0][0].width, 2);
        assert_eq!(screen.grid.cells[0][1].width, 0);
        assert_eq!(screen.grid.cursor_x, 2);
    }

    #[test]
    fn wide_char_wraps_at_end_of_line() {
        let mut screen = Screen::new(5, 3, 100);
        screen.process(b"ABCD"); // fill 4 of 5 cols
        screen.process("你".as_bytes()); // needs 2 cols, only 1 left -> should wrap
        // Col 4 should be blanked, wide char on next line
        assert_eq!(screen.grid.cells[0][4].c, ' ');
        assert_eq!(screen.grid.cells[1][0].c, '你');
        assert_eq!(screen.grid.cells[1][0].width, 2);
        assert_eq!(screen.grid.cells[1][1].width, 0);
    }

    #[test]
    fn esc_c_full_reset() {
        let mut screen = Screen::new(80, 24, 100);
        screen.process(b"\x1b[?2004h");  // enable bracketed paste
        screen.process(b"\x1b[5;10H");   // move cursor
        screen.process(b"Hello");
        screen.process(b"\x1b[2 q");     // set cursor shape
        screen.process(b"\x1bc");         // full reset
        assert_eq!(screen.grid.cursor_x, 0);
        assert_eq!(screen.grid.cursor_y, 0);
        assert!(!screen.grid.modes.bracketed_paste);
        assert_eq!(screen.grid.modes.cursor_shape, 0);
        assert!(screen.grid.cursor_visible);
        assert_eq!(screen.grid.cells[0][0].c, ' ');
        assert!(screen.state.title.is_empty());
    }

    #[test]
    fn osc_sets_title() {
        let mut screen = Screen::new(80, 24, 100);
        screen.process(b"\x1b]0;My Terminal\x07");
        assert_eq!(screen.state.title, "My Terminal");
        screen.process(b"\x1b]2;New Title\x07");
        assert_eq!(screen.state.title, "New Title");
    }

    #[test]
    fn mode_flags_bracketed_paste() {
        let mut screen = Screen::new(80, 24, 100);
        assert!(!screen.grid.modes.bracketed_paste);
        screen.process(b"\x1b[?2004h");
        assert!(screen.grid.modes.bracketed_paste);
        screen.process(b"\x1b[?2004l");
        assert!(!screen.grid.modes.bracketed_paste);
    }

    #[test]
    fn mode_flags_cursor_key_mode() {
        let mut screen = Screen::new(80, 24, 100);
        assert!(!screen.grid.modes.cursor_key_mode);
        screen.process(b"\x1b[?1h");
        assert!(screen.grid.modes.cursor_key_mode);
        screen.process(b"\x1b[?1l");
        assert!(!screen.grid.modes.cursor_key_mode);
    }

    #[test]
    fn mode_flags_mouse() {
        let mut screen = Screen::new(80, 24, 100);
        screen.process(b"\x1b[?1000h");
        assert_eq!(screen.grid.modes.mouse_mode, 1000);
        screen.process(b"\x1b[?1006h");
        assert_eq!(screen.grid.modes.mouse_encoding, 1006);
        screen.process(b"\x1b[?1000l");
        assert_eq!(screen.grid.modes.mouse_mode, 0);
    }

    #[test]
    fn keypad_app_mode() {
        let mut screen = Screen::new(80, 24, 100);
        assert!(!screen.grid.modes.keypad_app_mode);
        screen.process(b"\x1b=");
        assert!(screen.grid.modes.keypad_app_mode);
        screen.process(b"\x1b>");
        assert!(!screen.grid.modes.keypad_app_mode);
    }

    #[test]
    fn cursor_shape_decscusr() {
        let mut screen = Screen::new(80, 24, 100);
        screen.process(b"\x1b[2 q"); // steady block
        assert_eq!(screen.grid.modes.cursor_shape, 2);
        screen.process(b"\x1b[5 q"); // blinking bar
        assert_eq!(screen.grid.modes.cursor_shape, 5);
        screen.process(b"\x1b[0 q"); // reset to default
        assert_eq!(screen.grid.modes.cursor_shape, 0);
    }

    #[test]
    fn autowrap_mode_disable_prevents_wrap() {
        let mut screen = Screen::new(5, 3, 100);
        screen.process(b"\x1b[?7l"); // disable autowrap
        screen.process(b"ABCDEF");   // write 6 chars in 5 cols
        // Should NOT wrap — last char overwrites column 4
        assert_eq!(screen.grid.cursor_y, 0);
        assert_eq!(screen.grid.cells[0][4].c, 'F');
        assert!(!screen.grid.wrap_pending);
    }

    #[test]
    fn sgr_hidden_attribute() {
        let mut screen = Screen::new(80, 24, 100);
        screen.process(b"\x1b[8m"); // hidden
        screen.process(b"secret");
        assert!(screen.grid.cells[0][0].style.hidden);
        screen.process(b"\x1b[28m"); // reveal
        screen.process(b"visible");
        assert!(!screen.grid.cells[0][6].style.hidden);
    }

    #[test]
    fn cursor_save_restore() {
        let mut screen = Screen::new(80, 24, 100);
        screen.process(b"\x1b[5;10H"); // move to row 5, col 10
        screen.process(b"\x1b7");       // save cursor
        screen.process(b"\x1b[1;1H");   // move home
        assert_eq!(screen.grid.cursor_y, 0);
        screen.process(b"\x1b8");       // restore cursor
        assert_eq!(screen.grid.cursor_y, 4);
        assert_eq!(screen.grid.cursor_x, 9);
    }

    #[test]
    fn so_si_charset_switching() {
        let mut screen = Screen::new(80, 24, 100);
        screen.process(b"\x1b)0");  // set G1 to line drawing
        screen.process(b"\x0E");    // SO — activate G1
        screen.process(b"q");       // should be line drawing ─
        assert_eq!(screen.grid.cells[0][0].c, '─');
        screen.process(b"\x0F");    // SI — activate G0 (ASCII)
        screen.process(b"q");
        assert_eq!(screen.grid.cells[0][1].c, 'q');
    }

    #[test]
    fn cuu_cud_respects_scroll_region() {
        let mut screen = Screen::new(80, 24, 100);
        // Set scroll region to rows 5-15
        screen.process(b"\x1b[5;15r");
        // Cursor is at 0,0 after DECSTBM
        // Move into scroll region
        screen.process(b"\x1b[10;1H"); // row 10 (inside region)
        // Try moving up past scroll top
        screen.process(b"\x1b[20A");   // CUU 20 — should stop at row 5 (scroll_top=4)
        assert_eq!(screen.grid.cursor_y, 4); // 0-based row 4 = display row 5
        // Move back down past scroll bottom
        screen.process(b"\x1b[20B");   // CUD 20 — should stop at row 15 (scroll_bottom=14)
        assert_eq!(screen.grid.cursor_y, 14); // 0-based row 14 = display row 15
    }

    #[test]
    fn vt_ff_treated_as_lf() {
        let mut screen = Screen::new(80, 3, 100);
        screen.process(b"A");
        screen.process(&[0x0B]); // VT
        assert_eq!(screen.grid.cursor_y, 1);
        screen.process(&[0x0C]); // FF
        assert_eq!(screen.grid.cursor_y, 2);
    }

    #[test]
    fn mode_1048_save_restore_cursor() {
        let mut screen = Screen::new(80, 24, 100);
        screen.process(b"\x1b[5;10H");   // move cursor
        screen.process(b"\x1b[?1048h");  // save cursor
        screen.process(b"\x1b[1;1H");    // move home
        screen.process(b"\x1b[?1048l");  // restore cursor
        assert_eq!(screen.grid.cursor_y, 4);
        assert_eq!(screen.grid.cursor_x, 9);
    }
}
