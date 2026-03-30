use unicode_width::UnicodeWidthChar;
use vte::{Params, Perform};

use super::cell::Cell;
use super::grid::{ActiveCharset, Charset, CursorShape, MouseEncoding, TerminalModes};
use super::grid_mutator::GridMutator;
use super::ScreenState;
use super::style::{Style, StyleId};

/// Maximum combining marks (diacritics, etc.) attached to a single base character.
/// Unicode NFC normalization typically produces 0-3 marks; 16 handles extreme
/// cases (e.g. Zalgo text) without unbounded allocation per cell.
const MAX_COMBINING: usize = 16;

/// VTE `Perform` implementation that translates escape sequences into grid mutations.
pub(super) struct ScreenPerformer<'a, G: GridMutator> {
    pub(super) grid: &'a mut G,
    pub(super) state: &'a mut ScreenState,
}

impl<'a, G: GridMutator> ScreenPerformer<'a, G> {
    /// Intern a style, triggering GC if the table is full and retrying.
    fn intern_with_gc(&mut self, style: Style) -> StyleId {
        let id = self.grid.style_table_mut().intern(style);
        if id.is_default() && !style.is_default() && self.grid.style_table().is_full() {
            self.grid.compact_styles(self.state.saved_grid.as_ref());
            self.grid.style_table_mut().intern(style)
        } else {
            id
        }
    }

    /// Blank cell with current background color (BCE — Background Color Erase)
    fn blank_cell(&mut self) -> Cell {
        let style = Style { bg: self.state.current_style.bg, ..Style::default() };
        let id = self.intern_with_gc(style);
        Cell::new(' ', id, 1)
    }

    fn scroll_up(&mut self) {
        let fill = self.blank_cell();
        self.grid.scroll_up(self.state.in_alt_screen, fill);
    }

    fn scroll_down(&mut self) {
        let fill = self.blank_cell();
        self.grid.scroll_down(fill);
    }

    /// Map a character through the active DEC charset (line drawing)
    fn map_charset(&self, c: char) -> char {
        let charset = match self.grid.modes().active_charset {
            ActiveCharset::G0 => self.grid.modes().g0_charset,
            ActiveCharset::G1 => self.grid.modes().g1_charset,
        };
        match charset {
            Charset::LineDrawing => match c {
                'j' => '┘', 'k' => '┐', 'l' => '┌', 'm' => '└', 'n' => '┼',
                'q' => '─', 't' => '├', 'u' => '┤', 'v' => '┴', 'w' => '┬',
                'x' => '│', 'a' => '▒', '`' => '◆',
                _ => c,
            },
            Charset::Ascii => c,
        }
    }

    /// Save full cursor state: position, style, charsets, autowrap mode, wrap_pending.
    /// Used by CSI s, ESC 7, and mode 1048h.
    fn save_cursor(&mut self) {
        self.state.saved_cursor_state = Some(super::SavedCursor {
            x: self.grid.cursor_x(),
            y: self.grid.cursor_y(),
            style: self.state.current_style,
            g0_charset: self.grid.modes().g0_charset,
            g1_charset: self.grid.modes().g1_charset,
            active_charset: self.grid.modes().active_charset,
            autowrap_mode: self.grid.modes().autowrap_mode,
            origin_mode: self.grid.modes().origin_mode,
            wrap_pending: self.grid.wrap_pending(),
        });
    }

    /// Restore full cursor state saved by [`save_cursor`].
    /// Used by CSI u, ESC 8, and mode 1048l.
    fn restore_cursor(&mut self) {
        if let Some(ref saved) = self.state.saved_cursor_state {
            let x = saved.x.min(self.grid.cols() - 1);
            let y = saved.y.min(self.grid.rows() - 1);
            let style = saved.style;
            let g0 = saved.g0_charset;
            let g1 = saved.g1_charset;
            let active = saved.active_charset;
            let autowrap = saved.autowrap_mode;
            let origin = saved.origin_mode;
            let wrap_pending = saved.wrap_pending;
            self.grid.set_wrap_pending(wrap_pending);
            self.grid.set_cursor_x_unclamped(x);
            self.grid.set_cursor_y_unclamped(y);
            self.state.current_style = style;
            self.grid.modes_mut().g0_charset = g0;
            self.grid.modes_mut().g1_charset = g1;
            self.grid.modes_mut().active_charset = active;
            self.grid.modes_mut().autowrap_mode = autowrap;
            self.grid.modes_mut().origin_mode = origin;
        }
    }

