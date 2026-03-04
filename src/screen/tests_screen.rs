use super::*;

#[test]
fn alt_screen_save_restore() {
    let mut screen = Screen::new(10, 3, 100);

    // Write "Hello" on the main screen
    screen.process(b"Hello");
    assert_eq!(screen.grid.visible_row(0)[0].c, 'H');
    assert_eq!(screen.grid.visible_row(0)[4].c, 'o');

    // Enter alt screen (CSI ?1049h)
    screen.process(b"\x1b[?1049h");
    assert!(screen.state.in_alt_screen);
    // Alt screen should be cleared
    assert_eq!(screen.grid.visible_row(0)[0].c, ' ');

    // Write something on alt screen
    screen.process(b"Alt");
    assert_eq!(screen.grid.visible_row(0)[0].c, 'A');

    // Leave alt screen (CSI ?1049l) — should restore main buffer (fix S7)
    screen.process(b"\x1b[?1049l");
    assert!(!screen.state.in_alt_screen);
    assert_eq!(screen.grid.visible_row(0)[0].c, 'H');
    assert_eq!(screen.grid.visible_row(0)[4].c, 'o');
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
    assert_eq!(screen.grid.visible_row(0)[0].c, ' ');
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
    assert_eq!(screen.grid.visible_row(1)[0].c, 'F');
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
    assert_eq!(screen.grid.visible_row(0)[0].c, '\u{250C}'); // ┌
    assert_eq!(screen.grid.visible_row(0)[1].c, '\u{2500}'); // ─
    assert_eq!(screen.grid.visible_row(0)[2].c, '\u{2510}'); // ┐
    // Switch back to ASCII
    screen.process(b"\x1b(B");
    screen.process(b"l");
    assert_eq!(screen.grid.visible_row(0)[3].c, 'l'); // plain ASCII 'l'
}

#[test]
fn rep_repeats_last_char() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"A\x1b[3b"); // print A, then repeat 3 times
    assert_eq!(screen.grid.visible_row(0)[0].c, 'A');
    assert_eq!(screen.grid.visible_row(0)[1].c, 'A');
    assert_eq!(screen.grid.visible_row(0)[2].c, 'A');
    assert_eq!(screen.grid.visible_row(0)[3].c, 'A');
}

#[test]
fn wide_character_occupies_two_cells() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process("你".as_bytes());
    assert_eq!(screen.grid.visible_row(0)[0].c, '你');
    assert_eq!(screen.grid.visible_row(0)[0].width, 2);
    assert_eq!(screen.grid.visible_row(0)[1].width, 0);
    assert_eq!(screen.grid.cursor_x, 2);
}

#[test]
fn wide_char_wraps_at_end_of_line() {
    let mut screen = Screen::new(5, 3, 100);
    screen.process(b"ABCD"); // fill 4 of 5 cols
    screen.process("你".as_bytes()); // needs 2 cols, only 1 left -> should wrap
    // Col 4 should be blanked, wide char on next line
    assert_eq!(screen.grid.visible_row(0)[4].c, ' ');
    assert_eq!(screen.grid.visible_row(1)[0].c, '你');
    assert_eq!(screen.grid.visible_row(1)[0].width, 2);
    assert_eq!(screen.grid.visible_row(1)[1].width, 0);
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
    assert_eq!(screen.grid.visible_row(0)[0].c, ' ');
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
    assert_eq!(screen.grid.visible_row(0)[0].c, 'H', "BEL should not alter cell content");
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
    assert!(screen.grid.modes.mouse_modes.click);
    assert_eq!(screen.grid.modes.mouse_modes.effective(), super::grid::MouseMode::Click);
    screen.process(b"\x1b[?1006h");
    assert_eq!(screen.grid.modes.mouse_encoding, super::grid::MouseEncoding::Sgr);
    screen.process(b"\x1b[?1000l");
    assert!(!screen.grid.modes.mouse_modes.click);
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
    assert_eq!(screen.grid.visible_row(0)[4].c, 'F');
    assert!(!screen.grid.wrap_pending);
}

#[test]
fn sgr_hidden_attribute() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[8m"); // hidden
    screen.process(b"secret");
    assert!(screen.grid.visible_row(0)[0].style.hidden);
    screen.process(b"\x1b[28m"); // reveal
    screen.process(b"visible");
    assert!(!screen.grid.visible_row(0)[6].style.hidden);
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
    assert_eq!(screen.grid.visible_row(0)[0].c, '\u{2500}');
    screen.process(b"\x0F");    // SI — activate G0 (ASCII)
    screen.process(b"q");
    assert_eq!(screen.grid.visible_row(0)[1].c, 'q');
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
    assert_eq!(screen.grid.visible_row_count(), 5);
}

#[test]
fn il_large_count_clamped() {
    let mut screen = Screen::new(10, 5, 100);
    screen.process(b"\x1b[2;1H");
    screen.process(b"\x1b[99999L"); // IL with huge count
    assert_eq!(screen.grid.visible_row_count(), 5);
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
    assert_ne!(screen.grid.visible_row(0)[1].width, 0,
        "orphaned continuation cell after DCH");
}

#[test]
fn ich_pushes_wide_char_off_right_edge() {
    let mut screen = Screen::new(6, 3, 100);
    // Place wide char at cols 4-5 (the last two columns)
    screen.process(b"\x1b[1;5H"); // row 1, col 5 (0-based x=4)
    screen.process("你".as_bytes());
    assert_eq!(screen.grid.visible_row(0)[4].c, '你');
    assert_eq!(screen.grid.visible_row(0)[4].width, 2);
    assert_eq!(screen.grid.visible_row(0)[5].width, 0);
    // Move to col 1 and insert 1 char — pushes everything right,
    // the continuation cell falls off, orphaning width=2 at col 5
    screen.process(b"\x1b[1;1H");
    screen.process(b"\x1b[@");    // ICH 1
    // The rightmost cell should NOT be an orphaned width=2
    assert_ne!(screen.grid.visible_row(0)[5].width, 2,
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
    assert_eq!(screen.grid.visible_row(2)[0].c, 'X');
    assert_eq!(screen.grid.visible_row(2)[3].c, 'X');
    // Cells from cursor onward on row 2 should be blank
    assert_eq!(screen.grid.visible_row(2)[4].c, ' ');
    assert_eq!(screen.grid.visible_row(2)[9].c, ' ');
    // All cells on rows below should be blank
    assert_eq!(screen.grid.visible_row(3)[0].c, ' ');
    assert_eq!(screen.grid.visible_row(4)[5].c, ' ');
    // Rows above should be preserved
    assert_eq!(screen.grid.visible_row(0)[0].c, 'X');
    assert_eq!(screen.grid.visible_row(1)[9].c, 'X');
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
    assert_eq!(screen.grid.visible_row(0)[0].c, ' ');
    assert_eq!(screen.grid.visible_row(1)[9].c, ' ');
    // Cells on cursor row up to and including cursor should be blank
    assert_eq!(screen.grid.visible_row(2)[0].c, ' ');
    assert_eq!(screen.grid.visible_row(2)[4].c, ' ');
    // Cells after cursor on row 2 should be preserved
    assert_eq!(screen.grid.visible_row(2)[5].c, 'X');
    assert_eq!(screen.grid.visible_row(2)[9].c, 'X');
    // Rows below should be preserved
    assert_eq!(screen.grid.visible_row(3)[0].c, 'X');
    assert_eq!(screen.grid.visible_row(4)[5].c, 'X');
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
            assert_eq!(screen.grid.visible_row(row)[col].c, ' ',
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
    assert_eq!(screen.grid.visible_row(0)[0].c, 'A');
    assert_eq!(screen.grid.visible_row(0)[2].c, 'C');
    assert_eq!(screen.grid.visible_row(0)[3].c, ' '); // erased
    assert_eq!(screen.grid.visible_row(0)[9].c, ' '); // erased
}

#[test]
fn erase_in_line_k1() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"ABCDEFGHIJ"); // fill row 0
    screen.process(b"\x1b[1;4H");  // move to row 1, col 4 (0-based col 3)
    // CSI 1K — erase from start of line to cursor
    screen.process(b"\x1b[1K");
    assert_eq!(screen.grid.visible_row(0)[0].c, ' '); // erased
    assert_eq!(screen.grid.visible_row(0)[3].c, ' '); // erased (cursor position included)
    assert_eq!(screen.grid.visible_row(0)[4].c, 'E'); // preserved
    assert_eq!(screen.grid.visible_row(0)[9].c, 'J'); // preserved
}

#[test]
fn erase_in_line_k2() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"ABCDEFGHIJ"); // fill row 0
    screen.process(b"\x1b[1;4H");  // move to row 1, col 4 (0-based col 3)
    // CSI 2K — erase entire line
    screen.process(b"\x1b[2K");
    for col in 0..10 {
        assert_eq!(screen.grid.visible_row(0)[col].c, ' ',
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
    assert_eq!(screen.grid.visible_row(0)[0].c, 'A');
    assert_eq!(screen.grid.visible_row(0)[1].c, 'B');
    assert_eq!(screen.grid.visible_row(0)[2].c, ' '); // erased
    assert_eq!(screen.grid.visible_row(0)[3].c, ' '); // erased
    assert_eq!(screen.grid.visible_row(0)[4].c, ' '); // erased
    assert_eq!(screen.grid.visible_row(0)[5].c, ' '); // erased
    assert_eq!(screen.grid.visible_row(0)[6].c, 'G'); // preserved
    assert_eq!(screen.grid.visible_row(0)[9].c, 'J'); // preserved
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
    assert_eq!(screen.grid.visible_row(0)[0].c, 'A');
    assert_eq!(screen.grid.visible_row(0)[1].c, 'B');
    assert_eq!(screen.grid.visible_row(0)[2].c, 'E');
    assert_eq!(screen.grid.visible_row(0)[3].c, 'F');
    assert_eq!(screen.grid.visible_row(0)[7].c, 'J');
    assert_eq!(screen.grid.visible_row(0)[8].c, ' '); // blank fill
    assert_eq!(screen.grid.visible_row(0)[9].c, ' '); // blank fill
}

#[test]
fn insert_character_ich() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"ABCDEFGHIJ"); // fill row 0
    screen.process(b"\x1b[1;3H");  // move to col 3 (0-based col 2)
    // CSI 2@ — insert 2 blank chars at cursor, shifting right
    screen.process(b"\x1b[2@");
    assert_eq!(screen.grid.visible_row(0)[0].c, 'A');
    assert_eq!(screen.grid.visible_row(0)[1].c, 'B');
    assert_eq!(screen.grid.visible_row(0)[2].c, ' '); // inserted blank
    assert_eq!(screen.grid.visible_row(0)[3].c, ' '); // inserted blank
    assert_eq!(screen.grid.visible_row(0)[4].c, 'C'); // shifted right
    assert_eq!(screen.grid.visible_row(0)[5].c, 'D'); // shifted right
    // 'I' and 'J' fall off the right edge
    assert_eq!(screen.grid.visible_row(0)[9].c, 'H');
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
    assert_eq!(screen.grid.visible_row(0)[0].c, 'R');
    assert_eq!(screen.grid.visible_row(0)[3].c, '2');
    // Last two rows should be blank
    assert_eq!(screen.grid.visible_row(3)[0].c, ' ');
    assert_eq!(screen.grid.visible_row(4)[0].c, ' ');
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
    assert_eq!(screen.grid.visible_row(0)[0].c, ' ');
    assert_eq!(screen.grid.visible_row(1)[0].c, ' ');
    // Row 2 should now show what was row 0
    assert_eq!(screen.grid.visible_row(2)[0].c, 'R');
    assert_eq!(screen.grid.visible_row(2)[3].c, '0');
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
    assert_eq!(screen.grid.visible_row(1)[4].c, '3');
    // Row 2 should now be what was row 4 ("Line4")
    assert_eq!(screen.grid.visible_row(2)[4].c, '4');
    // Bottom rows should be blank
    assert_eq!(screen.grid.visible_row(3)[0].c, ' ');
    assert_eq!(screen.grid.visible_row(4)[0].c, ' ');
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
    assert_eq!(screen.grid.visible_row(0)[4].c, '0');
    // Rows 1 and 2 should be blank (inserted)
    assert_eq!(screen.grid.visible_row(1)[0].c, ' ');
    assert_eq!(screen.grid.visible_row(2)[0].c, ' ');
    // Row 3 should be what was row 1 ("Line1")
    assert_eq!(screen.grid.visible_row(3)[4].c, '1');
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
    assert_eq!(screen.grid.visible_row(1)[0].c, ' ');
    // Row 2 should now be "LineA" (shifted down)
    assert_eq!(screen.grid.visible_row(2)[0].c, 'L');
    assert_eq!(screen.grid.visible_row(2)[4].c, 'A');
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
    assert_eq!(screen.grid.visible_row(0)[4].c, 'F');

    // Re-enable autowrap
    screen.process(b"\x1b[?7h");
    assert!(screen.grid.modes.autowrap_mode);
    // Go back to start, fill line, and verify wrap now works
    screen.process(b"\x1b[1;1H");
    screen.process(b"12345");
    assert!(screen.grid.wrap_pending);
    screen.process(b"6");
    assert_eq!(screen.grid.cursor_y, 1);
    assert_eq!(screen.grid.visible_row(1)[0].c, '6');
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
    assert_eq!(screen.grid.visible_row(0)[3].style.bg, expected_bg,
        "erased cell at col 3 should have red background (BCE)");
    assert_eq!(screen.grid.visible_row(0)[9].style.bg, expected_bg,
        "erased cell at col 9 should have red background (BCE)");
    // Cells before cursor should NOT have the red bg (they were written before SGR 41)
    assert_eq!(screen.grid.visible_row(0)[0].style.bg, None,
        "cell at col 0 should have default background");

    // Also verify BCE with CSI 2J (erase entire display)
    screen.process(b"\x1b[2J");
    assert_eq!(screen.grid.visible_row(1)[5].style.bg, expected_bg,
        "CSI 2J erased cell should have red background (BCE)");

    // And ECH (erase character)
    screen.process(b"\x1b[1;1H");
    screen.process(b"XYZ");
    screen.process(b"\x1b[1;1H");
    screen.process(b"\x1b[2X"); // erase 2 chars
    assert_eq!(screen.grid.visible_row(0)[0].style.bg, expected_bg,
        "ECH erased cell should have red background (BCE)");
    assert_eq!(screen.grid.visible_row(0)[1].style.bg, expected_bg,
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
    assert_eq!(screen.grid.visible_row(0)[0].c, 'H');

    // Enter alt screen via mode 1047
    screen.process(b"\x1b[?1047h");
    assert!(screen.state.in_alt_screen);
    assert_eq!(screen.grid.visible_row(0)[0].c, ' '); // alt screen cleared

    screen.process(b"Alt");
    assert_eq!(screen.grid.visible_row(0)[0].c, 'A');

    // Leave alt screen
    screen.process(b"\x1b[?1047l");
    assert!(!screen.state.in_alt_screen);
    assert_eq!(screen.grid.visible_row(0)[0].c, 'H'); // main buffer restored
}

