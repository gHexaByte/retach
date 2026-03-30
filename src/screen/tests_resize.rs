use super::*;

/// Helper: render a screen with full=true (simulates reattach) and return text.
fn reattach_render(screen: &Screen) -> String {
    let mut cache = RenderCache::new();
    let output = screen.render(true, &mut cache);
    String::from_utf8_lossy(&output).into_owned()
}

fn assert_cell(screen: &Screen, row: usize, col: usize, expected: char) {
    let actual = screen.grid.visible_row(row)[col].c;
    assert_eq!(actual, expected,
        "cell ({}, {}) expected '{}', got '{}'", row, col, expected, actual);
}

/// Helper: collect all visible lines as strings (first char of each row, trimmed).
fn collect_screen_lines(screen: &Screen) -> Vec<String> {
    (0..screen.grid.visible_row_count()).map(|y| {
        let s: String = screen.grid.visible_row(y).iter().map(|c| c.c).collect();
        s.trim_end().to_string()
    }).collect()
}

/// Helper: collect scrollback + screen as one ordered sequence of line strings.
fn collect_full_history(screen: &Screen) -> Vec<String> {
    let mut lines: Vec<String> = screen.get_history().iter()
        .map(|b| String::from_utf8_lossy(b).trim_end().to_string())
        .collect();
    lines.extend(collect_screen_lines(screen).into_iter().filter(|s| !s.is_empty()));
    lines
}

#[test]
fn resize_clears_wrap_pending() {
    let mut screen = Screen::new(5, 3, 100);
    screen.process(b"ABCDE"); // fill line, triggers wrap_pending
    assert!(screen.grid.wrap_pending());
    screen.resize(10, 3);
    assert!(!screen.grid.wrap_pending(),
        "wrap_pending should be cleared on resize");
}

#[test]
fn resize_horizontal_expand_preserves_text() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"ABCDEFGHIJ"); // fill row 0
    screen.resize(20, 3);
    for (i, ch) in "ABCDEFGHIJ".chars().enumerate() {
        assert_cell(&screen, 0, i, ch);
    }
    // Extended columns blank
    for c in 10..20 {
        assert_cell(&screen, 0, c, ' ');
    }
}

#[test]
fn resize_horizontal_shrink_preserves_visible_text() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"ABCDEFGHIJ");
    screen.resize(5, 3);
    for (i, ch) in "ABCDE".chars().enumerate() {
        assert_cell(&screen, 0, i, ch);
    }
}

#[test]
fn resize_vertical_expand_preserves_text() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"\x1b[1;1HRow1");
    screen.process(b"\x1b[2;1HRow2");
    screen.process(b"\x1b[3;1HRow3");
    screen.resize(10, 6);
    for (i, ch) in "Row1".chars().enumerate() { assert_cell(&screen, 0, i, ch); }
    for (i, ch) in "Row2".chars().enumerate() { assert_cell(&screen, 1, i, ch); }
    for (i, ch) in "Row3".chars().enumerate() { assert_cell(&screen, 2, i, ch); }
    // New rows blank
    for r in 3..6 {
        assert_cell(&screen, r, 0, ' ');
    }
}

#[test]
fn resize_vertical_shrink_preserves_visible_text() {
    let mut screen = Screen::new(10, 5, 100);
    for i in 1..=5 {
        screen.process(format!("\x1b[{};1HLine{}", i, i).as_bytes());
    }
    screen.resize(10, 3);
    // First 3 rows intact
    for (i, ch) in "Line1".chars().enumerate() { assert_cell(&screen, 0, i, ch); }
    for (i, ch) in "Line2".chars().enumerate() { assert_cell(&screen, 1, i, ch); }
    for (i, ch) in "Line3".chars().enumerate() { assert_cell(&screen, 2, i, ch); }
}

#[test]
fn resize_both_expand_preserves_text() {
    let mut screen = Screen::new(5, 3, 100);
    screen.process(b"\x1b[1;1Hab");
    screen.process(b"\x1b[2;1Hcd");
    screen.process(b"\x1b[3;1Hef");
    screen.resize(10, 6);
    assert_cell(&screen, 0, 0, 'a');
    assert_cell(&screen, 0, 1, 'b');
    assert_cell(&screen, 1, 0, 'c');
    assert_cell(&screen, 1, 1, 'd');
    assert_cell(&screen, 2, 0, 'e');
    assert_cell(&screen, 2, 1, 'f');
    // New areas blank
    assert_cell(&screen, 0, 5, ' ');
    assert_cell(&screen, 3, 0, ' ');
}

#[test]
fn resize_both_shrink_preserves_visible_text() {
    let mut screen = Screen::new(10, 5, 100);
    screen.process(b"\x1b[1;1HABCDEFGHIJ");
    screen.process(b"\x1b[2;1H0123456789");
    screen.resize(5, 2);
    for (i, ch) in "ABCDE".chars().enumerate() { assert_cell(&screen, 0, i, ch); }
    for (i, ch) in "01234".chars().enumerate() { assert_cell(&screen, 1, i, ch); }
}

#[test]
fn resize_expand_cols_shrink_rows_preserves_overlap() {
    let mut screen = Screen::new(5, 5, 100);
    screen.process(b"\x1b[1;1Hone");
    screen.process(b"\x1b[2;1Htwo");
    screen.process(b"\x1b[3;1Htri");
    screen.resize(10, 2);
    for (i, ch) in "one".chars().enumerate() { assert_cell(&screen, 0, i, ch); }
    for (i, ch) in "two".chars().enumerate() { assert_cell(&screen, 1, i, ch); }
    assert_cell(&screen, 0, 5, ' '); // new cols blank
}

#[test]
fn resize_shrink_cols_expand_rows_preserves_overlap() {
    let mut screen = Screen::new(10, 2, 100);
    screen.process(b"\x1b[1;1HABCDEFGHIJ");
    screen.process(b"\x1b[2;1H0123456789");
    screen.resize(4, 6);
    for (i, ch) in "ABCD".chars().enumerate() { assert_cell(&screen, 0, i, ch); }
    for (i, ch) in "0123".chars().enumerate() { assert_cell(&screen, 1, i, ch); }
    assert_cell(&screen, 2, 0, ' '); // new rows blank
}

#[test]
fn resize_reattach_render_preserves_content() {
    let mut screen = Screen::new(20, 5, 100);
    screen.process(b"\x1b[1;1HHello World");
    screen.process(b"\x1b[3;1HResize Me");
    screen.resize(30, 8); // expand both
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("Hello World"),
        "reattach after expand should contain 'Hello World'");
    assert!(rendered.contains("Resize Me"),
        "reattach after expand should contain 'Resize Me'");
}

#[test]
fn resize_reattach_render_shrink_then_expand() {
    let mut screen = Screen::new(20, 5, 100);
    screen.process(b"\x1b[1;1HKeep This");
    screen.process(b"\x1b[5;1HBottom");
    screen.resize(10, 3); // shrink: "Bottom" on row 5 lost, "Keep This" truncated to 10
    screen.resize(20, 5); // expand back
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("Keep This"),
        "surviving content should render after shrink+expand");
    assert!(!rendered.contains("Bottom"),
        "truncated row content should not reappear");
}

#[test]
fn resize_preserves_styled_content() {
    let mut screen = Screen::new(10, 3, 100);
    // Write bold red text
    screen.process(b"\x1b[1;31mSTYLED\x1b[0m");
    screen.resize(20, 5); // expand
    // Verify styled cells survived
    assert_cell(&screen, 0, 0, 'S');
    assert!(screen.cell_style(0, 0).bold, "bold should survive resize");
    let rendered = reattach_render(&screen);
    assert!(rendered.contains("STYLED"),
        "styled text should survive resize and render");
}

