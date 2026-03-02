use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;
use vte::{Params, Perform};

use super::cell::Cell;
use super::grid::{Grid, TerminalModes};
use super::ScreenState;
use super::style::Style;

/// VTE `Perform` implementation that translates escape sequences into grid mutations.
pub struct ScreenPerformer<'a> {
    pub grid: &'a mut Grid,
    pub state: &'a mut ScreenState,
    pub scrollback: &'a mut VecDeque<Vec<u8>>,
    pub scrollback_limit: usize,
    pub pending_scrollback: &'a mut Vec<Vec<u8>>,
}

impl<'a> ScreenPerformer<'a> {
    /// Blank cell with current background color (BCE — Background Color Erase)
    fn blank_cell(&self) -> Cell {
        Cell {
            c: ' ',
            style: Style { bg: self.state.current_style.bg.clone(), ..Style::default() },
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
        let charset = if self.grid.modes.active_charset == 0 {
            self.grid.modes.g0_charset
        } else {
            self.grid.modes.g1_charset
        };
        if charset == 1 {
            // DEC Special Graphics (line drawing)
            match c {
                'j' => '┘', 'k' => '┐', 'l' => '┌', 'm' => '└', 'n' => '┼',
                'q' => '─', 't' => '├', 'u' => '┤', 'v' => '┴', 'w' => '┬',
                'x' => '│', 'a' => '▒', '`' => '◆',
                _ => c,
            }
        } else {
            c
        }
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
}

impl<'a> Perform for ScreenPerformer<'a> {
    fn print(&mut self, c: char) {
        // Apply charset mapping
        let c = self.map_charset(c);

        // Get display width
        let char_width = UnicodeWidthChar::width(c).unwrap_or(0) as u16;

        // Zero-width characters (combining marks, etc.) — attach to previous cell
        if char_width == 0 {
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
                style: self.state.current_style.clone(),
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
                        style: self.state.current_style.clone(),
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
                self.grid.modes.active_charset = 1;
            }
            0x0F => { // SI — Shift In (activate G0)
                self.grid.modes.active_charset = 0;
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
            'A' => { // CUU — Cursor Up (clamped to scroll region if inside it)
                self.grid.wrap_pending = false;
                let top = if self.grid.cursor_y >= self.grid.scroll_top {
                    self.grid.scroll_top
                } else {
                    0
                };
                self.grid.cursor_y = self.grid.cursor_y.saturating_sub(p(0, 1)).max(top);
            }
            'B' => { // CUD — Cursor Down (clamped to scroll region if inside it)
                self.grid.wrap_pending = false;
                let bottom = if self.grid.cursor_y <= self.grid.scroll_bottom {
                    self.grid.scroll_bottom
                } else {
                    self.grid.rows - 1
                };
                self.grid.cursor_y = (self.grid.cursor_y + p(0, 1)).min(bottom);
            }
            'C' => { self.grid.wrap_pending = false; self.grid.cursor_x = (self.grid.cursor_x + p(0, 1)).min(self.grid.cols - 1); }
            'D' => { self.grid.wrap_pending = false; self.grid.cursor_x = self.grid.cursor_x.saturating_sub(p(0, 1)); }
            'E' => { // CNL — Cursor Next Line
                self.grid.wrap_pending = false;
                self.grid.cursor_x = 0;
                self.grid.cursor_y = (self.grid.cursor_y + p(0, 1)).min(self.grid.rows - 1);
            }
            'F' => { // CPL — Cursor Previous Line
                self.grid.wrap_pending = false;
                self.grid.cursor_x = 0;
                self.grid.cursor_y = self.grid.cursor_y.saturating_sub(p(0, 1));
            }
            'G' => { // CHA — Cursor Horizontal Absolute
                self.grid.wrap_pending = false;
                self.grid.cursor_x = p(0, 1).saturating_sub(1).min(self.grid.cols - 1);
            }
            'H' | 'f' => {
                self.grid.wrap_pending = false;
                self.grid.cursor_y = p(0, 1).saturating_sub(1).min(self.grid.rows - 1);
                self.grid.cursor_x = p(1, 1).saturating_sub(1).min(self.grid.cols - 1);
            }
            'd' => { // VPA — Line Position Absolute
                self.grid.wrap_pending = false;
                self.grid.cursor_y = p(0, 1).saturating_sub(1).min(self.grid.rows - 1);
            }
            'J' => {
                let blank = self.blank_cell();
                let mode = p(0, 0);
                match mode {
                    0 => {
                        let y = self.grid.cursor_y as usize;
                        let x = self.grid.cursor_x as usize;
                        for i in x..self.grid.cols as usize { self.grid.cells[y][i] = blank.clone(); }
                        for row in self.grid.cells.iter_mut().skip(y + 1) {
                            for cell in row.iter_mut() { *cell = blank.clone(); }
                        }
                    }
                    1 => {
                        let y = self.grid.cursor_y as usize;
                        let x = self.grid.cursor_x as usize;
                        for row in self.grid.cells.iter_mut().take(y) {
                            for cell in row.iter_mut() { *cell = blank.clone(); }
                        }
                        for i in 0..=x.min(self.grid.cols as usize - 1) { self.grid.cells[y][i] = blank.clone(); }
                    }
                    2 => {
                        for row in self.grid.cells.iter_mut() {
                            for cell in row.iter_mut() { *cell = blank.clone(); }
                        }
                    }
                    3 => {
                        for row in self.grid.cells.iter_mut() {
                            for cell in row.iter_mut() { *cell = blank.clone(); }
                        }
                        self.scrollback.clear();
                        self.pending_scrollback.clear();
                    }
                    _ => {}
                }
            }
            'K' => {
                let blank = self.blank_cell();
                let mode = p(0, 0);
                let y = self.grid.cursor_y as usize;
                let x = self.grid.cursor_x as usize;
                match mode {
                    0 => { for i in x..self.grid.cols as usize { self.grid.cells[y][i] = blank.clone(); } }
                    1 => { for i in 0..=x.min(self.grid.cols as usize - 1) { self.grid.cells[y][i] = blank.clone(); } }
                    2 => { for cell in self.grid.cells[y].iter_mut() { *cell = blank.clone(); } }
                    _ => {}
                }
            }
            'X' => { // ECH — Erase Character (without moving cursor)
                let blank = self.blank_cell();
                let n = p(0, 1) as usize;
                let y = self.grid.cursor_y as usize;
                let x = self.grid.cursor_x as usize;
                if y < self.grid.rows as usize {
                    // Fix up wide char boundaries at erase start
                    self.fixup_wide_char(x, y);
                    let end = (x + n).min(self.grid.cols as usize);
                    // Fix up wide char boundary at erase end
                    if end < self.grid.cols as usize {
                        self.fixup_wide_char(end, y);
                    }
                    for i in x..end {
                        self.grid.cells[y][i] = blank.clone();
                    }
                }
            }
            'P' => { // DCH — Delete Character (shift left, blank fills right)
                let blank = self.blank_cell();
                let n = p(0, 1) as usize;
                let y = self.grid.cursor_y as usize;
                let x = self.grid.cursor_x as usize;
                let cols = self.grid.cols as usize;
                if y < self.grid.rows as usize {
                    self.fixup_wide_char(x, y);
                    for _ in 0..n.min(cols.saturating_sub(x)) {
                        self.grid.cells[y].remove(x);
                        self.grid.cells[y].push(blank.clone());
                    }
                }
            }
            '@' => { // ICH — Insert Character (shift right, blank at cursor)
                let blank = self.blank_cell();
                let n = p(0, 1) as usize;
                let y = self.grid.cursor_y as usize;
                let x = self.grid.cursor_x as usize;
                let cols = self.grid.cols as usize;
                if y < self.grid.rows as usize {
                    self.fixup_wide_char(x, y);
                    for _ in 0..n.min(cols.saturating_sub(x)) {
                        self.grid.cells[y].pop();
                        self.grid.cells[y].insert(x, blank.clone());
                    }
                }
            }
            'b' => { // REP — Repeat preceding graphic character
                let n = p(0, 1);
                let c = self.state.last_printed_char;
                for _ in 0..n {
                    self.print(c);
                }
            }
            'm' => {
                self.state.current_style.apply_sgr(&ps);
            }
            'n' if intermediates.is_empty() => { // DSR — Device Status Report
                let mode = p(0, 0);
                if mode == 6 {
                    // CPR — Cursor Position Report
                    let response = format!(
                        "\x1b[{};{}R",
                        self.grid.cursor_y + 1,
                        self.grid.cursor_x + 1
                    );
                    self.state.pending_responses.push(response.into_bytes());
                }
            }
            'c' => { // DA — Device Attributes
                if intermediates.is_empty() {
                    // DA1: Primary Device Attributes
                    if p(0, 0) == 0 {
                        self.state.pending_responses.push(b"\x1b[?62;c".to_vec());
                    }
                } else if intermediates == b">" {
                    // DA2: Secondary Device Attributes
                    if p(0, 0) == 0 {
                        self.state.pending_responses.push(b"\x1b[>0;10;1c".to_vec());
                    }
                }
            }
            'q' if intermediates == b" " => { // DECSCUSR — Set Cursor Style
                self.grid.modes.cursor_shape = p(0, 0) as u8;
            }
            'S' => { let n = p(0, 1); for _ in 0..n { self.scroll_up(); } }
            'T' => { // SD — Scroll Down
                if ps.len() <= 1 {
                    let n = p(0, 1);
                    for _ in 0..n { self.scroll_down(); }
                }
            }
            'M' => { // DL — Delete Lines (within scroll region)
                let blank = self.blank_cell();
                let n = p(0, 1) as usize;
                let y = self.grid.cursor_y as usize;
                let top = self.grid.scroll_top as usize;
                let bottom = self.grid.scroll_bottom as usize;
                if y >= top {
                    for _ in 0..n {
                        if y <= bottom && bottom < self.grid.cells.len() {
                            self.grid.cells.remove(y);
                            self.grid.cells.insert(bottom, vec![blank.clone(); self.grid.cols as usize]);
                        }
                    }
                }
            }
            'L' => { // IL — Insert Lines (within scroll region)
                let blank = self.blank_cell();
                let n = p(0, 1) as usize;
                let y = self.grid.cursor_y as usize;
                let top = self.grid.scroll_top as usize;
                let bottom = self.grid.scroll_bottom as usize;
                if y >= top {
                    for _ in 0..n {
                        if y <= bottom && bottom < self.grid.cells.len() {
                            self.grid.cells.remove(bottom);
                            self.grid.cells.insert(y, vec![blank.clone(); self.grid.cols as usize]);
                        }
                    }
                }
            }
            'r' if intermediates.is_empty() => { // DECSTBM — Set Scrolling Region
                let top = p(0, 1).saturating_sub(1);
                let bottom = p(1, self.grid.rows).saturating_sub(1).min(self.grid.rows - 1);
                if top < bottom {
                    self.grid.scroll_top = top;
                    self.grid.scroll_bottom = bottom;
                }
                self.grid.cursor_x = 0;
                self.grid.cursor_y = 0;
                self.grid.wrap_pending = false;
            }
            's' if intermediates.is_empty() => { // SCP — Save Cursor Position
                self.state.saved_cursor_state = Some(super::SavedCursor {
                    x: self.grid.cursor_x,
                    y: self.grid.cursor_y,
                    style: self.state.current_style.clone(),
                    g0_charset: self.grid.modes.g0_charset,
                    g1_charset: self.grid.modes.g1_charset,
                    active_charset: self.grid.modes.active_charset,
                    autowrap_mode: self.grid.modes.autowrap_mode,
                });
            }
            'u' if intermediates.is_empty() => { // RCP — Restore Cursor Position
                if let Some(ref saved) = self.state.saved_cursor_state {
                    self.grid.wrap_pending = false;
                    self.grid.cursor_x = saved.x.min(self.grid.cols - 1);
                    self.grid.cursor_y = saved.y.min(self.grid.rows - 1);
                    self.state.current_style = saved.style.clone();
                    self.grid.modes.g0_charset = saved.g0_charset;
                    self.grid.modes.g1_charset = saved.g1_charset;
                    self.grid.modes.active_charset = saved.active_charset;
                    self.grid.modes.autowrap_mode = saved.autowrap_mode;
                }
            }
            't' => {} // Window ops — ignore
            'h' | 'l' => {
                if intermediates == b"?" {
                    let enable = action == 'h';
                    for param in &ps {
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
                                if enable {
                                    self.state.saved_cursor_state = Some(super::SavedCursor {
                                        x: self.grid.cursor_x,
                                        y: self.grid.cursor_y,
                                        style: self.state.current_style.clone(),
                                        g0_charset: self.grid.modes.g0_charset,
                                        g1_charset: self.grid.modes.g1_charset,
                                        active_charset: self.grid.modes.active_charset,
                                        autowrap_mode: self.grid.modes.autowrap_mode,
                                    });
                                } else if let Some(ref saved) = self.state.saved_cursor_state {
                                    self.grid.wrap_pending = false;
                                    self.grid.cursor_x = saved.x.min(self.grid.cols - 1);
                                    self.grid.cursor_y = saved.y.min(self.grid.rows - 1);
                                    self.state.current_style = saved.style.clone();
                                    self.grid.modes.g0_charset = saved.g0_charset;
                                    self.grid.modes.g1_charset = saved.g1_charset;
                                    self.grid.modes.active_charset = saved.active_charset;
                                    self.grid.modes.autowrap_mode = saved.autowrap_mode;
                                }
                            }
                            Some(2004) => self.grid.modes.bracketed_paste = enable,
                            Some(1049 | 1047 | 47) => {
                                if enable {
                                    // Save main screen buffer, cursor, and terminal modes
                                    self.state.saved_grid = Some(self.grid.cells.clone());
                                    self.state.saved_cursor = Some((self.grid.cursor_x, self.grid.cursor_y));
                                    self.state.saved_modes = Some(self.grid.modes.clone());
                                    self.state.in_alt_screen = true;
                                    let blank = Cell::default();
                                    for row in self.grid.cells.iter_mut() {
                                        for cell in row.iter_mut() { *cell = blank.clone(); }
                                    }
                                    self.grid.cursor_x = 0;
                                    self.grid.cursor_y = 0;
                                    self.grid.scroll_top = 0;
                                    self.grid.scroll_bottom = self.grid.rows - 1;
                                } else {
                                    // Restore main screen buffer and terminal modes
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
                                    if let Some((cx, cy)) = self.state.saved_cursor.take() {
                                        self.grid.cursor_x = cx.min(self.grid.cols - 1);
                                        self.grid.cursor_y = cy.min(self.grid.rows - 1);
                                    }
                                    if let Some(modes) = self.state.saved_modes.take() {
                                        self.grid.modes = modes;
                                    }
                                    self.grid.scroll_top = 0;
                                    self.grid.scroll_bottom = self.grid.rows - 1;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
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
            ([], b'7') => { // DECSC — Save Cursor (position + style + charsets + autowrap)
                self.state.saved_cursor_state = Some(super::SavedCursor {
                    x: self.grid.cursor_x,
                    y: self.grid.cursor_y,
                    style: self.state.current_style.clone(),
                    g0_charset: self.grid.modes.g0_charset,
                    g1_charset: self.grid.modes.g1_charset,
                    active_charset: self.grid.modes.active_charset,
                    autowrap_mode: self.grid.modes.autowrap_mode,
                });
            }
            ([], b'8') => { // DECRC — Restore Cursor (position + style + charsets + autowrap)
                if let Some(ref saved) = self.state.saved_cursor_state {
                    self.grid.wrap_pending = false;
                    self.grid.cursor_x = saved.x.min(self.grid.cols - 1);
                    self.grid.cursor_y = saved.y.min(self.grid.rows - 1);
                    self.state.current_style = saved.style.clone();
                    self.grid.modes.g0_charset = saved.g0_charset;
                    self.grid.modes.g1_charset = saved.g1_charset;
                    self.grid.modes.active_charset = saved.active_charset;
                    self.grid.modes.autowrap_mode = saved.autowrap_mode;
                }
            }
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
                self.state.saved_cursor = None;
                self.state.saved_cursor_state = None;
                self.state.title.clear();
                self.state.last_printed_char = ' ';
                let blank = Cell::default();
                for row in self.grid.cells.iter_mut() {
                    for cell in row.iter_mut() { *cell = blank.clone(); }
                }
            }
            ([], b'=') => { // DECKPAM — Keypad Application Mode
                self.grid.modes.keypad_app_mode = true;
            }
            ([], b'>') => { // DECKPNM — Keypad Numeric Mode
                self.grid.modes.keypad_app_mode = false;
            }
            ([b'('], b'B') => { self.grid.modes.g0_charset = 0; } // G0 → ASCII
            ([b'('], b'0') => { self.grid.modes.g0_charset = 1; } // G0 → Line Drawing
            ([b')'], b'B') => { self.grid.modes.g1_charset = 0; } // G1 → ASCII
            ([b')'], b'0') => { self.grid.modes.g1_charset = 1; } // G1 → Line Drawing
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

        // Set window title (OSC 0 / OSC 2)
        // Other OSC codes (52=clipboard, 4/10/11=colors, 8=hyperlinks) are ignored
        if let Some(0 | 2) = osc_num {
            if params.len() >= 2 {
                if let Ok(title) = std::str::from_utf8(params[1]) {
                    self.state.title = title.to_string();
                }
            }
        }
    }

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
}