#[test]
fn alt_screen_mode_47() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"Main");
    assert_eq!(screen.grid.visible_row(0)[0].c, 'M');

    screen.process(b"\x1b[?47h");
    assert!(screen.state.in_alt_screen);
    assert_eq!(screen.grid.visible_row(0)[0].c, ' ');

    screen.process(b"\x1b[?47l");
    assert!(!screen.state.in_alt_screen);
    assert_eq!(screen.grid.visible_row(0)[0].c, 'M');
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
    assert_eq!(screen.grid.visible_row(2)[0].c, ' ',
        "inserted line should be blank");
    // Row 1 (above region) should be untouched
    assert_eq!(screen.grid.visible_row(0)[0].c, 'R',
        "row above scroll region should be untouched");
    // Row 5 (below region bottom) should be untouched
    assert_eq!(screen.grid.visible_row(5)[0].c, 'R',
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
    assert_eq!(screen.grid.visible_row(0)[0].c, 'e');
    assert_eq!(screen.grid.visible_row(0)[0].combining, vec!['\u{0301}']);
}

#[test]
fn combining_mark_with_wrap_pending() {
    let mut screen = Screen::new(5, 3, 100);
    // Fill the line to trigger wrap_pending
    screen.process(b"ABCDE");
    assert!(screen.grid.wrap_pending, "wrap should be pending after filling line");
    // Now send a combining mark — it should attach to the last cell (E)
    screen.process("\u{0308}".as_bytes()); // combining diaeresis
    assert_eq!(screen.grid.visible_row(0)[4].c, 'E');
    assert_eq!(screen.grid.visible_row(0)[4].combining, vec!['\u{0308}']);
}

#[test]
fn combining_mark_on_wide_char() {
    let mut screen = Screen::new(80, 24, 100);
    // Print a wide char followed by a combining mark
    screen.process("\u{4e16}\u{0301}".as_bytes()); // 世 + combining acute
    // The combining mark should attach to the wide char cell (col 0), not the continuation (col 1)
    assert_eq!(screen.grid.visible_row(0)[0].c, '\u{4e16}');
    assert_eq!(screen.grid.visible_row(0)[0].combining, vec!['\u{0301}']);
    assert_eq!(screen.grid.visible_row(0)[1].width, 0); // continuation cell
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

// ─── Scroll tests ────────────────────────────────────────────────────────

/// Helper: collect visible grid rows as trimmed strings.
fn screen_lines(screen: &Screen) -> Vec<String> {
    screen
        .grid
        .visible_rows()
        .map(|row| {
            let s: String = row.iter().map(|c| c.c).collect();
            s.trim_end().to_string()
        })
        .collect()
}

/// Helper: strip ANSI escape sequences, returning only printable text.
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

/// Helper: get scrollback history as trimmed text strings.
fn history_texts(screen: &Screen) -> Vec<String> {
    screen
        .get_history()
        .iter()
        .map(|b| strip_ansi(b))
        .collect()
}

#[test]
fn lf_at_bottom_scrolls_content_up() {
    // The most common scroll scenario: app outputs lines until the screen
    // is full, then the next LF at scroll_bottom scrolls everything up.
    let mut screen = Screen::new(10, 4, 100);
    // Fill all 4 rows
    screen.process(b"Row0\r\nRow1\r\nRow2\r\nRow3");
    assert_eq!(screen_lines(&screen), vec!["Row0", "Row1", "Row2", "Row3"]);

    // One more LF + content — triggers scroll
    screen.process(b"\r\nRow4");
    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "Row1", "first visible row after scroll");
    assert_eq!(lines[1], "Row2");
    assert_eq!(lines[2], "Row3");
    assert_eq!(lines[3], "Row4", "new content at bottom");
}

#[test]
fn lf_scroll_captures_scrollback() {
    // When a line scrolls off the top, it goes into scrollback.
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"AAA\r\nBBB\r\nCCC\r\nDDD");

    let hist = history_texts(&screen);
    assert_eq!(hist.len(), 1, "exactly one line scrolled off");
    assert_eq!(hist[0], "AAA");
}

#[test]
fn many_lines_overflow_screen() {
    // Simulates `cat large_file` — 20 lines output into a 5-row terminal.
    let mut screen = Screen::new(10, 5, 100);
    for i in 0..20 {
        if i > 0 {
            screen.process(b"\r\n");
        }
        screen.process(format!("L{:02}", i).as_bytes());
    }

    // Only the last 5 lines should be visible
    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "L15");
    assert_eq!(lines[1], "L16");
    assert_eq!(lines[2], "L17");
    assert_eq!(lines[3], "L18");
    assert_eq!(lines[4], "L19");

    // 15 lines should be in scrollback (L00..L14)
    let hist = history_texts(&screen);
    assert_eq!(hist.len(), 15);
    assert_eq!(hist[0], "L00", "first scrollback line");
    assert_eq!(hist[14], "L14", "last scrollback line");
}

#[test]
fn scrollback_order_preserved() {
    // Scrollback should maintain chronological order: oldest first.
    let mut screen = Screen::new(10, 3, 100);
    for i in 0..10 {
        if i > 0 {
            screen.process(b"\r\n");
        }
        screen.process(format!("Line{}", i).as_bytes());
    }

    let hist = history_texts(&screen);
    // 7 lines scrolled off (lines 0-6), 3 remain visible (7-9)
    assert_eq!(hist.len(), 7);
    for (idx, h) in hist.iter().enumerate() {
        assert_eq!(*h, format!("Line{}", idx),
            "scrollback line {} should be Line{}", idx, idx);
    }
}

#[test]
fn lf_within_scroll_region_only_scrolls_region() {
    // Apps like vim set a scroll region (e.g., leaving a status bar at bottom).
    // LF at scroll_bottom scrolls only within the region.
    let mut screen = Screen::new(10, 6, 100);

    // Put content on all rows first
    for row in 0..6 {
        screen.process(format!("\x1b[{};1H", row + 1).as_bytes());
        screen.process(format!("Row{}", row).as_bytes());
    }

    // Set scroll region to rows 2-5 (0-based: 1-4), leaving row 0 and row 5 outside
    screen.process(b"\x1b[2;5r");

    // Move to bottom of scroll region (row 5 = 0-based row 4)
    screen.process(b"\x1b[5;1H");
    // Write a LF — should scroll within region only
    screen.process(b"\r\n");
    screen.process(b"New");

    let lines = screen_lines(&screen);
    // Row 0 (above region) should be untouched
    assert_eq!(lines[0], "Row0", "row above scroll region must be untouched");
    // Row 5 (below region) should be untouched
    assert_eq!(lines[5], "Row5", "row below scroll region must be untouched");
    // Content within region should have scrolled up
    assert_eq!(lines[1], "Row2", "top of region should have what was row 2");
    assert_eq!(lines[2], "Row3");
    assert_eq!(lines[3], "Row4");
}

#[test]
fn scroll_region_preserves_outer_content_on_multiple_scrolls() {
    // Multiple scrolls within a region should never touch rows outside it.
    let mut screen = Screen::new(10, 6, 100);

    // Set header and footer
    screen.process(b"\x1b[1;1HHeader");
    screen.process(b"\x1b[6;1HFooter");

    // Set scroll region to rows 2-5
    screen.process(b"\x1b[2;5r");

    // Fill region and scroll many times
    screen.process(b"\x1b[2;1H");
    for i in 0..12 {
        if i > 0 {
            screen.process(b"\r\n");
        }
        screen.process(format!("Msg{:02}", i).as_bytes());
    }

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "Header", "header must survive region scrolling");
    assert_eq!(lines[5], "Footer", "footer must survive region scrolling");
}

#[test]
fn csi_s_within_scroll_region() {
    // CSI S (scroll up) should respect the scroll region.
    let mut screen = Screen::new(10, 6, 100);

    for row in 0..6 {
        screen.process(format!("\x1b[{};1H", row + 1).as_bytes());
        screen.process(format!("R{}", row).as_bytes());
    }

    // Set scroll region to rows 2-5 (0-based: 1-4)
    screen.process(b"\x1b[2;5r");
    // CSI 2S — scroll up 2 lines
    screen.process(b"\x1b[2S");

    let lines = screen_lines(&screen);
    // Row 0 (above region) untouched
    assert_eq!(lines[0], "R0", "row above region untouched after CSI S");
    // Row 5 (below region) untouched
    assert_eq!(lines[5], "R5", "row below region untouched after CSI S");
    // Region content scrolled up by 2
    assert_eq!(lines[1], "R3", "region top after scroll by 2");
    assert_eq!(lines[2], "R4", "region second row after scroll by 2");
    // Bottom two rows of region should be blank
    assert_eq!(lines[3], "", "blank line after scroll");
    assert_eq!(lines[4], "", "blank line after scroll");
}

