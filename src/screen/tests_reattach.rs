use super::*;

/// Helper: render a screen with full=true (simulates reattach) and return text.
fn reattach_render(screen: &Screen) -> String {
    let mut cache = RenderCache::new();
    let output = screen.render(true, &mut cache);
    String::from_utf8_lossy(&output).into_owned()
}

/// Helper: extract the CUP sequence (ESC[row;colH) that sets cursor position
/// in the render output. Returns (row, col) as 1-indexed values.
/// Finds the *last* CUP before the mode block (identified by DECSCUSR " q").
fn extract_cursor_cup(rendered: &str) -> (u16, u16) {
    // The cursor position CUP is emitted after all row content and before
    // mode sequences. Find the last ESC[r;cH before the DECSCUSR sequence.
    let mode_pos = rendered.find(" q").unwrap_or(rendered.len());
    let region = &rendered[..mode_pos];
    // Find all CUP sequences (ESC[digits;digitsH)
    let mut last_row = 0u16;
    let mut last_col = 0u16;
    let mut i = 0;
    let bytes = region.as_bytes();
    while i + 2 < bytes.len() {
        if bytes[i] == 0x1b && bytes[i + 1] == b'[' {
            let start = i + 2;
            let mut j = start;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'H' {
                let params = &region[start..j];
                let parts: Vec<&str> = params.split(';').collect();
                if parts.len() == 2 {
                    if let (Ok(r), Ok(c)) = (parts[0].parse::<u16>(), parts[1].parse::<u16>()) {
                        last_row = r;
                        last_col = c;
                    }
                }
            }
        }
        i += 1;
    }
    (last_row, last_col)
}

#[test]
fn reattach_cursor_at_origin() {
    let screen = Screen::new(80, 24, 100);
    let rendered = reattach_render(&screen);
    let (row, col) = extract_cursor_cup(&rendered);
    assert_eq!((row, col), (1, 1),
        "reattach: cursor at origin should render as CUP(1,1)");
}

#[test]
fn reattach_cursor_after_text_input() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"Hello");
    // Cursor should be at column 5 (0-based), row 0
    assert_eq!(screen.grid.cursor_x, 5);
    assert_eq!(screen.grid.cursor_y, 0);
    let rendered = reattach_render(&screen);
    let (row, col) = extract_cursor_cup(&rendered);
    assert_eq!((row, col), (1, 6),
        "reattach: cursor after 'Hello' should be at row 1, col 6 (1-indexed)");
}

#[test]
fn reattach_cursor_after_movement() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[15;40H"); // CUP to row 15, col 40
    let rendered = reattach_render(&screen);
    let (row, col) = extract_cursor_cup(&rendered);
    assert_eq!((row, col), (15, 40),
        "reattach: cursor after CUP(15,40) should be at (15,40)");
}

#[test]
fn reattach_cursor_after_newlines() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"line1\r\nline2\r\nline3");
    // Cursor should be at row 2 (0-based), col 5
    assert_eq!(screen.grid.cursor_y, 2);
    assert_eq!(screen.grid.cursor_x, 5);
    let rendered = reattach_render(&screen);
    let (row, col) = extract_cursor_cup(&rendered);
    assert_eq!((row, col), (3, 6),
        "reattach: cursor after newlines should be at row 3, col 6 (1-indexed)");
}

#[test]
fn reattach_cursor_bottom_right() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[24;80H"); // bottom-right corner
    let rendered = reattach_render(&screen);
    let (row, col) = extract_cursor_cup(&rendered);
    assert_eq!((row, col), (24, 80),
        "reattach: cursor at bottom-right should be at (24,80)");
}

#[test]
fn reattach_cursor_visibility_hidden() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[?25l"); // hide cursor
    let rendered = reattach_render(&screen);
    // Should contain the hide at the top (always emitted)
    assert!(rendered.contains("\x1b[?25l"),
        "reattach: hidden cursor should emit DECTCEM hide");
    // Should NOT contain cursor show
    assert!(!rendered.contains("\x1b[?25h"),
        "reattach: hidden cursor should NOT emit DECTCEM show");
}

#[test]
fn reattach_cursor_visibility_visible() {
    let screen = Screen::new(80, 24, 100);
    // Cursor is visible by default, verify it's restored
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("\x1b[?25h"),
        "reattach: visible cursor should emit DECTCEM show");
}

#[test]
fn reattach_cursor_shape_preserved() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[5 q"); // blinking bar
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("\x1b[5 q"),
        "reattach: cursor shape (blinking bar) should be in render output");
}

#[test]
fn reattach_cursor_shape_steady_block() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[2 q"); // steady block
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("\x1b[2 q"),
        "reattach: cursor shape (steady block) should be in render output");
}

#[test]
fn reattach_cursor_after_save_restore() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[10;20H"); // move to (10, 20)
    screen.process(b"\x1b7");        // save cursor
    screen.process(b"\x1b[1;1H");    // move home
    screen.process(b"\x1b8");        // restore cursor
    // Cursor should be back at (10, 20) → 0-based (9, 19)
    assert_eq!(screen.grid.cursor_y, 9);
    assert_eq!(screen.grid.cursor_x, 19);
    let rendered = reattach_render(&screen);
    let (row, col) = extract_cursor_cup(&rendered);
    assert_eq!((row, col), (10, 20),
        "reattach: cursor position after save/restore should be preserved");
}

#[test]
fn reattach_cursor_after_resize_clamp() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[24;80H"); // bottom-right
    // Simulate reattach with smaller terminal
    screen.resize(40, 12);
    assert_eq!(screen.grid.cursor_x, 39);
    assert_eq!(screen.grid.cursor_y, 11);
    let rendered = reattach_render(&screen);
    let (row, col) = extract_cursor_cup(&rendered);
    assert_eq!((row, col), (12, 40),
        "reattach: cursor should be clamped to new dimensions after resize");
}

#[test]
fn reattach_cursor_after_resize_within_bounds() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[5;10H"); // well within bounds
    screen.resize(40, 12);
    // Position (5,10) is within (40,12), should stay unchanged
    assert_eq!(screen.grid.cursor_x, 9);
    assert_eq!(screen.grid.cursor_y, 4);
    let rendered = reattach_render(&screen);
    let (row, col) = extract_cursor_cup(&rendered);
    assert_eq!((row, col), (5, 10),
        "reattach: cursor within bounds should not change after resize");
}

#[test]
fn reattach_cursor_after_scroll() {
    let mut screen = Screen::new(80, 5, 100);
    // Fill 5 rows and scroll by writing a 6th line
    screen.process(b"row1\r\nrow2\r\nrow3\r\nrow4\r\nrow5\r\nrow6");
    // After scroll, cursor should be on last row (row 4, 0-based)
    assert_eq!(screen.grid.cursor_y, 4);
    let rendered = reattach_render(&screen);
    let (row, col) = extract_cursor_cup(&rendered);
    assert_eq!(row, 5, "reattach: cursor row after scroll should be last row (5, 1-indexed)");
    assert_eq!(col, 5, "reattach: cursor col after 'row6' should be 5 (1-indexed)");
}

#[test]
fn reattach_cursor_after_alt_screen_exit() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[10;20H");   // position cursor
    screen.process(b"\x1b[?1049h");   // enter alt screen (saves cursor)
    screen.process(b"\x1b[5;5H");     // move on alt screen
    screen.process(b"\x1b[?1049l");   // exit alt screen (restores cursor)
    // Cursor should be restored to (10,20) → 0-based (9,19)
    assert_eq!(screen.grid.cursor_y, 9);
    assert_eq!(screen.grid.cursor_x, 19);
    let rendered = reattach_render(&screen);
    let (row, col) = extract_cursor_cup(&rendered);
    assert_eq!((row, col), (10, 20),
        "reattach: cursor should be restored after alt screen exit");
}

#[test]
fn reattach_bracketed_paste_mode() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[?2004h"); // enable bracketed paste
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("\x1b[?2004h"),
        "reattach: bracketed paste mode should be in render output");
}

