//! Tests for rendering correctness when output exceeds the visible grid area.
//!
//! Covers:
//! - Scrollback accumulation from rapid bulk output
//! - Incremental render after full-screen scrolls
//! - Dirty tracking across bulk content replacement
//! - Render with scrollback for large pending queues
//! - Scrollback limit enforcement under burst output
//! - Render stability after many scroll cycles

use super::*;
use render::RenderCache;

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Strip ANSI escape sequences, returning only printable text.
fn strip_ansi(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut in_esc = false;
    for ch in s.chars() {
        if in_esc {
            if ch.is_ascii_alphabetic() || ch == 'm' {
                in_esc = false;
            }
            continue;
        }
        if ch == '\x1b' {
            in_esc = true;
            continue;
        }
        if ch >= ' ' {
            out.push(ch);
        }
    }
    out.trim_end().to_string()
}

/// Collect visible grid rows as trimmed strings.
fn screen_lines(screen: &Screen) -> Vec<String> {
    screen
        .grid
        .cells
        .iter()
        .map(|row| {
            let s: String = row.iter().map(|c| c.c).collect();
            s.trim_end().to_string()
        })
        .collect()
}

/// Collect scrollback history as trimmed strings.
fn history_texts(screen: &Screen) -> Vec<String> {
    screen
        .get_history()
        .iter()
        .map(|b| strip_ansi(b))
        .collect()
}

/// Write N labeled lines ("L001\r\n", ..., "LNNN") to the screen.
fn write_many_lines(screen: &mut Screen, count: usize) {
    for i in 1..=count {
        if i < count {
            screen.process(format!("L{:03}\r\n", i).as_bytes());
        } else {
            screen.process(format!("L{:03}", i).as_bytes());
        }
    }
}

// ─── Scrollback accumulation from large output ──────────────────────────────

#[test]
fn bulk_output_scrollback_count() {
    // 100 lines on a 5-row screen → 95 in scrollback
    let mut screen = Screen::new(20, 5, 1000);
    write_many_lines(&mut screen, 100);
    let _ = screen.take_pending_scrollback();

    let hist = history_texts(&screen);
    assert_eq!(hist.len(), 95, "expected 95 scrollback lines, got {}", hist.len());

    // Visible grid should have the last 5 lines
    let visible = screen_lines(&screen);
    assert!(visible[0].contains("L096"), "row 0 should be L096, got: '{}'", visible[0]);
    assert!(visible[4].contains("L100"), "row 4 should be L100, got: '{}'", visible[4]);
}

#[test]
fn bulk_output_scrollback_ordering() {
    let mut screen = Screen::new(20, 3, 5000);
    write_many_lines(&mut screen, 500);
    let _ = screen.take_pending_scrollback();

    let hist = history_texts(&screen);
    assert_eq!(hist.len(), 497);

    // Every line should be in order
    for (i, line) in hist.iter().enumerate() {
        let expected = format!("L{:03}", i + 1);
        assert!(
            line.contains(&expected),
            "history line {} should contain '{}', got: '{}'",
            i, expected, line
        );
    }
}

#[test]
fn bulk_output_pending_scrollback_matches_total() {
    let mut screen = Screen::new(20, 4, 1000);
    write_many_lines(&mut screen, 50);

    // pending_scrollback should have all lines that scrolled off
    let pending = screen.take_pending_scrollback();
    assert_eq!(pending.len(), 46, "50 lines - 4 visible = 46 pending");

    // After drain, pending is empty but get_history still works
    let pending2 = screen.take_pending_scrollback();
    assert!(pending2.is_empty());
    assert_eq!(screen.get_history().len(), 46);
}

// ─── Scrollback limit enforcement ───────────────────────────────────────────

#[test]
fn scrollback_limit_caps_history() {
    let limit = 20;
    let mut screen = Screen::new(20, 5, limit);
    write_many_lines(&mut screen, 100);
    let _ = screen.take_pending_scrollback();

    let hist = history_texts(&screen);
    assert_eq!(
        hist.len(), limit,
        "scrollback should be capped at limit {}, got {}",
        limit, hist.len()
    );

    // The oldest lines should be evicted; history starts from L076
    assert!(
        hist[0].contains("L076"),
        "first history line should be L076 (oldest kept), got: '{}'",
        hist[0]
    );
    assert!(
        hist[limit - 1].contains("L095"),
        "last history line should be L095, got: '{}'",
        hist[limit - 1]
    );
}

