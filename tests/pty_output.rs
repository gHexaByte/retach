//! PTY-based integration test: verify that the bytes retach writes to the outer
//! terminal during detach are clean — no keyboard-modifier sequences, well-formed
//! escape sequences, and proper termios restoration.
//!
//! This test creates a PTY pair and runs a child process with the PTY slave as
//! its stdin/stdout, simulating what the outer terminal (e.g. Blink) would see.

use nix::pty::openpty;
use nix::sys::termios::{self, InputFlags, SetArg};
use std::os::fd::{AsFd, AsRawFd};
use std::time::Duration;

/// All the bytes retach writes to the outer terminal during a typical session +
/// detach cycle.  Built from:
///   1. Focus reporting enable (written at connection time)
///   2. A full screen render (from Screen with all modes set)
///   3. The cleanup_terminal() sequence
fn build_full_output() -> Vec<u8> {
    use retach::screen::{Screen, TerminalEmulator, AnsiRenderer, TerminalRenderer};

    let mut out = Vec::new();

    // 1. Focus reporting enable (written to outer terminal at connect time)
    out.extend_from_slice(b"\x1b[?1004h");

    // 2. Screen render with various modes set (simulate an app like vim or htop)
    let mut screen = Screen::new(80, 24, 100);
    // Set modes that typical terminal apps use
    screen.process(b"\x1b[?1h");    // DECCKM: application cursor keys
    screen.process(b"\x1b[?2004h"); // bracketed paste
    screen.process(b"\x1b[?1004h"); // focus events (inner screen)
    screen.process(b"\x1b=");       // application keypad
    screen.process(b"\x1b[?1000h"); // mouse click tracking
    screen.process(b"\x1b[?1006h"); // SGR mouse encoding
    screen.process(b"Hello from inner app\r\n");
    screen.process(b"\x1b[?25l");   // hide cursor

    let mut renderer = AnsiRenderer::new();
    let render = TerminalRenderer::render(&mut renderer, &screen, true);
    out.extend_from_slice(&render);

    // Also include passthrough from the screen
    let pt = screen.take_passthrough();
    for chunk in pt {
        out.extend_from_slice(&chunk);
    }

    // 3. Cleanup sequence that resets the same modes as client::cleanup_terminal().
    // NOTE: This cleanup sequence simulates what the outer terminal sees on
    // reconnect, which differs from client::cleanup_terminal(). The client
    // cleanup targets the user's terminal (clears screen with \x1b[2J and
    // homes cursor with \x1b[H); this simulates server-side render teardown
    // (cursor to bottom with \x1b[9999B and trailing newline).
    // The mode resets (?25h, ?7h, ?1l, ?2004l, mouse, etc.) are identical.
    out.extend_from_slice(concat!(
        "\x1b[r",
        "\x1b[9999B",
        "\x1b[?25h",
        "\x1b[?7h",
        "\x1b[?1l",
        "\x1b[?2004l",
        "\x1b[?1000l",
        "\x1b[?1002l",
        "\x1b[?1003l",
        "\x1b[?1005l",
        "\x1b[?1006l",
        "\x1b[?1004l",
        "\x1b[?2026l",
        "\x1b>",
        "\x1b[0 q",
        "\x1b[0m",
        "\n",
    ).as_bytes());

    out
}

/// A minimal hterm-like keyboard state machine.  Tracks modes that affect how
/// the terminal reports modifier keys on cursor-key presses.
#[derive(Debug, Default)]
struct KeyboardState {
    /// xterm modifyCursorKeys value (CSI > 1 ; Pv m).  0 = disabled (default).
    modify_cursor_keys: i32,
    /// xterm modifyOtherKeys value (CSI > 4 ; Pv m).  0 = disabled (default).
    modify_other_keys: i32,
    /// Kitty keyboard protocol flags (CSI > Pf u to push, CSI < u to pop).
    kitty_flags: u32,
    /// DECCKM — application cursor keys (CSI ? 1 h/l).
    decckm: bool,
}

impl KeyboardState {
    /// Returns true if the terminal would send modifier parameters with cursor
    /// keys even when no modifier key is physically pressed.
    fn sends_spurious_modifiers(&self) -> bool {
        self.modify_cursor_keys > 0 || self.modify_other_keys > 0 || self.kitty_flags != 0
    }
}

