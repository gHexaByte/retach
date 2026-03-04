//! Tests for live scrollback delivery while client is connected.
//!
//! Simulates the scenario where output arrives continuously and the client's
//! terminal may be scrolled up viewing history. Covers:
//! - Pending scrollback draining + render_with_scrollback correctness
//! - Cache invalidation after scrollback injection
//! - Multiple sequential scrollback batches (no duplication/loss)
//! - Mixed scrollback and screen-only update cycles
//! - Mode and scroll region restoration after scrollback injection
//! - Rapid output accumulation in pending scrollback
//! - Interleaved scrollback / no-scrollback render cycles

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

/// Collect pending scrollback as trimmed strings.
fn pending_texts(pending: &[Vec<u8>]) -> Vec<String> {
    pending.iter().map(|b| strip_ansi(b)).collect()
}

/// Simulate a throttled render cycle: drain pending scrollback and render.
/// Returns the rendered bytes.
fn do_render_cycle(screen: &mut Screen, cache: &mut RenderCache) -> Vec<u8> {
    let pending = screen.take_pending_scrollback();
    if !pending.is_empty() {
        screen.render_with_scrollback(&pending, cache)
    } else {
        screen.render(false, cache)
    }
}

// ─── Cache invalidation after scrollback injection ──────────────────────────

#[test]
fn incremental_render_after_scrollback_injection_redraws_all_rows() {
    // After render_with_scrollback invalidates the cache, the first
    // incremental render should emit all rows (cache sentinel mismatch).
    let mut screen = Screen::new(20, 4, 100);
    let mut cache = RenderCache::new();

    // Initial content: fill exactly 4 rows
    screen.process(b"AAAA\r\nBBBB\r\nCCCC\r\nDDDD");
    let _ = screen.render(false, &mut cache);

    // More output scrolls lines into pending scrollback
    screen.process(b"\r\nEEEE\r\nFFFF\r\nGGGG");
    let pending = screen.take_pending_scrollback();
    assert_eq!(pending.len(), 3, "3 lines should have scrolled off");

    // render_with_scrollback invalidates the cache
    let output = screen.render_with_scrollback(&pending, &mut cache);
    let text = String::from_utf8_lossy(&output);
    // Should contain scrollback before clear and screen after clear
    assert!(text.contains("\x1b[2J"), "should have screen clear");

    // Now: incremental render with NO changes should skip all rows
    let incr = screen.render(false, &mut cache);
    let incr_text = String::from_utf8_lossy(&incr);
    assert!(
        !incr_text.contains("\x1b[1;1H"),
        "no row redraws when nothing changed after scrollback injection"
    );
    assert!(
        !incr_text.contains("\x1b[2;1H"),
        "no row redraws when nothing changed after scrollback injection"
    );
}

#[test]
fn two_incremental_renders_after_scrollback_only_first_redraws() {
    let mut screen = Screen::new(20, 3, 100);
    let mut cache = RenderCache::new();

    // Fill and scroll
    screen.process(b"A\r\nB\r\nC\r\nD\r\nE\r\nF");
    let pending = screen.take_pending_scrollback();
    let _ = screen.render_with_scrollback(&pending, &mut cache);

    // Modify one row
    screen.process(b"\rMODIFIED");

    // First incremental: should redraw the modified row
    let r1 = screen.render(false, &mut cache);
    let t1 = String::from_utf8_lossy(&r1);
    assert!(t1.contains("MODIFIED"), "modified row should be in first incremental");
    assert!(t1.contains("\x1b[3;1H"), "bottom row should be redrawn");

    // Second incremental: nothing changed, no redraws
    let r2 = screen.render(false, &mut cache);
    let t2 = String::from_utf8_lossy(&r2);
    assert!(
        !t2.contains("\x1b[1;1H") && !t2.contains("\x1b[2;1H") && !t2.contains("\x1b[3;1H"),
        "no row redraws on second unchanged incremental"
    );
}

// ─── Multiple scrollback batches ────────────────────────────────────────────

#[test]
fn sequential_scrollback_batches_no_duplication() {
    let mut screen = Screen::new(20, 3, 100);
    let mut cache = RenderCache::new();

    // Initial: fill screen
    screen.process(b"R01\r\nR02\r\nR03");
    let _ = do_render_cycle(&mut screen, &mut cache);

    // Batch 1: 3 more lines → 3 scroll off
    screen.process(b"\r\nR04\r\nR05\r\nR06");
    let pending1 = screen.take_pending_scrollback();
    let texts1 = pending_texts(&pending1);
    assert_eq!(texts1.len(), 3, "batch 1: 3 lines scrolled off");
    assert!(texts1[0].contains("R01"));
    assert!(texts1[2].contains("R03"));

    let out1 = screen.render_with_scrollback(&pending1, &mut cache);
    let t1 = String::from_utf8_lossy(&out1);
    // Scrollback should have R01-R03, screen should have R04-R06
    let clear1 = t1.find("\x1b[2J").unwrap();
    assert!(t1[..clear1].contains("R01"), "batch1: R01 in scrollback");
    assert!(t1[clear1..].contains("R04"), "batch1: R04 on screen");

    // Batch 2: 2 more lines → 2 scroll off
    screen.process(b"\r\nR07\r\nR08");
    let pending2 = screen.take_pending_scrollback();
    let texts2 = pending_texts(&pending2);
    assert_eq!(texts2.len(), 2, "batch 2: 2 lines scrolled off");
    assert!(texts2[0].contains("R04"), "batch2 pending should start from R04");
    assert!(texts2[1].contains("R05"), "batch2 pending should have R05");

    let out2 = screen.render_with_scrollback(&pending2, &mut cache);
    let t2 = String::from_utf8_lossy(&out2);
    let clear2 = t2.find("\x1b[2J").unwrap();
    // Only new pending in scrollback portion
    assert!(t2[..clear2].contains("R04"), "batch2: R04 in scrollback");
    assert!(t2[..clear2].contains("R05"), "batch2: R05 in scrollback");
    assert!(
        !t2[..clear2].contains("R01"),
        "batch2: R01 should NOT be in this scrollback (already sent)"
    );
    // Screen should show R06-R08
    assert!(t2[clear2..].contains("R06"), "batch2: R06 on screen");
    assert!(t2[clear2..].contains("R08"), "batch2: R08 on screen");

    // Total history should have all 5 scrolled lines
    let hist = history_texts(&screen);
    assert_eq!(hist.len(), 5, "total history should be 5 lines");
    for i in 1..=5 {
        assert!(
            hist[i - 1].contains(&format!("R{:02}", i)),
            "history[{}] should be R{:02}, got: '{}'",
            i - 1,
            i,
            hist[i - 1]
        );
    }
}