#[test]
fn scrollback_limit_pending_also_capped() {
    let limit = 10;
    let mut screen = Screen::new(20, 3, limit);
    write_many_lines(&mut screen, 50);

    let pending = screen.take_pending_scrollback();
    assert_eq!(
        pending.len(), limit,
        "pending scrollback should be capped at limit {}, got {}",
        limit, pending.len()
    );
}

#[test]
fn scrollback_limit_zero_no_history() {
    let mut screen = Screen::new(20, 5, 0);
    write_many_lines(&mut screen, 50);

    let hist = screen.get_history();
    assert!(hist.is_empty(), "zero scrollback limit should produce no history");

    let pending = screen.take_pending_scrollback();
    assert!(pending.is_empty(), "zero scrollback limit should produce no pending");
}

// ─── Render correctness after bulk scrolling ────────────────────────────────

#[test]
fn full_render_after_bulk_scroll_shows_last_rows() {
    let mut screen = Screen::new(20, 5, 100);
    write_many_lines(&mut screen, 50);

    let mut cache = RenderCache::new();
    let output = screen.render(true, &mut cache);
    let text = String::from_utf8_lossy(&output);

    // Full render should contain the last 5 lines
    assert!(text.contains("L046"), "render should contain L046");
    assert!(text.contains("L050"), "render should contain L050");

    // Should NOT contain scrolled-off lines
    assert!(!text.contains("L001"), "render should not contain scrolled-off L001");
    assert!(!text.contains("L045"), "render should not contain scrolled-off L045");
}

#[test]
fn incremental_render_after_bulk_scroll() {
    let mut screen = Screen::new(20, 5, 100);
    let mut cache = RenderCache::new();

    // Initial content
    screen.process(b"AAAA\r\nBBBB\r\nCCCC\r\nDDDD\r\nEEEE");
    let _ = screen.render(false, &mut cache);

    // Bulk scroll: 50 new lines push everything off
    write_many_lines(&mut screen, 50);

    let output = screen.render(false, &mut cache);
    let text = String::from_utf8_lossy(&output);

    // All rows changed, so all should be redrawn even in incremental mode
    assert!(text.contains("\x1b[1;1H"), "row 1 should be redrawn");
    assert!(text.contains("\x1b[2;1H"), "row 2 should be redrawn");
    assert!(text.contains("\x1b[3;1H"), "row 3 should be redrawn");
    assert!(text.contains("\x1b[4;1H"), "row 4 should be redrawn");
    assert!(text.contains("\x1b[5;1H"), "row 5 should be redrawn");
}

#[test]
fn incremental_render_no_redraw_when_content_unchanged_after_scroll() {
    let mut screen = Screen::new(10, 3, 100);
    let mut cache = RenderCache::new();

    // Write 6 lines → screen shows L04, L05, L06
    write_many_lines(&mut screen, 6);
    let _ = screen.render(false, &mut cache);

    // No changes → incremental render should skip all rows
    let output = screen.render(false, &mut cache);
    let text = String::from_utf8_lossy(&output);

    // Row positioning for content should not appear (only cursor)
    assert!(!text.contains("\x1b[1;1H"), "no row redraws when unchanged");
    assert!(!text.contains("\x1b[2;1H"), "no row redraws when unchanged");
}

