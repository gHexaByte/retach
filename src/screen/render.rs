use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;

use super::cell::{Cell, Row};
use super::grid::{ActiveCharset, Charset, Grid, MouseEncoding, TerminalModes};
use super::style::{StyleId, StyleTable, write_u16};

/// Per-connection render cache for dirty tracking and mode delta.
pub struct RenderCache {
    row_hashes: Vec<u64>,
    last_modes: Option<TerminalModes>,
    last_scroll_region: Option<(u16, u16)>,
    last_title: String,
    last_cursor: Option<(u16, u16)>,
    last_cursor_visible: Option<bool>,
}

impl Default for RenderCache {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderCache {
    pub fn new() -> Self {
        Self {
            row_hashes: Vec::new(),
            last_modes: None,
            last_scroll_region: None,
            last_title: String::new(),
            last_cursor: None,
            last_cursor_visible: None,
        }
    }

    /// Invalidate the cache so the next render is a full redraw.
    pub fn invalidate(&mut self) {
        self.row_hashes.clear();
        self.last_modes = None;
        self.last_scroll_region = None;
        self.last_title.clear();
        self.last_cursor = None;
        self.last_cursor_visible = None;
    }
}

/// Index (inclusive) of the last cell with visible content or non-default style.
/// Returns `None` if the entire row is blank with default style.
fn last_content_position(row: &[Cell]) -> Option<usize> {
    row.iter()
        .rposition(|c| (c.c != ' ' && c.c != '\0') || !c.style_id.is_default())
}

fn hash_row(row: &Row) -> u64 {
    let mut hasher = DefaultHasher::new();
    row.hash(&mut hasher);
    hasher.finish()
}

/// Render a single row of cells as ANSI bytes with SGR codes.
/// Returns empty Vec for fully blank lines.
pub(super) fn render_line(row: &Row, styles: &StyleTable) -> Vec<u8> {
    let last_non_space = match last_content_position(row) {
        Some(pos) => pos,
        None => {
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    let mut current_id = StyleId::default();
    for (i, cell) in row.iter().enumerate() {
        if i > last_non_space { break; }
        // Skip wide char continuation cells
        if cell.width == 0 { continue; }
        if cell.style_id != current_id {
            styles.get(cell.style_id).write_sgr_with_reset_to(&mut out);
            current_id = cell.style_id;
        }
        let mut buf = [0u8; 4];
        out.extend_from_slice(cell.c.encode_utf8(&mut buf).as_bytes());
        for &mark in row.combining(i as u16) {
            out.extend_from_slice(mark.encode_utf8(&mut buf).as_bytes());
        }
    }
    if !current_id.is_default() {
        out.extend_from_slice(b"\x1b[0m");
    }
    out
}

/// Emit a single boolean DEC private mode sequence.
fn emit_dec_mode(out: &mut Vec<u8>, code: u16, enabled: bool) {
    out.extend_from_slice(b"\x1b[?");
    write_u16(out, code);
    out.push(if enabled { b'h' } else { b'l' });
}

/// Emit a character set designation: ESC `slot` `final`.
/// `slot` is `(` for G0, `)` for G1.
fn emit_charset(out: &mut Vec<u8>, slot: u8, charset: Charset) {
    out.push(0x1b);
    out.push(slot);
    out.push(match charset {
        Charset::Ascii => b'B',
        Charset::LineDrawing => b'0',
    });
}

/// Emit escape sequences for one mode, unconditionally.
fn emit_mode(out: &mut Vec<u8>, modes: &TerminalModes) {
    // Cursor shape (DECSCUSR)
    out.extend_from_slice(b"\x1b[");
    out.push(b'0' + modes.cursor_shape.to_param());
    out.extend_from_slice(b" q");

    // Boolean DEC private modes
    emit_dec_mode(out, 1, modes.cursor_key_mode);
    emit_dec_mode(out, 6, modes.origin_mode);
    emit_dec_mode(out, 7, modes.autowrap_mode);
    emit_dec_mode(out, 2004, modes.bracketed_paste);

    // Mouse modes: emit each independently
    emit_dec_mode(out, 1000, modes.mouse_modes.click);
    emit_dec_mode(out, 1002, modes.mouse_modes.button);
    emit_dec_mode(out, 1003, modes.mouse_modes.any);

    // Mouse encoding
    emit_mouse_encoding(out, modes.mouse_encoding);

    emit_dec_mode(out, 1004, modes.focus_reporting);

    // Keypad mode (not DEC private — uses ESC = / ESC >)
    out.extend_from_slice(if modes.keypad_app_mode { b"\x1b=" } else { b"\x1b>" });

    // Character set designations (G0/G1)
    emit_charset(out, b'(', modes.g0_charset);
    emit_charset(out, b')', modes.g1_charset);

    // Active charset: SI (0x0F) for G0, SO (0x0E) for G1
    out.push(match modes.active_charset {
        ActiveCharset::G0 => 0x0F, // SI
        ActiveCharset::G1 => 0x0E, // SO
    });
}

/// Emit mouse encoding sequence for the given encoding mode.
fn emit_mouse_encoding(out: &mut Vec<u8>, encoding: MouseEncoding) {
    match encoding {
        MouseEncoding::Sgr => { out.extend_from_slice(b"\x1b[?1005l"); out.extend_from_slice(b"\x1b[?1006h"); }
        MouseEncoding::Utf8 => { out.extend_from_slice(b"\x1b[?1006l"); out.extend_from_slice(b"\x1b[?1005h"); }
        MouseEncoding::X10 => out.extend_from_slice(b"\x1b[?1006l\x1b[?1005l"),
    }
}


/// Emit only mode sequences that changed since last render.
fn emit_mode_delta(out: &mut Vec<u8>, modes: &TerminalModes, prev: &TerminalModes) {
    if modes.cursor_shape != prev.cursor_shape {
        out.extend_from_slice(b"\x1b[");
        out.push(b'0' + modes.cursor_shape.to_param());
        out.extend_from_slice(b" q");
    }
    if modes.cursor_key_mode != prev.cursor_key_mode {
        emit_dec_mode(out, 1, modes.cursor_key_mode);
    }
    if modes.origin_mode != prev.origin_mode {
        emit_dec_mode(out, 6, modes.origin_mode);
    }
    if modes.autowrap_mode != prev.autowrap_mode {
        emit_dec_mode(out, 7, modes.autowrap_mode);
    }
    if modes.bracketed_paste != prev.bracketed_paste {
        emit_dec_mode(out, 2004, modes.bracketed_paste);
    }
    if modes.mouse_modes.click != prev.mouse_modes.click {
        emit_dec_mode(out, 1000, modes.mouse_modes.click);
    }
    if modes.mouse_modes.button != prev.mouse_modes.button {
        emit_dec_mode(out, 1002, modes.mouse_modes.button);
    }
    if modes.mouse_modes.any != prev.mouse_modes.any {
        emit_dec_mode(out, 1003, modes.mouse_modes.any);
    }
    if modes.mouse_encoding != prev.mouse_encoding {
        emit_mouse_encoding(out, modes.mouse_encoding);
    }
    if modes.focus_reporting != prev.focus_reporting {
        emit_dec_mode(out, 1004, modes.focus_reporting);
    }
    if modes.keypad_app_mode != prev.keypad_app_mode {
        out.extend_from_slice(if modes.keypad_app_mode { b"\x1b=" } else { b"\x1b>" });
    }
    if modes.g0_charset != prev.g0_charset {
        emit_charset(out, b'(', modes.g0_charset);
    }
    if modes.g1_charset != prev.g1_charset {
        emit_charset(out, b')', modes.g1_charset);
    }
    if modes.active_charset != prev.active_charset {
        out.push(match modes.active_charset {
            ActiveCharset::G0 => 0x0F, // SI
            ActiveCharset::G1 => 0x0E, // SO
        });
    }
}

/// Render the full screen grid as ANSI bytes.
/// If `full` is true, clears screen first (used on initial attach).
/// Otherwise uses dirty tracking to skip unchanged rows.
pub(super) fn render_screen(grid: &Grid, title: &str, full: bool, cache: &mut RenderCache) -> Vec<u8> {
    render_screen_impl(grid, title, &[], full, cache)
}

/// Render the screen with scrollback lines injected into the real terminal's
/// native scrollback buffer.
///
/// The entire output — scrollback injection and screen redraw — is wrapped in
/// a single synchronized-output block to prevent flicker.  Scrollback lines
/// are emitted first (cursor positioned at the bottom so `\r\n` triggers real
/// terminal scrolling), followed by a full screen clear and redraw.
pub(super) fn render_screen_with_scrollback(
    grid: &Grid,
    title: &str,
    scrollback: &[Vec<u8>],
    cache: &mut RenderCache,
) -> Vec<u8> {
    render_screen_impl(grid, title, scrollback, true, cache)
}

/// Inject scrollback lines into the terminal's native scrollback buffer.
/// Emitted OUTSIDE the synchronized output block — some terminals (Blink/hterm)
/// buffer all sync'd output atomically, preventing intermediate scroll operations
/// from pushing content into native scrollback.
fn render_scrollback(out: &mut Vec<u8>, grid: &Grid, scrollback: &[Vec<u8>]) {
    let rows = grid.rows() as usize;
    out.extend_from_slice(b"\x1b[?25l\x1b[r");

    for chunk in scrollback.chunks(rows) {
        for (i, line) in chunk.iter().enumerate() {
            out.extend_from_slice(b"\x1b[");
            write_u16(out, (i + 1) as u16);
            out.extend_from_slice(b";1H\x1b[0m");
            out.extend_from_slice(line);
            out.extend_from_slice(b"\x1b[K");
        }
        if chunk.len() < rows {
            for i in chunk.len()..rows {
                out.extend_from_slice(b"\x1b[");
                write_u16(out, (i + 1) as u16);
                out.extend_from_slice(b";1H\x1b[2K");
            }
        }
        out.extend_from_slice(b"\x1b[");
        write_u16(out, grid.rows());
        out.extend_from_slice(b";1H");
        out.extend(std::iter::repeat_n(b'\n', chunk.len()));
    }
}

/// Emit scroll region, cursor position, terminal modes, and window title.
/// Cursor visibility is handled separately after no-op detection because
/// the sync block unconditionally hides the cursor at the start.
fn render_modes_title_cursor(
    out: &mut Vec<u8>,
    grid: &Grid,
    title: &str,
    full: bool,
    cache: &mut RenderCache,
) {
    // Scroll region (DECSTBM): must be emitted BEFORE cursor position because
    // setting the scroll region resets the cursor to home.
    let scroll_region = grid.scroll_region();
    if full || cache.last_scroll_region != Some(scroll_region) {
        out.extend_from_slice(b"\x1b[");
        write_u16(out, scroll_region.0 + 1);
        out.push(b';');
        write_u16(out, scroll_region.1 + 1);
        out.push(b'r');
        cache.last_scroll_region = Some(scroll_region);
    }

    // Mode sequences before cursor position: emit_mode emits \x1b[?6h/l (DECOM)
    // which homes the cursor on xterm/VTE. CUP must come AFTER modes so it is
    // not overridden by cursor-homing side effects of DECOM state changes.
    let modes = grid.modes();
    match &cache.last_modes {
        Some(prev) if !full => emit_mode_delta(out, modes, prev),
        _ => emit_mode(out, modes),
    }
    cache.last_modes = Some(modes.clone());

    // Cursor position: emitted AFTER modes so it overrides any cursor-homing
    // caused by DECOM (?6) or other mode changes.
    let cursor_pos = grid.cursor_pos();
    if full || cache.last_cursor != Some(cursor_pos) {
        out.extend_from_slice(b"\x1b[");
        write_u16(out, cursor_pos.1 + 1);
        out.push(b';');
        write_u16(out, cursor_pos.0 + 1);
        out.push(b'H');
        cache.last_cursor = Some(cursor_pos);
    }

    // Window title: emit on full render (client may have stale title) or when changed.
    // Sanitized to prevent OSC injection.
    if full || title != cache.last_title {
        out.extend_from_slice(b"\x1b]2;");
        for &b in title.as_bytes() {
            if b >= 0x20 && b != 0x7f {
                out.push(b);
            }
        }
        out.push(0x07);
        cache.last_title = title.to_string();
    }

}

fn render_screen_impl(
    grid: &Grid,
    title: &str,
    scrollback: &[Vec<u8>],
    full: bool,
    cache: &mut RenderCache,
) -> Vec<u8> {
    let capacity = if full || !scrollback.is_empty() {
        grid.cols() as usize * grid.rows() as usize * 4
    } else {
        1024
    };
    let mut out = Vec::with_capacity(capacity);

    if !scrollback.is_empty() {
        render_scrollback(&mut out, grid, scrollback);
        cache.invalidate();
    }

    let full = full || !scrollback.is_empty();

    // Remember length before render payload to detect no-op.
    let pre_render_len = out.len();

    // Synchronized output block: begin
    out.extend_from_slice(b"\x1b[?2026h");
    out.extend_from_slice(b"\x1b[?25l");

    if full {
        out.extend_from_slice(b"\x1b[0m\x1b[2J\x1b[H");
        cache.invalidate();
    }

    let num_rows = grid.visible_row_count();
    if cache.row_hashes.len() != num_rows {
        cache.row_hashes.resize(num_rows, u64::MAX);
    }

    for (y, row) in grid.visible_rows().enumerate() {
        let row_hash = hash_row(row);

        if !full && cache.row_hashes[y] == row_hash {
            continue;
        }
        cache.row_hashes[y] = row_hash;

        out.extend_from_slice(b"\x1b[");
        write_u16(&mut out, y as u16 + 1);
        out.extend_from_slice(b";1H");

        let write_len = last_content_position(row)
            .map(|p| p + 1)
            .unwrap_or(0);

        let mut last_sid = StyleId::default();
        for (x, cell) in row.iter().enumerate() {
            if x >= write_len { break; }
            if cell.width == 0 { continue; }
            if cell.style_id != last_sid {
                grid.style_table().get(cell.style_id).write_sgr_with_reset_to(&mut out);
                last_sid = cell.style_id;
            }
            let mut buf = [0u8; 4];
            out.extend_from_slice(cell.c.encode_utf8(&mut buf).as_bytes());
            for &mark in row.combining(x as u16) {
                out.extend_from_slice(mark.encode_utf8(&mut buf).as_bytes());
            }
        }

        // Reset SGR; erase to EOL only for incremental (full already cleared via \x1b[2J)
        out.extend_from_slice(if full { b"\x1b[0m" as &[u8] } else { b"\x1b[0m\x1b[K" });
    }

    render_modes_title_cursor(&mut out, grid, title, full, cache);

    // Cursor visibility is handled AFTER no-op detection because the sync
    // block unconditionally hides the cursor (\x1b[?25l) at the start.
    // If we send any output, we must restore cursor visibility at the end.
    // Caching cursor visibility would cause the cursor to stay hidden on
    // subsequent renders where only rows changed.
    let cursor_visible = grid.cursor_visible();

    // No-op detection: nothing emitted besides sync header AND cursor
    // visibility unchanged — safe to skip the entire frame.
    let header_len = pre_render_len + b"\x1b[?2026h\x1b[?25l".len();
    if out.len() == header_len && cache.last_cursor_visible == Some(cursor_visible) {
        out.truncate(pre_render_len);
        return out;
    }
    cache.last_cursor_visible = Some(cursor_visible);

    // Restore cursor visibility (we always hide at sync start).
    // When cursor should be hidden, the sync-start \x1b[?25l suffices.
    if cursor_visible {
        out.extend_from_slice(b"\x1b[?25h");
    }

    // Close synchronized output block.
    out.extend_from_slice(b"\x1b[?2026l");
    out
}

/// ANSI escape sequence renderer.
///
/// Renders terminal emulator state as ANSI escape sequences suitable
/// for output to a real terminal. Uses dirty-tracking for incremental updates.
pub struct AnsiRenderer {
    cache: RenderCache,
}

impl Default for AnsiRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl AnsiRenderer {
    /// Create a new ANSI renderer.
    pub fn new() -> Self {
        Self { cache: RenderCache::new() }
    }

    /// Render a Screen directly (optimized path using Grid internals).
    ///
    /// This is the fast path used by retach's server, with row-hash dirty
    /// tracking and mode delta encoding.
    pub fn render_screen(&mut self, grid: &super::grid::Grid, title: &str, full: bool) -> Vec<u8> {
        render_screen(grid, title, full, &mut self.cache)
    }

    /// Render with scrollback lines prepended in a synchronized block.
    pub fn render_screen_with_scrollback(
        &mut self,
        grid: &super::grid::Grid,
        title: &str,
        scrollback: &[Vec<u8>],
    ) -> Vec<u8> {
        render_screen_with_scrollback(grid, title, scrollback, &mut self.cache)
    }

    /// Invalidate the cache, forcing a full redraw on next render.
    pub fn invalidate(&mut self) {
        self.cache.invalidate();
    }
}

impl super::traits::TerminalRenderer for AnsiRenderer {
    type Output = Vec<u8>;

    fn render(&mut self, emulator: &dyn super::traits::TerminalEmulator, full: bool) -> Vec<u8> {
        use super::style::Style;

        let capacity = if full {
            emulator.cols() as usize * emulator.rows() as usize * 4
        } else {
            1024
        };
        let mut out = Vec::with_capacity(capacity);

        // Synchronized output block: begin
        out.extend_from_slice(b"\x1b[?2026h");
        out.extend_from_slice(b"\x1b[?25l");

        if full {
            out.extend_from_slice(b"\x1b[0m\x1b[2J\x1b[H");
            self.cache.invalidate();
        }

        // Ensure row_hashes cache matches current row count
        let num_rows = emulator.rows() as usize;
        if self.cache.row_hashes.len() != num_rows {
            self.cache.row_hashes.resize(num_rows, u64::MAX);
        }

        let header_len = out.len();

        for (y, row) in emulator.visible_rows().enumerate() {
            let row_hash = hash_row(row);
            if !full && self.cache.row_hashes[y] == row_hash {
                continue;
            }
            self.cache.row_hashes[y] = row_hash;

            // Move cursor to start of row
            out.extend_from_slice(b"\x1b[");
            write_u16(&mut out, (y as u16) + 1);
            out.extend_from_slice(b";1H");

            let write_len = last_content_position(row)
                .map(|p| p + 1)
                .unwrap_or(0);

            let mut prev_style = Style::default();

            for (x, cell) in row.iter().enumerate() {
                if x >= write_len { break; }
                if cell.width == 0 {
                    continue;
                }
                let style = emulator.resolve_style(cell.style_id);
                if style != prev_style {
                    style.write_sgr_with_reset_to(&mut out);
                    prev_style = style;
                }
                let mut buf = [0u8; 4];
                out.extend_from_slice(cell.c.encode_utf8(&mut buf).as_bytes());
                for &mark in row.combining(x as u16) {
                    out.extend_from_slice(mark.encode_utf8(&mut buf).as_bytes());
                }
            }

            // Reset SGR; erase to EOL only for incremental (full already cleared via \x1b[2J)
            out.extend_from_slice(if full { b"\x1b[0m" as &[u8] } else { b"\x1b[0m\x1b[K" });
        }

        // Scroll region (DECSTBM) — emit before cursor position (DECSTBM resets cursor)
        let scroll_region = emulator.scroll_region();
        if full || self.cache.last_scroll_region != Some(scroll_region) {
            out.extend_from_slice(b"\x1b[");
            write_u16(&mut out, scroll_region.0 + 1);
            out.push(b';');
            write_u16(&mut out, scroll_region.1 + 1);
            out.push(b'r');
            self.cache.last_scroll_region = Some(scroll_region);
        }

        // Terminal modes: full on first render, delta afterwards
        let modes = emulator.modes();
        match &self.cache.last_modes {
            Some(prev) if !full => emit_mode_delta(&mut out, modes, prev),
            _ => emit_mode(&mut out, modes),
        }
        self.cache.last_modes = Some(modes.clone());

        // Cursor position: emit only when changed (after modes — DECOM can home cursor)
        let (cx, cy) = emulator.cursor_position();
        let cursor_pos = (cx, cy);
        if full || self.cache.last_cursor != Some(cursor_pos) {
            out.extend_from_slice(b"\x1b[");
            write_u16(&mut out, cy + 1);
            out.push(b';');
            write_u16(&mut out, cx + 1);
            out.push(b'H');
            self.cache.last_cursor = Some(cursor_pos);
        }

        // Window title — emit on full render or when changed
        let title = emulator.title();
        if full || title != self.cache.last_title {
            out.extend_from_slice(b"\x1b]2;");
            for &b in title.as_bytes() {
                if b >= 0x20 && b != 0x7f {
                    out.push(b);
                }
            }
            out.push(0x07);
            self.cache.last_title = title.to_string();
        }

        // Cursor visibility: handled after no-op detection because the sync
        // block unconditionally hides cursor at the start.
        let cursor_visible = emulator.cursor_visible();

        // No-op detection: nothing emitted AND cursor visibility unchanged
        if out.len() == header_len && self.cache.last_cursor_visible == Some(cursor_visible) {
            out.clear();
            return out;
        }
        self.cache.last_cursor_visible = Some(cursor_visible);

        // Restore cursor visibility (we always hide at sync start)
        if cursor_visible {
            out.extend_from_slice(b"\x1b[?25h");
        }

        // Close synchronized output block
        out.extend_from_slice(b"\x1b[?2026l");

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::grid::{CursorShape, MouseEncoding};
    use super::super::style::{Color, Style, StyleTable};

    /// Helper: intern a style and assign it to a cell in tests.
    fn set_style(cell: &mut Cell, style: Style, st: &mut StyleTable) {
        cell.style_id = st.intern(style);
    }

    #[test]
    fn render_line_blank() {
        let row = Row::new(80);
        let st = StyleTable::new();
        let result = render_line(&row, &st);
        assert!(result.is_empty(), "blank line should produce empty vec");
    }

    #[test]
    fn render_line_with_text() {
        let mut row = Row::new(10);
        row[0].c = 'H';
        row[1].c = 'i';
        let st = StyleTable::new();
        let result = render_line(&row, &st);
        assert_eq!(result, b"Hi");
    }

    #[test]
    fn render_line_with_style() {
        let mut st = StyleTable::new();
        let mut row = Row::new(10);
        row[0].c = 'R';
        set_style(&mut row[0], Style { fg: Some(Color::Indexed(1)), ..Style::default() }, &mut st);
        let result = render_line(&row, &st);
        // Should have combined reset+set SGR, then 'R', then reset
        assert!(result.starts_with(b"\x1b[0;31mR"),
            "expected combined reset+set, got: {:?}", String::from_utf8_lossy(&result));
        assert!(result.ends_with(b"\x1b[0m"));
    }

    #[test]
    fn render_screen_full() {
        let grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "", true, &mut cache);
        let text = String::from_utf8_lossy(&result);
        // Full render must clear the screen and home cursor
        assert!(text.contains("\x1b[2J\x1b[H"), "full render must emit screen clear");
        // Should contain cursor position
        assert!(text.contains("\x1b[1;1H"));
    }

    #[test]
    fn render_screen_incremental() {
        let grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        // Should NOT contain clear screen
        assert!(!text.contains("\x1b[2J"));
    }

    #[test]
    fn render_line_skips_wide_char_continuation() {
        let mut row = Row::new(10);
        row[0] = Cell::new('你', StyleId::default(), 2);
        row[1] = Cell::new('\0', StyleId::default(), 0);
        row[2].c = 'A';
        let st = StyleTable::new();
        let result = render_line(&row, &st);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains('你'));
        assert!(text.contains('A'));
        // Should not contain null bytes
        assert!(!text.contains('\0'));
    }

    #[test]
    fn render_screen_includes_title() {
        let grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "My Title", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b]2;My Title\x07"));
    }

    #[test]
    fn render_screen_no_title_when_empty() {
        let grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(!text.contains("\x1b]2;"));
    }

    #[test]
    fn render_screen_hidden_cursor() {
        let mut grid = Grid::new(10, 3, 0);
        grid.set_cursor_visible(false);
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        // Should hide cursor at top
        assert!(text.contains("\x1b[?25l"));
        // Should NOT restore cursor (it was hidden)
        assert!(!text.contains("\x1b[?25h"));
    }

    #[test]
    fn render_screen_incremental_dirty_tracking() {
        let mut grid = Grid::new(10, 5, 0);
        // Move cursor to a unique position so cursor-position output doesn't
        // collide with row-position sequences we're checking.
        grid.set_cursor_x_unclamped(3);
        grid.set_cursor_y_unclamped(4);
        let mut cache = RenderCache::new();
        // First render: all rows are drawn
        let result1 = render_screen(&grid, "", false, &mut cache);
        let text1 = String::from_utf8_lossy(&result1);
        // All 5 rows should be positioned
        assert!(text1.contains("\x1b[1;1H"), "row 1 should be drawn on first render");
        assert!(text1.contains("\x1b[2;1H"), "row 2 should be drawn on first render");
        assert!(text1.contains("\x1b[3;1H"), "row 3 should be drawn on first render");

        // Second render without changes: content rows should be skipped
        let result2 = render_screen(&grid, "", false, &mut cache);
        let text2 = String::from_utf8_lossy(&result2);
        // Row-content positioning should NOT appear (cache hit)
        // (Note: \x1b[5;4H will still appear for cursor positioning)
        assert!(!text2.contains("\x1b[1;1H"),
            "unchanged rows should be skipped in incremental render");
        assert!(!text2.contains("\x1b[2;1H"),
            "unchanged rows should be skipped in incremental render");

        // Now change row 2 (0-indexed=1) and render again
        grid.visible_row_mut(1)[0].c = 'X';
        let result3 = render_screen(&grid, "", false, &mut cache);
        let text3 = String::from_utf8_lossy(&result3);
        // Row 2 (1-indexed) should be redrawn
        assert!(text3.contains("\x1b[2;1H"),
            "changed row should be redrawn in incremental render");
        // Other rows should still be skipped
        assert!(!text3.contains("\x1b[1;1H"),
            "unchanged row 1 should be skipped");
        assert!(!text3.contains("\x1b[3;1H"),
            "unchanged row 3 should be skipped");
    }

    #[test]
    fn render_screen_synchronized_output() {
        let grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        // Must start with sync begin and end with sync end
        assert!(text.starts_with("\x1b[?2026h"),
            "render should start with synchronized output begin");
        assert!(text.ends_with("\x1b[?2026l"),
            "render should end with synchronized output end");
    }

    #[test]
    fn render_screen_cursor_position() {
        let mut grid = Grid::new(10, 5, 0);
        grid.set_cursor_x_unclamped(4);
        grid.set_cursor_y_unclamped(2);
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "", true, &mut cache);
        let text = String::from_utf8_lossy(&result);
        // Cursor should be positioned at row 3, col 5 (1-indexed)
        assert!(text.contains("\x1b[3;5H"),
            "cursor should be at row 3, col 5 (1-indexed), got: {:?}",
            text.matches("\x1b[").collect::<Vec<_>>());
    }

