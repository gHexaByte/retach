//! Tests for the boundary between the current screen and scrollback history.
//!
//! Covers:
//! - Screen-level invariants: no overlap between history and visible grid
//! - Pending scrollback drain semantics (reattach simulation)
//! - Resize interactions with scrollback (expand/shrink/roundtrip)
//! - Resize between reattach cycles
//! - End-to-end: Screen -> history + render -> client stdout contract

use super::*;
use super::test_helpers::*;
use render::RenderCache;

/// Collect all content (scrollback + visible) as ordered text lines.
fn all_content(screen: &Screen) -> Vec<String> {
    let mut lines = history_texts(screen);
    lines.extend(
        screen_lines(screen)
            .into_iter()
            .filter(|s| !s.is_empty()),
    );
    lines
}

/// Write N labeled lines ("L01\r\n", "L02\r\n", ..., "LNN") to the screen.
/// The last line has no trailing \r\n.
fn write_labeled_lines(screen: &mut Screen, count: usize) {
    for i in 1..=count {
        if i < count {
            screen.process(format!("L{:02}\r\n", i).as_bytes());
        } else {
            screen.process(format!("L{:02}", i).as_bytes());
        }
    }
}

/// Simulate the reattach flow: get history + build ScreenUpdate with flush newlines,
/// exactly as `send_initial_state` does. Returns (history_lines, screen_update_bytes).
fn simulate_reattach(screen: &Screen) -> (Vec<Vec<u8>>, Vec<u8>) {
    let hist = screen.get_history();
    let mut render_data = Vec::new();
    if !hist.is_empty() {
        // Position cursor at bottom row
        render_data.extend_from_slice(b"\x1b[");
        style::write_u16(&mut render_data, screen.grid.rows());
        render_data.extend_from_slice(b";1H");
        // Flush newlines
        render_data.extend(
            std::iter::repeat_n(b'\n', screen.grid.rows().saturating_sub(1) as usize),
        );
    }
    let mut cache = RenderCache::new();
    render_data.extend_from_slice(&screen.render(true, &mut cache));
    (hist, render_data)
}

/// Simulate what the client writes to stdout for a History message.
fn client_write_history(history: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for line in history {
        out.extend_from_slice(line);
        out.extend_from_slice(b"\r\n");
    }
    out
}

// ─── Section 1: Screen-level unit tests ─────────────────────────────────────

#[test]
fn history_and_screen_no_overlap() {
    let mut screen = Screen::new(10, 3, 100);
    // Write 8 lines on a 3-row screen → 5 in scrollback, 3 on screen
    write_labeled_lines(&mut screen, 8);
    let _ = screen.take_pending_scrollback();

    let hist = history_texts(&screen);
    let visible = screen_lines(&screen);

    // History should have exactly 5 lines
    assert_eq!(hist.len(), 5, "expected 5 history lines, got {:?}", hist);
    // Screen should show the last 3
    assert!(
        visible[0].contains("L06"),
        "screen row 0 should be L06, got: '{}'",
        visible[0]
    );
    assert!(
        visible[2].contains("L08"),
        "screen row 2 should be L08, got: '{}'",
        visible[2]
    );

    // No line should appear in both
    for h in &hist {
        for v in &visible {
            if !v.is_empty() {
                assert_ne!(
                    h, v,
                    "line '{}' appears in both history and screen",
                    h
                );
            }
        }
    }
}

#[test]
fn pending_scrollback_drained_before_reattach() {
    let mut screen = Screen::new(10, 3, 100);
    write_labeled_lines(&mut screen, 6);

    // First drain: should have pending lines
    let first = screen.take_pending_scrollback();
    assert!(
        !first.is_empty(),
        "first take should have pending scrollback"
    );

    // Second drain: should be empty (simulates send_initial_state drain)
    let second = screen.take_pending_scrollback();
    assert!(
        second.is_empty(),
        "second take should be empty after drain"
    );

    // But get_history() still returns all scrollback
    let hist = screen.get_history();
    assert!(
        !hist.is_empty(),
        "get_history should still return scrollback after pending drain"
    );
}

