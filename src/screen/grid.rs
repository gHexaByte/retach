use std::collections::VecDeque;

use super::cell::{Cell, Row};
use super::style::StyleTable;

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
    pub fn from_param(n: u8) -> Self {
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
    pub origin_mode: bool,        // ?6 DECOM
    pub autowrap_mode: bool,      // ?7 DECAWM (default true)
    pub focus_reporting: bool,    // ?1004
    pub mouse_modes: MouseModes,
    pub mouse_encoding: MouseEncoding,
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
            origin_mode: false,
            autowrap_mode: true,
            focus_reporting: false,
            mouse_modes: MouseModes::default(),
            mouse_encoding: MouseEncoding::X10,
            keypad_app_mode: false,
            cursor_shape: CursorShape::Default,
            g0_charset: Charset::Ascii,
            g1_charset: Charset::Ascii,
            active_charset: ActiveCharset::G0,
        }
    }
}

/// Mouse tracking mode.
#[cfg(test)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Hash)]
pub enum MouseMode {
    #[default]
    Off,
    Click,  // ?1000
    Button, // ?1002
    Any,    // ?1003
}

/// Per-mode mouse tracking flags, matching xterm behavior.
/// Each mode can be independently enabled/disabled. The effective mode
/// is the highest-priority enabled mode.
#[derive(Clone, Debug, Default, PartialEq, Hash)]
pub struct MouseModes {
    pub click: bool,   // ?1000
    pub button: bool,  // ?1002
    pub any: bool,     // ?1003
}

impl MouseModes {
    /// Set a mouse mode flag from a DEC private mode parameter.
    pub fn set(&mut self, param: u16, enable: bool) {
        match param {
            1000 => self.click = enable,
            1002 => self.button = enable,
            1003 => self.any = enable,
            _ => {}
        }
    }

    /// Return the effective mouse mode (highest priority enabled).
    #[cfg(test)]
    pub fn effective(&self) -> MouseMode {
        if self.any { MouseMode::Any }
        else if self.button { MouseMode::Button }
        else if self.click { MouseMode::Click }
        else { MouseMode::Off }
    }
}

/// Mouse coordinate encoding.
#[derive(Copy, Clone, Debug, Default, PartialEq, Hash)]
pub enum MouseEncoding {
    #[default]
    X10,
    Utf8, // ?1005
    Sgr,  // ?1006
}

impl MouseEncoding {
    /// Convert from a DEC private mode parameter.
    pub fn from_param(p: u16) -> Option<Self> {
        match p {
            1005 => Some(Self::Utf8),
            1006 => Some(Self::Sgr),
            _ => None,
        }
    }
}

/// Two-dimensional cell storage with cursor position, scroll region, and terminal modes.
///
/// Uses a unified buffer: `cells` holds `[scrollback | visible]` rows.
/// `scrollback_len` marks the boundary; `pending_start` tracks unsent scrollback.
pub struct Grid {
    cols: u16,
    rows: u16,
    /// Unified buffer: `cells[0..scrollback_len]` = scrollback,
    /// `cells[scrollback_len..]` = visible rows.
    cells: VecDeque<Row>,
    cursor_x: u16,
    cursor_y: u16,
    /// Deferred wrap: cursor is at the right margin, next printable char triggers wrap
    wrap_pending: bool,
    /// Scroll region top (inclusive, 0-based)
    scroll_top: u16,
    /// Scroll region bottom (inclusive, 0-based)
    scroll_bottom: u16,
    /// Cursor visibility (DECTCEM ?25h/?25l)
    cursor_visible: bool,
    /// Terminal modes and character set state
    modes: TerminalModes,
    /// Tab stop positions (true = tab stop set at this column)
    tab_stops: Vec<bool>,
    /// Number of scrollback rows at the front of `cells`
    scrollback_len: usize,
    /// Maximum number of scrollback lines to retain
    scrollback_limit: usize,
    /// Index where unsent scrollback begins (for live client updates)
    pending_start: usize,
    /// Interned style table shared by all cells in this grid
    style_table: StyleTable,
}

/// Saved visible rows and scrollback limit for alt screen save/restore.
/// Scrollback rows stay in the active grid during alt screen.
pub struct SavedGrid {
    visible_cells: VecDeque<Row>,
    scrollback_limit: usize,
}

impl SavedGrid {
    /// Create a new SavedGrid.
    pub(super) fn new(visible_cells: VecDeque<Row>, scrollback_limit: usize) -> Self {
        Self { visible_cells, scrollback_limit }
    }

    /// Iterate over saved visible rows (for style compaction).
    pub(super) fn visible_rows(&self) -> impl Iterator<Item = &Row> {
        self.visible_cells.iter()
    }

    /// Scrollback limit that was saved.
    pub(super) fn scrollback_limit(&self) -> usize {
        self.scrollback_limit
    }

    /// Consume the SavedGrid, returning (visible_cells, scrollback_limit).
    pub(super) fn into_parts(self) -> (VecDeque<Row>, usize) {
        (self.visible_cells, self.scrollback_limit)
    }
}

/// Create default tab stops every 8 columns for the given width.
pub(super) fn default_tab_stops(cols: u16) -> Vec<bool> {
    // Default tab stops every 8 columns per ECMA-48 / VT100 standard.
    (0..cols).map(|c| c > 0 && c % 8 == 0).collect()
}

impl Grid {
    /// Create a grid with the given dimensions, sanitized to at least 1x1.
    pub fn new(cols: u16, rows: u16, scrollback_limit: usize) -> Self {
        let TerminalSize { cols, rows } = sanitize_dimensions(cols, rows);
        Self {
            cols,
            rows,
            cells: (0..rows as usize).map(|_| Row::new(cols as usize)).collect(),
            cursor_x: 0,
            cursor_y: 0,
            wrap_pending: false,
            scroll_top: 0,
            scroll_bottom: rows - 1,
            cursor_visible: true,
            modes: TerminalModes::default(),
            tab_stops: default_tab_stops(cols),
            scrollback_len: 0,
            scrollback_limit,
            pending_start: 0,
            style_table: StyleTable::new(),
        }
    }