    /// Enter alt screen: save grid/modes/scroll-region, clear screen, reset cursor and scroll region.
    /// If `save_cursor` is true, also save cursor state (mode 1049).
    /// Ignored if already in alt screen (prevents overwriting the saved main screen).
    fn enter_alt_screen(&mut self, save_cursor: bool) {
        if self.state.in_alt_screen {
            return;
        }
        if save_cursor {
            self.save_cursor();
        }
        use super::grid::SavedGrid;
        // Save main screen scroll region before overwriting it.
        self.state.saved_scroll_region = Some((self.grid.scroll_top(), self.grid.scroll_bottom()));
        // Save visible rows; scrollback stays in the active grid
        let saved_visible = self.grid.drain_visible();
        let scrollback_limit = self.grid.scrollback_limit();
        self.state.saved_grid = Some(SavedGrid::new(saved_visible, scrollback_limit));
        // Create blank visible rows for alt screen
        self.grid.fill_visible_blank();
        self.grid.set_scrollback_limit(0);
        self.state.saved_modes = Some(self.grid.modes().clone());
        self.state.in_alt_screen = true;
        self.grid.set_cursor_x_unclamped(0);
        self.grid.set_cursor_y_unclamped(0);
        self.grid.reset_scroll_region();
        self.grid.set_wrap_pending(false);
    }

    /// Exit alt screen: restore grid/modes, reset scroll region.
    /// If `restore_cursor` is true, also restore cursor state (mode 1049).
    /// Ignored if not currently in alt screen.
    fn exit_alt_screen(&mut self, do_restore_cursor: bool) {
        if !self.state.in_alt_screen {
            return;
        }
        self.state.in_alt_screen = false;
        if let Some(saved) = self.state.saved_grid.take() {
            // Remove alt screen visible rows, restore saved visible rows
            let (visible_cells, scrollback_limit) = saved.into_parts();
            self.grid.replace_visible(visible_cells);
            self.grid.set_scrollback_limit(scrollback_limit);
            // Adjust visible rows for current dimensions (may have resized during alt screen)
            self.grid.adjust_visible_to_fit();
            let cols_usize = self.grid.cols() as usize;
            for y in 0..self.grid.visible_row_count() {
                self.grid.visible_row_mut(y).fix_wide_char_orphan_at_boundary(cols_usize);
                self.grid.visible_row_mut(y).resize(cols_usize, Cell::default());
            }
        }
        if let Some(modes) = self.state.saved_modes.take() {
            self.grid.set_modes(modes);
        }
        if do_restore_cursor {
            self.restore_cursor();
        }
        // Restore the main screen scroll region saved on alt screen entry.
        // Fallback to full-screen reset if somehow missing (defensive).
        if let Some(sr) = self.state.saved_scroll_region.take() {
            let bottom_max = self.grid.rows().saturating_sub(1);
            let top = sr.0.min(bottom_max);
            let bottom = sr.1.min(bottom_max).max(top);
            self.grid.set_scroll_region(top, bottom);
        } else {
            self.grid.reset_scroll_region();
        }

        // Compact styles after alt screen — apps like vim/htop create
        // many unique styles that become dead when returning to main screen.
        self.grid.compact_styles(None);
    }

    // --- CSI command methods ---

    fn csi_cursor_up(&mut self, n: u16) {
        self.grid.set_wrap_pending(false);
        let top = if self.grid.cursor_y() >= self.grid.scroll_top() {
            self.grid.scroll_top()
        } else {
            0
        };
        self.grid.set_cursor_y_unclamped(self.grid.cursor_y().saturating_sub(n).max(top));
    }

    fn csi_cursor_down(&mut self, n: u16) {
        self.grid.set_wrap_pending(false);
        let bottom = if self.grid.cursor_y() <= self.grid.scroll_bottom() {
            self.grid.scroll_bottom()
        } else {
            self.grid.rows() - 1
        };
        self.grid.set_cursor_y_unclamped(self.grid.cursor_y().saturating_add(n).min(bottom));
    }

