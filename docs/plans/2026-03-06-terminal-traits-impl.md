# Terminal Traits Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add `TerminalEmulator` and `TerminalRenderer` traits to retach, implement them for `Screen` and a new `AnsiRenderer`, make the `screen` module public via `lib.rs`, and widen visibility of data types so external crates can use them.

**Architecture:** Two traits in a new `src/screen/traits.rs` file. `Screen` implements `TerminalEmulator`. A new `AnsiRenderer` struct (wrapping the existing `RenderCache`) implements `TerminalRenderer`. `StyleId::index()` and `StyleId::is_default()` become public. A `lib.rs` re-exports the screen module. Internal consumers (`session_bridge.rs`) continue calling Screen methods directly — the traits are an additional API layer, not a forced migration.

**Tech Stack:** Rust, existing `vte` crate, no new dependencies.

---

### Task 1: Create `src/screen/traits.rs` with trait definitions

**Files:**
- Create: `src/screen/traits.rs`

**Step 1: Write the trait definitions file**

```rust
//! Trait abstractions for terminal emulation and rendering.

use super::cell::Row;
use super::style::{Style, StyleId};

/// A terminal emulator that processes byte streams and maintains a cell grid.
///
/// This is the primary abstraction for headless terminal emulation.
/// Feed bytes via [`process`](Self::process), then read the grid state
/// via row iterators, cursor position, and style resolution.
pub trait TerminalEmulator {
    /// Feed raw bytes (from SSH, PTY, etc.) through the VTE parser.
    fn process(&mut self, bytes: &[u8]);

    /// Resize the terminal grid to new dimensions.
    fn resize(&mut self, cols: u16, rows: u16);

    /// Number of columns in the terminal grid.
    fn cols(&self) -> u16;

    /// Number of visible rows in the terminal grid.
    fn rows(&self) -> u16;

    /// Iterate over visible rows (the current screen content).
    fn visible_rows(&self) -> Box<dyn Iterator<Item = &Row> + '_>;

    /// Iterate over scrollback rows (history above the visible screen).
    fn scrollback_rows(&self) -> Box<dyn Iterator<Item = &Row> + '_>;

    /// Number of scrollback rows currently stored.
    fn scrollback_len(&self) -> usize;

    /// Current cursor position as `(x, y)`, both 0-based.
    fn cursor_position(&self) -> (u16, u16);

    /// Whether the cursor is currently visible (DECTCEM).
    fn cursor_visible(&self) -> bool;

    /// Resolve a cell's interned style ID to a full [`Style`].
    fn resolve_style(&self, id: StyleId) -> Style;

    /// Whether the terminal is in alternate screen mode (e.g. vim, htop).
    fn in_alt_screen(&self) -> bool;

    /// Take pending responses that should be written back to the PTY/SSH stdin
    /// (e.g. DA, DSR query replies).
    fn take_responses(&mut self) -> Vec<Vec<u8>>;

    /// Current window title (set by OSC 0/2).
    fn title(&self) -> &str;
}

/// A rendering strategy that produces output from terminal emulator state.
///
/// The associated `Output` type allows different renderers to produce
/// different formats: `Vec<u8>` for ANSI sequences, `()` for direct
/// widget painting, or a custom draw command list.
pub trait TerminalRenderer {
    /// The output type produced by rendering.
    type Output;

    /// Render the current emulator state.
    ///
    /// When `full` is true, perform a complete redraw ignoring any cached state.
    /// When false, perform an incremental update based on what changed.
    fn render(&mut self, emulator: &dyn TerminalEmulator, full: bool) -> Self::Output;
}
```

**Step 2: Register the module in `src/screen/mod.rs`**

Add after the existing module declarations (line 8):

```rust
pub mod traits;
```

**Step 3: Run `cargo check` to verify compilation**

Run: `cargo check`
Expected: compiles with no errors

**Step 4: Commit**

```
git add src/screen/traits.rs src/screen/mod.rs
git commit -m "feat: add TerminalEmulator and TerminalRenderer trait definitions"
```

---

### Task 2: Widen visibility of `StyleId` methods and `Row::len()`

**Files:**
- Modify: `src/screen/style.rs:8-10` (StyleId methods)
- Modify: `src/screen/cell.rs:115-119` (Row::len)