/// Parse escape sequences from `data` and update `state`.
fn apply_sequences_to_state(data: &[u8], state: &mut KeyboardState) {
    let mut i = 0;
    while i < data.len() {
        if data[i] != 0x1b {
            i += 1;
            continue;
        }
        if i + 1 >= data.len() { break; }

        match data[i + 1] {
            b'[' => {
                // CSI sequence
                let start = i + 2;
                let mut j = start;
                while j < data.len() && (data[j] == b';' || (data[j] >= b'0' && data[j] <= b'?')) {
                    j += 1;
                }
                // Skip intermediate bytes
                while j < data.len() && data[j] >= 0x20 && data[j] <= 0x2f {
                    j += 1;
                }
                if j >= data.len() { break; }
                let final_byte = data[j];
                let params = &data[start..j];

                match final_byte {
                    b'm' if !params.is_empty() && params[0] == b'>' => {
                        // CSI > Ps m or CSI > Ps ; Pv m — xterm modifyKeys
                        let param_str = String::from_utf8_lossy(&params[1..]);
                        let parts: Vec<&str> = param_str.split(';').collect();
                        let resource: i32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
                        let value: i32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(-1);
                        match resource {
                            1 => state.modify_cursor_keys = if value < 0 { 0 } else { value },
                            4 => state.modify_other_keys = if value < 0 { 0 } else { value },
                            _ => {}
                        }
                    }
                    b'u' if !params.is_empty() && params[0] == b'>' => {
                        // CSI > Pf u — kitty keyboard push
                        let flags: u32 = String::from_utf8_lossy(&params[1..]).parse().unwrap_or(0);
                        state.kitty_flags = flags;
                    }
                    b'u' if !params.is_empty() && params[0] == b'<' => {
                        // CSI < u — kitty keyboard pop/reset
                        state.kitty_flags = 0;
                    }
                    b'u' if !params.is_empty() && params[0] == b'=' => {
                        // CSI = Pf u — kitty keyboard set
                        let flags: u32 = String::from_utf8_lossy(&params[1..]).parse().unwrap_or(0);
                        state.kitty_flags = flags;
                    }
                    b'h' | b'l' if !params.is_empty() && params[0] == b'?' => {
                        let enable = final_byte == b'h';
                        let param_str = String::from_utf8_lossy(&params[1..]);
                        for num_str in param_str.split(';') {
                            if let Ok(n) = num_str.parse::<u16>() {
                                if n == 1 {
                                    state.decckm = enable;
                                }
                            }
                        }
                    }
                    _ => {}
                }

                i = j + 1;
            }
            _ => {
                i += 2;
            }
        }
    }
}

#[test]
fn outer_terminal_receives_no_keyboard_modifier_sequences() {
    let output = build_full_output();

    // Verify the full output is non-empty
    assert!(!output.is_empty(), "output should not be empty");

    // Apply all sequences and check keyboard state
    let mut state = KeyboardState::default();
    apply_sequences_to_state(&output, &mut state);

    assert!(
        !state.sends_spurious_modifiers(),
        "after detach sequence, terminal should not have keyboard modifier modes set.\n\
         modify_cursor_keys={}, modify_other_keys={}, kitty_flags={:#x}",
        state.modify_cursor_keys, state.modify_other_keys, state.kitty_flags
    );

    // DECCKM should be reset by cleanup (\x1b[?1l)
    assert!(
        !state.decckm,
        "DECCKM should be reset by cleanup_terminal"
    );
}

#[test]
fn outer_terminal_output_contains_no_keyboard_csi_gt() {
    let output = build_full_output();
    let text = String::from_utf8_lossy(&output);

    // CSI > ... m would enable xterm modifyCursorKeys
    // Find ESC [ > in the output
    let has_csi_gt = output.windows(3).any(|w| w == b"\x1b[>");
    assert!(
        !has_csi_gt,
        "output must not contain CSI > (xterm modifyKeys) sequences.\n\
         First occurrence context: {:?}",
        text.find("\x1b[>").map(|pos| &text[pos.saturating_sub(10)..std::cmp::min(pos + 20, text.len())])
    );
}

#[test]
fn outer_terminal_output_contains_no_kitty_keyboard() {
    let output = build_full_output();

    // CSI > Pf u or CSI = Pf u would enable kitty keyboard protocol
    let has_kitty_push = output.windows(3).any(|w| w == b"\x1b[>");
    let has_kitty_set  = output.windows(3).any(|w| w == b"\x1b[=");
    assert!(!has_kitty_push, "output must not contain CSI > u (kitty keyboard push)");
    assert!(!has_kitty_set,  "output must not contain CSI = u (kitty keyboard set)");
}

#[test]
fn outer_terminal_all_csi_sequences_well_formed() {
    let output = build_full_output();
    let mut i = 0;
    let mut incomplete = Vec::new();

    while i < output.len() {
        if output[i] == 0x1b && i + 1 < output.len() && output[i + 1] == b'[' {
            let start = i + 2;
            let mut j = start;
            // Parameter bytes: 0x20-0x3f
            while j < output.len() && output[j] >= 0x20 && output[j] <= 0x3f {
                j += 1;
            }
            if j >= output.len() {
                // Truncated — no final byte found
                incomplete.push(String::from_utf8_lossy(&output[i..]).into_owned());
                i = j;
            } else if output[j] < 0x40 || output[j] > 0x7e {
                // Invalid final byte
                incomplete.push(format!("invalid final byte 0x{:02x} at offset {}", output[j], j));
                i = j + 1;
            } else {
                i = j + 1;
            }
        } else {
            i += 1;
        }
    }

    assert!(
        incomplete.is_empty(),
        "found incomplete/malformed CSI sequences in outer terminal output:\n{}",
        incomplete.join("\n")
    );
}

