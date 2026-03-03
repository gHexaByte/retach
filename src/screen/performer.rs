use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;
use vte::{Params, Perform};

use super::cell::Cell;
use super::grid::{ActiveCharset, Charset, CursorShape, Grid, TerminalModes};
use super::ScreenState;
use super::style::Style;

/// VTE `Perform` implementation that translates escape sequences into grid mutations.
pub struct ScreenPerformer<'a> {
    pub grid: &'a mut Grid,
    pub state: &'a mut ScreenState,
    pub scrollback: &'a mut VecDeque<Vec<u8>>,
    pub scrollback_limit: usize,
    pub pending_scrollback: &'a mut VecDeque<Vec<u8>>,
}

impl<'a> ScreenPerformer<'a> {
    /// Blank cell with current background color (BCE — Background Color Erase)
    fn blank_cell(&self) -> Cell {
        Cell {
            c: ' ',
            combining: None,
            style: Style { bg: self.state.current_style.bg, ..Style::default() },
            width: 1,
        }
    }

    fn scroll_up(&mut self) {
        let fill = self.blank_cell();
        self.grid.scroll_up(
            self.state.in_alt_screen,
            self.scrollback,
            self.scrollback_limit,
            self.pending_scrollback,
            fill,
        );
    }

    fn scroll_down(&mut self) {
        let fill = self.blank_cell();
        self.grid.scroll_down(fill);
    }