External consumers need `StyleId::index()` and `StyleId::is_default()` to inspect cells,
and `Row::len()` to know row width.

**Step 1: Make `StyleId::index()` and `StyleId::is_default()` public**

In `src/screen/style.rs`, change lines 8-10:

Old:
```rust
    pub(super) fn index(self) -> usize { self.0 as usize }
    /// True when this is the default style (index 0).
    pub(super) fn is_default(self) -> bool { self.0 == 0 }
```

New:
```rust
    pub fn index(self) -> usize { self.0 as usize }
    /// True when this is the default style (index 0).
    pub fn is_default(self) -> bool { self.0 == 0 }
```

**Step 2: Make `Row::len()` unconditionally public**

In `src/screen/cell.rs`, change lines 114-119:

Old:
```rust
    /// Number of cells in this row.
    #[cfg(test)]
    #[inline]
    pub fn len(&self) -> usize {
```

New:
```rust
    /// Number of cells in this row.
    #[inline]
    pub fn len(&self) -> usize {
```

**Step 3: Run `cargo check`**

Run: `cargo check`
Expected: compiles with no errors

**Step 4: Commit**

```
git add src/screen/style.rs src/screen/cell.rs
git commit -m "feat: make StyleId methods and Row::len() public for library consumers"
```

---

### Task 3: Implement `TerminalEmulator` for `Screen`

**Files:**
- Modify: `src/screen/mod.rs` (add trait import and impl block)

**Step 1: Write the failing test**

Add at the bottom of `src/screen/mod.rs`, before the existing test module declarations:

```rust
#[cfg(test)]
mod tests_traits {
    use super::*;
    use super::traits::TerminalEmulator;

    #[test]
    fn screen_implements_terminal_emulator() {
        let mut screen = Screen::new(80, 24, 100);

        // Test process + visible_rows
        TerminalEmulator::process(&mut screen, b"Hello");
        let rows: Vec<&cell::Row> = TerminalEmulator::visible_rows(&screen).collect();
        assert_eq!(rows.len(), 24);
        assert_eq!(rows[0][0].c, 'H');
        assert_eq!(rows[0][4].c, 'o');

        // Test dimensions
        assert_eq!(TerminalEmulator::cols(&screen), 80);
        assert_eq!(TerminalEmulator::rows(&screen), 24);

        // Test cursor
        assert_eq!(TerminalEmulator::cursor_position(&screen), (5, 0));
        assert!(TerminalEmulator::cursor_visible(&screen));

        // Test resolve_style
        let style = TerminalEmulator::resolve_style(&screen, rows[0][0].style_id);
        assert!(style.is_default());

        // Test alt screen
        assert!(!TerminalEmulator::in_alt_screen(&screen));

        // Test title
        assert_eq!(TerminalEmulator::title(&screen), "");

        // Test scrollback
        assert_eq!(TerminalEmulator::scrollback_len(&screen), 0);
        assert_eq!(TerminalEmulator::scrollback_rows(&screen).count(), 0);

        // Test take_responses
        assert!(TerminalEmulator::take_responses(&mut screen).is_empty());
    }

    #[test]
    fn screen_as_dyn_terminal_emulator() {
        let mut screen = Screen::new(40, 10, 50);
        let emu: &mut dyn TerminalEmulator = &mut screen;
        emu.process(b"test");
        assert_eq!(emu.cols(), 40);
        assert_eq!(emu.rows(), 10);
        let rows: Vec<_> = emu.visible_rows().collect();
        assert_eq!(rows[0][0].c, 't');
    }
}
```

**Step 2: Run the test to verify it fails**

Run: `cargo test tests_traits -- --no-capture`
Expected: FAIL — `TerminalEmulator` is not implemented for `Screen`

**Step 3: Add the trait implementation**

In `src/screen/mod.rs`, add this import at the top (after the existing use statements, around line 17):

```rust
pub use traits::{TerminalEmulator, TerminalRenderer};
```

Then add the impl block after the existing `impl Screen { ... }` block (after line 258, before `compact_styles`):