#[test]
fn many_sequential_batches_accumulate_correctly() {
    let mut screen = Screen::new(20, 3, 1000);
    let mut cache = RenderCache::new();
    let mut total_pending_sent = 0;

    // Fill screen initially
    screen.process(b"init1\r\ninit2\r\ninit3");
    let _ = do_render_cycle(&mut screen, &mut cache);

    // 10 batches, each producing 5 new lines (scrolling 5 off)
    for batch in 0..10 {
        for j in 0..5 {
            let n = 4 + batch * 5 + j; // numbering continues from init
            screen.process(format!("\r\nB{:03}", n).as_bytes());
        }

        let pending = screen.take_pending_scrollback();
        assert!(
            !pending.is_empty(),
            "batch {} should have pending scrollback",
            batch
        );
        total_pending_sent += pending.len();

        let _ = screen.render_with_scrollback(&pending, &mut cache);
    }

    // Total scrollback should match what we sent
    let hist = history_texts(&screen);
    assert_eq!(
        hist.len(), total_pending_sent,
        "total history should match total pending sent across all batches"
    );
}

// ─── Mixed scrollback and screen-only changes ───────────────────────────────

#[test]
fn screen_only_change_after_scrollback_renders_incrementally() {
    let mut screen = Screen::new(20, 4, 100);
    let mut cache = RenderCache::new();

    // Fill and scroll
    screen.process(b"A\r\nB\r\nC\r\nD\r\nE\r\nF\r\nG\r\nH");
    let _ = do_render_cycle(&mut screen, &mut cache);

    // Screen-only change: overwrite part of current line (no scroll)
    screen.process(b"\rCHANGED");
    let pending = screen.take_pending_scrollback();
    assert!(pending.is_empty(), "cursor overwrite should not generate scrollback");

    // Incremental render: only changed row
    let output = screen.render(false, &mut cache);
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("CHANGED"), "changed content should appear");
    // Should be an incremental render (no screen clear)
    assert!(!text.contains("\x1b[2J"), "screen-only change should not clear screen");
}

#[test]
fn alternating_scrollback_and_no_scrollback_cycles() {
    let mut screen = Screen::new(20, 3, 100);
    let mut cache = RenderCache::new();

    // Fill screen
    screen.process(b"line1\r\nline2\r\nline3");
    let _ = do_render_cycle(&mut screen, &mut cache);

    // Cycle 1: scrollback
    screen.process(b"\r\nnew1\r\nnew2");
    let out1 = do_render_cycle(&mut screen, &mut cache);
    let t1 = String::from_utf8_lossy(&out1);
    assert!(t1.contains("\x1b[2J"), "scrollback cycle should clear screen");

    // Cycle 2: screen-only (cursor move + overwrite)
    screen.process(b"\x1b[1;1H");  // move to top
    screen.process(b"OVER");
    let out2 = do_render_cycle(&mut screen, &mut cache);
    let t2 = String::from_utf8_lossy(&out2);
    assert!(!t2.contains("\x1b[2J"), "screen-only cycle should not clear");
    assert!(t2.contains("OVER"), "overwrite should appear");

    // Cycle 3: scrollback again
    screen.process(b"\x1b[3;1H");  // cursor to bottom
    screen.process(b"\r\nnew3\r\nnew4\r\nnew5");
    let out3 = do_render_cycle(&mut screen, &mut cache);
    let t3 = String::from_utf8_lossy(&out3);
    assert!(t3.contains("\x1b[2J"), "scrollback cycle should clear screen again");

    // Cycle 4: no changes at all
    let out4 = do_render_cycle(&mut screen, &mut cache);
    let t4 = String::from_utf8_lossy(&out4);
    assert!(!t4.contains("\x1b[2J"), "no-change cycle should not clear");
    assert!(
        !t4.contains("\x1b[1;1H") && !t4.contains("\x1b[2;1H") && !t4.contains("\x1b[3;1H"),
        "no-change cycle should not redraw rows"
    );
}

// ─── Mode delta correctness across scrollback/incremental transitions ───────
//
// render_with_scrollback forces full=true, so modes are always emitted.
// The real test: does the cache correctly track modes so that SUBSEQUENT
// incremental renders produce correct deltas?

#[test]
fn cursor_shape_delta_correct_after_scrollback_then_change() {
    let mut screen = Screen::new(20, 3, 100);
    let mut cache = RenderCache::new();

    // Set cursor shape to bar, fill and scroll
    screen.process(b"\x1b[5 q"); // blinking bar
    screen.process(b"A\r\nB\r\nC\r\nD\r\nE");
    let _ = do_render_cycle(&mut screen, &mut cache);
    // Cache now has cursor_shape = blinking bar

    // Change cursor shape to block
    screen.process(b"\x1b[2 q"); // steady block

    // Incremental render should emit ONLY the new shape (delta from bar → block)
    let incr = screen.render(false, &mut cache);
    let incr_text = String::from_utf8_lossy(&incr);
    assert!(
        incr_text.contains("\x1b[2 q"),
        "incremental should emit new cursor shape (block)"
    );
    assert!(
        !incr_text.contains("\x1b[5 q"),
        "incremental should NOT re-emit old cursor shape (bar)"
    );

    // Next incremental with no change: should NOT emit cursor shape at all
    let incr2 = screen.render(false, &mut cache);
    let incr2_text = String::from_utf8_lossy(&incr2);
    assert!(
        !incr2_text.contains(" q"),
        "no cursor shape emission when nothing changed"
    );
}

#[test]
fn bracketed_paste_delta_after_scrollback_cycle() {
    let mut screen = Screen::new(20, 3, 100);
    let mut cache = RenderCache::new();

    // Enable bracketed paste, scroll
    screen.process(b"\x1b[?2004h");
    screen.process(b"A\r\nB\r\nC\r\nD\r\nE");
    let _ = do_render_cycle(&mut screen, &mut cache);
    // Cache now has bracketed_paste = true

    // Incremental with no mode change: should NOT re-emit ?2004h
    let incr1 = screen.render(false, &mut cache);
    let incr1_text = String::from_utf8_lossy(&incr1);
    assert!(
        !incr1_text.contains("?2004"),
        "no mode change → should not re-emit bracketed paste"
    );

    // Disable bracketed paste
    screen.process(b"\x1b[?2004l");
    let incr2 = screen.render(false, &mut cache);
    let incr2_text = String::from_utf8_lossy(&incr2);
    assert!(
        incr2_text.contains("\x1b[?2004l"),
        "disabling bracketed paste should emit ?2004l in delta"
    );
}