#[test]
fn render_with_large_pending_scrollback() {
    let mut screen = Screen::new(20, 5, 200);
    write_many_lines(&mut screen, 100);

    let pending = screen.take_pending_scrollback();
    assert_eq!(pending.len(), 95);

    let mut cache = RenderCache::new();
    let output = screen.render_with_scrollback(&pending, &mut cache);
    let text = String::from_utf8_lossy(&output);

    // Should have sync block
    assert!(text.starts_with("\x1b[?2026h"));
    assert!(text.ends_with("\x1b[?2026l"));

    // Scrollback lines should appear before screen clear
    let pos_l001 = text.find("L001").expect("L001 should be in scrollback");
    let pos_clear = text.find("\x1b[2J").expect("screen clear should be present");
    assert!(pos_l001 < pos_clear, "scrollback should precede screen clear");

    // Screen content after clear
    let after_clear = &text[pos_clear..];
    assert!(after_clear.contains("L096"), "visible L096 should be after clear");
    assert!(after_clear.contains("L100"), "visible L100 should be after clear");

    // No scrollback lines in the screen portion
    assert!(!after_clear.contains("L001"), "L001 should not be in screen portion");
    assert!(!after_clear.contains("L050"), "L050 should not be in screen portion");
}

// ─── Multiple render cycles with bulk updates ───────────────────────────────

#[test]
fn multiple_bulk_updates_dirty_tracking() {
    let mut screen = Screen::new(20, 4, 100);
    let mut cache = RenderCache::new();

    // First bulk: 20 lines
    write_many_lines(&mut screen, 20);
    let r1 = screen.render(false, &mut cache);
    let t1 = String::from_utf8_lossy(&r1);
    // All rows drawn on first render
    assert!(t1.contains("\x1b[1;1H"));
    assert!(t1.contains("\x1b[4;1H"));

    // Second bulk: 20 more lines (complete content replacement)
    for i in 21..=40 {
        screen.process(format!("M{:03}\r\n", i).as_bytes());
    }
    screen.process(b"M041");

    let r2 = screen.render(false, &mut cache);
    let t2 = String::from_utf8_lossy(&r2);
    // All rows changed, all should be redrawn
    assert!(t2.contains("\x1b[1;1H"), "all rows should redraw after second bulk");
    assert!(t2.contains("M038") || t2.contains("M039") || t2.contains("M040") || t2.contains("M041"),
        "new content should appear in render");

    // Third render without changes → no row redraws
    let r3 = screen.render(false, &mut cache);
    let t3 = String::from_utf8_lossy(&r3);
    assert!(!t3.contains("\x1b[1;1H"), "no redraws on third render without changes");
}

#[test]
fn alternating_bulk_and_single_line_updates() {
    let mut screen = Screen::new(20, 3, 100);
    let mut cache = RenderCache::new();

    // Bulk: 10 lines
    write_many_lines(&mut screen, 10);
    let _ = screen.render(false, &mut cache);

    // Single line addition (scrolls by 1)
    screen.process(b"\r\nSINGLE");
    let r = screen.render(false, &mut cache);
    let t = String::from_utf8_lossy(&r);

    // All rows shifted, all should be redrawn
    assert!(t.contains("\x1b[1;1H"), "row 1 should redraw after scroll");
    assert!(t.contains("SINGLE"), "new content should be visible");
}

// ─── Cursor position after large scrolls ────────────────────────────────────

#[test]
fn cursor_position_after_bulk_output() {
    let mut screen = Screen::new(20, 5, 100);
    write_many_lines(&mut screen, 100);

    // Cursor should be at column 4 (after "L100"), row 4 (last row, 0-indexed)
    assert_eq!(screen.grid.cursor_y, 4, "cursor_y should be at bottom row");
    assert_eq!(screen.grid.cursor_x, 4, "cursor_x should be after 'L100'");

    let mut cache = RenderCache::new();
    let output = screen.render(true, &mut cache);
    let text = String::from_utf8_lossy(&output);
    // 1-indexed: row 5, col 5
    assert!(text.contains("\x1b[5;5H"), "cursor should be at row 5, col 5 (1-indexed)");
}

#[test]
fn cursor_stays_on_bottom_row_during_continuous_scroll() {
    let mut screen = Screen::new(20, 3, 100);
    // Fill the screen first so cursor reaches the bottom
    screen.process(b"a\r\nb\r\nc");
    assert_eq!(screen.grid.cursor_y, 2);

    // Now every \r\n should scroll, keeping cursor at bottom row
    for i in 1..=50 {
        screen.process(format!("\r\nline{}", i).as_bytes());
        assert_eq!(
            screen.grid.cursor_y, 2,
            "cursor_y should stay at bottom row (2) after scroll, iteration {}",
            i
        );
    }
}

