//! Tests for scrollback behavior across reconnect cycles.
//!
//! Simulates the scenario where a TUI app (like Claude Code) renders to the
//! normal screen (not alt screen) and the user reconnects through retach
//! multiple times, potentially with different terminal sizes.
//!
//! Key question: does scrollback accumulate duplicate content (e.g. logos)
//! across reconnect cycles?

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

/// Collect scrollback history as trimmed strings.
fn history_texts(screen: &Screen) -> Vec<String> {
    screen
        .get_history()
        .iter()
        .map(|b| strip_ansi(b))
        .collect()
}

/// Simulate a reconnect: drain pending scrollback, get full history,
/// render full screen.  Returns (history_lines, render_bytes).
fn simulate_reconnect(screen: &mut Screen) -> (Vec<Vec<u8>>, Vec<u8>) {
    // Drain stale pending scrollback (as send_initial_state does)
    let _ = screen.take_pending_scrollback();
    let _ = screen.take_passthrough();

    let history = screen.get_history();
    let mut cache = RenderCache::new();
    let render = screen.render(true, &mut cache);
    (history, render)
}

/// Simulate an app that redraws in-place using cursor positioning.
/// Writes each line at explicit row positions (like Claude Code's diff renderer).
fn app_redraw_inplace(screen: &mut Screen, lines: &[&str]) {
    for (i, line) in lines.iter().enumerate() {
        // CSI row;1H — move cursor to row i+1, col 1
        let cup = format!("\x1b[{};1H", i + 1);
        screen.process(cup.as_bytes());
        screen.process(line.as_bytes());
        // Erase to end of line
        screen.process(b"\x1b[K");
    }
}

/// Simulate an app that redraws by writing lines sequentially with \r\n.
/// This is the pattern that causes scrollback accumulation when content
/// overflows the screen height.
fn app_redraw_sequential(screen: &mut Screen, lines: &[&str]) {
    // Move to top-left first
    screen.process(b"\x1b[H");
    for (i, line) in lines.iter().enumerate() {
        screen.process(line.as_bytes());
        if i < lines.len() - 1 {
            screen.process(b"\r\n");
        }
    }
}

/// Count occurrences of a substring in the history.
fn count_in_history(screen: &Screen, needle: &str) -> usize {
    history_texts(screen)
        .iter()
        .filter(|line| line.contains(needle))
        .count()
}

// ─── get_history() idempotency ──────────────────────────────────────────────

#[test]
fn get_history_is_idempotent() {
    let mut screen = Screen::new(20, 3, 100);
    screen.process(b"line1\r\nline2\r\nline3\r\nline4\r\nline5");
    let _ = screen.take_pending_scrollback();

    let h1 = screen.get_history();
    let h2 = screen.get_history();
    assert_eq!(h1.len(), h2.len(), "get_history should return same length each time");
    for (a, b) in h1.iter().zip(h2.iter()) {
        assert_eq!(a, b, "get_history should return identical data each time");
    }
}

#[test]
fn get_history_unchanged_without_new_output() {
    let mut screen = Screen::new(20, 3, 100);
    screen.process(b"A\r\nB\r\nC\r\nD\r\nE");
    let _ = screen.take_pending_scrollback();

    let h1 = history_texts(&screen);

    // Simulate reconnect (drain + full render) without any new PTY output
    let _ = simulate_reconnect(&mut screen);

    let h2 = history_texts(&screen);
    assert_eq!(h1, h2, "history should be unchanged after reconnect without new output");
}

// ─── In-place redraw (cursor positioning) ───────────────────────────────────

#[test]
fn inplace_redraw_does_not_grow_scrollback() {
    let mut screen = Screen::new(20, 5, 100);

    // Initial app output: fill screen with content
    let content = ["LOGO: MyApp", "===========", "Status: OK", "Line 4", "Line 5"];
    app_redraw_inplace(&mut screen, &content);
    let _ = screen.take_pending_scrollback();
    let initial_history_len = screen.get_history().len();

    // Simulate reconnect
    let _ = simulate_reconnect(&mut screen);

    // App receives SIGWINCH and redraws in-place (same content)
    app_redraw_inplace(&mut screen, &content);

    let after_history_len = screen.get_history().len();
    assert_eq!(
        initial_history_len, after_history_len,
        "in-place redraw should not add to scrollback"
    );
}

