```
               __             __
   ________  _/ /_____ ______/ /
  / ___/ _ \/ __/ __ `/ ___/ __ \
 / /  /  __/ /_/ /_/ / /__/ / / /
/_/   \___/\__/\__,_/\___/_/ /_/

```

# retach

Persistent terminal sessions with native scrollback.

## Problem

Traditional terminal multiplexers (tmux, screen, zellij) intercept your terminal's scrollback buffer. To scroll through output you have to enter a special "copy mode" with different keybindings. This is annoying on a regular desktop and completely unusable on mobile.

## Solution

retach passes completed scrollback lines directly to your terminal's stdout as plain text. Your terminal app handles scrolling natively: touchscreen swipe, trackpad gesture, scroll wheel — whatever works on your device. The daemon keeps a virtual screen (VTE-parsed grid) and a scrollback buffer. On reattach, it replays stored history so you see the full context.

## SSH sessions

retach is especially useful over SSH. If your connection drops, the session keeps running on the remote host. Reconnect with `ssh` and run `retach open work` to pick up right where you left off — full scrollback history and screen state are restored instantly.

## Install

```
cargo install --path .
```

Requires Rust 1.70+. macOS or Linux.

## Usage

```bash
# Create or attach to a session (recommended)
retach open work

# Create a new session (fails if "work" already exists)
retach new work

# Create with auto-generated name
retach new

# Attach to existing session (fails if not found)
retach attach work

# List sessions
retach list

# Kill a session
retach kill work
```

**Detach:** press `Ctrl+\` inside any session.

**Custom scrollback size** (default 10,000 lines, max 1,000,000):

```bash
retach open work --history 50000
```

The server daemon starts automatically on the first `retach open` or `retach new` command.

## How it works

```
Client (retach)            Daemon                     Shell
    |                        |                          |
    |--- Input(keys) ------->|--- write to PTY -------->|
    |                        |                          |
    |<-- ScreenUpdate -------|    Persistent PTY Reader |
    |    (grid + scrollback) |<-- PTY output -----------|
    |<-- History (reattach) -|    (VTE parsed, always)  |
    |                        |                          |
  stdout                  Grid + Scrollback          bash/zsh
(native terminal)         (in memory, always live)
```

**Daemon + client over Unix socket** at `$XDG_RUNTIME_DIR/retach/retach.sock` (fallback: `/tmp/retach-<uid>/retach.sock`).

The daemon spawns a PTY per session with a **persistent reader thread** that runs for the entire session lifetime. This thread continuously reads PTY output and processes it through a VTE state machine, keeping the virtual grid and scrollback buffer up-to-date even when no client is connected. When a client attaches, it receives a fully current screen state immediately.

When a line scrolls off the top of the grid, it is included in the next `ScreenUpdate` as an atomic operation: the cursor is positioned at the bottom of the screen, the scrollback line is written with `\r\n` (triggering a real terminal scroll), and the grid is redrawn — all within a single synchronized-output block to prevent flicker. Periodically (60 FPS cap), the daemon sends incremental `ScreenUpdate` messages with only the changed rows.

On reattach, the daemon sends the stored scrollback history as `History` messages (the client writes each line to stdout with `\r\n`, letting the native terminal scroll them into its scrollback buffer), then sends a full `ScreenUpdate` to redraw the visible area.

**Alt screen** (vim, less, htop, etc.) is handled separately: scrollback passthrough is paused while the child process uses the alternate screen buffer. When the child exits alt screen, the main grid is restored.

### Modules

| Module | Purpose |
|--------|---------|
| `client/` | Connects to daemon, raw terminal mode, stdin/stdout I/O, SIGWINCH handling |
| `server/` | Unix socket listener, per-client handler, screen↔client bridge |
| `protocol/` | Binary message encoding (bincode with size limits, length-prefixed), message types |
| `screen/` | VTE parser, virtual grid, cell/style representation, ANSI rendering |
| `session.rs` | Session (PTY + screen + metadata), session manager, persistent PTY reader |
| `pty.rs` | PTY allocation and process spawning via `portable-pty` |

## Logging

retach uses `tracing` with the `RUST_LOG` env variable:

```bash
RUST_LOG=retach=debug retach open work
RUST_LOG=retach=trace retach open work
```

Server logs go to `$XDG_RUNTIME_DIR/retach/retach.log` (fallback: `/tmp/retach-<uid>/retach.log`).

## Supported terminal features

- Unicode and wide characters (CJK)
- 256-color and RGB color
- Bold, dim, italic, underline (single/double/curly/dotted/dashed), blink, inverse, strikethrough, hidden
- DEC line drawing charset
- Alternate screen buffer (save/restore)
- Scroll regions (DECSTBM)
- Cursor shapes (DECSCUSR)
- Bracketed paste mode
- Mouse reporting (1000/1002/1003, SGR encoding)
- Focus reporting
- Synchronized output (mode 2026)
- Window title passthrough (OSC 0/2)
- Device status reports (DSR, DA1, DA2)

## Limitations

- No panes or splits
- No status bar
- No configuration file
- Single-user (Unix socket permissions, not multi-tenant)

## License

BSD-2-Clause
