use super::*;

#[test]
fn alt_screen_save_restore() {
    let mut screen = Screen::new(10, 3, 100);

    // Write "Hello" on the main screen
    screen.process(b"Hello");
    assert_eq!(screen.grid.cells[0][0].c, 'H');
    assert_eq!(screen.grid.cells[0][4].c, 'o');

    // Enter alt screen (CSI ?1049h)
    screen.process(b"\x1b[?1049h");
    assert!(screen.state.in_alt_screen);
    // Alt screen should be cleared
    assert_eq!(screen.grid.cells[0][0].c, ' ');

    // Write something on alt screen
    screen.process(b"Alt");
    assert_eq!(screen.grid.cells[0][0].c, 'A');

    // Leave alt screen (CSI ?1049l) — should restore main buffer (fix S7)
    screen.process(b"\x1b[?1049l");
    assert!(!screen.state.in_alt_screen);
    assert_eq!(screen.grid.cells[0][0].c, 'H');
    assert_eq!(screen.grid.cells[0][4].c, 'o');
}

#[test]
fn scrollback_on_scroll() {
    let mut screen = Screen::new(10, 3, 100);
    // Fill 3 rows and scroll
    screen.process(b"Line1\r\nLine2\r\nLine3\r\nLine4");
    let scrollback = screen.take_pending_scrollback();
    assert!(!scrollback.is_empty());
    // First scrolled line should contain "Line1"
    let first = String::from_utf8_lossy(&scrollback[0]);
    assert!(first.contains("Line1"), "scrollback should contain Line1, got: {}", first);
}

#[test]
fn no_scrollback_in_alt_screen() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"\x1b[?1049h"); // enter alt screen
    screen.process(b"A\r\nB\r\nC\r\nD"); // scroll in alt
    let scrollback = screen.take_pending_scrollback();
    assert!(scrollback.is_empty(), "alt screen should not generate scrollback");
}

#[test]
fn history_preserved_across_sessions() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"A\r\nB\r\nC\r\nD\r\nE");
    let _ = screen.take_pending_scrollback();
    let history = screen.get_history();
    assert!(!history.is_empty());
}

#[test]
fn deferred_wrap_cr_stays_on_same_line() {
    // Simulates zsh PROMPT_SP: fill line to end, CR, overwrite
    let mut screen = Screen::new(5, 3, 100);
    // Write exactly 5 chars to fill the line
    screen.process(b"%    ");
    // wrap_pending should be set, cursor stays on row 0
    assert!(screen.grid.wrap_pending);
    assert_eq!(screen.grid.cursor_y, 0);
    // CR should clear wrap_pending and go to column 0 of SAME row
    screen.process(b"\r");
    assert!(!screen.grid.wrap_pending);
    assert_eq!(screen.grid.cursor_x, 0);
    assert_eq!(screen.grid.cursor_y, 0);
    // Space overwrites the '%'
    screen.process(b" ");
    assert_eq!(screen.grid.cells[0][0].c, ' ');
}

#[test]
fn deferred_wrap_next_print_wraps() {
    let mut screen = Screen::new(5, 3, 100);
    // Fill line
    screen.process(b"ABCDE");
    assert!(screen.grid.wrap_pending);
    assert_eq!(screen.grid.cursor_y, 0);
    // Next char triggers actual wrap
    screen.process(b"F");
    assert_eq!(screen.grid.cursor_y, 1);
    assert_eq!(screen.grid.cells[1][0].c, 'F');
}

// --- New tests for escape sequence completeness ---

#[test]
fn dsr_cursor_position_report() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[5;10H"); // move to row 5, col 10
    screen.process(b"\x1b[6n");     // request CPR
    let responses = screen.take_responses();
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0], b"\x1b[5;10R");
}

#[test]
fn da1_primary_device_attributes() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[c");
    let responses = screen.take_responses();
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0], b"\x1b[?62;c");
}

#[test]
fn da2_secondary_device_attributes() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[>c");
    let responses = screen.take_responses();
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0], b"\x1b[>0;10;1c");
}

#[test]
fn dec_line_drawing_charset() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b(0");  // switch G0 to line drawing
    screen.process(b"lqk");    // should produce box-drawing chars
    assert_eq!(screen.grid.cells[0][0].c, '\u{250C}'); // ┌
    assert_eq!(screen.grid.cells[0][1].c, '\u{2500}'); // ─
    assert_eq!(screen.grid.cells[0][2].c, '\u{2510}'); // ┐
    // Switch back to ASCII
    screen.process(b"\x1b(B");
    screen.process(b"l");
    assert_eq!(screen.grid.cells[0][3].c, 'l'); // plain ASCII 'l'
}

#[test]
fn rep_repeats_last_char() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"A\x1b[3b"); // print A, then repeat 3 times
    assert_eq!(screen.grid.cells[0][0].c, 'A');
    assert_eq!(screen.grid.cells[0][1].c, 'A');
    assert_eq!(screen.grid.cells[0][2].c, 'A');
    assert_eq!(screen.grid.cells[0][3].c, 'A');
}

#[test]
fn wide_character_occupies_two_cells() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process("你".as_bytes());
    assert_eq!(screen.grid.cells[0][0].c, '你');
    assert_eq!(screen.grid.cells[0][0].width, 2);
    assert_eq!(screen.grid.cells[0][1].width, 0);
    assert_eq!(screen.grid.cursor_x, 2);
}

#[test]
fn wide_char_wraps_at_end_of_line() {
    let mut screen = Screen::new(5, 3, 100);
    screen.process(b"ABCD"); // fill 4 of 5 cols
    screen.process("你".as_bytes()); // needs 2 cols, only 1 left -> should wrap
    // Col 4 should be blanked, wide char on next line
    assert_eq!(screen.grid.cells[0][4].c, ' ');
    assert_eq!(screen.grid.cells[1][0].c, '你');
    assert_eq!(screen.grid.cells[1][0].width, 2);
    assert_eq!(screen.grid.cells[1][1].width, 0);
}

#[test]
fn esc_c_full_reset() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[?2004h");  // enable bracketed paste
    screen.process(b"\x1b[5;10H");   // move cursor
    screen.process(b"Hello");
    screen.process(b"\x1b[2 q");     // set cursor shape
    screen.process(b"\x1bc");         // full reset
    assert_eq!(screen.grid.cursor_x, 0);
    assert_eq!(screen.grid.cursor_y, 0);
    assert!(!screen.grid.modes.bracketed_paste);
    assert_eq!(screen.grid.modes.cursor_shape, grid::CursorShape::Default);
    assert!(screen.grid.cursor_visible);
    assert_eq!(screen.grid.cells[0][0].c, ' ');
    assert!(screen.state.title.is_empty());
}

#[test]
fn osc_sets_title() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b]0;My Terminal\x07");
    assert_eq!(screen.state.title, "My Terminal");
    screen.process(b"\x1b]2;New Title\x07");
    assert_eq!(screen.state.title, "New Title");
}

#[test]
fn osc_passthrough_non_title() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b]777;notify;Test;Hello\x07");
    let pt = screen.take_passthrough();
    assert_eq!(pt.len(), 1, "should have one passthrough sequence");
    assert_eq!(pt[0], b"\x1b]777;notify;Test;Hello\x07");
    // Title should not be set
    assert_eq!(screen.state.title, "");
}