#[test]
fn inplace_redraw_multiple_reconnects_no_growth() {
    let mut screen = Screen::new(20, 4, 100);

    let content = ["LOGO", "====", "Info", "Prompt>"];
    app_redraw_inplace(&mut screen, &content);
    let _ = screen.take_pending_scrollback();

    // Simulate 5 reconnect cycles, each triggering an in-place redraw
    for cycle in 0..5 {
        let _ = simulate_reconnect(&mut screen);
        app_redraw_inplace(&mut screen, &content);
        let logo_count = count_in_history(&screen, "LOGO");
        assert_eq!(
            logo_count, 0,
            "cycle {}: in-place redraw should never push LOGO into scrollback",
            cycle
        );
    }
}

// ─── Sequential redraw (\n-based) — the problematic pattern ─────────────────

#[test]
fn sequential_redraw_at_top_does_not_scroll_when_fits() {
    // If the app moves to \e[H and writes lines that fit within the screen,
    // no scroll_up should occur and scrollback should not grow.
    let mut screen = Screen::new(20, 5, 100);

    // Content that fits exactly in 5 rows
    let content = ["LOGO", "====", "Line1", "Line2", "Line3"];
    app_redraw_sequential(&mut screen, &content);
    let _ = screen.take_pending_scrollback();
    let h0 = screen.get_history().len();

    // Reconnect + same redraw
    let _ = simulate_reconnect(&mut screen);
    app_redraw_sequential(&mut screen, &content);

    let h1 = screen.get_history().len();
    assert_eq!(
        h0, h1,
        "sequential redraw fitting within screen should not grow scrollback"
    );
}

#[test]
fn sequential_redraw_overflows_screen_grows_scrollback() {
    // If the app writes MORE lines than screen height using \n,
    // lines scroll off the top into scrollback.
    let mut screen = Screen::new(20, 3, 100);

    // App renders 5 lines on a 3-row screen using \n — 2 will scroll off
    let content = ["LOGO", "====", "Line1", "Line2", "Line3"];
    app_redraw_sequential(&mut screen, &content);

    let pending = screen.take_pending_scrollback();
    assert_eq!(
        pending.len(), 2,
        "2 lines should scroll off when 5 lines written to 3-row screen"
    );
    let h = history_texts(&screen);
    assert!(h[0].contains("LOGO"), "LOGO should be first scrollback line");
}

// ─── The full duplication scenario ──────────────────────────────────────────

#[test]
fn logo_duplicates_on_reconnect_with_overflow_redraw() {
    // This test demonstrates the exact mechanism that causes duplicate logos
    // when using Claude Code through retach:
    //
    // 1. App writes LOGO + content to normal screen (not alt screen)
    // 2. Content scrolls, LOGO goes into scrollback
    // 3. User reconnects — retach sends scrollback (with LOGO) as History
    // 4. SIGWINCH → app redraws by writing LOGO + content again
    // 5. If redraw causes scroll, LOGO enters scrollback AGAIN
    // 6. Next reconnect: scrollback has TWO LOGOs

    let mut screen = Screen::new(20, 4, 100);

    // Step 1: App starts, writes logo + content that fills screen
    screen.process(b"LOGO\r\n====\r\nLine1\r\nLine2");
    let _ = screen.take_pending_scrollback();
    assert_eq!(count_in_history(&screen, "LOGO"), 0, "LOGO still on screen");

    // Step 2: More output scrolls LOGO off screen
    screen.process(b"\r\nLine3\r\nLine4\r\nLine5");
    let _ = screen.take_pending_scrollback();
    assert_eq!(count_in_history(&screen, "LOGO"), 1, "LOGO scrolled into history once");

    // Step 3: Reconnect
    let _ = simulate_reconnect(&mut screen);

    // Step 4: SIGWINCH → app redraws with overflow (writes from top, \n-based)
    // This simulates an app that re-renders its full UI including the logo,
    // and the content overflows the screen causing scroll_up.
    screen.process(b"\x1b[H"); // cursor home
    screen.process(b"LOGO\nLine6\nLine7\nLine8\nLine9");
    let _ = screen.take_pending_scrollback();

    // Step 5: Now LOGO appears TWICE in scrollback
    let logo_count = count_in_history(&screen, "LOGO");
    assert_eq!(
        logo_count, 2,
        "expected 2 LOGOs in scrollback (original + redraw overflow), got {}",
        logo_count
    );
}