#[test]
fn reattach_mouse_mode_1003() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[?1003h"); // enable any-event tracking
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("\x1b[?1003h"),
        "reattach: mouse mode 1003 should be in render output");
}

#[test]
fn reattach_mouse_sgr_encoding() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[?1000h"); // enable mouse
    screen.process(b"\x1b[?1006h"); // SGR encoding
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("\x1b[?1000h"),
        "reattach: mouse mode 1000 should be in render output");
    assert!(rendered.contains("\x1b[?1006h"),
        "reattach: SGR mouse encoding should be in render output");
}

#[test]
fn reattach_focus_reporting() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[?1004h"); // enable focus reporting
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("\x1b[?1004h"),
        "reattach: focus reporting should be in render output");
}

#[test]
fn reattach_cursor_key_mode() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[?1h"); // enable cursor key mode (DECCKM)
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("\x1b[?1h"),
        "reattach: cursor key mode (DECCKM) should be in render output");
}

#[test]
fn reattach_keypad_app_mode() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b="); // enable keypad application mode
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("\x1b="),
        "reattach: keypad application mode should be in render output");
}

#[test]
fn reattach_title_preserved() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b]2;My Session\x07"); // set title
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("\x1b]2;My Session\x07"),
        "reattach: window title should be in render output");
}

#[test]
fn reattach_cell_content_preserved() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"Hello");
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("Hello"),
        "reattach: cell content should be preserved in render output");
}

#[test]
fn reattach_wrap_pending_cursor_at_right_margin() {
    let mut screen = Screen::new(5, 3, 100);
    screen.process(b"ABCDE"); // fill line, triggers wrap_pending
    assert!(screen.grid.wrap_pending);
    // Cursor x is at 4 (0-based) with wrap pending — next char wraps
    assert_eq!(screen.grid.cursor_x, 4);
    assert_eq!(screen.grid.cursor_y, 0);
    let rendered = reattach_render(&screen);
    let (row, col) = extract_cursor_cup(&rendered);
    assert_eq!((row, col), (1, 5),
        "reattach: cursor with wrap_pending should be at right margin");
}

#[test]
fn reattach_with_scrollback_preserves_cursor() {
    let mut screen = Screen::new(80, 5, 100);
    // Generate some scrollback
    for i in 0..10 {
        screen.process(format!("line{}\r\n", i).as_bytes());
    }
    let history = screen.get_history();
    assert!(!history.is_empty(), "should have scrollback");

    // Position cursor precisely
    screen.process(b"\x1b[3;15H");
    assert_eq!(screen.grid.cursor_y, 2);
    assert_eq!(screen.grid.cursor_x, 14);

    // Render with scrollback (as reattach would)
    let mut cache = RenderCache::new();
    let output = screen.render_with_scrollback(&history, &mut cache);
    let rendered = String::from_utf8_lossy(&output).into_owned();

    // Find the cursor position CUP after screen clear
    let clear_pos = rendered.find("\x1b[2J").expect("screen clear missing");
    let after_clear = &rendered[clear_pos..];
    // The cursor CUP after content should be at row 3, col 15
    assert!(after_clear.contains("\x1b[3;15H"),
        "reattach with scrollback: cursor should be at (3,15), rendered: {:?}",
        &after_clear[..after_clear.len().min(200)]);
}

#[test]
fn reattach_full_state_roundtrip() {
    // Simulate a complex session state and verify full restoration
    let mut screen = Screen::new(80, 24, 100);

    // Set up various terminal state
    screen.process(b"\x1b[?2004h");  // bracketed paste
    screen.process(b"\x1b[?1h");     // DECCKM
    screen.process(b"\x1b[?1003h");  // mouse any-event
    screen.process(b"\x1b[?1006h");  // SGR mouse encoding
    screen.process(b"\x1b[5 q");     // blinking bar cursor
    screen.process(b"\x1b]2;complex session\x07"); // title

    // Write content and position cursor
    screen.process(b"Hello World");
    screen.process(b"\x1b[12;35H");  // move cursor to specific position

    // Reattach render
    let rendered = reattach_render(&screen);

    // Verify cursor position
    let (row, col) = extract_cursor_cup(&rendered);
    assert_eq!((row, col), (12, 35),
        "roundtrip: cursor position should be preserved");

    // Verify all modes
    assert!(rendered.contains("\x1b[?2004h"), "roundtrip: bracketed paste");
    assert!(rendered.contains("\x1b[?1h"), "roundtrip: DECCKM");
    assert!(rendered.contains("\x1b[?1003h"), "roundtrip: mouse mode");
    assert!(rendered.contains("\x1b[?1006h"), "roundtrip: SGR encoding");
    assert!(rendered.contains("\x1b[5 q"), "roundtrip: cursor shape");
    assert!(rendered.contains("\x1b]2;complex session\x07"), "roundtrip: title");

    // Verify content
    assert!(rendered.contains("Hello World"), "roundtrip: cell content");

    // Verify cursor visibility (should be shown)
    assert!(rendered.contains("\x1b[?25h"), "roundtrip: cursor visible");

    // Verify sync wrapper
    assert!(rendered.starts_with("\x1b[?2026h"), "roundtrip: sync begin");
    assert!(rendered.ends_with("\x1b[?2026l"), "roundtrip: sync end");
}

#[test]
fn reattach_after_multiple_resizes() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[20;60H"); // row 20, col 60

    // Resize down
    screen.resize(40, 12);
    assert_eq!(screen.grid.cursor_x, 39); // clamped
    assert_eq!(screen.grid.cursor_y, 11); // clamped

    // Resize back up
    screen.resize(100, 30);
    // Cursor stays at (39, 11), not reset
    assert_eq!(screen.grid.cursor_x, 39);
    assert_eq!(screen.grid.cursor_y, 11);

    let rendered = reattach_render(&screen);
    let (row, col) = extract_cursor_cup(&rendered);
    assert_eq!((row, col), (12, 40),
        "reattach: cursor should be at clamped position after multiple resizes");
}

#[test]
fn reattach_cursor_after_clear_screen() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"lots of content here");
    screen.process(b"\x1b[10;25H"); // position cursor
    screen.process(b"\x1b[2J");      // clear screen
    // Clear screen does NOT move cursor
    assert_eq!(screen.grid.cursor_y, 9);
    assert_eq!(screen.grid.cursor_x, 24);
    let rendered = reattach_render(&screen);
    let (row, col) = extract_cursor_cup(&rendered);
    assert_eq!((row, col), (10, 25),
        "reattach: cursor position should survive clear screen");
}

#[test]
fn reattach_cursor_after_erase_in_display() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[15;30H");
    screen.process(b"\x1b[0J"); // erase below
    // Cursor stays at (15,30)
    assert_eq!(screen.grid.cursor_y, 14);
    assert_eq!(screen.grid.cursor_x, 29);
    let rendered = reattach_render(&screen);
    let (row, col) = extract_cursor_cup(&rendered);
    assert_eq!((row, col), (15, 30),
        "reattach: cursor should be preserved after erase in display");
}

#[test]
fn reattach_second_render_uses_cache() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"Hello");
    screen.process(b"\x1b[5;10H");

    let mut cache = RenderCache::new();
    // First render (full reattach)
    let _render1 = screen.render(true, &mut cache);
    // Second render (incremental) — should still have cursor position
    let render2 = screen.render(false, &mut cache);
    let text2 = String::from_utf8_lossy(&render2);
    assert!(text2.contains("\x1b[5;10H"),
        "incremental render should still set cursor position");
}

#[test]
fn reattach_fresh_cache_always_full_render() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"data on row 1");
    screen.process(b"\x1b[2;1H");
    screen.process(b"data on row 2");
    screen.process(b"\x1b[8;20H"); // final cursor position

    // New cache simulates new client connection (reattach)
    let mut cache = RenderCache::new();
    let rendered = String::from_utf8_lossy(&screen.render(true, &mut cache)).into_owned();

    // Full render includes screen clear
    assert!(rendered.contains("\x1b[2J\x1b[H"),
        "reattach with fresh cache should clear screen");
    // Cell content present
    assert!(rendered.contains("data on row 1"),
        "reattach should include row 1 content");
    assert!(rendered.contains("data on row 2"),
        "reattach should include row 2 content");
    // Cursor position
    let (row, col) = extract_cursor_cup(&rendered);
    assert_eq!((row, col), (8, 20),
        "reattach with fresh cache should position cursor correctly");
}