#[test]
fn resize_same_dimensions_is_noop_for_content() {
    let mut screen = Screen::new(10, 5, 100);
    for i in 1..=5 {
        screen.process(format!("\x1b[{};1HLine{}", i, i).as_bytes());
    }
    screen.resize(10, 5); // same
    for i in 1..=5 {
        let tag = format!("Line{}", i);
        for (j, ch) in tag.chars().enumerate() {
            assert_cell(&screen, i - 1, j, ch);
        }
    }
}

// ---------------------------------------------------------------
// Resize with off-screen content (scrollback)
// ---------------------------------------------------------------

#[test]
fn resize_after_scroll_preserves_scrollback() {
    // 3-row screen, write 6 lines => first 3 scroll off into scrollback
    let mut screen = Screen::new(15, 3, 100);
    for i in 1..=6 {
        screen.process(format!("Line{}\r\n", i).as_bytes());
    }
    let scrollback_before = screen.get_history();
    assert!(!scrollback_before.is_empty(), "scrollback should exist before resize");

    // Expand by 2 rows — 2 lines restored from scrollback
    screen.resize(20, 5);

    let scrollback_after = screen.get_history();
    // 2 lines restored to screen, so scrollback shrinks by 2
    assert_eq!(scrollback_after.len(), scrollback_before.len() - 2,
        "scrollback should shrink by restored line count");
    // Remaining scrollback lines should be the earliest ones (unchanged)
    for (i, (before, after)) in scrollback_before.iter().zip(scrollback_after.iter()).enumerate() {
        assert_eq!(before, after,
            "remaining scrollback line {} should be identical", i);
    }
}

#[test]
fn resize_after_scroll_preserves_visible_and_scrollback() {
    // Write 8 lines on a 3-row screen
    let mut screen = Screen::new(15, 3, 100);
    for i in 1..=8 {
        screen.process(format!("Msg{}\r\n", i).as_bytes());
    }

    // Scrollback should have lines that scrolled off
    let history_before = screen.get_history();
    // Expand by 3 rows (3 → 6): restores up to 3 lines from scrollback
    screen.resize(15, 6);

    let history_after = screen.get_history();
    // 3 lines restored from scrollback
    let restored = history_before.len() - history_after.len();
    assert_eq!(restored, 3, "should restore 3 lines from scrollback");
    // Remaining scrollback lines are the earliest (unchanged)
    for (i, (b, a)) in history_before.iter().zip(history_after.iter()).enumerate() {
        assert_eq!(b, a, "remaining scrollback line {} changed after expand", i);
    }

    // Visible content should include restored lines and original content
    let rendered = reattach_render(&screen);
    assert!(!rendered.is_empty(), "rendered content should not be empty after resize");
}

#[test]
fn resize_shrink_after_scroll_keeps_scrollback() {
    let mut screen = Screen::new(20, 5, 100);
    for i in 1..=10 {
        screen.process(format!("Item{}\r\n", i).as_bytes());
    }
    let history_before = screen.get_history();
    // Verify some content scrolled off
    assert!(history_before.len() >= 5,
        "expected at least 5 scrollback lines, got {}", history_before.len());

    // Shrink screen
    screen.resize(10, 3);

    let history_after = screen.get_history();
    assert_eq!(history_before.len(), history_after.len(),
        "scrollback should survive shrink resize");
    // Scrollback content byte-identical
    for (i, (b, a)) in history_before.iter().zip(history_after.iter()).enumerate() {
        assert_eq!(b, a, "scrollback line {} corrupted after shrink", i);
    }
}

#[test]
fn resize_scrollback_contains_correct_content() {
    // Write labeled lines, scroll some off, resize, verify labels in scrollback
    let mut screen = Screen::new(20, 3, 100);
    for i in 1..=7 {
        screen.process(format!("Tag{}\r\n", i).as_bytes());
    }
    let _ = screen.take_pending_scrollback();
    let history = screen.get_history();

    // Earliest scrollback lines should have Tag1, Tag2, ...
    let first_line = String::from_utf8_lossy(&history[0]);
    assert!(first_line.contains("Tag1"),
        "first scrollback line should contain Tag1, got: {}", first_line);

    // Now resize
    screen.resize(30, 6);
    let history_after = screen.get_history();
    let first_after = String::from_utf8_lossy(&history_after[0]);
    assert!(first_after.contains("Tag1"),
        "first scrollback line after resize should still contain Tag1, got: {}", first_after);
}

#[test]
fn resize_pending_scrollback_independent_of_horizontal_resize() {
    let mut screen = Screen::new(15, 3, 100);
    for i in 1..=5 {
        screen.process(format!("L{}\r\n", i).as_bytes());
    }
    // Don't drain pending — horizontal-only resize should not affect it
    let pending_before = screen.grid.scrollback_len() - screen.grid.pending_start();
    assert!(pending_before > 0, "pending scrollback should exist");

    screen.resize(20, 3); // same rows, different cols

    let pending_after = screen.grid.scrollback_len() - screen.grid.pending_start();
    assert_eq!(pending_before, pending_after,
        "pending scrollback count should survive horizontal-only resize");
}

#[test]
fn resize_vertical_grow_restores_scrollback_from_pending() {
    let mut screen = Screen::new(15, 3, 100);
    for i in 1..=5 {
        screen.process(format!("L{}\r\n", i).as_bytes());
    }
    let sb_before = screen.grid.scrollback_len();
    assert!(sb_before > 0, "scrollback should exist");

    // Growing vertically restores scrollback rows into visible area
    screen.resize(15, 5);

    let restored = sb_before - screen.grid.scrollback_len();
    assert!(restored > 0, "some scrollback should have been restored");
    // pending_start clamped to scrollback_len
    assert!(screen.grid.pending_start() <= screen.grid.scrollback_len());
}

#[test]
fn resize_expand_after_scroll_visible_cells_intact() {
    // 6-col x 3-row screen, fill and scroll, then expand
    let mut screen = Screen::new(6, 3, 100);
    screen.process(b"\x1b[1;1Haaa");
    screen.process(b"\x1b[2;1Hbbb");
    screen.process(b"\x1b[3;1Hccc");
    // Scroll up 1 — "aaa" goes to scrollback, "bbb" becomes row 0
    screen.process(b"\x1b[S");
    assert_cell(&screen, 0, 0, 'b');
    assert_cell(&screen, 1, 0, 'c');

    // Expand by 3 rows (3 → 6): restores 1 line ("aaa") from scrollback
    screen.resize(10, 6);
    // "aaa" restored at row 0, original content shifted down by 1
    assert_cell(&screen, 0, 0, 'a');
    assert_cell(&screen, 0, 1, 'a');
    assert_cell(&screen, 0, 2, 'a');
    assert_cell(&screen, 1, 0, 'b');
    assert_cell(&screen, 1, 1, 'b');
    assert_cell(&screen, 1, 2, 'b');
    assert_cell(&screen, 2, 0, 'c');
    assert_cell(&screen, 2, 1, 'c');
    assert_cell(&screen, 2, 2, 'c');
    // New rows blank
    assert_cell(&screen, 3, 0, ' ');
    assert_cell(&screen, 0, 6, ' ');

    // Scrollback should now be empty (1 line was restored)
    let history = screen.get_history();
    assert!(history.is_empty(), "scrollback should be empty after restore");
}

