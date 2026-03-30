//! Integration test: verify the public library API works for external consumers.

use retach::screen::{
    Screen, TerminalEmulator, TerminalRenderer, AnsiRenderer,
    CursorShape, MouseEncoding, Charset, ActiveCharset,
};

#[test]
fn headless_terminal_emulator_workflow() {
    let mut screen = Screen::new(40, 10, 100);

    // Feed bytes via trait
    TerminalEmulator::process(&mut screen, b"Hello World\r\n");
    TerminalEmulator::process(&mut screen, b"\x1b[1mBold\x1b[0m Normal");

    // Read state via trait
    assert_eq!(screen.cols(), 40);
    assert_eq!(screen.rows(), 10);

    let rows: Vec<_> = screen.visible_rows().collect();
    assert_eq!(rows[0][0].c, 'H');
    assert_eq!(rows[0][4].c, 'o');

    // Cursor should be after "Normal"
    let (cx, cy) = screen.cursor_position();
    assert_eq!(cy, 1); // second line
    assert!(cx > 0);

    // Style resolution
    let bold_style = screen.resolve_style(rows[1][0].style_id);
    assert!(bold_style.bold);
    let normal_style = screen.resolve_style(rows[1][4].style_id);
    assert!(!normal_style.bold);

    // Resize
    screen.resize(20, 5);
    assert_eq!(screen.cols(), 20);
    assert_eq!(screen.rows(), 5);
}

#[test]
fn ansi_renderer_via_trait() {
    let mut screen = Screen::new(20, 5, 0);
    screen.process(b"Test");

    let mut renderer = AnsiRenderer::new();
    let output = TerminalRenderer::render(&mut renderer, &screen, true);
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("Test"));
}

#[test]
fn dyn_terminal_emulator_works() {
    let mut screen = Screen::new(80, 24, 50);
    let emu: &mut dyn TerminalEmulator = &mut screen;
    emu.process(b"dynamic dispatch");
    assert_eq!(emu.cols(), 80);
    let rows: Vec<_> = emu.visible_rows().collect();
    assert_eq!(rows[0][0].c, 'd');
}

#[test]
fn modes_returns_defaults() {
    let screen = Screen::new(80, 24, 0);
    let modes = screen.modes();
    assert!(modes.autowrap_mode);
    assert!(!modes.bracketed_paste);
    assert!(!modes.cursor_key_mode);
    assert!(!modes.origin_mode);
    assert!(!modes.focus_reporting);
    assert!(!modes.keypad_app_mode);
    assert_eq!(modes.mouse_encoding, MouseEncoding::X10);
    assert_eq!(modes.cursor_shape, CursorShape::Default);
    assert_eq!(modes.g0_charset, Charset::Ascii);
    assert_eq!(modes.g1_charset, Charset::Ascii);
    assert_eq!(modes.active_charset, ActiveCharset::G0);
    assert!(!modes.mouse_modes.click);
    assert!(!modes.mouse_modes.button);
    assert!(!modes.mouse_modes.any);
}

#[test]
fn scroll_region_defaults_to_full_screen() {
    let screen = Screen::new(80, 24, 0);
    assert_eq!(screen.scroll_region(), (0, 23));
}

#[test]
fn cursor_shape_default() {
    let screen = Screen::new(80, 24, 0);
    assert_eq!(screen.cursor_shape(), CursorShape::Default);
}

#[test]
fn take_passthrough_drains() {
    let mut screen = Screen::new(80, 24, 0);
    // BEL is forwarded as passthrough
    screen.process(b"\x07");
    let pt = screen.take_passthrough();
    assert!(!pt.is_empty());
    // Second call returns empty
    let pt2 = screen.take_passthrough();
    assert!(pt2.is_empty());
}

#[test]
fn take_queued_notifications_drains() {
    let mut screen = Screen::new(80, 24, 0);
    // OSC 9 notification
    screen.process(b"\x1b]9;Hello\x07");
    let notifs = screen.take_queued_notifications();
    assert!(!notifs.is_empty());
    // Second call returns empty
    let notifs2 = screen.take_queued_notifications();
    assert!(notifs2.is_empty());
}

#[test]
fn ansi_renderer_trait_emits_modes_title_scroll_region() {
    let mut screen = Screen::new(20, 5, 0);
    // Set title
    screen.process(b"\x1b]2;MyTitle\x07");
    // Set bracketed paste mode
    screen.process(b"\x1b[?2004h");
    // Write content
    screen.process(b"Hello");

    let mut renderer = AnsiRenderer::new();
    let output = TerminalRenderer::render(&mut renderer, &screen, true);
    let text = String::from_utf8_lossy(&output);

    // Should contain content
    assert!(text.contains("Hello"), "output should contain 'Hello'");
    // Should contain title
    assert!(text.contains("MyTitle"), "output should contain title");
    // Should contain scroll region (1;5r for 5-row screen)
    assert!(text.contains("\x1b[1;5r"), "output should contain scroll region");
    // Should contain bracketed paste mode enable
    assert!(text.contains("\x1b[?2004h"), "output should contain bracketed paste mode");
}
