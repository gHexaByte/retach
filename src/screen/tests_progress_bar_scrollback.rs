//! Tests for progress bar / spinner erasure and scrollback interaction.
//!
//! Simulates the scenario where a CLI tool (like Claude Code) shows a progress
//! indicator, erases it via cursor movement (CUU + EL), and then outputs a large
//! response.  The key question: does the erased progress bar leak into scrollback?
//!
//! Scenarios covered:
//! - Basic CUU + EL erase followed by large output
//! - Multi-line progress widget erase
//! - Progress bar already scrolled into scrollback before erase attempt
//! - Spinner in-place update loop followed by erase and output
//! - Blank lines from erased region leaking into scrollback
//! - ED (erase display) patterns vs CUU + EL patterns

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
fn do_render_cycle(screen: &mut Screen, cache: &mut RenderCache) -> Vec<u8> {
    let pending = screen.take_pending_scrollback();
    if !pending.is_empty() {
        screen.render_with_scrollback(&pending, cache)
    } else {
        screen.render(false, cache)
    }
}

/// Read text content of visible grid row (0-indexed).
fn grid_row_text(screen: &Screen, row: usize) -> String {
    screen.grid.visible_row(row)
        .iter()
        .map(|c| c.c)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// Count occurrences of a substring in the history.
fn count_in_history(screen: &Screen, needle: &str) -> usize {
    history_texts(screen)
        .iter()
        .filter(|line| line.contains(needle))
        .count()
}

/// Count non-empty lines in history.
fn count_nonempty_history(screen: &Screen) -> usize {
    history_texts(screen)
        .iter()
        .filter(|line| !line.is_empty())
        .count()
}

/// Count empty/blank lines in history.
fn count_blank_history(screen: &Screen) -> usize {
    history_texts(screen)
        .iter()
        .filter(|line| line.is_empty())
        .count()
}

// ─── Basic: CUU + EL erase then large output ────────────────────────────────

#[test]
fn single_line_progress_erased_before_scrolling() {
    // Scenario: prompt on screen, progress bar below it, then erase + response.
    // The erase happens BEFORE any scrolling, so the blank line is what
    // eventually scrolls into scrollback (not the progress bar).
    let mut screen = Screen::new(40, 5, 100);

    // User prompt on row 0
    screen.process(b"user> what is rust?\r\n");
    // Progress bar on row 1 — cursor is at (1, 0) after \r\n
    screen.process(b"Thinking...");
    // Cursor is now at (1, 10)

    // App finishes thinking — erase progress bar
    // Cursor is already on row 1, just need CR + EL to erase
    screen.process(b"\r\x1b[K");  // CR + EL — erase the progress bar line

    // Now print a large response that will scroll everything off
    for i in 1..=10 {
        screen.process(format!("Response line {}\r\n", i).as_bytes());
    }

    let _ = screen.take_pending_scrollback();

    // The progress bar should NOT be in scrollback (it was erased before scrolling)
    assert_eq!(
        count_in_history(&screen, "Thinking"),
        0,
        "erased progress bar should not appear in scrollback"
    );

    // The prompt SHOULD be in scrollback
    assert_eq!(
        count_in_history(&screen, "user>"),
        1,
        "user prompt should be in scrollback"
    );
}

#[test]
fn multiline_progress_erased_before_scrolling() {
    // Progress widget spanning 3 lines: status, progress bar, ETA.
    // All three erased via CUU + EL before response output.
    let mut screen = Screen::new(40, 8, 100);

    // Prompt
    screen.process(b"user> complex query\r\n");
    // 3-line progress widget
    screen.process(b"Status: thinking\r\n");
    screen.process(b"[=====>     ] 50%\r\n");
    screen.process(b"ETA: 3s");

    // Erase 3-line widget: CUU 2 (go to "Status" line), then EL on each line
    screen.process(b"\x1b[2A");     // CUU 2 — up to "Status" line
    screen.process(b"\r\x1b[K");    // erase "Status: thinking"
    screen.process(b"\n\r\x1b[K");  // down, erase "[====..."
    screen.process(b"\n\r\x1b[K");  // down, erase "ETA: 3s"

    // Large response
    for i in 1..=20 {
        screen.process(format!("Answer line {}\r\n", i).as_bytes());
    }

    let _ = screen.take_pending_scrollback();

    assert_eq!(count_in_history(&screen, "Status: thinking"), 0);
    assert_eq!(count_in_history(&screen, "=====>"), 0);
    assert_eq!(count_in_history(&screen, "ETA:"), 0);
    assert_eq!(count_in_history(&screen, "user>"), 1);
}

// ─── Erased lines create blank lines in scrollback ──────────────────────────

#[test]
fn erased_progress_becomes_blank_line_in_scrollback() {
    // When progress bar is erased and then content scrolls it off,
    // the erased (blank) row enters scrollback.
    // This is expected terminal behavior — but it creates visible blank lines.
    let mut screen = Screen::new(40, 4, 100);

    // Fill the screen
    screen.process(b"prompt line\r\n");
    screen.process(b"Thinking...\r\n");
    screen.process(b"content A\r\n");
    screen.process(b"content B");

    // Erase "Thinking..." by going to its row and clearing
    screen.process(b"\x1b[2;1H");  // CUP to row 2, col 1 (the Thinking line)
    screen.process(b"\x1b[K");     // EL — erase line

    // Move cursor to bottom and output more lines (causing scroll)
    screen.process(b"\x1b[4;1H");  // CUP to row 4 (bottom)
    screen.process(b"\r\nnew1\r\nnew2\r\nnew3\r\nnew4");

    let _ = screen.take_pending_scrollback();
    let hist = history_texts(&screen);

    // "prompt line" should be in scrollback
    assert!(hist.iter().any(|l| l.contains("prompt")), "prompt should be in scrollback");

    // The blank line (formerly "Thinking...") will be in scrollback too
    // This is a known consequence — the blank row scrolls off
    let blank_count = count_blank_history(&screen);
    assert!(
        blank_count >= 1,
        "at least one blank line should be in scrollback (from erased progress bar)"
    );

    // But "Thinking..." itself should NOT be there
    assert_eq!(
        count_in_history(&screen, "Thinking"),
        0,
        "erased progress bar text should not appear in scrollback"
    );
}

// ─── Progress bar scrolls into scrollback BEFORE erase attempt ──────────────

#[test]
fn progress_bar_captured_in_scrollback_before_erase() {
    // THE CORE BUG: if output scrolls the progress bar into scrollback
    // before the erase sequence is processed, the progress bar persists
    // in scrollback because CUU+EL only affect the visible grid.
    //
    // Sequence:
    // 1. Progress bar on screen
    // 2. New output lines scroll it off into scrollback
    // 3. CUU+EL tries to erase — but only clears a visible grid row
    // 4. Progress bar remains in scrollback forever
    let mut screen = Screen::new(40, 4, 100);

    // Screen: 4 rows
    // Row 0: prompt
    // Row 1: Thinking...
    // Row 2: (empty)
    // Row 3: (empty)
    screen.process(b"user> hello\r\n");
    screen.process(b"Thinking...");

    // Now the app starts outputting response lines BEFORE erasing the progress bar.
    // This pushes the progress bar (and prompt) into scrollback.
    screen.process(b"\r\n");
    screen.process(b"Line 1\r\n");
    screen.process(b"Line 2\r\n");
    screen.process(b"Line 3\r\n");
    screen.process(b"Line 4");
    // At this point: "user> hello" and "Thinking..." are in scrollback.

    // NOW try to erase the progress bar with CUU + EL
    // CUU 4 would try to go up 4 rows, but we're at row 3 — we end up at row 0
    // This does NOT reach scrollback — it only clears whatever is on the visible grid
    screen.process(b"\x1b[4A");   // CUU 4 — go up (stops at row 0)
    screen.process(b"\r\x1b[K");  // erase line — only affects visible grid row 0

    let _ = screen.take_pending_scrollback();

    // The progress bar IS in scrollback — it was captured before the erase attempt
    assert_eq!(
        count_in_history(&screen, "Thinking"),
        1,
        "progress bar should be in scrollback (captured before erase could reach it)"
    );

    // The erase only affected grid row 0 (which was "Line 1"), not scrollback
    assert_eq!(
        count_in_history(&screen, "user>"),
        1,
        "prompt should also be in scrollback"
    );
}

#[test]
fn cuu_cannot_reach_scrollback() {
    // Verify that CUU (cursor up) stops at row 0 and cannot affect scrollback.
    let mut screen = Screen::new(40, 3, 100);

    // Fill and scroll: 5 lines on 3-row screen → 2 go to scrollback
    screen.process(b"SCROLL1\r\nSCROLL2\r\nVIS1\r\nVIS2\r\nVIS3");
    let _ = screen.take_pending_scrollback();
    assert_eq!(screen.get_history().len(), 2);

    // Cursor is at row 2 (bottom). CUU 100 — should stop at row 0.
    screen.process(b"\x1b[100A");
    assert_eq!(screen.grid.cursor_y, 0, "CUU should stop at row 0");

    // Erase entire line at row 0 — this erases "VIS1" on screen, NOT scrollback
    // Use EL 2 (erase entire line) since CUU doesn't reset cursor_x
    screen.process(b"\x1b[2K");

    // Scrollback should still have both original lines untouched
    let hist = history_texts(&screen);
    assert_eq!(hist.len(), 2);
    assert!(hist[0].contains("SCROLL1"));
    assert!(hist[1].contains("SCROLL2"));

    // Grid row 0 should now be blank
    assert!(grid_row_text(&screen, 0).is_empty(), "grid row 0 should be erased");
}

// ─── Claude Code-like spinner loop ──────────────────────────────────────────

#[test]
fn spinner_loop_then_erase_then_large_output() {
    // Simulates Claude Code's spinner pattern:
    // 1. Print "⏳ Thinking..."
    // 2. CR, overwrite with "⏳ Thinking... (2s)"
    // 3. CR, overwrite with "⏳ Thinking... (4s)"
    // 4. Erase spinner line
    // 5. Print large response
    let mut screen = Screen::new(60, 6, 200);

    // User prompt
    screen.process(b"user> explain monads\r\n");

    // Spinner iterations (overwrite in place with CR)
    screen.process(b"Thinking...");
    screen.process(b"\rThinking... (2s)\x1b[K");
    screen.process(b"\rThinking... (4s)\x1b[K");

    // Erase spinner
    screen.process(b"\r\x1b[K");

    // Large response
    for i in 1..=30 {
        screen.process(format!("Response line {:02}\r\n", i).as_bytes());
    }

    let _ = screen.take_pending_scrollback();

    // No spinner remnants in scrollback
    assert_eq!(
        count_in_history(&screen, "Thinking"),
        0,
        "spinner text should not appear in scrollback after erasure"
    );

    // But prompt should be there
    assert_eq!(
        count_in_history(&screen, "user>"),
        1,
        "user prompt should be in scrollback"
    );

    // Response lines should be in scrollback
    assert!(
        count_in_history(&screen, "Response line") > 0,
        "response lines should be in scrollback"
    );
}

#[test]
fn multiline_spinner_then_erase_then_large_output() {
    // Like Claude Code's thinking indicator which may span 2-3 lines:
    // Line 1: "⏳ Thinking..."
    // Line 2: "   effecting changes..."
    // Then erase both and print response.
    let mut screen = Screen::new(60, 6, 200);

    screen.process(b"user> refactor auth module\r\n");

    // 2-line progress widget
    screen.process(b"Thinking...\r\n");
    screen.process(b"  effecting changes...");

    // Erase: CUU 1 to go to "Thinking..." line, erase both lines
    screen.process(b"\x1b[A");      // CUU 1
    screen.process(b"\r\x1b[K");    // erase "Thinking..."
    screen.process(b"\n\r\x1b[K");  // down + erase "effecting..."
    // Move cursor back up to where the progress was
    screen.process(b"\x1b[A");      // CUU 1

    // Large response from current position
    for i in 1..=30 {
        screen.process(format!("Response {:02}\r\n", i).as_bytes());
    }

    let _ = screen.take_pending_scrollback();

    assert_eq!(count_in_history(&screen, "Thinking"), 0);
    assert_eq!(count_in_history(&screen, "effecting"), 0);
    assert_eq!(count_in_history(&screen, "user>"), 1);
}

// ─── The exact Claude Code reconnect scenario ───────────────────────────────

#[test]
fn progress_bar_visible_in_scrollback_after_reconnect() {
    // Full scenario through retach:
    // 1. User types prompt
    // 2. Claude Code shows spinner
    // 3. Spinner is erased
    // 4. Large response is output (scrolls everything up)
    // 5. User reconnects — scrollback should be clean
    let mut screen = Screen::new(60, 8, 500);
    let mut cache = RenderCache::new();

    // Step 1-2: prompt + spinner
    screen.process(b"user> explain async/await\r\n");
    screen.process(b"Thinking...");
    let _ = do_render_cycle(&mut screen, &mut cache);

    // Step 3: erase spinner (CR + EL)
    screen.process(b"\r\x1b[K");

    // Step 4: large response
    for i in 1..=50 {
        screen.process(format!("Async/await line {:02}\r\n", i).as_bytes());
    }
    let _ = do_render_cycle(&mut screen, &mut cache);

    // Step 5: simulate reconnect
    let _ = screen.take_pending_scrollback();
    let history = screen.get_history();
    let hist_texts: Vec<String> = history.iter().map(|b| strip_ansi(b)).collect();

    // Verify: no spinner in history
    assert!(
        !hist_texts.iter().any(|l| l.contains("Thinking")),
        "spinner should not be in scrollback history after reconnect"
    );

    // Verify: prompt IS in history
    assert!(
        hist_texts.iter().any(|l| l.contains("user>")),
        "prompt should be in scrollback history"
    );

    // Verify: response lines are in history
    assert!(
        hist_texts.iter().any(|l| l.contains("Async/await line")),
        "response content should be in scrollback history"
    );
}

// ─── Blank line artifacts ────────────────────────────────────────────────────

#[test]
fn erased_region_creates_blank_artifact_in_scrollback() {
    // When a progress widget is erased (lines become blank) and then
    // scrolling pushes those blank lines into scrollback, the user sees
    // unexpected blank lines when scrolling up.
    //
    // This test documents the behavior — blank lines from erasure DO end up
    // in scrollback. This is expected behavior in any terminal (real or emulated).
    let mut screen = Screen::new(40, 5, 100);

    // Fill screen with content
    screen.process(b"Header\r\n");
    screen.process(b"Progress: [===>   ]\r\n");
    screen.process(b"Status: working\r\n");
    screen.process(b"content A\r\n");
    screen.process(b"content B");

    // Erase the 2-line progress widget (rows 1-2)
    screen.process(b"\x1b[2;1H\x1b[K");  // CUP(2,1) + EL — erase "Progress..."
    screen.process(b"\x1b[3;1H\x1b[K");  // CUP(3,1) + EL — erase "Status..."

    // Now output enough to scroll everything off
    screen.process(b"\x1b[5;1H");  // CUP to bottom row
    for i in 1..=10 {
        screen.process(format!("\r\nnew line {}", i).as_bytes());
    }

    let _ = screen.take_pending_scrollback();
    let hist = history_texts(&screen);

    // Header should be in scrollback
    assert!(hist.iter().any(|l| l.contains("Header")));

    // The erased progress lines should NOT be in scrollback as their original text
    assert!(!hist.iter().any(|l| l.contains("Progress:")));
    assert!(!hist.iter().any(|l| l.contains("Status: working")));

    // But blank lines WILL be in scrollback (the erased rows)
    let blanks = hist.iter().filter(|l| l.is_empty()).count();
    assert!(
        blanks >= 2,
        "expected at least 2 blank lines in scrollback (from erased progress widget), got {}",
        blanks
    );
}

// ─── ED (erase display) patterns ─────────────────────────────────────────────

#[test]
fn ed2_erase_display_does_not_leak_to_scrollback() {
    // ED 2 (ESC[2J) erases visible display but should NOT add to scrollback.
    // Some apps use this instead of CUU+EL to clear before redrawing.
    let mut screen = Screen::new(40, 5, 100);

    screen.process(b"prompt\r\n");
    screen.process(b"Thinking...\r\n");
    screen.process(b"more stuff");

    // Clear entire display
    screen.process(b"\x1b[2J");
    screen.process(b"\x1b[H");  // cursor home

    // New content
    for i in 1..=20 {
        screen.process(format!("Result {}\r\n", i).as_bytes());
    }

    let _ = screen.take_pending_scrollback();

    // Content before ED 2 should NOT be in scrollback
    // (ED 2 blanks the grid but doesn't push to scrollback)
    assert_eq!(count_in_history(&screen, "Thinking"), 0);
    assert_eq!(count_in_history(&screen, "prompt"), 0);

    // Only the "Result" lines that scrolled off should be in scrollback
    assert!(count_in_history(&screen, "Result") > 0);
}

#[test]
fn ed3_erase_display_with_scrollback_clears_history() {
    // ED 3 (ESC[3J) should clear the scrollback buffer entirely.
    let mut screen = Screen::new(40, 3, 100);

    // Create some scrollback
    screen.process(b"old1\r\nold2\r\nold3\r\nold4\r\nold5");
    let _ = screen.take_pending_scrollback();
    assert!(screen.get_history().len() > 0, "should have scrollback before ED 3");

    // ED 3 — erase display including scrollback
    screen.process(b"\x1b[3J");

    assert_eq!(
        screen.get_history().len(),
        0,
        "ED 3 should clear scrollback history"
    );
}

// ─── EL (erase line) cannot reach scrollback ────────────────────────────────

#[test]
fn el_only_affects_current_grid_row() {
    // Verify EL variants only affect the current row on the visible grid.
    let mut screen = Screen::new(40, 3, 100);

    // Scroll content off
    screen.process(b"scrolled off\r\nVIS1\r\nVIS2\r\nVIS3");
    let _ = screen.take_pending_scrollback();
    assert!(history_texts(&screen).iter().any(|l| l.contains("scrolled off")));

    // EL 2 on row 0 (VIS1)
    screen.process(b"\x1b[1;1H");  // CUP row 1
    screen.process(b"\x1b[2K");    // EL 2 — erase entire line

    // Scrollback should be unaffected
    assert!(
        history_texts(&screen).iter().any(|l| l.contains("scrolled off")),
        "EL should not modify scrollback"
    );

    // Grid row 0 should be blank
    assert!(grid_row_text(&screen, 0).is_empty(), "grid row should be erased");
}

// ─── Large response interleaved with cursor movement ────────────────────────

#[test]
fn response_with_cursor_home_and_overwrite_no_scrollback_leak() {
    // Simulates an app that uses cursor home + overwrite to update a status area
    // at the top while outputting content below. If the status is overwritten
    // BEFORE it scrolls off, scrollback should contain the new version.
    let mut screen = Screen::new(40, 5, 100);

    // Initial layout:
    // Row 0: status bar (will be updated in place)
    // Row 1-3: content (not enough to cause scroll)
    screen.process(b"\x1b[1;1HStatus: idle\x1b[K");
    screen.process(b"\x1b[2;1Hcontent 1\x1b[K");
    screen.process(b"\x1b[3;1Hcontent 2\x1b[K");
    screen.process(b"\x1b[4;1Hcontent 3\x1b[K");

    // Status update BEFORE any scrolling (in-place overwrite)
    screen.process(b"\x1b[1;1HStatus: busy\x1b[K");

    // Now output from bottom — this will cause scrolling
    screen.process(b"\x1b[5;1H");
    for i in 4..=20 {
        screen.process(format!("content {}\r\n", i).as_bytes());
    }

    let _ = screen.take_pending_scrollback();

    // "Status: idle" should NOT be in scrollback (was overwritten before scrolling)
    assert_eq!(
        count_in_history(&screen, "Status: idle"),
        0,
        "overwritten status should not appear in scrollback"
    );

    // "Status: busy" SHOULD be in scrollback (the version that was on screen when it scrolled)
    assert_eq!(
        count_in_history(&screen, "Status: busy"),
        1,
        "current status should appear in scrollback after scrolling"
    );
}

// ─── Progress bar with SGR styling ──────────────────────────────────────────

#[test]
fn styled_progress_bar_erased_cleanly() {
    // Progress bar with colors/bold, erased before large output.
    // Verifies no styled remnants leak into scrollback.
    let mut screen = Screen::new(60, 5, 100);

    screen.process(b"user> go\r\n");
    // Bold yellow progress bar
    screen.process(b"\x1b[1;33m[=====>     ] 50%\x1b[0m");

    // Erase it
    screen.process(b"\r\x1b[K");

    // Large response
    for i in 1..=20 {
        screen.process(format!("Reply line {}\r\n", i).as_bytes());
    }

    let _ = screen.take_pending_scrollback();

    assert_eq!(count_in_history(&screen, "=====>"), 0);
    assert_eq!(count_in_history(&screen, "50%"), 0);
    assert_eq!(count_in_history(&screen, "user>"), 1);
}

// ─── Race condition: output before erase ────────────────────────────────────

#[test]
fn output_arrives_before_progress_erase_captures_progress_in_scrollback() {
    // Simulates a timing issue where response output starts before the
    // progress bar erase sequence. This can happen if the app writes
    // response data to stdout before the erase sequence.
    //
    // In this case, the progress bar scrolls into scrollback BEFORE
    // the erase can reach it — and there's nothing retach can do about it.
    let mut screen = Screen::new(40, 3, 100);

    // Prompt + progress bar (on 3-row screen)
    screen.process(b"prompt\r\n");
    screen.process(b"Thinking...\r\n");
    screen.process(b"Ready");

    // Response starts arriving (new lines push old ones into scrollback)
    screen.process(b"\r\nResponse 1\r\nResponse 2\r\nResponse 3");
    // Now "prompt" and "Thinking..." are in scrollback

    // Erase attempt comes too late — CUU can only reach visible grid
    screen.process(b"\x1b[3A\r\x1b[K");

    let _ = screen.take_pending_scrollback();

    // Progress bar IS in scrollback because it scrolled off before erase
    assert_eq!(
        count_in_history(&screen, "Thinking"),
        1,
        "progress bar should be in scrollback (scrolled off before erase)"
    );
}

// ─── Save/restore cursor around progress bar ────────────────────────────────

#[test]
fn save_restore_cursor_around_progress_erase() {
    // Some apps save cursor, write progress, restore cursor, then continue output.
    // The progress bar should be overwritten by subsequent output, not leak.
    let mut screen = Screen::new(60, 5, 100);

    screen.process(b"Header\r\n");
    screen.process(b"Line 1\r\n");
    screen.process(b"Line 2");

    // Save cursor position (at end of "Line 2")
    screen.process(b"\x1b7");  // DECSC

    // Write progress on next line
    screen.process(b"\r\n");
    screen.process(b"Spinning...");

    // Restore cursor and erase from cursor to end of display
    screen.process(b"\x1b8");      // DECRC — back to "Line 2"
    screen.process(b"\x1b[J");     // ED 0 — erase from cursor to end

    // Continue output
    for i in 3..=20 {
        screen.process(format!("\r\nLine {}", i).as_bytes());
    }

    let _ = screen.take_pending_scrollback();

    // Spinning should not be in scrollback (erased by ED 0)
    assert_eq!(
        count_in_history(&screen, "Spinning"),
        0,
        "progress written below saved cursor should be erased by ED 0"
    );
}

// ─── Full client session simulation ─────────────────────────────────────────

#[test]
fn full_session_prompt_spinner_response_scrollback_consistency() {
    // Simulates a full Claude Code session through retach:
    // Multiple prompt/response cycles, each with spinner.
    let mut screen = Screen::new(80, 10, 1000);
    let mut cache = RenderCache::new();

    for cycle in 1..=5 {
        // User prompt
        screen.process(format!("user> question {}\r\n", cycle).as_bytes());

        // Spinner
        screen.process(format!("Thinking (cycle {})...", cycle).as_bytes());
        let _ = do_render_cycle(&mut screen, &mut cache);

        // Erase spinner
        screen.process(b"\r\x1b[K");

        // Response (enough to fill ~2 screens)
        for j in 1..=15 {
            screen.process(format!("Answer {} line {:02}\r\n", cycle, j).as_bytes());
        }
        let _ = do_render_cycle(&mut screen, &mut cache);
    }

    // Verify: no spinners in history
    for cycle in 1..=5 {
        assert_eq!(
            count_in_history(&screen, &format!("Thinking (cycle {})", cycle)),
            0,
            "spinner from cycle {} should not be in scrollback",
            cycle
        );
    }

    // Verify: all prompts are in history
    for cycle in 1..=5 {
        assert_eq!(
            count_in_history(&screen, &format!("question {}", cycle)),
            1,
            "prompt from cycle {} should appear exactly once in scrollback",
            cycle
        );
    }
}

// ─── Scroll region interaction ──────────────────────────────────────────────

#[test]
fn progress_in_scroll_region_does_not_leak_to_scrollback() {
    // App uses a scroll region to contain a status area.
    // Progress bar in the non-scrolling area should never reach scrollback.
    let mut screen = Screen::new(40, 6, 100);

    // Row 0: header (fixed)
    screen.process(b"\x1b[1;1HHeader\x1b[K");
    // Row 5: status/progress (fixed)
    screen.process(b"\x1b[6;1HThinking...\x1b[K");

    // Set scroll region to rows 2-5 (middle area)
    screen.process(b"\x1b[2;5r");
    screen.process(b"\x1b[2;1H");

    // Output in scroll region
    for i in 1..=20 {
        screen.process(format!("scrolling content {}\r\n", i).as_bytes());
    }

    let _ = screen.take_pending_scrollback();

    // Content scrolling within a non-top region should NOT go to scrollback
    assert_eq!(
        screen.get_history().len(),
        0,
        "scroll region not at top should not generate scrollback"
    );
}

// ─── DEC save/restore and progress ──────────────────────────────────────────

#[test]
fn progress_overwritten_by_normal_output_no_scrollback_leak() {
    // If the cursor is positioned at the progress bar location and new text
    // overwrites it character by character, the overwritten content should
    // be what ends up in scrollback — not the original progress bar.
    let mut screen = Screen::new(40, 4, 100);

    // Fill screen
    screen.process(b"line A\r\n");
    screen.process(b"Thinking...\r\n");
    screen.process(b"line C\r\n");
    screen.process(b"line D");

    // Overwrite "Thinking..." with "Response 1" by positioning cursor there
    screen.process(b"\x1b[2;1H");
    screen.process(b"Response 01\x1b[K");

    // Continue from bottom
    screen.process(b"\x1b[4;1H");
    for i in 1..=10 {
        screen.process(format!("\r\nmore output {}", i).as_bytes());
    }

    let _ = screen.take_pending_scrollback();

    // "Thinking..." should NOT be in scrollback (was overwritten)
    assert_eq!(count_in_history(&screen, "Thinking"), 0);

    // "Response 01" should be in scrollback (the overwritten version)
    assert_eq!(
        count_in_history(&screen, "Response 01"),
        1,
        "overwritten line should be in scrollback with new content"
    );
}

// ─── Pending scrollback during progress/erase cycle ─────────────────────────

#[test]
fn pending_scrollback_correct_during_progress_erase_cycle() {
    // Verifies that pending scrollback (used for live client updates)
    // correctly reflects the state after progress erasure and response output.
    let mut screen = Screen::new(40, 4, 100);
    let mut cache = RenderCache::new();

    // Initial content
    screen.process(b"A\r\nB\r\nC\r\nD");
    let _ = do_render_cycle(&mut screen, &mut cache);

    // Progress bar (in place, no scroll)
    screen.process(b"\r\x1b[K");
    screen.process(b"Thinking...");

    // No scrollback should have been generated yet
    let pending0 = screen.take_pending_scrollback();
    assert!(pending0.is_empty(), "progress bar in-place should not generate scrollback");

    // Erase progress bar
    screen.process(b"\r\x1b[K");

    // Response output (scrolls)
    screen.process(b"\r\nE\r\nF\r\nG\r\nH\r\nI");
    let pending1 = screen.take_pending_scrollback();
    let texts = pending_texts(&pending1);

    // The pending scrollback should contain what scrolled off
    // A, B, C were on screen. D was overwritten with "Thinking..." then erased.
    // After erase, row 3 is blank. Then \r\nE pushes A off, etc.
    assert!(pending1.len() > 0, "response output should generate scrollback");

    // None of the pending lines should contain "Thinking..."
    assert!(
        !texts.iter().any(|t| t.contains("Thinking")),
        "pending scrollback should not contain erased progress bar"
    );
}

// ─── Reverse index (RI) at top ──────────────────────────────────────────────

#[test]
fn reverse_index_at_top_does_not_affect_scrollback() {
    // RI (ESC M) at row 0 scrolls the grid DOWN (inserts blank at top).
    // This should NOT interact with scrollback at all.
    let mut screen = Screen::new(40, 3, 100);

    // Create scrollback
    screen.process(b"S1\r\nS2\r\nV1\r\nV2\r\nV3");
    let _ = screen.take_pending_scrollback();
    let hist_before = screen.get_history().len();

    // Move to top and do reverse index
    screen.process(b"\x1b[1;1H");
    screen.process(b"\x1bM");  // RI — reverse index

    let hist_after = screen.get_history().len();
    assert_eq!(
        hist_before, hist_after,
        "reverse index should not modify scrollback"
    );
}