#[test]
fn osc_title_not_passedthrough() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b]0;My Title\x07");
    let pt = screen.take_passthrough();
    assert!(pt.is_empty(), "OSC 0 should not be passedthrough");
    assert_eq!(screen.state.title, "My Title");
}

// --- Bell / BEL tests ---

#[test]
fn bell_forwarded_as_passthrough() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x07");
    let pt = screen.take_passthrough();
    assert_eq!(pt.len(), 1, "standalone BEL should produce one passthrough");
    assert_eq!(pt[0], b"\x07");
}

#[test]
fn bell_does_not_affect_screen_state() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"Hello");
    let (cx, cy) = (screen.grid.cursor_x, screen.grid.cursor_y);
    screen.process(b"\x07");
    assert_eq!(screen.grid.cursor_x, cx, "BEL should not move cursor x");
    assert_eq!(screen.grid.cursor_y, cy, "BEL should not move cursor y");
    assert_eq!(screen.grid.cells[0][0].c, 'H', "BEL should not alter cell content");
}

#[test]
fn bell_drained_after_take() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x07");
    let pt1 = screen.take_passthrough();
    assert_eq!(pt1.len(), 1);
    // Second take should be empty
    let pt2 = screen.take_passthrough();
    assert!(pt2.is_empty(), "BEL should not persist after take_passthrough()");
}

#[test]
fn bell_not_resent_on_render() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x07");
    let _ = screen.take_passthrough(); // drain
    // Render (simulates screen redraw)
    let mut cache = RenderCache::new();
    let _ = screen.render(true, &mut cache);
    // Passthrough should still be empty
    let pt = screen.take_passthrough();
    assert!(pt.is_empty(), "BEL must not be re-sent on full redraw");
}

#[test]
fn bell_not_resent_on_incremental_render() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x07");
    let _ = screen.take_passthrough(); // drain
    let mut cache = RenderCache::new();
    let _ = screen.render(false, &mut cache);
    let pt = screen.take_passthrough();
    assert!(pt.is_empty(), "BEL must not be re-sent on incremental render");
}

#[test]
fn bell_not_resent_on_resize() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x07");
    let _ = screen.take_passthrough(); // drain
    screen.resize(120, 40);
    let pt = screen.take_passthrough();
    assert!(pt.is_empty(), "BEL must not be re-sent after resize");
}

#[test]
fn osc_777_drained_after_take() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b]777;notify;title;body\x07");
    let pt1 = screen.take_passthrough();
    assert_eq!(pt1.len(), 1);
    let pt2 = screen.take_passthrough();
    assert!(pt2.is_empty(), "OSC 777 should not persist after take_passthrough()");
}

#[test]
fn osc_777_not_resent_on_render() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b]777;notify;title;body\x07");
    let _ = screen.take_passthrough(); // drain
    let mut cache = RenderCache::new();
    let _ = screen.render(true, &mut cache);
    let pt = screen.take_passthrough();
    assert!(pt.is_empty(), "OSC 777 must not be re-sent on full redraw");
}

#[test]
fn osc_777_not_resent_on_resize() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b]777;notify;title;body\x07");
    let _ = screen.take_passthrough(); // drain
    screen.resize(120, 40);
    let pt = screen.take_passthrough();
    assert!(pt.is_empty(), "OSC 777 must not be re-sent after resize");
}

#[test]
fn osc_777_not_resent_on_resize_then_render() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b]777;notify;title;body\x07");
    let _ = screen.take_passthrough(); // drain
    screen.resize(40, 10);
    let mut cache = RenderCache::new();
    let _ = screen.render(true, &mut cache);
    let pt = screen.take_passthrough();
    assert!(pt.is_empty(), "OSC 777 must not re-appear after resize + full redraw");
}

#[test]
fn multiple_bells_all_forwarded() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x07\x07\x07");
    let pt = screen.take_passthrough();
    assert_eq!(pt.len(), 3, "three BELs should produce three passthrough entries");
    for entry in &pt {
        assert_eq!(entry, &vec![0x07u8]);
    }
}

#[test]
fn bell_and_osc_777_interleaved() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x07\x1b]777;notify;t;b\x07\x07");
    let pt = screen.take_passthrough();
    assert_eq!(pt.len(), 3, "BEL + OSC 777 + BEL = 3 passthrough entries");
    assert_eq!(pt[0], b"\x07", "first should be standalone BEL");
    assert_eq!(pt[1], b"\x1b]777;notify;t;b\x07", "second should be OSC 777");
    assert_eq!(pt[2], b"\x07", "third should be standalone BEL");
}

#[test]
fn bell_in_osc_is_terminator_not_separate_bell() {
    let mut screen = Screen::new(80, 24, 100);
    // The BEL inside OSC is a terminator, not a separate bell event
    screen.process(b"\x1b]777;notify;title;body\x07");
    let pt = screen.take_passthrough();
    assert_eq!(pt.len(), 1, "OSC terminated by BEL should produce exactly 1 passthrough");
    // The passthrough should be the full OSC, not a separate BEL
    assert!(pt[0].starts_with(b"\x1b]"), "passthrough should be the OSC sequence");
}

#[test]
fn bell_not_resent_on_render_with_scrollback() {
    let mut screen = Screen::new(10, 3, 100);
    // Generate some scrollback
    screen.process(b"A\r\nB\r\nC\r\nD");
    let scrollback = screen.take_pending_scrollback();
    // Now send a bell
    screen.process(b"\x07");
    let _ = screen.take_passthrough(); // drain
    // Render with scrollback (atomic scrollback injection + full redraw)
    let mut cache = RenderCache::new();
    let _ = screen.render_with_scrollback(&scrollback, &mut cache);
    let pt = screen.take_passthrough();
    assert!(pt.is_empty(), "BEL must not re-appear after render_with_scrollback");
}

#[test]
fn osc_777_not_resent_on_render_with_scrollback() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"A\r\nB\r\nC\r\nD");
    let scrollback = screen.take_pending_scrollback();
    screen.process(b"\x1b]777;notify;title;body\x07");
    let _ = screen.take_passthrough(); // drain
    let mut cache = RenderCache::new();
    let _ = screen.render_with_scrollback(&scrollback, &mut cache);
    let pt = screen.take_passthrough();
    assert!(pt.is_empty(), "OSC 777 must not re-appear after render_with_scrollback");
}

#[test]
fn mode_flags_bracketed_paste() {
    let mut screen = Screen::new(80, 24, 100);
    assert!(!screen.grid.modes.bracketed_paste);
    screen.process(b"\x1b[?2004h");
    assert!(screen.grid.modes.bracketed_paste);
    screen.process(b"\x1b[?2004l");
    assert!(!screen.grid.modes.bracketed_paste);
}

#[test]
fn mode_flags_cursor_key_mode() {
    let mut screen = Screen::new(80, 24, 100);
    assert!(!screen.grid.modes.cursor_key_mode);
    screen.process(b"\x1b[?1h");
    assert!(screen.grid.modes.cursor_key_mode);
    screen.process(b"\x1b[?1l");
    assert!(!screen.grid.modes.cursor_key_mode);
}