#[test]
fn history_ordering_preserved_with_many_lines() {
    let mut screen = Screen::new(20, 3, 5000);
    // Write 200 lines
    for i in 1..=200 {
        if i < 200 {
            screen.process(format!("LINE{:04}\r\n", i).as_bytes());
        } else {
            screen.process(format!("LINE{:04}", i).as_bytes());
        }
    }
    let _ = screen.take_pending_scrollback();

    let hist = history_texts(&screen);
    // 200 lines - 3 visible = 197 in scrollback
    assert_eq!(hist.len(), 197);

    // Verify ordering: each line should have a higher number than the previous
    for (i, line) in hist.iter().enumerate() {
        let expected = format!("LINE{:04}", i + 1);
        assert!(
            line.contains(&expected),
            "history line {} should contain '{}', got: '{}'",
            i,
            expected,
            line
        );
    }
}

#[test]
fn resize_expand_moves_scrollback_to_screen_no_duplication() {
    let mut screen = Screen::new(10, 3, 100);
    write_labeled_lines(&mut screen, 10);
    let _ = screen.take_pending_scrollback();

    // State: scrollback=[L01..L07], screen=[L08, L09, L10]
    assert_eq!(screen.get_history().len(), 7);

    // Expand to 6 rows → should restore 3 lines from scrollback
    screen.resize(10, 6);

    let all = all_content(&screen);
    // Total should still be 10 unique lines, no duplicates
    assert_eq!(all.len(), 10, "total lines after expand: {:?}", all);
    for (i, line) in all.iter().enumerate() {
        let expected = format!("L{:02}", i + 1);
        assert!(
            line.contains(&expected),
            "line {} should be '{}', got: '{}'",
            i,
            expected,
            line
        );
    }

    // Scrollback should have shrunk
    assert_eq!(
        screen.get_history().len(),
        4,
        "scrollback should have 4 lines after restoring 3"
    );
}

#[test]
fn resize_shrink_then_expand_roundtrip_no_duplication() {
    let mut screen = Screen::new(10, 5, 100);
    write_labeled_lines(&mut screen, 8);
    let _ = screen.take_pending_scrollback();

    // State: scrollback=[L01..L03], screen=[L04..L08]
    let content_before = all_content(&screen);
    assert_eq!(content_before.len(), 8);

    // Shrink to 3 rows (drops bottom 2 screen rows L07, L08)
    screen.resize(10, 3);

    // Expand back to 5 rows (restores from scrollback)
    screen.resize(10, 5);

    let content_after = all_content(&screen);
    // L07, L08 were lost in shrink — they're gone from screen and not in scrollback.
    // But L01..L06 should be preserved without duplication.
    let unique: std::collections::HashSet<&String> = content_after.iter().collect();
    assert_eq!(
        unique.len(),
        content_after.len(),
        "no duplicates allowed after shrink/expand: {:?}",
        content_after
    );
}

#[test]
fn stale_pending_scrollback_only_new_lines() {
    let mut screen = Screen::new(10, 3, 100);

    // First batch: scroll 2 lines off
    screen.process(b"A1\r\nA2\r\nA3\r\nA4\r\nA5");
    let batch1 = screen.take_pending_scrollback();
    let batch1_texts: Vec<String> = batch1.iter().map(|b| strip_ansi(b)).collect();
    assert_eq!(batch1_texts.len(), 2, "first batch: {:?}", batch1_texts);
    assert!(batch1_texts[0].contains("A1"));
    assert!(batch1_texts[1].contains("A2"));

    // Second batch: scroll 1 more line off
    screen.process(b"\r\nA6");
    let batch2 = screen.take_pending_scrollback();
    let batch2_texts: Vec<String> = batch2.iter().map(|b| strip_ansi(b)).collect();
    assert_eq!(batch2_texts.len(), 1, "second batch: {:?}", batch2_texts);
    assert!(
        batch2_texts[0].contains("A3"),
        "second batch should only have A3, got: '{}'",
        batch2_texts[0]
    );
}

#[test]
fn history_render_flush_newlines_match_rows() {
    for rows in [3u16, 5, 10, 24] {
        let mut screen = Screen::new(80, rows, 100);
        // Generate enough lines to have scrollback
        for i in 1..=(rows as usize + 5) {
            if i < (rows as usize + 5) {
                screen.process(format!("line{}\r\n", i).as_bytes());
            } else {
                screen.process(format!("line{}", i).as_bytes());
            }
        }
        let _ = screen.take_pending_scrollback();

        let (hist, screen_update) = simulate_reattach(&screen);
        assert!(!hist.is_empty(), "should have history for rows={}", rows);

        // The screen update should start with cursor positioning to bottom row,
        // then rows-1 newlines, then the render
        let expected_prefix = format!("\x1b[{};1H", rows);
        let prefix_bytes = expected_prefix.as_bytes();
        assert!(
            screen_update.starts_with(prefix_bytes),
            "rows={}: update should start with cursor-to-bottom '{}', got: {:?}",
            rows,
            expected_prefix,
            String::from_utf8_lossy(&screen_update[..20.min(screen_update.len())])
        );

        // Count newlines between cursor positioning and sync begin
        let after_cursor = &screen_update[prefix_bytes.len()..];
        let newline_count = after_cursor.iter().take_while(|&&b| b == b'\n').count();
        assert_eq!(
            newline_count,
            (rows - 1) as usize,
            "rows={}: expected {} flush newlines, got {}",
            rows,
            rows - 1,
            newline_count
        );
    }
}