#[test]
fn autowrap_delta_after_scrollback_cycle() {
    let mut screen = Screen::new(20, 3, 100);
    let mut cache = RenderCache::new();

    // Disable autowrap, scroll
    screen.process(b"\x1b[?7l");
    screen.process(b"A\r\nB\r\nC\r\nD\r\nE");
    let _ = do_render_cycle(&mut screen, &mut cache);
    // Cache has autowrap = false

    // Re-enable autowrap
    screen.process(b"\x1b[?7h");
    let incr = screen.render(false, &mut cache);
    let incr_text = String::from_utf8_lossy(&incr);
    assert!(
        incr_text.contains("\x1b[?7h"),
        "re-enabling autowrap should appear in incremental delta after scrollback"
    );

    // No further change
    let incr2 = screen.render(false, &mut cache);
    let incr2_text = String::from_utf8_lossy(&incr2);
    assert!(
        !incr2_text.contains("?7"),
        "no autowrap change → should not re-emit"
    );
}

// ─── Scroll region cache correctness across scrollback/incremental ──────────

#[test]
fn scroll_region_cached_after_scrollback_not_re_emitted() {
    let mut screen = Screen::new(20, 5, 100);
    let mut cache = RenderCache::new();

    // Set custom scroll region, fill and scroll
    screen.process(b"\x1b[2;4r");
    screen.process(b"\x1b[H");
    for i in 1..=10 {
        screen.process(format!("L{:02}\r\n", i).as_bytes());
    }

    // Scrollback render: emits scroll region, caches it
    let _ = do_render_cycle(&mut screen, &mut cache);

    // Incremental render: scroll region unchanged → should NOT re-emit
    let incr = screen.render(false, &mut cache);
    let incr_text = String::from_utf8_lossy(&incr);
    assert!(
        !incr_text.contains("\x1b[2;4r"),
        "unchanged scroll region should NOT be re-emitted on incremental render"
    );

    // Change scroll region
    screen.process(b"\x1b[1;3r");
    let incr2 = screen.render(false, &mut cache);
    let incr2_text = String::from_utf8_lossy(&incr2);
    assert!(
        incr2_text.contains("\x1b[1;3r"),
        "changed scroll region should be emitted on incremental render"
    );
}

// ─── Cursor position after scrollback injection ─────────────────────────────

#[test]
fn cursor_position_correct_after_scrollback_injection() {
    let mut screen = Screen::new(20, 4, 100);
    let mut cache = RenderCache::new();

    // Fill screen and then scroll
    screen.process(b"row1\r\nrow2\r\nrow3\r\nrow4");
    let _ = do_render_cycle(&mut screen, &mut cache);

    // More output → scrollback
    screen.process(b"\r\nrow5\r\nrow6");
    // Cursor should be at row 3 (0-indexed), col 4 (after "row6")
    assert_eq!(screen.grid.cursor_y, 3);
    assert_eq!(screen.grid.cursor_x, 4);

    let pending = screen.take_pending_scrollback();
    let output = screen.render_with_scrollback(&pending, &mut cache);
    let text = String::from_utf8_lossy(&output);

    // CUP: ESC[4;5H (1-indexed)
    assert!(
        text.contains("\x1b[4;5H"),
        "cursor should be at row 4, col 5 (1-indexed) after scrollback injection, got: {}",
        text.chars()
            .collect::<String>()
            .replace('\x1b', "ESC")
    );
}

// ─── Rapid output accumulation ──────────────────────────────────────────────

#[test]
fn large_pending_scrollback_renders_all_lines() {
    let mut screen = Screen::new(20, 3, 5000);
    let mut cache = RenderCache::new();

    // Fill screen initially
    screen.process(b"init1\r\ninit2\r\ninit3");
    let _ = do_render_cycle(&mut screen, &mut cache);

    // Rapid burst: 500 lines arrive at once (simulates `cat large_file`)
    for i in 1..=500 {
        screen.process(format!("\r\nR{:04}", i).as_bytes());
    }

    let pending = screen.take_pending_scrollback();
    assert_eq!(
        pending.len(), 500,
        "all 500 scrolled lines should be in pending"
    );

    let output = screen.render_with_scrollback(&pending, &mut cache);
    let text = String::from_utf8_lossy(&output);

    // All scrollback lines should be present before the clear
    let clear_pos = text.find("\x1b[2J").expect("should have screen clear");
    let scrollback_portion = &text[..clear_pos];
    assert!(scrollback_portion.contains("R0001"), "first line should be in scrollback");
    assert!(scrollback_portion.contains("R0250"), "middle line should be in scrollback");
    assert!(scrollback_portion.contains("R0497"), "last scrolled-off line should be in scrollback");

    // Screen should show the last 3 lines (R0498, R0499, R0500)
    let screen_portion = &text[clear_pos..];
    assert!(screen_portion.contains("R0498"), "R0498 on screen");
    assert!(screen_portion.contains("R0500"), "R0500 on screen");
}

#[test]
fn pending_scrollback_limit_enforced_during_rapid_output() {
    let limit = 50;
    let mut screen = Screen::new(20, 3, limit);
    let mut cache = RenderCache::new();

    screen.process(b"A\r\nB\r\nC");
    let _ = do_render_cycle(&mut screen, &mut cache);

    // 200 lines → pending capped at limit
    for i in 1..=200 {
        screen.process(format!("\r\nL{:04}", i).as_bytes());
    }

    let pending = screen.take_pending_scrollback();
    assert_eq!(
        pending.len(), limit,
        "pending should be capped at scrollback limit"
    );

    // The pending should contain the MOST RECENT lines that scrolled off
    let texts = pending_texts(&pending);
    // Screen has 3 rows. After 200 \r\n lines, screen shows L0198/L0199/L0200.
    // 200 lines scrolled off total: A, B, C, L0001...L0197.
    // Pending is capped at 50, keeping most recent → L0148...L0197.
    assert!(
        texts.first().unwrap().contains("L0148"),
        "first pending should be L0148 (oldest kept), got: '{}'",
        texts.first().unwrap()
    );
    assert!(
        texts.last().unwrap().contains("L0197"),
        "last pending should be L0197 (last scrolled-off line), got: '{}'",
        texts.last().unwrap()
    );
}

// ─── Synchronized output wrapping ───────────────────────────────────────────