#[test]
fn logo_accumulates_with_each_reconnect_cycle() {
    // Demonstrates logo accumulation across multiple reconnect/SIGWINCH cycles.
    // Each cycle the app redraws its full UI, overflow scrolls the logo again.

    let mut screen = Screen::new(20, 3, 100);

    // Initial: app writes logo + content (fills 3-row screen)
    screen.process(b"LOGO\r\nContent1\r\nPrompt>");
    let _ = screen.take_pending_scrollback();

    // More output pushes LOGO into scrollback
    screen.process(b"\r\nOutput1\r\nOutput2");
    let _ = screen.take_pending_scrollback();
    assert_eq!(count_in_history(&screen, "LOGO"), 1);

    // Simulate N reconnect cycles with overflow redraws
    for cycle in 1..=3 {
        let _ = simulate_reconnect(&mut screen);

        // App redraws: LOGO + enough content to overflow 3-row screen
        screen.process(b"\x1b[H");
        screen.process(b"LOGO\nRedrawn\nMore\nPrompt>");
        let _ = screen.take_pending_scrollback();

        let logo_count = count_in_history(&screen, "LOGO");
        assert_eq!(
            logo_count, 1 + cycle,
            "after {} reconnect cycles, expected {} LOGOs, got {}",
            cycle, 1 + cycle, logo_count
        );
    }
}

// ─── Resize effects on scrollback ───────────────────────────────────────────

#[test]
fn shrink_vertical_does_not_push_to_scrollback() {
    // Grid::resize() pops from bottom, NOT from top.
    // So shrinking does NOT move visible content into scrollback.
    let mut screen = Screen::new(20, 5, 100);

    screen.process(b"Row1\r\nRow2\r\nRow3\r\nRow4\r\nRow5");
    let _ = screen.take_pending_scrollback();
    let h_before = screen.get_history().len();

    // Shrink from 5 rows to 3 — bottom 2 rows are LOST, not scrolled
    screen.resize(20, 3);
    let h_after = screen.get_history().len();

    assert_eq!(
        h_before, h_after,
        "vertical shrink should not push rows into scrollback"
    );
}

#[test]
fn grow_vertical_restores_from_scrollback() {
    let mut screen = Screen::new(20, 3, 100);

    // Fill and scroll: 5 lines on 3-row screen → 2 in scrollback
    screen.process(b"LOGO\r\nLine2\r\nLine3\r\nLine4\r\nLine5");
    let _ = screen.take_pending_scrollback();
    assert_eq!(screen.get_history().len(), 2);

    // Grow from 3 to 5 rows → should restore 2 lines from scrollback
    screen.resize(20, 5);
    assert_eq!(
        screen.get_history().len(), 0,
        "growing should restore scrollback lines to grid"
    );
}

#[test]
fn shrink_then_grow_cycle_scrollback_consistency() {
    let mut screen = Screen::new(20, 5, 100);

    // Fill screen and create 3 scrollback lines
    screen.process(b"S1\r\nS2\r\nS3\r\nV1\r\nV2\r\nV3\r\nV4\r\nV5");
    let _ = screen.take_pending_scrollback();
    let h_initial = screen.get_history().len();
    assert_eq!(h_initial, 3, "should have 3 lines in scrollback");

    // Shrink to 3 rows: bottom 2 rows lost, scrollback unchanged
    screen.resize(20, 3);
    assert_eq!(screen.get_history().len(), 3, "shrink should not affect scrollback count");

    // Grow back to 5 rows: 2 lines restored from scrollback
    screen.resize(20, 5);
    assert_eq!(screen.get_history().len(), 1, "grow should restore 2 lines from scrollback");
}