// ---------------------------------------------------------------
// Reattach screen content restoration tests
// ---------------------------------------------------------------

#[test]
fn reattach_bold_text_preserved() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[1mBOLD\x1b[0m");
    let rendered = reattach_render(&screen);
    // Bold SGR (param 1) should be in the render output before BOLD text
    assert!(rendered.contains("BOLD"), "bold text content should be present");
    // The combined reset+set SGR for bold: \x1b[0;1m
    assert!(rendered.contains("\x1b[0;1m"),
        "reattach: bold SGR should be present in render output");
}

#[test]
fn reattach_colored_text_preserved() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[31mRED\x1b[0m \x1b[32mGREEN\x1b[0m");
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("RED"), "red text should be present");
    assert!(rendered.contains("GREEN"), "green text should be present");
    // Red fg: param 31
    assert!(rendered.contains("31m"), "reattach: red color SGR should be present");
    // Green fg: param 32
    assert!(rendered.contains("32m"), "reattach: green color SGR should be present");
}

#[test]
fn reattach_rgb_color_preserved() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[38;2;100;200;50mRGB\x1b[0m");
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("RGB"), "RGB-colored text should be present");
    assert!(rendered.contains("38;2;100;200;50"),
        "reattach: RGB color SGR should be preserved");
}

#[test]
fn reattach_256_color_preserved() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[38;5;200mPAL\x1b[0m");
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("PAL"), "256-color text should be present");
    assert!(rendered.contains("38;5;200"),
        "reattach: 256-color SGR should be preserved");
}

#[test]
fn reattach_background_color_preserved() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[44m BG \x1b[0m");
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("44"),
        "reattach: background color SGR should be preserved");
}

#[test]
fn reattach_combined_sgr_attributes() {
    let mut screen = Screen::new(80, 24, 100);
    // Bold + italic + underline + red fg + blue bg
    screen.process(b"\x1b[1;3;4;31;44mSTYLED\x1b[0m");
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("STYLED"), "styled text should be present");
    // Check individual attributes in the combined SGR
    assert!(rendered.contains(";1;"), "reattach: bold should be in SGR");
    assert!(rendered.contains(";3;"), "reattach: italic should be in SGR");
    assert!(rendered.contains(";4;"), "reattach: underline should be in SGR");
    assert!(rendered.contains("31"), "reattach: red fg should be in SGR");
    assert!(rendered.contains("44"), "reattach: blue bg should be in SGR");
}

#[test]
fn reattach_inverse_attribute() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[7mINV\x1b[0m");
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("INV"), "inverse text should be present");
    assert!(rendered.contains(";7"),
        "reattach: inverse (SGR 7) should be preserved");
}

#[test]
fn reattach_strikethrough_attribute() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[9mSTRIKE\x1b[0m");
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("STRIKE"), "strikethrough text should be present");
    assert!(rendered.contains(";9"),
        "reattach: strikethrough (SGR 9) should be preserved");
}

#[test]
fn reattach_dim_attribute() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[2mDIM\x1b[0m");
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("DIM"), "dim text should be present");
    assert!(rendered.contains(";2"),
        "reattach: dim (SGR 2) should be preserved");
}

#[test]
fn reattach_wide_characters() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process("你好世界".as_bytes());
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("你好世界"),
        "reattach: wide CJK characters should be preserved");
}

#[test]
fn reattach_combining_marks() {
    let mut screen = Screen::new(80, 24, 100);
    // e + combining acute accent = é
    screen.process("e\u{0301}".as_bytes());
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("e\u{0301}"),
        "reattach: combining marks should be preserved");
}

#[test]
fn reattach_line_drawing_characters() {
    let mut screen = Screen::new(80, 24, 100);
    // Switch to line drawing charset and draw a box corner
    screen.process(b"\x1b(0");  // G0 = line drawing
    screen.process(b"lqk");     // ┌─┐ (l=corner, q=horiz, k=corner)
    let rendered = reattach_render(&screen);
    assert!(rendered.contains('┌'), "reattach: line drawing ┌ should be present");
    assert!(rendered.contains('─'), "reattach: line drawing ─ should be present");
    assert!(rendered.contains('┐'), "reattach: line drawing ┐ should be present");
}

#[test]
fn reattach_multiple_rows_content() {
    let mut screen = Screen::new(40, 10, 100);
    screen.process(b"\x1b[1;1HRow One");
    screen.process(b"\x1b[2;1HRow Two");
    screen.process(b"\x1b[3;1HRow Three");
    screen.process(b"\x1b[5;1HRow Five");
    // Row 4 intentionally blank
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("Row One"), "reattach: row 1 content");
    assert!(rendered.contains("Row Two"), "reattach: row 2 content");
    assert!(rendered.contains("Row Three"), "reattach: row 3 content");
    assert!(rendered.contains("Row Five"), "reattach: row 5 content");
}

#[test]
fn reattach_row_order_correct() {
    let mut screen = Screen::new(40, 5, 100);
    screen.process(b"\x1b[1;1HFIRST");
    screen.process(b"\x1b[3;1HSECOND");
    screen.process(b"\x1b[5;1HTHIRD");
    let rendered = reattach_render(&screen);
    let pos_first = rendered.find("FIRST").expect("FIRST missing");
    let pos_second = rendered.find("SECOND").expect("SECOND missing");
    let pos_third = rendered.find("THIRD").expect("THIRD missing");
    assert!(pos_first < pos_second, "FIRST should appear before SECOND");
    assert!(pos_second < pos_third, "SECOND should appear before THIRD");
}

#[test]
fn reattach_content_after_autowrap() {
    let mut screen = Screen::new(5, 3, 100);
    screen.process(b"ABCDEfgh"); // ABCDE fills row 0, fgh wraps to row 1
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("ABCDE"), "reattach: first row content after wrap");
    assert!(rendered.contains("fgh"), "reattach: wrapped content on second row");
}

#[test]
fn reattach_content_last_column() {
    let mut screen = Screen::new(10, 3, 100);
    // Place char at last column
    screen.process(b"\x1b[1;10H");
    screen.process(b"X");
    assert_eq!(screen.grid.visible_row(0)[9].c, 'X');
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("X"),
        "reattach: content at last column should be present");
}

#[test]
fn reattach_content_last_row() {
    let mut screen = Screen::new(10, 5, 100);
    screen.process(b"\x1b[5;1HBottom");
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("Bottom"),
        "reattach: content on last row should be present");
}

#[test]
fn reattach_content_after_insert_lines() {
    let mut screen = Screen::new(20, 5, 100);
    screen.process(b"\x1b[1;1HLine1");
    screen.process(b"\x1b[2;1HLine2");
    screen.process(b"\x1b[3;1HLine3");
    // Position at row 2 and insert a blank line
    screen.process(b"\x1b[2;1H");
    screen.process(b"\x1b[L"); // IL 1
    // Line2 and Line3 should shift down, row 2 is now blank
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("Line1"), "reattach: Line1 should remain");
    assert!(rendered.contains("Line2"), "reattach: Line2 should be shifted down");
    assert!(rendered.contains("Line3"), "reattach: Line3 should be shifted down");
    // Verify order: Line1 < Line2 < Line3 still holds
    let pos1 = rendered.find("Line1").unwrap();
    let pos2 = rendered.find("Line2").unwrap();
    let pos3 = rendered.find("Line3").unwrap();
    assert!(pos1 < pos2 && pos2 < pos3,
        "reattach: line order should be preserved after insert");
}