    fn csi_cursor_forward(&mut self, n: u16) {
        self.grid.set_wrap_pending(false);
        self.grid.set_cursor_x_unclamped(self.grid.cursor_x().saturating_add(n).min(self.grid.cols() - 1));
    }

    fn csi_cursor_back(&mut self, n: u16) {
        self.grid.set_wrap_pending(false);
        self.grid.set_cursor_x_unclamped(self.grid.cursor_x().saturating_sub(n));
    }

    fn csi_cursor_next_line(&mut self, n: u16) {
        self.grid.set_wrap_pending(false);
        self.grid.set_cursor_x_unclamped(0);
        let bottom = if self.grid.cursor_y() <= self.grid.scroll_bottom() {
            self.grid.scroll_bottom()
        } else {
            self.grid.rows() - 1
        };
        self.grid.set_cursor_y_unclamped(self.grid.cursor_y().saturating_add(n).min(bottom));
    }

    fn csi_cursor_prev_line(&mut self, n: u16) {
        self.grid.set_wrap_pending(false);
        self.grid.set_cursor_x_unclamped(0);
        let top = if self.grid.cursor_y() >= self.grid.scroll_top() {
            self.grid.scroll_top()
        } else {
            0
        };
        self.grid.set_cursor_y_unclamped(self.grid.cursor_y().saturating_sub(n).max(top));
    }

    fn csi_cursor_horizontal_absolute(&mut self, col: u16) {
        self.grid.set_wrap_pending(false);
        self.grid.set_cursor_x_unclamped(col.saturating_sub(1).min(self.grid.cols() - 1));
    }

    fn csi_cursor_position(&mut self, row: u16, col: u16) {
        self.grid.set_wrap_pending(false);
        if self.grid.modes().origin_mode {
            let top = self.grid.scroll_top();
            let bottom = self.grid.scroll_bottom();
            self.grid.set_cursor_y_unclamped(
                top.saturating_add(row.saturating_sub(1)).min(bottom)
            );
        } else {
            self.grid.set_cursor_y_unclamped(
                row.saturating_sub(1).min(self.grid.rows() - 1)
            );
        }
        self.grid.set_cursor_x_unclamped(
            col.saturating_sub(1).min(self.grid.cols() - 1)
        );
    }

    fn csi_line_position_absolute(&mut self, row: u16) {
        self.grid.set_wrap_pending(false);
        if self.grid.modes().origin_mode {
            let top = self.grid.scroll_top();
            let bottom = self.grid.scroll_bottom();
            self.grid.set_cursor_y_unclamped(
                top.saturating_add(row.saturating_sub(1)).min(bottom)
            );
        } else {
            self.grid.set_cursor_y_unclamped(
                row.saturating_sub(1).min(self.grid.rows() - 1)
            );
        }
    }

    fn csi_erase_display(&mut self, mode: u16) {
        let blank = self.blank_cell();
        match mode {
            0 => {
                let y = self.grid.cursor_y() as usize;
                let x = self.grid.cursor_x() as usize;
                let cols = self.grid.cols() as usize;
                self.grid.erase_cells(y, x, cols, blank);
                self.grid.erase_rows(y + 1, self.grid.rows() as usize, blank);
            }
            1 => {
                let y = self.grid.cursor_y() as usize;
                let x = self.grid.cursor_x() as usize;
                self.grid.erase_rows(0, y, blank);
                let end = (x + 1).min(self.grid.cols() as usize);
                self.grid.erase_cells(y, 0, end, blank);
            }
            2 => {
                self.grid.erase_rows(0, self.grid.rows() as usize, blank);
            }
            3 => {
                // ED 3 = Erase Saved Lines (xterm): scrollback only, visible screen untouched
                self.grid.clear_scrollback();
                // Forward to outer terminal so it clears its native scrollback too
                self.state.push_passthrough(b"\x1b[3J".to_vec());
            }
            _ => {}
        }
    }

    fn csi_erase_line(&mut self, mode: u16) {
        let blank = self.blank_cell();
        let y = self.grid.cursor_y() as usize;
        let x = self.grid.cursor_x() as usize;
        match mode {
            0 => {
                let cols = self.grid.cols() as usize;
                self.grid.erase_cells(y, x, cols, blank);
            }
            1 => {
                let end = (x + 1).min(self.grid.cols() as usize);
                self.grid.erase_cells(y, 0, end, blank);
            }
            2 => {
                self.grid.erase_cells(y, 0, self.grid.cols() as usize, blank);
            }
            _ => {}
        }
    }

