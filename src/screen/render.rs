use super::cell::Cell;
use super::grid::Grid;
use super::style::Style;

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
            out.extend_from_slice(b"\x1b[0m");
            let sgr = cell.style.to_sgr();
            if !sgr.is_empty() {
                out.extend_from_slice(&sgr);
            }
            current = cell.style.clone();
        }
        let mut buf = [0u8; 4];
        out.extend_from_slice(cell.c.encode_utf8(&mut buf).as_bytes());
    }
    if !current.is_default() {
        out.extend_from_slice(b"\x1b[0m");
    }
    out
}

/// Render the full screen grid as ANSI bytes.
/// If `full` is true, clears screen first (used on initial attach).
/// Otherwise overwrites in place (no flicker).
pub fn render_screen(grid: &Grid, title: &str, full: bool) -> Vec<u8> {
    let mut out = Vec::new();
    // Synchronized output: begin
    out.extend_from_slice(b"\x1b[?2026h");
    // Hide cursor during redraw
    out.extend_from_slice(b"\x1b[?25l");
    if full {
        out.extend_from_slice(b"\x1b[2J\x1b[H");
    }
    for (y, row) in grid.cells.iter().enumerate() {
        out.extend_from_slice(format!("\x1b[{};1H\x1b[0m\x1b[K", y + 1).as_bytes());

        let write_len = row.iter()
            .rposition(|c| (c.c != ' ' && c.c != '\0') || !c.style.is_default())
            .map(|p| p + 1)
            .unwrap_or(0);

        let mut last_style = Style::default();
        for (x, cell) in row.iter().enumerate() {
            if x >= write_len { break; }
            // Skip wide char continuation cells
            if cell.width == 0 { continue; }
            if cell.style != last_style {
                out.extend_from_slice(b"\x1b[0m");
                let sgr = cell.style.to_sgr();
                if !sgr.is_empty() {
                    out.extend_from_slice(&sgr);
                }
                last_style = cell.style.clone();
            }
            let mut buf = [0u8; 4];
            out.extend_from_slice(cell.c.encode_utf8(&mut buf).as_bytes());
        }
    }
    out.extend_from_slice(b"\x1b[0m");
    out.extend_from_slice(
        format!("\x1b[{};{}H", grid.cursor_y + 1, grid.cursor_x + 1).as_bytes(),
    );

    // Emit mode passthrough sequences so the real terminal matches child state
    let modes = &grid.modes;

    // Cursor shape (DECSCUSR)
    if modes.cursor_shape != 0 {
        out.extend_from_slice(format!("\x1b[{} q", modes.cursor_shape).as_bytes());
    } else {
        out.extend_from_slice(b"\x1b[0 q"); // Reset to default
    }

    // Cursor key mode (DECCKM)
    if modes.cursor_key_mode {
        out.extend_from_slice(b"\x1b[?1h");
    } else {
        out.extend_from_slice(b"\x1b[?1l");
    }

    // Bracketed paste
    if modes.bracketed_paste {
        out.extend_from_slice(b"\x1b[?2004h");
    } else {
        out.extend_from_slice(b"\x1b[?2004l");
    }

    // Mouse mode
    if modes.mouse_mode != 0 {
        out.extend_from_slice(format!("\x1b[?{}h", modes.mouse_mode).as_bytes());
    } else {
        // Disable all mouse modes
        out.extend_from_slice(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l");
    }

    // SGR mouse encoding
    if modes.mouse_encoding == 1006 {
        out.extend_from_slice(b"\x1b[?1006h");
    } else {
        out.extend_from_slice(b"\x1b[?1006l");
    }

    // Focus reporting
    if modes.focus_reporting {
        out.extend_from_slice(b"\x1b[?1004h");
    } else {
        out.extend_from_slice(b"\x1b[?1004l");
    }

    // Keypad application mode
    if modes.keypad_app_mode {
        out.extend_from_slice(b"\x1b=");
    } else {
        out.extend_from_slice(b"\x1b>");
    }

    // Window title passthrough
    if !title.is_empty() {
        out.extend_from_slice(b"\x1b]2;");
        out.extend_from_slice(title.as_bytes());
        out.push(0x07); // BEL terminator
    }

    // Restore cursor visibility to match child process state
    if grid.cursor_visible {
        out.extend_from_slice(b"\x1b[?25h");
    }
    // End synchronized output
    out.extend_from_slice(b"\x1b[?2026l");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
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
        // Should have reset, then red SGR, then 'R', then reset
        assert!(result.starts_with(b"\x1b[0m\x1b[31mR"));
        assert!(result.ends_with(b"\x1b[0m"));
    }

    #[test]
    fn render_screen_full() {
        let grid = Grid::new(10, 3);
        let result = render_screen(&grid, "", true);
        let text = String::from_utf8_lossy(&result);
        // Should contain clear screen sequence
        assert!(text.contains("\x1b[2J\x1b[H"));
        // Should contain cursor position
        assert!(text.contains("\x1b[1;1H"));
    }

    #[test]
    fn render_screen_incremental() {
        let grid = Grid::new(10, 3);
        let result = render_screen(&grid, "", false);
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
        let result = render_screen(&grid, "My Title", false);
        let text = String::from_utf8_lossy(&result);
        assert!(text.contains("\x1b]2;My Title\x07"));
    }

    #[test]
    fn render_screen_no_title_when_empty() {
        let grid = Grid::new(10, 3);
        let result = render_screen(&grid, "", false);
        let text = String::from_utf8_lossy(&result);
        assert!(!text.contains("\x1b]2;"));
    }
}