#[test]
fn reattach_content_after_delete_lines() {
    let mut screen = Screen::new(20, 5, 100);
    screen.process(b"\x1b[1;1HLine1");
    screen.process(b"\x1b[2;1HLine2");
    screen.process(b"\x1b[3;1HLine3");
    // Delete row 2
    screen.process(b"\x1b[2;1H");
    screen.process(b"\x1b[M"); // DL 1
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("Line1"), "reattach: Line1 should remain after DL");
    // Line2 is deleted, Line3 moves up
    assert!(rendered.contains("Line3"), "reattach: Line3 should be shifted up after DL");
}

#[test]
fn reattach_content_after_delete_characters() {
    let mut screen = Screen::new(20, 3, 100);
    screen.process(b"HelloWorld");
    // Move to col 5 and delete 5 chars
    screen.process(b"\x1b[1;6H");
    screen.process(b"\x1b[5P"); // DCH 5
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("Hello"),
        "reattach: content before DCH should remain");
}

#[test]
fn reattach_content_after_insert_characters() {
    let mut screen = Screen::new(20, 3, 100);
    screen.process(b"ABCDE");
    screen.process(b"\x1b[1;3H"); // position at col 3
    screen.process(b"\x1b[2@");   // ICH 2 — insert 2 blanks
    // AB..CDE → AB  CDE (C pushed right)
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("AB"), "reattach: content before ICH preserved");
    assert!(rendered.contains("CDE"), "reattach: content after ICH shifted right");
}

#[test]
fn reattach_content_after_erase_to_end_of_line() {
    let mut screen = Screen::new(20, 3, 100);
    screen.process(b"Hello World!");
    screen.process(b"\x1b[1;6H"); // position at col 6
    screen.process(b"\x1b[0K");   // EL 0: erase from cursor to end
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("Hello"),
        "reattach: content before erase should remain");
    // " World!" should be erased
    assert!(!rendered.contains("World"),
        "reattach: erased content should not be present");
}

#[test]
fn reattach_content_with_scroll_region() {
    let mut screen = Screen::new(20, 6, 100);
    // Set scroll region to rows 2-5
    screen.process(b"\x1b[2;5r");
    // Write content on each row
    screen.process(b"\x1b[1;1HTop");     // row 1 (outside region, above)
    screen.process(b"\x1b[2;1HIn2");     // row 2 (inside region)
    screen.process(b"\x1b[3;1HIn3");     // row 3 (inside region)
    screen.process(b"\x1b[6;1HBottom");  // row 6 (outside region, below)
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("Top"), "reattach: content above scroll region");
    assert!(rendered.contains("In2"), "reattach: content inside scroll region");
    assert!(rendered.contains("In3"), "reattach: content inside scroll region");
    assert!(rendered.contains("Bottom"), "reattach: content below scroll region");
}

#[test]
fn reattach_content_after_scroll_within_region() {
    let mut screen = Screen::new(20, 6, 100);
    screen.process(b"\x1b[1;1HFixed");
    screen.process(b"\x1b[6;1HFooter");
    // Set scroll region to rows 2-5 and fill it
    screen.process(b"\x1b[2;5r");
    screen.process(b"\x1b[2;1H");
    screen.process(b"R2\r\nR3\r\nR4\r\nR5\r\nR6"); // R6 scrolls region
    let rendered = reattach_render(&screen);
    // Fixed and Footer should be untouched (outside scroll region)
    assert!(rendered.contains("Fixed"),
        "reattach: content outside scroll region should survive scroll");
    assert!(rendered.contains("Footer"),
        "reattach: content below scroll region should survive scroll");
}

#[test]
fn reattach_content_after_reverse_index() {
    let mut screen = Screen::new(20, 5, 100);
    screen.process(b"\x1b[1;1HLine1");
    screen.process(b"\x1b[2;1HLine2");
    // Position at top row and do reverse index (ESC M) — scrolls down
    screen.process(b"\x1b[1;1H");
    screen.process(b"\x1bM"); // RI
    // Line1 and Line2 shift down, new blank row at top
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("Line1"), "reattach: Line1 after RI");
    assert!(rendered.contains("Line2"), "reattach: Line2 after RI");
}

#[test]
fn reattach_alt_screen_content() {
    let mut screen = Screen::new(20, 5, 100);
    screen.process(b"Main Screen");
    // Enter alt screen
    screen.process(b"\x1b[?1049h");
    screen.process(b"Alt Content");
    // If reattach while in alt screen, should see alt content
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("Alt Content"),
        "reattach: alt screen content should be rendered when in alt screen");
    assert!(!rendered.contains("Main Screen"),
        "reattach: main screen content should NOT be visible while in alt screen");
}

#[test]
fn reattach_after_alt_screen_roundtrip() {
    let mut screen = Screen::new(20, 5, 100);
    screen.process(b"Original");
    screen.process(b"\x1b[?1049h");
    screen.process(b"Temporary");
    screen.process(b"\x1b[?1049l");
    // Back to main screen
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("Original"),
        "reattach: main screen content should be restored after alt screen exit");
    assert!(!rendered.contains("Temporary"),
        "reattach: alt screen content should be gone after exit");
}

#[test]
fn reattach_tab_aligned_content() {
    let mut screen = Screen::new(40, 3, 100);
    screen.process(b"A\tB\tC");
    let rendered = reattach_render(&screen);
    // Tab stops at column 8, 16, etc. — chars should be at those positions
    assert!(rendered.contains("A"), "reattach: content before tab");
    assert!(rendered.contains("B"), "reattach: content after first tab");
    assert!(rendered.contains("C"), "reattach: content after second tab");
    // Verify tab alignment: B should be at column 8 (0-indexed)
    assert_eq!(screen.grid.visible_row(0)[8].c, 'B',
        "reattach: B should be at tab stop column 8");
    assert_eq!(screen.grid.visible_row(0)[16].c, 'C',
        "reattach: C should be at tab stop column 16");
}

#[test]
fn reattach_background_color_erase() {
    let mut screen = Screen::new(20, 3, 100);
    // Set background color, then erase line — BCE should apply
    screen.process(b"\x1b[41m"); // red background
    screen.process(b"\x1b[2K");  // erase entire line
    // Cells on row 0 should have red background
    assert_eq!(screen.grid.visible_row(0)[0].style.bg,
        Some(super::style::Color::Indexed(1)),
        "BCE: erased cells should have red background");
    // Verify render includes the background color
    let rendered = reattach_render(&screen);
    // The cell has red bg with space char — render should include SGR for bg
    assert!(rendered.contains("41"),
        "reattach: BCE background color should be in render output");
}

#[test]
fn reattach_mixed_styled_unstyled_regions() {
    let mut screen = Screen::new(30, 3, 100);
    screen.process(b"plain ");
    screen.process(b"\x1b[1;31mbold red\x1b[0m");
    screen.process(b" plain again");
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("plain"), "reattach: unstyled text present");
    assert!(rendered.contains("bold red"), "reattach: styled text present");
    assert!(rendered.contains("plain again"), "reattach: trailing unstyled text");
    // Verify style reset appears between regions
    assert!(rendered.contains("\x1b[0m"),
        "reattach: SGR reset should appear for style transitions");
}

#[test]
fn reattach_empty_screen() {
    let screen = Screen::new(80, 24, 100);
    let rendered = reattach_render(&screen);
    // Should still have the structural elements
    assert!(rendered.contains("\x1b[?2026h"), "reattach: sync begin on empty screen");
    assert!(rendered.contains("\x1b[?2026l"), "reattach: sync end on empty screen");
    assert!(rendered.contains("\x1b[2J"), "reattach: clear screen on empty screen");
}