```rust
impl traits::TerminalEmulator for Screen {
    fn process(&mut self, bytes: &[u8]) {
        self.process(bytes);
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.resize(cols, rows);
    }

    fn cols(&self) -> u16 {
        self.grid.cols()
    }

    fn rows(&self) -> u16 {
        self.grid.rows()
    }

    fn visible_rows(&self) -> Box<dyn Iterator<Item = &cell::Row> + '_> {
        Box::new(self.grid.visible_rows())
    }

    fn scrollback_rows(&self) -> Box<dyn Iterator<Item = &cell::Row> + '_> {
        Box::new(self.grid.scrollback_rows())
    }

    fn scrollback_len(&self) -> usize {
        self.grid.scrollback_len()
    }

    fn cursor_position(&self) -> (u16, u16) {
        self.grid.cursor_pos()
    }

    fn cursor_visible(&self) -> bool {
        self.grid.cursor_visible()
    }

    fn resolve_style(&self, id: style::StyleId) -> style::Style {
        self.grid.style_table().get(id)
    }

    fn in_alt_screen(&self) -> bool {
        self.state.in_alt_screen
    }

    fn take_responses(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.state.pending_responses)
    }

    fn title(&self) -> &str {
        &self.state.title
    }
}
```

**Step 4: Run the test to verify it passes**

Run: `cargo test tests_traits -- --no-capture`
Expected: PASS — both tests pass

**Step 5: Run the full test suite**

Run: `cargo test`
Expected: all existing tests still pass

**Step 6: Commit**

```
git add src/screen/mod.rs
git commit -m "feat: implement TerminalEmulator trait for Screen"
```

---

### Task 4: Create `AnsiRenderer` and implement `TerminalRenderer`

**Files:**
- Modify: `src/screen/render.rs` (add AnsiRenderer struct + impl)
- Modify: `src/screen/mod.rs` (re-export AnsiRenderer)

The existing `render_screen` function reads Grid directly. The `TerminalRenderer` trait
takes `&dyn TerminalEmulator`. We need a new function that works through the trait,
OR we can have `AnsiRenderer` keep an internal approach. Since the existing render logic
is tightly coupled to Grid internals (hash-based dirty tracking, mode deltas, scroll regions),
the simplest approach is: `AnsiRenderer` wraps the existing render, and the `render` method
on it calls through to `Screen` methods directly.

However, the trait signature is `fn render(&mut self, emulator: &dyn TerminalEmulator, full: bool)`.
The ANSI renderer needs `Grid` internals (mode delta, row hashing) that aren't exposed through
`TerminalEmulator`. Two options:

**Option chosen:** `AnsiRenderer` provides a concrete `render_screen` method that takes `&Screen`
directly (current behavior), PLUS implements `TerminalRenderer` with a simpler render that
only uses trait methods (full redraw each time — no dirty tracking). This way the existing
retach server path stays optimal, while the trait path works for any `TerminalEmulator`.

**Step 1: Write the failing test**

Add to `src/screen/mod.rs` in `tests_traits`:

```rust
    #[test]
    fn ansi_renderer_implements_terminal_renderer() {
        use super::render::AnsiRenderer;
        use super::traits::TerminalRenderer;

        let mut screen = Screen::new(10, 3, 0);
        screen.process(b"Hi");

        let mut renderer = AnsiRenderer::new();
        let output = renderer.render(&screen, true);
        // Output should contain "Hi" somewhere
        let text = String::from_utf8_lossy(&output);
        assert!(text.contains("Hi"), "render output should contain 'Hi', got: {text}");
    }
```

**Step 2: Run the test to verify it fails**

Run: `cargo test tests_traits::ansi_renderer_implements_terminal_renderer -- --no-capture`
Expected: FAIL — `AnsiRenderer` doesn't exist

**Step 3: Create `AnsiRenderer` in `src/screen/render.rs`**

Add at the bottom of render.rs (before any `#[cfg(test)]` block):