#[test]
fn mode_flags_mouse() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[?1000h");
    assert_eq!(screen.grid.modes.mouse_mode, 1000);
    screen.process(b"\x1b[?1006h");
    assert_eq!(screen.grid.modes.mouse_encoding, 1006);
    screen.process(b"\x1b[?1000l");
    assert_eq!(screen.grid.modes.mouse_mode, 0);
}

#[test]
fn keypad_app_mode() {
    let mut screen = Screen::new(80, 24, 100);
    assert!(!screen.grid.modes.keypad_app_mode);
    screen.process(b"\x1b=");
    assert!(screen.grid.modes.keypad_app_mode);
    screen.process(b"\x1b>");
    assert!(!screen.grid.modes.keypad_app_mode);
}

#[test]
fn cursor_shape_decscusr() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[2 q"); // steady block
    assert_eq!(screen.grid.modes.cursor_shape, grid::CursorShape::SteadyBlock);
    screen.process(b"\x1b[5 q"); // blinking bar
    assert_eq!(screen.grid.modes.cursor_shape, grid::CursorShape::BlinkBar);
    screen.process(b"\x1b[0 q"); // reset to default
    assert_eq!(screen.grid.modes.cursor_shape, grid::CursorShape::Default);
}

#[test]
fn autowrap_mode_disable_prevents_wrap() {
    let mut screen = Screen::new(5, 3, 100);
    screen.process(b"\x1b[?7l"); // disable autowrap
    screen.process(b"ABCDEF");   // write 6 chars in 5 cols
    // Should NOT wrap — last char overwrites column 4
    assert_eq!(screen.grid.cursor_y, 0);
    assert_eq!(screen.grid.cells[0][4].c, 'F');
    assert!(!screen.grid.wrap_pending);
}

#[test]
fn sgr_hidden_attribute() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[8m"); // hidden
    screen.process(b"secret");
    assert!(screen.grid.cells[0][0].style.hidden);
    screen.process(b"\x1b[28m"); // reveal
    screen.process(b"visible");
    assert!(!screen.grid.cells[0][6].style.hidden);
}

#[test]
fn cursor_save_restore() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[5;10H"); // move to row 5, col 10
    screen.process(b"\x1b7");       // save cursor
    screen.process(b"\x1b[1;1H");   // move home
    assert_eq!(screen.grid.cursor_y, 0);
    screen.process(b"\x1b8");       // restore cursor
    assert_eq!(screen.grid.cursor_y, 4);
    assert_eq!(screen.grid.cursor_x, 9);
}

#[test]
fn so_si_charset_switching() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b)0");  // set G1 to line drawing
    screen.process(b"\x0E");    // SO — activate G1
    screen.process(b"q");       // should be line drawing ─
    assert_eq!(screen.grid.cells[0][0].c, '\u{2500}');
    screen.process(b"\x0F");    // SI — activate G0 (ASCII)
    screen.process(b"q");
    assert_eq!(screen.grid.cells[0][1].c, 'q');
}

#[test]
fn cuu_cud_respects_scroll_region() {
    let mut screen = Screen::new(80, 24, 100);
    // Set scroll region to rows 5-15
    screen.process(b"\x1b[5;15r");
    // Cursor is at 0,0 after DECSTBM
    // Move into scroll region
    screen.process(b"\x1b[10;1H"); // row 10 (inside region)
    // Try moving up past scroll top
    screen.process(b"\x1b[20A");   // CUU 20 — should stop at row 5 (scroll_top=4)
    assert_eq!(screen.grid.cursor_y, 4); // 0-based row 4 = display row 5
    // Move back down past scroll bottom
    screen.process(b"\x1b[20B");   // CUD 20 — should stop at row 15 (scroll_bottom=14)
    assert_eq!(screen.grid.cursor_y, 14); // 0-based row 14 = display row 15
}

#[test]
fn vt_ff_treated_as_lf() {
    let mut screen = Screen::new(80, 3, 100);
    screen.process(b"A");
    screen.process(&[0x0B]); // VT
    assert_eq!(screen.grid.cursor_y, 1);
    screen.process(&[0x0C]); // FF
    assert_eq!(screen.grid.cursor_y, 2);
}

#[test]
fn dl_large_count_clamped() {
    let mut screen = Screen::new(10, 5, 100);
    screen.process(b"\x1b[2;1H"); // row 2 (1-indexed)
    screen.process(b"\x1b[99999M"); // DL with huge count
    assert_eq!(screen.grid.cells.len(), 5);
}

#[test]
fn il_large_count_clamped() {
    let mut screen = Screen::new(10, 5, 100);
    screen.process(b"\x1b[2;1H");
    screen.process(b"\x1b[99999L"); // IL with huge count
    assert_eq!(screen.grid.cells.len(), 5);
}

#[test]
fn alt_screen_mode_47_no_cursor_save() {
    let mut screen = Screen::new(10, 5, 100);
    // Move cursor to (3, 2) — row 3, col 4 (1-indexed)
    screen.process(b"\x1b[3;4H");
    assert_eq!(screen.grid.cursor_y, 2);
    assert_eq!(screen.grid.cursor_x, 3);
    // Save cursor explicitly with ESC 7
    screen.process(b"\x1b7");
    // Enter alt screen with mode 47 (should NOT save cursor again)
    screen.process(b"\x1b[?47h");
    // Move cursor on alt screen
    screen.process(b"\x1b[1;1H");
    assert_eq!(screen.grid.cursor_x, 0);
    assert_eq!(screen.grid.cursor_y, 0);
    // Exit alt screen with mode 47 (should NOT restore cursor)
    screen.process(b"\x1b[?47l");
    // Cursor should remain at (0, 0) since mode 47 doesn't restore
    assert_eq!(screen.grid.cursor_x, 0);
    assert_eq!(screen.grid.cursor_y, 0);
    // But ESC 8 should still restore the original saved cursor
    screen.process(b"\x1b8");
    assert_eq!(screen.grid.cursor_x, 3);
    assert_eq!(screen.grid.cursor_y, 2);
}

#[test]
fn mode_1048_save_restore_cursor() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[5;10H");   // move cursor
    screen.process(b"\x1b[?1048h");  // save cursor
    screen.process(b"\x1b[1;1H");    // move home
    screen.process(b"\x1b[?1048l");  // restore cursor
    assert_eq!(screen.grid.cursor_y, 4);
    assert_eq!(screen.grid.cursor_x, 9);
}

#[test]
fn dch_through_wide_char_no_orphan() {
    let mut screen = Screen::new(10, 3, 100);
    // Place: A [你] B — cells: A(w1) 你(w2) \0(w0) B(w1)
    screen.process(b"A");
    screen.process("你".as_bytes());
    screen.process(b"B");
    // Cursor at col 4. Move to col 1 (the wide char start) and delete 1
    screen.process(b"\x1b[1;2H"); // row 1, col 2 (0-based x=1)
    screen.process(b"\x1b[P");    // DCH 1
    // The continuation cell (width=0) should NOT remain at x=1
    assert_ne!(screen.grid.cells[0][1].width, 0,
        "orphaned continuation cell after DCH");
}