    #[test]
    fn render_screen_title_cached() {
        let grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        // First render with title
        let result1 = render_screen(&grid, "Title1", false, &mut cache);
        assert!(String::from_utf8_lossy(&result1).contains("\x1b]2;Title1\x07"));

        // Second render with same title: should NOT re-emit
        let result2 = render_screen(&grid, "Title1", false, &mut cache);
        assert!(!String::from_utf8_lossy(&result2).contains("\x1b]2;"),
            "same title should not be re-emitted");

        // Third render with different title: should emit
        let result3 = render_screen(&grid, "Title2", false, &mut cache);
        assert!(String::from_utf8_lossy(&result3).contains("\x1b]2;Title2\x07"),
            "changed title should be emitted");
    }

    #[test]
    fn render_screen_title_sanitized() {
        let grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        // Title with BEL (0x07) that would break OSC
        let evil_title = "bad\x07title";
        let result = render_screen(&grid, evil_title, false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        // The OSC should contain "badtitle" (BEL stripped), not terminate early
        assert!(text.contains("\x1b]2;badtitle\x07"),
            "control chars should be stripped from title");
    }

    #[test]
    fn render_line_multiple_style_changes() {
        let mut st = StyleTable::new();
        let mut row = Row::new(10);
        row[0].c = 'R';
        set_style(&mut row[0], Style { fg: Some(Color::Indexed(1)), ..Style::default() }, &mut st);
        row[1].c = 'G';
        set_style(&mut row[1], Style { fg: Some(Color::Indexed(2)), ..Style::default() }, &mut st);
        row[2].c = 'N'; // default style
        let result = render_line(&row, &st);
        let text = String::from_utf8_lossy(&result);
        // Should have red, then reset+green, then reset
        assert!(text.contains("R"), "should contain 'R'");
        assert!(text.contains("G"), "should contain 'G'");
        assert!(text.contains("N"), "should contain 'N'");
        // With to_sgr_with_reset, style changes use combined reset+set (e.g., \x1b[0;31m)
        // Only transitions back to default produce a bare \x1b[0m
        let combined_sgr_count = text.matches("\x1b[0;").count() + text.matches("\x1b[0m").count();
        assert!(combined_sgr_count >= 2,
            "expected at least 2 combined reset+set SGR sequences, got {}", combined_sgr_count);
    }

    #[test]
    fn render_screen_full_mode_emits_mouse_modes() {
        let mut grid = Grid::new(10, 3, 0);
        grid.modes_mut().mouse_modes.any = true;
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "", true, &mut cache);
        let text = String::from_utf8_lossy(&result);
        // Full render should disable inactive modes and enable active
        assert!(text.contains("\x1b[?1000l"),
            "full render should disable mouse mode 1000");
        assert!(text.contains("\x1b[?1002l"),
            "full render should disable mouse mode 1002");
        assert!(text.contains("\x1b[?1003h"),
            "full render should enable active mouse mode 1003");
    }