#[test]
fn reattach_render_structure_order() {
    let mut screen = Screen::new(20, 5, 100);
    screen.process(b"\x1b[1;31mContent\x1b[0m");
    screen.process(b"\x1b]2;TestTitle\x07");
    screen.process(b"\x1b[3;10H"); // cursor at row 3, col 10
    let rendered = reattach_render(&screen);

    // Verify order: sync_begin < hide_cursor < clear < content < cursor_pos < modes < title < show_cursor < sync_end
    let sync_begin = rendered.find("\x1b[?2026h").expect("sync begin");
    let hide_cursor = rendered.find("\x1b[?25l").expect("hide cursor");
    let clear = rendered.find("\x1b[2J").expect("clear screen");
    let content = rendered.find("Content").expect("content");
    let title = rendered.find("\x1b]2;TestTitle").expect("title");
    let show_cursor = rendered.rfind("\x1b[?25h").expect("show cursor");
    let sync_end = rendered.rfind("\x1b[?2026l").expect("sync end");

    assert!(sync_begin < hide_cursor, "sync begin before hide cursor");
    assert!(hide_cursor < clear, "hide cursor before clear");
    assert!(clear < content, "clear before content");
    assert!(content < title, "content before title");
    assert!(title < show_cursor, "title before show cursor");
    assert!(show_cursor < sync_end, "show cursor before sync end");
}

#[test]
fn reattach_scrollback_not_in_grid_render() {
    let mut screen = Screen::new(20, 3, 100);
    // Generate scrollback by filling more lines than rows
    screen.process(b"scroll1\r\nscroll2\r\nscroll3\r\nvisible");
    // scroll1 should be in scrollback, not in grid
    let rendered = reattach_render(&screen);
    assert!(!rendered.contains("scroll1"),
        "reattach: scrollback lines should NOT be in grid render");
    assert!(rendered.contains("visible"),
        "reattach: visible content should be in render");
}

#[test]
fn reattach_scrollback_lines_in_history_render() {
    let mut screen = Screen::new(20, 3, 100);
    screen.process(b"scroll1\r\nscroll2\r\nscroll3\r\nvisible");
    let history = screen.get_history();
    assert!(!history.is_empty(), "should have scrollback history");

    let mut cache = RenderCache::new();
    let output = screen.render_with_scrollback(&history, &mut cache);
    let rendered = String::from_utf8_lossy(&output);
    // History render should include scrollback AND grid content
    assert!(rendered.contains("scroll1"),
        "reattach with history: scroll1 should be present");
    assert!(rendered.contains("visible"),
        "reattach with history: visible content should be present");
}

#[test]
fn reattach_multiple_style_changes_per_row() {
    let mut screen = Screen::new(40, 3, 100);
    screen.process(b"\x1b[31mR\x1b[32mG\x1b[34mB\x1b[0m N");
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("R"), "red char present");
    assert!(rendered.contains("G"), "green char present");
    assert!(rendered.contains("B"), "blue char present");
    assert!(rendered.contains("N"), "normal char present");
    // At least 3 style change sequences (for R, G, B)
    let sgr_count = rendered.matches("\x1b[0;").count();
    assert!(sgr_count >= 3,
        "reattach: should have at least 3 SGR style changes, got {}", sgr_count);
}

#[test]
fn reattach_content_fills_entire_screen() {
    let mut screen = Screen::new(5, 3, 100);
    // Fill every cell
    for row in 0..3 {
        screen.process(format!("\x1b[{};1H", row + 1).as_bytes());
        screen.process(b"XXXXX");
    }
    let rendered = reattach_render(&screen);
    // Count X's in the render — should have at least 15 (5 cols × 3 rows)
    let x_count = rendered.matches('X').count();
    assert_eq!(x_count, 15,
        "reattach: fully filled screen should have 15 X's, got {}", x_count);
}

#[test]
fn reattach_cursor_position_independent_of_content() {
    // Cursor can be positioned anywhere, not just after written content
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[1;1HHello");
    screen.process(b"\x1b[20;50H"); // cursor far from content
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("Hello"), "content should be present");
    let (row, col) = extract_cursor_cup(&rendered);
    assert_eq!((row, col), (20, 50),
        "cursor should be at (20,50), independent of content position");
}

#[test]
fn reattach_after_full_reset() {
    let mut screen = Screen::new(80, 24, 100);
    // Set up complex state
    screen.process(b"\x1b[1;31mColored\x1b[0m");
    screen.process(b"\x1b[?2004h");     // bracketed paste
    screen.process(b"\x1b[5 q");        // blinking bar
    screen.process(b"\x1b[10;20H");     // cursor position
    screen.process(b"\x1b]2;Title\x07"); // title
    // Full reset (RIS)
    screen.process(b"\x1bc");
    let rendered = reattach_render(&screen);
    // After RIS, screen should be blank
    assert!(!rendered.contains("Colored"),
        "reattach after RIS: content should be cleared");
    // Cursor at origin
    let (row, col) = extract_cursor_cup(&rendered);
    assert_eq!((row, col), (1, 1),
        "reattach after RIS: cursor should be at origin");
    // Cursor visible
    assert!(rendered.contains("\x1b[?25h"),
        "reattach after RIS: cursor should be visible");
    // Default cursor shape (param 0)
    assert!(rendered.contains("\x1b[0 q"),
        "reattach after RIS: cursor shape should be default");
}

#[test]
fn reattach_overwritten_cell_shows_latest() {
    let mut screen = Screen::new(20, 3, 100);
    screen.process(b"OLD");
    screen.process(b"\x1b[1;1H"); // move home
    screen.process(b"NEW");       // overwrite
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("NEW"),
        "reattach: overwritten cells should show latest content");
}

#[test]
fn reattach_wide_char_at_end_of_row() {
    let mut screen = Screen::new(10, 3, 100);
    // Position at second-to-last column and write wide char
    screen.process(b"\x1b[1;9H");
    screen.process("你".as_bytes()); // occupies cols 8-9
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("你"),
        "reattach: wide char at end of row should be preserved");
}

#[test]
fn reattach_wide_char_wraps_at_boundary() {
    let mut screen = Screen::new(5, 3, 100);
    // Fill 4 columns, then write wide char that doesn't fit
    screen.process(b"ABCD");
    screen.process("你".as_bytes()); // should wrap to next row
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("ABCD"), "narrow chars before wrap boundary");
    assert!(rendered.contains("你"), "wide char should be on next row");
}

#[test]
fn reattach_hidden_text_attribute() {
    let mut screen = Screen::new(20, 3, 100);
    screen.process(b"\x1b[8mSECRET\x1b[0m");
    // Cell content should be there, just with hidden attribute
    assert!(screen.grid.visible_row(0)[0].style.hidden);
    let rendered = reattach_render(&screen);
    // Content should still be in render (hidden is an SGR attribute, terminal handles display)
    assert!(rendered.contains("SECRET"),
        "reattach: hidden text content should still be in render output");
    assert!(rendered.contains(";8"),
        "reattach: hidden attribute (SGR 8) should be preserved");
}

#[test]
fn reattach_preserves_all_rows_after_partial_scroll() {
    let mut screen = Screen::new(20, 5, 100);
    // Write to all rows
    for i in 1..=5 {
        screen.process(format!("\x1b[{};1HRow{}", i, i).as_bytes());
    }
    // Scroll up by 2 (CSI 2 S)
    screen.process(b"\x1b[2S");
    // Row1 and Row2 scrolled off, Row3 is now at top
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("Row3"), "reattach: Row3 should be at top after scroll");
    assert!(rendered.contains("Row4"), "reattach: Row4 should be visible");
    assert!(rendered.contains("Row5"), "reattach: Row5 should be visible");
    assert!(!rendered.contains("Row1"), "reattach: Row1 should be scrolled off");
    assert!(!rendered.contains("Row2"), "reattach: Row2 should be scrolled off");
}

// ---------------------------------------------------------------
// Bug: missing mode restoration on reattach (htop artifacts)
// ---------------------------------------------------------------

/// DECAWM (autowrap, ?7) must be restored on reattach.
/// If an app disables autowrap and we reconnect, the outer terminal must
/// also disable it — otherwise lines wrap unexpectedly.
#[test]
fn reattach_restores_autowrap_disabled() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[?7l"); // disable autowrap
    assert!(!screen.grid.modes.autowrap_mode);
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("\x1b[?7l"),
        "reattach: disabled autowrap (DECAWM) should be emitted in render output");
}