#[test]
fn csi_t_within_scroll_region() {
    // CSI T (scroll down) should respect the scroll region.
    let mut screen = Screen::new(10, 6, 100);

    for row in 0..6 {
        screen.process(format!("\x1b[{};1H", row + 1).as_bytes());
        screen.process(format!("R{}", row).as_bytes());
    }

    // Set scroll region to rows 2-5 (0-based: 1-4)
    screen.process(b"\x1b[2;5r");
    // CSI 2T — scroll down 2 lines
    screen.process(b"\x1b[2T");

    let lines = screen_lines(&screen);
    // Row 0 (above region) untouched
    assert_eq!(lines[0], "R0", "row above region untouched after CSI T");
    // Row 5 (below region) untouched
    assert_eq!(lines[5], "R5", "row below region untouched after CSI T");
    // Top two rows of region should be blank (scrolled down)
    assert_eq!(lines[1], "", "blank line after scroll down");
    assert_eq!(lines[2], "", "blank line after scroll down");
    // Original region content shifted down by 2
    assert_eq!(lines[3], "R1", "shifted content after scroll down");
    assert_eq!(lines[4], "R2", "shifted content after scroll down");
}

#[test]
fn scroll_down_does_not_generate_scrollback() {
    // Scroll down (CSI T) should NOT produce scrollback — lines are lost at the bottom.
    let mut screen = Screen::new(10, 5, 100);
    for row in 0..5 {
        screen.process(format!("\x1b[{};1H", row + 1).as_bytes());
        screen.process(format!("Row{}", row).as_bytes());
    }

    screen.process(b"\x1b[3T"); // scroll down 3

    let scrollback = screen.take_pending_scrollback();
    assert!(scrollback.is_empty(),
        "CSI T (scroll down) should not generate scrollback");
}

#[test]
fn cursor_stays_in_place_during_lf_scroll() {
    // When LF triggers scroll, the cursor stays at scroll_bottom and
    // its column does not change.
    let mut screen = Screen::new(20, 3, 100);
    screen.process(b"Row0\r\nRow1\r\nRow2");
    // Cursor is now at row 2, col 4
    assert_eq!(screen.grid.cursor_y, 2);
    assert_eq!(screen.grid.cursor_x, 4);

    // Move cursor to col 10
    screen.process(b"\x1b[1;11H"); // row 1, col 11 (0-based: row 0, col 10)
    screen.process(b"\x1b[3;11H"); // move to row 3 (bottom), col 11

    // Trigger LF-based scroll
    screen.process(b"\r\n");
    // Cursor should be at scroll_bottom, col 0 (CR moved it)
    assert_eq!(screen.grid.cursor_y, 2, "cursor_y stays at scroll_bottom");
    assert_eq!(screen.grid.cursor_x, 0, "cursor_x reset by CR");
}

#[test]
fn cursor_column_preserved_on_bare_lf_scroll() {
    // Bare LF (without CR) at the bottom: cursor_x should not change.
    let mut screen = Screen::new(20, 3, 100);
    screen.process(b"Row0\r\nRow1\r\n");
    // Cursor at row 2, col 0
    screen.process(b"\x1b[3;8H"); // move to row 3, col 8 (0-based: row 2, col 7)
    assert_eq!(screen.grid.cursor_y, 2);
    assert_eq!(screen.grid.cursor_x, 7);

    // Bare LF (no CR) triggers scroll
    screen.process(b"\n");
    assert_eq!(screen.grid.cursor_y, 2, "cursor_y stays at scroll_bottom");
    assert_eq!(screen.grid.cursor_x, 7, "bare LF preserves cursor_x");
}

#[test]
fn reverse_index_at_top_of_full_screen_scrolls_down() {
    // ESC M (RI) at row 0 with full-screen scroll region scrolls entire screen down.
    let mut screen = Screen::new(10, 4, 100);
    screen.process(b"Row0\r\nRow1\r\nRow2\r\nRow3");

    // Move cursor to row 0
    screen.process(b"\x1b[1;1H");
    // ESC M — reverse index
    screen.process(b"\x1bM");

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "", "new blank line at top");
    assert_eq!(lines[1], "Row0", "original row 0 shifted down");
    assert_eq!(lines[2], "Row1", "original row 1 shifted down");
    assert_eq!(lines[3], "Row2", "original row 2 shifted down");
    // Row3 is lost (pushed off bottom)
}

#[test]
fn rapid_scroll_up_down_content_integrity() {
    // Alternating scroll up and scroll down should not corrupt content.
    let mut screen = Screen::new(10, 5, 100);
    for row in 0..5 {
        screen.process(format!("\x1b[{};1H", row + 1).as_bytes());
        screen.process(format!("Row{}", row).as_bytes());
    }

    // Scroll up 2, then scroll down 2 — content should differ because
    // scroll_up blanks from bottom, scroll_down blanks from top
    screen.process(b"\x1b[2S"); // scroll up 2
    screen.process(b"\x1b[2T"); // scroll down 2

    let lines = screen_lines(&screen);
    // After scroll up 2: [Row2, Row3, Row4, "", ""]
    // After scroll down 2: pop 2 blanks from bottom, push 2 blanks at top
    // Result: ["", "", Row2, Row3, Row4]
    assert_eq!(lines[0], "", "blank at top after scroll down");
    assert_eq!(lines[1], "", "blank at top after scroll down");
    assert_eq!(lines[2], "Row2", "surviving content");
    assert_eq!(lines[3], "Row3", "surviving content");
    assert_eq!(lines[4], "Row4", "surviving content shifted back");
}

#[test]
fn lf_scroll_with_styled_content() {
    // BCE: when scrolling introduces a new blank line, it should use the
    // current background color. Verify blank cells have the active style.
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"Row0\r\nRow1\r\nRow2");

    // Set background to red before scrolling
    screen.process(b"\x1b[41m");
    // LF at bottom triggers scroll — new blank line should have red bg
    screen.process(b"\r\n");

    // The bottom row (row 2) should be blank with red background
    let cell = &screen.grid.visible_row(2)[0];
    assert_eq!(cell.c, ' ', "new line should be blank");
    assert_eq!(cell.style.bg, Some(style::Color::Indexed(1)), "blank line should inherit current bg (BCE)");
}

#[test]
fn scroll_region_lf_no_scrollback_when_top_nonzero() {
    // When scroll_top > 0, LF-driven scroll should NOT capture scrollback
    // (the line is internal to a partial region, not the physical top).
    let mut screen = Screen::new(10, 6, 100);

    // Set scroll region to rows 3-6 (0-based: 2-5) — scroll_top != 0
    screen.process(b"\x1b[3;6r");

    // Fill region and overflow
    screen.process(b"\x1b[3;1H");
    for i in 0..10 {
        if i > 0 {
            screen.process(b"\r\n");
        }
        screen.process(format!("Msg{}", i).as_bytes());
    }

    let scrollback = screen.take_pending_scrollback();
    assert!(scrollback.is_empty(),
        "scroll_top > 0: no scrollback should be captured");
}

// ─── TUI app scroll tests ───────────────────────────────────────────────
// Scenarios for interactive programs (vim, htop, Claude CLI, etc.)
// that enter alt screen, set scroll regions, and scroll content.

#[test]
fn tui_app_alt_screen_scroll_region_lf() {
    // A TUI app enters alt screen, sets a scroll region (status bar at bottom),
    // and outputs enough lines to scroll the content area via LF.
    let mut screen = Screen::new(20, 6, 100);
    screen.process(b"MainContent"); // main screen content

    // Enter alt screen
    screen.process(b"\x1b[?1049h");

    // Draw status bar on row 6
    screen.process(b"\x1b[6;1HStatus: OK");

    // Set scroll region to rows 1-5 (content area, leaving status bar outside)
    screen.process(b"\x1b[1;5r");

    // Fill content area and overflow — triggers scroll within region
    screen.process(b"\x1b[1;1H");
    for i in 0..8 {
        if i > 0 {
            screen.process(b"\r\n");
        }
        screen.process(format!("Msg{:02}", i).as_bytes());
    }

    let lines = screen_lines(&screen);
    // Status bar (row 5, outside region) must be untouched
    assert_eq!(lines[5], "Status: OK", "status bar must survive scroll");
    // Last 5 messages should be visible in the content area
    assert_eq!(lines[0], "Msg03");
    assert_eq!(lines[1], "Msg04");
    assert_eq!(lines[2], "Msg05");
    assert_eq!(lines[3], "Msg06");
    assert_eq!(lines[4], "Msg07");
}

#[test]
fn tui_app_alt_screen_explicit_scroll_up() {
    // A TUI app uses CSI S (explicit scroll up) in alt screen to scroll content.
    let mut screen = Screen::new(15, 5, 100);

    // Enter alt screen
    screen.process(b"\x1b[?1049h");

    // Place content
    for row in 0..5 {
        screen.process(format!("\x1b[{};1H", row + 1).as_bytes());
        screen.process(format!("Line{}", row).as_bytes());
    }

    // CSI 2S — scroll up 2 lines
    screen.process(b"\x1b[2S");

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "Line2", "content shifted up by 2");
    assert_eq!(lines[1], "Line3");
    assert_eq!(lines[2], "Line4");
    assert_eq!(lines[3], "", "blank line at bottom");
    assert_eq!(lines[4], "", "blank line at bottom");

    // No scrollback should be generated in alt screen
    let scrollback = screen.take_pending_scrollback();
    assert!(scrollback.is_empty(),
        "alt screen CSI S should not generate scrollback");
}

#[test]
fn tui_app_alt_screen_scroll_then_exit_restores_main() {
    // After scrolling in alt screen, exiting should restore the original main screen.
    let mut screen = Screen::new(15, 4, 100);
    screen.process(b"Original0\r\nOriginal1\r\nOriginal2\r\nOriginal3");

    // Enter alt screen, scroll heavily
    screen.process(b"\x1b[?1049h");
    for i in 0..20 {
        if i > 0 {
            screen.process(b"\r\n");
        }
        screen.process(format!("Alt{}", i).as_bytes());
    }

    // Exit alt screen
    screen.process(b"\x1b[?1049l");

    // Main screen should be fully restored
    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "Original0");
    assert_eq!(lines[1], "Original1");
    assert_eq!(lines[2], "Original2");
    assert_eq!(lines[3], "Original3");
}

#[test]
fn tui_app_scroll_region_with_header_and_footer() {
    // App with header (row 1), content area (rows 2-4), footer (row 5).
    // Only content area scrolls.
    let mut screen = Screen::new(20, 5, 100);

    screen.process(b"\x1b[?1049h"); // enter alt screen

    // Draw header and footer
    screen.process(b"\x1b[1;1H== My App ==");
    screen.process(b"\x1b[5;1H[Ctrl+C quit]");

    // Set scroll region to rows 2-4
    screen.process(b"\x1b[2;4r");

    // Fill content area and scroll
    screen.process(b"\x1b[2;1H");
    for i in 0..10 {
        if i > 0 {
            screen.process(b"\r\n");
        }
        screen.process(format!("item{}", i).as_bytes());
    }

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "== My App ==", "header must be preserved");
    assert_eq!(lines[4], "[Ctrl+C quit]", "footer must be preserved");
    // Last 3 items should be visible
    assert_eq!(lines[1], "item7");
    assert_eq!(lines[2], "item8");
    assert_eq!(lines[3], "item9");
}

#[test]
fn tui_app_delete_lines_within_scroll_region() {
    // A TUI app removes a line from a list using CSI M (Delete Lines)
    // within a scroll region. Content below shifts up, blank appears at region bottom.
    let mut screen = Screen::new(15, 6, 100);

    screen.process(b"\x1b[?1049h");
    screen.process(b"\x1b[1;1HTitle");
    screen.process(b"\x1b[6;1HStatus");

    // Set scroll region for content area
    screen.process(b"\x1b[2;5r");

    // Fill content area
    screen.process(b"\x1b[2;1HItem-A");
    screen.process(b"\x1b[3;1HItem-B");
    screen.process(b"\x1b[4;1HItem-C");
    screen.process(b"\x1b[5;1HItem-D");

    // Delete Item-B (row 3) — cursor must be at the line to delete
    screen.process(b"\x1b[3;1H");
    screen.process(b"\x1b[M"); // DL 1

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "Title", "title preserved");
    assert_eq!(lines[5], "Status", "status preserved");
    assert_eq!(lines[1], "Item-A", "Item-A stays");
    assert_eq!(lines[2], "Item-C", "Item-C shifted up into Item-B's position");
    assert_eq!(lines[3], "Item-D", "Item-D shifted up");
    assert_eq!(lines[4], "", "blank line at region bottom");
}