    fn csi_erase_character(&mut self, n: u16) {
        let blank = self.blank_cell();
        let n = n as usize;
        let y = self.grid.cursor_y() as usize;
        let x = self.grid.cursor_x() as usize;
        if y < self.grid.rows() as usize {
            let end = (x + n).min(self.grid.cols() as usize);
            self.grid.erase_cells(y, x, end, blank);
        }
    }

    fn csi_delete_character(&mut self, n: u16) {
        let blank = self.blank_cell();
        let n = n as usize;
        let y = self.grid.cursor_y() as usize;
        let x = self.grid.cursor_x() as usize;
        let cols = self.grid.cols() as usize;
        if y < self.grid.rows() as usize {
            self.grid.fixup_wide_char_at(x, y);
            let delete_count = n.min(cols.saturating_sub(x));
            for _ in 0..delete_count {
                self.grid.visible_row_mut(y).remove(x);
                self.grid.visible_row_mut(y).push(blank);
            }
            // Fix orphaned continuation cell(s) that shifted into position x
            if x < cols && self.grid.visible_row(y)[x].width == 0 {
                self.grid.visible_row_mut(y)[x] = blank;
                // If the continuation was part of a wide char whose first half
                // is also now at x-1 as an orphan, fixup_wide_char handles it
                // but we already blanked position x, so the base at x-1 with
                // width==2 now has no continuation — blank it too.
                if x > 0 && self.grid.visible_row(y)[x - 1].width == 2 {
                    self.grid.visible_row_mut(y)[x - 1] = blank;
                }
            }
        }
    }

    fn csi_insert_character(&mut self, n: u16) {
        let blank = self.blank_cell();
        let n = n as usize;
        let y = self.grid.cursor_y() as usize;
        let x = self.grid.cursor_x() as usize;
        let cols = self.grid.cols() as usize;
        if y < self.grid.rows() as usize {
            self.grid.fixup_wide_char_at(x, y);
            for _ in 0..n.min(cols.saturating_sub(x)) {
                self.grid.visible_row_mut(y).pop();
                self.grid.visible_row_mut(y).insert(x, blank);
            }
            let last = cols - 1;
            if self.grid.visible_row(y)[last].width == 2 {
                self.grid.visible_row_mut(y)[last] = blank;
            } else if self.grid.visible_row(y)[last].width == 0 {
                // Orphaned continuation cell: its base was pushed off-screen
                self.grid.visible_row_mut(y)[last] = blank;
                if last > 0 && self.grid.visible_row(y)[last - 1].width == 2 {
                    self.grid.visible_row_mut(y)[last - 1] = blank;
                }
            }
        }
    }

    fn csi_scroll_up_n(&mut self, n: u16) {
        let n = n.min(self.grid.rows());
        for _ in 0..n { self.scroll_up(); }
    }

    fn csi_scroll_down_n(&mut self, n: u16) {
        let n = n.min(self.grid.rows());
        for _ in 0..n { self.scroll_down(); }
    }

    fn csi_delete_lines(&mut self, n: u16) {
        let blank = self.blank_cell();
        let n = n as usize;
        let y = self.grid.cursor_y() as usize;
        let top = self.grid.scroll_top() as usize;
        let bottom = self.grid.scroll_bottom() as usize;
        if y >= top && y <= bottom {
            self.grid.set_wrap_pending(false);
            let n = n.min(bottom - y + 1);
            for _ in 0..n {
                if y <= bottom && bottom < self.grid.visible_row_count() {
                    self.grid.remove_visible_row(y);
                    self.grid.insert_visible_row(bottom, self.grid.new_blank_row(blank));
                }
            }
        }
    }

    fn csi_insert_lines(&mut self, n: u16) {
        let blank = self.blank_cell();
        let n = n as usize;
        let y = self.grid.cursor_y() as usize;
        let top = self.grid.scroll_top() as usize;
        let bottom = self.grid.scroll_bottom() as usize;
        if y >= top && y <= bottom {
            self.grid.set_wrap_pending(false);
            let n = n.min(bottom - y + 1);
            for _ in 0..n {
                if y <= bottom && bottom < self.grid.visible_row_count() {
                    self.grid.remove_visible_row(bottom);
                    self.grid.insert_visible_row(y, self.grid.new_blank_row(blank));
                }
            }
        }
    }