#[test]
fn resize_shrink_after_scroll_visible_cells_intact() {
    let mut screen = Screen::new(10, 4, 100);
    screen.process(b"\x1b[1;1HAAAA");
    screen.process(b"\x1b[2;1HBBBB");
    screen.process(b"\x1b[3;1HCCCC");
    screen.process(b"\x1b[4;1HDDDD");
    // Scroll up 2 — "AAAA" and "BBBB" go to scrollback
    screen.process(b"\x1b[2S");
    assert_cell(&screen, 0, 0, 'C');
    assert_cell(&screen, 1, 0, 'D');

    // Shrink
    screen.resize(6, 2);
    assert_cell(&screen, 0, 0, 'C');
    assert_cell(&screen, 0, 1, 'C');
    assert_cell(&screen, 1, 0, 'D');
    assert_cell(&screen, 1, 1, 'D');

    // Scrollback should still have the old lines
    let history = screen.get_history();
    let all_text: String = history.iter()
        .map(|l| String::from_utf8_lossy(l).into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all_text.contains("AAAA"), "scrollback should contain AAAA");
    assert!(all_text.contains("BBBB"), "scrollback should contain BBBB");
}

#[test]
fn resize_after_scroll_reattach_renders_correctly() {
    let mut screen = Screen::new(20, 3, 100);
    screen.process(b"\x1b[1;1HAlpha");
    screen.process(b"\x1b[2;1HBravo");
    screen.process(b"\x1b[3;1HCharlie");
    // Scroll 1 — "Alpha" off-screen
    screen.process(b"\x1b[S");
    // Now visible: Bravo (row 0), Charlie (row 1), blank (row 2)

    // Expand by 2 rows (3 → 5): restores "Alpha" from scrollback
    screen.resize(25, 5);
    let rendered = reattach_render(&screen);
    // Alpha is now restored to the screen
    assert!(rendered.contains("Alpha"),
        "Alpha should be restored to screen after expand");
    assert!(rendered.contains("Bravo"),
        "Bravo should be visible after scroll+expand");
    assert!(rendered.contains("Charlie"),
        "Charlie should be visible after scroll+expand");

    // Scrollback should be empty (Alpha was restored)
    let history = screen.get_history();
    assert!(history.is_empty(),
        "scrollback should be empty after all lines restored");
}

#[test]
fn resize_alt_screen_no_scrollback_leak() {
    let mut screen = Screen::new(10, 3, 100);
    // Write on main screen and scroll some content off
    screen.process(b"Main1\r\nMain2\r\nMain3\r\nMain4\r\n");
    let main_history = screen.get_history();

    // Enter alt screen, write content, resize
    screen.process(b"\x1b[?1049h");
    screen.process(b"AltContent");
    screen.resize(20, 5);

    // No new scrollback from alt screen resize
    let history_after = screen.get_history();
    assert_eq!(main_history.len(), history_after.len(),
        "alt screen resize should not add scrollback lines");
}

// ---------------------------------------------------------------
// Scrollback / screen boundary stitch tests
// ---------------------------------------------------------------

#[test]
fn resize_vertical_shrink_drops_bottom_rows_without_scrollback() {
    // Key behavior: vertical shrink pop_back()s bottom rows,
    // they do NOT go into scrollback — they're just lost.
    let mut screen = Screen::new(10, 5, 100);
    screen.process(b"\x1b[1;1HTop");
    screen.process(b"\x1b[5;1HBottom");
    let history_before = screen.get_history();

    // Shrink to 3 rows — rows 4 and 5 (including "Bottom") are dropped
    screen.resize(10, 3);

    let history_after = screen.get_history();
    assert_eq!(history_before.len(), history_after.len(),
        "vertical shrink should NOT add dropped rows to scrollback");
    // "Top" survives on screen, "Bottom" is gone everywhere
    assert_cell(&screen, 0, 0, 'T');
    let rendered = reattach_render(&screen);
    assert!(!rendered.contains("Bottom"),
        "Bottom row should be permanently lost after vertical shrink");
}

#[test]
fn scrollback_frozen_at_old_width_after_resize() {
    // Scrollback lines are ANSI bytes rendered at the width when they scrolled off.
    // After resize, they keep the old width — not re-rendered.
    let mut screen = Screen::new(20, 3, 100);
    // Write a long line and scroll it off
    screen.process(b"\x1b[1;1HABCDEFGHIJKLMNOPQRST"); // 20 chars, fills row
    screen.process(b"\x1b[2;1HSecondLine");
    screen.process(b"\x1b[3;1HThirdLine");
    screen.process(b"\x1b[S"); // scroll up — ABCDEF... goes to scrollback

    let history_before = screen.get_history();
    let scrollback_line = String::from_utf8_lossy(&history_before[0]);
    assert!(scrollback_line.contains("ABCDEFGHIJKLMNOPQRST"),
        "scrollback should contain full 20-char line");

    // Now resize to narrower terminal
    screen.resize(10, 3);

    // Scrollback should still have the full 20-char line (not truncated to 10)
    let history_after = screen.get_history();
    let scrollback_after = String::from_utf8_lossy(&history_after[0]);
    assert!(scrollback_after.contains("ABCDEFGHIJKLMNOPQRST"),
        "scrollback line should retain original width (20 chars), \
         not be truncated to new width (10). Got: {}", scrollback_after);
}

#[test]
fn scrollback_mixed_widths_after_multiple_resizes() {
    // Scroll off lines at different widths — all should be preserved as-is.
    let mut screen = Screen::new(15, 3, 100);
    screen.process(b"\x1b[1;1HWidth15_LineLn");
    screen.process(b"\x1b[S"); // scroll off at width 15

    screen.resize(10, 3);
    screen.process(b"\x1b[1;1HWidth10_Li");
    screen.process(b"\x1b[S"); // scroll off at width 10

    screen.resize(20, 3);
    screen.process(b"\x1b[1;1HWidth20_LineContent!");
    screen.process(b"\x1b[S"); // scroll off at width 20

    let history = screen.get_history();
    let lines: Vec<String> = history.iter()
        .map(|l| String::from_utf8_lossy(l).into_owned())
        .collect();

    assert!(lines[0].contains("Width15"),
        "first scrollback (width=15) should be preserved: {}", lines[0]);
    assert!(lines[1].contains("Width10"),
        "second scrollback (width=10) should be preserved: {}", lines[1]);
    assert!(lines[2].contains("Width20"),
        "third scrollback (width=20) should be preserved: {}", lines[2]);
}

#[test]
fn render_with_scrollback_after_resize_positions_correctly() {
    // After resize, render_with_scrollback should position cursor at
    // the NEW grid.rows, not the old one.
    let mut screen = Screen::new(20, 5, 100);
    screen.process(b"Line1\r\nLine2\r\nLine3\r\nLine4\r\nLine5\r\nLine6\r\n");
    let _ = screen.take_pending_scrollback();
    let history = screen.get_history();
    assert!(!history.is_empty());

    // Resize to 8 rows
    screen.resize(20, 8);
    let mut cache = RenderCache::new();
    let output = screen.render_with_scrollback(&history, &mut cache);
    let rendered = String::from_utf8_lossy(&output);

    // Cursor positioning for scrollback injection should use row 8 (new height).
    // Scrollback injection now happens BEFORE the sync block, so the scroll
    // position (\n at the bottom) should target the new row count.
    assert!(rendered.contains("\x1b[8;1H"),
        "scrollback injection should position at new row count (8), \
         rendered: {}", rendered.chars().take(200).collect::<String>());
    // Scrollback injection should appear before the sync block begins.
    let sync_start = rendered.find("\x1b[?2026h").unwrap();
    let scroll_pos = rendered.find("\x1b[8;1H").unwrap();
    assert!(scroll_pos < sync_start,
        "scrollback injection (pos {}) should appear before sync block (pos {})",
        scroll_pos, sync_start);
}