// ─── Reattach after large output ────────────────────────────────────────────

#[test]
fn reattach_after_1000_lines() {
    let mut screen = Screen::new(20, 5, 500);
    write_many_lines(&mut screen, 1000);
    let _ = screen.take_pending_scrollback();

    let hist = screen.get_history();
    // Capped at scrollback_limit
    assert_eq!(hist.len(), 500, "history should be capped at 500");

    // Reattach render
    let mut cache = RenderCache::new();
    let output = screen.render_with_scrollback(&hist, &mut cache);
    let text = String::from_utf8_lossy(&output);

    // Should have screen clear
    assert!(text.contains("\x1b[2J"));

    // Screen portion should have last 5 lines
    let clear_pos = text.find("\x1b[2J").unwrap();
    let after_clear = &text[clear_pos..];
    assert!(after_clear.contains("L996"), "screen should show L996");
    assert!(after_clear.contains("L1000"), "screen should show L1000");
}

#[test]
fn reattach_render_no_standalone_bell_after_bulk() {
    let mut screen = Screen::new(20, 5, 100);
    // Set a title via OSC so BEL bytes will actually appear in the render
    screen.process(b"\x1b]2;Bulk Test Title\x07");
    write_many_lines(&mut screen, 200);
    let _ = screen.take_pending_scrollback();

    let hist = screen.get_history();
    let mut cache = RenderCache::new();
    let output = screen.render_with_scrollback(&hist, &mut cache);

    // BEL bytes should exist (from the title) but only inside OSC sequences
    let bell_count = output.iter().filter(|&&b| b == 0x07).count();
    assert!(bell_count > 0, "title should produce at least one BEL byte in render");

    for (i, &byte) in output.iter().enumerate() {
        if byte == 0x07 {
            let prefix = &output[..i];
            let osc_start = prefix.windows(2).rposition(|w| w == b"\x1b]");
            assert!(osc_start.is_some(),
                "BEL at offset {} is standalone after bulk output reattach", i);
        }
    }
}

// ─── Edge cases ─────────────────────────────────────────────────────────────

#[test]
fn output_exactly_fills_screen_no_scroll() {
    let mut screen = Screen::new(20, 5, 100);
    // Write exactly 5 lines (fills screen, no scroll)
    write_many_lines(&mut screen, 5);

    let hist = screen.get_history();
    assert!(hist.is_empty(), "no scrollback when output exactly fills screen");

    let visible = screen_lines(&screen);
    assert!(visible[0].contains("L001"), "row 0 should be L001");
    assert!(visible[4].contains("L005"), "row 4 should be L005");
}

#[test]
fn output_one_more_than_screen_scrolls_once() {
    let mut screen = Screen::new(20, 5, 100);
    write_many_lines(&mut screen, 6);

    let hist = history_texts(&screen);
    assert_eq!(hist.len(), 1, "one line should scroll off");
    assert!(hist[0].contains("L001"), "scrolled line should be L001");

    let visible = screen_lines(&screen);
    assert!(visible[0].contains("L002"), "row 0 should be L002");
    assert!(visible[4].contains("L006"), "row 4 should be L006");
}

#[test]
fn rapid_output_then_partial_overwrite() {
    let mut screen = Screen::new(20, 3, 100);
    let mut cache = RenderCache::new();

    // Bulk output: 30 lines
    write_many_lines(&mut screen, 30);
    let _ = screen.render(false, &mut cache);

    // Now overwrite just the current line (cursor is at end of L030)
    screen.process(b"\rOVERWRITTEN");
    let output = screen.render(false, &mut cache);
    let text = String::from_utf8_lossy(&output);

    // Only the bottom row should be redrawn
    assert!(text.contains("\x1b[3;1H"), "bottom row should be redrawn");
    assert!(text.contains("OVERWRITTEN"), "overwritten content should appear");
    // Other rows unchanged
    assert!(!text.contains("\x1b[1;1H"), "row 1 should not be redrawn");
    assert!(!text.contains("\x1b[2;1H"), "row 2 should not be redrawn");
}

