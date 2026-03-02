use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;

use super::cell::Cell;
use super::grid::{Grid, TerminalModes};
use super::style::{Style, write_u16};

/// Per-connection render cache for dirty tracking and mode delta.
pub struct RenderCache {
    row_hashes: Vec<u64>,
    last_modes: Option<TerminalModes>,
    last_title: String,
}

impl RenderCache {
    pub fn new() -> Self {
        Self {
            row_hashes: Vec::new(),
            last_modes: None,
            last_title: String::new(),
        }
    }

    /// Invalidate the cache so the next render is a full redraw.
    pub fn invalidate(&mut self) {
        self.row_hashes.clear();
        self.last_modes = None;
        self.last_title.clear();
    }
}

fn hash_row(row: &[Cell]) -> u64 {
    let mut hasher = DefaultHasher::new();
    row.hash(&mut hasher);
    hasher.finish()
}

/// Render a single row of cells as ANSI bytes with SGR codes.
/// Fix C1: returns a single space for fully blank lines so they're not empty.
pub fn render_line(row: &[Cell]) -> Vec<u8> {
    let last_non_space = row.iter()
        .rposition(|c| (c.c != ' ' && c.c != '\0') || !c.style.is_default());

    let last_non_space = match last_non_space {
        Some(pos) => pos,
        None => {
            // Entirely blank line — emit a single space so it's not empty
            return b" ".to_vec();
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

/// Emit escape sequences for one mode, unconditionally.
fn emit_mode(out: &mut Vec<u8>, modes: &TerminalModes) {
    // Cursor shape (DECSCUSR)
    out.extend_from_slice(b"\x1b[");
        out.push(b'0' + modes.cursor_shape.to_param());
        out.extend_from_slice(b" q");

    // Boolean DEC private modes
    emit_dec_mode(out, 1, modes.cursor_key_mode);
    emit_dec_mode(out, 2004, modes.bracketed_paste);

    // Mouse mode: always reset all first, then enable the active one
    out.extend_from_slice(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l");
    if modes.mouse_mode != 0 {
        emit_dec_mode(out, modes.mouse_mode, true);
    }

    // Mouse encoding
    emit_mouse_encoding(out, modes.mouse_encoding);

    emit_dec_mode(out, 1004, modes.focus_reporting);

    // Keypad mode (not DEC private — uses ESC = / ESC >)
    out.extend_from_slice(if modes.keypad_app_mode { b"\x1b=" } else { b"\x1b>" });
}

/// Emit mouse encoding sequence for the given encoding mode.
fn emit_mouse_encoding(out: &mut Vec<u8>, encoding: u16) {
    match encoding {
        1006 => { out.extend_from_slice(b"\x1b[?1005l"); out.extend_from_slice(b"\x1b[?1006h"); }
        1005 => { out.extend_from_slice(b"\x1b[?1006l"); out.extend_from_slice(b"\x1b[?1005h"); }
        _ => out.extend_from_slice(b"\x1b[?1006l\x1b[?1005l"),
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
    if modes.bracketed_paste != prev.bracketed_paste {
        emit_dec_mode(out, 2004, modes.bracketed_paste);
    }
    if modes.mouse_mode != prev.mouse_mode {
        // Disable the old mode first, then enable the new one
        if prev.mouse_mode != 0 {
            emit_dec_mode(out, prev.mouse_mode, false);
        }
        if modes.mouse_mode != 0 {
            emit_dec_mode(out, modes.mouse_mode, true);
        }
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
}

/// Render the full screen grid as ANSI bytes.
/// If `full` is true, clears screen first (used on initial attach).
/// Otherwise uses dirty tracking to skip unchanged rows.
pub fn render_screen(grid: &Grid, title: &str, full: bool, cache: &mut RenderCache) -> Vec<u8> {
    let mut out = Vec::new();
    // Synchronized output: begin
    out.extend_from_slice(b"\x1b[?2026h");
    // Hide cursor during redraw
    out.extend_from_slice(b"\x1b[?25l");

    if full {
        // Reset SGR before clearing to prevent leftover background color from
        // filling the screen (e.g. after history output with styled lines).
        out.extend_from_slice(b"\x1b[0m\x1b[2J\x1b[H");
        cache.invalidate();
    }

    // Ensure cache row_hashes is the right length
    let num_rows = grid.cells.len();
    if cache.row_hashes.len() != num_rows {
        cache.row_hashes.resize(num_rows, u64::MAX); // sentinel: won't match any real hash
    }

    for (y, row) in grid.cells.iter().enumerate() {
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
        }

        // Reset style + erase to end of line (clears any leftover from previous content)
        out.extend_from_slice(b"\x1b[0m\x1b[K");
    }

    // Cursor position
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
    use super::super::grid::CursorShape;
    use super::super::style::Color;

    #[test]
    fn render_line_blank() {
        let row = vec![Cell::default(); 80];
        let result = render_line(&row);
        assert_eq!(result, b" ");
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
        let grid = Grid::new(10, 3);
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
        let grid = Grid::new(10, 3);
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        // Should NOT contain clear screen
        assert!(!text.contains("\x1b[2J"));
    }

    #[test]
    fn render_line_skips_wide_char_continuation() {
        let mut row = vec![Cell::default(); 10];
        row[0] = Cell { c: '你', style: Style::default(), width: 2 };
        row[1] = Cell { c: '\0', style: Style::default(), width: 0 };
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
        let grid = Grid::new(10, 3);
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "My Title", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b]2;My Title\x07"));
    }

    #[test]
    fn render_screen_no_title_when_empty() {
        let grid = Grid::new(10, 3);
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(!text.contains("\x1b]2;"));
    }

    #[test]
    fn render_screen_hidden_cursor() {
        let mut grid = Grid::new(10, 3);
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
        let mut grid = Grid::new(10, 5);
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
        grid.cells[1][0].c = 'X';
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
        let grid = Grid::new(10, 3);
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
        let mut grid = Grid::new(10, 5);
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
        let grid = Grid::new(10, 3);
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
        let grid = Grid::new(10, 3);
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
    fn render_screen_full_mode_resets_all_mouse_modes() {
        let mut grid = Grid::new(10, 3);
        grid.modes.mouse_mode = 1003;
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "", true, &mut cache);
        let text = String::from_utf8_lossy(&result);
        // Full render should reset all mouse modes first
        assert!(text.contains("\x1b[?1000l"),
            "full render should reset mouse mode 1000");
        assert!(text.contains("\x1b[?1002l"),
            "full render should reset mouse mode 1002");
        assert!(text.contains("\x1b[?1003l"),
            "full render should reset mouse mode 1003");
        // Then enable the active one
        assert!(text.contains("\x1b[?1003h"),
            "full render should enable active mouse mode 1003");
    }

    #[test]
    fn render_screen_mode_delta_mouse_switch() {
        let mut grid = Grid::new(10, 3);
        grid.modes.mouse_mode = 1000;
        let mut cache = RenderCache::new();
        // Initial render (full)
        let _ = render_screen(&grid, "", true, &mut cache);

        // Switch mouse mode from 1000 to 1003
        grid.modes.mouse_mode = 1003;
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
        let mut grid = Grid::new(10, 3);
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
        let grid = Grid::new(10, 3);
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
        row[8] = Cell { c: '\u{4e16}', style: Style::default(), width: 2 }; // 世
        row[9] = Cell { c: '\0', style: Style::default(), width: 0 }; // continuation
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
        let grid = Grid::new(10, 3);
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
        let grid = Grid::new(10, 3);
        let mut cache = RenderCache::new();
        let _ = render_screen(&grid, "", false, &mut cache);

        // Simulate resize: new grid with more rows
        let grid2 = Grid::new(10, 5);
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
        let mut grid = Grid::new(10, 3);
        let mut cache = RenderCache::new();
        let _ = render_screen(&grid, "", false, &mut cache);

        // Change style of a cell without changing the char
        grid.cells[1][0].style.fg = Some(Color::Indexed(1));
        let result = render_screen(&grid, "", false, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b[2;1H"),
            "row with style-only change should be redrawn");
    }

    #[test]
    fn render_screen_1x1_grid() {
        let grid = Grid::new(1, 1);
        let mut cache = RenderCache::new();
        // Should not panic
        let result = render_screen(&grid, "", true, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b[1;1H"), "1x1 grid should position at 1,1");
    }

    #[test]
    fn render_screen_cursor_bottom_right() {
        let mut grid = Grid::new(80, 24);
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
        let mut grid = Grid::new(10, 3);
        grid.modes.mouse_encoding = 1006;
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "", true, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b[?1006h"), "SGR mouse encoding should be enabled");
    }

    #[test]
    fn render_screen_mouse_encoding_1005() {
        let mut grid = Grid::new(10, 3);
        grid.modes.mouse_encoding = 1005;
        let mut cache = RenderCache::new();
        let result = render_screen(&grid, "", true, &mut cache);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b[?1005h"), "UTF-8 mouse encoding should be enabled");
    }

    #[test]
    fn render_screen_cursor_shape_delta() {
        let mut grid = Grid::new(10, 3);
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
        let mut grid = Grid::new(10, 3);
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
}
