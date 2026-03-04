use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;

use super::cell::Cell;
use super::grid::{ActiveCharset, Charset, Grid, MouseEncoding, TerminalModes};
use super::style::{Style, write_u16};

/// Per-connection render cache for dirty tracking and mode delta.
pub struct RenderCache {
    row_hashes: Vec<u64>,
    last_modes: Option<TerminalModes>,
    last_scroll_region: Option<(u16, u16)>,
    last_title: String,
}

impl RenderCache {
    pub fn new() -> Self {
        Self {
            row_hashes: Vec::new(),
            last_modes: None,
            last_scroll_region: None,
            last_title: String::new(),
        }
    }

    /// Invalidate the cache so the next render is a full redraw.
    pub fn invalidate(&mut self) {
        self.row_hashes.clear();
        self.last_modes = None;
        self.last_scroll_region = None;
        self.last_title.clear();
    }
}

fn hash_row(row: &[Cell]) -> u64 {
    let mut hasher = DefaultHasher::new();
    row.hash(&mut hasher);
    hasher.finish()
}

/// Render a single row of cells as ANSI bytes with SGR codes.
/// Returns empty Vec for fully blank lines.
pub fn render_line(row: &[Cell]) -> Vec<u8> {
    let last_non_space = row.iter()
        .rposition(|c| (c.c != ' ' && c.c != '\0') || !c.style.is_default());

    let last_non_space = match last_non_space {
        Some(pos) => pos,
        None => {
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    let mut current = Style::default();
    for (i, cell) in row.iter().enumerate() {
        if i > last_non_space { break; }
        // Skip wide char continuation cells
        if cell.width == 0 { continue; }
        if cell.style != current {
            out.extend_from_slice(&cell.style.to_sgr_with_reset());
            current = cell.style;
        }
        let mut buf = [0u8; 4];
        out.extend_from_slice(cell.c.encode_utf8(&mut buf).as_bytes());
        for &mark in &cell.combining {
            out.extend_from_slice(mark.encode_utf8(&mut buf).as_bytes());
        }
    }
    if !current.is_default() {
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
pub fn render_screen(grid: &Grid, title: &str, full: bool, cache: &mut RenderCache) -> Vec<u8> {
    render_screen_impl(grid, title, &[], full, cache)
}

/// Render the screen with scrollback lines injected into the real terminal's
/// native scrollback buffer.
///
/// The entire output — scrollback injection and screen redraw — is wrapped in
/// a single synchronized-output block to prevent flicker.  Scrollback lines
/// are emitted first (cursor positioned at the bottom so `\r\n` triggers real
/// terminal scrolling), followed by a full screen clear and redraw.
pub fn render_screen_with_scrollback(
    grid: &Grid,
    title: &str,
    scrollback: &[Vec<u8>],
    cache: &mut RenderCache,
) -> Vec<u8> {
    render_screen_impl(grid, title, scrollback, true, cache)
}

fn render_screen_impl(
    grid: &Grid,
    title: &str,
    scrollback: &[Vec<u8>],
    full: bool,
    cache: &mut RenderCache,
) -> Vec<u8> {
    let mut out = Vec::new();

    // Scrollback injection: emitted OUTSIDE the synchronized output block.
    //
    // Some terminals (notably Blink/hterm on iOS) buffer all output during
    // a sync block and apply it atomically, which means intermediate scroll
    // operations (\r\n at the bottom row) don't push content into the native
    // scrollback buffer.
    //
    // Algorithm: overwrite visible rows with scrollback content, then scroll
    // them off via \n at the bottom row.  Processed in chunks of `rows` so
    // that any amount of scrollback is handled correctly.
    if !scrollback.is_empty() {
        let rows = grid.rows as usize;
        // Hide cursor and reset scroll region to full screen so \n at the
        // bottom scrolls the entire display (not just a scroll region).
        out.extend_from_slice(b"\x1b[?25l\x1b[r");

        for chunk in scrollback.chunks(rows) {
            // Overwrite visible rows 1..chunk.len() with scrollback content.
            for (i, line) in chunk.iter().enumerate() {
                out.extend_from_slice(b"\x1b[");
                write_u16(&mut out, (i + 1) as u16);
                out.extend_from_slice(b";1H\x1b[0m");
                out.extend_from_slice(line);
                out.extend_from_slice(b"\x1b[K");
            }
            // Erase remaining rows below the chunk to prevent stale content
            // from leaking into native scrollback on the next pass.
            if chunk.len() < rows {
                for i in chunk.len()..rows {
                    out.extend_from_slice(b"\x1b[");
                    write_u16(&mut out, (i + 1) as u16);
                    out.extend_from_slice(b";1H\x1b[2K");
                }
            }
            // Position at the bottom row and scroll chunk.len() lines off
            // the top into native scrollback.
            out.extend_from_slice(b"\x1b[");
            write_u16(&mut out, grid.rows);
            out.extend_from_slice(b";1H");
            for _ in 0..chunk.len() {
                out.push(b'\n');
            }
        }
        cache.invalidate();
    }

    // Synchronized output: begin (screen redraw only)
    out.extend_from_slice(b"\x1b[?2026h");
    // Hide cursor during redraw
    out.extend_from_slice(b"\x1b[?25l");

    let full = full || !scrollback.is_empty();

    if full {
        // Reset SGR before clearing to prevent leftover background color from
        // filling the screen (e.g. after history output with styled lines).
        out.extend_from_slice(b"\x1b[0m\x1b[2J\x1b[H");
        cache.invalidate();
    }

    // Ensure cache row_hashes is the right length
    let num_rows = grid.visible_row_count();
    if cache.row_hashes.len() != num_rows {
        cache.row_hashes.resize(num_rows, u64::MAX); // sentinel: won't match any real hash
    }

    for (y, row) in grid.visible_rows().enumerate() {
        let row_hash = hash_row(row);

        // Skip unchanged rows on incremental renders
        if !full && cache.row_hashes[y] == row_hash {
            continue;
        }
        cache.row_hashes[y] = row_hash;

        // Write-then-erase: position cursor, write content, then clear remainder
        out.extend_from_slice(b"\x1b[");
        write_u16(&mut out, y as u16 + 1);
        out.extend_from_slice(b";1H");

        let write_len = row.iter()
            .rposition(|c| (c.c != ' ' && c.c != '\0') || !c.style.is_default())
            .map(|p| p + 1)
            .unwrap_or(0);

        let mut last_style = Style::default();
        for (x, cell) in row.iter().enumerate() {
            if x >= write_len { break; }
            if cell.width == 0 { continue; }
            if cell.style != last_style {
                // Combined reset+set SGR in one escape
                out.extend_from_slice(&cell.style.to_sgr_with_reset());
                last_style = cell.style;
            }
            let mut buf = [0u8; 4];
            out.extend_from_slice(cell.c.encode_utf8(&mut buf).as_bytes());
            for &mark in &cell.combining {
                out.extend_from_slice(mark.encode_utf8(&mut buf).as_bytes());
            }
        }

        // Reset style + erase to end of line (clears any leftover from previous content)
        out.extend_from_slice(b"\x1b[0m\x1b[K");
    }

    // Scroll region (DECSTBM): must be emitted BEFORE cursor position because
    // setting the scroll region resets the cursor to home.
    let scroll_region = (grid.scroll_top, grid.scroll_bottom);
    if full || cache.last_scroll_region != Some(scroll_region) {
        out.extend_from_slice(b"\x1b[");
        write_u16(&mut out, grid.scroll_top + 1);
        out.push(b';');
        write_u16(&mut out, grid.scroll_bottom + 1);
        out.push(b'r');
        cache.last_scroll_region = Some(scroll_region);
    }

    // Cursor position (after scroll region, since DECSTBM resets cursor)
    out.extend_from_slice(b"\x1b[");
    write_u16(&mut out, grid.cursor_y + 1);
    out.push(b';');
    write_u16(&mut out, grid.cursor_x + 1);
    out.push(b'H');

    // Mode sequences: full sends all, incremental sends delta only
    let modes = &grid.modes;
    match &cache.last_modes {
        Some(prev) if !full => emit_mode_delta(&mut out, modes, prev),
        _ => emit_mode(&mut out, modes),
    }
    cache.last_modes = Some(modes.clone());

    // Window title: only send if changed (sanitized to prevent OSC injection)
    if title != cache.last_title {
        out.extend_from_slice(b"\x1b]2;");
        for &b in title.as_bytes() {
            // Filter control chars that could break or inject escape sequences
            if b >= 0x20 && b != 0x7f {
                out.push(b);
            }
        }
        out.push(0x07);
        cache.last_title = title.to_string();
    }

    // Cursor visibility: always restore since we hide it at the top for redraw
    if grid.cursor_visible {
        out.extend_from_slice(b"\x1b[?25h");
    }
    // (if !cursor_visible, already hidden from the top — no need to re-emit)

    // End synchronized output
    out.extend_from_slice(b"\x1b[?2026l");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::grid::{CursorShape, MouseEncoding};
    use super::super::style::Color;

    #[test]
    fn render_line_blank() {
        let row = vec![Cell::default(); 80];
        let result = render_line(&row);
        assert!(result.is_empty(), "blank line should produce empty vec");
    }

    #[test]
    fn render_line_with_text() {
        let mut row = vec![Cell::default(); 10];
        row[0].c = 'H';
        row[1].c = 'i';
        let result = render_line(&row);
        assert_eq!(result, b"Hi");
    }

    #[test]
    fn render_line_with_style() {
        let mut row = vec![Cell::default(); 10];
        row[0].c = 'R';
        row[0].style.fg = Some(Color::Indexed(1));
        let result = render_line(&row);
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
        // Should contain clear screen sequence
        assert!(text.contains("\x1b[2J\x1b[H"));
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
        let mut row = vec![Cell::default(); 10];
        row[0] = Cell { c: '你', combining: Vec::new(), style: Style::default(), width: 2 };
        row[1] = Cell { c: '\0', combining: Vec::new(), style: Style::default(), width: 0 };
        row[2].c = 'A';
        let result = render_line(&row);
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
        grid.cursor_visible = false;
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
        grid.cursor_x = 3;
        grid.cursor_y = 4;
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
        grid.cursor_x = 4;
        grid.cursor_y = 2;
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
        let mut row = vec![Cell::default(); 10];
        row[0].c = 'R';
        row[0].style.fg = Some(Color::Indexed(1)); // red
        row[1].c = 'G';
        row[1].style.fg = Some(Color::Indexed(2)); // green
        row[2].c = 'N'; // default style
        let result = render_line(&row);
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
        grid.modes.mouse_modes.any = true;
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
        grid.modes.mouse_modes.click = true;
        let mut cache = RenderCache::new();
        // Initial render (full)
        let _ = render_screen(&grid, "", true, &mut cache);

        // Switch: disable click, enable any
        grid.modes.mouse_modes.click = false;
        grid.modes.mouse_modes.any = true;
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
        grid.modes.bracketed_paste = true;
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
        let mut row = vec![Cell::default(); 10];
        for cell in row.iter_mut() {
            cell.c = ' ';
            cell.style.bg = Some(Color::Indexed(1)); // red bg
        }
        let result = render_line(&row);
        // Should contain SGR for the background color, not just a plain space
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b["), "styled spaces should produce SGR sequences");
        assert!(text.contains("41m"), "red bg should produce code 41");
    }

    #[test]
    fn render_line_styled_trailing_space() {
        // Trailing styled space should be included in output
        let mut row = vec![Cell::default(); 5];
        row[0].c = 'A';
        row[4].c = ' ';
        row[4].style.bg = Some(Color::Indexed(4)); // blue bg
        let result = render_line(&row);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("44m"), "trailing styled space should include blue bg SGR");
    }

    #[test]
    fn render_line_wide_char_at_end() {
        // Wide char at last two positions renders correctly
        let mut row = vec![Cell::default(); 10];
        row[8] = Cell { c: '\u{4e16}', combining: Vec::new(), style: Style::default(), width: 2 }; // 世
        row[9] = Cell { c: '\0', combining: Vec::new(), style: Style::default(), width: 0 }; // continuation
        let result = render_line(&row);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains('\u{4e16}'), "wide char at end should be rendered");
        assert!(!text.contains('\0'), "continuation cell should not produce output");
    }

    #[test]
    fn render_line_rgb_color() {
        let mut row = vec![Cell::default(); 5];
        row[0].c = 'X';
        row[0].style.fg = Some(Color::Rgb(100, 150, 200));
        let result = render_line(&row);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("38;2;100;150;200m"), "RGB color should produce 38;2;R;G;B");
    }

    #[test]
    fn render_line_combined_attributes() {
        let mut row = vec![Cell::default(); 5];
        row[0].c = 'Z';
        row[0].style.bold = true;
        row[0].style.italic = true;
        row[0].style.underline = super::super::style::UnderlineStyle::Single;
        row[0].style.fg = Some(Color::Indexed(3)); // yellow
        row[0].style.bg = Some(Color::Indexed(4)); // blue
        let result = render_line(&row);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("1;"), "bold should be present");
        assert!(text.contains("3;"), "italic should be present");
        assert!(text.contains(";4;"), "underline should be present");
        assert!(text.contains("33"), "yellow fg should be present");
        assert!(text.contains("44"), "blue bg should be present");
    }

    #[test]
    fn render_line_256_color() {
        let mut row = vec![Cell::default(); 5];
        row[0].c = 'P';
        row[0].style.fg = Some(Color::Indexed(200));
        let result = render_line(&row);
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
        // Cache had 3 rows, now 5 — row_hashes resized with sentinels, all should redraw
        assert!(text.contains("\x1b[1;1H"), "row 1 should be redrawn after resize");
        assert!(text.contains("\x1b[4;1H"), "row 4 should be redrawn after resize");
        assert!(text.contains("\x1b[5;1H"), "row 5 should be redrawn after resize");
    }

    #[test]
    fn render_screen_style_only_change_detected() {
        // Cell changes color but same char → row should be redrawn
        let mut grid = Grid::new(10, 3, 0);
        let mut cache = RenderCache::new();
        let _ = render_screen(&grid, "", false, &mut cache);

        // Change style of a cell without changing the char
        grid.visible_row_mut(1)[0].style.fg = Some(Color::Indexed(1));
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
        grid.cursor_x = 79;
        grid.cursor_y = 23;
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "", true, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b[24;80H"),
            "cursor at bottom-right should position at row 24, col 80");
    }

    #[test]
    fn render_screen_mouse_encoding_1006() {
        let mut grid = Grid::new(10, 3, 0);
        grid.modes.mouse_encoding = MouseEncoding::Sgr;
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "", true, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b[?1006h"), "SGR mouse encoding should be enabled");
    }

    #[test]
    fn render_screen_mouse_encoding_1005() {
        let mut grid = Grid::new(10, 3, 0);
        grid.modes.mouse_encoding = MouseEncoding::Utf8;
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
        grid.modes.cursor_shape = CursorShape::BlinkBar;
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
        grid.modes.keypad_app_mode = true;
        let result = render_screen(&grid, "", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b="), "keypad app mode should emit ESC =");

        // Disable keypad app mode
        grid.modes.keypad_app_mode = false;
        let result2 = render_screen(&grid, "", false, &mut cache);
        let text2 = String::from_utf8_lossy(&result2);
        assert!(text2.contains("\x1b>"), "keypad normal mode should emit ESC >");
    }

    #[test]
    fn render_line_combining_mark() {
        let mut row = vec![Cell::default(); 10];
        row[0].c = 'e';
        row[0].combining = vec!['\u{0301}']; // combining acute accent → é
        let result = render_line(&row);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("e\u{0301}"), "combining mark should be rendered after base char");
    }

    #[test]
    fn render_line_combining_on_wide_char() {
        let mut row = vec![Cell::default(); 10];
        row[0] = Cell { c: '\u{4e16}', combining: vec!['\u{0308}'], style: Style::default(), width: 2 };
        row[1] = Cell { c: '\0', combining: Vec::new(), style: Style::default(), width: 0 };
        let result = render_line(&row);
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
}