#[test]
fn tui_app_insert_lines_within_scroll_region() {
    // A TUI app inserts a new line into a list using CSI L (Insert Lines)
    // within a scroll region. Content below shifts down, last line in region is lost.
    let mut screen = Screen::new(15, 6, 100);

    screen.process(b"\x1b[?1049h");
    screen.process(b"\x1b[1;1HTitle");
    screen.process(b"\x1b[6;1HStatus");

    // Set scroll region for content area
    screen.process(b"\x1b[2;5r");

    // Fill content area
    screen.process(b"\x1b[2;1HItem-A");
    screen.process(b"\x1b[3;1HItem-B");
    screen.process(b"\x1b[4;1HItem-C");
    screen.process(b"\x1b[5;1HItem-D");

    // Insert a blank line at row 3 (between Item-A and Item-B)
    screen.process(b"\x1b[3;1H");
    screen.process(b"\x1b[L"); // IL 1
    screen.process(b"NEW ITEM");

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "Title", "title preserved");
    assert_eq!(lines[5], "Status", "status preserved");
    assert_eq!(lines[1], "Item-A", "Item-A stays");
    assert_eq!(lines[2], "NEW ITEM", "new item inserted");
    assert_eq!(lines[3], "Item-B", "Item-B shifted down");
    assert_eq!(lines[4], "Item-C", "Item-C shifted down");
    // Item-D is pushed off the bottom of the scroll region
}

#[test]
fn tui_app_reverse_index_in_scroll_region() {
    // A TUI app uses ESC M (Reverse Index) at the top of a scroll region
    // to scroll content down — e.g., inserting a line at the top of the output area.
    let mut screen = Screen::new(15, 6, 100);

    screen.process(b"\x1b[?1049h");
    screen.process(b"\x1b[1;1HHeader");
    screen.process(b"\x1b[6;1HFooter");

    // Set scroll region
    screen.process(b"\x1b[2;5r");

    // Fill content area
    screen.process(b"\x1b[2;1HMsg-A");
    screen.process(b"\x1b[3;1HMsg-B");
    screen.process(b"\x1b[4;1HMsg-C");
    screen.process(b"\x1b[5;1HMsg-D");

    // Move to top of scroll region and reverse index
    screen.process(b"\x1b[2;1H");
    screen.process(b"\x1bM"); // RI — scroll content down within region

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "Header", "header preserved");
    assert_eq!(lines[5], "Footer", "footer preserved");
    assert_eq!(lines[1], "", "new blank line at region top");
    assert_eq!(lines[2], "Msg-A", "Msg-A shifted down");
    assert_eq!(lines[3], "Msg-B", "Msg-B shifted down");
    assert_eq!(lines[4], "Msg-C", "Msg-C shifted down");
    // Msg-D pushed off region bottom
}

#[test]
fn tui_app_scroll_region_change_mid_session() {
    // A TUI app changes its scroll region mid-session (e.g., toggling
    // a bottom panel). Content outside both old and new regions should survive.
    let mut screen = Screen::new(20, 8, 100);

    screen.process(b"\x1b[?1049h");
    screen.process(b"\x1b[1;1HTop Bar");
    screen.process(b"\x1b[8;1HBottom Bar");

    // First scroll region: rows 2-7 (full content area)
    screen.process(b"\x1b[2;7r");
    screen.process(b"\x1b[2;1H");
    for i in 0..6 {
        if i > 0 { screen.process(b"\r\n"); }
        screen.process(format!("V1_{}", i).as_bytes());
    }

    // Verify initial state
    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "Top Bar");
    assert_eq!(lines[7], "Bottom Bar");

    // Change scroll region: now rows 2-4 (smaller content area, panel at rows 5-7)
    screen.process(b"\x1b[2;4r");
    screen.process(b"\x1b[5;1HPanel-A");
    screen.process(b"\x1b[6;1HPanel-B");
    screen.process(b"\x1b[7;1HPanel-C");

    // Scroll within the new smaller region
    screen.process(b"\x1b[2;1H");
    for i in 0..6 {
        if i > 0 { screen.process(b"\r\n"); }
        screen.process(format!("V2_{}", i).as_bytes());
    }

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "Top Bar", "top bar survives region change");
    assert_eq!(lines[7], "Bottom Bar", "bottom bar survives region change");
    // Panel rows (5-7, outside new scroll region) should be intact
    assert_eq!(lines[4], "Panel-A", "panel row survives scroll in smaller region");
    assert_eq!(lines[5], "Panel-B", "panel row survives scroll in smaller region");
    assert_eq!(lines[6], "Panel-C", "panel row survives scroll in smaller region");
}

// ─── Edge-case scroll tests ────────────────────────────────────────────

#[test]
fn scroll_up_count_exceeds_region_size() {
    // CSI 100S on a 3-row region — should blank the entire region without panic.
    let mut screen = Screen::new(10, 6, 100);
    for row in 0..6 {
        screen.process(format!("\x1b[{};1HR{}", row + 1, row).as_bytes());
    }
    // Set 3-row scroll region (rows 2-4, 0-based: 1-3)
    screen.process(b"\x1b[2;4r");
    // Scroll up way more than the region size
    screen.process(b"\x1b[100S");

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "R0", "row above region untouched");
    assert_eq!(lines[5], "R5", "row below region untouched");
    // Entire region should be blank
    assert_eq!(lines[1], "", "region blanked");
    assert_eq!(lines[2], "", "region blanked");
    assert_eq!(lines[3], "", "region blanked");
}

#[test]
fn scroll_down_count_exceeds_region_size() {
    // CSI 100T on a 3-row region — should blank the entire region without panic.
    let mut screen = Screen::new(10, 6, 100);
    for row in 0..6 {
        screen.process(format!("\x1b[{};1HR{}", row + 1, row).as_bytes());
    }
    screen.process(b"\x1b[2;4r");
    screen.process(b"\x1b[100T");

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "R0", "row above region untouched");
    assert_eq!(lines[5], "R5", "row below region untouched");
    assert_eq!(lines[1], "", "region blanked");
    assert_eq!(lines[2], "", "region blanked");
    assert_eq!(lines[3], "", "region blanked");
}

#[test]
fn lf_at_last_row_outside_scroll_region() {
    // Cursor at the absolute last row, but scroll region ends earlier.
    // LF should do nothing — cursor is stuck.
    let mut screen = Screen::new(10, 6, 100);
    screen.process(b"\x1b[6;1HBottom");

    // Set scroll region to rows 1-4 (0-based: 0-3)
    screen.process(b"\x1b[1;4r");

    // Move cursor to the very last row (below scroll region)
    screen.process(b"\x1b[6;1H");
    assert_eq!(screen.grid.cursor_y, 5);

    // LF — cursor is at row 5 (last), scroll_bottom is 3. Should not scroll, should not move.
    screen.process(b"\n");
    assert_eq!(screen.grid.cursor_y, 5, "cursor stuck at last row outside region");

    let lines = screen_lines(&screen);
    assert_eq!(lines[5], "Bottom", "content at last row unchanged");
}

#[test]
fn lf_between_scroll_top_and_bottom_just_moves_cursor() {
    // LF when cursor is inside the scroll region but NOT at scroll_bottom
    // should just move cursor down without scrolling.
    let mut screen = Screen::new(10, 6, 100);
    for row in 0..6 {
        screen.process(format!("\x1b[{};1HR{}", row + 1, row).as_bytes());
    }

    screen.process(b"\x1b[2;5r"); // region rows 2-5 (0-based: 1-4)
    screen.process(b"\x1b[3;1H"); // cursor at row 3 (0-based: 2), inside region

    screen.process(b"\n"); // LF

    assert_eq!(screen.grid.cursor_y, 3, "cursor moved down by 1");
    // No scrolling — all content intact
    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "R0");
    assert_eq!(lines[1], "R1");
    assert_eq!(lines[2], "R2");
    assert_eq!(lines[3], "R3");
    assert_eq!(lines[4], "R4");
    assert_eq!(lines[5], "R5");
}

#[test]
fn autowrap_at_scroll_bottom_triggers_scroll() {
    // When the cursor is at (scroll_bottom, last_col) with autowrap on,
    // the next character triggers wrap which triggers scroll.
    let mut screen = Screen::new(4, 4, 100);
    screen.process(b"AAA0\r\nBBB1\r\nCCC2\r\nDDD3");

    // "DDD3" is 4 chars in 4-col terminal → cursor at col 3 with wrap_pending
    assert!(screen.grid.wrap_pending, "wrap_pending after filling last cell");

    // Print one more character — triggers deferred wrap → scroll.
    screen.process(b"X");

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "BBB1", "scroll happened: AAA0 gone");
    assert_eq!(lines[1], "CCC2");
    assert_eq!(lines[2], "DDD3");
    assert_eq!(lines[3], "X", "new char on fresh line");
}

#[test]
fn autowrap_at_scroll_region_bottom_triggers_region_scroll() {
    // Same as above but within a scroll region.
    let mut screen = Screen::new(5, 6, 100);

    screen.process(b"\x1b[1;1HHead");
    screen.process(b"\x1b[6;1HFoot");

    // Set scroll region to rows 2-5 (0-based: 1-4)
    screen.process(b"\x1b[2;5r");

    // Fill the entire region row by row to the last column
    screen.process(b"\x1b[2;1HAAAAA"); // row 2 (all 5 cols filled)
    screen.process(b"\x1b[3;1HBBBBB"); // row 3
    screen.process(b"\x1b[4;1HCCCCC"); // row 4
    screen.process(b"\x1b[5;1HDDDDD"); // row 5 (scroll_bottom)

    // Cursor is at the end of row 5, wrap_pending is set.
    // Print a char — should trigger scroll within region.
    screen.process(b"Z");

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "Head", "header survives autowrap scroll");
    assert_eq!(lines[5], "Foot", "footer survives autowrap scroll");
    assert_eq!(lines[1], "BBBBB", "AAAAA scrolled off, BBBBB now at region top");
    assert_eq!(lines[4], "Z", "Z on new line at region bottom");
}

#[test]
fn wide_char_wrap_at_scroll_bottom_triggers_scroll() {
    // A wide character (2 cells) that doesn't fit at the end of scroll_bottom
    // should trigger wrap + scroll.
    let mut screen = Screen::new(5, 3, 100);
    screen.process(b"Row0\r\nRow1\r\n");
    // Cursor at row 2, col 0. Fill to col 4 (last col)
    screen.process(b"ABCD");
    // Cursor at col 4 (0-based), which is the last column.
    assert_eq!(screen.grid.cursor_x, 4);

    // Print a wide char — it needs 2 cells but only 1 available.
    // Should: fill col 4 with blank, wrap to next line (triggers scroll), then print wide char.
    screen.process("你".as_bytes());

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "Row1", "Row0 scrolled off");
    assert_eq!(lines[1], "ABCD", "original row with blank at col 4");
    // Wide char is on the new line
    assert_eq!(screen.grid.visible_row(2)[0].c, '你', "wide char on scrolled-in line");
    assert_eq!(screen.grid.visible_row(2)[0].width, 2);
}

#[test]
fn csi_r_reset_restores_full_screen_scroll() {
    // CSI r (no params) should reset scroll region to full screen.
    let mut screen = Screen::new(10, 6, 100);

    // Set partial region
    screen.process(b"\x1b[2;5r");
    assert_eq!(screen.grid.scroll_top, 1);
    assert_eq!(screen.grid.scroll_bottom, 4);

    // Reset with CSI r (no params)
    screen.process(b"\x1b[r");
    assert_eq!(screen.grid.scroll_top, 0, "scroll_top reset to 0");
    assert_eq!(screen.grid.scroll_bottom, 5, "scroll_bottom reset to rows-1");
}