#[test]
fn render_with_scrollback_after_shrink_positions_correctly() {
    let mut screen = Screen::new(20, 6, 100);
    for i in 1..=10 {
        screen.process(format!("Row{}\r\n", i).as_bytes());
    }
    let history = screen.get_history();

    // Shrink to 3 rows
    screen.resize(20, 3);
    let mut cache = RenderCache::new();
    let output = screen.render_with_scrollback(&history, &mut cache);
    let rendered = String::from_utf8_lossy(&output);

    // Cursor should be at new height (3)
    assert!(rendered.contains("\x1b[3;1H"),
        "scrollback injection should position at shrunk row count (3)");
}

#[test]
fn reattach_with_scrollback_after_resize_has_both() {
    // Full reattach flow: content scrolled off → resize → render_with_scrollback
    // Both scrollback and screen content should appear in output.
    let mut screen = Screen::new(20, 3, 100);
    screen.process(b"\x1b[1;1HOldLine1");
    screen.process(b"\x1b[2;1HOldLine2");
    screen.process(b"\x1b[3;1HOldLine3");
    // Scroll 2 up — OldLine1, OldLine2 to scrollback
    screen.process(b"\x1b[2S");
    // Write new content on the freed rows
    screen.process(b"\x1b[2;1HNewLine4");
    screen.process(b"\x1b[3;1HNewLine5");

    let history = screen.get_history();
    let hist_text: String = history.iter()
        .map(|l| String::from_utf8_lossy(l).into_owned())
        .collect::<Vec<_>>()
        .join("|");
    assert!(hist_text.contains("OldLine1"), "scrollback should have OldLine1");
    assert!(hist_text.contains("OldLine2"), "scrollback should have OldLine2");

    // Resize
    screen.resize(25, 5);

    // Render with scrollback (as reattach would send to client)
    let mut cache = RenderCache::new();
    let output = screen.render_with_scrollback(&history, &mut cache);
    let rendered = String::from_utf8_lossy(&output);

    // Scrollback content in output (injected via cursor-at-bottom + \r\n)
    assert!(rendered.contains("OldLine1"),
        "reattach render should include scrollback OldLine1");
    assert!(rendered.contains("OldLine2"),
        "reattach render should include scrollback OldLine2");
    // Current screen content in output (after clear + redraw)
    assert!(rendered.contains("OldLine3"),
        "reattach render should include visible OldLine3");
    assert!(rendered.contains("NewLine4"),
        "reattach render should include visible NewLine4");
    assert!(rendered.contains("NewLine5"),
        "reattach render should include visible NewLine5");
}

#[test]
fn scroll_after_resize_captures_at_new_width() {
    // After resize, new scrollback lines should be rendered at the NEW width.
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"\x1b[1;1H0123456789"); // fill 10-col row

    // Resize to 20 cols, then add content and scroll
    screen.resize(20, 3);
    screen.process(b"\x1b[1;1HABCDEFGHIJ1234567890"); // 20-char line
    screen.process(b"\x1b[S"); // scroll off — should be rendered at width 20

    let history = screen.get_history();
    let last = String::from_utf8_lossy(&history[history.len() - 1]);
    assert!(last.contains("ABCDEFGHIJ1234567890"),
        "line scrolled off after resize should contain full 20-char content, got: {}", last);
}

#[test]
fn vertical_shrink_with_scrollback_then_reattach() {
    // Scrollback exists, then vertical shrink drops visible rows,
    // but scrollback is untouched. Reattach renders both correctly.
    let mut screen = Screen::new(15, 4, 100);
    // Generate scrollback
    for i in 1..=8 {
        screen.process(format!("Hist{}\r\n", i).as_bytes());
    }
    let scrollback_count = screen.get_history().len();
    assert!(scrollback_count > 0);

    // Now visible screen has some content; shrink drops bottom rows
    screen.process(b"\x1b[1;1HVisible1");
    screen.process(b"\x1b[4;1HVisible4");
    screen.resize(15, 2); // rows 3,4 dropped (including "Visible4")

    // Scrollback unchanged
    assert_eq!(screen.get_history().len(), scrollback_count,
        "scrollback count must not change on vertical shrink");

    // Reattach: scrollback + screen
    let history = screen.get_history();
    let mut cache = RenderCache::new();
    let output = screen.render_with_scrollback(&history, &mut cache);
    let rendered = String::from_utf8_lossy(&output);

    assert!(rendered.contains("Visible1"),
        "surviving top row should render");
    assert!(!rendered.contains("Visible4"),
        "dropped bottom row should not render");
    // Scrollback lines should be present
    assert!(rendered.contains("Hist1"),
        "scrollback should be in reattach output");
}

#[test]
fn scrollback_not_duplicated_on_resize() {
    // Resize must not duplicate scrollback lines — only restore or leave unchanged.
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"A\r\nB\r\nC\r\nD\r\nE\r\n");
    let count_before = screen.get_history().len();

    // expand 3→5: restores up to 2 lines from scrollback
    screen.resize(20, 5);
    let after_expand = screen.get_history().len();
    assert!(after_expand <= count_before,
        "scrollback should not grow on expand");

    // shrink 5→2: no scrollback change (current behavior)
    screen.resize(5, 2);
    let after_shrink = screen.get_history().len();
    assert_eq!(after_shrink, after_expand,
        "scrollback should not change on shrink");

    // expand 2→3: may restore 1 more if available
    screen.resize(10, 3);
    let after_reexpand = screen.get_history().len();
    assert!(after_reexpand <= after_shrink,
        "scrollback should not grow on re-expand");
}

// ---------------------------------------------------------------
// 1. Wide characters at column boundary during horizontal shrink
// ---------------------------------------------------------------

#[test]
fn resize_shrink_splits_wide_char_at_boundary() {
    // Wide char occupies cols 4-5 (width=2 + continuation width=0).
    // Shrink to 5 cols → continuation at col 5 is truncated.
    // The orphaned width=2 cell at col 4 must be cleaned up.
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"\x1b[1;5H"); // col 5 (0-indexed col 4)
    screen.process("你".as_bytes()); // wide char at cols 4-5
    assert_eq!(screen.grid.visible_row(0)[4].width, 2);
    assert_eq!(screen.grid.visible_row(0)[5].width, 0);

    screen.resize(5, 3); // shrink — col 5 gone, col 4 is last
    // The orphaned width=2 cell should not remain broken.
    // It either gets blanked or its width becomes 1.
    let cell4 = &screen.grid.visible_row(0)[4];
    assert_ne!(cell4.width, 2,
        "orphaned wide char (width=2 without continuation) should be cleaned up, \
         got width={} char='{}'", cell4.width, cell4.c);
}

#[test]
fn resize_shrink_wide_char_fully_inside_survives() {
    // Wide char at cols 2-3, shrink to 6 cols — fully inside, should survive.
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"\x1b[1;3H"); // col 3 (0-indexed col 2)
    screen.process("世".as_bytes()); // wide char at cols 2-3
    assert_eq!(screen.grid.visible_row(0)[2].c, '世');
    assert_eq!(screen.grid.visible_row(0)[2].width, 2);
    assert_eq!(screen.grid.visible_row(0)[3].width, 0);

    screen.resize(6, 3);
    assert_eq!(screen.grid.visible_row(0)[2].c, '世');
    assert_eq!(screen.grid.visible_row(0)[2].width, 2);
    assert_eq!(screen.grid.visible_row(0)[3].width, 0,
        "wide char fully inside new width should survive intact");
}