/// When autowrap is enabled (default), the render should explicitly set it
/// so the outer terminal is in the correct state regardless of its previous state.
#[test]
fn reattach_restores_autowrap_enabled() {
    let screen = Screen::new(80, 24, 100);
    assert!(screen.grid.modes.autowrap_mode);
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("\x1b[?7h"),
        "reattach: enabled autowrap (DECAWM) should be emitted in render output");
}

/// G0 charset (line drawing) must be restored on reattach.
/// Apps like `mc`, `vim`, `htop` use DEC line drawing for box borders.
/// If the charset isn't restored, subsequent output uses ASCII instead of
/// box-drawing characters.
#[test]
fn reattach_restores_g0_line_drawing_charset() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b(0"); // G0 = DEC line drawing
    assert_eq!(screen.grid.modes.g0_charset, super::grid::Charset::LineDrawing);
    let rendered = reattach_render(&screen);
    // ESC ( 0 designates G0 as line drawing
    assert!(rendered.contains("\x1b(0"),
        "reattach: G0 line drawing charset should be restored");
}

/// G0 ASCII charset (default) should be explicitly set so the outer terminal
/// is reset even if it was previously in line drawing mode.
#[test]
fn reattach_restores_g0_ascii_charset() {
    let screen = Screen::new(80, 24, 100);
    assert_eq!(screen.grid.modes.g0_charset, super::grid::Charset::Ascii);
    let rendered = reattach_render(&screen);
    // ESC ( B designates G0 as ASCII
    assert!(rendered.contains("\x1b(B"),
        "reattach: G0 ASCII charset should be explicitly set");
}

/// G1 line drawing charset must be restored.
#[test]
fn reattach_restores_g1_line_drawing_charset() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b)0"); // G1 = DEC line drawing
    assert_eq!(screen.grid.modes.g1_charset, super::grid::Charset::LineDrawing);
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("\x1b)0"),
        "reattach: G1 line drawing charset should be restored");
}

/// Active charset (G1 via SO) must be restored on reattach.
/// If the app switched to G1 with SO (0x0E), we must re-emit SO so the
/// outer terminal maps characters through G1.
#[test]
fn reattach_restores_active_charset_g1() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x0e"); // SO — activate G1
    assert_eq!(screen.grid.modes.active_charset, super::grid::ActiveCharset::G1);
    let rendered = reattach_render(&screen);
    // The render output should contain SO (0x0E) to activate G1
    assert!(rendered.as_bytes().contains(&0x0E),
        "reattach: active charset G1 (SO) should be restored");
}

/// When active charset is G0 (default), SI (0x0F) should be emitted to
/// ensure the outer terminal isn't stuck in G1 from a previous session.
#[test]
fn reattach_restores_active_charset_g0() {
    let screen = Screen::new(80, 24, 100);
    assert_eq!(screen.grid.modes.active_charset, super::grid::ActiveCharset::G0);
    let rendered = reattach_render(&screen);
    // The render output should contain SI (0x0F) to ensure G0 is active
    assert!(rendered.as_bytes().contains(&0x0F),
        "reattach: active charset G0 (SI) should be explicitly set");
}

/// Incremental render (mode delta) must detect autowrap changes.
#[test]
fn mode_delta_detects_autowrap_change() {
    let mut grid = super::grid::Grid::new(10, 3, 0);
    let mut cache = super::render::RenderCache::new();
    // Initial render with default modes (autowrap=true)
    let _ = super::render::render_screen(&grid, "", true, &mut cache);

    // Disable autowrap
    grid.modes.autowrap_mode = false;
    let result = super::render::render_screen(&grid, "", false, &mut cache);
    let text = String::from_utf8_lossy(&result);
    assert!(text.contains("\x1b[?7l"),
        "mode delta should emit DECAWM disable when autowrap changes to false");
}

/// Incremental render (mode delta) must detect charset changes.
#[test]
fn mode_delta_detects_charset_change() {
    let mut grid = super::grid::Grid::new(10, 3, 0);
    let mut cache = super::render::RenderCache::new();
    let _ = super::render::render_screen(&grid, "", true, &mut cache);

    // Switch G0 to line drawing
    grid.modes.g0_charset = super::grid::Charset::LineDrawing;
    let result = super::render::render_screen(&grid, "", false, &mut cache);
    let text = String::from_utf8_lossy(&result);
    assert!(text.contains("\x1b(0"),
        "mode delta should emit G0 line drawing when charset changes");
}

// ---------------------------------------------------------------
// Simulated htop reattach scenarios
// ---------------------------------------------------------------

/// Simulate htop-like screen state (alt screen, colors, box drawing)
/// and verify multiple reattach cycles produce correct output.
#[test]
fn reattach_htop_multiple_reconnections() {
    let mut screen = Screen::new(40, 10, 100);

    // Simulate htop: enter alt screen, set up UI
    screen.process(b"\x1b[?1049h");  // enter alt screen
    screen.process(b"\x1b[?25l");    // hide cursor

    // Draw a colored header
    screen.process(b"\x1b[1;1H\x1b[1;37;44m CPU [");
    screen.process(b"\x1b[32m|||||||||\x1b[37m..........");
    screen.process(b"\x1b[37;44m 45.2%]\x1b[0m");

    // Draw some process lines
    screen.process(b"\x1b[3;1H\x1b[32m  PID USER      PRI  NI\x1b[0m");
    screen.process(b"\x1b[4;1H    1 root       20   0");

    // First reattach — should be correct
    let rendered1 = reattach_render(&screen);
    assert!(rendered1.contains("CPU"), "first reattach: header content");
    assert!(rendered1.contains("PID"), "first reattach: column headers");
    assert!(rendered1.contains("root"), "first reattach: process data");

    // Simulate htop updating between reconnections
    screen.process(b"\x1b[1;1H\x1b[1;37;44m CPU [");
    screen.process(b"\x1b[32m|||||||\x1b[37m...........");
    screen.process(b"\x1b[37;44m 38.1%]\x1b[0m");

    // Second reattach
    let rendered2 = reattach_render(&screen);
    assert!(rendered2.contains("38.1%"), "second reattach: updated percentage");
    assert!(rendered2.contains("PID"), "second reattach: column headers still present");

    // Third reattach with another update
    screen.process(b"\x1b[1;1H\x1b[1;37;44m CPU [");
    screen.process(b"\x1b[32m|||||\x1b[37m.............");
    screen.process(b"\x1b[37;44m 27.5%]\x1b[0m");

    let rendered3 = reattach_render(&screen);
    assert!(rendered3.contains("27.5%"), "third reattach: updated percentage");
    assert!(rendered3.contains("PID"), "third reattach: column headers still present");
    assert!(rendered3.contains("root"), "third reattach: process data still present");
}

/// VTE parser state must not accumulate corruption across process() calls.
/// Simulates data loss at connection boundary: a chunk ends mid-escape-sequence,
/// and the next chunk starts with unrelated data.
#[test]
fn vte_parser_recovers_from_partial_escape_sequence() {
    let mut screen = Screen::new(40, 5, 0);

    // Normal output — sets up known state
    screen.process(b"\x1b[1;1HBefore");

    // Simulate a partial escape sequence (as if data was lost mid-sequence).
    // Feed just the CSI introducer without the final byte.
    screen.process(b"\x1b[38;5;");

    // Now feed new data as if from a new connection's first read.
    // This data starts with a digit that the parser will try to consume
    // as part of the incomplete CSI. The parser should eventually recover.
    screen.process(b"\x1b[2;1HAfter");

    // The "After" text should appear correctly on row 2
    let row2: String = screen.grid.visible_row(1).iter().map(|c| c.c).collect();
    assert!(row2.starts_with("After"),
        "VTE parser should recover from partial escape: row 2 = {:?}", row2.trim());
}

