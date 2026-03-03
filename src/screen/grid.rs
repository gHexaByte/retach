use std::collections::VecDeque;

use super::cell::Cell;
use super::render::render_line;

/// DEC character set designator.
#[derive(Copy, Clone, Debug, Default, PartialEq, Hash)]
pub enum Charset {
    #[default]
    Ascii,
    LineDrawing,
}

/// Which character set slot (G0/G1) is active.
#[derive(Copy, Clone, Debug, Default, PartialEq, Hash)]
pub enum ActiveCharset {
    #[default]
    G0,
    G1,
}

/// DECSCUSR cursor shape.
#[derive(Copy, Clone, Debug, Default, PartialEq, Hash)]
pub enum CursorShape {
    #[default]
    Default,
    BlinkBlock,
    SteadyBlock,
    BlinkUnderline,
    SteadyUnderline,
    BlinkBar,
    SteadyBar,
}

impl CursorShape {
    /// Convert from raw DECSCUSR parameter.
    pub fn from_sgr(n: u8) -> Self {
        match n {
            1 => Self::BlinkBlock,
            2 => Self::SteadyBlock,
            3 => Self::BlinkUnderline,
            4 => Self::SteadyUnderline,
            5 => Self::BlinkBar,
            6 => Self::SteadyBar,
            _ => Self::Default,
        }
    }

    /// Raw DECSCUSR parameter value.
    pub fn to_param(self) -> u8 {
        match self {
            Self::Default => 0,
            Self::BlinkBlock => 1,
            Self::SteadyBlock => 2,
            Self::BlinkUnderline => 3,
            Self::SteadyUnderline => 4,
            Self::BlinkBar => 5,
            Self::SteadyBar => 6,
        }
    }
}

/// Terminal mode flags and character set state, separated from cell storage.
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalModes {
    pub cursor_key_mode: bool,    // ?1 DECCKM
    pub bracketed_paste: bool,    // ?2004
    pub autowrap_mode: bool,      // ?7 DECAWM (default true)
    pub focus_reporting: bool,    // ?1004
    pub mouse_mode: u16,          // 0=off, 1000/1002/1003
    pub mouse_encoding: u16,      // 0=X10, 1006=SGR
    pub keypad_app_mode: bool,    // ESC = / ESC >
    pub cursor_shape: CursorShape,
    // DEC character sets
    pub g0_charset: Charset,
    pub g1_charset: Charset,
    pub active_charset: ActiveCharset,
}

impl Default for TerminalModes {
    fn default() -> Self {
        Self {
            cursor_key_mode: false,
            bracketed_paste: false,
            autowrap_mode: true,
            focus_reporting: false,
            mouse_mode: 0,
            mouse_encoding: 0,
            keypad_app_mode: false,
            cursor_shape: CursorShape::Default,
            g0_charset: Charset::Ascii,
            g1_charset: Charset::Ascii,
            active_charset: ActiveCharset::G0,
        }
    }
}

/// Two-dimensional cell storage with cursor position, scroll region, and terminal modes.
pub struct Grid {
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<Vec<Cell>>,
    pub cursor_x: u16,
    pub cursor_y: u16,
    /// Deferred wrap: cursor is at the right margin, next printable char triggers wrap
    pub wrap_pending: bool,
    /// Scroll region top (inclusive, 0-based)
    pub scroll_top: u16,
    /// Scroll region bottom (inclusive, 0-based)
    pub scroll_bottom: u16,
    /// Cursor visibility (DECTCEM ?25h/?25l)
    pub cursor_visible: bool,
    /// Terminal modes and character set state
    pub modes: TerminalModes,
}

impl Grid {
    /// Create a grid with the given dimensions, sanitized to at least 1x1.
    pub fn new(cols: u16, rows: u16) -> Self {
        let (cols, rows) = sanitize_dimensions(cols, rows);
        Self {
            cols,
            rows,
            cells: vec![vec![Cell::default(); cols as usize]; rows as usize],
            cursor_x: 0,
            cursor_y: 0,
            wrap_pending: false,
            scroll_top: 0,
            scroll_bottom: rows - 1,
            cursor_visible: true,
            modes: TerminalModes::default(),
        }
    }

    /// Scroll the region up by one line, capturing scrollback on the main screen.
    pub fn scroll_up(
        &mut self,
        in_alt_screen: bool,
        scrollback: &mut VecDeque<Vec<u8>>,
        scrollback_limit: usize,
        pending_scrollback: &mut VecDeque<Vec<u8>>,
        fill: Cell,
    ) {
        let top = self.scroll_top as usize;
        let bottom = self.scroll_bottom as usize;

        // Capture scrollback whenever a line scrolls off the top of the screen
        if !in_alt_screen && top == 0 {
            let line = render_line(&self.cells[0]);
            if scrollback.len() >= scrollback_limit {
                scrollback.pop_front();
            }
            scrollback.push_back(line.clone());
            if pending_scrollback.len() >= scrollback_limit {
                pending_scrollback.pop_front();
            }
            pending_scrollback.push_back(line);
        }

        if top <= bottom && bottom < self.cells.len() {
            self.cells.remove(top);
            self.cells.insert(bottom, vec![fill; self.cols as usize]);
        }
    }