    #[test]
    fn render_screen_mode_delta_mouse_switch() {
        let mut grid = Grid::new(10, 3, 0);
        grid.modes_mut().mouse_modes.click = true;
        let mut cache = RenderCache::new();
        // Initial render (full)
        let _ = render_screen(&grid, "", true, &mut cache);

        // Switch: disable click, enable any
        grid.modes_mut().mouse_modes.click = false;
        grid.modes_mut().mouse_modes.any = true;
        let result = render_screen(&grid, "", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        // Should disable old mode
        assert!(text.contains("\x1b[?1000l"),
            "delta should disable old mouse mode 1000");
        // Should enable new mode
        assert!(text.contains("\x1b[?1003h"),
            "delta should enable new mouse mode 1003");
    }

    #[test]
    fn render_screen_mode_delta_bracketed_paste() {
        let mut grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        let _ = render_screen(&grid, "", true, &mut cache);

        // Enable bracketed paste
        grid.modes_mut().bracketed_paste = true;
        let result = render_screen(&grid, "", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b[?2004h"),
            "delta should emit bracketed paste enable");
    }

    #[test]
    fn render_cache_invalidate() {
        let grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        // Populate cache
        let _ = render_screen(&grid, "test", false, &mut cache);
        assert!(!cache.row_hashes.is_empty());
        assert!(cache.last_modes.is_some());

        // Invalidate
        cache.invalidate();
        assert!(cache.row_hashes.is_empty());
        assert!(cache.last_modes.is_none());
        assert!(cache.last_title.is_empty());

        // Next render should redraw everything (all rows emitted)
        let result = render_screen(&grid, "", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b[1;1H"), "after invalidate, all rows should be redrawn");
        assert!(text.contains("\x1b[2;1H"), "after invalidate, all rows should be redrawn");
        assert!(text.contains("\x1b[3;1H"), "after invalidate, all rows should be redrawn");
    }

    // --- New tests ---

    #[test]
    fn render_line_styled_spaces_not_blank() {
        // Row of spaces with colored bg should render SGR, not b" "
        let mut st = StyleTable::new();
        let mut row = Row::new(10);
        let red_bg = st.intern(Style { bg: Some(Color::Indexed(1)), ..Style::default() });
        for cell in row.iter_mut() {
            cell.c = ' ';
            cell.style_id = red_bg;
        }
        let result = render_line(&row, &st);
        // Should contain SGR for the background color, not just a plain space
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b["), "styled spaces should produce SGR sequences");
        assert!(text.contains("41m"), "red bg should produce code 41");
    }

    #[test]
    fn render_line_styled_trailing_space() {
        // Trailing styled space should be included in output
        let mut st = StyleTable::new();
        let mut row = Row::new(5);
        row[0].c = 'A';
        row[4].c = ' ';
        set_style(&mut row[4], Style { bg: Some(Color::Indexed(4)), ..Style::default() }, &mut st);
        let result = render_line(&row, &st);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("44m"), "trailing styled space should include blue bg SGR");
    }

    #[test]
    fn render_line_wide_char_at_end() {
        // Wide char at last two positions renders correctly
        let mut row = Row::new(10);
        row[8] = Cell::new('\u{4e16}', StyleId::default(), 2); // 世
        row[9] = Cell::new('\0', StyleId::default(), 0); // continuation
        let st = StyleTable::new();
        let result = render_line(&row, &st);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains('\u{4e16}'), "wide char at end should be rendered");
        assert!(!text.contains('\0'), "continuation cell should not produce output");
    }

    #[test]
    fn render_line_rgb_color() {
        let mut st = StyleTable::new();
        let mut row = Row::new(5);
        row[0].c = 'X';
        set_style(&mut row[0], Style { fg: Some(Color::Rgb(100, 150, 200)), ..Style::default() }, &mut st);
        let result = render_line(&row, &st);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("38;2;100;150;200m"), "RGB color should produce 38;2;R;G;B");
    }

    #[test]
    fn render_line_combined_attributes() {
        let mut st = StyleTable::new();
        let mut row = Row::new(5);
        row[0].c = 'Z';
        set_style(&mut row[0], Style {
            bold: true,
            italic: true,
            underline: super::super::style::UnderlineStyle::Single,
            fg: Some(Color::Indexed(3)),
            bg: Some(Color::Indexed(4)),
            ..Style::default()
        }, &mut st);
        let result = render_line(&row, &st);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("1;"), "bold should be present");
        assert!(text.contains("3;"), "italic should be present");
        assert!(text.contains(";4;"), "underline should be present");
        assert!(text.contains("33"), "yellow fg should be present");
        assert!(text.contains("44"), "blue bg should be present");
    }

    #[test]
    fn render_line_256_color() {
        let mut st = StyleTable::new();
        let mut row = Row::new(5);
        row[0].c = 'P';
        set_style(&mut row[0], Style { fg: Some(Color::Indexed(200)), ..Style::default() }, &mut st);
        let result = render_line(&row, &st);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("38;5;200m"), "palette index 200 should produce 38;5;200");
    }