/// After a partial escape sequence, subsequent SGR commands should work correctly.
#[test]
fn vte_parser_sgr_correct_after_partial_sequence() {
    let mut screen = Screen::new(40, 5, 0);

    // Write initial content with a color
    screen.process(b"\x1b[31mRed\x1b[0m");

    // Simulate partial CSI (data loss)
    screen.process(b"\x1b[1;");

    // New data with a color change
    screen.process(b"\x1b[32mGreen\x1b[0m");

    // Find "Green" in the grid and verify its color
    let mut found_green = false;
    for row in screen.grid.visible_rows() {
        for (i, cell) in row.iter().enumerate() {
            if cell.c == 'G' {
                // Check subsequent cells spell "Green"
                let word: String = row[i..].iter().take(5).map(|c| c.c).collect();
                if word == "Green" {
                    assert_eq!(cell.style.fg, Some(super::style::Color::Indexed(2)),
                        "Green text should have green foreground after parser recovery");
                    found_green = true;
                    break;
                }
            }
        }
        if found_green { break; }
    }
    assert!(found_green, "Should find 'Green' text in grid after parser recovery");
}

/// Simulates the exact eviction scenario: screen has htop running,
/// some data is "lost" (not processed), then a full render + new data arrives.
#[test]
fn reattach_after_simulated_data_loss() {
    let mut screen = Screen::new(40, 10, 0);

    // Enter alt screen (htop)
    screen.process(b"\x1b[?1049h");

    // Draw initial htop frame
    screen.process(b"\x1b[1;1H\x1b[32mCPU: 50%\x1b[0m");
    screen.process(b"\x1b[2;1H\x1b[33mMem: 2G/8G\x1b[0m");
    screen.process(b"\x1b[3;1HPID  COMMAND");

    // "Lost" data: partial escape sequence that never completes
    // (simulates old reader consuming data but async task dropping it)
    screen.process(b"\x1b[4;1H\x1b[38;2;");

    // Verify screen is still usable after partial escape
    let mut cache = RenderCache::new();
    let render1 = screen.render(true, &mut cache);
    assert!(!render1.is_empty(), "render after partial escape should work");

    // New htop frame arrives (as if after reconnection)
    screen.process(b"\x1b[1;1H\x1b[32mCPU: 65%\x1b[0m");
    screen.process(b"\x1b[2;1H\x1b[33mMem: 3G/8G\x1b[0m");
    screen.process(b"\x1b[3;1HPID  COMMAND");
    screen.process(b"\x1b[4;1H  42 htop");

    // Second render should show updated content
    cache = RenderCache::new();
    let render2 = screen.render(true, &mut cache);
    let text = String::from_utf8_lossy(&render2);

    assert!(text.contains("65%"), "updated CPU should appear after data loss recovery");
    assert!(text.contains("3G/8G"), "updated Mem should appear after data loss recovery");
    assert!(text.contains("htop"), "process list should appear after data loss recovery");
}

/// Verify that the render output includes ALL necessary sequences to fully
/// initialize a fresh terminal. This catches missing mode restoration.
#[test]
fn reattach_render_is_self_contained() {
    let mut screen = Screen::new(80, 24, 100);

    // Set up non-default state for everything
    screen.process(b"\x1b[?7l");     // autowrap off
    screen.process(b"\x1b(0");       // G0 = line drawing
    screen.process(b"\x1b)0");       // G1 = line drawing
    screen.process(b"\x0e");         // SO — activate G1
    screen.process(b"\x1b[?1h");     // cursor key mode
    screen.process(b"\x1b[?2004h");  // bracketed paste
    screen.process(b"\x1b[?1003h");  // mouse any-event
    screen.process(b"\x1b[?1006h");  // SGR mouse encoding
    screen.process(b"\x1b[?1004h");  // focus reporting
    screen.process(b"\x1b=");        // keypad app mode
    screen.process(b"\x1b[5 q");     // blinking bar cursor
    screen.process(b"\x1b[?25l");    // hide cursor

    let rendered = reattach_render(&screen);

    // Every non-default mode must be present in the render output
    assert!(rendered.contains("\x1b[?7l"), "self-contained: DECAWM off");
    assert!(rendered.contains("\x1b(0"), "self-contained: G0 line drawing");
    assert!(rendered.contains("\x1b)0"), "self-contained: G1 line drawing");
    assert!(rendered.as_bytes().contains(&0x0E), "self-contained: SO (activate G1)");
    assert!(rendered.contains("\x1b[?1h"), "self-contained: DECCKM");
    assert!(rendered.contains("\x1b[?2004h"), "self-contained: bracketed paste");
    assert!(rendered.contains("\x1b[?1003h"), "self-contained: mouse mode");
    assert!(rendered.contains("\x1b[?1006h"), "self-contained: SGR encoding");
    assert!(rendered.contains("\x1b[?1004h"), "self-contained: focus reporting");
    assert!(rendered.contains("\x1b="), "self-contained: keypad app mode");
    assert!(rendered.contains("\x1b[5 q"), "self-contained: cursor shape");
    assert!(!rendered.contains("\x1b[?25h"), "self-contained: cursor hidden");
}

// ---------------------------------------------------------------
// Bug: history re-injection in alt screen causes accumulating
// blank lines in outer terminal scrollback on each reconnect
// ---------------------------------------------------------------

/// In alt screen mode, get_history() still returns main-screen scrollback.
/// But send_initial_state must NOT inject it — the scrollback is irrelevant
/// while the alt screen app (htop/vim) is running, and re-injecting on
/// every reconnect accumulates duplicate lines in the outer terminal.
#[test]
fn alt_screen_skips_history_on_reattach() {
    let mut screen = Screen::new(20, 5, 100);

    // Generate scrollback on main screen
    for i in 0..10 {
        screen.process(format!("line{}\r\n", i).as_bytes());
    }
    assert!(!screen.get_history().is_empty(), "should have scrollback");

    // Enter alt screen (htop)
    screen.process(b"\x1b[?1049h");
    screen.process(b"Alt content");
    assert!(screen.in_alt_screen());

    // Scrollback still exists internally
    assert!(!screen.get_history().is_empty(),
        "scrollback should persist internally in alt screen");

    // But in_alt_screen() should be true so callers can skip injection
    assert!(screen.in_alt_screen(),
        "in_alt_screen() must be true so send_initial_state skips history");
}

/// After exiting alt screen, history should be available again for injection.
#[test]
fn history_available_after_alt_screen_exit() {
    let mut screen = Screen::new(20, 5, 100);

    // Generate scrollback
    for i in 0..10 {
        screen.process(format!("line{}\r\n", i).as_bytes());
    }

    // Alt screen roundtrip
    screen.process(b"\x1b[?1049h");
    screen.process(b"\x1b[?1049l");

    assert!(!screen.in_alt_screen());
    assert!(!screen.get_history().is_empty(),
        "scrollback should be available after exiting alt screen");
}

/// Verify that the flush newlines (rows-1) are only emitted when history
/// is non-empty. With empty history (alt screen case), no flush should occur.
#[test]
fn no_flush_newlines_without_history() {
    let mut screen = Screen::new(20, 5, 100);
    screen.process(b"\x1b[?1049h"); // enter alt screen
    screen.process(b"Alt content");

    // Render as if on reattach — no history means no flush newlines
    let mut cache = RenderCache::new();
    let render = screen.render(true, &mut cache);

    // Count newlines before the first ESC sequence (flush newlines would be
    // raw \n bytes before the sync-begin \x1b[?2026h)
    let leading_newlines = render.iter().take_while(|&&b| b == b'\n').count();
    assert_eq!(leading_newlines, 0,
        "no flush newlines should be emitted when history is empty (alt screen)");
}

// ---------------------------------------------------------------
// Scroll region (DECSTBM) restoration on reattach
// ---------------------------------------------------------------