#[test]
fn resize_shrink_wide_char_at_exact_right_edge() {
    // Wide char at cols (cols-2, cols-1) — exactly fills the right edge.
    // Shrink by 1 col → continuation lost.
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"\x1b[1;9H"); // col 9 (0-indexed col 8)
    screen.process("界".as_bytes()); // wide char at cols 8-9
    assert_eq!(screen.grid.visible_row(0)[8].width, 2);
    assert_eq!(screen.grid.visible_row(0)[9].width, 0);

    screen.resize(9, 3); // shrink — col 9 gone
    let cell8 = &screen.grid.visible_row(0)[8];
    assert_ne!(cell8.width, 2,
        "wide char at right edge with truncated continuation should be cleaned up");
}

#[test]
fn resize_shrink_multiple_wide_chars_on_boundary() {
    // Multiple wide chars, some split by resize boundary.
    let mut screen = Screen::new(12, 3, 100);
    // Place wide chars at cols 0-1, 2-3, 4-5, 6-7, 8-9, 10-11
    screen.process("你好世界很棒".as_bytes());
    // Shrink to 7 cols — wide char at cols 6-7 is split (col 7 lost)
    screen.resize(7, 3);
    // Chars at 0-1, 2-3, 4-5 should survive
    assert_eq!(screen.grid.visible_row(0)[0].c, '你');
    assert_eq!(screen.grid.visible_row(0)[0].width, 2);
    assert_eq!(screen.grid.visible_row(0)[2].c, '好');
    assert_eq!(screen.grid.visible_row(0)[4].c, '世');
    // Col 6 had '界' (width=2) with continuation at col 7 — now orphaned
    assert_ne!(screen.grid.visible_row(0)[6].width, 2,
        "wide char split at resize boundary should be cleaned up");
}

// ---------------------------------------------------------------
// 2. Combining marks + wide chars + resize
// ---------------------------------------------------------------

#[test]
fn resize_preserves_combining_marks() {
    let mut screen = Screen::new(10, 3, 100);
    // 'e' followed by combining acute accent U+0301
    screen.process("e\u{0301}".as_bytes());
    assert_eq!(screen.grid.visible_row(0)[0].c, 'e');
    assert_eq!(screen.grid.visible_row(0).combining(0), &['\u{0301}']);

    screen.resize(20, 5);
    assert_eq!(screen.grid.visible_row(0)[0].c, 'e');
    assert_eq!(screen.grid.visible_row(0).combining(0), &['\u{0301}'],
        "combining marks should survive resize expand");

    screen.resize(5, 2);
    assert_eq!(screen.grid.visible_row(0)[0].c, 'e');
    assert_eq!(screen.grid.visible_row(0).combining(0), &['\u{0301}'],
        "combining marks should survive resize shrink");
}

#[test]
fn resize_wide_char_with_combining_survives() {
    let mut screen = Screen::new(10, 3, 100);
    // Wide char '你' followed by combining mark
    screen.process("你\u{0308}".as_bytes()); // 你 + diaeresis
    assert_eq!(screen.grid.visible_row(0)[0].c, '你');
    assert_eq!(screen.grid.visible_row(0)[0].width, 2);
    assert!(screen.grid.visible_row(0).combining(0).contains(&'\u{0308}'));

    screen.resize(15, 3); // expand — should survive
    assert_eq!(screen.grid.visible_row(0)[0].c, '你');
    assert_eq!(screen.grid.visible_row(0)[0].width, 2);
    assert!(screen.grid.visible_row(0).combining(0).contains(&'\u{0308}'),
        "combining mark on wide char should survive resize");
}

// ---------------------------------------------------------------
// 3. Scroll region + content survival
// ---------------------------------------------------------------

#[test]
fn resize_with_scroll_region_preserves_region_content() {
    let mut screen = Screen::new(20, 10, 100);
    // Set scroll region to rows 3-7 (1-indexed)
    screen.process(b"\x1b[3;7r");
    // Write content in every row
    for i in 1..=10 {
        screen.process(format!("\x1b[{};1HR{}", i, i).as_bytes());
    }
    // Resize — scroll region resets, but content should survive
    screen.resize(20, 6);
    // Rows 1-6 (0-indexed 0-5) should have their content
    for (i, ch) in "R1".chars().enumerate() { assert_cell(&screen, 0, i, ch); }
    for (i, ch) in "R2".chars().enumerate() { assert_cell(&screen, 1, i, ch); }
    for (i, ch) in "R3".chars().enumerate() { assert_cell(&screen, 2, i, ch); }
    for (i, ch) in "R4".chars().enumerate() { assert_cell(&screen, 3, i, ch); }
    for (i, ch) in "R5".chars().enumerate() { assert_cell(&screen, 4, i, ch); }
    for (i, ch) in "R6".chars().enumerate() { assert_cell(&screen, 5, i, ch); }
    // Scroll region should be reset to full screen
    assert_eq!(screen.grid.scroll_top(), 0);
    assert_eq!(screen.grid.scroll_bottom(), 5);
}

#[test]
fn resize_after_scroll_within_region() {
    let mut screen = Screen::new(15, 6, 100);
    // Set scroll region rows 2-4
    screen.process(b"\x1b[2;4r");
    // Write inside region
    screen.process(b"\x1b[2;1HInR2");
    screen.process(b"\x1b[3;1HInR3");
    screen.process(b"\x1b[4;1HInR4");
    // Write outside region
    screen.process(b"\x1b[1;1HOutR1");
    screen.process(b"\x1b[6;1HOutR6");

    // Scroll within region
    screen.process(b"\x1b[2;4r"); // re-set region
    screen.process(b"\x1b[4;1H"); // move to bottom of region
    screen.process(b"\r\n");       // scroll within region

    screen.resize(15, 4);
    // Outside-region row 1 should survive
    for (i, ch) in "OutR1".chars().enumerate() { assert_cell(&screen, 0, i, ch); }
    // Scroll region should be reset
    assert_eq!(screen.grid.scroll_top(), 0);
    assert_eq!(screen.grid.scroll_bottom(), 3);
}

// ---------------------------------------------------------------
// 4. Saved cursor (DECSC/DECRC) across resize
// ---------------------------------------------------------------

#[test]
fn resize_between_decsc_decrc_clamps_cursor() {
    let mut screen = Screen::new(80, 24, 100);
    // Save cursor at (79, 23) — bottom-right corner
    screen.process(b"\x1b[24;80H"); // 1-indexed
    screen.process(b"\x1b7");        // DECSC
    assert_eq!(screen.grid.cursor_x(), 79);
    assert_eq!(screen.grid.cursor_y(), 23);

    // Move cursor elsewhere
    screen.process(b"\x1b[1;1H");

    // Resize to smaller
    screen.resize(40, 12);

    // Restore cursor — should clamp to (39, 11)
    screen.process(b"\x1b8"); // DECRC
    assert_eq!(screen.grid.cursor_x(), 39,
        "restored cursor_x should clamp to cols-1 after resize");
    assert_eq!(screen.grid.cursor_y(), 11,
        "restored cursor_y should clamp to rows-1 after resize");
}

#[test]
fn resize_between_csi_s_u_clamps_cursor() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[20;70H"); // row 20, col 70 (1-indexed)
    screen.process(b"\x1b[s");       // CSI s — save
    screen.process(b"\x1b[1;1H");    // move away

    screen.resize(30, 10);

    screen.process(b"\x1b[u");       // CSI u — restore
    assert_eq!(screen.grid.cursor_x(), 29,
        "CSI u cursor_x should clamp to cols-1 after resize");
    assert_eq!(screen.grid.cursor_y(), 9,
        "CSI u cursor_y should clamp to rows-1 after resize");
}

