use std::collections::VecDeque;

use super::cell::Cell;

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
    pub cells: VecDeque<Vec<Cell>>,
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
    /// Tab stop positions (true = tab stop set at this column)
    pub tab_stops: Vec<bool>,
}

/// Create default tab stops every 8 columns for the given width.
pub fn default_tab_stops(cols: u16) -> Vec<bool> {
    (0..cols).map(|c| c > 0 && c % 8 == 0).collect()
}

impl Grid {
    /// Create a grid with the given dimensions, sanitized to at least 1x1.
    pub fn new(cols: u16, rows: u16) -> Self {
        let (cols, rows) = sanitize_dimensions(cols, rows);
        Self {
            cols,
            rows,
            cells: (0..rows as usize).map(|_| vec![Cell::default(); cols as usize]).collect(),
            cursor_x: 0,
            cursor_y: 0,
            wrap_pending: false,
            scroll_top: 0,
            scroll_bottom: rows - 1,
            cursor_visible: true,
            modes: TerminalModes::default(),
            tab_stops: default_tab_stops(cols),
        }
    }

    /// Find the next tab stop column at or after `col`, clamped to right margin.
    pub fn next_tab_stop(&self, col: u16) -> u16 {
        for c in (col as usize + 1)..self.tab_stops.len() {
            if self.tab_stops[c] {
                return c as u16;
            }
        }
        self.cols - 1
    }

    /// Scroll the region up by one line, capturing scrollback on the main screen.
    pub fn scroll_up(
        &mut self,
        in_alt_screen: bool,
        scrollback: &mut VecDeque<Vec<Cell>>,
        scrollback_limit: usize,
        pending_scrollback: &mut VecDeque<Vec<Cell>>,
        fill: Cell,
    ) {
        let top = self.scroll_top as usize;
        let bottom = self.scroll_bottom as usize;

        // Capture scrollback whenever a line scrolls off the top of the screen
        if !in_alt_screen && top == 0 {
            let line = self.cells[0].clone();
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
            if top == 0 && bottom == self.cells.len() - 1 {
                // Full-screen scroll: O(1) with VecDeque
                self.cells.pop_front();
                self.cells.push_back(vec![fill; self.cols as usize]);
            } else {
                // Partial scroll region: O(n) remove+insert
                self.cells.remove(top);
                self.cells.insert(bottom, vec![fill; self.cols as usize]);
            }
        }
    }

    /// Scroll the region down by one line, inserting a blank row at the top.
    pub fn scroll_down(&mut self, fill: Cell) {
        let top = self.scroll_top as usize;
        let bottom = self.scroll_bottom as usize;

        if top <= bottom && bottom < self.cells.len() {
            if top == 0 && bottom == self.cells.len() - 1 {
                // Full-screen scroll: O(1) with VecDeque
                self.cells.pop_back();
                self.cells.push_front(vec![fill; self.cols as usize]);
            } else {
                // Partial scroll region: O(n) remove+insert
                self.cells.remove(bottom);
                self.cells.insert(top, vec![fill; self.cols as usize]);
            }
        }
    }

    /// Resize the grid, clamping cursor position and resetting scroll region and tab stops.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = sanitize_dimensions(cols, rows);
        self.cols = cols;
        self.rows = rows;
        let rows_usize = rows as usize;
        while self.cells.len() > rows_usize {
            self.cells.pop_back();
        }
        while self.cells.len() < rows_usize {
            self.cells.push_back(vec![Cell::default(); cols as usize]);
        }
        let cols_usize = cols as usize;
        for row in &mut self.cells {
            // Clean up orphaned wide chars at the new right edge:
            // if a width=2 cell at cols-1 would lose its continuation cell,
            // replace it with a blank to avoid rendering artifacts.
            if row.len() > cols_usize && cols_usize > 0 {
                let last = cols_usize - 1;
                if row[last].width == 2 {
                    row[last] = Cell::default();
                }
            }
            row.resize(cols_usize, Cell::default());
        }
        if self.cursor_x >= cols { self.cursor_x = cols - 1; }
        if self.cursor_y >= rows { self.cursor_y = rows - 1; }
        self.wrap_pending = false;
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.tab_stops = default_tab_stops(cols);
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

    // ---------------------------------------------------------------
    // Helper: paint a checkerboard pattern on the grid using two chars
    // ---------------------------------------------------------------

    /// Fill the grid with a checkerboard pattern: 'A' for even (row+col), 'B' for odd.
    fn paint_checkerboard(grid: &mut Grid) {
        for r in 0..grid.rows as usize {
            for c in 0..grid.cols as usize {
                grid.cells[r][c].c = if (r + c) % 2 == 0 { 'A' } else { 'B' };
            }
        }
    }

    /// Assert the checkerboard pattern holds for all cells within (rows x cols).
    fn assert_checkerboard(grid: &Grid, rows: usize, cols: usize) {
        for r in 0..rows {
            for c in 0..cols {
                let expected = if (r + c) % 2 == 0 { 'A' } else { 'B' };
                assert_eq!(grid.cells[r][c].c, expected,
                    "checkerboard mismatch at ({}, {}): expected '{}', got '{}'",
                    r, c, expected, grid.cells[r][c].c);
            }
        }
    }