/// Helper: extract the DECSTBM sequence (ESC[top;bottomr) from render output.
/// Returns Some((top, bottom)) as 1-indexed values, or None if not found.
fn extract_decstbm(rendered: &str) -> Option<(u16, u16)> {
    let bytes = rendered.as_bytes();
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == 0x1b && bytes[i + 1] == b'[' {
            let start = i + 2;
            let mut j = start;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'r' {
                let params = &rendered[start..j];
                let parts: Vec<&str> = params.split(';').collect();
                if parts.len() == 2 {
                    if let (Ok(t), Ok(b)) = (parts[0].parse::<u16>(), parts[1].parse::<u16>()) {
                        return Some((t, b));
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// On reattach, a non-default scroll region must be restored via DECSTBM.
#[test]
fn reattach_restores_custom_scroll_region() {
    let mut screen = Screen::new(80, 24, 100);
    // Set scroll region to rows 2-23 (like htop: header at row 1, footer at row 24)
    screen.process(b"\x1b[2;23r");
    assert_eq!(screen.grid.scroll_top, 1);    // 0-based
    assert_eq!(screen.grid.scroll_bottom, 22); // 0-based

    let rendered = reattach_render(&screen);
    let decstbm = extract_decstbm(&rendered);
    assert_eq!(decstbm, Some((2, 23)),
        "reattach: DECSTBM should restore scroll region 2;23");
}

/// Even a full-screen scroll region should be emitted on full render
/// so the outer terminal's state is explicitly set.
#[test]
fn reattach_emits_full_screen_scroll_region() {
    let screen = Screen::new(80, 24, 100);
    // Default scroll region: full screen (1;24)
    let rendered = reattach_render(&screen);
    let decstbm = extract_decstbm(&rendered);
    assert_eq!(decstbm, Some((1, 24)),
        "reattach: DECSTBM should emit full-screen scroll region");
}

/// DECSTBM resets cursor to home, so it must appear BEFORE the final cursor
/// position sequence. Otherwise cursor would be wrong.
#[test]
fn reattach_scroll_region_before_cursor_position() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[2;23r");
    screen.process(b"\x1b[10;5H"); // position cursor at row 10, col 5

    let rendered = reattach_render(&screen);
    let decstbm_pos = rendered.find(";23r")
        .expect("DECSTBM should be present in render output");
    // The final cursor CUP: ESC[10;5H
    // Find it after the DECSTBM
    let after_decstbm = &rendered[decstbm_pos..];
    assert!(after_decstbm.contains("\x1b[10;5H"),
        "cursor position must appear AFTER DECSTBM (which resets cursor)");
}

/// Simulate htop layout: fixed header, scrollable middle, fixed footer.
/// After reattach, the scroll region must be preserved to keep the layout intact.
#[test]
fn reattach_htop_scroll_region_layout() {
    let mut screen = Screen::new(80, 24, 100);
    // Enter alt screen (htop)
    screen.process(b"\x1b[?1049h");
    // Set scroll region to rows 4-22 (header=rows 1-3, footer=row 23-24)
    screen.process(b"\x1b[4;22r");
    // Write header content (row 1)
    screen.process(b"\x1b[1;1HCPU [||||||||  50%]");
    // Write footer content (row 24)
    screen.process(b"\x1b[24;1HF1Help F2Setup F10Quit");
    // Position cursor in scrollable area
    screen.process(b"\x1b[10;1H");

    let rendered = reattach_render(&screen);
    let decstbm = extract_decstbm(&rendered);
    assert_eq!(decstbm, Some((4, 22)),
        "reattach: htop scroll region (4;22) must be restored");

    // Both header and footer content should be present
    assert!(rendered.contains("CPU"), "header content should be preserved");
    assert!(rendered.contains("F1Help"), "footer content should be preserved");
}

/// Incremental render should detect scroll region changes.
#[test]
fn mode_delta_detects_scroll_region_change() {
    let mut screen = Screen::new(80, 24, 100);
    let mut cache = RenderCache::new();
    // Initial full render (default scroll region)
    let _ = screen.render(true, &mut cache);

    // Now change scroll region
    screen.process(b"\x1b[5;20r");
    let result = screen.render(false, &mut cache);
    let text = String::from_utf8_lossy(&result);

    // Should contain the new DECSTBM
    let decstbm = extract_decstbm(&text);
    assert_eq!(decstbm, Some((5, 20)),
        "incremental render should detect scroll region change");
}

/// When scroll region hasn't changed, incremental render should skip DECSTBM.
#[test]
fn mode_delta_skips_unchanged_scroll_region() {
    let screen = Screen::new(80, 24, 100);
    let mut cache = RenderCache::new();
    // Initial full render
    let _ = screen.render(true, &mut cache);

    // Render again with no scroll region change
    let result = screen.render(false, &mut cache);
    let text = String::from_utf8_lossy(&result);

    // Should NOT contain DECSTBM (no change)
    let decstbm = extract_decstbm(&text);
    assert!(decstbm.is_none(),
        "unchanged scroll region should not emit DECSTBM on incremental render");
}

// ---------------------------------------------------------------
// VTE parser desync from data loss (channel eviction scenario)
// ---------------------------------------------------------------

/// Demonstrate that dropping PTY bytes mid-stream corrupts grid state.
/// This is the root cause of artifacts after reconnection: the eviction
/// path dropped unprocessed channel data, losing escape sequences.
#[test]
fn data_loss_corrupts_scroll_region() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[?1049h"); // enter alt screen

    // htop sets scroll region and draws header/footer
    screen.process(b"\x1b[4;22r");
    screen.process(b"\x1b[1;1HCPU [||||]");
    screen.process(b"\x1b[24;1HF1Help");

    // Now simulate a reconnection where DECSTBM is lost:
    // htop sends a batch with cursor positioning + DECSTBM + content,
    // but the DECSTBM bytes are in a channel chunk that gets dropped.
    let mut screen_with_loss = Screen::new(80, 24, 100);
    screen_with_loss.process(b"\x1b[?1049h");
    // SKIP: b"\x1b[4;22r" — lost in eviction
    screen_with_loss.process(b"\x1b[1;1HCPU [||||]");
    screen_with_loss.process(b"\x1b[24;1HF1Help");

    // Without the DECSTBM, scroll region defaults to full screen
    assert_eq!(screen_with_loss.grid.scroll_top, 0,
        "lost DECSTBM leaves scroll region at default");
    assert_eq!(screen_with_loss.grid.scroll_bottom, 23,
        "lost DECSTBM leaves scroll region at default");

    // Now when htop scrolls at the bottom of its expected scroll region,
    // the full screen scrolls instead, corrupting header/footer
    screen_with_loss.process(b"\x1b[22;1H"); // cursor at htop's scroll_bottom
    screen_with_loss.process(b"\n"); // LF — should scroll within region

    // With correct scroll region (4;22), row 3 would scroll out, header stays
    // With wrong scroll region (full screen), cursor just moves down (row 23)
    // because cursor_y (21) != scroll_bottom (23)
    // The real corruption happens over many cycles as htop's operations
    // assume a different scroll region than what the grid has.
    assert_ne!(screen.grid.scroll_top, screen_with_loss.grid.scroll_top,
        "data loss should cause scroll region mismatch");
}

/// Full data processing (no loss) keeps grid perfectly in sync.
#[test]
fn full_data_processing_keeps_grid_in_sync() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[?1049h");
    screen.process(b"\x1b[4;22r");
    screen.process(b"\x1b[1;1HCPU [||||||||  50%]");
    screen.process(b"\x1b[24;1HF1Help F2Setup F10Quit");

    // Fill process list area
    for i in 4..=22 {
        screen.process(format!("\x1b[{};1Hprocess_{:02}", i, i).as_bytes());
    }

    // Verify everything is correct
    assert_eq!(screen.grid.scroll_top, 3); // 0-based
    assert_eq!(screen.grid.scroll_bottom, 21); // 0-based
    assert_eq!(screen.grid.visible_row(0)[0].c, 'C'); // header
    assert_eq!(screen.grid.visible_row(23)[0].c, 'F'); // footer

    // Scroll within region — header and footer must be preserved
    screen.process(b"\x1b[22;1H\n"); // LF at scroll_bottom
    assert_eq!(screen.grid.visible_row(0)[0].c, 'C', "header must survive scroll");
    assert_eq!(screen.grid.visible_row(23)[0].c, 'F', "footer must survive scroll");
    // Row 3 (old top of region) should have shifted up
    assert_eq!(screen.grid.visible_row(3)[0].c, 'p', "row 4 content shifted to row 3");
}