#[test]
fn resize_between_reattach_preserves_content() {
    let mut screen = Screen::new(10, 5, 100);
    write_labeled_lines(&mut screen, 12);
    let _ = screen.take_pending_scrollback();

    // State: scrollback=[L01..L07], screen=[L08..L12]
    let content_before = all_content(&screen);
    assert_eq!(content_before.len(), 12);

    // Simulate detach: take snapshot
    let (hist_before, _) = simulate_reattach(&screen);
    assert_eq!(hist_before.len(), 7);

    // Resize while "detached" (server-side resize, e.g., from another client or API)
    screen.resize(10, 3);

    // Simulate reattach with new size
    let _ = screen.take_pending_scrollback(); // drain stale
    let (hist_after, screen_update_after) = simulate_reattach(&screen);

    // Verify: scrollback + screen still form a coherent sequence (no duplication)
    let content_after = all_content(&screen);
    let unique: std::collections::HashSet<&String> = content_after.iter().collect();
    assert_eq!(
        unique.len(),
        content_after.len(),
        "no duplicates after resize between reattach: {:?}",
        content_after
    );

    // History should still have content
    assert!(
        !hist_after.is_empty(),
        "history should not be empty after resize"
    );

    // Screen update should have flush newlines (since history is non-empty)
    let update_text = String::from_utf8_lossy(&screen_update_after);
    assert!(
        update_text.contains("\x1b[3;1H"),
        "flush should position cursor at new bottom row (3)"
    );
}

#[test]
fn resize_expand_between_reattach_restores_scrollback() {
    let mut screen = Screen::new(10, 3, 100);
    write_labeled_lines(&mut screen, 8);
    let _ = screen.take_pending_scrollback();

    // State: scrollback=[L01..L05], screen=[L06, L07, L08]
    assert_eq!(screen.get_history().len(), 5);

    // Simulate detach + resize to larger terminal
    screen.resize(10, 8);

    // 5 lines should be restored from scrollback
    assert_eq!(
        screen.get_history().len(),
        0,
        "all scrollback should be restored after expanding to 8 rows"
    );

    // All 8 lines should be on screen
    let visible = screen_lines(&screen);
    for i in 1..=8 {
        let expected = format!("L{:02}", i);
        assert!(
            visible.iter().any(|v| v.contains(&expected)),
            "L{:02} should be visible after expand, screen: {:?}",
            i,
            visible
        );
    }

    // Reattach: no history → no flush newlines
    let (hist, screen_update) = simulate_reattach(&screen);
    assert!(hist.is_empty(), "no history after full restore");
    // Should start with sync begin directly (no cursor-to-bottom + newlines)
    assert!(
        screen_update.starts_with(b"\x1b[?2026h"),
        "no-history reattach should start with sync begin"
    );
}

// ─── Section 2: End-to-end reattach simulation ──────────────────────────────

#[test]
fn e2e_reattach_history_then_screen() {
    let mut screen = Screen::new(10, 3, 100);
    write_labeled_lines(&mut screen, 8);
    let _ = screen.take_pending_scrollback();

    // Simulate server side: build History + ScreenUpdate
    let (hist, screen_update) = simulate_reattach(&screen);
    assert_eq!(hist.len(), 5, "should have 5 history lines");

    // Simulate client side: write history to stdout, then screen update
    let mut stdout = Vec::new();
    // Client processes History message
    for line in &hist {
        stdout.extend_from_slice(line);
        stdout.extend_from_slice(b"\r\n");
    }
    // Client processes ScreenUpdate message
    stdout.extend_from_slice(&screen_update);

    let stdout_text = String::from_utf8_lossy(&stdout);

    // History lines should appear before screen clear
    let pos_l01 = stdout_text.find("L01").expect("L01 should be in output");
    let pos_l05 = stdout_text.find("L05").expect("L05 should be in output");
    let pos_clear = stdout_text.find("\x1b[2J").expect("screen clear should be in output");

    assert!(
        pos_l01 < pos_l05,
        "history lines should be in order"
    );
    assert!(
        pos_l05 < pos_clear,
        "history should appear before screen clear"
    );

    // Screen content should appear after screen clear
    let after_clear = &stdout_text[pos_clear..];
    assert!(
        after_clear.contains("L06"),
        "screen should contain L06 after clear"
    );
    assert!(
        after_clear.contains("L08"),
        "screen should contain L08 after clear"
    );

    // History lines should NOT appear after the clear (no duplication)
    // L01-L05 should only be in the history portion
    for label in &["L01", "L02", "L03", "L04", "L05"] {
        assert!(
            !after_clear.contains(label),
            "'{}' should not appear in screen portion (after clear)",
            label
        );
    }
}