    // ---------------------------------------------------------------
    // Horizontal resize — columns only
    // ---------------------------------------------------------------

    #[test]
    fn resize_horizontal_expand_preserves_content() {
        let mut grid = Grid::new(5, 4);
        paint_checkerboard(&mut grid);
        grid.resize(10, 4); // widen: 5 -> 10 cols, same rows
        assert_eq!(grid.cols, 10);
        assert_eq!(grid.cells[0].len(), 10);
        // Original 5x4 region untouched
        assert_checkerboard(&grid, 4, 5);
        // New columns should be blank
        for r in 0..4 {
            for c in 5..10 {
                assert_eq!(grid.cells[r][c].c, ' ',
                    "new cell at ({}, {}) should be blank", r, c);
            }
        }
    }

    #[test]
    fn resize_horizontal_shrink_preserves_visible_content() {
        let mut grid = Grid::new(10, 4);
        paint_checkerboard(&mut grid);
        grid.resize(5, 4); // narrow: 10 -> 5 cols
        assert_eq!(grid.cols, 5);
        assert_eq!(grid.cells[0].len(), 5);
        // First 5 columns of pattern intact
        assert_checkerboard(&grid, 4, 5);
    }

    #[test]
    fn resize_horizontal_shrink_then_expand_loses_truncated() {
        let mut grid = Grid::new(10, 3);
        paint_checkerboard(&mut grid);
        grid.resize(5, 3);   // shrink — cols 5..9 lost
        grid.resize(10, 3);  // expand back
        // First 5 cols: pattern intact
        assert_checkerboard(&grid, 3, 5);
        // Cols 5..9: blank (data was truncated, not recoverable)
        for r in 0..3 {
            for c in 5..10 {
                assert_eq!(grid.cells[r][c].c, ' ',
                    "truncated cell at ({}, {}) should be blank after re-expand", r, c);
            }
        }
    }

    // ---------------------------------------------------------------
    // Vertical resize — rows only
    // ---------------------------------------------------------------

    #[test]
    fn resize_vertical_expand_preserves_content() {
        let mut grid = Grid::new(6, 3);
        paint_checkerboard(&mut grid);
        grid.resize(6, 8); // taller: 3 -> 8 rows
        assert_eq!(grid.rows, 8);
        assert_eq!(grid.cells.len(), 8);
        // Original 3 rows intact
        assert_checkerboard(&grid, 3, 6);
        // New rows blank
        for r in 3..8 {
            for c in 0..6 {
                assert_eq!(grid.cells[r][c].c, ' ',
                    "new cell at ({}, {}) should be blank", r, c);
            }
        }
    }

    #[test]
    fn resize_vertical_shrink_preserves_visible_content() {
        let mut grid = Grid::new(6, 8);
        paint_checkerboard(&mut grid);
        grid.resize(6, 3); // shorter: 8 -> 3 rows
        assert_eq!(grid.rows, 3);
        assert_eq!(grid.cells.len(), 3);
        // First 3 rows of pattern intact
        assert_checkerboard(&grid, 3, 6);
    }

    #[test]
    fn resize_vertical_shrink_then_expand_loses_truncated() {
        let mut grid = Grid::new(6, 8);
        paint_checkerboard(&mut grid);
        grid.resize(6, 3);  // rows 3..7 lost
        grid.resize(6, 8);  // expand back
        assert_checkerboard(&grid, 3, 6);
        for r in 3..8 {
            for c in 0..6 {
                assert_eq!(grid.cells[r][c].c, ' ',
                    "truncated cell at ({}, {}) should be blank after re-expand", r, c);
            }
        }
    }

    // ---------------------------------------------------------------
    // Combined resize — both dimensions at once
    // ---------------------------------------------------------------

    #[test]
    fn resize_both_expand() {
        let mut grid = Grid::new(4, 3);
        paint_checkerboard(&mut grid);
        grid.resize(8, 6); // double both
        assert_checkerboard(&grid, 3, 4);
        // New cols in old rows blank
        for r in 0..3 {
            for c in 4..8 {
                assert_eq!(grid.cells[r][c].c, ' ',
                    "new col cell at ({}, {}) should be blank", r, c);
            }
        }
        // New rows entirely blank
        for r in 3..6 {
            for c in 0..8 {
                assert_eq!(grid.cells[r][c].c, ' ',
                    "new row cell at ({}, {}) should be blank", r, c);
            }
        }
    }

    #[test]
    fn resize_both_shrink() {
        let mut grid = Grid::new(10, 8);
        paint_checkerboard(&mut grid);
        grid.resize(5, 4); // halve both
        assert_eq!(grid.cells.len(), 4);
        assert_eq!(grid.cells[0].len(), 5);
        assert_checkerboard(&grid, 4, 5);
    }