    /// Scroll the region down by one line, inserting a blank row at the top.
    pub fn scroll_down(&mut self, fill: Cell) {
        let top = self.scroll_top as usize;
        let bottom = self.scroll_bottom as usize;

        if top <= bottom && bottom < self.cells.len() {
            self.cells.remove(bottom);
            self.cells.insert(top, vec![fill; self.cols as usize]);
        }
    }

    /// Resize the grid, clamping cursor position and resetting scroll region.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = sanitize_dimensions(cols, rows);
        self.cols = cols;
        self.rows = rows;
        self.cells.resize(rows as usize, vec![Cell::default(); cols as usize]);
        for row in &mut self.cells {
            row.resize(cols as usize, Cell::default());
        }
        if self.cursor_x >= cols { self.cursor_x = cols - 1; }
        if self.cursor_y >= rows { self.cursor_y = rows - 1; }
        self.wrap_pending = false;
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
    }

}

/// Clamp dimensions to at least 1x1 to prevent underflow (fix I3)
pub fn sanitize_dimensions(cols: u16, rows: u16) -> (u16, u16) {
    (cols.max(1), rows.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_zero_dimensions() {
        assert_eq!(sanitize_dimensions(0, 0), (1, 1));
        assert_eq!(sanitize_dimensions(80, 0), (80, 1));
        assert_eq!(sanitize_dimensions(0, 24), (1, 24));
    }

    #[test]
    fn grid_new_creates_correct_size() {
        let grid = Grid::new(80, 24);
        assert_eq!(grid.cells.len(), 24);
        assert_eq!(grid.cells[0].len(), 80);
    }

    #[test]
    fn grid_new_zero_dimensions() {
        let grid = Grid::new(0, 0);
        assert_eq!(grid.cols, 1);
        assert_eq!(grid.rows, 1);
        assert_eq!(grid.cells.len(), 1);
        assert_eq!(grid.cells[0].len(), 1);
    }

    #[test]
    fn grid_resize() {
        let mut grid = Grid::new(80, 24);
        grid.cursor_x = 79;
        grid.cursor_y = 23;
        grid.resize(40, 12);
        assert_eq!(grid.cells.len(), 12);
        assert_eq!(grid.cells[0].len(), 40);
        assert_eq!(grid.cursor_x, 39);
        assert_eq!(grid.cursor_y, 11);
    }

    #[test]
    fn grid_resize_zero() {
        let mut grid = Grid::new(80, 24);
        grid.resize(0, 0);
        assert_eq!(grid.cols, 1);
        assert_eq!(grid.rows, 1);
    }

    #[test]
    fn grid_scroll_up() {
        let mut grid = Grid::new(10, 3);
        grid.cells[0][0].c = 'A';
        let mut scrollback = VecDeque::new();
        let mut pending = VecDeque::new();
        grid.scroll_up(false, &mut scrollback, 100, &mut pending, Cell::default());
        assert_eq!(scrollback.len(), 1);
        assert_eq!(pending.len(), 1);
        assert_eq!(grid.cells.len(), 3);
        // Row 0 should now be what was row 1 (blank)
        assert_eq!(grid.cells[0][0].c, ' ');
    }

    #[test]
    fn grid_scroll_up_alt_screen_no_scrollback() {
        let mut grid = Grid::new(10, 3);
        grid.cells[0][0].c = 'A';
        let mut scrollback = VecDeque::new();
        let mut pending = VecDeque::new();
        grid.scroll_up(true, &mut scrollback, 100, &mut pending, Cell::default());
        assert_eq!(scrollback.len(), 0);
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn grid_scroll_up_respects_limit() {
        let mut grid = Grid::new(10, 3);
        let mut scrollback = VecDeque::new();
        let mut pending = VecDeque::new();
        for _ in 0..5 {
            grid.scroll_up(false, &mut scrollback, 3, &mut pending, Cell::default());
        }
        assert_eq!(scrollback.len(), 3);
    }

    #[test]
    fn pending_scrollback_respects_limit() {
        let mut grid = Grid::new(10, 3);
        let mut scrollback = VecDeque::new();
        let mut pending = VecDeque::new();
        for _ in 0..20 {
            grid.scroll_up(false, &mut scrollback, 5, &mut pending, Cell::default());
        }
        assert!(pending.len() <= 5, "pending_scrollback should be bounded, got {}", pending.len());
    }

    #[test]
    fn terminal_modes_default() {
        let modes = TerminalModes::default();
        assert!(modes.autowrap_mode);
        assert!(!modes.cursor_key_mode);
        assert!(!modes.bracketed_paste);
        assert_eq!(modes.mouse_mode, 0);
        assert_eq!(modes.cursor_shape, CursorShape::Default);
    }
}