#[test]
fn resize_expand_between_save_restore_preserves_cursor() {
    let mut screen = Screen::new(40, 12, 100);
    screen.process(b"\x1b[5;10H"); // save at (9, 4)
    screen.process(b"\x1b7");
    screen.process(b"\x1b[1;1H");

    screen.resize(80, 24); // expand

    screen.process(b"\x1b8"); // restore
    assert_eq!(screen.grid.cursor_x(), 9,
        "cursor_x within bounds should not change after expand");
    assert_eq!(screen.grid.cursor_y(), 4,
        "cursor_y within bounds should not change after expand");
}

#[test]
fn resize_saved_cursor_style_preserved() {
    let mut screen = Screen::new(20, 5, 100);
    // Set bold+red style, then save
    screen.process(b"\x1b[1;31m");
    screen.process(b"\x1b[5;10H");
    screen.process(b"\x1b7"); // DECSC saves position + style

    screen.process(b"\x1b[0m\x1b[1;1H"); // reset style, move away
    screen.resize(10, 3); // shrink

    screen.process(b"\x1b8"); // restore
    assert!(screen.current_style().bold,
        "restored style should be bold after resize");
}

// ---------------------------------------------------------------
// 5. Alt screen saved_grid integrity after resize
// ---------------------------------------------------------------

#[test]
fn resize_in_alt_screen_then_exit_restores_main_resized() {
    let mut screen = Screen::new(20, 5, 100);
    // Write on main screen
    screen.process(b"\x1b[1;1HMainContent");
    screen.process(b"\x1b[3;1HRow3Data");

    // Enter alt screen
    screen.process(b"\x1b[?1049h");
    assert!(screen.in_alt_screen());
    screen.process(b"AltText");

    // Resize while in alt screen (saved_grid is 20x5, grid becomes 10x3)
    screen.resize(10, 3);

    // Exit alt screen — saved_grid (20x5) must be adjusted to (10x3)
    screen.process(b"\x1b[?1049l");
    assert!(!screen.in_alt_screen());

    // Grid dimensions should match resize
    assert_eq!(screen.grid.visible_row_count(), 3);
    assert_eq!(screen.grid.visible_row(0).len(), 10);
    // Main content that fits in 10x3 should be restored
    for (i, ch) in "MainConten".chars().enumerate() {
        assert_cell(&screen, 0, i, ch);
    }
}

#[test]
fn resize_in_alt_screen_expand_then_exit() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"\x1b[1;1HSmall");
    screen.process(b"\x1b[?1049h"); // enter alt
    screen.resize(20, 6); // expand
    screen.process(b"\x1b[?1049l"); // exit alt

    assert_eq!(screen.grid.visible_row_count(), 6);
    assert_eq!(screen.grid.visible_row(0).len(), 20);
    // Original content should be on row 0
    for (i, ch) in "Small".chars().enumerate() {
        assert_cell(&screen, 0, i, ch);
    }
    // Expanded rows/cols should be blank
    assert_cell(&screen, 3, 0, ' ');
    assert_cell(&screen, 0, 10, ' ');
}

#[test]
fn resize_in_alt_screen_multiple_then_exit() {
    let mut screen = Screen::new(20, 5, 100);
    screen.process(b"\x1b[1;1HOriginal");
    screen.process(b"\x1b[?1049h"); // enter alt

    screen.resize(10, 3);  // shrink
    screen.resize(30, 8);  // expand
    screen.resize(15, 4);  // settle

    screen.process(b"\x1b[?1049l"); // exit alt

    assert_eq!(screen.grid.visible_row_count(), 4);
    assert_eq!(screen.grid.visible_row(0).len(), 15);
    // "Original" (8 chars) fits in 15 cols
    for (i, ch) in "Original".chars().enumerate() {
        assert_cell(&screen, 0, i, ch);
    }
}

// ---------------------------------------------------------------
// 6. SGR state / current_style persistence across resize
// ---------------------------------------------------------------

#[test]
fn resize_current_style_persists_for_new_content() {
    let mut screen = Screen::new(10, 3, 100);
    // Set bold red
    screen.process(b"\x1b[1;31m");
    screen.process(b"AB"); // write styled text
    assert!(screen.cell_style(0, 0).bold);

    screen.resize(20, 5);

    // Write more text — should inherit the pre-resize style
    screen.process(b"CD");
    assert!(screen.cell_style(0, 2).bold,
        "new text after resize should inherit bold from pre-resize style");
    assert_eq!(screen.grid.visible_row(0)[2].c, 'C');
}

#[test]
fn resize_does_not_reset_sgr_state() {
    let mut screen = Screen::new(20, 5, 100);
    // Set complex style: bold + italic + underline + fg=green
    screen.process(b"\x1b[1;3;4;32m");
    screen.process(b"X");

    let style_before = screen.current_style();

    screen.resize(10, 3);

    assert_eq!(screen.current_style(), style_before,
        "current_style should not change on resize");

    // Write after resize — same style
    screen.process(b"Y");
    assert_eq!(screen.cell_style(0, 1), style_before,
        "text written after resize should have identical style");
}

// ---------------------------------------------------------------
// 7. Tab stops with content after resize
// ---------------------------------------------------------------

#[test]
fn resize_tab_content_preserved_but_stops_reset() {
    let mut screen = Screen::new(40, 3, 100);
    // Write tab-separated content
    screen.process(b"A\tB\tC");
    // A at col 0, B at col 8, C at col 16
    assert_cell(&screen, 0, 0, 'A');
    assert_cell(&screen, 0, 8, 'B');
    assert_cell(&screen, 0, 16, 'C');

    screen.resize(20, 3);
    // Content at those positions should survive
    assert_cell(&screen, 0, 0, 'A');
    assert_cell(&screen, 0, 8, 'B');
    assert_cell(&screen, 0, 16, 'C');

    // But tab stops are default for new width
    assert_eq!(screen.grid.tab_stops_len(), 20);
    assert!(screen.grid.tab_stop_at(8));
    assert!(screen.grid.tab_stop_at(16));
}

#[test]
fn resize_custom_tab_stop_lost() {
    let mut screen = Screen::new(40, 3, 100);
    // Set custom tab stop at column 5 (move to col 5, ESC H = set tab)
    screen.process(b"\x1b[1;6H"); // col 6, 1-indexed = col 5, 0-indexed
    screen.process(b"\x1bH");      // HTS — set tab stop
    // Write using tab to verify
    screen.process(b"\x1b[1;1H");
    screen.process(b"X\tY"); // X at col 0, Y at col 5 (custom stop)
    assert_cell(&screen, 0, 5, 'Y');

    screen.resize(30, 3);
    // Content stays, but custom tab stop is gone
    assert_cell(&screen, 0, 5, 'Y');
    // Tab at col 5 should NOT be set (only defaults at 8, 16, 24)
    assert!(!screen.grid.tab_stop_at(5),
        "custom tab stop at col 5 should be gone after resize");
}

// ---------------------------------------------------------------
// 8. Cursor at all four corners + resize
// ---------------------------------------------------------------

#[test]
fn resize_cursor_at_top_left_stays() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[1;1H"); // top-left
    screen.resize(40, 12);
    assert_eq!(screen.grid.cursor_x(), 0);
    assert_eq!(screen.grid.cursor_y(), 0);
}