#[test]
fn ich_pushes_wide_char_off_right_edge() {
    let mut screen = Screen::new(6, 3, 100);
    // Place wide char at cols 4-5 (the last two columns)
    screen.process(b"\x1b[1;5H"); // row 1, col 5 (0-based x=4)
    screen.process("你".as_bytes());
    assert_eq!(screen.grid.cells[0][4].c, '你');
    assert_eq!(screen.grid.cells[0][4].width, 2);
    assert_eq!(screen.grid.cells[0][5].width, 0);
    // Move to col 1 and insert 1 char — pushes everything right,
    // the continuation cell falls off, orphaning width=2 at col 5
    screen.process(b"\x1b[1;1H");
    screen.process(b"\x1b[@");    // ICH 1
    // The rightmost cell should NOT be an orphaned width=2
    assert_ne!(screen.grid.cells[0][5].width, 2,
        "orphaned wide char at right edge after ICH");
}

#[test]
fn scrollback_captured_with_partial_scroll_region() {
    let mut screen = Screen::new(10, 5, 100);
    // Set scroll region to rows 1-3 (partial — not full screen)
    screen.process(b"\x1b[1;3r");
    // Move to row 1 and fill it, then scroll
    screen.process(b"\x1b[1;1H");
    screen.process(b"Line1\r\n");
    screen.process(b"Line2\r\n");
    screen.process(b"Line3\r\n"); // this should scroll within region
    let scrollback = screen.take_pending_scrollback();
    assert!(!scrollback.is_empty(),
        "scrollback should be captured even with partial scroll region (scroll_top==0)");
}

// ---------------------------------------------------------------
// Additional performer.rs coverage tests
// ---------------------------------------------------------------

#[test]
fn csi_s_u_save_restore_cursor() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[5;10H"); // move to row 5, col 10
    screen.process(b"\x1b[s");      // CSI s — save cursor
    screen.process(b"\x1b[1;1H");   // move home
    assert_eq!(screen.grid.cursor_y, 0);
    assert_eq!(screen.grid.cursor_x, 0);
    screen.process(b"\x1b[u");      // CSI u — restore cursor
    assert_eq!(screen.grid.cursor_y, 4);  // 0-based row 4
    assert_eq!(screen.grid.cursor_x, 9);  // 0-based col 9
}

#[test]
fn cursor_movement_cuf_cub() {
    let mut screen = Screen::new(80, 24, 100);
    // Start at home (0,0)
    screen.process(b"\x1b[5C");  // CUF 5 — forward 5
    assert_eq!(screen.grid.cursor_x, 5);
    screen.process(b"\x1b[2D");  // CUB 2 — backward 2
    assert_eq!(screen.grid.cursor_x, 3);
    // CUB should not go below 0
    screen.process(b"\x1b[100D");
    assert_eq!(screen.grid.cursor_x, 0);
    // CUF should clamp to cols-1
    screen.process(b"\x1b[200C");
    assert_eq!(screen.grid.cursor_x, 79);
}

#[test]
fn cursor_movement_cnl_cpl() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[10;15H"); // move to row 10, col 15
    assert_eq!(screen.grid.cursor_y, 9);
    assert_eq!(screen.grid.cursor_x, 14);

    // CNL 3 — move down 3 lines, cursor to column 0
    screen.process(b"\x1b[3E");
    assert_eq!(screen.grid.cursor_y, 12);
    assert_eq!(screen.grid.cursor_x, 0);

    // CPL 2 — move up 2 lines, cursor to column 0
    screen.process(b"\x1b[5;20H"); // reposition with a non-zero column
    screen.process(b"\x1b[2F");
    assert_eq!(screen.grid.cursor_y, 2);  // row 5 - 1 (0-based=4) minus 2 = 2
    assert_eq!(screen.grid.cursor_x, 0);

    // CNL should clamp to last row
    screen.process(b"\x1b[100E");
    assert_eq!(screen.grid.cursor_y, 23);
    assert_eq!(screen.grid.cursor_x, 0);

    // CPL should clamp to row 0
    screen.process(b"\x1b[100F");
    assert_eq!(screen.grid.cursor_y, 0);
    assert_eq!(screen.grid.cursor_x, 0);
}

#[test]
fn cursor_horizontal_absolute() {
    let mut screen = Screen::new(80, 24, 100);
    // CHA — CSI G sets cursor column (1-based)
    screen.process(b"\x1b[20G");
    assert_eq!(screen.grid.cursor_x, 19); // 0-based
    // CHA 1 should go to column 0
    screen.process(b"\x1b[1G");
    assert_eq!(screen.grid.cursor_x, 0);
    // CHA beyond cols should clamp
    screen.process(b"\x1b[200G");
    assert_eq!(screen.grid.cursor_x, 79);
    // CHA with default (no param) should go to column 0
    screen.process(b"\x1b[G");
    assert_eq!(screen.grid.cursor_x, 0);
}

#[test]
fn cursor_position_cup() {
    let mut screen = Screen::new(80, 24, 100);
    // CUP — CSI H sets row and column (1-based)
    screen.process(b"\x1b[12;40H");
    assert_eq!(screen.grid.cursor_y, 11); // 0-based
    assert_eq!(screen.grid.cursor_x, 39); // 0-based
    // CUP with no params goes to (0,0)
    screen.process(b"\x1b[H");
    assert_eq!(screen.grid.cursor_y, 0);
    assert_eq!(screen.grid.cursor_x, 0);
    // CUP should clamp to screen bounds
    screen.process(b"\x1b[100;200H");
    assert_eq!(screen.grid.cursor_y, 23);
    assert_eq!(screen.grid.cursor_x, 79);
}

#[test]
fn vpa_line_position_absolute() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[5;10H"); // start at row 5, col 10
    // VPA — CSI d sets cursor row (1-based), column unchanged
    screen.process(b"\x1b[15d");
    assert_eq!(screen.grid.cursor_y, 14); // 0-based row 14
    assert_eq!(screen.grid.cursor_x, 9);  // column unchanged
    // VPA should clamp to last row
    screen.process(b"\x1b[100d");
    assert_eq!(screen.grid.cursor_y, 23);
    // VPA with default goes to row 0
    screen.process(b"\x1b[d");
    assert_eq!(screen.grid.cursor_y, 0);
}

#[test]
fn erase_in_display_j0() {
    let mut screen = Screen::new(10, 5, 100);
    // Fill entire screen with 'X'
    for row in 0..5 {
        screen.process(format!("\x1b[{};1H", row + 1).as_bytes());
        screen.process(b"XXXXXXXXXX");
    }
    // Move cursor to row 3, col 5 (0-based: row 2, col 4)
    screen.process(b"\x1b[3;5H");
    // CSI 0J — erase from cursor to end of screen
    screen.process(b"\x1b[0J");
    // Cells before cursor on row 2 should be preserved
    assert_eq!(screen.grid.cells[2][0].c, 'X');
    assert_eq!(screen.grid.cells[2][3].c, 'X');
    // Cells from cursor onward on row 2 should be blank
    assert_eq!(screen.grid.cells[2][4].c, ' ');
    assert_eq!(screen.grid.cells[2][9].c, ' ');
    // All cells on rows below should be blank
    assert_eq!(screen.grid.cells[3][0].c, ' ');
    assert_eq!(screen.grid.cells[4][5].c, ' ');
    // Rows above should be preserved
    assert_eq!(screen.grid.cells[0][0].c, 'X');
    assert_eq!(screen.grid.cells[1][9].c, 'X');
}