    #[test]
    fn render_screen_title_cleared() {
        // Bug 1 regression test: title change to "" should emit empty OSC
        let grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        // First render with a non-empty title
        let _ = render_screen(&grid, "Hello", false, &mut cache);
        // Now clear the title
        let result = render_screen(&grid, "", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b]2;\x07"),
            "clearing title should emit empty OSC, got: {:?}", text);
    }

    #[test]
    fn render_screen_after_resize() {
        // When row count changes, all rows should be redrawn
        let grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        let _ = render_screen(&grid, "", false, &mut cache);

        // Simulate resize: new grid with more rows
        let grid2 = Grid::new(10, 5, 0);
        let result = render_screen(&grid2, "", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        // Cache had 3 rows, now 5 — row_hashes resized with sentinels for new rows.
        // Rows 1-3 are still blank (same hash), so dirty tracking skips them.
        // Rows 4-5 have sentinel u64::MAX so they get redrawn.
        assert!(!text.contains("\x1b[1;1H"), "unchanged row 1 should be skipped after resize");
        assert!(text.contains("\x1b[4;1H"), "new row 4 should be redrawn after resize");
        assert!(text.contains("\x1b[5;1H"), "new row 5 should be redrawn after resize");
    }

    #[test]
    fn render_screen_style_only_change_detected() {
        // Cell changes color but same char → row should be redrawn
        let mut grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        let _ = render_screen(&grid, "", false, &mut cache);

        // Change style of a cell without changing the char
        let sid = grid.style_table_mut().intern(Style { fg: Some(Color::Indexed(1)), ..Style::default() });
        grid.visible_row_mut(1)[0].style_id = sid;
        let result = render_screen(&grid, "", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b[2;1H"),
            "row with style-only change should be redrawn");
    }

    #[test]
    fn render_screen_1x1_grid() {
        let grid = Grid::new(1, 1, 0);
        let mut cache = RenderCache::new();
        // Should not panic
        let result = render_screen(&grid, "", true, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b[1;1H"), "1x1 grid should position at 1,1");
    }

    #[test]
    fn render_screen_cursor_bottom_right() {
        let mut grid = Grid::new(80, 24, 0);
        grid.set_cursor_x_unclamped(79);
        grid.set_cursor_y_unclamped(23);
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "", true, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b[24;80H"),
            "cursor at bottom-right should position at row 24, col 80");
    }

    #[test]
    fn render_screen_mouse_encoding_1006() {
        let mut grid = Grid::new(10, 3, 0);
        grid.modes_mut().mouse_encoding = MouseEncoding::Sgr;
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "", true, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b[?1006h"), "SGR mouse encoding should be enabled");
    }

    #[test]
    fn render_screen_mouse_encoding_1005() {
        let mut grid = Grid::new(10, 3, 0);
        grid.modes_mut().mouse_encoding = MouseEncoding::Utf8;
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "", true, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b[?1005h"), "UTF-8 mouse encoding should be enabled");
    }

    #[test]
    fn render_screen_cursor_shape_delta() {
        let mut grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        let _ = render_screen(&grid, "", true, &mut cache);

        // Change cursor shape to blinking bar (5)
        grid.modes_mut().cursor_shape = CursorShape::BlinkBar;
        let result = render_screen(&grid, "", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b[5 q"), "cursor shape change should emit DECSCUSR");
    }

    #[test]
    fn render_screen_keypad_mode_delta() {
        let mut grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        let _ = render_screen(&grid, "", true, &mut cache);

        // Enable keypad app mode
        grid.modes_mut().keypad_app_mode = true;
        let result = render_screen(&grid, "", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b="), "keypad app mode should emit ESC =");

        // Disable keypad app mode
        grid.modes_mut().keypad_app_mode = false;
        let result2 = render_screen(&grid, "", false, &mut cache);
        let text2 = String::from_utf8_lossy(&result2);
        assert!(text2.contains("\x1b>"), "keypad normal mode should emit ESC >");
    }

    #[test]
    fn render_line_combining_mark() {
        let mut row = Row::new(10);
        row[0].c = 'e';
        row.push_combining(0, '\u{0301}'); // combining acute accent → é
        let st = StyleTable::new();
        let result = render_line(&row, &st);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("e\u{0301}"), "combining mark should be rendered after base char");
    }

    #[test]
    fn render_line_combining_on_wide_char() {
        let mut row = Row::new(10);
        row[0] = Cell::new('\u{4e16}', StyleId::default(), 2);
        row.push_combining(0, '\u{0308}');
        row[1] = Cell::new('\0', StyleId::default(), 0);
        let st = StyleTable::new();
        let result = render_line(&row, &st);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\u{4e16}\u{0308}"), "combining mark on wide char should render");
    }

    // --- Scrollback injection tests ---

    #[test]
    fn scrollback_positions_cursor_at_bottom() {
        let grid = Grid::new(80, 24, 0);
        let mut cache = RenderCache::new();
        let scrollback = vec![b"line one".to_vec()];
        let result = render_screen_with_scrollback(&grid, "", &scrollback, &mut cache);
        let text = String::from_utf8_lossy(&result);
        // Cursor must be positioned at the last row (24) before scrollback content
        assert!(text.contains("\x1b[24;1H"),
            "scrollback should position cursor at bottom row");
    }

    #[test]
    fn scrollback_lines_appear_before_screen_clear() {
        let grid = Grid::new(80, 24, 0);
        let mut cache = RenderCache::new();
        let scrollback = vec![b"old prompt".to_vec(), b"ls output".to_vec()];
        let result = render_screen_with_scrollback(&grid, "", &scrollback, &mut cache);
        let text = String::from_utf8_lossy(&result);

        let pos_line1 = text.find("old prompt").expect("scrollback line 1 missing");
        let pos_line2 = text.find("ls output").expect("scrollback line 2 missing");
        let pos_clear = text.find("\x1b[2J").expect("screen clear missing");

        assert!(pos_line1 < pos_line2, "scrollback lines must be in order");
        assert!(pos_line2 < pos_clear, "scrollback must precede screen clear");
    }

    #[test]
    fn scrollback_lines_use_cursor_positioning() {
        let grid = Grid::new(80, 24, 0);
        let mut cache = RenderCache::new();
        let scrollback = vec![b"AAA".to_vec(), b"BBB".to_vec()];
        let result = render_screen_with_scrollback(&grid, "", &scrollback, &mut cache);
        let text = String::from_utf8_lossy(&result);

        // Lines should be written at rows 1 and 2 via CUP
        assert!(text.contains("AAA"), "AAA should be present");
        assert!(text.contains("BBB"), "BBB should be present");
        // Should end each line with EL (erase to end of line)
        let raw = &result;
        let pos_a = raw.windows(3).position(|w| w == b"AAA").expect("AAA missing");
        // After "AAA" there should be \x1b[K (erase to end of line)
        assert_eq!(&raw[pos_a + 3..pos_a + 6], b"\x1b[K",
            "scrollback line should end with EL");
    }

    #[test]
    fn scrollback_outside_sync_block() {
        let grid = Grid::new(80, 24, 0);
        let mut cache = RenderCache::new();
        let scrollback = vec![b"scroll line".to_vec()];
        let result = render_screen_with_scrollback(&grid, "", &scrollback, &mut cache);
        let text = String::from_utf8_lossy(&result);

        let sync_begin = text.find("\x1b[?2026h").expect("sync begin missing");
        let pos_scroll = text.find("scroll line").expect("scrollback content missing");
        let sync_end = text.rfind("\x1b[?2026l").expect("sync end missing");

        // Scrollback injection must be BEFORE the sync block
        assert!(pos_scroll < sync_begin,
            "scrollback must be before sync begin (scrollback at {}, sync at {})",
            pos_scroll, sync_begin);
        assert!(sync_begin < sync_end, "sync begin must precede sync end");
    }

    #[test]
    fn scrollback_forces_full_redraw() {
        let mut grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        // Populate cache with an initial render
        let _ = render_screen(&grid, "", false, &mut cache);
        assert!(!cache.row_hashes.is_empty());

        // Modify only row 2 — normally only row 2 would be redrawn
        grid.visible_row_mut(1)[0].c = 'X';

        // Render with scrollback — all rows must be redrawn (full redraw)
        let scrollback = vec![b"old".to_vec()];
        let result = render_screen_with_scrollback(&grid, "", &scrollback, &mut cache);
        let text = String::from_utf8_lossy(&result);

        assert!(text.contains("\x1b[1;1H"), "row 1 must be redrawn after scrollback");
        assert!(text.contains("\x1b[2;1H"), "row 2 must be redrawn after scrollback");
        assert!(text.contains("\x1b[3;1H"), "row 3 must be redrawn after scrollback");
    }

    #[test]
    fn no_scrollback_no_crlf_in_output() {
        let grid = Grid::new(80, 24, 0);
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "", false, &mut cache);
        // Normal render must never contain \r\n — that's only emitted for scrollback
        assert!(!result.windows(2).any(|w| w == b"\r\n"),
            "render without scrollback must not contain \\r\\n");
    }

    /// Simulates the reattach scenario: history lines are written by the
    /// client with `\r\n`, leaving the last `rows - 1` lines on screen.
    /// The server prepends exactly `rows - 1` newlines to the ScreenUpdate
    /// to flush them into the real terminal's scrollback buffer.
    ///
    /// If too few `\n`s are sent, some history lines are lost (cleared by
    /// `\x1b[2J`).  If too many, a blank line leaks into the scrollback.
    #[test]
    fn reattach_history_flush_count() {
        let rows: u16 = 5;
        let grid = Grid::new(80, rows, 0);
        let mut cache = RenderCache::new();
        let render = render_screen(&grid, "", true, &mut cache);

        // Build the ScreenUpdate data the same way send_initial_state does:
        // prepend (rows - 1) newlines, then the full render.
        let mut reattach_data = Vec::new();
        let flush_count = rows.saturating_sub(1) as usize;
        reattach_data.extend(std::iter::repeat(b'\n').take(flush_count));
        reattach_data.extend_from_slice(&render);

        // Count leading \n bytes before any escape sequence
        let leading_newlines = reattach_data.iter().take_while(|&&b| b == b'\n').count();
        assert_eq!(leading_newlines, (rows - 1) as usize,
            "reattach should prepend exactly rows-1 newlines, got {}", leading_newlines);

        // The render portion must still start with sync begin
        assert_eq!(&reattach_data[flush_count..flush_count + 8], b"\x1b[?2026h",
            "render must start with synchronized output after flush newlines");
    }

    #[test]
    fn reattach_no_flush_without_history() {
        let grid = Grid::new(80, 5, 0);
        let mut cache = RenderCache::new();
        let render = render_screen(&grid, "", true, &mut cache);
        // Without history, no leading newlines should be added
        assert_eq!(render[0], b'\x1b',
            "render without history must start directly with escape, not newline");
    }

    // --- BEL-in-render tests ---

    /// Every BEL (0x07) in render output must be inside an OSC sequence,
    /// never a standalone bell that would trigger an audible beep.
    #[test]
    fn render_no_standalone_bell() {
        let grid = Grid::new(80, 24, 0);
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "My Title", true, &mut cache);
        // Find all BEL bytes and verify each is preceded by an OSC intro
        for (i, &byte) in result.iter().enumerate() {
            if byte == 0x07 {
                // This BEL must be a terminator for an OSC sequence.
                // Scan backward to find \x1b] (ESC ])
                let prefix = &result[..i];
                let osc_start = prefix.windows(2).rposition(|w| w == b"\x1b]");
                assert!(osc_start.is_some(),
                    "BEL at byte offset {} is standalone (not inside an OSC sequence)", i);
            }
        }
    }

    /// Full redraw should not produce standalone BEL even with title changes.
    #[test]
    fn render_full_redraw_no_standalone_bell() {
        let grid = Grid::new(80, 24, 0);
        let mut cache = RenderCache::new();
        // First render with title
        let _ = render_screen(&grid, "Title1", false, &mut cache);
        // Full redraw with different title
        cache.invalidate();
        let result = render_screen(&grid, "Title2", true, &mut cache);
        for (i, &byte) in result.iter().enumerate() {
            if byte == 0x07 {
                let prefix = &result[..i];
                let osc_start = prefix.windows(2).rposition(|w| w == b"\x1b]");
                assert!(osc_start.is_some(),
                    "BEL at byte offset {} is standalone after cache invalidate", i);
            }
        }
    }

    /// Render without title should produce zero BEL bytes.
    #[test]
    fn render_no_title_no_bell_bytes() {
        let grid = Grid::new(80, 24, 0);
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "", false, &mut cache);
        let bell_count = result.iter().filter(|&&b| b == 0x07).count();
        assert_eq!(bell_count, 0,
            "render with empty title should produce zero BEL bytes, got {}", bell_count);
    }

    /// Repeated renders with the same title should not produce BEL on second render.
    #[test]
    fn render_cached_title_no_bell() {
        let grid = Grid::new(80, 24, 0);
        let mut cache = RenderCache::new();
        let _ = render_screen(&grid, "Hello", false, &mut cache);
        // Second render with same title — should skip title OSC entirely
        let result = render_screen(&grid, "Hello", false, &mut cache);
        let bell_count = result.iter().filter(|&&b| b == 0x07).count();
        assert_eq!(bell_count, 0,
            "cached title should produce zero BEL bytes, got {}", bell_count);
    }

    // --- AnsiRenderer trait-path dirty tracking tests ---

    #[test]
    fn trait_render_incremental_skips_unchanged_rows() {
        use super::super::Screen;
        use super::super::traits::{TerminalEmulator, TerminalRenderer};

        let mut screen = Screen::new(10, 5, 0);
        // Write text on row 0
        screen.process(b"Hello");
        // Move cursor to row 3 col 3 so CUP doesn't collide with row positions
        screen.process(b"\x1b[4;4H");

        let mut renderer = AnsiRenderer::new();
        // First render — all rows drawn
        let result1 = renderer.render(&screen, false);
        let text1 = String::from_utf8_lossy(&result1);
        assert!(text1.contains("\x1b[1;1H"), "first render should draw row 1");
        assert!(text1.contains("\x1b[2;1H"), "first render should draw row 2");
        assert!(text1.contains("\x1b[3;1H"), "first render should draw row 3");

        // Second render without changes — content rows should be skipped
        let result2 = renderer.render(&screen, false);
        let text2 = String::from_utf8_lossy(&result2);
        assert!(!text2.contains("\x1b[1;1H"),
            "unchanged row 1 should be skipped in trait incremental render");
        assert!(!text2.contains("\x1b[2;1H"),
            "unchanged row 2 should be skipped in trait incremental render");
    }

    #[test]
    fn trait_render_incremental_redraws_changed_row() {
        use super::super::Screen;
        use super::super::traits::{TerminalEmulator, TerminalRenderer};

        let mut screen = Screen::new(10, 5, 0);
        screen.process(b"Hello");
        screen.process(b"\x1b[4;4H");

        let mut renderer = AnsiRenderer::new();
        // First render to populate cache
        let _ = renderer.render(&screen, false);

        // Change row 0 by overwriting
        screen.process(b"\x1b[1;1HWorld");
        screen.process(b"\x1b[4;4H");

        let result = renderer.render(&screen, false);
        let text = String::from_utf8_lossy(&result);
        // Row 1 changed — should be redrawn
        assert!(text.contains("\x1b[1;1H"),
            "changed row should be redrawn via trait path");
        // Row 3 unchanged — should be skipped
        assert!(!text.contains("\x1b[3;1H"),
            "unchanged row 3 should be skipped via trait path");
    }

    #[test]
    fn trait_render_full_redraws_all_rows() {
        use super::super::Screen;
        use super::super::traits::{TerminalEmulator, TerminalRenderer};

        let mut screen = Screen::new(10, 3, 0);
        screen.process(b"Hi");

        let mut renderer = AnsiRenderer::new();
        // Populate cache
        let _ = renderer.render(&screen, false);

        // Full render should redraw everything regardless of cache
        let result = renderer.render(&screen, true);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b[1;1H"), "full render should draw row 1");
        assert!(text.contains("\x1b[2;1H"), "full render should draw row 2");
        assert!(text.contains("\x1b[3;1H"), "full render should draw row 3");
    }

    // ====================================================================
    // Rendering optimality tests
    //
    // These tests verify that the renderer avoids unnecessary work:
    // redundant escape sequences, no-op renders, and re-rendering of
    // unchanged state. They catch regressions that waste bandwidth
    // or cause visual artifacts (flicker).
    // ====================================================================

    /// Helper: count occurrences of a byte pattern in a byte slice.
    fn count_pattern(haystack: &[u8], needle: &[u8]) -> usize {
        haystack.windows(needle.len()).filter(|w| *w == needle).count()
    }

    // --- No-op render detection ---

    #[test]
    fn noop_render_returns_empty() {
        let grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        // First render populates the cache
        let _ = render_screen(&grid, "", false, &mut cache);
        // Second render with nothing changed: must return empty
        let result = render_screen(&grid, "", false, &mut cache);
        assert!(result.is_empty(),
            "no-op render should return empty, got {} bytes: {:?}",
            result.len(), String::from_utf8_lossy(&result));
    }

    #[test]
    fn noop_render_trait_returns_empty() {
        use super::super::Screen;
        use super::super::traits::{TerminalEmulator, TerminalRenderer};

        let mut screen = Screen::new(10, 3, 0);
        screen.process(b"Test");
        screen.process(b"\x1b[2;3H");

        let mut renderer = AnsiRenderer::new();
        let _ = renderer.render(&screen, false);
        // Second render — nothing changed
        let result = renderer.render(&screen, false);
        assert!(result.is_empty(),
            "trait path no-op render should return empty, got {} bytes",
            result.len());
    }

    #[test]
    fn noop_render_after_cursor_only_change() {
        let mut grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        let _ = render_screen(&grid, "", false, &mut cache);
        // Move cursor — this should produce output (CUP)
        grid.set_cursor_x_unclamped(5);
        let result = render_screen(&grid, "", false, &mut cache);
        assert!(!result.is_empty(), "cursor move should produce output");
        // Render again without change — should be no-op
        let result2 = render_screen(&grid, "", false, &mut cache);
        assert!(result2.is_empty(),
            "second render after cursor-only change should be no-op");
    }

    // --- Cursor position caching ---

    #[test]
    fn cursor_position_cached_across_renders() {
        let mut grid = Grid::new(10, 5, 0);
        grid.set_cursor_x_unclamped(3);
        grid.set_cursor_y_unclamped(2);
        let mut cache = RenderCache::new();
        let _ = render_screen(&grid, "", false, &mut cache);
        // Same cursor position — CUP should not be re-emitted
        let result = render_screen(&grid, "", false, &mut cache);
        assert!(result.is_empty(),
            "same cursor position should not produce output");
        // Change cursor position — CUP should be emitted
        grid.set_cursor_x_unclamped(7);
        let result = render_screen(&grid, "", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b[3;8H"),
            "new cursor position should emit CUP, got: {:?}", text);
    }

    #[test]
    fn cursor_position_always_emitted_on_full() {
        let mut grid = Grid::new(10, 3, 0);
        grid.set_cursor_x_unclamped(2);
        grid.set_cursor_y_unclamped(1);
        let mut cache = RenderCache::new();
        let _ = render_screen(&grid, "", false, &mut cache);
        // Full render with same cursor — CUP must still be emitted
        let result = render_screen(&grid, "", true, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b[2;3H"),
            "full render must always emit CUP even if cached");
    }

    // --- Cursor visibility caching ---

    #[test]
    fn cursor_visibility_cached() {
        let grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        // First render — cursor visible, emitted
        let result = render_screen(&grid, "", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b[?25h"),
            "first render should emit cursor show");
        // Second render — same visibility, should be no-op
        let result2 = render_screen(&grid, "", false, &mut cache);
        assert!(!String::from_utf8_lossy(&result2).contains("\x1b[?25h"),
            "cached cursor visibility should not be re-emitted");
    }

    #[test]
    fn cursor_visibility_change_detected() {
        let mut grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        let _ = render_screen(&grid, "", false, &mut cache);
        // Hide cursor
        grid.set_cursor_visible(false);
        let result = render_screen(&grid, "", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b[?25l"),
            "cursor hide should be emitted when visibility changes");
        // Render again — same state, should not re-emit
        let result2 = render_screen(&grid, "", false, &mut cache);
        assert!(result2.is_empty(),
            "same cursor visibility should produce no-op render");
    }

    // --- Scroll region caching ---

    #[test]
    fn scroll_region_cached() {
        let grid = Grid::new(10, 5, 0);
        let mut cache = RenderCache::new();
        let result1 = render_screen(&grid, "", false, &mut cache);
        let text1 = String::from_utf8_lossy(&result1);
        assert!(text1.contains("r"), "first render should emit DECSTBM");
        // Second render — same scroll region
        let result2 = render_screen(&grid, "", false, &mut cache);
        assert!(result2.is_empty(),
            "same scroll region should produce no-op render");
    }

    #[test]
    fn scroll_region_change_emitted() {
        let mut grid = Grid::new(10, 10, 0);
        let mut cache = RenderCache::new();
        let _ = render_screen(&grid, "", false, &mut cache);
        // Change scroll region to rows 2-8 (0-indexed: 1-7)
        grid.set_scroll_region(1, 7);
        let result = render_screen(&grid, "", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b[2;8r"),
            "changed scroll region should emit DECSTBM");
    }

    // --- Mode delta encoding ---

    #[test]
    fn modes_delta_skips_unchanged() {
        let mut grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        // First render establishes baseline modes
        let _ = render_screen(&grid, "", false, &mut cache);
        // Change only bracketed paste
        grid.modes_mut().bracketed_paste = true;
        let result = render_screen(&grid, "", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        // Should have bracketed paste enable
        assert!(text.contains("\x1b[?2004h"),
            "changed mode should be emitted");
        // Should NOT re-emit unchanged modes like cursor shape, autowrap, etc.
        assert!(!text.contains(" q"),
            "unchanged cursor shape should not be re-emitted in delta");
        assert!(!text.contains("\x1b[?7"),
            "unchanged autowrap should not be re-emitted in delta");
    }

    #[test]
    fn modes_full_always_emitted() {
        let grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        let _ = render_screen(&grid, "", false, &mut cache);
        // Full render with no mode changes — all modes must still be emitted
        let result = render_screen(&grid, "", true, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains(" q"), "full render must emit cursor shape");
        assert!(text.contains("\x1b[?7"), "full render must emit autowrap");
    }

    // --- Title caching ---

    #[test]
    fn title_not_reemitted_when_unchanged() {
        let grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        let _ = render_screen(&grid, "MyApp", false, &mut cache);
        // Same title — should not be in output
        let result = render_screen(&grid, "MyApp", false, &mut cache);
        assert_eq!(count_pattern(&result, b"\x1b]2;"), 0,
            "unchanged title should not produce OSC");
    }

    #[test]
    fn title_emitted_when_changed() {
        let grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        let _ = render_screen(&grid, "OldTitle", false, &mut cache);
        let result = render_screen(&grid, "NewTitle", false, &mut cache);
        assert_eq!(count_pattern(&result, b"\x1b]2;"), 1,
            "changed title should emit exactly one OSC");
        assert!(result.windows(8).any(|w| w == b"NewTitle"),
            "new title should be in output");
    }

    // --- Incremental render byte efficiency ---

    #[test]
    fn incremental_render_smaller_than_full() {
        let mut grid = Grid::new(80, 24, 0);
        // Fill several rows with text
        for i in 0..24 {
            grid.visible_row_mut(i)[0].c = 'A';
        }
        let mut cache = RenderCache::new();
        // Full render
        let full = render_screen(&grid, "Title", true, &mut cache);
        // Change only one row
        grid.visible_row_mut(5)[1].c = 'B';
        let incr = render_screen(&grid, "Title", false, &mut cache);
        assert!(incr.len() < full.len(),
            "incremental single-row change ({} bytes) should be smaller than full ({} bytes)",
            incr.len(), full.len());
    }

    #[test]
    fn incremental_no_screen_clear() {
        let grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        let _ = render_screen(&grid, "", false, &mut cache);
        // Change one cell
        let mut grid2 = grid;
        grid2.visible_row_mut(0)[0].c = 'X';
        let result = render_screen(&grid2, "", false, &mut cache);
        assert_eq!(count_pattern(&result, b"\x1b[2J"), 0,
            "incremental render must never emit screen clear");
    }

    // --- Sync block structure ---

    #[test]
    fn noop_render_no_sync_block() {
        let grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        let _ = render_screen(&grid, "", false, &mut cache);
        let result = render_screen(&grid, "", false, &mut cache);
        // No-op should not even start a sync block
        assert_eq!(count_pattern(&result, b"\x1b[?2026h"), 0,
            "no-op render should not emit sync begin");
        assert_eq!(count_pattern(&result, b"\x1b[?2026l"), 0,
            "no-op render should not emit sync end");
    }

    #[test]
    fn non_noop_render_has_sync_block() {
        let mut grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        let _ = render_screen(&grid, "", false, &mut cache);
        grid.visible_row_mut(0)[0].c = 'X';
        let result = render_screen(&grid, "", false, &mut cache);
        assert_eq!(count_pattern(&result, b"\x1b[?2026h"), 1,
            "non-noop render must have exactly one sync begin");
        assert_eq!(count_pattern(&result, b"\x1b[?2026l"), 1,
            "non-noop render must have exactly one sync end");
    }

    // --- Full redraw does NOT emit per-row EL ---

    #[test]
    fn full_render_no_erase_line() {
        let mut grid = Grid::new(10, 3, 0);
        grid.visible_row_mut(0)[0].c = 'A';
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "", true, &mut cache);
        // Full render clears screen with \x1b[2J so per-row \x1b[K is redundant
        assert_eq!(count_pattern(&result, b"\x1b[K"), 0,
            "full render should not emit per-row erase (screen already cleared)");
    }

    #[test]
    fn incremental_render_uses_erase_line() {
        let mut grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        let _ = render_screen(&grid, "", false, &mut cache);
        grid.visible_row_mut(0)[0].c = 'A';
        let result = render_screen(&grid, "", false, &mut cache);
        assert!(count_pattern(&result, b"\x1b[K") >= 1,
            "incremental render should use erase-to-EOL for changed rows");
    }

    // --- Trait path optimality parity ---

    #[test]
    fn trait_noop_render_no_sync_block() {
        use super::super::Screen;
        use super::super::traits::{TerminalEmulator, TerminalRenderer};

        let mut screen = Screen::new(10, 3, 0);
        screen.process(b"Hi");

        let mut renderer = AnsiRenderer::new();
        let _ = renderer.render(&screen, false);
        let result = renderer.render(&screen, false);
        assert!(result.is_empty(), "trait no-op should be empty");
        assert_eq!(count_pattern(&result, b"\x1b[?2026h"), 0,
            "trait no-op should not emit sync begin");
    }

    #[test]
    fn trait_cursor_position_cached() {
        use super::super::Screen;
        use super::super::traits::{TerminalEmulator, TerminalRenderer};

        let mut screen = Screen::new(10, 5, 0);
        screen.process(b"\x1b[3;4H"); // cursor at row 3, col 4

        let mut renderer = AnsiRenderer::new();
        let result1 = renderer.render(&screen, false);
        let text1 = String::from_utf8_lossy(&result1);
        assert!(text1.contains("\x1b[3;4H"), "first render should emit CUP");

        // Same position — no CUP
        let result2 = renderer.render(&screen, false);
        assert!(result2.is_empty(),
            "trait path: same cursor position should produce no-op");

        // Move cursor — CUP emitted
        screen.process(b"\x1b[1;1H");
        let result3 = renderer.render(&screen, false);
        let text3 = String::from_utf8_lossy(&result3);
        assert!(text3.contains("\x1b[1;1H"),
            "trait path: new cursor position should emit CUP");
    }

    #[test]
    fn trait_mode_delta_only_changed() {
        use super::super::Screen;
        use super::super::traits::{TerminalEmulator, TerminalRenderer};

        let mut screen = Screen::new(10, 3, 0);
        let mut renderer = AnsiRenderer::new();
        let _ = renderer.render(&screen, false);

        // Enable bracketed paste
        screen.process(b"\x1b[?2004h");
        let result = renderer.render(&screen, false);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b[?2004h"), "changed mode should be emitted");
        // Should not re-emit all modes
        assert!(!text.contains(" q"), "unchanged cursor shape should be skipped");
    }

    #[test]
    fn trait_title_cached() {
        use super::super::Screen;
        use super::super::traits::{TerminalEmulator, TerminalRenderer};

        let mut screen = Screen::new(10, 3, 0);
        screen.process(b"\x1b]2;AppTitle\x07");

        let mut renderer = AnsiRenderer::new();
        let result1 = renderer.render(&screen, false);
        assert!(count_pattern(&result1, b"\x1b]2;") == 1, "first render emits title");

        let result2 = renderer.render(&screen, false);
        assert!(result2.is_empty(), "same title should produce no-op");

        // Change title
        screen.process(b"\x1b]2;NewTitle\x07");
        let result3 = renderer.render(&screen, false);
        assert!(count_pattern(&result3, b"\x1b]2;") == 1, "changed title emitted");
    }

    #[test]
    fn trait_incremental_smaller_than_full() {
        use super::super::Screen;
        use super::super::traits::{TerminalEmulator, TerminalRenderer};

        let mut screen = Screen::new(80, 24, 0);
        for i in 0..24u8 {
            screen.process(format!("\x1b[{};1H{}", i + 1, (b'A' + i) as char).as_bytes());
        }

        let mut renderer = AnsiRenderer::new();
        let full = renderer.render(&screen, true);

        // Change one row
        screen.process(b"\x1b[5;1HCHANGED");
        let incr = renderer.render(&screen, false);
        assert!(incr.len() < full.len(),
            "trait incremental ({} bytes) should be smaller than full ({} bytes)",
            incr.len(), full.len());
    }
}