```rust
/// ANSI escape sequence renderer.
///
/// Renders terminal emulator state as ANSI escape sequences suitable
/// for output to a real terminal. Uses dirty-tracking for incremental updates.
pub struct AnsiRenderer {
    cache: RenderCache,
}

impl AnsiRenderer {
    /// Create a new ANSI renderer.
    pub fn new() -> Self {
        Self { cache: RenderCache::new() }
    }

    /// Render a Screen directly (optimized path using Grid internals).
    ///
    /// This is the fast path used by retach's server, with row-hash dirty
    /// tracking and mode delta encoding.
    pub fn render_screen(&mut self, grid: &super::grid::Grid, title: &str, full: bool) -> Vec<u8> {
        render_screen(grid, title, full, &mut self.cache)
    }

    /// Render with scrollback lines prepended in a synchronized block.
    pub fn render_screen_with_scrollback(
        &mut self,
        grid: &super::grid::Grid,
        title: &str,
        scrollback: &[Vec<u8>],
    ) -> Vec<u8> {
        render_screen_with_scrollback(grid, title, scrollback, &mut self.cache)
    }

    /// Invalidate the cache, forcing a full redraw on next render.
    pub fn invalidate(&mut self) {
        self.cache.invalidate();
    }
}
```

**Step 4: Implement `TerminalRenderer` for `AnsiRenderer`**

Add below the `AnsiRenderer` impl block:

```rust
impl super::traits::TerminalRenderer for AnsiRenderer {
    type Output = Vec<u8>;

    fn render(&mut self, emulator: &dyn super::traits::TerminalEmulator, full: bool) -> Vec<u8> {
        // Build output using trait methods (no Grid internals).
        // This path always does a full render — no dirty tracking.
        let mut out = Vec::with_capacity(4096);

        let cols = emulator.cols();
        let rows = emulator.rows();

        // Begin synchronized update
        out.extend_from_slice(b"\x1b[?2026h");

        // Hide cursor during render
        out.extend_from_slice(b"\x1b[?25l");

        if full {
            // Clear screen
            out.extend_from_slice(b"\x1b[H\x1b[2J");
        }

        let mut prev_style = Style::default();

        for (y, row) in emulator.visible_rows().enumerate() {
            // Move cursor to start of row
            out.extend_from_slice(b"\x1b[");
            write_u16(&mut out, (y as u16) + 1);
            out.extend_from_slice(b";1H");

            // Erase line
            out.extend_from_slice(b"\x1b[2K");

            for cell in row.iter() {
                let style = emulator.resolve_style(cell.style_id);
                if style != prev_style {
                    out.extend_from_slice(&style.to_sgr_with_reset());
                    prev_style = style;
                }
                if cell.width == 0 {
                    continue; // wide char continuation
                }
                let mut buf = [0u8; 4];
                let s = cell.c.encode_utf8(&mut buf);
                out.extend_from_slice(s.as_bytes());

                // Combining marks
                for &mark in row.combining(y as u16) {
                    let s = mark.encode_utf8(&mut buf);
                    out.extend_from_slice(s.as_bytes());
                }
            }
        }

        // Reset style
        if prev_style != Style::default() {
            out.extend_from_slice(b"\x1b[0m");
        }

        // Position cursor
        let (cx, cy) = emulator.cursor_position();
        out.extend_from_slice(b"\x1b[");
        write_u16(&mut out, cy + 1);
        out.push(b';');
        write_u16(&mut out, cx + 1);
        out.push(b'H');

        // Restore cursor visibility
        if emulator.cursor_visible() {
            out.extend_from_slice(b"\x1b[?25h");
        }

        // End synchronized update
        out.extend_from_slice(b"\x1b[?2026l");

        let _ = full; // dirty tracking not used in trait path
        out
    }
}
```

Note: `use super::style::{Style, write_u16};` should already be available via existing
imports at the top of render.rs. If `Style` is not imported, add it.

**Step 5: Re-export `AnsiRenderer` from `mod.rs`**

In `src/screen/mod.rs`, add to the existing re-exports:

```rust
pub use render::AnsiRenderer;
```

**Step 6: Run the test**

Run: `cargo test tests_traits -- --no-capture`
Expected: PASS

**Step 7: Run the full test suite**

Run: `cargo test`
Expected: all tests pass

**Step 8: Commit**

```
git add src/screen/render.rs src/screen/mod.rs
git commit -m "feat: add AnsiRenderer implementing TerminalRenderer trait"
```

---

### Task 5: Migrate Screen's render methods to use AnsiRenderer internally

**Files:**
- Modify: `src/screen/mod.rs` (Screen methods delegate to AnsiRenderer where feasible)

The existing `Screen::render()`, `Screen::render_with_scrollback()`, and `Screen::take_and_render()`
take a `&mut RenderCache` from the caller. These stay as-is since they're used by session_bridge.
No migration needed — the old API and new trait API coexist.