    // =================================================================
    // Public read-only accessors
    // =================================================================

    /// Number of columns.
    #[inline]
    pub fn cols(&self) -> u16 { self.cols }

    /// Number of visible rows.
    #[inline]
    pub fn rows(&self) -> u16 { self.rows }

    /// Current cursor position (x, y), both 0-based.
    #[inline]
    pub fn cursor_pos(&self) -> (u16, u16) { (self.cursor_x, self.cursor_y) }

    /// Whether the cursor is currently visible (DECTCEM).
    #[inline]
    pub fn cursor_visible(&self) -> bool { self.cursor_visible }

    /// Current scroll region (top, bottom), both 0-based inclusive.
    #[inline]
    pub fn scroll_region(&self) -> (u16, u16) { (self.scroll_top, self.scroll_bottom) }

    /// Terminal modes (read-only).
    #[inline]
    pub fn modes(&self) -> &TerminalModes { &self.modes }

    /// Number of scrollback rows.
    #[inline]
    pub fn scrollback_len(&self) -> usize { self.scrollback_len }

    /// Maximum scrollback lines to retain.
    #[inline]
    pub fn scrollback_limit(&self) -> usize { self.scrollback_limit }

    /// Index where unsent scrollback begins.
    #[inline]
    pub fn pending_start(&self) -> usize { self.pending_start }

    /// Style table (read-only).
    #[inline]
    pub fn style_table(&self) -> &StyleTable { &self.style_table }

    /// Whether a deferred wrap is pending.
    #[inline]
    pub fn wrap_pending(&self) -> bool { self.wrap_pending }

    /// Whether a tab stop is set at the given column.
    #[cfg(test)]
    #[inline]
    pub fn tab_stop_at(&self, col: usize) -> bool {
        self.tab_stops.get(col).copied().unwrap_or(false)
    }

    /// Number of tab stop slots (equals column count).
    #[cfg(test)]
    #[inline]
    pub fn tab_stops_len(&self) -> usize { self.tab_stops.len() }

    // =================================================================
    // Mutation accessors (for performer)
    // =================================================================

    /// Current cursor X position.
    #[inline]
    pub(super) fn cursor_x(&self) -> u16 { self.cursor_x }

    /// Current cursor Y position.
    #[inline]
    pub(super) fn cursor_y(&self) -> u16 { self.cursor_y }

    /// Set cursor X (caller ensures valid value).
    #[inline]
    pub(super) fn set_cursor_x_unclamped(&mut self, x: u16) {
        self.cursor_x = x;
    }

    /// Set cursor Y (caller ensures valid value).
    #[inline]
    pub(super) fn set_cursor_y_unclamped(&mut self, y: u16) {
        self.cursor_y = y;
    }

    /// Set wrap_pending flag.
    #[inline]
    pub(super) fn set_wrap_pending(&mut self, val: bool) {
        self.wrap_pending = val;
    }

    /// Set cursor visibility.
    #[inline]
    pub(super) fn set_cursor_visible(&mut self, visible: bool) {
        self.cursor_visible = visible;
    }