#[test]
fn erase_in_display_j1() {
    let mut screen = Screen::new(10, 5, 100);
    // Fill entire screen with 'X'
    for row in 0..5 {
        screen.process(format!("\x1b[{};1H", row + 1).as_bytes());
        screen.process(b"XXXXXXXXXX");
    }
    // Move cursor to row 3, col 5 (0-based: row 2, col 4)
    screen.process(b"\x1b[3;5H");
    // CSI 1J — erase from start of screen to cursor
    screen.process(b"\x1b[1J");
    // All rows above cursor row should be blank
    assert_eq!(screen.grid.cells[0][0].c, ' ');
    assert_eq!(screen.grid.cells[1][9].c, ' ');
    // Cells on cursor row up to and including cursor should be blank
    assert_eq!(screen.grid.cells[2][0].c, ' ');
    assert_eq!(screen.grid.cells[2][4].c, ' ');
    // Cells after cursor on row 2 should be preserved
    assert_eq!(screen.grid.cells[2][5].c, 'X');
    assert_eq!(screen.grid.cells[2][9].c, 'X');
    // Rows below should be preserved
    assert_eq!(screen.grid.cells[3][0].c, 'X');
    assert_eq!(screen.grid.cells[4][5].c, 'X');
}

#[test]
fn erase_in_display_j2() {
    let mut screen = Screen::new(10, 5, 100);
    // Fill entire screen with 'X'
    for row in 0..5 {
        screen.process(format!("\x1b[{};1H", row + 1).as_bytes());
        screen.process(b"XXXXXXXXXX");
    }
    // Move cursor somewhere (should not matter for J2)
    screen.process(b"\x1b[3;5H");
    // CSI 2J — erase entire screen
    screen.process(b"\x1b[2J");
    // All cells should be blank
    for row in 0..5 {
        for col in 0..10 {
            assert_eq!(screen.grid.cells[row][col].c, ' ',
                "cell [{row}][{col}] should be blank after CSI 2J");
        }
    }
}

#[test]
fn erase_in_line_k0() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"ABCDEFGHIJ"); // fill row 0
    screen.process(b"\x1b[1;4H");  // move to row 1, col 4 (0-based col 3)
    // CSI 0K — erase from cursor to end of line
    screen.process(b"\x1b[0K");
    assert_eq!(screen.grid.cells[0][0].c, 'A');
    assert_eq!(screen.grid.cells[0][2].c, 'C');
    assert_eq!(screen.grid.cells[0][3].c, ' '); // erased
    assert_eq!(screen.grid.cells[0][9].c, ' '); // erased
}

#[test]
fn erase_in_line_k1() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"ABCDEFGHIJ"); // fill row 0
    screen.process(b"\x1b[1;4H");  // move to row 1, col 4 (0-based col 3)
    // CSI 1K — erase from start of line to cursor
    screen.process(b"\x1b[1K");
    assert_eq!(screen.grid.cells[0][0].c, ' '); // erased
    assert_eq!(screen.grid.cells[0][3].c, ' '); // erased (cursor position included)
    assert_eq!(screen.grid.cells[0][4].c, 'E'); // preserved
    assert_eq!(screen.grid.cells[0][9].c, 'J'); // preserved
}

#[test]
fn erase_in_line_k2() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"ABCDEFGHIJ"); // fill row 0
    screen.process(b"\x1b[1;4H");  // move to row 1, col 4 (0-based col 3)
    // CSI 2K — erase entire line
    screen.process(b"\x1b[2K");
    for col in 0..10 {
        assert_eq!(screen.grid.cells[0][col].c, ' ',
            "col {col} should be blank after CSI 2K");
    }
}

#[test]
fn erase_character_ech() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"ABCDEFGHIJ"); // fill row 0
    screen.process(b"\x1b[1;3H");  // move to col 3 (0-based col 2)
    // CSI 4X — erase 4 chars starting at cursor, without moving cursor
    screen.process(b"\x1b[4X");
    assert_eq!(screen.grid.cells[0][0].c, 'A');
    assert_eq!(screen.grid.cells[0][1].c, 'B');
    assert_eq!(screen.grid.cells[0][2].c, ' '); // erased
    assert_eq!(screen.grid.cells[0][3].c, ' '); // erased
    assert_eq!(screen.grid.cells[0][4].c, ' '); // erased
    assert_eq!(screen.grid.cells[0][5].c, ' '); // erased
    assert_eq!(screen.grid.cells[0][6].c, 'G'); // preserved
    assert_eq!(screen.grid.cells[0][9].c, 'J'); // preserved
    // Cursor should not have moved
    assert_eq!(screen.grid.cursor_x, 2);
}

#[test]
fn delete_character_dch() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"ABCDEFGHIJ"); // fill row 0
    screen.process(b"\x1b[1;3H");  // move to col 3 (0-based col 2)
    // CSI 2P — delete 2 chars at cursor, shifting left
    screen.process(b"\x1b[2P");
    // 'C' and 'D' are deleted; E-J shift left, blanks fill right
    assert_eq!(screen.grid.cells[0][0].c, 'A');
    assert_eq!(screen.grid.cells[0][1].c, 'B');
    assert_eq!(screen.grid.cells[0][2].c, 'E');
    assert_eq!(screen.grid.cells[0][3].c, 'F');
    assert_eq!(screen.grid.cells[0][7].c, 'J');
    assert_eq!(screen.grid.cells[0][8].c, ' '); // blank fill
    assert_eq!(screen.grid.cells[0][9].c, ' '); // blank fill
}

#[test]
fn insert_character_ich() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"ABCDEFGHIJ"); // fill row 0
    screen.process(b"\x1b[1;3H");  // move to col 3 (0-based col 2)
    // CSI 2@ — insert 2 blank chars at cursor, shifting right
    screen.process(b"\x1b[2@");
    assert_eq!(screen.grid.cells[0][0].c, 'A');
    assert_eq!(screen.grid.cells[0][1].c, 'B');
    assert_eq!(screen.grid.cells[0][2].c, ' '); // inserted blank
    assert_eq!(screen.grid.cells[0][3].c, ' '); // inserted blank
    assert_eq!(screen.grid.cells[0][4].c, 'C'); // shifted right
    assert_eq!(screen.grid.cells[0][5].c, 'D'); // shifted right
    // 'I' and 'J' fall off the right edge
    assert_eq!(screen.grid.cells[0][9].c, 'H');
}

#[test]
fn scroll_up_su() {
    let mut screen = Screen::new(10, 5, 100);
    // Place identifiable content on each row
    for row in 0..5 {
        screen.process(format!("\x1b[{};1H", row + 1).as_bytes());
        screen.process(format!("Row{}", row).as_bytes());
    }
    // CSI 2S — scroll up 2 lines
    screen.process(b"\x1b[2S");
    // Row 0 should now show what was row 2
    assert_eq!(screen.grid.cells[0][0].c, 'R');
    assert_eq!(screen.grid.cells[0][3].c, '2');
    // Last two rows should be blank
    assert_eq!(screen.grid.cells[3][0].c, ' ');
    assert_eq!(screen.grid.cells[4][0].c, ' ');
}