#[test]
fn scrollback_injection_outside_synchronized_output() {
    let mut screen = Screen::new(20, 3, 100);
    let mut cache = RenderCache::new();

    screen.process(b"A\r\nB\r\nC\r\nD\r\nE");
    let pending = screen.take_pending_scrollback();
    let output = screen.render_with_scrollback(&pending, &mut cache);
    let text = String::from_utf8_lossy(&output);

    // Scrollback injection must be BEFORE the sync block so that terminals
    // (like Blink/hterm) process scroll operations immediately.
    let sync_begin = text.find("\x1b[?2026h").expect("sync begin missing");
    let pos_a = text.find("A").unwrap();
    assert!(
        pos_a < sync_begin,
        "scrollback content should appear before sync block"
    );
    assert!(
        text.ends_with("\x1b[?2026l"),
        "output should end with synchronized output end"
    );
}

// ─── Scrollback injection overwrites rows then scrolls via \n ───────────────

#[test]
fn scrollback_injection_starts_at_bottom_row() {
    let mut screen = Screen::new(20, 5, 100);
    let mut cache = RenderCache::new();

    // Use distinctive labels to avoid matching stray bytes
    screen.process(b"SCRLL1\r\nSCRLL2\r\nCC\r\nDD\r\nEE\r\nFF\r\nGG");
    let pending = screen.take_pending_scrollback();
    assert!(!pending.is_empty());

    let output = screen.render_with_scrollback(&pending, &mut cache);
    let text = String::from_utf8_lossy(&output);

    // Scrollback content should be present and before the sync block
    let pos_first_scrollback = text.find("SCRLL1").expect("SCRLL1 missing");
    let sync_begin = text.find("\x1b[?2026h").expect("sync begin missing");
    assert!(
        pos_first_scrollback < sync_begin,
        "scrollback content should precede sync block"
    );

    // The bottom-row positioning (for \n scroll) should appear after the content
    // and before the sync block
    assert!(
        text.contains("\x1b[5;1H"),
        "should position cursor at bottom row for scrolling"
    );
}

// ─── Content ordering: scrollback before clear, screen after clear ──────────

#[test]
fn scrollback_content_before_clear_screen_content_after() {
    let mut screen = Screen::new(20, 3, 100);
    let mut cache = RenderCache::new();

    // Write 8 lines on 3-row screen
    for i in 1..=8 {
        if i < 8 {
            screen.process(format!("LINE{:02}\r\n", i).as_bytes());
        } else {
            screen.process(format!("LINE{:02}", i).as_bytes());
        }
    }

    let pending = screen.take_pending_scrollback();
    let texts = pending_texts(&pending);
    assert_eq!(texts.len(), 5, "5 lines should be pending");

    let output = screen.render_with_scrollback(&pending, &mut cache);
    let text = String::from_utf8_lossy(&output);
    let clear_pos = text.find("\x1b[2J").unwrap();

    let before_clear = &text[..clear_pos];
    let after_clear = &text[clear_pos..];

    // All 5 scrollback lines should be before the clear
    for i in 1..=5 {
        let label = format!("LINE{:02}", i);
        assert!(
            before_clear.contains(&label),
            "{} should be in scrollback (before clear)",
            label
        );
        assert!(
            !after_clear.contains(&label),
            "{} should NOT be in screen portion (after clear)",
            label
        );
    }

    // Last 3 lines should be on screen (after clear)
    for i in 6..=8 {
        let label = format!("LINE{:02}", i);
        assert!(
            after_clear.contains(&label),
            "{} should be on screen (after clear)",
            label
        );
    }
}

// ─── Simulate the full relay cycle ──────────────────────────────────────────

#[test]
fn full_relay_cycle_scrollback_then_incremental_then_scrollback() {
    // Simulates the screen_to_client relay loop:
    // cycle 1: output arrives, pending scrollback → render_with_scrollback
    // cycle 2: cursor movement only → incremental render
    // cycle 3: more output, scrollback again → render_with_scrollback
    // Verifies each cycle produces correct output.

    let mut screen = Screen::new(20, 4, 100);
    let mut cache = RenderCache::new();

    // Initial fill
    screen.process(b"A001\r\nA002\r\nA003\r\nA004");
    let _ = do_render_cycle(&mut screen, &mut cache);

    // --- Cycle 1: scrollback ---
    screen.process(b"\r\nB005\r\nB006\r\nB007");
    let out1 = do_render_cycle(&mut screen, &mut cache);
    let t1 = String::from_utf8_lossy(&out1);
    assert!(t1.contains("\x1b[2J"), "cycle 1 should clear (scrollback)");
    let c1 = t1.find("\x1b[2J").unwrap();
    assert!(t1[..c1].contains("A001"), "cycle 1: A001 in scrollback");
    assert!(t1[c1..].contains("B007"), "cycle 1: B007 on screen");

    // --- Cycle 2: cursor move only ---
    screen.process(b"\x1b[1;1H");  // cursor to top-left
    let out2 = do_render_cycle(&mut screen, &mut cache);
    let t2 = String::from_utf8_lossy(&out2);
    assert!(!t2.contains("\x1b[2J"), "cycle 2 should not clear (no scrollback)");
    // Cursor should be repositioned
    assert!(t2.contains("\x1b[1;1H"), "cycle 2: cursor at top-left");

    // --- Cycle 3: more scrollback ---
    screen.process(b"\x1b[4;1H");  // cursor back to bottom
    screen.process(b"\r\nC008\r\nC009");
    let out3 = do_render_cycle(&mut screen, &mut cache);
    let t3 = String::from_utf8_lossy(&out3);
    assert!(t3.contains("\x1b[2J"), "cycle 3 should clear (scrollback)");
    let c3 = t3.find("\x1b[2J").unwrap();
    // Only newly scrolled lines in this batch's scrollback
    assert!(
        !t3[..c3].contains("A001"),
        "cycle 3: A001 already sent, should not be in this batch"
    );
    assert!(t3[c3..].contains("C009"), "cycle 3: C009 on screen");
}

// ─── Edge case: single-line scrollback ──────────────────────────────────────