#[test]
fn scroll_single_row_region() {
    // CSI n;nr where top == bottom — should NOT change scroll region.
    // (Spec: top must be < bottom for valid region.)
    let mut screen = Screen::new(10, 6, 100);
    let old_top = screen.grid.scroll_top;
    let old_bottom = screen.grid.scroll_bottom;

    screen.process(b"\x1b[3;3r"); // top == bottom

    // Region should stay at old values (full screen)
    assert_eq!(screen.grid.scroll_top, old_top, "invalid region not applied");
    assert_eq!(screen.grid.scroll_bottom, old_bottom, "invalid region not applied");
    // But cursor still resets to home
    assert_eq!(screen.grid.cursor_x, 0);
    assert_eq!(screen.grid.cursor_y, 0);
}

#[test]
fn scroll_region_reversed_params_ignored() {
    // CSI 15;5r (top > bottom after conversion) — should NOT change region.
    let mut screen = Screen::new(10, 20, 100);
    let old_top = screen.grid.scroll_top;
    let old_bottom = screen.grid.scroll_bottom;

    screen.process(b"\x1b[15;5r"); // top 14 > bottom 4

    assert_eq!(screen.grid.scroll_top, old_top);
    assert_eq!(screen.grid.scroll_bottom, old_bottom);
}

#[test]
fn scrollback_limit_enforced_during_scroll() {
    // Scrollback should never grow beyond the configured limit.
    let limit = 5;
    let mut screen = Screen::new(10, 3, limit);

    // Generate more scrollback than the limit
    for i in 0..20 {
        if i > 0 { screen.process(b"\r\n"); }
        screen.process(format!("L{:02}", i).as_bytes());
    }

    let hist = history_texts(&screen);
    assert_eq!(hist.len(), limit,
        "scrollback should be exactly at limit {}, got {}", limit, hist.len());
    // The oldest lines should have been evicted
    assert_eq!(hist[0], "L12", "oldest scrollback should be evicted");
}

#[test]
fn csi_s_does_not_move_cursor() {
    // CSI S should scroll content but NOT change cursor position.
    let mut screen = Screen::new(10, 5, 100);
    screen.process(b"\x1b[3;5H"); // cursor at row 3, col 5 (0-based: 2, 4)
    assert_eq!(screen.grid.cursor_y, 2);
    assert_eq!(screen.grid.cursor_x, 4);

    screen.process(b"\x1b[2S"); // scroll up 2

    assert_eq!(screen.grid.cursor_y, 2, "CSI S must not change cursor_y");
    assert_eq!(screen.grid.cursor_x, 4, "CSI S must not change cursor_x");
}

#[test]
fn csi_t_does_not_move_cursor() {
    // CSI T should scroll content but NOT change cursor position.
    let mut screen = Screen::new(10, 5, 100);
    screen.process(b"\x1b[3;5H");
    assert_eq!(screen.grid.cursor_y, 2);
    assert_eq!(screen.grid.cursor_x, 4);

    screen.process(b"\x1b[2T"); // scroll down 2

    assert_eq!(screen.grid.cursor_y, 2, "CSI T must not change cursor_y");
    assert_eq!(screen.grid.cursor_x, 4, "CSI T must not change cursor_x");
}

// ─── Scroll + content update tests (main screen) ───────────────────────
// Scenarios where an app scrolls and immediately overwrites content
// on the main screen (not alt screen): build logs, streaming output,
// status monitors, progress bars, etc.

#[test]
fn scroll_then_overwrite_last_line() {
    // Build output pattern: scroll up, then write new content on the bottom line.
    // Like `make` printing compiler output line by line.
    let mut screen = Screen::new(20, 4, 100);
    screen.process(b"compile a.c\r\n");
    screen.process(b"compile b.c\r\n");
    screen.process(b"compile c.c\r\n");
    screen.process(b"compile d.c");
    // Screen full: [a.c, b.c, c.c, d.c]

    // Next line scrolls + writes
    screen.process(b"\r\ncompile e.c");

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "compile b.c");
    assert_eq!(lines[1], "compile c.c");
    assert_eq!(lines[2], "compile d.c");
    assert_eq!(lines[3], "compile e.c");

    // a.c should be in scrollback
    let hist = history_texts(&screen);
    assert_eq!(hist.len(), 1);
    assert_eq!(hist[0], "compile a.c");
}

#[test]
fn scroll_then_cup_overwrite_middle() {
    // App scrolls content, then uses CUP to jump back and overwrite a
    // middle row (e.g., updating a progress bar or status line on main screen).
    let mut screen = Screen::new(20, 5, 100);
    for i in 0..5 {
        if i > 0 { screen.process(b"\r\n"); }
        screen.process(format!("Line{}", i).as_bytes());
    }
    // Screen: [Line0, Line1, Line2, Line3, Line4]

    // Scroll by 2
    screen.process(b"\r\nLine5\r\nLine6");
    // Screen: [Line2, Line3, Line4, Line5, Line6]

    // Jump to row 1 and overwrite with a progress bar
    screen.process(b"\x1b[1;1H");
    screen.process(b"[=====>    ] 50%");

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "[=====>    ] 50%", "row 0 overwritten after scroll");
    assert_eq!(lines[1], "Line3", "row 1 unchanged");
    assert_eq!(lines[2], "Line4");
    assert_eq!(lines[3], "Line5");
    assert_eq!(lines[4], "Line6");

    // Scrollback should have Line0 and Line1
    let hist = history_texts(&screen);
    assert_eq!(hist.len(), 2);
    assert_eq!(hist[0], "Line0");
    assert_eq!(hist[1], "Line1");
}

#[test]
fn continuous_scroll_with_bottom_status_update() {
    // Pattern: streaming log on main screen, with a status line at the bottom
    // updated via CUP after each scroll. Like a download progress during install.
    let mut screen = Screen::new(30, 5, 100);

    // Set scroll region: rows 1-4, row 5 is the status line
    screen.process(b"\x1b[1;4r");
    screen.process(b"\x1b[5;1HProgress: 0%");

    // Stream log lines in the scroll region
    screen.process(b"\x1b[1;1H");
    for i in 0..10 {
        if i > 0 { screen.process(b"\r\n"); }
        screen.process(format!("Installing pkg-{:02}...", i).as_bytes());

        // After each log line, update progress on row 5
        let pct = (i + 1) * 10;
        screen.process(format!("\x1b7\x1b[5;1H\x1b[2KProgress: {}%\x1b8", pct).as_bytes());
    }

    let lines = screen_lines(&screen);
    // Status line should show final progress
    assert_eq!(lines[4], "Progress: 100%", "status line updated");
    // Last 4 log lines visible in the content area
    assert_eq!(lines[0], "Installing pkg-06...");
    assert_eq!(lines[1], "Installing pkg-07...");
    assert_eq!(lines[2], "Installing pkg-08...");
    assert_eq!(lines[3], "Installing pkg-09...");
}

#[test]
fn scroll_with_erase_and_rewrite() {
    // Pattern: app scrolls, erases part of a line, rewrites it.
    // Like a compiler that shows "compiling..." then replaces with "done".
    let mut screen = Screen::new(20, 3, 100);
    screen.process(b"Step 1: done\r\n");
    screen.process(b"Step 2: done\r\n");
    screen.process(b"Step 3: running...");
    // Screen: [Step 1, Step 2, Step 3: running...]

    // Scroll + new line
    screen.process(b"\r\nStep 4: running...");
    // Screen: [Step 2, Step 3: running..., Step 4: running...]

    // Go back to Step 3's row (now row 1) and replace "running..." with "done"
    screen.process(b"\x1b[2;9H");  // row 2, col 9 (after "Step 3: ")
    screen.process(b"\x1b[0K");     // erase to end of line
    screen.process(b"done");

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "Step 2: done");
    assert_eq!(lines[1], "Step 3: done", "rewritten after scroll");
    assert_eq!(lines[2], "Step 4: running...");
}

#[test]
fn scroll_region_scroll_then_overwrite_fixed_rows() {
    // App with header/footer on main screen (no alt screen).
    // Content scrolls in a region; header and footer get updated independently.
    let mut screen = Screen::new(25, 6, 100);

    screen.process(b"\x1b[1;1HTitle: My App v1.0");
    screen.process(b"\x1b[6;1HItems: 0");

    // Scroll region for content area
    screen.process(b"\x1b[2;5r");
    screen.process(b"\x1b[2;1H");

    // Add items, scrolling the content area
    for i in 0..8 {
        if i > 0 { screen.process(b"\r\n"); }
        screen.process(format!("Item #{}", i + 1).as_bytes());

        // Update footer count after each item
        screen.process(format!("\x1b7\x1b[6;1H\x1b[2KItems: {}\x1b8", i + 1).as_bytes());
    }

    // Update the header too
    screen.process(b"\x1b7\x1b[1;1H\x1b[2KTitle: My App v2.0\x1b8");

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "Title: My App v2.0", "header updated after scrolling");
    assert_eq!(lines[5], "Items: 8", "footer updated with count");
    // Content area: last 4 items
    assert_eq!(lines[1], "Item #5");
    assert_eq!(lines[2], "Item #6");
    assert_eq!(lines[3], "Item #7");
    assert_eq!(lines[4], "Item #8");
}

#[test]
fn scroll_then_overwrite_scrolled_line_content_integrity() {
    // Verify that after scroll, overwriting a line doesn't corrupt adjacent lines.
    let mut screen = Screen::new(10, 5, 100);
    for i in 0..8 {
        if i > 0 { screen.process(b"\r\n"); }
        screen.process(format!("L{:02}", i).as_bytes());
    }
    // Visible: [L03, L04, L05, L06, L07]

    // Overwrite the middle line (row 2)
    screen.process(b"\x1b[3;1H\x1b[2K");
    screen.process(b"REPLACED");

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "L03", "line above overwrite unchanged");
    assert_eq!(lines[1], "L04", "line above overwrite unchanged");
    assert_eq!(lines[2], "REPLACED", "middle line replaced");
    assert_eq!(lines[3], "L06", "line below overwrite unchanged");
    assert_eq!(lines[4], "L07", "line below overwrite unchanged");

    // Scrollback integrity
    let hist = history_texts(&screen);
    assert_eq!(hist.len(), 3);
    assert_eq!(hist[0], "L00");
    assert_eq!(hist[1], "L01");
    assert_eq!(hist[2], "L02");
}

#[test]
fn interleaved_scroll_and_cup_writes() {
    // Rapid interleaving: scroll one line, CUP to write somewhere, scroll again.
    // Simulates an app that mixes streaming output with in-place updates.
    let mut screen = Screen::new(15, 4, 100);

    // Initial content
    screen.process(b"A\r\nB\r\nC\r\nD");
    // [A, B, C, D]

    // Scroll, write at row 0
    screen.process(b"\r\nE");
    // [B, C, D, E]
    screen.process(b"\x1b[1;1H\x1b[2KHeader");
    // [Header, C, D, E]

    // Scroll again
    screen.process(b"\x1b[4;1H"); // go to bottom
    screen.process(b"\r\nF");
    // [C, D, E, F]  — "Header" was on old row 0, now scrolled off

    // Overwrite row 0 again
    screen.process(b"\x1b[1;1H\x1b[2KNewHdr");

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "NewHdr");
    assert_eq!(lines[1], "D");
    assert_eq!(lines[2], "E");
    assert_eq!(lines[3], "F");

    // Scrollback should contain A, B, Header (in that order? or B, Header?)
    // A scrolled off first (when E was added), then B scrolled off (when F was added).
    // Wait — let me trace more carefully:
    // Initial: [A, B, C, D]
    // "\r\nE" → scroll: A to scrollback, [B, C, D, E]
    // CUP + write: [Header, C, D, E] (no scroll)
    // CUP to row 3, "\r\nF" → scroll: Header to scrollback, [C, D, E, F]
    // CUP + write: [NewHdr, D, E, F] (no scroll)
    let hist = history_texts(&screen);
    assert_eq!(hist.len(), 2);
    assert_eq!(hist[0], "A", "first scrolled-off line");
    assert_eq!(hist[1], "Header", "overwritten row scrolled off");
}