#[test]
fn bulk_output_with_scroll_region() {
    // Scroll region set to rows 2-4 of a 5-row screen
    let mut screen = Screen::new(20, 5, 100);
    screen.process(b"\x1b[2;4r"); // DECSTBM: rows 2-4 (1-indexed)
    screen.process(b"\x1b[2;1H"); // Move to row 2

    // Write 10 lines inside the scroll region
    for i in 1..=10 {
        if i < 10 {
            screen.process(format!("R{:02}\r\n", i).as_bytes());
        } else {
            screen.process(format!("R{:02}", i).as_bytes());
        }
    }

    // Row 1 (outside scroll region, top) should be untouched
    assert_eq!(screen.grid.cells[0][0].c, ' ', "row 0 should be blank (outside scroll region)");
    // Row 5 (outside scroll region, bottom) should be untouched
    assert_eq!(screen.grid.cells[4][0].c, ' ', "row 4 should be blank (outside scroll region)");

    // Scroll region rows should have the last 3 of the 10 lines:
    // 10 lines written in region of 3 rows → 7 scrolled off, R08/R09/R10 remain
    let visible = screen_lines(&screen);
    assert!(visible[1].contains("R08"),
        "scroll region row 1 should be R08, got: '{}'", visible[1]);
    assert!(visible[2].contains("R09"),
        "scroll region row 2 should be R09, got: '{}'", visible[2]);
    assert!(visible[3].contains("R10"),
        "scroll region row 3 should be R10, got: '{}'", visible[3]);

    // Scrollback should NOT capture lines scrolled within non-top scroll region
    let hist = screen.get_history();
    assert!(hist.is_empty(),
        "scroll region not starting at top should not generate scrollback, got {} lines",
        hist.len());
}

#[test]
fn bulk_output_with_styles_renders_correctly() {
    let mut screen = Screen::new(30, 3, 100);
    let mut cache = RenderCache::new();

    // Write styled lines that scroll off
    for i in 1..=10 {
        let color = 31 + (i % 7); // cycle through colors
        screen.process(format!("\x1b[{}mLine{:02}\x1b[0m\r\n", color, i).as_bytes());
    }
    screen.process(b"\x1b[1;33mLastLine\x1b[0m");

    let output = screen.render(true, &mut cache);
    let text = String::from_utf8_lossy(&output);

    // Should contain the styled content
    assert!(text.contains("LastLine"), "last line should be visible");

    // History should preserve styles
    let hist = screen.get_history();
    assert!(!hist.is_empty(), "styled lines should be in scrollback");
    let first_hist = String::from_utf8_lossy(&hist[0]);
    assert!(first_hist.contains("\x1b["), "scrollback should preserve SGR codes");
}

#[test]
fn cache_invalidate_mid_bulk_produces_correct_render() {
    let mut screen = Screen::new(20, 4, 100);
    let mut cache = RenderCache::new();

    // First render
    write_many_lines(&mut screen, 20);
    let _ = screen.render(false, &mut cache);

    // Invalidate cache manually (simulates reconnect scenario)
    cache.invalidate();

    // Render should redraw all rows
    let output = screen.render(false, &mut cache);
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("\x1b[1;1H"), "row 1 redrawn after invalidate");
    assert!(text.contains("\x1b[2;1H"), "row 2 redrawn after invalidate");
    assert!(text.contains("\x1b[3;1H"), "row 3 redrawn after invalidate");
    assert!(text.contains("\x1b[4;1H"), "row 4 redrawn after invalidate");
}

#[test]
fn sync_block_wraps_large_render() {
    let mut screen = Screen::new(20, 5, 100);
    write_many_lines(&mut screen, 200);

    let mut cache = RenderCache::new();
    let output = screen.render(true, &mut cache);
    let text = String::from_utf8_lossy(&output);

    assert!(text.starts_with("\x1b[?2026h"), "should start with sync begin");
    assert!(text.ends_with("\x1b[?2026l"), "should end with sync end");
}