#[test]
fn single_line_scrollback_injection() {
    let mut screen = Screen::new(20, 3, 100);
    let mut cache = RenderCache::new();

    screen.process(b"line1\r\nline2\r\nline3");
    let _ = do_render_cycle(&mut screen, &mut cache);

    // One more line → one line scrolls off
    screen.process(b"\r\nline4");
    let pending = screen.take_pending_scrollback();
    assert_eq!(pending.len(), 1, "exactly one line should scroll off");

    let output = screen.render_with_scrollback(&pending, &mut cache);
    let text = String::from_utf8_lossy(&output);

    assert!(text.contains("\x1b[2J"), "should have screen clear");
    let clear_pos = text.find("\x1b[2J").unwrap();
    assert!(
        text[..clear_pos].contains("line1"),
        "single scrollback line should be present"
    );
    assert!(text[clear_pos..].contains("line4"), "line4 on screen");
}

// ─── Edge case: scrollback with styled content ──────────────────────────────

#[test]
fn styled_scrollback_lines_preserve_formatting() {
    let mut screen = Screen::new(30, 3, 100);
    let mut cache = RenderCache::new();

    // Write styled content that will scroll off
    screen.process(b"\x1b[1;31mBOLD_RED\x1b[0m\r\n");
    screen.process(b"\x1b[4;32mUNDERLINE_GREEN\x1b[0m\r\n");
    screen.process(b"plain\r\n");
    screen.process(b"visible1\r\n");
    screen.process(b"visible2");

    let pending = screen.take_pending_scrollback();
    assert_eq!(pending.len(), 2, "2 styled lines should scroll off");

    // Pending lines should have specific SGR codes
    let line0 = String::from_utf8_lossy(&pending[0]);
    assert!(line0.contains("BOLD_RED"), "content should be preserved");
    // render_line uses to_sgr_with_reset which emits "0;attr;color" format
    // Bold (1) and red fg (31) should both be present
    assert!(
        line0.contains(";1;") && line0.contains(";31m"),
        "bold+red SGR codes should be preserved in scrollback, got: '{}'",
        line0
    );

    let line1 = String::from_utf8_lossy(&pending[1]);
    assert!(line1.contains("UNDERLINE_GREEN"), "content should be preserved");
    // Underline (4) and green fg (32) should both be present
    assert!(
        line1.contains(";4;") && line1.contains(";32m"),
        "underline+green SGR codes should be preserved in scrollback, got: '{}'",
        line1
    );

    // Render with scrollback should include the styled lines
    let output = screen.render_with_scrollback(&pending, &mut cache);
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("BOLD_RED"), "styled content in render output");
    assert!(text.contains("UNDERLINE_GREEN"), "styled content in render output");
}

// ─── Title cache correctness across scrollback/incremental ──────────────────

#[test]
fn title_cached_after_scrollback_not_re_emitted() {
    let mut screen = Screen::new(20, 3, 100);
    let mut cache = RenderCache::new();

    // Set title, scroll → render_with_scrollback caches the title
    screen.process(b"\x1b]2;My Terminal\x07");
    screen.process(b"A\r\nB\r\nC\r\nD\r\nE");
    let _ = do_render_cycle(&mut screen, &mut cache);

    // Incremental render: title unchanged → should NOT re-emit
    let incr = screen.render(false, &mut cache);
    let incr_text = String::from_utf8_lossy(&incr);
    assert!(
        !incr_text.contains("My Terminal"),
        "unchanged title should NOT be re-emitted on incremental render"
    );

    // Change title
    screen.process(b"\x1b]2;New Title\x07");
    let incr2 = screen.render(false, &mut cache);
    let incr2_text = String::from_utf8_lossy(&incr2);
    assert!(
        incr2_text.contains("\x1b]2;New Title\x07"),
        "changed title should be emitted on incremental render"
    );
    assert!(
        !incr2_text.contains("My Terminal"),
        "old title should not appear"
    );
}

// ─── Cursor visibility preserved ────────────────────────────────────────────

#[test]
fn cursor_hidden_state_preserved_after_scrollback_injection() {
    let mut screen = Screen::new(20, 3, 100);
    let mut cache = RenderCache::new();

    // Hide cursor then produce output
    screen.process(b"\x1b[?25l");
    screen.process(b"A\r\nB\r\nC\r\nD\r\nE");

    let pending = screen.take_pending_scrollback();
    let output = screen.render_with_scrollback(&pending, &mut cache);
    let text = String::from_utf8_lossy(&output);

    // The render hides cursor at the start (?25l is always emitted early).
    // If cursor_visible is false, ?25h should NOT appear anywhere in the output.
    let show_count = text.matches("\x1b[?25h").count();
    assert_eq!(
        show_count, 0,
        "cursor show (?25h) should not appear when cursor is hidden"
    );

    // Verify the render DOES contain the hide sequence
    assert!(
        text.contains("\x1b[?25l"),
        "cursor hide should be present in render"
    );
}

// ─── Pending scrollback empty after drain ───────────────────────────────────

#[test]
fn pending_empty_after_drain_no_double_send() {
    let mut screen = Screen::new(20, 3, 100);

    screen.process(b"A\r\nB\r\nC\r\nD\r\nE\r\nF");

    let first = screen.take_pending_scrollback();
    assert_eq!(first.len(), 3, "first drain should have 3 lines");

    let second = screen.take_pending_scrollback();
    assert!(second.is_empty(), "second drain should be empty");

    // But get_history still returns all scrollback
    assert_eq!(
        screen.get_history().len(), 3,
        "history should still have 3 lines after drain"
    );
}

// ─── Output arriving between take_pending and render ────────────────────────

#[test]
fn output_between_take_and_render_captured_in_next_cycle() {
    // In the real server, the screen lock is held across take+render.
    // But this test verifies that if new output arrives after take_pending
    // but before the next cycle, it's correctly captured.

    let mut screen = Screen::new(20, 3, 100);
    let mut cache = RenderCache::new();

    screen.process(b"A\r\nB\r\nC\r\nD\r\nE");

    // Take pending (2 lines: A, B scrolled off the 3-row screen)
    let pending1 = screen.take_pending_scrollback();
    assert_eq!(pending1.len(), 2);

    // More output arrives before we render
    screen.process(b"\r\nF\r\nG");

    // Render with first batch (the new output is NOT in this render's scrollback)
    let _ = screen.render_with_scrollback(&pending1, &mut cache);

    // Next cycle: take new pending
    // After first take (A, B), we processed \r\nF\r\nG which scrolled off C and D.
    let pending2 = screen.take_pending_scrollback();
    assert_eq!(
        pending2.len(), 2,
        "new lines should be in next pending batch"
    );
    let texts2 = pending_texts(&pending2);
    assert!(texts2[0].contains("C"), "pending2 should have C");
    assert!(texts2[1].contains("D"), "pending2 should have D");
}

// ─── Alt screen interaction with pending scrollback ─────────────────────────