#[test]
fn scroll_down_sd() {
    let mut screen = Screen::new(10, 5, 100);
    // Place identifiable content on each row
    for row in 0..5 {
        screen.process(format!("\x1b[{};1H", row + 1).as_bytes());
        screen.process(format!("Row{}", row).as_bytes());
    }
    // CSI 2T — scroll down 2 lines
    screen.process(b"\x1b[2T");
    // First two rows should be blank
    assert_eq!(screen.grid.cells[0][0].c, ' ');
    assert_eq!(screen.grid.cells[1][0].c, ' ');
    // Row 2 should now show what was row 0
    assert_eq!(screen.grid.cells[2][0].c, 'R');
    assert_eq!(screen.grid.cells[2][3].c, '0');
}

#[test]
fn delete_lines_dl() {
    let mut screen = Screen::new(10, 5, 100);
    // Fill rows with identifiable content
    for row in 0..5 {
        screen.process(format!("\x1b[{};1H", row + 1).as_bytes());
        screen.process(format!("Line{}", row).as_bytes());
    }
    // Move cursor to row 2 (0-based row 1)
    screen.process(b"\x1b[2;1H");
    // CSI 2M — delete 2 lines at cursor
    screen.process(b"\x1b[2M");
    // Row 1 should now be what was row 3 ("Line3")
    assert_eq!(screen.grid.cells[1][4].c, '3');
    // Row 2 should now be what was row 4 ("Line4")
    assert_eq!(screen.grid.cells[2][4].c, '4');
    // Bottom rows should be blank
    assert_eq!(screen.grid.cells[3][0].c, ' ');
    assert_eq!(screen.grid.cells[4][0].c, ' ');
}

#[test]
fn insert_lines_il() {
    let mut screen = Screen::new(10, 5, 100);
    // Fill rows with identifiable content
    for row in 0..5 {
        screen.process(format!("\x1b[{};1H", row + 1).as_bytes());
        screen.process(format!("Line{}", row).as_bytes());
    }
    // Move cursor to row 2 (0-based row 1)
    screen.process(b"\x1b[2;1H");
    // CSI 2L — insert 2 blank lines at cursor
    screen.process(b"\x1b[2L");
    // Row 0 should still be "Line0"
    assert_eq!(screen.grid.cells[0][4].c, '0');
    // Rows 1 and 2 should be blank (inserted)
    assert_eq!(screen.grid.cells[1][0].c, ' ');
    assert_eq!(screen.grid.cells[2][0].c, ' ');
    // Row 3 should be what was row 1 ("Line1")
    assert_eq!(screen.grid.cells[3][4].c, '1');
    // "Line3" and "Line4" have been pushed off the bottom
}

#[test]
fn decstbm_set_scroll_region() {
    let mut screen = Screen::new(80, 24, 100);
    // Move cursor to a non-home position first
    screen.process(b"\x1b[10;20H");
    assert_eq!(screen.grid.cursor_y, 9);
    assert_eq!(screen.grid.cursor_x, 19);
    // CSI 5;15r — set scroll region to rows 5-15
    screen.process(b"\x1b[5;15r");
    // Scroll region should be set (0-based)
    assert_eq!(screen.grid.scroll_top, 4);
    assert_eq!(screen.grid.scroll_bottom, 14);
    // Cursor should move to 0,0
    assert_eq!(screen.grid.cursor_x, 0);
    assert_eq!(screen.grid.cursor_y, 0);
    // wrap_pending should be cleared
    assert!(!screen.grid.wrap_pending);
}

#[test]
fn reverse_index_ri() {
    let mut screen = Screen::new(10, 5, 100);
    // Set scroll region to rows 2-4 (0-based: 1-3)
    screen.process(b"\x1b[2;4r");
    // Place content in scroll region
    screen.process(b"\x1b[2;1H");
    screen.process(b"LineA");
    screen.process(b"\x1b[3;1H");
    screen.process(b"LineB");
    screen.process(b"\x1b[4;1H");
    screen.process(b"LineC");
    // Move to top of scroll region (row 2, 0-based row 1)
    screen.process(b"\x1b[2;1H");
    assert_eq!(screen.grid.cursor_y, 1);
    // ESC M — reverse index at top of scroll region should scroll down
    screen.process(b"\x1bM");
    // Cursor stays at scroll_top
    assert_eq!(screen.grid.cursor_y, 1);
    // Row 1 should now be blank (new line scrolled in)
    assert_eq!(screen.grid.cells[1][0].c, ' ');
    // Row 2 should now be "LineA" (shifted down)
    assert_eq!(screen.grid.cells[2][0].c, 'L');
    assert_eq!(screen.grid.cells[2][4].c, 'A');
}

#[test]
fn reverse_index_ri_not_at_top() {
    // When cursor is NOT at the scroll_top, RI just moves cursor up one line
    let mut screen = Screen::new(10, 5, 100);
    screen.process(b"\x1b[3;1H"); // row 3, col 1 (0-based row 2)
    screen.process(b"\x1bM");      // ESC M
    assert_eq!(screen.grid.cursor_y, 1); // moved up one
}

#[test]
fn focus_reporting_mode() {
    let mut screen = Screen::new(80, 24, 100);
    assert!(!screen.grid.modes.focus_reporting);
    // CSI ?1004h — enable focus reporting
    screen.process(b"\x1b[?1004h");
    assert!(screen.grid.modes.focus_reporting);
    // CSI ?1004l — disable focus reporting
    screen.process(b"\x1b[?1004l");
    assert!(!screen.grid.modes.focus_reporting);
}

#[test]
fn autowrap_mode_re_enable() {
    let mut screen = Screen::new(5, 3, 100);
    // Disable autowrap
    screen.process(b"\x1b[?7l");
    assert!(!screen.grid.modes.autowrap_mode);
    // Write past end of line — should NOT wrap
    screen.process(b"ABCDEF");
    assert_eq!(screen.grid.cursor_y, 0);
    assert_eq!(screen.grid.cells[0][4].c, 'F');

    // Re-enable autowrap
    screen.process(b"\x1b[?7h");
    assert!(screen.grid.modes.autowrap_mode);
    // Go back to start, fill line, and verify wrap now works
    screen.process(b"\x1b[1;1H");
    screen.process(b"12345");
    assert!(screen.grid.wrap_pending);
    screen.process(b"6");
    assert_eq!(screen.grid.cursor_y, 1);
    assert_eq!(screen.grid.cells[1][0].c, '6');
}

#[test]
fn bce_erase_uses_bg_color() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"ABCDEFGHIJ"); // fill row 0
    // Set background color to red (SGR 41)
    screen.process(b"\x1b[41m");
    // Move to col 3 and erase to end of line
    screen.process(b"\x1b[1;4H");
    screen.process(b"\x1b[0K");
    // Erased cells should have the red background
    let expected_bg = Some(style::Color::Indexed(1)); // red = index 1
    assert_eq!(screen.grid.cells[0][3].style.bg, expected_bg,
        "erased cell at col 3 should have red background (BCE)");
    assert_eq!(screen.grid.cells[0][9].style.bg, expected_bg,
        "erased cell at col 9 should have red background (BCE)");
    // Cells before cursor should NOT have the red bg (they were written before SGR 41)
    assert_eq!(screen.grid.cells[0][0].style.bg, None,
        "cell at col 0 should have default background");

    // Also verify BCE with CSI 2J (erase entire display)
    screen.process(b"\x1b[2J");
    assert_eq!(screen.grid.cells[1][5].style.bg, expected_bg,
        "CSI 2J erased cell should have red background (BCE)");

    // And ECH (erase character)
    screen.process(b"\x1b[1;1H");
    screen.process(b"XYZ");
    screen.process(b"\x1b[1;1H");
    screen.process(b"\x1b[2X"); // erase 2 chars
    assert_eq!(screen.grid.cells[0][0].style.bg, expected_bg,
        "ECH erased cell should have red background (BCE)");
    assert_eq!(screen.grid.cells[0][1].style.bg, expected_bg,
        "ECH erased cell at col 1 should have red background (BCE)");
}