    #[test]
    fn resize_expand_cols_shrink_rows() {
        let mut grid = Grid::new(4, 8);
        paint_checkerboard(&mut grid);
        grid.resize(10, 3); // wider but shorter
        assert_eq!(grid.cells.len(), 3);
        assert_eq!(grid.cells[0].len(), 10);
        // First 3 rows x 4 cols intact
        assert_checkerboard(&grid, 3, 4);
        // New cols in surviving rows blank
        for r in 0..3 {
            for c in 4..10 {
                assert_eq!(grid.cells[r][c].c, ' ',
                    "new cell at ({}, {}) should be blank", r, c);
            }
        }
    }

    #[test]
    fn resize_shrink_cols_expand_rows() {
        let mut grid = Grid::new(10, 3);
        paint_checkerboard(&mut grid);
        grid.resize(4, 8); // narrower but taller
        assert_eq!(grid.cells.len(), 8);
        assert_eq!(grid.cells[0].len(), 4);
        // First 3 rows x 4 cols intact
        assert_checkerboard(&grid, 3, 4);
        // New rows blank
        for r in 3..8 {
            for c in 0..4 {
                assert_eq!(grid.cells[r][c].c, ' ',
                    "new row cell at ({}, {}) should be blank", r, c);
            }
        }
    }

    // ---------------------------------------------------------------
    // Multiple sequential resizes — stress pattern preservation
    // ---------------------------------------------------------------

    #[test]
    fn resize_multiple_sequential_preserves_overlap() {
        let mut grid = Grid::new(10, 10);
        paint_checkerboard(&mut grid);
        // Shrink → expand → shrink differently
        grid.resize(5, 5);
        assert_checkerboard(&grid, 5, 5);
        grid.resize(8, 12);
        assert_checkerboard(&grid, 5, 5);
        grid.resize(3, 3);
        assert_checkerboard(&grid, 3, 3);
        grid.resize(20, 20);
        assert_checkerboard(&grid, 3, 3);
    }

    // ---------------------------------------------------------------
    // Resize with cursor in content area
    // ---------------------------------------------------------------

    #[test]
    fn resize_horizontal_shrink_clamps_cursor() {
        let mut grid = Grid::new(10, 5);
        grid.cursor_x = 8;
        grid.cursor_y = 2;
        grid.resize(5, 5);
        assert_eq!(grid.cursor_x, 4, "cursor_x should clamp to cols-1");
        assert_eq!(grid.cursor_y, 2, "cursor_y should not change");
    }

    #[test]
    fn resize_vertical_shrink_clamps_cursor() {
        let mut grid = Grid::new(10, 10);
        grid.cursor_x = 3;
        grid.cursor_y = 8;
        grid.resize(10, 5);
        assert_eq!(grid.cursor_x, 3, "cursor_x should not change");
        assert_eq!(grid.cursor_y, 4, "cursor_y should clamp to rows-1");
    }

    #[test]
    fn resize_both_shrink_clamps_cursor() {
        let mut grid = Grid::new(20, 20);
        grid.cursor_x = 15;
        grid.cursor_y = 18;
        grid.resize(5, 5);
        assert_eq!(grid.cursor_x, 4);
        assert_eq!(grid.cursor_y, 4);
    }

    #[test]
    fn resize_expand_preserves_cursor() {
        let mut grid = Grid::new(10, 10);
        grid.cursor_x = 5;
        grid.cursor_y = 7;
        grid.resize(20, 20);
        assert_eq!(grid.cursor_x, 5, "cursor_x should not change on expand");
        assert_eq!(grid.cursor_y, 7, "cursor_y should not change on expand");
    }

    // ---------------------------------------------------------------
    // Resize to same dimensions — no-op semantics
    // ---------------------------------------------------------------

    #[test]
    fn resize_same_dimensions_preserves_everything() {
        let mut grid = Grid::new(8, 6);
        paint_checkerboard(&mut grid);
        grid.cursor_x = 3;
        grid.cursor_y = 2;
        grid.resize(8, 6); // same
        assert_checkerboard(&grid, 6, 8);
        assert_eq!(grid.cursor_x, 3);
        assert_eq!(grid.cursor_y, 2);
    }

    // ---------------------------------------------------------------
    // Resize scroll region / tab stops reset
    // ---------------------------------------------------------------

    #[test]
    fn resize_resets_scroll_region() {
        let mut grid = Grid::new(80, 24);
        grid.scroll_top = 5;
        grid.scroll_bottom = 18;
        grid.resize(80, 30);
        assert_eq!(grid.scroll_top, 0);
        assert_eq!(grid.scroll_bottom, 29, "scroll_bottom should be rows-1");
    }

    #[test]
    fn resize_resets_tab_stops() {
        let mut grid = Grid::new(80, 24);
        // Manually set a custom tab stop
        grid.tab_stops[3] = true;
        grid.resize(40, 24);
        assert_eq!(grid.tab_stops.len(), 40);
        // Tab stops should be default (every 8 cols)
        assert!(!grid.tab_stops[0]);
        assert!(grid.tab_stops[8]);
        assert!(grid.tab_stops[16]);
        assert!(!grid.tab_stops[3], "custom tab stop should be gone after resize");
    }
}