#[test]
fn alt_screen_does_not_generate_scrollback() {
    let mut screen = Screen::new(20, 3, 100);
    let mut cache = RenderCache::new();

    // Fill screen in main mode
    screen.process(b"main1\r\nmain2\r\nmain3");
    let _ = do_render_cycle(&mut screen, &mut cache);

    // Enter alt screen
    screen.process(b"\x1b[?1049h");

    // Write many lines in alt screen — should NOT generate scrollback
    for i in 1..=20 {
        screen.process(format!("alt{:02}\r\n", i).as_bytes());
    }

    let pending = screen.take_pending_scrollback();
    assert!(
        pending.is_empty(),
        "alt screen output should NOT generate pending scrollback, got {} lines",
        pending.len()
    );

    let hist = screen.get_history();
    assert!(
        hist.is_empty(),
        "alt screen output should NOT generate history, got {} lines",
        hist.len()
    );
}

#[test]
fn pending_scrollback_preserved_across_alt_screen_excursion() {
    let mut screen = Screen::new(20, 3, 100);

    // Generate scrollback in main mode
    screen.process(b"L01\r\nL02\r\nL03\r\nL04\r\nL05");
    // Pending should have 2 lines (L01, L02)

    // Enter alt screen (like vim opening)
    screen.process(b"\x1b[?1049h");
    screen.process(b"alt content\r\nalt line 2\r\nalt line 3\r\nalt line 4");

    // Exit alt screen (vim closing — restores main grid)
    screen.process(b"\x1b[?1049l");

    // The pending scrollback from main mode should still be there
    let pending = screen.take_pending_scrollback();
    assert_eq!(
        pending.len(), 2,
        "pending from before alt screen should survive the excursion"
    );
    let texts = pending_texts(&pending);
    assert!(texts[0].contains("L01"), "first pending should be L01");
    assert!(texts[1].contains("L02"), "second pending should be L02");
}

// ─── Non-zero scroll region does NOT generate scrollback ────────────────────

#[test]
fn scroll_region_not_at_top_produces_no_scrollback() {
    let mut screen = Screen::new(20, 5, 100);

    // Set scroll region to rows 2-4 (not starting at top)
    screen.process(b"\x1b[2;4r");
    screen.process(b"\x1b[2;1H"); // cursor to row 2

    // Write many lines inside scroll region — they scroll within the region
    for i in 1..=20 {
        if i < 20 {
            screen.process(format!("SR{:02}\r\n", i).as_bytes());
        } else {
            screen.process(format!("SR{:02}", i).as_bytes());
        }
    }

    let pending = screen.take_pending_scrollback();
    assert!(
        pending.is_empty(),
        "scrolling within non-top scroll region should NOT generate scrollback, got {} lines",
        pending.len()
    );

    let hist = screen.get_history();
    assert!(
        hist.is_empty(),
        "scrolling within non-top scroll region should NOT generate history"
    );

    // Row 0 (outside scroll region) should be untouched
    assert_eq!(
        screen.grid.visible_row(0)[0].c, ' ',
        "row above scroll region should be blank"
    );
}

// ─── Wide characters in scrollback ──────────────────────────────────────────

#[test]
fn wide_chars_in_scrollback_render_correctly() {
    let mut screen = Screen::new(20, 3, 100);

    // Write lines with wide characters that will scroll off
    screen.process("你好世界\r\n".as_bytes());
    screen.process("テスト\r\n".as_bytes());
    screen.process("plain\r\n".as_bytes());
    screen.process("visible1\r\n".as_bytes());
    screen.process("visible2".as_bytes());

    let pending = screen.take_pending_scrollback();
    assert_eq!(pending.len(), 2, "2 lines should scroll off");

    // Verify the rendered scrollback contains the wide characters
    let line0 = String::from_utf8_lossy(&pending[0]);
    assert!(
        line0.contains("你好世界"),
        "wide chars should be preserved in scrollback render, got: '{}'",
        line0
    );

    let line1 = String::from_utf8_lossy(&pending[1]);
    assert!(
        line1.contains("テスト"),
        "wide chars should be preserved in scrollback render, got: '{}'",
        line1
    );

    // Render with scrollback should not crash or produce garbage
    let mut cache = RenderCache::new();
    let output = screen.render_with_scrollback(&pending, &mut cache);
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("你好世界"), "wide chars in scrollback render output");
    assert!(text.contains("テスト"), "wide chars in scrollback render output");
}

// ─── Empty/blank lines in scrollback ────────────────────────────────────────

#[test]
fn blank_lines_in_scrollback_produce_empty_entries() {
    let mut screen = Screen::new(20, 3, 100);

    // Write blank lines that will scroll off
    screen.process(b"\r\n\r\n\r\nvisible1\r\nvisible2\r\nvisible3");
    // 3 blank lines scroll off, then visible1 scrolls off too

    let pending = screen.take_pending_scrollback();
    assert_eq!(pending.len(), 3, "3 lines should scroll off");

    // Blank lines should produce empty rendered entries
    assert!(
        pending[0].is_empty(),
        "blank scrollback line should render as empty, got {} bytes",
        pending[0].len()
    );
    assert!(
        pending[1].is_empty(),
        "blank scrollback line should render as empty, got {} bytes",
        pending[1].len()
    );

    // render_with_scrollback should handle empty entries without panicking
    let mut cache = RenderCache::new();
    let output = screen.render_with_scrollback(&pending, &mut cache);
    let text = String::from_utf8_lossy(&output);

    // Scrollback portion is everything before the sync block
    let sync_pos = text.find("\x1b[?2026h").expect("should have sync begin");
    let scrollback_portion = &text[..sync_pos];

    // Each scrollback entry (even blank) produces a row + EL via CUP positioning,
    // then \n at the bottom to scroll. Count \n in scrollback portion.
    let newline_count = scrollback_portion.matches('\n').count();
    assert!(
        newline_count >= pending.len(),
        "each scrollback entry (even blank) should produce a scroll \\n, got {} for {} entries",
        newline_count,
        pending.len()
    );
}

// ─── Mode change between scrollback batches ─────────────────────────────────