#[test]
fn resize_cursor_at_top_right_clamps() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[1;80H"); // top-right
    assert_eq!(screen.grid.cursor_x(), 79);
    screen.resize(40, 12);
    assert_eq!(screen.grid.cursor_x(), 39);
    assert_eq!(screen.grid.cursor_y(), 0);
}

#[test]
fn resize_cursor_at_bottom_left_clamps() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[24;1H"); // bottom-left
    assert_eq!(screen.grid.cursor_y(), 23);
    screen.resize(40, 12);
    assert_eq!(screen.grid.cursor_x(), 0);
    assert_eq!(screen.grid.cursor_y(), 11);
}

#[test]
fn resize_cursor_at_bottom_right_clamps() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[24;80H"); // bottom-right
    screen.resize(40, 12);
    assert_eq!(screen.grid.cursor_x(), 39);
    assert_eq!(screen.grid.cursor_y(), 11);
}

#[test]
fn resize_extreme_shrink_cursor_to_origin() {
    let mut screen = Screen::new(100, 50, 100);
    screen.process(b"\x1b[50;100H"); // very far corner
    screen.resize(1, 1);
    assert_eq!(screen.grid.cursor_x(), 0);
    assert_eq!(screen.grid.cursor_y(), 0);
}

// ---------------------------------------------------------------
// 9. Render cache invalidation after resize
// ---------------------------------------------------------------

#[test]
fn resize_invalidates_render_cache() {
    let mut screen = Screen::new(20, 5, 100);
    screen.process(b"\x1b[1;1HCached");

    // First render — populates cache
    let mut cache = RenderCache::new();
    let first = screen.render(true, &mut cache);
    let first_str = String::from_utf8_lossy(&first);
    assert!(first_str.contains("Cached"));

    // Resize
    screen.resize(30, 8);
    screen.process(b"\x1b[6;1HNewRow");

    // Incremental render — cache row count mismatch should force redraw
    let second = screen.render(false, &mut cache);
    let second_str = String::from_utf8_lossy(&second);
    assert!(second_str.contains("NewRow"),
        "incremental render after resize should include new content \
         (cache should be invalidated/resized)");
}

#[test]
fn resize_render_cache_shrink_then_expand() {
    let mut screen = Screen::new(20, 6, 100);
    for i in 1..=6 {
        screen.process(format!("\x1b[{};1HR{}", i, i).as_bytes());
    }
    let mut cache = RenderCache::new();
    let _ = screen.render(true, &mut cache);

    // Shrink — cache truncates from 6 to 3 entries.
    // Surviving rows are unchanged, so incremental render correctly
    // skips them (cache hit). Full render should include them.
    screen.resize(20, 3);
    let r2 = screen.render(true, &mut cache);
    let r2_str = String::from_utf8_lossy(&r2);
    assert!(r2_str.contains("R1"),
        "full render after shrink should include surviving content");

    // Expand back — new rows get sentinel hashes, forcing redraw
    screen.resize(20, 6);
    screen.process(b"\x1b[5;1HNew5");
    let r3 = screen.render(false, &mut cache);
    let r3_str = String::from_utf8_lossy(&r3);
    assert!(r3_str.contains("New5"),
        "incremental render after expand should redraw new rows (sentinel cache)");
}

// ---------------------------------------------------------------
// 10. Mid-escape-sequence resize
// ---------------------------------------------------------------

#[test]
fn resize_mid_csi_sequence_completes_after() {
    let mut screen = Screen::new(20, 5, 100);
    // Start a CSI sequence but don't finish it
    screen.process(b"\x1b["); // CSI begun, no final char yet

    // Resize while parser is mid-sequence
    screen.resize(10, 3);

    // Complete the sequence: "5;3H" = move cursor to row 5, col 3
    // But after resize, rows=3, so row 5 would be clamped
    screen.process(b"3;5H");
    assert_eq!(screen.grid.cursor_y(), 2, "cursor row should be clamped to rows-1");
    assert_eq!(screen.grid.cursor_x(), 4, "cursor col within bounds");
}

#[test]
fn resize_mid_osc_sequence_completes_after() {
    let mut screen = Screen::new(20, 5, 100);
    // Start OSC title set
    screen.process(b"\x1b]2;My Ti"); // incomplete title

    screen.resize(10, 3);

    // Complete it
    screen.process(b"tle\x07"); // BEL terminates OSC
    assert_eq!(screen.title(), "My Title",
        "title should be set correctly despite resize mid-OSC");
}

#[test]
fn resize_mid_sgr_sequence_style_applied_after() {
    let mut screen = Screen::new(20, 5, 100);
    // Start SGR but don't finish
    screen.process(b"\x1b[1;3"); // bold + partial "31" (red)

    screen.resize(10, 3);

    // Finish: "1m" → complete SGR is [1;31m = bold + red
    screen.process(b"1m");
    screen.process(b"X");
    assert!(screen.cell_style(0, 0).bold,
        "bold should be applied despite resize mid-SGR");
}

// ---------------------------------------------------------------
// 11. Alt screen saved modes after resize
// ---------------------------------------------------------------

#[test]
fn resize_in_alt_screen_modes_restored_correctly() {
    let mut screen = Screen::new(20, 5, 100);
    // Set some modes on main screen
    screen.process(b"\x1b[?2004h"); // bracketed paste
    screen.process(b"\x1b[?1000h"); // mouse mode
    assert!(screen.grid.modes().bracketed_paste);
    assert!(screen.grid.modes().mouse_modes.click);

    // Enter alt screen (saves modes)
    screen.process(b"\x1b[?1049h");
    // Change modes in alt screen
    screen.process(b"\x1b[?2004l"); // disable bracketed paste
    assert!(!screen.grid.modes().bracketed_paste);

    // Resize
    screen.resize(10, 3);

    // Exit alt screen — modes should be restored from saved state
    screen.process(b"\x1b[?1049l");
    assert!(screen.grid.modes().bracketed_paste,
        "bracketed paste should be restored from saved modes after resize");
    assert!(screen.grid.modes().mouse_modes.click,
        "mouse mode should be restored from saved modes after resize");
    // Scroll region should be reset to new dimensions
    assert_eq!(screen.grid.scroll_top(), 0);
    assert_eq!(screen.grid.scroll_bottom(), 2);
}

// ---------------------------------------------------------------
// 12. Stress: multiple rapid resizes with complex content
// ---------------------------------------------------------------

#[test]
fn resize_rapid_with_mixed_content() {
    let mut screen = Screen::new(20, 5, 100);
    // Write styled wide chars + combining marks + regular text
    screen.process(b"\x1b[1;31m"); // bold red
    screen.process("A你e\u{0301}B".as_bytes()); // A + wide + combining + B
    screen.process(b"\x1b[0m");
    screen.process(b"\x1b[3;1HRow3");

    // Rapid resize sequence
    screen.resize(10, 3);
    screen.resize(30, 8);
    screen.resize(5, 2);
    screen.resize(15, 4);
    screen.resize(20, 5);

    // After settling back to original size, verify no crash and
    // content that survived all the shrinks is correct
    assert_eq!(screen.grid.visible_row_count(), 5);
    assert_eq!(screen.grid.visible_row(0).len(), 20);
    // 'A' at col 0 should survive all resizes (always within bounds)
    assert_cell(&screen, 0, 0, 'A');
    assert!(screen.cell_style(0, 0).bold,
        "style should survive rapid resizes");
}

// ---------------------------------------------------------------
// Scrollback restoration on vertical expand
// ---------------------------------------------------------------