    /// Map a character through the active DEC charset (line drawing)
    fn map_charset(&self, c: char) -> char {
        let charset = match self.grid.modes.active_charset {
            ActiveCharset::G0 => self.grid.modes.g0_charset,
            ActiveCharset::G1 => self.grid.modes.g1_charset,
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

    /// Save full cursor state: position, style, charsets, autowrap mode.
    /// Used by CSI s, ESC 7, and mode 1048h.
    fn save_cursor(&mut self) {
        self.state.saved_cursor_state = Some(super::SavedCursor {
            x: self.grid.cursor_x,
            y: self.grid.cursor_y,
            style: self.state.current_style,
            g0_charset: self.grid.modes.g0_charset,
            g1_charset: self.grid.modes.g1_charset,
            active_charset: self.grid.modes.active_charset,
            autowrap_mode: self.grid.modes.autowrap_mode,
        });
    }

    /// Restore full cursor state saved by [`save_cursor`].
    /// Used by CSI u, ESC 8, and mode 1048l.
    fn restore_cursor(&mut self) {
        if let Some(ref saved) = self.state.saved_cursor_state {
            self.grid.wrap_pending = false;
            self.grid.cursor_x = saved.x.min(self.grid.cols - 1);
            self.grid.cursor_y = saved.y.min(self.grid.rows - 1);
            self.state.current_style = saved.style;
            self.grid.modes.g0_charset = saved.g0_charset;
            self.grid.modes.g1_charset = saved.g1_charset;
            self.grid.modes.active_charset = saved.active_charset;
            self.grid.modes.autowrap_mode = saved.autowrap_mode;
        }
    }

    /// Enter alt screen: save grid/modes, clear screen, reset cursor and scroll region.
    /// If `save_cursor` is true, also save cursor state (mode 1049).
    fn enter_alt_screen(&mut self, save_cursor: bool) {
        if save_cursor {
            self.save_cursor();
        }
        self.state.saved_grid = Some(self.grid.cells.clone());
        self.state.saved_modes = Some(self.grid.modes.clone());
        self.state.in_alt_screen = true;
        let blank = Cell::default();
        for row in self.grid.cells.iter_mut() {
            for cell in row.iter_mut() { *cell = blank; }
        }
        self.grid.cursor_x = 0;
        self.grid.cursor_y = 0;
        self.grid.scroll_top = 0;
        self.grid.scroll_bottom = self.grid.rows - 1;
        self.grid.wrap_pending = false;
    }

    /// Exit alt screen: restore grid/modes, reset scroll region.
    /// If `restore_cursor` is true, also restore cursor state (mode 1049).
    fn exit_alt_screen(&mut self, do_restore_cursor: bool) {
        self.state.in_alt_screen = false;
        if let Some(saved) = self.state.saved_grid.take() {
            self.grid.cells = saved;
            self.grid.cells.resize(
                self.grid.rows as usize,
                vec![Cell::default(); self.grid.cols as usize],
            );
            for row in &mut self.grid.cells {
                row.resize(self.grid.cols as usize, Cell::default());
            }
        }
        if let Some(modes) = self.state.saved_modes.take() {
            self.grid.modes = modes;
        }
        if do_restore_cursor {
            self.restore_cursor();
        }
        self.grid.scroll_top = 0;
        self.grid.scroll_bottom = self.grid.rows - 1;
    }

    /// Erase half of a wide character: if a cell is part of a wide char pair,
    /// blank both halves to avoid rendering artifacts.
    fn fixup_wide_char(&mut self, x: usize, y: usize) {
        if y >= self.grid.rows as usize || x >= self.grid.cols as usize {
            return;
        }
        let cell_width = self.grid.cells[y][x].width;
        if cell_width == 2 {
            // This is the first half; blank the continuation cell too
            let next = x + 1;
            if next < self.grid.cols as usize {
                self.grid.cells[y][next] = self.blank_cell();
            }
        } else if cell_width == 0 && x > 0 {
            // This is the continuation half; blank the first half too
            self.grid.cells[y][x - 1] = self.blank_cell();
        }
    }

    // --- CSI command methods ---

    fn csi_cursor_up(&mut self, n: u16) {
        self.grid.wrap_pending = false;
        let top = if self.grid.cursor_y >= self.grid.scroll_top {
            self.grid.scroll_top
        } else {
            0
        };
        self.grid.cursor_y = self.grid.cursor_y.saturating_sub(n).max(top);
    }

    fn csi_cursor_down(&mut self, n: u16) {
        self.grid.wrap_pending = false;
        let bottom = if self.grid.cursor_y <= self.grid.scroll_bottom {
            self.grid.scroll_bottom
        } else {
            self.grid.rows - 1
        };
        self.grid.cursor_y = self.grid.cursor_y.saturating_add(n).min(bottom);
    }

    fn csi_cursor_forward(&mut self, n: u16) {
        self.grid.wrap_pending = false;
        self.grid.cursor_x = self.grid.cursor_x.saturating_add(n).min(self.grid.cols - 1);
    }

    fn csi_cursor_back(&mut self, n: u16) {
        self.grid.wrap_pending = false;
        self.grid.cursor_x = self.grid.cursor_x.saturating_sub(n);
    }

    fn csi_cursor_next_line(&mut self, n: u16) {
        self.grid.wrap_pending = false;
        self.grid.cursor_x = 0;
        let bottom = if self.grid.cursor_y <= self.grid.scroll_bottom {
            self.grid.scroll_bottom
        } else {
            self.grid.rows - 1
        };
        self.grid.cursor_y = self.grid.cursor_y.saturating_add(n).min(bottom);
    }

    fn csi_cursor_prev_line(&mut self, n: u16) {
        self.grid.wrap_pending = false;
        self.grid.cursor_x = 0;
        let top = if self.grid.cursor_y >= self.grid.scroll_top {
            self.grid.scroll_top
        } else {
            0
        };
        self.grid.cursor_y = self.grid.cursor_y.saturating_sub(n).max(top);
    }

    fn csi_cursor_horizontal_absolute(&mut self, col: u16) {
        self.grid.wrap_pending = false;
        self.grid.cursor_x = col.saturating_sub(1).min(self.grid.cols - 1);
    }

    fn csi_cursor_position(&mut self, row: u16, col: u16) {
        self.grid.wrap_pending = false;
        self.grid.cursor_y = row.saturating_sub(1).min(self.grid.rows - 1);
        self.grid.cursor_x = col.saturating_sub(1).min(self.grid.cols - 1);
    }

    fn csi_line_position_absolute(&mut self, row: u16) {
        self.grid.wrap_pending = false;
        self.grid.cursor_y = row.saturating_sub(1).min(self.grid.rows - 1);
    }

    fn csi_erase_display(&mut self, mode: u16) {
        let blank = self.blank_cell();
        match mode {
            0 => {
                let y = self.grid.cursor_y as usize;
                let x = self.grid.cursor_x as usize;
                self.fixup_wide_char(x, y);
                for i in x..self.grid.cols as usize { self.grid.cells[y][i] = blank; }
                for row in self.grid.cells.iter_mut().skip(y + 1) {
                    for cell in row.iter_mut() { *cell = blank; }
                }
            }
            1 => {
                let y = self.grid.cursor_y as usize;
                let x = self.grid.cursor_x as usize;
                for row in self.grid.cells.iter_mut().take(y) {
                    for cell in row.iter_mut() { *cell = blank; }
                }
                let end = x.min(self.grid.cols as usize - 1);
                self.fixup_wide_char(end, y);
                for i in 0..=end { self.grid.cells[y][i] = blank; }
            }
            2 => {
                for row in self.grid.cells.iter_mut() {
                    for cell in row.iter_mut() { *cell = blank; }
                }
            }
            3 => {
                for row in self.grid.cells.iter_mut() {
                    for cell in row.iter_mut() { *cell = blank; }
                }
                self.scrollback.clear();
                self.pending_scrollback.clear();
            }
            _ => {}
        }
    }

    fn csi_erase_line(&mut self, mode: u16) {
        let blank = self.blank_cell();
        let y = self.grid.cursor_y as usize;
        let x = self.grid.cursor_x as usize;
        match mode {
            0 => {
                self.fixup_wide_char(x, y);
                for i in x..self.grid.cols as usize { self.grid.cells[y][i] = blank; }
            }
            1 => {
                let end = x.min(self.grid.cols as usize - 1);
                self.fixup_wide_char(end, y);
                for i in 0..=end { self.grid.cells[y][i] = blank; }
            }
            2 => { for cell in self.grid.cells[y].iter_mut() { *cell = blank; } }
            _ => {}
        }
    }

    fn csi_erase_character(&mut self, n: u16) {
        let blank = self.blank_cell();
        let n = n as usize;
        let y = self.grid.cursor_y as usize;
        let x = self.grid.cursor_x as usize;
        if y < self.grid.rows as usize {
            self.fixup_wide_char(x, y);
            let end = (x + n).min(self.grid.cols as usize);
            if end < self.grid.cols as usize {
                self.fixup_wide_char(end, y);
            }
            for i in x..end {
                self.grid.cells[y][i] = blank;
            }
        }
    }

    fn csi_delete_character(&mut self, n: u16) {
        let blank = self.blank_cell();
        let n = n as usize;
        let y = self.grid.cursor_y as usize;
        let x = self.grid.cursor_x as usize;
        let cols = self.grid.cols as usize;
        if y < self.grid.rows as usize {
            self.fixup_wide_char(x, y);
            for _ in 0..n.min(cols.saturating_sub(x)) {
                self.grid.cells[y].remove(x);
                self.grid.cells[y].push(blank);
            }
            if x < cols && self.grid.cells[y][x].width == 0 {
                self.grid.cells[y][x] = blank;
            }
        }
    }

    fn csi_insert_character(&mut self, n: u16) {
        let blank = self.blank_cell();
        let n = n as usize;
        let y = self.grid.cursor_y as usize;
        let x = self.grid.cursor_x as usize;
        let cols = self.grid.cols as usize;
        if y < self.grid.rows as usize {
            self.fixup_wide_char(x, y);
            for _ in 0..n.min(cols.saturating_sub(x)) {
                self.grid.cells[y].pop();
                self.grid.cells[y].insert(x, blank);
            }
            let last = cols - 1;
            if self.grid.cells[y][last].width == 2 {
                self.grid.cells[y][last] = blank;
            }
        }
    }

    fn csi_scroll_up_n(&mut self, n: u16) {
        let n = n.min(self.grid.rows);
        for _ in 0..n { self.scroll_up(); }
    }

    fn csi_scroll_down_n(&mut self, n: u16) {
        let n = n.min(self.grid.rows);
        for _ in 0..n { self.scroll_down(); }
    }

    fn csi_delete_lines(&mut self, n: u16) {
        let blank = self.blank_cell();
        let n = n as usize;
        let y = self.grid.cursor_y as usize;
        let top = self.grid.scroll_top as usize;
        let bottom = self.grid.scroll_bottom as usize;
        if y >= top && y <= bottom {
            self.grid.cursor_x = 0;
            self.grid.wrap_pending = false;
            let n = n.min(bottom - y + 1);
            for _ in 0..n {
                if y <= bottom && bottom < self.grid.cells.len() {
                    self.grid.cells.remove(y);
                    self.grid.cells.insert(bottom, vec![blank; self.grid.cols as usize]);
                }
            }
        }
    }

    fn csi_insert_lines(&mut self, n: u16) {
        let blank = self.blank_cell();
        let n = n as usize;
        let y = self.grid.cursor_y as usize;
        let top = self.grid.scroll_top as usize;
        let bottom = self.grid.scroll_bottom as usize;
        if y >= top && y <= bottom {
            self.grid.cursor_x = 0;
            self.grid.wrap_pending = false;
            let n = n.min(bottom - y + 1);
            for _ in 0..n {
                if y <= bottom && bottom < self.grid.cells.len() {
                    self.grid.cells.remove(bottom);
                    self.grid.cells.insert(y, vec![blank; self.grid.cols as usize]);
                }
            }
        }
    }

    fn csi_set_scrolling_region(&mut self, top: u16, bottom: u16) {
        let top = top.saturating_sub(1);
        let bottom = bottom.saturating_sub(1).min(self.grid.rows - 1);
        if top < bottom {
            self.grid.scroll_top = top;
            self.grid.scroll_bottom = bottom;
        }
        self.grid.cursor_x = 0;
        self.grid.cursor_y = 0;
        self.grid.wrap_pending = false;
    }

    fn csi_set_dec_private_mode(&mut self, ps: &[Vec<u16>], enable: bool) {
        for param in ps {
            match param.first().copied() {
                Some(1) => self.grid.modes.cursor_key_mode = enable,
                Some(7) => self.grid.modes.autowrap_mode = enable,
                Some(12) => {} // Cursor blink — cosmetic, ignore
                Some(25) => self.grid.cursor_visible = enable,
                Some(1000 | 1002 | 1003) => {
                    if enable {
                        self.grid.modes.mouse_mode = param[0];
                    } else {
                        self.grid.modes.mouse_mode = 0;
                    }
                }
                Some(1005 | 1006) => {
                    if enable {
                        self.grid.modes.mouse_encoding = param[0];
                    } else {
                        self.grid.modes.mouse_encoding = 0;
                    }
                }
                Some(1004) => self.grid.modes.focus_reporting = enable,
                Some(1048) => {
                    if enable { self.save_cursor(); } else { self.restore_cursor(); }
                }
                Some(2004) => self.grid.modes.bracketed_paste = enable,
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

impl<'a> Perform for ScreenPerformer<'a> {
    fn print(&mut self, c: char) {
        // Apply charset mapping
        let c = self.map_charset(c);

        // Get display width
        let char_width = UnicodeWidthChar::width(c).unwrap_or(0) as u16;

        // Zero-width characters (combining marks, etc.) — attach to previous cell
        if char_width == 0 {
            let cx = self.grid.cursor_x as usize;
            let cy = self.grid.cursor_y as usize;
            if cy < self.grid.rows as usize {
                // Find the target cell: if wrap_pending, the cursor sits on the
                // last cell of the current line; otherwise step back one column.
                let tx = if self.grid.wrap_pending {
                    cx
                } else if cx > 0 {
                    cx - 1
                } else {
                    return; // no previous cell to attach to
                };
                if tx < self.grid.cols as usize {
                    // If the target is a continuation cell (width==0), step back
                    // one more to reach the actual wide character cell.
                    let tx = if self.grid.cells[cy][tx].width == 0 && tx > 0 {
                        tx - 1
                    } else {
                        tx
                    };
                    self.grid.cells[cy][tx].combining = Some(c);
                }
            }
            return;
        }

        // Deferred wrap: if a previous print left cursor at right margin,
        // NOW we perform the actual wrap before printing the new character.
        if self.grid.wrap_pending && self.grid.modes.autowrap_mode {
            self.grid.wrap_pending = false;
            self.grid.cursor_x = 0;
            if self.grid.cursor_y == self.grid.scroll_bottom {
                self.scroll_up();
            } else if self.grid.cursor_y < self.grid.rows - 1 {
                self.grid.cursor_y += 1;
            }
        }

        // Wide char: if it doesn't fit at end of line, fill current cell with space and wrap
        if char_width == 2 && self.grid.cursor_x >= self.grid.cols - 1 {
            if self.grid.modes.autowrap_mode {
                let x = self.grid.cursor_x as usize;
                let y = self.grid.cursor_y as usize;
                if x < self.grid.cols as usize && y < self.grid.rows as usize {
                    self.grid.cells[y][x] = self.blank_cell();
                }
                // Wrap
                self.grid.cursor_x = 0;
                if self.grid.cursor_y == self.grid.scroll_bottom {
                    self.scroll_up();
                } else if self.grid.cursor_y < self.grid.rows - 1 {
                    self.grid.cursor_y += 1;
                }
            } else {
                // No autowrap — just don't print the wide char (it doesn't fit)
                return;
            }
        }

        let x = self.grid.cursor_x as usize;
        let y = self.grid.cursor_y as usize;
        if x < self.grid.cols as usize && y < self.grid.rows as usize {
            // Fix up any wide char we're overwriting
            self.fixup_wide_char(x, y);

            self.grid.cells[y][x] = Cell {
                c,
                combining: None,
                style: self.state.current_style,
                width: char_width as u8,
            };

            if char_width == 2 {
                // Place continuation cell
                let next = x + 1;
                if next < self.grid.cols as usize {
                    // Fix up any wide char at the continuation position
                    self.fixup_wide_char(next, y);
                    self.grid.cells[y][next] = Cell {
                        c: '\0',
                        combining: None,
                        style: self.state.current_style,
                        width: 0,
                    };
                }
            }

            self.state.last_printed_char = c;
            self.grid.cursor_x += char_width;
            if self.grid.cursor_x >= self.grid.cols {
                self.grid.cursor_x = self.grid.cols - 1;
                if self.grid.modes.autowrap_mode {
                    self.grid.wrap_pending = true;
                }
                // When autowrap is off, cursor stays at right margin (no wrap_pending)
            }
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x0D => { // CR
                self.grid.cursor_x = 0;
                self.grid.wrap_pending = false;
            }
            0x0A..=0x0C => { // LF, VT, FF — all treated as line feed
                self.grid.wrap_pending = false;
                if self.grid.cursor_y == self.grid.scroll_bottom { self.scroll_up(); }
                else if self.grid.cursor_y < self.grid.rows - 1 { self.grid.cursor_y += 1; }
            }
            0x08 => { // BS
                self.grid.wrap_pending = false;
                if self.grid.cursor_x > 0 { self.grid.cursor_x -= 1; }
            }
            0x09 => { // Tab
                self.grid.wrap_pending = false;
                self.grid.cursor_x = (self.grid.cursor_x + 8) & !7;
                if self.grid.cursor_x >= self.grid.cols { self.grid.cursor_x = self.grid.cols - 1; }
            }
            0x0E => { // SO — Shift Out (activate G1)
                self.grid.modes.active_charset = ActiveCharset::G1;
            }
            0x0F => { // SI — Shift In (activate G0)
                self.grid.modes.active_charset = ActiveCharset::G0;
            }
            0x07 => {} // Bell
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
                    write_u16(&mut r, self.grid.cursor_y + 1);
                    r.push(b';');
                    write_u16(&mut r, self.grid.cursor_x + 1);
                    r.push(b'R');
                    self.state.pending_responses.push(r);
                }
            }
            'c' => {
                if intermediates.is_empty() {
                    if p(0, 0) == 0 { self.state.pending_responses.push(b"\x1b[?62;c".to_vec()); }
                } else if intermediates == b">" && p(0, 0) == 0 {
                    self.state.pending_responses.push(b"\x1b[>0;10;1c".to_vec());
                }
            }
            'q' if intermediates == b" " => self.grid.modes.cursor_shape = CursorShape::from_sgr(p(0, 0) as u8),
            'S' => self.csi_scroll_up_n(p(0, 1)),
            'T' if ps.len() <= 1 => self.csi_scroll_down_n(p(0, 1)),
            'M' => self.csi_delete_lines(p(0, 1)),
            'L' => self.csi_insert_lines(p(0, 1)),
            'r' if intermediates.is_empty() => self.csi_set_scrolling_region(p(0, 1), p(1, self.grid.rows)),
            's' if intermediates.is_empty() => self.save_cursor(),
            'u' if intermediates.is_empty() => self.restore_cursor(),
            't' => {}
            'h' | 'l' if intermediates == b"?" => self.csi_set_dec_private_mode(&ps, action == 'h'),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        match (intermediates, byte) {
            ([], b'M') => { // RI — Reverse Index (scroll down at top margin)
                if self.grid.cursor_y == self.grid.scroll_top {
                    self.scroll_down();
                } else if self.grid.cursor_y > 0 {
                    self.grid.cursor_y -= 1;
                }
            }
            ([], b'7') => self.save_cursor(),   // DECSC — Save Cursor
            ([], b'8') => self.restore_cursor(), // DECRC — Restore Cursor
            ([], b'c') => { // RIS — Full Reset
                self.grid.cursor_x = 0;
                self.grid.cursor_y = 0;
                self.grid.scroll_top = 0;
                self.grid.scroll_bottom = self.grid.rows - 1;
                self.grid.cursor_visible = true;
                self.grid.wrap_pending = false;
                self.grid.modes = TerminalModes::default();
                self.state.current_style = Style::default();
                self.state.in_alt_screen = false;
                self.state.saved_grid = None;
                self.state.saved_cursor_state = None;
                self.state.title.clear();
                self.state.last_printed_char = ' ';
                let blank = Cell::default();
                for row in self.grid.cells.iter_mut() {
                    for cell in row.iter_mut() { *cell = blank; }
                }
            }
            ([], b'=') => { // DECKPAM — Keypad Application Mode
                self.grid.modes.keypad_app_mode = true;
            }
            ([], b'>') => { // DECKPNM — Keypad Numeric Mode
                self.grid.modes.keypad_app_mode = false;
            }
            ([b'('], b'B') => { self.grid.modes.g0_charset = Charset::Ascii; }
            ([b'('], b'0') => { self.grid.modes.g0_charset = Charset::LineDrawing; }
            ([b')'], b'B') => { self.grid.modes.g1_charset = Charset::Ascii; }
            ([b')'], b'0') => { self.grid.modes.g1_charset = Charset::LineDrawing; }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
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
                    self.state.title = title.to_string();
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
        buf.push(0x07); // BEL terminator
        self.state.pending_passthrough.push(buf);
    }

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
}