// ─── Resize + SIGWINCH redraw interaction ───────────────────────────────────

#[test]
fn resize_smaller_then_app_redraw_inplace_no_duplication() {
    let mut screen = Screen::new(20, 5, 100);

    // App renders logo + content
    let content = ["LOGO", "====", "Line1", "Line2", "Prompt>"];
    app_redraw_inplace(&mut screen, &content);
    let _ = screen.take_pending_scrollback();
    assert_eq!(count_in_history(&screen, "LOGO"), 0);

    // Reconnect with smaller screen (3 rows instead of 5)
    screen.resize(20, 3);
    let _ = simulate_reconnect(&mut screen);

    // App redraws in-place for 3 rows — no overflow
    let short_content = ["LOGO", "====", "Prompt>"];
    app_redraw_inplace(&mut screen, &short_content);

    assert_eq!(
        count_in_history(&screen, "LOGO"), 0,
        "in-place redraw after resize should not push LOGO into scrollback"
    );
}

#[test]
fn resize_smaller_then_app_sequential_redraw_overflows() {
    let mut screen = Screen::new(20, 5, 100);

    // App renders 5 lines with cursor positioning (fits perfectly)
    let content = ["LOGO", "====", "Line1", "Line2", "Prompt>"];
    app_redraw_inplace(&mut screen, &content);
    let _ = screen.take_pending_scrollback();
    assert_eq!(count_in_history(&screen, "LOGO"), 0);

    // Reconnect with smaller screen
    screen.resize(20, 3);
    let _ = simulate_reconnect(&mut screen);

    // App redraws sequentially with 5 lines on 3-row screen — overflows!
    app_redraw_sequential(&mut screen, &content);
    let _ = screen.take_pending_scrollback();

    assert_eq!(
        count_in_history(&screen, "LOGO"), 1,
        "sequential redraw overflow after shrink should push LOGO into scrollback"
    );
}

// ─── Clear screen (\e[2J) does NOT generate scrollback ──────────────────────

#[test]
fn clear_screen_does_not_generate_scrollback() {
    let mut screen = Screen::new(20, 3, 100);

    screen.process(b"LOGO\r\n====\r\nPrompt>");
    let _ = screen.take_pending_scrollback();
    let h_before = screen.get_history().len();

    // Clear screen (ED 2) — should erase grid, NOT scroll into scrollback
    screen.process(b"\x1b[2J");
    let _ = screen.take_pending_scrollback();
    let h_after = screen.get_history().len();

    assert_eq!(
        h_before, h_after,
        "\\e[2J should not push content into scrollback"
    );
}

#[test]
fn clear_then_redraw_inplace_no_scrollback() {
    // Simulates an app that clears screen then redraws with cursor positioning
    let mut screen = Screen::new(20, 4, 100);

    // Initial content
    app_redraw_inplace(&mut screen, &["LOGO", "====", "Line1", "Line2"]);
    let _ = screen.take_pending_scrollback();

    // App clears and redraws (like response to SIGWINCH)
    screen.process(b"\x1b[2J");
    app_redraw_inplace(&mut screen, &["LOGO", "====", "NewLine1", "NewLine2"]);

    let h = screen.get_history().len();
    assert_eq!(h, 0, "clear + in-place redraw should not generate scrollback");
}

// ─── Full reconnect simulation ──────────────────────────────────────────────