    fn csi_set_scrolling_region(&mut self, top: u16, bottom: u16) {
        let top = top.saturating_sub(1);
        let bottom = bottom.saturating_sub(1).min(self.grid.rows() - 1);
        if top <= bottom {
            self.grid.set_scroll_region(top, bottom);
            self.grid.set_cursor_x_unclamped(0);
            if self.grid.modes().origin_mode {
                self.grid.set_cursor_y_unclamped(top);
            } else {
                self.grid.set_cursor_y_unclamped(0);
            }
            self.grid.set_wrap_pending(false);
        }
    }

    /// Attach a zero-width combining mark to the previous cell.
    fn handle_combining_mark(&mut self, c: char) {
        let cx = self.grid.cursor_x() as usize;
        let cy = self.grid.cursor_y() as usize;
        if cy >= self.grid.rows() as usize {
            return;
        }
        let tx = if self.grid.wrap_pending() {
            cx
        } else if cx > 0 {
            cx - 1
        } else {
            return;
        };
        if tx >= self.grid.cols() as usize {
            return;
        }
        let tx = if self.grid.visible_row(cy)[tx].width == 0 && tx > 0 {
            tx - 1
        } else {
            tx
        };
        if self.grid.visible_row(cy).combining_len(tx as u16) < MAX_COMBINING {
            self.grid.visible_row_mut(cy).push_combining(tx as u16, c);
        }
    }

    /// Execute a deferred line wrap: advance to column 0 of the next row,
    /// scrolling if at the bottom of the scroll region.
    fn perform_deferred_wrap(&mut self) {
        self.grid.set_wrap_pending(false);
        self.grid.set_cursor_x_unclamped(0);
        if self.grid.cursor_y() == self.grid.scroll_bottom() {
            self.scroll_up();
        } else if self.grid.cursor_y() < self.grid.rows() - 1 {
            self.grid.set_cursor_y_unclamped(self.grid.cursor_y() + 1);
        }
    }

    fn csi_set_dec_private_mode(&mut self, ps: &[Vec<u16>], enable: bool) {
        for param in ps {
            match param.first().copied() {
                Some(1) => self.grid.modes_mut().cursor_key_mode = enable,
                Some(6) => {
                    self.grid.modes_mut().origin_mode = enable;
                    if enable {
                        self.grid.set_cursor_x_unclamped(0);
                        self.grid.set_cursor_y_unclamped(self.grid.scroll_top());
                        self.grid.set_wrap_pending(false);
                    }
                }
                Some(7) => self.grid.modes_mut().autowrap_mode = enable,
                Some(12) => {} // Cursor blink — cosmetic, ignore
                Some(25) => self.grid.set_cursor_visible(enable),
                Some(1000 | 1002 | 1003) => {
                    self.grid.modes_mut().mouse_modes.set(param[0], enable);
                }
                Some(1005 | 1006) => {
                    self.grid.modes_mut().mouse_encoding = if enable {
                        MouseEncoding::from_param(param[0]).unwrap_or(MouseEncoding::X10)
                    } else {
                        MouseEncoding::X10
                    };
                }
                Some(1004) => self.grid.modes_mut().focus_reporting = enable,
                Some(1048) => {
                    if enable { self.save_cursor(); } else { self.restore_cursor(); }
                }
                Some(2004) => self.grid.modes_mut().bracketed_paste = enable,
                Some(1049) => {
                    if enable { self.enter_alt_screen(true); }
                    else { self.exit_alt_screen(true); }
                }
                Some(1047 | 47) => {
                    if enable { self.enter_alt_screen(false); }
                    else { self.exit_alt_screen(false); }
                }
                _ => {}
            }
        }
    }
}