#[test]
fn mode_change_between_scrollback_batches_reflected_correctly() {
    // Real scenario: app enables bracketed paste, output scrolls,
    // then app disables bracketed paste, more output scrolls.
    // Each render cycle should reflect the mode state at that time.
    let mut screen = Screen::new(20, 3, 100);
    let mut cache = RenderCache::new();

    // Batch 1: bracketed paste ON, scroll
    screen.process(b"\x1b[?2004h");
    screen.process(b"A\r\nB\r\nC\r\nD\r\nE");
    let out1 = do_render_cycle(&mut screen, &mut cache);
    let t1 = String::from_utf8_lossy(&out1);
    assert!(
        t1.contains("\x1b[?2004h"),
        "batch 1: bracketed paste should be ON"
    );

    // Batch 2: disable bracketed paste, more scroll
    screen.process(b"\x1b[?2004l");
    screen.process(b"\r\nF\r\nG");
    let out2 = do_render_cycle(&mut screen, &mut cache);
    let t2 = String::from_utf8_lossy(&out2);
    // Full render (scrollback) emits all modes — bracketed paste should be OFF
    assert!(
        t2.contains("\x1b[?2004l"),
        "batch 2: bracketed paste should be OFF"
    );
    assert!(
        !t2.contains("\x1b[?2004h"),
        "batch 2: bracketed paste ON should NOT appear"
    );

    // Incremental render: no change → no mode emission
    let incr = screen.render(false, &mut cache);
    let incr_text = String::from_utf8_lossy(&incr);
    assert!(
        !incr_text.contains("?2004"),
        "incremental after batch 2: no mode change → no emission"
    );
}

// ─── Overwrite-then-scroll algorithm tests ──────────────────────────────────

/// Scrollback injection must reset the scroll region (`\x1b[r`) before
/// emitting \n at the bottom row, so that a custom DECSTBM from a previous
/// render doesn't confine the scroll to a sub-region.
#[test]
fn scrollback_injection_resets_scroll_region() {
    let mut screen = Screen::new(20, 5, 100);
    let mut cache = RenderCache::new();

    // Set a custom scroll region (rows 2-4)
    screen.process(b"\x1b[2;4r");
    // Generate some scrollback
    screen.process(b"\x1b[r"); // reset for output
    screen.process(b"A\r\nB\r\nC\r\nD\r\nE\r\nF\r\nG");
    let pending = screen.take_pending_scrollback();
    assert!(!pending.is_empty());

    let output = screen.render_with_scrollback(&pending, &mut cache);
    let text = String::from_utf8_lossy(&output);

    // \x1b[r (DECSTBM reset to full screen) must appear before the scrollback
    // content and before the bottom-row positioning
    let reset_pos = text.find("\x1b[r").expect("scroll region reset missing");
    let first_content = text.find("A").unwrap_or(text.len());
    assert!(
        reset_pos < first_content,
        "scroll region reset (pos {}) must precede scrollback content (pos {})",
        reset_pos, first_content
    );
}

/// When scrollback fits in a single chunk (scrollback.len() <= rows),
/// each line should be written to the correct row via CUP before scrolling.
#[test]
fn scrollback_single_chunk_overwrites_rows() {
    let mut screen = Screen::new(20, 5, 100);
    let mut cache = RenderCache::new();

    screen.process(b"AA\r\nBB\r\nCC\r\nDD\r\nEE\r\nFF\r\nGG");
    let pending = screen.take_pending_scrollback();
    assert_eq!(pending.len(), 2, "2 lines should scroll off");

    let output = screen.render_with_scrollback(&pending, &mut cache);
    let text = String::from_utf8_lossy(&output);

    // Lines should be positioned at rows 1 and 2 via CUP
    assert!(text.contains("\x1b[1;1H"), "row 1 positioning for first scrollback line");
    assert!(text.contains("\x1b[2;1H"), "row 2 positioning for second scrollback line");

    // Content should appear in correct order
    let pos_aa = text.find("AA").expect("AA missing");
    let pos_bb = text.find("BB").expect("BB missing");
    assert!(pos_aa < pos_bb, "AA should appear before BB");

    // EL (\x1b[K) should follow each line to clear remainder
    let raw = output;
    let aa_pos = raw.windows(2).position(|w| w == b"AA").unwrap();
    assert_eq!(&raw[aa_pos + 2..aa_pos + 5], b"\x1b[K",
        "EL should follow scrollback line content");
}

/// When scrollback exceeds visible rows, it should be processed in chunks.
/// Each chunk overwrites rows then scrolls. Final native scrollback must
/// contain all lines without duplication.
#[test]
fn scrollback_multi_chunk_processes_correctly() {
    let mut screen = Screen::new(20, 3, 100);
    let mut cache = RenderCache::new();

    // Generate 8 lines of scrollback (3 rows visible, so 3 chunks: 3+3+2)
    screen.process(b"L01\r\nL02\r\nL03\r\nL04\r\nL05\r\nL06\r\nL07\r\nL08\r\nV01\r\nV02\r\nV03");
    let pending = screen.take_pending_scrollback();
    assert_eq!(pending.len(), 8, "8 lines should scroll off");

    let output = screen.render_with_scrollback(&pending, &mut cache);
    let text = String::from_utf8_lossy(&output);

    // All 8 lines should appear in the output (overwritten to rows)
    for i in 1..=8 {
        let label = format!("L{:02}", i);
        assert!(text.contains(&label), "{} should be in scrollback output", label);
    }

    // Each chunk should have bottom-row positioning (\x1b[3;1H) followed by \n
    // There should be 3 bottom-row positionings (3 chunks)
    let bottom_count = text.matches("\x1b[3;1H").count();
    assert!(
        bottom_count >= 3,
        "expected at least 3 bottom-row positionings for 3 chunks, got {}",
        bottom_count
    );

    // All scrollback content must be before the sync block
    let sync_begin = text.find("\x1b[?2026h").expect("sync begin missing");
    let last_scrollback = text.rfind("L08").expect("L08 missing");
    assert!(
        last_scrollback < sync_begin,
        "all scrollback content must precede sync block"
    );

    // Visible content should be in the sync block (after screen clear)
    let pos_clear = text.find("\x1b[2J").expect("screen clear missing");
    let after_clear = &text[pos_clear..];
    assert!(after_clear.contains("V01"), "V01 should be in screen portion");
    assert!(after_clear.contains("V03"), "V03 should be in screen portion");
}