    /// Set scroll region. Caller must ensure top <= bottom < rows.
    #[inline]
    pub(super) fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        self.scroll_top = top;
        self.scroll_bottom = bottom;
    }

    /// Scroll region top (0-based inclusive).
    #[inline]
    pub(super) fn scroll_top(&self) -> u16 { self.scroll_top }

    /// Scroll region bottom (0-based inclusive).
    #[inline]
    pub(super) fn scroll_bottom(&self) -> u16 { self.scroll_bottom }

    /// Terminal modes (mutable).
    #[inline]
    pub(super) fn modes_mut(&mut self) -> &mut TerminalModes { &mut self.modes }

    /// Set terminal modes directly (for restore).
    #[inline]
    pub(super) fn set_modes(&mut self, modes: TerminalModes) {
        self.modes = modes;
    }

    /// Style table (mutable).
    #[inline]
    pub(super) fn style_table_mut(&mut self) -> &mut StyleTable { &mut self.style_table }

    /// Set a tab stop at the given column.
    pub(super) fn set_tab_stop(&mut self, col: u16) {
        let c = col as usize;
        if c < self.tab_stops.len() { self.tab_stops[c] = true; }
    }

    /// Clear a tab stop at the given column.
    pub(super) fn clear_tab_stop(&mut self, col: u16) {
        let c = col as usize;
        if c < self.tab_stops.len() { self.tab_stops[c] = false; }
    }

    /// Clear all tab stops.
    pub(super) fn clear_all_tab_stops(&mut self) {
        for stop in self.tab_stops.iter_mut() { *stop = false; }
    }

    /// Reset tab stops to default (every 8 columns).
    pub(super) fn reset_tab_stops(&mut self) {
        self.tab_stops = default_tab_stops(self.cols);
    }

    /// Set maximum scrollback limit.
    #[inline]
    pub(super) fn set_scrollback_limit(&mut self, limit: usize) {
        self.scrollback_limit = limit;
    }

    /// Set pending_start index.
    #[inline]
    pub(super) fn set_pending_start(&mut self, val: usize) {
        self.pending_start = val;
    }

    /// Iterate over scrollback rows.
    pub fn scrollback_rows(&self) -> impl Iterator<Item = &Row> {
        self.cells.iter().take(self.scrollback_len)
    }

    /// Drain visible rows for alt screen save. Returns saved rows.
    pub(super) fn drain_visible(&mut self) -> VecDeque<Row> {
        self.cells.drain(self.scrollback_len..).collect()
    }

    /// Replace visible rows (for alt screen restore).
    pub(super) fn replace_visible(&mut self, rows: VecDeque<Row>) {
        self.cells.truncate(self.scrollback_len);
        for row in rows {
            self.cells.push_back(row);
        }
    }

    /// Add blank visible rows for alt screen.
    pub(super) fn fill_visible_blank(&mut self) {
        for _ in 0..self.rows as usize {
            self.cells.push_back(Row::new(self.cols as usize));
        }
        self.check_invariants();
    }

    /// Adjust visible row count to match `self.rows` (trim or pad).
    pub(super) fn adjust_visible_to_fit(&mut self) {
        let rows_usize = self.rows as usize;
        while self.visible_row_count() > rows_usize {
            self.cells.pop_back();
        }
        while self.visible_row_count() < rows_usize {
            self.cells.push_back(Row::new(self.cols as usize));
        }
        self.check_invariants();
    }

    /// Decrement scrollback_len by `count` (for restoring scrollback on vertical grow).
    /// Note: does NOT call check_invariants() because the caller (Screen::resize)
    /// adjusts visible row count via grid.resize() immediately after.
    pub(super) fn restore_scrollback(&mut self, count: usize) {
        debug_assert!(count <= self.scrollback_len);
        self.scrollback_len = self.scrollback_len.saturating_sub(count);
        self.pending_start = self.pending_start.min(self.scrollback_len);
    }

    /// Access a scrollback row by index (for tests).
    #[cfg(test)]
    pub(super) fn scrollback_row(&self, idx: usize) -> &Row {
        &self.cells[idx]
    }

    // =================================================================
    // Existing methods
    // =================================================================

    /// Access a visible row by index.
    pub fn visible_row(&self, y: usize) -> &Row {
        debug_assert!(
            y < self.rows as usize,
            "visible_row: y={} out of bounds (rows={})", y, self.rows
        );
        &self.cells[self.scrollback_len + y]
    }

    /// Mutably access a visible row by index.
    pub fn visible_row_mut(&mut self, y: usize) -> &mut Row {
        debug_assert!(
            y < self.rows as usize,
            "visible_row_mut: y={} out of bounds (rows={})", y, self.rows
        );
        let offset = self.scrollback_len;
        &mut self.cells[offset + y]
    }

    /// Iterate over visible rows.
    pub fn visible_rows(&self) -> impl Iterator<Item = &Row> {
        self.cells.iter().skip(self.scrollback_len).take(self.rows as usize)
    }

    /// Mutably iterate over visible rows.
    pub fn visible_rows_mut(&mut self) -> impl Iterator<Item = &mut Row> {
        let skip = self.scrollback_len;
        let take = self.rows as usize;
        self.cells.iter_mut().skip(skip).take(take)
    }

    /// Number of visible rows.
    pub fn visible_row_count(&self) -> usize {
        self.cells.len().saturating_sub(self.scrollback_len)
    }

    /// Remove a visible row by index, returning it.
    pub fn remove_visible_row(&mut self, y: usize) -> Row {
        let idx = self.scrollback_len + y;
        debug_assert!(idx < self.cells.len(), "remove_visible_row: index {} out of bounds (cells len {})", idx, self.cells.len());
        self.cells.remove(idx).unwrap_or_else(|| Row::new(self.cols as usize))
    }

    /// Insert a row at a visible row index.
    pub fn insert_visible_row(&mut self, y: usize, row: Row) {
        self.cells.insert(self.scrollback_len + y, row);
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
    ///
    /// When scrollback is enabled and the full screen scrolls, the top visible row
    /// becomes a scrollback row by moving the boundary — zero clones.
    pub fn scroll_up(&mut self, in_alt_screen: bool, fill: Cell) {
        let top = self.scroll_top as usize;
        let bottom = self.scroll_bottom as usize;
        let visible_len = self.cells.len() - self.scrollback_len;

        if !in_alt_screen && top == 0 && self.scrollback_limit > 0 {
            // Top visible row becomes scrollback — just move the boundary
            self.scrollback_len += 1;
            if self.scrollback_len > self.scrollback_limit {
                self.cells.pop_front();
                self.scrollback_len -= 1;
                if self.pending_start > 0 { self.pending_start -= 1; }
            }
            // Insert blank row at the scroll region bottom, not necessarily
            // the end of cells (partial scroll region must not shift rows below).
            if bottom >= visible_len - 1 {
                self.cells.push_back(Row::from_cells(vec![fill; self.cols as usize]));
            } else {
                self.cells.insert(self.scrollback_len + bottom, Row::from_cells(vec![fill; self.cols as usize]));
            }
        } else if top <= bottom && bottom < visible_len {
            if top == 0 && bottom == visible_len - 1 {
                // Full screen, no scrollback: O(1)
                self.cells.remove(self.scrollback_len);
                self.cells.push_back(Row::from_cells(vec![fill; self.cols as usize]));
            } else {
                // Partial scroll region
                self.cells.remove(self.scrollback_len + top);
                self.cells.insert(self.scrollback_len + bottom, Row::from_cells(vec![fill; self.cols as usize]));
            }
        }
        self.check_invariants();
    }

    /// Scroll the region down by one line, inserting a blank row at the top.
    pub fn scroll_down(&mut self, fill: Cell) {
        let top = self.scroll_top as usize;
        let bottom = self.scroll_bottom as usize;
        let visible_len = self.cells.len() - self.scrollback_len;

        if top <= bottom && bottom < visible_len {
            self.cells.remove(self.scrollback_len + bottom);
            self.cells.insert(self.scrollback_len + top, Row::from_cells(vec![fill; self.cols as usize]));
        }
        self.check_invariants();
    }

    /// Clear all scrollback rows and reset pending counters.
    pub fn clear_scrollback(&mut self) {
        self.cells.drain(..self.scrollback_len);
        self.scrollback_len = 0;
        self.pending_start = 0;
        self.check_invariants();
    }

    /// Reset scroll region to full screen.
    pub fn reset_scroll_region(&mut self) {
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
    }

    /// Create a new blank row filled with `fill`, matching grid width.
    pub fn new_blank_row(&self, fill: Cell) -> Row {
        Row::from_cells(vec![fill; self.cols as usize])
    }

    /// Number of scrollback rows that haven't been sent to the client yet.
    pub fn pending_scrollback_count(&self) -> usize {
        self.scrollback_len.saturating_sub(self.pending_start)
    }

    /// Resize the grid, clamping cursor position and resetting scroll region and tab stops.
    /// Only resizes visible rows; scrollback rows keep their original column width.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let TerminalSize { cols, rows } = sanitize_dimensions(cols, rows);
        self.cols = cols;
        self.rows = rows;
        let rows_usize = rows as usize;
        let visible_len = self.cells.len() - self.scrollback_len;
        if visible_len > rows_usize {
            let excess = visible_len - rows_usize;
            for _ in 0..excess { self.cells.pop_back(); }
        } else if visible_len < rows_usize {
            let deficit = rows_usize - visible_len;
            for _ in 0..deficit {
                self.cells.push_back(Row::new(cols as usize));
            }
        }
        let cols_usize = cols as usize;
        for row in self.cells.iter_mut().skip(self.scrollback_len) {
            row.fix_wide_char_orphan_at_boundary(cols_usize);
            row.resize(cols_usize, Cell::default());
        }
        if self.cursor_x >= cols { self.cursor_x = cols - 1; }
        if self.cursor_y >= rows { self.cursor_y = rows - 1; }
        self.wrap_pending = false;
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.tab_stops = default_tab_stops(cols);
        self.check_invariants();
    }

    /// Write a cell safely, with automatic wide-char fixup and continuation placement.
    ///
    /// - If overwriting part of a wide char, blanks the other half
    /// - If `cell.width == 2`, places continuation cell at `x+1`
    /// - Clears combining marks at affected positions
    /// - No-op if (x, y) is out of bounds
    pub fn set_cell(&mut self, x: usize, y: usize, cell: Cell) {
        if y >= self.rows as usize || x >= self.cols as usize {
            return;
        }
        self.fixup_wide_char_at(x, y);
        let row = self.visible_row_mut(y);
        row[x] = cell;
        row.clear_combining(x as u16);
        if cell.width == 2 {
            let next = x + 1;
            if next < self.cols as usize {
                self.fixup_wide_char_at(next, y);
                let row = self.visible_row_mut(y);
                row[next] = Cell::new('\0', cell.style_id, 0);
                row.clear_combining(next as u16);
            }
        }
    }

    /// Erase cells in range [from, to) on row y, with wide-char fixup at boundaries.
    pub fn erase_cells(&mut self, y: usize, from: usize, to: usize, blank: Cell) {
        if y >= self.rows as usize {
            return;
        }
        let cols = self.cols as usize;
        let from = from.min(cols);
        let to = to.min(cols);
        if from >= to {
            return;
        }
        self.fixup_wide_char_at(from, y);
        if to < cols {
            self.fixup_wide_char_at(to, y);
        }
        let row = self.visible_row_mut(y);
        for i in from..to {
            row[i] = blank;
        }
        row.clear_combining_range(from as u16, to as u16);
    }

    /// Erase all cells in rows [from_y, to_y) with `blank`.
    pub fn erase_rows(&mut self, from_y: usize, to_y: usize, blank: Cell) {
        let max_y = self.rows as usize;
        let from_y = from_y.min(max_y);
        let to_y = to_y.min(max_y);
        for y in from_y..to_y {
            let row = self.visible_row_mut(y);
            for cell in row.iter_mut() {
                *cell = blank;
            }
            row.clear_all_combining();
        }
    }

    /// Fix up a wide char at position (x, y).
    /// If (x, y) is part of a wide char pair, blanks both halves.
    pub(crate) fn fixup_wide_char_at(&mut self, x: usize, y: usize) {
        if y >= self.rows as usize || x >= self.cols as usize {
            return;
        }
        let cell_width = self.visible_row(y)[x].width;
        if cell_width == 2 {
            let next = x + 1;
            if next < self.cols as usize {
                self.visible_row_mut(y)[next] = Cell::default();
            }
            self.visible_row_mut(y)[x] = Cell::default();
        } else if cell_width == 0 && x > 0 {
            self.visible_row_mut(y)[x - 1] = Cell::default();
            self.visible_row_mut(y)[x] = Cell::default();
        }
    }

    /// Debug-only invariant check. Call after mutations in debug builds.
    #[cfg(debug_assertions)]
    pub(crate) fn check_invariants(&self) {
        debug_assert!(
            self.pending_start <= self.scrollback_len,
            "pending_start ({}) > scrollback_len ({})",
            self.pending_start, self.scrollback_len
        );
        let expected_total = self.scrollback_len + self.rows as usize;
        debug_assert!(
            self.cells.len() == expected_total,
            "cells.len() ({}) != scrollback_len ({}) + rows ({})",
            self.cells.len(), self.scrollback_len, self.rows
        );
    }

    #[cfg(not(debug_assertions))]
    #[inline(always)]
    pub(crate) fn check_invariants(&self) {}

}