// ---------------------------------------------------------------
// Additional coverage tests
// ---------------------------------------------------------------

#[test]
fn tab_advances_to_next_tab_stop() {
    let mut screen = Screen::new(80, 3, 100);
    screen.process(b"AB"); // cursor at col 2
    screen.process(b"\t"); // tab should advance to col 8
    assert_eq!(screen.grid.cursor_x, 8);
    screen.process(b"\t"); // next tab stop at col 16
    assert_eq!(screen.grid.cursor_x, 16);
}

#[test]
fn tab_at_end_of_line_clamps() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"ABCDEFGH"); // cursor at col 8
    screen.process(b"\t"); // tab should clamp to col 9 (cols-1)
    assert_eq!(screen.grid.cursor_x, 9);
}

#[test]
fn backspace_at_column_zero() {
    let mut screen = Screen::new(80, 3, 100);
    assert_eq!(screen.grid.cursor_x, 0);
    screen.process(b"\x08"); // BS at col 0
    assert_eq!(screen.grid.cursor_x, 0, "BS at column 0 should stay at 0");
}

#[test]
fn backspace_clears_wrap_pending() {
    let mut screen = Screen::new(5, 3, 100);
    screen.process(b"ABCDE"); // wrap_pending = true
    assert!(screen.grid.wrap_pending);
    screen.process(b"\x08"); // BS
    assert!(!screen.grid.wrap_pending, "BS should clear wrap_pending");
    assert_eq!(screen.grid.cursor_x, 3);
}

#[test]
fn erase_scrollback_j3() {
    let mut screen = Screen::new(10, 3, 100);
    // Generate scrollback
    screen.process(b"Line1\r\nLine2\r\nLine3\r\nLine4\r\nLine5");
    let history = screen.get_history();
    assert!(!history.is_empty(), "should have scrollback before J3");

    // CSI 3J — erase scrollback
    screen.process(b"\x1b[3J");
    let history_after = screen.get_history();
    assert!(history_after.is_empty(),
        "CSI 3J should clear all scrollback, got {} lines", history_after.len());
}

#[test]
fn alt_screen_clears_wrap_pending() {
    let mut screen = Screen::new(5, 3, 100);
    screen.process(b"ABCDE"); // fills line, wrap_pending = true
    assert!(screen.grid.wrap_pending);

    // Enter alt screen
    screen.process(b"\x1b[?1049h");
    assert!(!screen.grid.wrap_pending,
        "wrap_pending should be cleared on alt screen enter");
    assert_eq!(screen.grid.cursor_x, 0);
    assert_eq!(screen.grid.cursor_y, 0);
}

#[test]
fn alt_screen_mode_1047() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"Hello");
    assert_eq!(screen.grid.cells[0][0].c, 'H');

    // Enter alt screen via mode 1047
    screen.process(b"\x1b[?1047h");
    assert!(screen.state.in_alt_screen);
    assert_eq!(screen.grid.cells[0][0].c, ' '); // alt screen cleared

    screen.process(b"Alt");
    assert_eq!(screen.grid.cells[0][0].c, 'A');

    // Leave alt screen
    screen.process(b"\x1b[?1047l");
    assert!(!screen.state.in_alt_screen);
    assert_eq!(screen.grid.cells[0][0].c, 'H'); // main buffer restored
}

#[test]
fn alt_screen_mode_47() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"Main");
    assert_eq!(screen.grid.cells[0][0].c, 'M');

    screen.process(b"\x1b[?47h");
    assert!(screen.state.in_alt_screen);
    assert_eq!(screen.grid.cells[0][0].c, ' ');

    screen.process(b"\x1b[?47l");
    assert!(!screen.state.in_alt_screen);
    assert_eq!(screen.grid.cells[0][0].c, 'M');
}

#[test]
fn alt_screen_restores_modes() {
    let mut screen = Screen::new(10, 3, 100);
    // Set some modes on main screen
    screen.process(b"\x1b[?2004h"); // bracketed paste
    screen.process(b"\x1b[?1h");    // cursor key mode
    assert!(screen.grid.modes.bracketed_paste);
    assert!(screen.grid.modes.cursor_key_mode);

    // Enter alt screen
    screen.process(b"\x1b[?1049h");
    // Modes should still be there (saved, but current grid is alt)
    // Now change modes on alt screen
    screen.process(b"\x1b[?2004l");
    screen.process(b"\x1b[?1l");
    assert!(!screen.grid.modes.bracketed_paste);
    assert!(!screen.grid.modes.cursor_key_mode);

    // Leave alt screen — modes should be restored
    screen.process(b"\x1b[?1049l");
    assert!(screen.grid.modes.bracketed_paste,
        "bracketed paste should be restored on alt screen exit");
    assert!(screen.grid.modes.cursor_key_mode,
        "cursor key mode should be restored on alt screen exit");
}

#[test]
fn cursor_visibility_mode_25() {
    let mut screen = Screen::new(80, 24, 100);
    assert!(screen.grid.cursor_visible);
    screen.process(b"\x1b[?25l");
    assert!(!screen.grid.cursor_visible, "cursor should be hidden after ?25l");
    screen.process(b"\x1b[?25h");
    assert!(screen.grid.cursor_visible, "cursor should be visible after ?25h");
}

#[test]
fn render_with_hidden_cursor() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"\x1b[?25l"); // hide cursor
    let mut cache = RenderCache::new();
    let result = screen.render(false, &mut cache);
    let text = String::from_utf8_lossy(&result);
    // Should NOT contain ?25h (cursor show) since cursor is hidden
    assert!(!text.contains("\x1b[?25h"),
        "hidden cursor should not emit ?25h in render output");
    // Should contain ?25l (cursor hide for redraw)
    assert!(text.contains("\x1b[?25l"),
        "render should always hide cursor during redraw");
}

#[test]
fn render_full_reattach_redraws_all() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"Hello");
    let mut cache = RenderCache::new();
    // First render
    let _ = screen.render(false, &mut cache);

    // Simulate reattach: full render with existing cache
    let result = screen.render(true, &mut cache);
    let text = String::from_utf8_lossy(&result);
    assert!(text.contains("\x1b[2J\x1b[H"),
        "full render should clear screen");
    assert!(text.contains("Hello"),
        "full render should include screen content");
}

#[test]
fn pending_scrollback_drained_separately() {
    let mut screen = Screen::new(10, 3, 100);
    // Cause scrollback
    screen.process(b"A\r\nB\r\nC\r\nD");
    let pending = screen.take_pending_scrollback();
    assert!(!pending.is_empty(), "should have pending scrollback");

    // Second drain should be empty
    let pending2 = screen.take_pending_scrollback();
    assert!(pending2.is_empty(), "second drain should be empty");

    // History should still contain everything
    let history = screen.get_history();
    assert!(!history.is_empty(), "history should be preserved after drain");
}