#[test]
fn resize_vertical_expand_restores_scrollback() {
    let mut screen = Screen::new(10, 3, 100);
    // 5 lines → 2 scroll off, 3 visible
    screen.process(b"Line1\r\nLine2\r\nLine3\r\nLine4\r\nLine5");
    // scrollback=[Line1, Line2], grid=[Line3, Line4, Line5]

    screen.resize(10, 5);

    // Restored: Line1 at row 0, Line2 at row 1, original content shifted down
    assert_eq!(screen.grid.visible_row(0)[0].c, 'L');
    assert_eq!(screen.grid.visible_row(0)[4].c, '1');
    assert_eq!(screen.grid.visible_row(1)[4].c, '2');
    assert_eq!(screen.grid.visible_row(2)[4].c, '3');
    assert_eq!(screen.grid.visible_row(3)[4].c, '4');
    assert_eq!(screen.grid.visible_row(4)[4].c, '5');
}

#[test]
fn resize_vertical_expand_shifts_cursor() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"Line1\r\nLine2\r\nLine3\r\nLine4\r\nLine5");
    assert_eq!(screen.grid.cursor_y(), 2);
    let old_x = screen.grid.cursor_x();

    screen.resize(10, 5);

    // 2 lines restored → cursor shifted down by 2
    assert_eq!(screen.grid.cursor_y(), 4);
    assert_eq!(screen.grid.cursor_x(), old_x);
}

#[test]
fn resize_vertical_expand_limited_by_scrollback() {
    let mut screen = Screen::new(10, 3, 100);
    // Only 1 line scrolls off
    screen.process(b"AAA\r\nBBB\r\nCCC\r\nDDD");
    // scrollback=[AAA], grid=[BBB, CCC, DDD]

    screen.resize(10, 7); // grow by 4, but only 1 in scrollback

    // Row 0: restored AAA
    assert_eq!(screen.grid.visible_row(0)[0].c, 'A');
    // Row 1: BBB (shifted by 1)
    assert_eq!(screen.grid.visible_row(1)[0].c, 'B');
    // Rows 4-6: blank (not enough scrollback)
    for r in 4..7 {
        assert_eq!(screen.grid.visible_row(r)[0].c, ' ',
            "row {} should be blank", r);
    }
    // Cursor shifted by 1
    assert_eq!(screen.grid.cursor_y(), 3);
}

#[test]
fn resize_vertical_expand_no_restore_in_alt_screen() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"Line1\r\nLine2\r\nLine3\r\nLine4");
    // scrollback has Line1

    screen.process(b"\x1b[?1049h"); // enter alt screen
    screen.resize(10, 5);

    // No restoration in alt screen — rows should be blank
    for r in 0..5 {
        assert_eq!(screen.grid.visible_row(r)[0].c, ' ',
            "row {} should be blank in alt screen", r);
    }
}

#[test]
fn resize_vertical_expand_removes_restored_from_scrollback() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"L1\r\nL2\r\nL3\r\nL4\r\nL5");
    // scrollback=[L1, L2]
    assert_eq!(screen.get_history().len(), 2);

    screen.resize(10, 5); // restore both

    // Scrollback should be empty now
    assert_eq!(screen.get_history().len(), 0);
}

#[test]
fn resize_same_height_no_scrollback_restore() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"L1\r\nL2\r\nL3\r\nL4\r\nL5");
    let hist_before = screen.get_history().len();

    screen.resize(10, 3); // same height
    assert_eq!(screen.get_history().len(), hist_before);
}

#[test]
fn resize_scrollback_screen_boundary_integrity() {
    // Write 10 labeled lines on a 3-row screen.
    // scrollback gets 7 lines, screen has 3.
    // Verify that expand/shrink cycles keep the full sequence intact
    // at the scrollback↔screen boundary.
    let mut screen = Screen::new(10, 3, 100);
    for i in 1..=10 {
        if i < 10 {
            screen.process(format!("L{:02}\r\n", i).as_bytes());
        } else {
            screen.process(format!("L{:02}", i).as_bytes());
        }
    }
    // Expected state: scrollback=[L01..L07], screen=[L08, L09, L10]
    let full_before = collect_full_history(&screen);
    assert_eq!(full_before.len(), 10);
    for (i, line) in full_before.iter().enumerate() {
        assert!(line.contains(&format!("L{:02}", i + 1)),
            "line {} should contain L{:02}, got: '{}'", i, i + 1, line);
    }

    // --- Expand 3→7: restores 4 lines from scrollback ---
    screen.resize(10, 7);
    let full_after_expand = collect_full_history(&screen);
    // Total content must be the same 10 lines in order
    assert_eq!(full_after_expand.len(), 10,
        "total line count should stay 10 after expand, got: {:?}", full_after_expand);
    for (i, line) in full_after_expand.iter().enumerate() {
        assert!(line.contains(&format!("L{:02}", i + 1)),
            "after expand: line {} should contain L{:02}, got: '{}'", i, i + 1, line);
    }
    // Boundary check: scrollback now has 3 lines, screen row 0 should be L04
    assert_eq!(screen.get_history().len(), 3);
    assert_eq!(screen.grid.visible_row(0)[0].c, 'L');
    assert_eq!(screen.grid.visible_row(0)[1].c, '0');
    assert_eq!(screen.grid.visible_row(0)[2].c, '4');

    // --- Shrink 7→4: loses bottom 3 rows, no scrollback change ---
    screen.resize(10, 4);
    let scrollback_after_shrink = screen.get_history().len();
    assert_eq!(scrollback_after_shrink, 3, "shrink should not alter scrollback");
    // Screen shows L04..L07 (first 4 visible rows survive)
    let screen_lines = collect_screen_lines(&screen);
    assert!(screen_lines[0].contains("L04"), "row 0 after shrink: '{}'", screen_lines[0]);
    assert!(screen_lines[3].contains("L07"), "row 3 after shrink: '{}'", screen_lines[3]);

    // --- Expand 4→10: restores all 3 remaining scrollback lines ---
    screen.resize(10, 10);
    assert_eq!(screen.get_history().len(), 0, "all scrollback should be restored");
    // First 3 rows should be L01, L02, L03 (from scrollback)
    let screen_lines = collect_screen_lines(&screen);
    assert!(screen_lines[0].contains("L01"),
        "row 0 should be L01 from scrollback, got: '{}'", screen_lines[0]);
    assert!(screen_lines[1].contains("L02"),
        "row 1 should be L02 from scrollback, got: '{}'", screen_lines[1]);
    assert!(screen_lines[2].contains("L03"),
        "row 2 should be L03 from scrollback, got: '{}'", screen_lines[2]);
    // Followed by L04..L07 that were on screen
    assert!(screen_lines[3].contains("L04"),
        "row 3 should be L04, got: '{}'", screen_lines[3]);
}

#[test]
fn resize_expand_in_alt_screen_skips_scrollback_restore() {
    // Scrollback should not be consumed during alt screen resize,
    // and should be preserved for later use.
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"L1\r\nL2\r\nL3\r\nL4\r\nL5");
    let hist_before = screen.get_history().len();
    assert!(hist_before > 0, "should have scrollback before alt screen");

    // Enter alt screen
    screen.process(b"\x1b[?1049h");

    // Expand while in alt screen
    screen.resize(10, 8);

    // Scrollback should NOT be consumed
    assert_eq!(screen.get_history().len(), hist_before,
        "scrollback should be preserved during alt screen resize");

    // Exit alt screen
    screen.process(b"\x1b[?1049l");

    // Scrollback should still be intact
    assert_eq!(screen.get_history().len(), hist_before,
        "scrollback should be preserved after exiting alt screen");
}