impl<'a, G: GridMutator> Perform for ScreenPerformer<'a, G> {
    fn print(&mut self, c: char) {
        self.state.last_printed_char = c;  // Save pre-map char for REP
        let c = self.map_charset(c);
        let char_width = UnicodeWidthChar::width(c).unwrap_or(0) as u16;

        if char_width == 0 {
            self.handle_combining_mark(c);
            return;
        }

        if self.grid.wrap_pending() && self.grid.modes().autowrap_mode {
            self.perform_deferred_wrap();
        }

        // Wide char at end of line: fill margin cell with space and wrap
        if char_width == 2 && self.grid.cursor_x() >= self.grid.cols() - 1 {
            if self.grid.modes().autowrap_mode {
                if self.grid.cols() < 2 {
                    return;
                }
                let x = self.grid.cursor_x() as usize;
                let y = self.grid.cursor_y() as usize;
                if x < self.grid.cols() as usize && y < self.grid.rows() as usize {
                    let blank = self.blank_cell();
                    self.grid.visible_row_mut(y)[x] = blank;
                }
                self.perform_deferred_wrap();
            } else {
                return;
            }
        }

        let x = self.grid.cursor_x() as usize;
        let y = self.grid.cursor_y() as usize;
        if x < self.grid.cols() as usize && y < self.grid.rows() as usize {
            let sid = self.intern_with_gc(self.state.current_style);
            self.grid.set_cell(x, y, Cell::new(c, sid, char_width as u8));

            let new_x = self.grid.cursor_x() + char_width;
            if new_x >= self.grid.cols() {
                self.grid.set_cursor_x_unclamped(self.grid.cols() - 1);
                if self.grid.modes().autowrap_mode {
                    self.grid.set_wrap_pending(true);
                }
            } else {
                self.grid.set_cursor_x_unclamped(new_x);
            }
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x0D => { // CR
                self.grid.set_cursor_x_unclamped(0);
                self.grid.set_wrap_pending(false);
            }
            0x0A..=0x0C => { // LF, VT, FF — all treated as line feed
                self.grid.set_wrap_pending(false);
                if self.grid.cursor_y() == self.grid.scroll_bottom() { self.scroll_up(); }
                else if self.grid.cursor_y() < self.grid.rows() - 1 { self.grid.set_cursor_y_unclamped(self.grid.cursor_y() + 1); }
            }
            0x08 => { // BS
                self.grid.set_wrap_pending(false);
                if self.grid.cursor_x() > 0 { self.grid.set_cursor_x_unclamped(self.grid.cursor_x() - 1); }
            }
            0x09 => { // Tab
                self.grid.set_wrap_pending(false);
                let cur_x = self.grid.cursor_x();
                let next = self.grid.next_tab_stop(cur_x);
                self.grid.set_cursor_x_unclamped(next);
            }
            0x0E => { // SO — Shift Out (activate G1)
                self.grid.modes_mut().active_charset = ActiveCharset::G1;
            }
            0x0F => { // SI — Shift In (activate G0)
                self.grid.modes_mut().active_charset = ActiveCharset::G0;
            }
            0x07 => { // Bell — forward to outer terminal
                self.state.push_passthrough(vec![0x07]);
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        let ps: Vec<Vec<u16>> = params.iter().map(|p| p.to_vec()).collect();
        let p = |i: usize, default: u16| -> u16 {
            ps.get(i).and_then(|v| v.first().copied()).filter(|&v| v != 0).unwrap_or(default)
        };

        match action {
            'A' => self.csi_cursor_up(p(0, 1)),
            'B' => self.csi_cursor_down(p(0, 1)),
            'C' => self.csi_cursor_forward(p(0, 1)),
            'D' => self.csi_cursor_back(p(0, 1)),
            'E' => self.csi_cursor_next_line(p(0, 1)),
            'F' => self.csi_cursor_prev_line(p(0, 1)),
            'G' => self.csi_cursor_horizontal_absolute(p(0, 1)),
            'H' | 'f' => self.csi_cursor_position(p(0, 1), p(1, 1)),
            'd' => self.csi_line_position_absolute(p(0, 1)),
            'J' => self.csi_erase_display(p(0, 0)),
            'K' => self.csi_erase_line(p(0, 0)),
            'X' => self.csi_erase_character(p(0, 1)),
            'P' => self.csi_delete_character(p(0, 1)),
            '@' => self.csi_insert_character(p(0, 1)),
            'b' => { let c = self.state.last_printed_char; for _ in 0..p(0, 1) { self.print(c); } }
            'm' => self.state.current_style.apply_sgr(&ps),
            'n' if intermediates.is_empty() => {
                if p(0, 0) == 6 {
                    use super::style::write_u16;
                    let mut r = Vec::with_capacity(16);
                    r.extend_from_slice(b"\x1b[");
                    let row = if self.grid.modes().origin_mode {
                        self.grid.cursor_y().saturating_sub(self.grid.scroll_top()) + 1
                    } else {
                        self.grid.cursor_y() + 1
                    };
                    write_u16(&mut r, row);
                    r.push(b';');
                    write_u16(&mut r, self.grid.cursor_x() + 1);
                    r.push(b'R');
                    self.state.push_response(r);
                }
            }
            'c' => {
                if intermediates.is_empty() {
                    if p(0, 0) == 0 { self.state.push_response(b"\x1b[?62;c".to_vec()); }
                } else if intermediates == b">" && p(0, 0) == 0 {
                    self.state.push_response(b"\x1b[>0;10;1c".to_vec());
                }
            }
            'q' if intermediates == b" " => self.grid.modes_mut().cursor_shape = CursorShape::from_param(p(0, 0) as u8),
            'S' => self.csi_scroll_up_n(p(0, 1)),
            'T' if ps.len() <= 1 => self.csi_scroll_down_n(p(0, 1)),
            'M' => self.csi_delete_lines(p(0, 1)),
            'L' => self.csi_insert_lines(p(0, 1)),
            'r' if intermediates.is_empty() => self.csi_set_scrolling_region(p(0, 1), p(1, self.grid.rows())),
            's' if intermediates.is_empty() => self.save_cursor(),
            'u' if intermediates.is_empty() => self.restore_cursor(),
            'g' if intermediates.is_empty() => { // TBC — Tab Clear
                match p(0, 0) {
                    0 => { // Clear tab stop at cursor
                        self.grid.clear_tab_stop(self.grid.cursor_x());
                    }
                    3 => { // Clear all tab stops
                        self.grid.clear_all_tab_stops();
                    }
                    _ => {}
                }
            }
            't' => {
                let cmd = p(0, 0);
                let scope = p(1, 0);
                match cmd {
                    22 if scope == 0 || scope == 2 => {
                        if self.state.title_stack.len() < 16 {
                            let mut t = self.state.title.clone();
                            t.truncate(4096);
                            self.state.title_stack.push(t);
                        }
                    }
                    23 if scope == 0 || scope == 2 => {
                        if let Some(title) = self.state.title_stack.pop() {
                            self.state.title = title;
                        }
                    }
                    _ => {}
                }
            }
            'h' | 'l' if intermediates == b"?" => self.csi_set_dec_private_mode(&ps, action == 'h'),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        match (intermediates, byte) {
            ([], b'H') => { // HTS — Horizontal Tab Set
                self.grid.set_tab_stop(self.grid.cursor_x());
            }
            ([], b'D') => { // IND — Index (scroll up at bottom margin)
                self.grid.set_wrap_pending(false);
                if self.grid.cursor_y() == self.grid.scroll_bottom() {
                    self.scroll_up();
                } else if self.grid.cursor_y() < self.grid.rows() - 1 {
                    self.grid.set_cursor_y_unclamped(self.grid.cursor_y() + 1);
                }
            }
            ([], b'E') => { // NEL — Next Line (CR + LF)
                self.grid.set_cursor_x_unclamped(0);
                self.grid.set_wrap_pending(false);
                if self.grid.cursor_y() == self.grid.scroll_bottom() {
                    self.scroll_up();
                } else if self.grid.cursor_y() < self.grid.rows() - 1 {
                    self.grid.set_cursor_y_unclamped(self.grid.cursor_y() + 1);
                }
            }
            ([], b'M') => { // RI — Reverse Index (scroll down at top margin)
                if self.grid.cursor_y() == self.grid.scroll_top() {
                    self.scroll_down();
                } else if self.grid.cursor_y() > 0 {
                    self.grid.set_cursor_y_unclamped(self.grid.cursor_y() - 1);
                }
            }
            ([], b'7') => self.save_cursor(),   // DECSC — Save Cursor
            ([], b'8') => self.restore_cursor(), // DECRC — Restore Cursor
            ([], b'c') => { // RIS — Full Reset
                self.grid.set_cursor_x_unclamped(0);
                self.grid.set_cursor_y_unclamped(0);
                self.grid.reset_scroll_region();
                self.grid.set_cursor_visible(true);
                self.grid.set_wrap_pending(false);
                self.grid.set_modes(TerminalModes::default());
                self.state.current_style = Style::default();
                // Restore scrollback_limit before discarding saved_grid —
                // alt screen entry sets it to 0, and the original is saved
                // inside SavedGrid. Without this, RIS during alt screen
                // permanently kills scrollback capture.
                if let Some(ref saved) = self.state.saved_grid {
                    self.grid.set_scrollback_limit(saved.scrollback_limit());
                }
                self.state.in_alt_screen = false;
                self.state.saved_grid = None;
                self.state.saved_modes = None;
                self.state.saved_cursor_state = None;
                self.state.saved_scroll_region = None;
                self.state.title.clear();
                self.state.title_stack.clear();
                self.state.last_printed_char = ' ';
                self.grid.reset_tab_stops();
                self.grid.style_table_mut().reset();
                self.grid.erase_rows(0, self.grid.rows() as usize, Cell::default());
                // Clear scrollback (real terminals do this on RIS)
                self.grid.clear_scrollback();
                // Forward \e[3J to clear outer terminal's native scrollback.
                // We don't forward \ec itself — that would reset the outer
                // terminal's state and interfere with retach's rendering.
                self.state.push_passthrough(b"\x1b[3J".to_vec());
            }
            ([], b'=') => { // DECKPAM — Keypad Application Mode
                self.grid.modes_mut().keypad_app_mode = true;
            }
            ([], b'>') => { // DECKPNM — Keypad Numeric Mode
                self.grid.modes_mut().keypad_app_mode = false;
            }
            ([b'('], b'B') => { self.grid.modes_mut().g0_charset = Charset::Ascii; }
            ([b'('], b'0') => { self.grid.modes_mut().g0_charset = Charset::LineDrawing; }
            ([b')'], b'B') => { self.grid.modes_mut().g1_charset = Charset::Ascii; }
            ([b')'], b'0') => { self.grid.modes_mut().g1_charset = Charset::LineDrawing; }
            ([b'#'], b'8') => { // DECALN — Screen Alignment Pattern
                self.grid.reset_scroll_region();
                self.grid.set_cursor_x_unclamped(0);
                self.grid.set_cursor_y_unclamped(0);
                self.grid.set_wrap_pending(false);
                let sid = self.intern_with_gc(Style::default());
                let e_cell = Cell::new('E', sid, 1);
                for y in 0..self.grid.rows() as usize {
                    for x in 0..self.grid.cols() as usize {
                        self.grid.set_cell(x, y, e_cell);
                    }
                }
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        if params.is_empty() {
            return;
        }
        // params[0] is the OSC number as bytes
        let osc_num = std::str::from_utf8(params[0])
            .ok()
            .and_then(|s| s.parse::<u16>().ok());

        // Set window title (OSC 0 / OSC 2) — handled locally
        if let Some(0 | 2) = osc_num {
            if params.len() >= 2 {
                if let Ok(title) = std::str::from_utf8(params[1]) {
                    if title.len() <= 4096 {
                        self.state.title = title.to_string();
                    } else {
                        self.state.title = title[..4096].to_string();
                    }
                }
            }
            return;
        }

        // All other OSC sequences: reconstruct and forward to the outer terminal.
        // This covers notifications (777, 9), clipboard (52), hyperlinks (8), etc.
        let mut buf = Vec::new();
        buf.extend_from_slice(b"\x1b]");
        for (i, param) in params.iter().enumerate() {
            if i > 0 {
                buf.push(b';');
            }
            buf.extend_from_slice(param);
        }
        if bell_terminated {
            buf.push(0x07); // BEL terminator
        } else {
            buf.extend_from_slice(b"\x1b\\"); // ST terminator
        }
        // Text notifications go to a separate consumable queue so they are
        // delivered exactly once (by the relay or on reconnect).
        // Other OSC sequences (clipboard, hyperlinks, etc.) go to passthrough.
        if matches!(osc_num, Some(9 | 777 | 99)) {
            self.state.push_notification(buf);
        } else {
            self.state.push_passthrough(buf);
        }
    }

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, action: char) {
        tracing::debug!(action = %action, "dropping DCS sequence (not supported)");
    }
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
}