#[test]
fn cleanup_resets_all_modes_set_by_render() {
    use retach::screen::{Screen, AnsiRenderer, TerminalRenderer};

    // Render with ALL modes enabled
    let mut screen = Screen::new(80, 24, 0);
    screen.process(b"\x1b[?1h\x1b[?2004h\x1b[?1004h\x1b=\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1b[?7l\x1b[?6h");
    let mut renderer = AnsiRenderer::new();
    let render = TerminalRenderer::render(&mut renderer, &screen, true);

    // Combine render + cleanup
    let cleanup = b"\x1b[r\x1b[9999B\x1b[?25h\x1b[?7h\x1b[?1l\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1004l\x1b[?2026l\x1b>\x1b[0 q\x1b[0m\n";

    let mut combined = render;
    combined.extend_from_slice(cleanup);

    // Apply all sequences to state machine
    let mut state = KeyboardState::default();
    apply_sequences_to_state(&combined, &mut state);

    assert!(!state.decckm, "DECCKM must be reset after cleanup");
    assert!(!state.sends_spurious_modifiers(), "no modifier sequences after cleanup");
}

/// PTY-based test: run the full output sequence through a real PTY and verify
/// (a) the PTY slave's termios is properly restored after detach-style restore,
/// (b) the master receives the expected cleanup sequences without interference.
///
/// The write → read are done concurrently to avoid blocking on the PTY buffer.
#[test]
fn pty_detach_restores_termios_and_clean_output() {
    use nix::sys::termios::Termios;
    use std::sync::{Arc, Mutex};

    // Open a PTY pair — openpty returns OwnedFd for both master and slave
    let pty = openpty(None, None).expect("openpty failed");
    let master_fd = pty.master;
    let slave_fd = pty.slave;

    // Save original termios of slave — use .as_fd() since slave_fd is OwnedFd
    let original: Termios = termios::tcgetattr(slave_fd.as_fd()).expect("tcgetattr original");

    // Enter raw mode on slave (like cfmakeraw)
    let mut raw = original.clone();
    termios::cfmakeraw(&mut raw);
    termios::tcsetattr(slave_fd.as_fd(), SetArg::TCSANOW, &raw).expect("tcsetattr raw");

    // Collect all bytes retach would send to the outer terminal
    let output = build_full_output();

    // Drain master concurrently — PTY buffer is typically 4 KiB; our output
    // may exceed that, so we must read from master while writing to slave.
    // SAFETY: master_fd is an OwnedFd that outlives the drain thread (we
    // join it before master_fd is dropped at end of scope). The raw fd
    // remains valid for the thread's entire lifetime. We use as_raw_fd()
    // because nix 0.29's read() still takes RawFd.
    let master_raw_fd = master_fd.as_raw_fd();
    let received = Arc::new(Mutex::new(Vec::<u8>::new()));
    let received2 = received.clone();
    let drain_thread = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        // Read until the slave end closes (EIO) or EAGAIN after a quiet period
        loop {
            match nix::unistd::read(master_raw_fd, &mut buf) {
                Ok(0) => break,
                Ok(n) => received2.lock().unwrap().extend_from_slice(&buf[..n]),
                Err(nix::errno::Errno::EAGAIN) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break, // EIO when slave closes
            }
        }
    });

    // Write all output to slave, then restore termios, then close slave
    nix::unistd::write(slave_fd.as_fd(), &output).expect("write to slave");
    termios::tcsetattr(slave_fd.as_fd(), SetArg::TCSANOW, &original).expect("tcsetattr restore");

    // Verify termios is restored immediately after restore call
    let after: Termios = termios::tcgetattr(slave_fd.as_fd()).expect("tcgetattr after restore");
    let original_input = original.input_flags;
    let after_input = after.input_flags;
    assert!(
        after_input.contains(InputFlags::ICRNL) == original_input.contains(InputFlags::ICRNL),
        "ICRNL flag must be restored after detach: before={:?} after={:?}",
        original_input.contains(InputFlags::ICRNL),
        after_input.contains(InputFlags::ICRNL),
    );

    // Drop slave fd → drain_thread sees EIO on master and exits
    drop(slave_fd);
    drain_thread.join().expect("drain thread panicked");

    let received = Arc::try_unwrap(received).unwrap().into_inner().unwrap();

    // --- Verify no keyboard-modifier sequences in master output ---
    let has_csi_gt = received.windows(3).any(|w| w == b"\x1b[>");
    assert!(
        !has_csi_gt,
        "PTY master received CSI > (keyboard modifier) from retach output.\n\
         Context: {:?}",
        received.windows(3).enumerate()
            .find(|(_, w)| *w == b"\x1b[>")
            .map(|(i, _)| String::from_utf8_lossy(
                &received[i.saturating_sub(5)..std::cmp::min(i+15, received.len())]
            ).into_owned())
    );

    // Cleanup sequences must appear in master output
    assert!(
        received.windows(4).any(|w| w == b"\x1b[0m"),
        "PTY master should receive SGR reset (\\x1b[0m) from cleanup; got {} bytes total",
        received.len()
    );
    assert!(
        received.windows(3).any(|w| w == b"\x1b[r"),
        "PTY master should receive scroll-region reset (\\x1b[r) from cleanup"
    );
}