This task just verifies that `RenderCache` is still usable by session_bridge and that
`AnsiRenderer` is a separate path.

**Step 1: Verify session_bridge still compiles**

Run: `cargo check`
Expected: compiles with no errors (session_bridge uses Screen methods and RenderCache directly)

**Step 2: Commit (no-op if nothing changed)**

Skip commit if no changes are needed.

---

### Task 6: Create `src/lib.rs` and make screen module public

**Files:**
- Create: `src/lib.rs`
- Modify: `src/screen/mod.rs` (widen module visibility)

**Step 1: Create `src/lib.rs`**

```rust
//! retach — terminal multiplexer with native scrollback passthrough.
//!
//! This crate provides a headless terminal emulator ([`screen::Screen`])
//! that can be used as a library for building terminal-aware applications.
//!
//! # Example
//!
//! ```rust
//! use retach::screen::{Screen, TerminalEmulator};
//!
//! let mut screen = Screen::new(80, 24, 1000);
//! screen.process(b"Hello \x1b[1mWorld\x1b[0m");
//!
//! for row in screen.visible_rows() {
//!     for cell in row.iter() {
//!         let style = screen.resolve_style(cell.style_id);
//!         // render cell.c with style
//!     }
//! }
//! ```

pub mod screen;
```

**Step 2: Widen module visibility in `src/screen/mod.rs`**

Change the module declarations from `pub(crate)` / `pub(super)` to `pub`:

Old (lines 4-8):
```rust
pub(crate) mod style;
pub(crate) mod cell;
pub(crate) mod grid;
pub(super) mod performer;
pub(super) mod render;
```

New:
```rust
pub mod style;
pub mod cell;
pub mod grid;
pub(crate) mod performer;
pub(crate) mod render;
```

`performer` stays `pub(crate)` because it's an internal implementation detail
(VTE Perform impl). `render` stays `pub(crate)` because external consumers
use `AnsiRenderer` via the re-export, not the module directly.

Also ensure the public re-exports at the top of mod.rs include all needed types:

```rust
pub use cell::{Cell, Row};
pub use style::{Style, StyleId, Color, UnderlineStyle, StyleTable};
pub use grid::{Grid, CursorShape, TerminalSize};
pub use render::AnsiRenderer;
pub use traits::{TerminalEmulator, TerminalRenderer};
```

**Step 3: Update `main.rs` to use lib crate path**

In `src/main.rs`, change:

Old (lines 6-12):
```rust
mod cli;
mod client;
mod protocol;
mod pty;
mod screen;
mod server;
mod session;
```

New:
```rust
mod cli;
mod client;
mod protocol;
mod pty;
mod server;
mod session;

// screen module is provided by the library crate
use retach::screen;
```

**Step 4: Run `cargo check`**

Run: `cargo check`
Expected: compiles. If there are visibility errors from main.rs accessing
`screen` internals, they need to be addressed — performer and render
sub-modules should be `pub(crate)` to remain accessible from the binary.

**Important:** `main.rs` doesn't access screen internals directly — only
`session_bridge.rs` does, and it's already in the same crate. The `pub(crate)`
visibility on performer/render should suffice.

If `cargo check` fails because `main.rs` `mod screen;` conflicts with `lib.rs`
`pub mod screen;`, the fix is simply removing `mod screen;` from main.rs
(already done above) since the lib crate provides it.

**Step 5: Run the full test suite**

Run: `cargo test`
Expected: all tests pass

**Step 6: Commit**

```
git add src/lib.rs src/main.rs src/screen/mod.rs
git commit -m "feat: expose screen module as library via lib.rs"
```

---

### Task 7: Add doc-test and integration test

**Files:**
- Create: `tests/library_api.rs`

**Step 1: Write an integration test exercising the public library API**

```rust
//! Integration test: verify the public library API works for external consumers.

use retach::screen::{Screen, TerminalEmulator, TerminalRenderer, AnsiRenderer};

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
```

**Step 2: Run the integration test**

Run: `cargo test --test library_api`
Expected: PASS

**Step 3: Run the full test suite including doc-tests**

Run: `cargo test`
Expected: all tests pass, including the doc example in lib.rs

**Step 4: Commit**

```
git add tests/library_api.rs
git commit -m "test: add integration tests for public library API"
```