/// Terminal dimensions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

/// Clamp dimensions to at least 1x1 to prevent underflow (fix I3)
pub fn sanitize_dimensions(cols: u16, rows: u16) -> TerminalSize {
    TerminalSize { cols: cols.max(1), rows: rows.max(1) }
}

use super::grid_mutator::GridMutator;

impl GridMutator for Grid {
    fn cols(&self) -> u16 { self.cols }
    fn rows(&self) -> u16 { self.rows }
    fn visible_row_count(&self) -> usize { self.cells.len().saturating_sub(self.scrollback_len) }
    fn cursor_x(&self) -> u16 { self.cursor_x }
    fn cursor_y(&self) -> u16 { self.cursor_y }
    fn set_cursor_x_unclamped(&mut self, x: u16) { self.cursor_x = x; }
    fn set_cursor_y_unclamped(&mut self, y: u16) { self.cursor_y = y; }
    fn wrap_pending(&self) -> bool { self.wrap_pending }
    fn set_wrap_pending(&mut self, val: bool) { self.wrap_pending = val; }
    fn set_cursor_visible(&mut self, visible: bool) { self.cursor_visible = visible; }
    fn scroll_top(&self) -> u16 { self.scroll_top }
    fn scroll_bottom(&self) -> u16 { self.scroll_bottom }
    fn set_scroll_region(&mut self, top: u16, bottom: u16) { Grid::set_scroll_region(self, top, bottom) }
    fn reset_scroll_region(&mut self) { Grid::reset_scroll_region(self) }
    fn visible_row(&self, y: usize) -> &Row { Grid::visible_row(self, y) }
    fn visible_row_mut(&mut self, y: usize) -> &mut Row { Grid::visible_row_mut(self, y) }
    fn new_blank_row(&self, fill: Cell) -> Row { Grid::new_blank_row(self, fill) }
    fn remove_visible_row(&mut self, y: usize) -> Row { Grid::remove_visible_row(self, y) }
    fn insert_visible_row(&mut self, y: usize, row: Row) { Grid::insert_visible_row(self, y, row) }
    fn set_cell(&mut self, x: usize, y: usize, cell: Cell) { Grid::set_cell(self, x, y, cell) }
    fn erase_cells(&mut self, y: usize, from: usize, to: usize, blank: Cell) { Grid::erase_cells(self, y, from, to, blank) }
    fn erase_rows(&mut self, from_y: usize, to_y: usize, blank: Cell) { Grid::erase_rows(self, from_y, to_y, blank) }
    fn fixup_wide_char_at(&mut self, x: usize, y: usize) { Grid::fixup_wide_char_at(self, x, y) }
    fn scroll_up(&mut self, in_alt_screen: bool, fill: Cell) { Grid::scroll_up(self, in_alt_screen, fill) }
    fn scroll_down(&mut self, fill: Cell) { Grid::scroll_down(self, fill) }
    fn modes(&self) -> &TerminalModes { &self.modes }
    fn modes_mut(&mut self) -> &mut TerminalModes { &mut self.modes }
    fn set_modes(&mut self, modes: TerminalModes) { self.modes = modes; }
    fn style_table(&self) -> &StyleTable { &self.style_table }
    fn style_table_mut(&mut self) -> &mut StyleTable { &mut self.style_table }
    fn next_tab_stop(&self, col: u16) -> u16 { Grid::next_tab_stop(self, col) }
    fn set_tab_stop(&mut self, col: u16) { Grid::set_tab_stop(self, col) }
    fn clear_tab_stop(&mut self, col: u16) { Grid::clear_tab_stop(self, col) }
    fn clear_all_tab_stops(&mut self) { Grid::clear_all_tab_stops(self) }
    fn reset_tab_stops(&mut self) { Grid::reset_tab_stops(self) }
    fn drain_visible(&mut self) -> VecDeque<Row> { Grid::drain_visible(self) }
    fn fill_visible_blank(&mut self) { Grid::fill_visible_blank(self) }
    fn replace_visible(&mut self, rows: VecDeque<Row>) { Grid::replace_visible(self, rows) }
    fn adjust_visible_to_fit(&mut self) { Grid::adjust_visible_to_fit(self) }
    fn set_scrollback_limit(&mut self, limit: usize) { Grid::set_scrollback_limit(self, limit) }
    fn scrollback_limit(&self) -> usize { self.scrollback_limit }
    fn clear_scrollback(&mut self) { Grid::clear_scrollback(self) }
    fn compact_styles(&mut self, saved_grid: Option<&SavedGrid>) {
        super::compact_styles(self, saved_grid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_zero_dimensions() {
        assert_eq!(sanitize_dimensions(0, 0), TerminalSize { cols: 1, rows: 1 });
        assert_eq!(sanitize_dimensions(80, 0), TerminalSize { cols: 80, rows: 1 });
        assert_eq!(sanitize_dimensions(0, 24), TerminalSize { cols: 1, rows: 24 });
    }

    #[test]
    fn grid_new_creates_correct_size() {
        let grid = Grid::new(80, 24, 0);
        assert_eq!(grid.visible_row_count(), 24);
        assert_eq!(grid.visible_row(0).len(), 80);
    }

    #[test]
    fn grid_new_zero_dimensions() {
        let grid = Grid::new(0, 0, 0);
        assert_eq!(grid.cols(), 1);
        assert_eq!(grid.rows(), 1);
        assert_eq!(grid.visible_row_count(), 1);
        assert_eq!(grid.visible_row(0).len(), 1);
    }

    #[test]
    fn grid_resize() {
        let mut grid = Grid::new(80, 24, 0);
        grid.set_cursor_x_unclamped(79);
        grid.set_cursor_y_unclamped(23);
        grid.resize(40, 12);
        assert_eq!(grid.visible_row_count(), 12);
        assert_eq!(grid.visible_row(0).len(), 40);
        assert_eq!(grid.cursor_x(), 39);
        assert_eq!(grid.cursor_y(), 11);
    }

    #[test]
    fn grid_resize_zero() {
        let mut grid = Grid::new(80, 24, 0);
        grid.resize(0, 0);
        assert_eq!(grid.cols(), 1);
        assert_eq!(grid.rows(), 1);
    }

    #[test]
    fn grid_scroll_up() {
        let mut grid = Grid::new(10, 3, 100);
        grid.visible_row_mut(0)[0].c = 'A';
        grid.scroll_up(false, Cell::default());
        assert_eq!(grid.scrollback_len(), 1);
        // Pending count = scrollback_len - pending_start
        assert_eq!(grid.scrollback_len() - grid.pending_start(), 1);
        assert_eq!(grid.visible_row_count(), 3);
        // Scrollback row should contain 'A'
        assert_eq!(grid.scrollback_row(0)[0].c, 'A');
        // Row 0 should now be what was row 1 (blank)
        assert_eq!(grid.visible_row(0)[0].c, ' ');
    }

    #[test]
    fn grid_scroll_up_alt_screen_no_scrollback() {
        let mut grid = Grid::new(10, 3, 100);
        grid.visible_row_mut(0)[0].c = 'A';
        grid.scroll_up(true, Cell::default());
        assert_eq!(grid.scrollback_len(), 0);
    }

    #[test]
    fn grid_scroll_up_respects_limit() {
        let mut grid = Grid::new(10, 3, 3);
        for _ in 0..5 {
            grid.scroll_up(false, Cell::default());
        }
        assert_eq!(grid.scrollback_len(), 3);
    }

    #[test]
    fn pending_scrollback_respects_limit() {
        let mut grid = Grid::new(10, 3, 5);
        for _ in 0..20 {
            grid.scroll_up(false, Cell::default());
        }
        let pending_count = grid.scrollback_len() - grid.pending_start();
        assert_eq!(pending_count, 5, "pending scrollback should be exactly at limit, got {}", pending_count);
    }

    /// scroll_up with partial scroll region (top=0, bottom < last row) and scrollback enabled
    /// should NOT corrupt rows below the scroll region.
    #[test]
    fn scroll_up_partial_region_with_scrollback_preserves_rows_below() {
        // 5 visible rows, scrollback enabled, scroll region = rows 0..2
        let mut grid = Grid::new(5, 5, 100);
        // Label rows: A=0, B=1, C=2, D=3, E=4
        for (r, ch) in ['A', 'B', 'C', 'D', 'E'].iter().enumerate() {
            grid.visible_row_mut(r)[0].c = *ch;
        }
        grid.set_scroll_region(0, 2); // partial: rows 0-2 scroll, rows 3-4 fixed

        grid.scroll_up(false, Cell::default());

        // Row 'A' should go to scrollback
        assert_eq!(grid.scrollback_len(), 1);
        assert_eq!(grid.scrollback_row(0)[0].c, 'A'); // scrollback row

        // Visible: [B, C, blank, D, E]
        assert_eq!(grid.visible_row(0)[0].c, 'B', "row 0 should be B");
        assert_eq!(grid.visible_row(1)[0].c, 'C', "row 1 should be C");
        assert_eq!(grid.visible_row(2)[0].c, ' ', "row 2 should be blank (new)");
        assert_eq!(grid.visible_row(3)[0].c, 'D', "row 3 should be D (untouched)");
        assert_eq!(grid.visible_row(4)[0].c, 'E', "row 4 should be E (untouched)");

        // Total visible rows should still be 5
        assert_eq!(grid.visible_row_count(), 5);
    }

    /// scroll_up with full scroll region + scrollback should still work (regression guard)
    #[test]
    fn scroll_up_full_region_with_scrollback_still_works() {
        let mut grid = Grid::new(5, 5, 100);
        for (r, ch) in ['A', 'B', 'C', 'D', 'E'].iter().enumerate() {
            grid.visible_row_mut(r)[0].c = *ch;
        }
        // Full screen scroll region (default)
        assert_eq!(grid.scroll_top(), 0);
        assert_eq!(grid.scroll_bottom(), 4);

        grid.scroll_up(false, Cell::default());

        assert_eq!(grid.scrollback_len(), 1);
        assert_eq!(grid.scrollback_row(0)[0].c, 'A');
        assert_eq!(grid.visible_row(0)[0].c, 'B');
        assert_eq!(grid.visible_row(1)[0].c, 'C');
        assert_eq!(grid.visible_row(2)[0].c, 'D');
        assert_eq!(grid.visible_row(3)[0].c, 'E');
        assert_eq!(grid.visible_row(4)[0].c, ' ');
        assert_eq!(grid.visible_row_count(), 5);
    }

    #[test]
    fn terminal_modes_default() {
        let modes = TerminalModes::default();
        assert!(modes.autowrap_mode);
        assert!(!modes.cursor_key_mode);
        assert!(!modes.bracketed_paste);
        assert_eq!(modes.mouse_modes, MouseModes::default());
        assert_eq!(modes.cursor_shape, CursorShape::Default);
    }

    // ---------------------------------------------------------------
    // Helper: paint a checkerboard pattern on the grid using two chars
    // ---------------------------------------------------------------

    /// Fill the grid with a checkerboard pattern: 'A' for even (row+col), 'B' for odd.
    fn paint_checkerboard(grid: &mut Grid) {
        for r in 0..grid.rows() as usize {
            for c in 0..grid.cols() as usize {
                grid.visible_row_mut(r)[c].c = if (r + c) % 2 == 0 { 'A' } else { 'B' };
            }
        }
    }

    /// Assert the checkerboard pattern holds for all cells within (rows x cols).
    fn assert_checkerboard(grid: &Grid, rows: usize, cols: usize) {
        for r in 0..rows {
            for c in 0..cols {
                let expected = if (r + c) % 2 == 0 { 'A' } else { 'B' };
                assert_eq!(grid.visible_row(r)[c].c, expected,
                    "checkerboard mismatch at ({}, {}): expected '{}', got '{}'",
                    r, c, expected, grid.visible_row(r)[c].c);
            }
        }
    }

    // ---------------------------------------------------------------
    // Horizontal resize — columns only
    // ---------------------------------------------------------------

    #[test]
    fn resize_horizontal_expand_preserves_content() {
        let mut grid = Grid::new(5, 4, 0);
        paint_checkerboard(&mut grid);
        grid.resize(10, 4); // widen: 5 -> 10 cols, same rows
        assert_eq!(grid.cols(), 10);
        assert_eq!(grid.visible_row(0).len(), 10);
        // Original 5x4 region untouched
        assert_checkerboard(&grid, 4, 5);
        // New columns should be blank
        for r in 0..4 {
            for c in 5..10 {
                assert_eq!(grid.visible_row(r)[c].c, ' ',
                    "new cell at ({}, {}) should be blank", r, c);
            }
        }
    }

    #[test]
    fn resize_horizontal_shrink_preserves_visible_content() {
        let mut grid = Grid::new(10, 4, 0);
        paint_checkerboard(&mut grid);
        grid.resize(5, 4); // narrow: 10 -> 5 cols
        assert_eq!(grid.cols(), 5);
        assert_eq!(grid.visible_row(0).len(), 5);
        // First 5 columns of pattern intact
        assert_checkerboard(&grid, 4, 5);
    }

    #[test]
    fn resize_horizontal_shrink_then_expand_loses_truncated() {
        let mut grid = Grid::new(10, 3, 0);
        paint_checkerboard(&mut grid);
        grid.resize(5, 3);   // shrink — cols 5..9 lost
        grid.resize(10, 3);  // expand back
        // First 5 cols: pattern intact
        assert_checkerboard(&grid, 3, 5);
        // Cols 5..9: blank (data was truncated, not recoverable)
        for r in 0..3 {
            for c in 5..10 {
                assert_eq!(grid.visible_row(r)[c].c, ' ',
                    "truncated cell at ({}, {}) should be blank after re-expand", r, c);
            }
        }
    }

    // ---------------------------------------------------------------
    // Vertical resize — rows only
    // ---------------------------------------------------------------

    #[test]
    fn resize_vertical_expand_preserves_content() {
        let mut grid = Grid::new(6, 3, 0);
        paint_checkerboard(&mut grid);
        grid.resize(6, 8); // taller: 3 -> 8 rows
        assert_eq!(grid.rows(), 8);
        assert_eq!(grid.visible_row_count(), 8);
        // Original 3 rows intact
        assert_checkerboard(&grid, 3, 6);
        // New rows blank
        for r in 3..8 {
            for c in 0..6 {
                assert_eq!(grid.visible_row(r)[c].c, ' ',
                    "new cell at ({}, {}) should be blank", r, c);
            }
        }
    }

    #[test]
    fn resize_vertical_shrink_preserves_visible_content() {
        let mut grid = Grid::new(6, 8, 0);
        paint_checkerboard(&mut grid);
        grid.resize(6, 3); // shorter: 8 -> 3 rows
        assert_eq!(grid.rows(), 3);
        assert_eq!(grid.visible_row_count(), 3);
        // First 3 rows of pattern intact
        assert_checkerboard(&grid, 3, 6);
    }

    #[test]
    fn resize_vertical_shrink_then_expand_loses_truncated() {
        let mut grid = Grid::new(6, 8, 0);
        paint_checkerboard(&mut grid);
        grid.resize(6, 3);  // rows 3..7 lost
        grid.resize(6, 8);  // expand back
        assert_checkerboard(&grid, 3, 6);
        for r in 3..8 {
            for c in 0..6 {
                assert_eq!(grid.visible_row(r)[c].c, ' ',
                    "truncated cell at ({}, {}) should be blank after re-expand", r, c);
            }
        }
    }

    // ---------------------------------------------------------------
    // Combined resize — both dimensions at once
    // ---------------------------------------------------------------

    #[test]
    fn resize_both_expand() {
        let mut grid = Grid::new(4, 3, 0);
        paint_checkerboard(&mut grid);
        grid.resize(8, 6); // double both
        assert_checkerboard(&grid, 3, 4);
        // New cols in old rows blank
        for r in 0..3 {
            for c in 4..8 {
                assert_eq!(grid.visible_row(r)[c].c, ' ',
                    "new col cell at ({}, {}) should be blank", r, c);
            }
        }
        // New rows entirely blank
        for r in 3..6 {
            for c in 0..8 {
                assert_eq!(grid.visible_row(r)[c].c, ' ',
                    "new row cell at ({}, {}) should be blank", r, c);
            }
        }
    }

    #[test]
    fn resize_both_shrink() {
        let mut grid = Grid::new(10, 8, 0);
        paint_checkerboard(&mut grid);
        grid.resize(5, 4); // halve both
        assert_eq!(grid.visible_row_count(), 4);
        assert_eq!(grid.visible_row(0).len(), 5);
        assert_checkerboard(&grid, 4, 5);
    }

    #[test]
    fn resize_expand_cols_shrink_rows() {
        let mut grid = Grid::new(4, 8, 0);
        paint_checkerboard(&mut grid);
        grid.resize(10, 3); // wider but shorter
        assert_eq!(grid.visible_row_count(), 3);
        assert_eq!(grid.visible_row(0).len(), 10);
        // First 3 rows x 4 cols intact
        assert_checkerboard(&grid, 3, 4);
        // New cols in surviving rows blank
        for r in 0..3 {
            for c in 4..10 {
                assert_eq!(grid.visible_row(r)[c].c, ' ',
                    "new cell at ({}, {}) should be blank", r, c);
            }
        }
    }

    #[test]
    fn resize_shrink_cols_expand_rows() {
        let mut grid = Grid::new(10, 3, 0);
        paint_checkerboard(&mut grid);
        grid.resize(4, 8); // narrower but taller
        assert_eq!(grid.visible_row_count(), 8);
        assert_eq!(grid.visible_row(0).len(), 4);
        // First 3 rows x 4 cols intact
        assert_checkerboard(&grid, 3, 4);
        // New rows blank
        for r in 3..8 {
            for c in 0..4 {
                assert_eq!(grid.visible_row(r)[c].c, ' ',
                    "new row cell at ({}, {}) should be blank", r, c);
            }
        }
    }

    // ---------------------------------------------------------------
    // Multiple sequential resizes — stress pattern preservation
    // ---------------------------------------------------------------

    #[test]
    fn resize_multiple_sequential_preserves_overlap() {
        let mut grid = Grid::new(10, 10, 0);
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
        let mut grid = Grid::new(10, 5, 0);
        grid.set_cursor_x_unclamped(8);
        grid.set_cursor_y_unclamped(2);
        grid.resize(5, 5);
        assert_eq!(grid.cursor_x(), 4, "cursor_x should clamp to cols-1");
        assert_eq!(grid.cursor_y(), 2, "cursor_y should not change");
    }

    #[test]
    fn resize_vertical_shrink_clamps_cursor() {
        let mut grid = Grid::new(10, 10, 0);
        grid.set_cursor_x_unclamped(3);
        grid.set_cursor_y_unclamped(8);
        grid.resize(10, 5);
        assert_eq!(grid.cursor_x(), 3, "cursor_x should not change");
        assert_eq!(grid.cursor_y(), 4, "cursor_y should clamp to rows-1");
    }

    #[test]
    fn resize_both_shrink_clamps_cursor() {
        let mut grid = Grid::new(20, 20, 0);
        grid.set_cursor_x_unclamped(15);
        grid.set_cursor_y_unclamped(18);
        grid.resize(5, 5);
        assert_eq!(grid.cursor_x(), 4);
        assert_eq!(grid.cursor_y(), 4);
    }

    #[test]
    fn resize_expand_preserves_cursor() {
        let mut grid = Grid::new(10, 10, 0);
        grid.set_cursor_x_unclamped(5);
        grid.set_cursor_y_unclamped(7);
        grid.resize(20, 20);
        assert_eq!(grid.cursor_x(), 5, "cursor_x should not change on expand");
        assert_eq!(grid.cursor_y(), 7, "cursor_y should not change on expand");
    }

    // ---------------------------------------------------------------
    // Resize to same dimensions — no-op semantics
    // ---------------------------------------------------------------

    #[test]
    fn resize_same_dimensions_preserves_everything() {
        let mut grid = Grid::new(8, 6, 0);
        paint_checkerboard(&mut grid);
        grid.set_cursor_x_unclamped(3);
        grid.set_cursor_y_unclamped(2);
        grid.resize(8, 6); // same
        assert_checkerboard(&grid, 6, 8);
        assert_eq!(grid.cursor_x(), 3);
        assert_eq!(grid.cursor_y(), 2);
    }

    // ---------------------------------------------------------------
    // Resize scroll region / tab stops reset
    // ---------------------------------------------------------------

    #[test]
    fn resize_resets_scroll_region() {
        let mut grid = Grid::new(80, 24, 0);
        grid.set_scroll_region(5, 18);
        grid.resize(80, 30);
        assert_eq!(grid.scroll_top(), 0);
        assert_eq!(grid.scroll_bottom(), 29, "scroll_bottom should be rows-1");
    }

    #[test]
    fn resize_resets_tab_stops() {
        let mut grid = Grid::new(80, 24, 0);
        // Manually set a custom tab stop
        grid.set_tab_stop(3);
        grid.resize(40, 24);
        assert_eq!(grid.tab_stops_len(), 40);
        // Tab stops should be default (every 8 cols)
        assert!(!grid.tab_stop_at(0));
        assert!(grid.tab_stop_at(8));
        assert!(grid.tab_stop_at(16));
        assert!(!grid.tab_stop_at(3), "custom tab stop should be gone after resize");
    }
}

#[cfg(test)]
mod tests_grid_safe_api {
    use super::*;
    use crate::screen::style::StyleId;

    #[test]
    fn set_cell_basic() {
        let mut grid = Grid::new(10, 5, 0);
        let cell = Cell::new('A', StyleId::default(), 1);
        grid.set_cell(3, 0, cell);
        assert_eq!(grid.visible_row(0)[3].c, 'A');
    }

    #[test]
    fn set_cell_wide_char_overwrites_previous() {
        let mut grid = Grid::new(10, 5, 0);
        let wide = Cell::new('\u{6F22}', StyleId::default(), 2);
        grid.set_cell(3, 0, wide);
        assert_eq!(grid.visible_row(0)[3].width, 2);
        assert_eq!(grid.visible_row(0)[4].width, 0);

        let narrow = Cell::new('B', StyleId::default(), 1);
        grid.set_cell(4, 0, narrow);
        assert_eq!(grid.visible_row(0)[3].c, ' ');
        assert_eq!(grid.visible_row(0)[4].c, 'B');
    }

    #[test]
    fn set_cell_wide_char_places_continuation() {
        let mut grid = Grid::new(10, 5, 0);
        let wide = Cell::new('\u{6F22}', StyleId::default(), 2);
        grid.set_cell(2, 0, wide);
        assert_eq!(grid.visible_row(0)[2].c, '\u{6F22}');
        assert_eq!(grid.visible_row(0)[2].width, 2);
        assert_eq!(grid.visible_row(0)[3].c, '\0');
        assert_eq!(grid.visible_row(0)[3].width, 0);
    }

    #[test]
    fn set_cell_out_of_bounds_noop() {
        let mut grid = Grid::new(10, 5, 0);
        let cell = Cell::new('X', StyleId::default(), 1);
        grid.set_cell(10, 0, cell);
        grid.set_cell(0, 5, cell);
    }

    #[test]
    fn erase_cells_basic() {
        let mut grid = Grid::new(10, 5, 0);
        let cell_a = Cell::new('A', StyleId::default(), 1);
        for i in 0..10 { grid.visible_row_mut(0)[i] = cell_a; }
        grid.erase_cells(0, 3, 7, Cell::default());
        assert_eq!(grid.visible_row(0)[2].c, 'A');
        assert_eq!(grid.visible_row(0)[3].c, ' ');
        assert_eq!(grid.visible_row(0)[6].c, ' ');
        assert_eq!(grid.visible_row(0)[7].c, 'A');
    }

    #[test]
    fn erase_cells_fixes_wide_char_at_boundary() {
        let mut grid = Grid::new(10, 5, 0);
        grid.visible_row_mut(0)[4] = Cell::new('\u{6F22}', StyleId::default(), 2);
        grid.visible_row_mut(0)[5] = Cell::new('\0', StyleId::default(), 0);
        grid.erase_cells(0, 3, 5, Cell::default());
        assert_eq!(grid.visible_row(0)[5].c, ' ');
    }

    #[test]
    fn erase_rows_basic() {
        let mut grid = Grid::new(10, 5, 0);
        let cell_a = Cell::new('A', StyleId::default(), 1);
        for i in 0..10 { grid.visible_row_mut(1)[i] = cell_a; }
        grid.erase_rows(0, 3, Cell::default());
        assert_eq!(grid.visible_row(0)[0].c, ' ');
        assert_eq!(grid.visible_row(1)[0].c, ' ');
        assert_eq!(grid.visible_row(2)[0].c, ' ');
    }

    #[test]
    fn check_invariants_passes_on_fresh_grid() {
        let grid = Grid::new(80, 24, 100);
        grid.check_invariants();
    }
}