#[test]
fn scroll_and_partial_line_overwrite() {
    // App scrolls then overwrites only part of a line (not the whole line).
    // The rest of the line should keep its old content.
    let mut screen = Screen::new(20, 3, 100);
    screen.process(b"AAAAAAAAAAAAAAAAAAAA\r\n"); // 20 A's
    screen.process(b"BBBBBBBBBBBBBBBBBBBB\r\n"); // 20 B's
    screen.process(b"CCCCCCCCCCCCCCCCCCCC");     // 20 C's

    // Scroll
    screen.process(b"\r\nDDDDDDDDDDDDDDDDDDDD");
    // [BBB..., CCC..., DDD...]

    // Overwrite just the first 3 chars of row 0
    screen.process(b"\x1b[1;1H");
    screen.process(b"XYZ");

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "XYZBBBBBBBBBBBBBBBBB", "partial overwrite: first 3 replaced, rest intact");
    assert_eq!(lines[1], "CCCCCCCCCCCCCCCCCCCC", "row 1 untouched");
    assert_eq!(lines[2], "DDDDDDDDDDDDDDDDDDDD", "row 2 untouched");
}

// ─── Risky scroll edge cases ───────────────────────────────────────────

#[test]
fn ind_esc_d_at_scroll_bottom_scrolls() {
    // ESC D (IND — Index) should move cursor down, scrolling if at scroll_bottom.
    // This is the counterpart of ESC M (RI). Some apps use ESC D explicitly.
    let mut screen = Screen::new(10, 4, 100);
    screen.process(b"Row0\r\nRow1\r\nRow2\r\nRow3");

    // Cursor is at row 3 (scroll_bottom). ESC D should scroll up.
    screen.process(b"\x1bD");

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "Row1", "ESC D at bottom should scroll: Row0 gone");
    assert_eq!(lines[1], "Row2");
    assert_eq!(lines[2], "Row3");
    assert_eq!(lines[3], "", "new blank line at bottom after IND scroll");
}

#[test]
fn ind_esc_d_mid_screen_just_moves_cursor() {
    // ESC D not at scroll_bottom should just move cursor down.
    let mut screen = Screen::new(10, 5, 100);
    screen.process(b"\x1b[2;1H"); // row 2 (0-based: 1)
    screen.process(b"\x1bD");

    assert_eq!(screen.grid.cursor_y, 2, "ESC D should move cursor down by 1");
}

#[test]
fn save_cursor_scroll_restore_cursor_writes_at_shifted_content() {
    // Save cursor → scroll → restore cursor → print.
    // Cursor returns to the saved (row, col), but the content there has shifted.
    let mut screen = Screen::new(10, 4, 100);
    screen.process(b"Row0\r\nRow1\r\nRow2\r\nRow3");

    // Save cursor at row 1, col 5
    screen.process(b"\x1b[2;6H"); // row 2, col 6 (0-based: 1, 5)
    screen.process(b"\x1b7");     // DECSC save

    // Scroll by 2
    screen.process(b"\x1b[2S");
    // Screen now: [Row2, Row3, "", ""]

    // Restore cursor — should go back to (1, 5)
    screen.process(b"\x1b8");
    assert_eq!(screen.grid.cursor_y, 1, "restored cursor_y");
    assert_eq!(screen.grid.cursor_x, 5, "restored cursor_x");

    // Print at restored position — overwrites whatever is at row 1 col 5
    // (Row3 is now at row 1)
    screen.process(b"!");

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "Row2", "row 0 after scroll");
    assert_eq!(lines[1], "Row3 !", "cursor writes at restored position in shifted content");
}

#[test]
fn erase_display_ignores_scroll_region() {
    // CSI 2J should erase the ENTIRE display, not just the scroll region.
    let mut screen = Screen::new(10, 6, 100);
    for row in 0..6 {
        screen.process(format!("\x1b[{};1HR{}", row + 1, row).as_bytes());
    }

    // Set scroll region to rows 2-5
    screen.process(b"\x1b[2;5r");

    // Erase entire display
    screen.process(b"\x1b[2J");

    let lines = screen_lines(&screen);
    for (i, line) in lines.iter().enumerate() {
        assert_eq!(line, &"", "row {} should be erased by CSI 2J (regardless of scroll region)", i);
    }
}

#[test]
fn erase_display_0_from_cursor_ignores_scroll_region() {
    // CSI 0J (erase from cursor to end) should erase to screen end, not region end.
    let mut screen = Screen::new(10, 6, 100);
    for row in 0..6 {
        screen.process(format!("\x1b[{};1HR{}", row + 1, row).as_bytes());
    }

    // Set scroll region rows 2-4
    screen.process(b"\x1b[2;4r");

    // Put cursor at row 3 (inside region)
    screen.process(b"\x1b[3;1H");

    // CSI 0J — erase from cursor to END OF SCREEN (not end of region)
    screen.process(b"\x1b[0J");

    let lines = screen_lines(&screen);
    assert_eq!(lines[0], "R0", "row above cursor preserved");
    assert_eq!(lines[1], "R1", "row above cursor preserved");
    assert_eq!(lines[2], "", "cursor row erased");
    assert_eq!(lines[3], "", "row below cursor erased");
    assert_eq!(lines[4], "", "row below region erased (ED ignores region)");
    assert_eq!(lines[5], "", "last row erased (ED ignores region)");
}

#[test]
fn reattach_render_after_scroll_and_cup_overwrite() {
    // After scroll + CUP overwrite on main screen, a reattach render should
    // show the correct current screen state.
    let mut screen = Screen::new(15, 4, 100);
    for i in 0..7 {
        if i > 0 { screen.process(b"\r\n"); }
        screen.process(format!("Line{}", i).as_bytes());
    }
    // Visible: [Line3, Line4, Line5, Line6]
    // Scrollback: [Line0, Line1, Line2]

    // Overwrite row 1 via CUP
    screen.process(b"\x1b[2;1H\x1b[2KModified");
    // Visible: [Line3, Modified, Line5, Line6]

    // Render for reattach
    let mut cache = render::RenderCache::new();
    let output = screen.render(true, &mut cache);
    let rendered = String::from_utf8_lossy(&output);

    assert!(rendered.contains("Line3"), "reattach should show Line3");
    assert!(rendered.contains("Modified"), "reattach should show Modified (not Line4)");
    assert!(rendered.contains("Line5"), "reattach should show Line5");
    assert!(rendered.contains("Line6"), "reattach should show Line6");
    assert!(!rendered.contains("Line4"), "Line4 was overwritten, should not appear");
}

#[test]
fn scrollback_history_after_scroll_and_cup_overwrite() {
    // If we overwrite a line via CUP and THEN it scrolls off,
    // scrollback should contain the MODIFIED version.
    let mut screen = Screen::new(15, 3, 100);
    screen.process(b"Original\r\nRow1\r\nRow2");
    // Screen: [Original, Row1, Row2]

    // Overwrite row 0
    screen.process(b"\x1b[1;1H\x1b[2KChanged");
    // Screen: [Changed, Row1, Row2]

    // Scroll to push "Changed" into scrollback
    screen.process(b"\x1b[3;1H\r\nRow3");
    // Screen: [Row1, Row2, Row3], scrollback: [Changed]

    let hist = history_texts(&screen);
    assert_eq!(hist.len(), 1);
    assert_eq!(hist[0], "Changed", "scrollback should have the modified content, not 'Original'");
}

#[test]
fn scroll_region_lf_cursor_outside_region_between_bottom_and_last_row() {
    // Cursor below scroll_bottom but not at last row.
    // LF should just move cursor down (no scroll).
    let mut screen = Screen::new(10, 8, 100);
    for row in 0..8 {
        screen.process(format!("\x1b[{};1HR{}", row + 1, row).as_bytes());
    }

    // Scroll region rows 1-4 (0-based: 0-3)
    screen.process(b"\x1b[1;4r");

    // Put cursor at row 6 (0-based: 5), between scroll_bottom(3) and last row(7)
    screen.process(b"\x1b[6;1H");
    assert_eq!(screen.grid.cursor_y, 5);

    screen.process(b"\n"); // LF

    assert_eq!(screen.grid.cursor_y, 6, "cursor moves down, no scroll");
    // ALL content should be unchanged
    let lines = screen_lines(&screen);
    for row in 0..8 {
        assert_eq!(lines[row], format!("R{}", row), "row {} unchanged", row);
    }
}

// ─── Edge case tests ─────────────────────────────────────────────────────────

#[test]
fn combining_mark_at_column_zero_no_previous_cell() {
    // A combining mark at column 0 with no previous cell should be silently ignored
    let mut screen = Screen::new(10, 3, 100);
    screen.process("\u{0301}".as_bytes()); // combining acute accent
    // Should not crash, cursor should stay at 0
    assert_eq!(screen.grid.cursor_x, 0);
    assert_eq!(screen.grid.visible_row(0)[0].c, ' ');
    assert!(screen.grid.visible_row(0)[0].combining.is_empty());
}

#[test]
fn multiple_combining_marks_on_single_cell() {
    let mut screen = Screen::new(80, 3, 100);
    // e + combining acute + combining diaeresis
    screen.process("e\u{0301}\u{0308}".as_bytes());
    assert_eq!(screen.grid.visible_row(0)[0].c, 'e');
    assert_eq!(screen.grid.visible_row(0)[0].combining.len(), 2);
    assert_eq!(screen.grid.visible_row(0)[0].combining[0], '\u{0301}');
    assert_eq!(screen.grid.visible_row(0)[0].combining[1], '\u{0308}');
}

#[test]
fn wide_char_exactly_fills_line() {
    // Wide char at cols-2 should fit without wrapping
    let mut screen = Screen::new(4, 3, 100);
    screen.process(b"AB");       // cursor at col 2
    screen.process("你".as_bytes()); // width 2, needs cols 2-3 → fits exactly
    assert_eq!(screen.grid.visible_row(0)[2].c, '你');
    assert_eq!(screen.grid.visible_row(0)[2].width, 2);
    assert_eq!(screen.grid.visible_row(0)[3].width, 0);
    assert_eq!(screen.grid.cursor_y, 0, "should NOT have wrapped");
    assert!(screen.grid.wrap_pending, "wrap should be pending after filling line");
}

#[test]
fn wide_char_on_2_column_terminal() {
    let mut screen = Screen::new(2, 3, 100);
    screen.process("你".as_bytes());
    assert_eq!(screen.grid.visible_row(0)[0].c, '你');
    assert_eq!(screen.grid.visible_row(0)[0].width, 2);
    assert_eq!(screen.grid.visible_row(0)[1].width, 0);
    assert_eq!(screen.grid.cursor_y, 0);
    assert!(screen.grid.wrap_pending);
}

#[test]
fn wide_char_on_1_column_terminal() {
    // A 1-column terminal cannot display a width-2 char — it should be dropped.
    let mut screen = Screen::new(1, 3, 100);
    screen.process("你".as_bytes());
    // Wide char dropped: cursor stays at row 0, cell stays blank
    assert_eq!(screen.grid.cursor_y, 0, "wide char should be dropped on 1-col terminal");
    assert_eq!(screen.grid.cursor_x, 0);
    assert_eq!(screen.grid.visible_row(0)[0].c, ' ', "cell should remain blank");
}

#[test]
fn wide_char_no_autowrap_at_last_col() {
    // With autowrap off, wide char that doesn't fit at end is simply dropped
    let mut screen = Screen::new(5, 3, 100);
    screen.process(b"\x1b[?7l"); // disable autowrap
    screen.process(b"ABCD");     // cursor at col 4, 'D' at col 3
    screen.process("你".as_bytes()); // width 2, only 1 col left, autowrap off → dropped
    // 'D' is at col 3, col 4 is still blank (wide char was dropped)
    assert_eq!(screen.grid.visible_row(0)[3].c, 'D');
    assert_eq!(screen.grid.visible_row(0)[4].c, ' ', "wide char dropped, col 4 stays blank");
    assert_eq!(screen.grid.cursor_y, 0, "no wrap");
}

#[test]
fn rep_with_no_prior_print() {
    // CSI b with default last_printed_char (' ') should repeat spaces
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"\x1b[3b"); // repeat last char 3 times
    // Default last_printed_char is ' ', so 3 spaces (no visible change)
    assert_eq!(screen.grid.cursor_x, 3);
    assert_eq!(screen.grid.visible_row(0)[0].c, ' ');
    assert_eq!(screen.grid.visible_row(0)[1].c, ' ');
    assert_eq!(screen.grid.visible_row(0)[2].c, ' ');
}