#[test]
fn stale_pending_scrollback_after_reattach_simulation() {
    // Simulates: client1 processes data, disconnects mid-scroll,
    // client2 connects and shouldn't see duplicate scrollback
    let mut screen = Screen::new(10, 3, 100);

    // Simulate first client processing output (causes scrollback)
    screen.process(b"Line1\r\nLine2\r\nLine3\r\nLine4");
    // Client1 takes pending scrollback (normal operation)
    let _ = screen.take_pending_scrollback();

    // More output causes more scrollback
    screen.process(b"\r\nLine5\r\nLine6");
    // Client1 disconnects WITHOUT draining pending scrollback

    // Simulate reattach: get history (what would be sent as History msg)
    let history = screen.get_history();
    let history_count = history.len();

    // Drain stale pending scrollback (the fix in session_bridge.rs)
    let stale = screen.take_pending_scrollback();
    assert!(!stale.is_empty(),
        "there should be stale pending scrollback from the disconnect");

    // Now new PTY output arrives
    screen.process(b"\r\nLine7");
    let new_pending = screen.take_pending_scrollback();

    // New pending should only contain Line7's scroll, not duplicates
    let new_history = screen.get_history();
    assert_eq!(new_history.len(), history_count + new_pending.len(),
        "new scrollback should only contain lines added after reattach drain");
}

#[test]
fn window_ops_ignored() {
    let mut screen = Screen::new(80, 24, 100);
    // CSI t (window ops) should be silently ignored
    screen.process(b"\x1b[14t"); // report window size
    screen.process(b"\x1b[22;0t"); // push title
    // Should not crash, no responses generated
    let responses = screen.take_responses();
    assert!(responses.is_empty(), "window ops should not generate responses");
}

#[test]
fn scroll_region_il_dl_interaction() {
    let mut screen = Screen::new(10, 6, 100);
    // Set scroll region to rows 2-5
    screen.process(b"\x1b[2;5r");
    // Fill all rows
    for row in 0..6 {
        screen.process(format!("\x1b[{};1H", row + 1).as_bytes());
        screen.process(format!("R{}", row).as_bytes());
    }
    // Move into scroll region and insert a line
    screen.process(b"\x1b[3;1H"); // row 3 (inside region)
    screen.process(b"\x1b[L");    // IL 1

    // Row 2 (0-indexed) should be blank (inserted)
    assert_eq!(screen.grid.cells[2][0].c, ' ',
        "inserted line should be blank");
    // Row 1 (above region) should be untouched
    assert_eq!(screen.grid.cells[0][0].c, 'R',
        "row above scroll region should be untouched");
    // Row 5 (below region bottom) should be untouched
    assert_eq!(screen.grid.cells[5][0].c, 'R',
        "row below scroll region should be untouched");
}

// --- New integration tests ---

#[test]
fn render_bce_erase_output() {
    // Rendered ANSI should include background color after BCE erase
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"ABCDEFGHIJ"); // fill row 0
    screen.process(b"\x1b[41m");   // set bg red
    screen.process(b"\x1b[1;4H");  // move to col 3 (1-indexed col 4)
    screen.process(b"\x1b[0K");    // erase to end of line

    let mut cache = RenderCache::new();
    let result = screen.render(true, &mut cache);
    let text = String::from_utf8_lossy(&result);
    // The rendered output should include the red bg SGR (code 41)
    // for the erased cells
    assert!(text.contains("41"), "rendered output should include red bg (41) after BCE erase");
}

#[test]
fn wide_char_scrollback_rendering() {
    // Wide char in scrollback line should render correctly
    let mut screen = Screen::new(10, 3, 100);
    // Write a wide char on row 0
    screen.process("\u{4e16}\u{754c}".as_bytes()); // 世界
    // Scroll it into scrollback
    screen.process(b"\r\nLine2\r\nLine3\r\nLine4");

    let history = screen.get_history();
    assert!(!history.is_empty(), "should have scrollback");
    // The first scrollback line should contain the wide chars rendered as ANSI
    let first_line = String::from_utf8_lossy(&history[0]);
    assert!(first_line.contains('\u{4e16}'), "scrollback should contain wide char 世");
    assert!(first_line.contains('\u{754c}'), "scrollback should contain wide char 界");
}

#[test]
fn combining_mark_attaches_to_previous_cell() {
    let mut screen = Screen::new(80, 24, 100);
    // Print 'e' followed by combining acute accent U+0301
    screen.process("e\u{0301}".as_bytes());
    assert_eq!(screen.grid.cells[0][0].c, 'e');
    assert_eq!(screen.grid.cells[0][0].combining, vec!['\u{0301}']);
}

#[test]
fn combining_mark_with_wrap_pending() {
    let mut screen = Screen::new(5, 3, 100);
    // Fill the line to trigger wrap_pending
    screen.process(b"ABCDE");
    assert!(screen.grid.wrap_pending, "wrap should be pending after filling line");
    // Now send a combining mark — it should attach to the last cell (E)
    screen.process("\u{0308}".as_bytes()); // combining diaeresis
    assert_eq!(screen.grid.cells[0][4].c, 'E');
    assert_eq!(screen.grid.cells[0][4].combining, vec!['\u{0308}']);
}

#[test]
fn combining_mark_on_wide_char() {
    let mut screen = Screen::new(80, 24, 100);
    // Print a wide char followed by a combining mark
    screen.process("\u{4e16}\u{0301}".as_bytes()); // 世 + combining acute
    // The combining mark should attach to the wide char cell (col 0), not the continuation (col 1)
    assert_eq!(screen.grid.cells[0][0].c, '\u{4e16}');
    assert_eq!(screen.grid.cells[0][0].combining, vec!['\u{0301}']);
    assert_eq!(screen.grid.cells[0][1].width, 0); // continuation cell
}

#[test]
fn combining_mark_renders_in_output() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process("e\u{0301}".as_bytes());
    let mut cache = RenderCache::new();
    let output = screen.render(true, &mut cache);
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("e\u{0301}"), "rendered output should contain base char + combining mark");
}

#[test]
fn delete_lines_preserves_cursor_x() {
    let mut screen = Screen::new(80, 24, 100);
    // Position cursor at column 10, row 5
    screen.process(b"\x1b[6;11H");  // CUP row=6, col=11 (1-indexed)
    assert_eq!(screen.grid.cursor_x, 10);
    assert_eq!(screen.grid.cursor_y, 5);
    // Delete 1 line
    screen.process(b"\x1b[M");
    // cursor_x must be preserved per ECMA-48
    assert_eq!(screen.grid.cursor_x, 10, "DL must not change cursor_x");
    assert_eq!(screen.grid.cursor_y, 5, "DL must not change cursor_y");
}

#[test]
fn insert_lines_preserves_cursor_x() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[6;11H");
    assert_eq!(screen.grid.cursor_x, 10);
    // Insert 1 line
    screen.process(b"\x1b[L");
    assert_eq!(screen.grid.cursor_x, 10, "IL must not change cursor_x");
    assert_eq!(screen.grid.cursor_y, 5, "IL must not change cursor_y");
}