#[test]
fn full_reconnect_cycle_same_size_inplace_no_duplication() {
    // End-to-end: connect → use → disconnect → reconnect (same size) → SIGWINCH redraw
    let mut screen = Screen::new(20, 5, 100);

    // Session 1: app starts, renders, user interacts
    app_redraw_inplace(&mut screen, &["LOGO", "====", "", "", ""]);
    screen.process(b"\x1b[3;1H"); // cursor to row 3
    screen.process(b"user> hello\r\n");
    screen.process(b"response: hi\r\n");
    screen.process(b"user> ");
    let _ = screen.take_pending_scrollback();

    // Disconnect + reconnect (same size)
    let (history, _render) = simulate_reconnect(&mut screen);
    assert_eq!(count_in_history(&screen, "LOGO"), 0, "LOGO should still be on screen");

    // SIGWINCH → app redraws in-place
    app_redraw_inplace(&mut screen, &["LOGO", "====", "user> hello", "response: hi", "user> "]);

    assert_eq!(
        count_in_history(&screen, "LOGO"), 0,
        "reconnect + in-place redraw should not create duplicate LOGO"
    );
    let _ = history; // suppress unused warning
}

#[test]
fn full_reconnect_cycle_different_size_with_overflow_causes_duplication() {
    // End-to-end: connect (5 rows) → scrollback → reconnect (3 rows) → overflow redraw
    let mut screen = Screen::new(20, 5, 100);

    // Fill screen, scroll LOGO into scrollback
    screen.process(b"LOGO\r\n====\r\nLine1\r\nLine2\r\nLine3");
    let _ = screen.take_pending_scrollback();
    screen.process(b"\r\nLine4\r\nLine5\r\nLine6");
    let _ = screen.take_pending_scrollback();
    assert_eq!(count_in_history(&screen, "LOGO"), 1, "1 LOGO in scrollback");

    // Reconnect with smaller terminal (3 rows)
    screen.resize(20, 3);
    let _ = simulate_reconnect(&mut screen);

    // App redraws: writes 5 lines on 3-row screen → 2 scroll off (including LOGO)
    screen.process(b"\x1b[H");
    screen.process(b"LOGO\n====\nLine7\nLine8\nLine9");
    let _ = screen.take_pending_scrollback();

    let logo_count = count_in_history(&screen, "LOGO");
    assert!(
        logo_count >= 2,
        "expected at least 2 LOGOs after overflow redraw on smaller screen, got {}",
        logo_count
    );
}

// ─── Scrollback limit prevents unbounded growth ─────────────────────────────

#[test]
fn scrollback_limit_caps_logo_accumulation() {
    // Even with repeated reconnects causing logo duplication,
    // the scrollback limit prevents unbounded growth.
    let limit = 10;
    let mut screen = Screen::new(20, 3, limit);

    // Fill and scroll to create initial scrollback
    for i in 1..=5 {
        screen.process(format!("line{}\r\n", i).as_bytes());
    }
    let _ = screen.take_pending_scrollback();

    // Simulate 20 reconnect cycles with overflow redraws
    for _ in 0..20 {
        let _ = simulate_reconnect(&mut screen);
        screen.process(b"\x1b[H");
        screen.process(b"LOGO\nStuff\nMore\nExtra\nEnd");
        let _ = screen.take_pending_scrollback();
    }

    let total = screen.get_history().len();
    assert!(
        total <= limit,
        "scrollback should be capped at limit {}, got {}",
        limit, total
    );
}

// ─── Alt screen apps don't cause duplication ────────────────────────────────

#[test]
fn alt_screen_app_no_scrollback_on_reconnect() {
    let mut screen = Screen::new(20, 5, 100);

    // App enters alt screen (like vim, htop)
    screen.process(b"\x1b[?1049h");
    screen.process(b"Alt content\r\nMore alt\r\nEven more\r\n");
    for _ in 0..20 {
        screen.process(b"scroll in alt\r\n");
    }
    let _ = screen.take_pending_scrollback();

    // Reconnect — alt screen history should be skipped (per send_initial_state)
    assert!(screen.in_alt_screen(), "should be in alt screen");
    let history = screen.get_history();
    assert!(
        history.is_empty(),
        "alt screen should have no scrollback history, got {} lines",
        history.len()
    );
}