#[test]
fn restore_cursor_with_no_saved_state() {
    // ESC 8 (restore cursor) with no prior save should be a no-op
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[5;10H"); // move cursor
    screen.process(b"\x1b8");       // restore (no save done)
    // Cursor should remain unchanged
    assert_eq!(screen.grid.cursor_y, 4);
    assert_eq!(screen.grid.cursor_x, 9);
}

#[test]
fn restore_cursor_csi_u_with_no_saved_state() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[3;5H");
    screen.process(b"\x1b[u"); // CSI u restore with no prior CSI s
    assert_eq!(screen.grid.cursor_y, 2);
    assert_eq!(screen.grid.cursor_x, 4);
}

#[test]
fn save_cursor_resize_then_restore_clamps() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[20;70H"); // cursor at row 20, col 70
    screen.process(b"\x1b7");        // save cursor
    screen.resize(40, 10);           // shrink
    screen.process(b"\x1b8");        // restore cursor (should clamp)
    assert_eq!(screen.grid.cursor_x, 39, "restored x should clamp to cols-1");
    assert_eq!(screen.grid.cursor_y, 9, "restored y should clamp to rows-1");
}

#[test]
fn double_enter_alt_screen() {
    // Entering alt screen when already in alt screen should be ignored,
    // preserving the original main screen in saved_grid.
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"Main");
    screen.process(b"\x1b[?1049h"); // enter alt (saves "Main")
    assert!(screen.state.in_alt_screen);
    screen.process(b"Alt1");
    screen.process(b"\x1b[?1049h"); // enter alt again — ignored
    assert!(screen.state.in_alt_screen);
    // Alt1 content should still be on screen (second enter was ignored, no clear)
    assert_eq!(screen.grid.visible_row(0)[0].c, 'A');
    screen.process(b"\x1b[?1049l"); // exit restores original main screen
    assert!(!screen.state.in_alt_screen);
    assert_eq!(screen.grid.visible_row(0)[0].c, 'M');
    assert_eq!(screen.grid.visible_row(0)[3].c, 'n');
}

#[test]
fn exit_alt_screen_when_not_in_alt() {
    // Exiting alt screen when not in alt should be a no-op:
    // no scroll region reset, no cursor restore, no grid change.
    let mut screen = Screen::new(10, 5, 100);
    screen.process(b"Hello");
    // Set a custom scroll region and move cursor
    screen.process(b"\x1b[2;4r");
    screen.process(b"\x1b[3;5H"); // row 3, col 5
    let sr_top = screen.grid.scroll_top;
    let sr_bot = screen.grid.scroll_bottom;
    let cx = screen.grid.cursor_x;
    let cy = screen.grid.cursor_y;
    // Exit alt without entering — should be completely ignored
    screen.process(b"\x1b[?1049l");
    assert!(!screen.state.in_alt_screen);
    assert_eq!(screen.grid.scroll_top, sr_top, "scroll region top must not change");
    assert_eq!(screen.grid.scroll_bottom, sr_bot, "scroll region bottom must not change");
    assert_eq!(screen.grid.cursor_x, cx, "cursor x must not change");
    assert_eq!(screen.grid.cursor_y, cy, "cursor y must not change");
}

#[test]
fn hts_sets_tab_stop() {
    let mut screen = Screen::new(80, 3, 100);
    screen.process(b"\x1b[1;5H"); // move to col 5
    screen.process(b"\x1bH");      // HTS — set tab stop at col 4
    screen.process(b"\x1b[1;1H"); // move home
    screen.process(b"\t");          // tab should stop at col 4 (our custom stop)
    assert_eq!(screen.grid.cursor_x, 4);
}

#[test]
fn tbc_clear_current_tab_stop() {
    let mut screen = Screen::new(80, 3, 100);
    // Default tab stop at col 8
    screen.process(b"\x1b[1;9H"); // move to col 9 (0-based: col 8)
    screen.process(b"\x1b[0g");    // TBC 0 — clear tab stop at current col (8)
    screen.process(b"\x1b[1;1H"); // move home
    screen.process(b"\t");          // tab should skip col 8, go to col 16
    assert_eq!(screen.grid.cursor_x, 16);
}

#[test]
fn tbc_clear_all_tab_stops() {
    let mut screen = Screen::new(80, 3, 100);
    screen.process(b"\x1b[3g");   // TBC 3 — clear all tab stops
    screen.process(b"\t");         // no tab stops → clamp to right margin
    assert_eq!(screen.grid.cursor_x, 79);
}

#[test]
fn scroll_region_top_equals_bottom_rejected() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[5;5r"); // top == bottom → should be ignored
    // Scroll region should remain full screen
    assert_eq!(screen.grid.scroll_top, 0);
    assert_eq!(screen.grid.scroll_bottom, 23);
}

#[test]
fn scroll_region_reversed_params_rejected() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[15;5r"); // top > bottom → should be ignored
    assert_eq!(screen.grid.scroll_top, 0);
    assert_eq!(screen.grid.scroll_bottom, 23);
}

#[test]
fn ech_count_exceeds_remaining_columns() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"ABCDEFGHIJ");
    screen.process(b"\x1b[1;6H"); // col 6 (0-based: 5)
    screen.process(b"\x1b[100X"); // ECH 100 — should clamp
    // Cells 5-9 should be erased, cells 0-4 intact
    for i in 0..5 {
        assert_ne!(screen.grid.visible_row(0)[i].c, ' ');
    }
    for i in 5..10 {
        assert_eq!(screen.grid.visible_row(0)[i].c, ' ',
            "col {} should be erased", i);
    }
}

#[test]
fn dch_at_end_of_line() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"ABCDEFGHIJ");
    screen.process(b"\x1b[1;10H"); // last column (0-based: 9)
    screen.process(b"\x1b[P");     // DCH 1 at end
    // Last cell should be blank, rest intact
    assert_eq!(screen.grid.visible_row(0)[8].c, 'I');
    assert_eq!(screen.grid.visible_row(0)[9].c, ' ');
}

#[test]
fn dch_count_exceeds_remaining_columns() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"ABCDEFGHIJ");
    screen.process(b"\x1b[1;6H"); // col 6 (0-based: 5)
    screen.process(b"\x1b[100P"); // DCH 100 — should clamp
    // Cols 5-9 should be blank, 0-4 intact
    for (i, ch) in "ABCDE".chars().enumerate() {
        assert_eq!(screen.grid.visible_row(0)[i].c, ch);
    }
    for i in 5..10 {
        assert_eq!(screen.grid.visible_row(0)[i].c, ' ',
            "col {} should be blank after large DCH", i);
    }
}

#[test]
fn ich_count_exceeds_remaining_columns() {
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"ABCDEFGHIJ");
    screen.process(b"\x1b[1;1H"); // col 1 (0-based: 0)
    screen.process(b"\x1b[100@"); // ICH 100 — should clamp
    // All cells should be blank (original content pushed off)
    for i in 0..10 {
        assert_eq!(screen.grid.visible_row(0)[i].c, ' ',
            "col {} should be blank after large ICH", i);
    }
}

#[test]
fn ind_esc_d_within_scroll_region_nonzero_top() {
    // ESC D (IND) when cursor is at scroll_bottom of a non-full-screen region
    let mut screen = Screen::new(10, 6, 100);
    for row in 0..6 {
        screen.process(format!("\x1b[{};1HR{}", row + 1, row).as_bytes());
    }
    // Set scroll region rows 2-4 (0-based: 1-3)
    screen.process(b"\x1b[2;4r");
    // Move cursor to scroll_bottom (row 4, 0-based: 3)
    screen.process(b"\x1b[4;1H");
    assert_eq!(screen.grid.cursor_y, 3);
    // IND should scroll within region only
    screen.process(b"\x1bD");
    // Row 0 (outside region) should be unchanged
    assert_eq!(screen.grid.visible_row(0)[0].c, 'R');
    assert_eq!(screen.grid.visible_row(0)[1].c, '0');
    // Row 5 (outside region) should be unchanged
    assert_eq!(screen.grid.visible_row(5)[0].c, 'R');
    assert_eq!(screen.grid.visible_row(5)[1].c, '5');
    // Within region, rows should have shifted up
    // Row 1 was R1, now should be R2 (shifted up)
    assert_eq!(screen.grid.visible_row(1)[1].c, '2');
}

#[test]
fn ed_0_cursor_on_wide_char_continuation() {
    // Erase from cursor when cursor is on the continuation cell of a wide char
    let mut screen = Screen::new(10, 3, 100);
    screen.process("AB你CD".as_bytes());
    // 你 is at cols 2-3, cursor is at col 6
    // Move cursor to col 3 (continuation of 你)
    screen.process(b"\x1b[1;4H"); // 0-based: col 3
    screen.process(b"\x1b[J");    // ED 0 — erase from cursor
    // The first half of 你 should also be blanked (fixup_wide_char)
    assert_eq!(screen.grid.visible_row(0)[2].c, ' ', "first half of wide char should be blanked");
    assert_eq!(screen.grid.visible_row(0)[3].c, ' ', "continuation cell should be blanked");
}

#[test]
fn el_1_cursor_on_wide_char_continuation() {
    // Erase to cursor when cursor is on continuation cell of a wide char
    let mut screen = Screen::new(10, 3, 100);
    screen.process("你BCDE".as_bytes());
    // 你 is at cols 0-1
    screen.process(b"\x1b[1;2H"); // 0-based: col 1 (continuation of 你)
    screen.process(b"\x1b[1K");   // EL 1 — erase to cursor
    // Both halves of 你 should be erased
    assert_eq!(screen.grid.visible_row(0)[0].c, ' ');
    assert_eq!(screen.grid.visible_row(0)[1].c, ' ');
    // B at col 2 should be intact
    assert_eq!(screen.grid.visible_row(0)[2].c, 'B');
}

#[test]
fn ris_preserves_scrollback() {
    // ESC c (full reset) should NOT clear scrollback (unlike CSI 3 J)
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"A\r\nB\r\nC\r\nD\r\nE");
    let _ = screen.take_pending_scrollback();
    let hist_before = screen.get_history().len();
    assert!(hist_before > 0);
    screen.process(b"\x1bc"); // RIS
    let hist_after = screen.get_history().len();
    assert_eq!(hist_after, hist_before,
        "ESC c should not clear scrollback history");
}

#[test]
fn scrollback_limit_zero() {
    // With scrollback_limit=0, scrollback should be completely disabled
    let mut screen = Screen::new(10, 3, 0);
    screen.process(b"A\r\nB\r\nC\r\nD\r\nE");
    let hist = screen.get_history();
    assert_eq!(hist.len(), 0,
        "scrollback with limit=0 should store nothing, got {}", hist.len());
    let pending = screen.take_pending_scrollback();
    assert!(pending.is_empty(),
        "pending scrollback with limit=0 should be empty");
}