#[test]
fn e2e_reattach_no_history_no_flush() {
    let mut screen = Screen::new(10, 3, 100);
    // Only 2 lines — no scrollback
    screen.process(b"Hello\r\nWorld");

    let (hist, screen_update) = simulate_reattach(&screen);
    assert!(hist.is_empty(), "should have no history");

    // ScreenUpdate should not have cursor-to-bottom or flush newlines
    // It should start directly with sync begin
    assert!(
        screen_update.starts_with(b"\x1b[?2026h"),
        "no-history reattach should start with sync begin, got: {:?}",
        String::from_utf8_lossy(&screen_update[..20.min(screen_update.len())])
    );
}

#[test]
fn e2e_reattach_with_styled_history() {
    let mut screen = Screen::new(20, 3, 100);
    // Write styled content that will scroll into history
    screen.process(b"\x1b[1;31mRED BOLD\x1b[0m normal\r\n");
    screen.process(b"\x1b[32mGREEN\x1b[0m\r\n");
    screen.process(b"plain1\r\n");
    screen.process(b"plain2\r\n");
    screen.process(b"visible");
    let _ = screen.take_pending_scrollback();

    let (hist, _) = simulate_reattach(&screen);
    assert_eq!(hist.len(), 2, "2 lines should be in history");

    // Verify styled line is preserved in history
    let line0 = &hist[0];
    let line0_text = String::from_utf8_lossy(line0);
    assert!(
        line0_text.contains("RED BOLD"),
        "history should preserve text content"
    );
    // SGR codes should be present
    assert!(
        line0_text.contains("\x1b["),
        "history should preserve SGR escape codes"
    );
}

#[test]
fn e2e_resize_between_reattach_cycles() {
    // Simulate: create session → produce output → detach → resize → reattach
    let mut screen = Screen::new(10, 5, 100);
    write_labeled_lines(&mut screen, 10);
    let _ = screen.take_pending_scrollback();

    // === First reattach (at 5 rows) ===
    let (hist1, update1) = simulate_reattach(&screen);
    assert_eq!(hist1.len(), 5, "first reattach: 5 history lines");

    // Verify first reattach output is correct
    let mut stdout1 = client_write_history(&hist1);
    stdout1.extend_from_slice(&update1);
    let text1 = String::from_utf8_lossy(&stdout1);
    assert!(text1.contains("L01"), "first reattach should have L01 in history");

    // === Simulate detach, then resize to 3 rows ===
    screen.resize(10, 3);
    let _ = screen.take_pending_scrollback(); // drain stale

    // === Second reattach (at 3 rows) ===
    let (hist2, update2) = simulate_reattach(&screen);

    // Build client output
    let mut stdout2 = client_write_history(&hist2);
    stdout2.extend_from_slice(&update2);
    let text2 = String::from_utf8_lossy(&stdout2);

    // Scrollback should have grown (screen shrank, but only bottom rows were lost)
    assert!(
        hist2.len() >= hist1.len(),
        "shrink should not reduce scrollback, before={}, after={}",
        hist1.len(),
        hist2.len()
    );

    // Verify no duplication between history and screen content after screen clear
    let clear_pos2 = text2.find("\x1b[2J").expect("screen clear in second reattach");
    let history_portion = &text2[..clear_pos2];
    let screen_portion = &text2[clear_pos2..];

    // Find which labels are in history vs screen — they should not overlap
    for i in 1..=10 {
        let label = format!("L{:02}", i);
        let in_hist = history_portion.contains(&label);
        let in_screen = screen_portion.contains(&label);
        assert!(
            !(in_hist && in_screen),
            "'{}' appears in both history and screen portions",
            label
        );
    }

    // === Simulate detach, then resize back to 8 rows ===
    screen.resize(10, 8);
    let _ = screen.take_pending_scrollback();

    // === Third reattach (at 8 rows) ===
    let (hist3, update3) = simulate_reattach(&screen);

    let mut stdout3 = client_write_history(&hist3);
    stdout3.extend_from_slice(&update3);
    let _text3 = String::from_utf8_lossy(&stdout3);

    // Scrollback should have shrunk (lines restored to screen)
    assert!(
        hist3.len() < hist2.len(),
        "expand should reduce scrollback, before={}, after={}",
        hist2.len(),
        hist3.len()
    );

    // Flush newlines should match new row count if history is present
    if !hist3.is_empty() {
        let cursor_prefix = format!("\x1b[{};1H", 8);
        assert!(
            String::from_utf8_lossy(&update3).contains(&cursor_prefix),
            "flush should use new row count (8)"
        );
    }

    // Content should still be coherent
    let all = all_content(&screen);
    let unique: std::collections::HashSet<&String> = all.iter().collect();
    assert_eq!(
        unique.len(),
        all.len(),
        "no duplicates after multiple resize+reattach cycles: {:?}",
        all
    );
}