/// Partial last chunk should erase remaining rows below the content to prevent
/// stale data from leaking into native scrollback.
#[test]
fn scrollback_partial_chunk_erases_remaining_rows() {
    let mut screen = Screen::new(20, 4, 100);
    let mut cache = RenderCache::new();

    // 5 scrollback lines with 4 rows → chunk1(4) + chunk2(1)
    screen.process(b"S1\r\nS2\r\nS3\r\nS4\r\nS5\r\nV1\r\nV2\r\nV3\r\nV4");
    let pending = screen.take_pending_scrollback();
    assert_eq!(pending.len(), 5);

    let output = screen.render_with_scrollback(&pending, &mut cache);
    let text = String::from_utf8_lossy(&output);

    // In the second chunk (1 line), rows 2-4 should be erased (\x1b[2K)
    // to prevent stale content from leaking into scrollback.
    // Count EL2 (\x1b[...H\x1b[2K) sequences — should have at least 3
    // (rows 2, 3, 4 in the partial chunk).
    let el2_count = text.matches("\x1b[2K").count();
    assert!(
        el2_count >= 3,
        "partial chunk should erase at least 3 remaining rows, got {} EL2 sequences",
        el2_count
    );
}

/// Exactly `rows` scrollback lines should be handled in a single chunk
/// with no row erasure (no partial chunk).
#[test]
fn scrollback_exactly_rows_lines_single_chunk() {
    let mut screen = Screen::new(20, 4, 100);
    let mut cache = RenderCache::new();

    // Exactly 4 scrollback lines with 4 visible rows
    screen.process(b"E1\r\nE2\r\nE3\r\nE4\r\nV1\r\nV2\r\nV3\r\nV4");
    let pending = screen.take_pending_scrollback();
    assert_eq!(pending.len(), 4);

    let output = screen.render_with_scrollback(&pending, &mut cache);
    let text = String::from_utf8_lossy(&output);

    // All lines present
    for i in 1..=4 {
        let label = format!("E{}", i);
        assert!(text.contains(&label), "{} missing from output", label);
    }

    // No EL2 (\x1b[2K) should appear — all rows filled, no partial chunk
    let el2_count = text.matches("\x1b[2K").count();
    assert_eq!(
        el2_count, 0,
        "full chunk should not erase any rows, got {} EL2 sequences",
        el2_count
    );

    // Bottom-row positioning should appear exactly once (one chunk)
    let bottom_positions = text.matches("\x1b[4;1H").count();
    assert!(
        bottom_positions >= 1,
        "should have bottom-row positioning for the single chunk"
    );
}

/// Scrollback injection with a single line should still overwrite row 1
/// and scroll via \n at the bottom.
#[test]
fn scrollback_single_line_overwrites_and_scrolls() {
    let mut screen = Screen::new(20, 3, 100);
    let mut cache = RenderCache::new();

    screen.process(b"ONLY\r\nV1\r\nV2\r\nV3");
    let pending = screen.take_pending_scrollback();
    assert_eq!(pending.len(), 1);

    let output = screen.render_with_scrollback(&pending, &mut cache);
    let text = String::from_utf8_lossy(&output);

    // Row 1 should have the content
    assert!(text.contains("\x1b[1;1H"), "should position at row 1");
    assert!(text.contains("ONLY"), "scrollback content missing");

    // Rows 2-3 should be erased (partial chunk: 1 line, 3 rows)
    let el2_count = text.matches("\x1b[2K").count();
    assert!(
        el2_count >= 2,
        "should erase rows 2-3, got {} EL2 sequences",
        el2_count
    );

    // Bottom-row positioning + \n for scrolling
    assert!(text.contains("\x1b[3;1H"), "should position at bottom row for scroll");

    // All before sync block
    let sync_begin = text.find("\x1b[?2026h").unwrap();
    let content_pos = text.find("ONLY").unwrap();
    assert!(content_pos < sync_begin, "scrollback should precede sync block");
}

/// Verify the output byte ordering is correct end-to-end:
/// [hide cursor + reset scroll region] → [overwrite+scroll chunks] → [sync begin] → [clear+redraw] → [sync end]
#[test]
fn scrollback_byte_ordering_end_to_end() {
    let mut screen = Screen::new(20, 3, 100);
    let mut cache = RenderCache::new();

    screen.process(b"X1\r\nX2\r\nX3\r\nX4\r\nVIS");
    let pending = screen.take_pending_scrollback();

    let output = screen.render_with_scrollback(&pending, &mut cache);
    let text = String::from_utf8_lossy(&output);

    // 1. Starts with cursor hide
    assert!(text.starts_with("\x1b[?25l"), "should start with cursor hide");

    // 2. Scroll region reset before content
    let reset_pos = text.find("\x1b[r").expect("scroll region reset missing");

    // 3. Content before sync block
    let content_pos = text.find("X1").expect("X1 missing");
    assert!(reset_pos < content_pos, "reset before content");

    // 4. Sync block after all scrollback
    let sync_begin = text.find("\x1b[?2026h").expect("sync begin missing");
    let last_content = text.rfind("X2").expect("X2 missing");
    assert!(last_content < sync_begin, "content before sync");

    // 5. Screen clear inside sync block
    let clear_pos = text.find("\x1b[2J").expect("clear missing");
    assert!(sync_begin < clear_pos, "clear inside sync block");

    // 6. Ends with sync end
    assert!(text.ends_with("\x1b[?2026l"), "should end with sync end");
}

/// Large burst: simulate 100+ lines scrolling in one render cycle.
/// Verifies chunking handles large amounts correctly.
#[test]
fn scrollback_large_burst_chunked_correctly() {
    let mut screen = Screen::new(40, 5, 500);
    let mut cache = RenderCache::new();

    // Generate 50 lines of scrollback (54 lines with \r\n → 50 scroll off)
    for i in 1..=54 {
        screen.process(format!("LINE{:03}\r\n", i).as_bytes());
    }
    let pending = screen.take_pending_scrollback();
    assert_eq!(pending.len(), 50);

    let output = screen.render_with_scrollback(&pending, &mut cache);
    let text = String::from_utf8_lossy(&output);

    // All 50 scrollback lines should appear
    for i in 1..=50 {
        let label = format!("LINE{:03}", i);
        assert!(text.contains(&label), "{} missing from scrollback output", label);
    }

    // 50 lines / 5 rows = 10 full chunks, no partial
    // Each chunk has a bottom-row positioning
    let bottom_count = text.matches("\x1b[5;1H").count();
    assert!(
        bottom_count >= 10,
        "expected at least 10 bottom-row positionings for 10 chunks, got {}",
        bottom_count
    );

    // No EL2 (all chunks are full)
    let el2_count = text.matches("\x1b[2K").count();
    assert_eq!(el2_count, 0, "full chunks should not erase rows");

    // Visible lines after clear
    let after_clear = &text[text.find("\x1b[2J").unwrap()..];
    assert!(after_clear.contains("LINE051"), "LINE051 should be visible");
    assert!(after_clear.contains("LINE054"), "LINE054 should be visible");
}