#[test]
fn csi_f_is_alias_for_cup() {
    // CSI f should work the same as CSI H
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[5;10f"); // HVP
    assert_eq!(screen.grid.cursor_y, 4);
    assert_eq!(screen.grid.cursor_x, 9);
}

#[test]
fn csi_cup_zero_params_default_to_home() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[5;10H"); // move away
    screen.process(b"\x1b[H");     // CUP with no params → (1,1)
    assert_eq!(screen.grid.cursor_y, 0);
    assert_eq!(screen.grid.cursor_x, 0);
}

#[test]
fn csi_cup_params_beyond_screen_clamp() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[999;999H");
    assert_eq!(screen.grid.cursor_y, 23);
    assert_eq!(screen.grid.cursor_x, 79);
}

#[test]
fn dsr_at_boundary_positions() {
    // CPR at (1,1) — home position
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[1;1H");
    screen.process(b"\x1b[6n");
    let r = screen.take_responses();
    assert_eq!(r[0], b"\x1b[1;1R");

    // CPR at bottom-right corner
    screen.process(b"\x1b[24;80H");
    screen.process(b"\x1b[6n");
    let r = screen.take_responses();
    assert_eq!(r[0], b"\x1b[24;80R");
}

#[test]
fn responses_drained_after_take() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[6n");
    let r1 = screen.take_responses();
    assert_eq!(r1.len(), 1);
    let r2 = screen.take_responses();
    assert!(r2.is_empty(), "responses should be empty after drain");
}

#[test]
fn cursor_movement_with_zero_params_defaults() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[10;10H"); // start at (10,10)
    screen.process(b"\x1b[A");  // CUU default 1
    assert_eq!(screen.grid.cursor_y, 8);
    screen.process(b"\x1b[B");  // CUD default 1
    assert_eq!(screen.grid.cursor_y, 9);
    screen.process(b"\x1b[C");  // CUF default 1
    assert_eq!(screen.grid.cursor_x, 10);
    screen.process(b"\x1b[D");  // CUB default 1
    assert_eq!(screen.grid.cursor_x, 9);
}

#[test]
fn cursor_movement_beyond_bounds_clamps() {
    let mut screen = Screen::new(10, 5, 100);
    screen.process(b"\x1b[3;5H"); // center
    screen.process(b"\x1b[999A"); // CUU way past top
    assert_eq!(screen.grid.cursor_y, 0);
    screen.process(b"\x1b[999B"); // CUD way past bottom
    assert_eq!(screen.grid.cursor_y, 4);
    screen.process(b"\x1b[999D"); // CUB way past left
    assert_eq!(screen.grid.cursor_x, 0);
    screen.process(b"\x1b[999C"); // CUF way past right
    assert_eq!(screen.grid.cursor_x, 9);
}

#[test]
fn overwrite_wide_char_first_half() {
    // Writing a narrow char on the first half of a wide char should blank continuation
    let mut screen = Screen::new(10, 3, 100);
    screen.process("你好".as_bytes()); // cols 0-1: 你, cols 2-3: 好
    screen.process(b"\x1b[1;1H");     // back to col 0
    screen.process(b"X");              // overwrite first half of 你
    assert_eq!(screen.grid.visible_row(0)[0].c, 'X');
    assert_eq!(screen.grid.visible_row(0)[0].width, 1);
    assert_eq!(screen.grid.visible_row(0)[1].c, ' ', "continuation should be blanked");
    assert_eq!(screen.grid.visible_row(0)[1].width, 1);
}

#[test]
fn overwrite_wide_char_second_half() {
    // Writing a narrow char on the continuation cell should blank the first half
    let mut screen = Screen::new(10, 3, 100);
    screen.process("你好".as_bytes());
    screen.process(b"\x1b[1;2H");    // col 2 (0-based: 1, the continuation of 你)
    screen.process(b"X");             // overwrite continuation
    assert_eq!(screen.grid.visible_row(0)[0].c, ' ', "first half should be blanked");
    assert_eq!(screen.grid.visible_row(0)[0].width, 1);
    assert_eq!(screen.grid.visible_row(0)[1].c, 'X');
    assert_eq!(screen.grid.visible_row(0)[1].width, 1);
}

#[test]
fn lf_at_last_row_without_scroll_region() {
    // LF when cursor is at the absolute last row (no scroll region) should scroll
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"\x1b[3;1H"); // row 3 (last row)
    screen.process(b"X");
    screen.process(b"\n");         // should scroll up
    // Cursor stays at row 2 (scroll_bottom), content shifted
    assert_eq!(screen.grid.cursor_y, 2);
}

#[test]
fn cr_does_not_scroll() {
    // CR should never cause scrolling
    let mut screen = Screen::new(10, 3, 100);
    screen.process(b"ABCDEFGHIJ"); // fill row 0, wrap pending
    screen.process(b"\r");          // CR
    assert_eq!(screen.grid.cursor_x, 0);
    assert_eq!(screen.grid.cursor_y, 0, "CR should stay on same row");
}

#[test]
fn sgr_color_reset_39_49() {
    let mut screen = Screen::new(80, 3, 100);
    screen.process(b"\x1b[31;42m"); // red fg, green bg
    screen.process(b"A");
    assert!(screen.grid.visible_row(0)[0].style.fg.is_some());
    assert!(screen.grid.visible_row(0)[0].style.bg.is_some());
    screen.process(b"\x1b[39m");    // reset fg to default
    screen.process(b"B");
    assert!(screen.grid.visible_row(0)[1].style.fg.is_none(), "fg should be reset");
    assert!(screen.grid.visible_row(0)[1].style.bg.is_some(), "bg should remain");
    screen.process(b"\x1b[49m");    // reset bg to default
    screen.process(b"C");
    assert!(screen.grid.visible_row(0)[2].style.fg.is_none());
    assert!(screen.grid.visible_row(0)[2].style.bg.is_none(), "bg should be reset");
}

#[test]
fn scroll_down_within_region_does_not_produce_scrollback() {
    // CSI T (scroll down) should never produce scrollback
    let mut screen = Screen::new(10, 5, 100);
    screen.process(b"\x1b[2;4r"); // scroll region rows 2-4
    screen.process(b"\x1b[2;1H"); // inside region
    screen.process(b"\x1b[3T");   // scroll down 3 lines within region
    let pending = screen.take_pending_scrollback();
    assert!(pending.is_empty(), "scroll down should never produce scrollback");
}

#[test]
fn mouse_mode_per_mode_toggle() {
    // Each mouse mode is independently toggled (xterm behavior)
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[?1000h");
    assert!(screen.grid.modes.mouse_modes.click);
    assert!(!screen.grid.modes.mouse_modes.button);
    screen.process(b"\x1b[?1000l");
    assert!(!screen.grid.modes.mouse_modes.click);
}

#[test]
fn mouse_mode_multiple_simultaneous() {
    // Multiple modes can be enabled simultaneously
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[?1000h");
    screen.process(b"\x1b[?1003h");
    assert!(screen.grid.modes.mouse_modes.click);
    assert!(screen.grid.modes.mouse_modes.any);
    // Effective is highest priority
    assert_eq!(screen.grid.modes.mouse_modes.effective(), super::grid::MouseMode::Any);
}

#[test]
fn mouse_mode_disable_own_mode_only() {
    // Disabling one mode doesn't affect others
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[?1000h");
    screen.process(b"\x1b[?1002h");
    screen.process(b"\x1b[?1000l"); // disable only click
    assert!(!screen.grid.modes.mouse_modes.click);
    assert!(screen.grid.modes.mouse_modes.button);
    assert_eq!(screen.grid.modes.mouse_modes.effective(), super::grid::MouseMode::Button);
}

#[test]
fn mouse_mode_priority_resolution() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[?1000h");
    screen.process(b"\x1b[?1002h");
    screen.process(b"\x1b[?1003h");
    assert_eq!(screen.grid.modes.mouse_modes.effective(), super::grid::MouseMode::Any);
    screen.process(b"\x1b[?1003l");
    assert_eq!(screen.grid.modes.mouse_modes.effective(), super::grid::MouseMode::Button);
    screen.process(b"\x1b[?1002l");
    assert_eq!(screen.grid.modes.mouse_modes.effective(), super::grid::MouseMode::Click);
    screen.process(b"\x1b[?1000l");
    assert_eq!(screen.grid.modes.mouse_modes.effective(), super::grid::MouseMode::Off);
}

#[test]
fn mouse_encoding_disable_resets_to_x10() {
    let mut screen = Screen::new(80, 24, 100);
    screen.process(b"\x1b[?1006h"); // SGR encoding
    assert_eq!(screen.grid.modes.mouse_encoding, super::grid::MouseEncoding::Sgr);
    screen.process(b"\x1b[?1006l");
    assert_eq!(screen.grid.modes.mouse_encoding, super::grid::MouseEncoding::X10);
}

#[test]
fn cursor_visibility_toggle() {
    let mut screen = Screen::new(80, 24, 100);
    assert!(screen.grid.cursor_visible);
    screen.process(b"\x1b[?25l"); // hide
    assert!(!screen.grid.cursor_visible);
    screen.process(b"\x1b[?25h"); // show
    assert!(screen.grid.cursor_visible);
}

#[test]
fn dl_il_outside_scroll_region_no_op() {
    // DL/IL when cursor is outside the scroll region should be no-op
    let mut screen = Screen::new(10, 6, 100);
    for row in 0..6 {
        screen.process(format!("\x1b[{};1HR{}", row + 1, row).as_bytes());
    }
    screen.process(b"\x1b[2;4r"); // region rows 2-4 (0-based: 1-3)
    screen.process(b"\x1b[6;1H"); // row 6 (0-based: 5), outside region
    screen.process(b"\x1b[M");    // DL — should be no-op
    screen.process(b"\x1b[L");    // IL — should be no-op
    let lines = screen_lines(&screen);
    for row in 0..6 {
        assert_eq!(lines[row], format!("R{}", row),
            "row {} should be unchanged after DL/IL outside region", row);
    }
}

#[test]
fn dl_clears_wrap_pending() {
    let mut screen = Screen::new(5, 3, 100);
    screen.process(b"ABCDE"); // wrap_pending = true
    assert!(screen.grid.wrap_pending);
    screen.process(b"\x1b[M"); // DL
    assert!(!screen.grid.wrap_pending);
}

#[test]
fn il_clears_wrap_pending() {
    let mut screen = Screen::new(5, 3, 100);
    screen.process(b"ABCDE");
    assert!(screen.grid.wrap_pending);
    screen.process(b"\x1b[L"); // IL
    assert!(!screen.grid.wrap_pending);
}

#[test]
fn erase_display_preserves_cursor_position() {
    for mode in [0u8, 1, 2] {
        let mut screen = Screen::new(20, 10, 100);
        screen.process(b"ABCDEFGHIJ\r\nXYZ");
        screen.process(b"\x1b[3;5H"); // move to row 3, col 5
        let (cx, cy) = (screen.grid.cursor_x, screen.grid.cursor_y);
        let seq = format!("\x1b[{}J", mode);
        screen.process(seq.as_bytes()); // ED 0/1/2
        assert_eq!(screen.grid.cursor_x, cx,
            "ED {} should preserve cursor_x", mode);
        assert_eq!(screen.grid.cursor_y, cy,
            "ED {} should preserve cursor_y", mode);
    }
}

#[test]
fn erase_line_preserves_cursor_position() {
    for mode in [0u8, 1, 2] {
        let mut screen = Screen::new(20, 10, 100);
        screen.process(b"ABCDEFGHIJ\r\nXYZ");
        screen.process(b"\x1b[2;7H"); // move to row 2, col 7
        let (cx, cy) = (screen.grid.cursor_x, screen.grid.cursor_y);
        let seq = format!("\x1b[{}K", mode);
        screen.process(seq.as_bytes()); // EL 0/1/2
        assert_eq!(screen.grid.cursor_x, cx,
            "EL {} should preserve cursor_x", mode);
        assert_eq!(screen.grid.cursor_y, cy,
            "EL {} should preserve cursor_y", mode);
    }
}

#[test]
fn rep_repeats_wide_char() {
    let mut screen = Screen::new(20, 3, 100);
    // Print '世' (width 2)
    screen.process("世".as_bytes());
    assert_eq!(screen.grid.visible_row(0)[0].c, '世');
    assert_eq!(screen.grid.visible_row(0)[0].width, 2);
    // REP: repeat 2 more times (CSI 2 b)
    screen.process(b"\x1b[2b");
    // Should have 3 wide chars occupying 6 cells
    assert_eq!(screen.grid.visible_row(0)[0].c, '世');
    assert_eq!(screen.grid.visible_row(0)[0].width, 2);
    assert_eq!(screen.grid.visible_row(0)[2].c, '世');
    assert_eq!(screen.grid.visible_row(0)[2].width, 2);
    assert_eq!(screen.grid.visible_row(0)[4].c, '世');
    assert_eq!(screen.grid.visible_row(0)[4].width, 2);
    // Continuation cells
    assert_eq!(screen.grid.visible_row(0)[1].width, 0);
    assert_eq!(screen.grid.visible_row(0)[3].width, 0);
    assert_eq!(screen.grid.visible_row(0)[5].width, 0);
}