#[test]
fn e2e_scrollback_during_session_then_reattach() {
    // Simulate active scrollback (render_with_scrollback) then reattach
    let mut screen = Screen::new(10, 3, 100);
    write_labeled_lines(&mut screen, 5);

    // Take pending scrollback as the pty_to_client loop would
    let pending = screen.take_pending_scrollback();
    assert_eq!(pending.len(), 2, "2 lines scrolled off");

    // Render with scrollback (atomic update as server does)
    let mut cache = RenderCache::new();
    let atomic_update = screen.render_with_scrollback(&pending, &mut cache);
    let atomic_text = String::from_utf8_lossy(&atomic_update);

    // Scrollback lines should appear before screen clear
    assert!(atomic_text.contains("L01"), "scrollback should contain L01");
    let pos_clear = atomic_text
        .find("\x1b[2J")
        .expect("atomic update should have screen clear");
    let l01_pos = atomic_text
        .find("L01")
        .expect("L01 should be in output");
    assert!(
        l01_pos < pos_clear,
        "scrollback content should precede screen clear"
    );

    // Now simulate reattach
    let (hist, screen_update) = simulate_reattach(&screen);

    // The scrollback lines should be in history
    let hist_texts: Vec<String> = hist.iter().map(|b| strip_ansi(b)).collect();
    assert!(
        hist_texts.iter().any(|t| t.contains("L01")),
        "L01 should be in reattach history: {:?}",
        hist_texts
    );

    // Full client output should be coherent
    let mut stdout = client_write_history(&hist);
    stdout.extend_from_slice(&screen_update);
    let text = String::from_utf8_lossy(&stdout);
    assert!(text.contains("L01"), "reattach should include L01");
    assert!(text.contains("L05"), "reattach should include L05");
}

#[test]
fn e2e_reattach_wide_terminal_to_narrow() {
    let mut screen = Screen::new(40, 5, 100);
    // Write long lines that will be truncated on resize
    for i in 1..=8 {
        let line = format!("LINE{:02}--padding-to-fill-wide-terminal---", i);
        if i < 8 {
            screen.process(format!("{}\r\n", line).as_bytes());
        } else {
            screen.process(line.as_bytes());
        }
    }
    let _ = screen.take_pending_scrollback();

    // First reattach at 40 cols
    let (hist_wide, _) = simulate_reattach(&screen);
    let hist_wide_texts: Vec<String> = hist_wide.iter().map(|b| strip_ansi(b)).collect();

    // Resize to narrow terminal
    screen.resize(10, 5);
    let _ = screen.take_pending_scrollback();

    // Second reattach at 10 cols — scrollback should still have old-width content
    let (hist_narrow, screen_update) = simulate_reattach(&screen);
    let hist_narrow_texts: Vec<String> = hist_narrow.iter().map(|b| strip_ansi(b)).collect();

    // Scrollback lines from before resize keep their original width
    for line in &hist_narrow_texts[..hist_wide_texts.len().min(hist_narrow_texts.len())] {
        assert!(
            line.contains("LINE"),
            "old scrollback line should still have content: '{}'",
            line
        );
    }

    // Screen update should be valid
    let update_text = String::from_utf8_lossy(&screen_update);
    assert!(
        update_text.contains("\x1b[?2026h"),
        "screen update should have sync begin"
    );
    assert!(
        update_text.contains("\x1b[?2026l"),
        "screen update should have sync end"
    );
}
