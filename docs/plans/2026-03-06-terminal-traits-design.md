# Terminal Traits Design

## Goal

Expose retach's terminal emulation as a library API via traits, enabling external
consumers (e.g. an SSH client with native UI) to use retach's Screen as a headless
terminal emulator and plug in their own rendering strategy.

## Approach

Rust-idiomatic: traits only at boundaries where implementations can realistically
be swapped. Internal data types (Cell, Row, Style, StyleId) remain concrete structs.

Two traits, living in `src/screen/traits.rs`:

## Trait: TerminalEmulator

Abstraction over Screen — the terminal state machine that ingests bytes and
maintains a cell grid.

```rust
pub trait TerminalEmulator {
    /// Feed raw bytes (from SSH/PTY) through the VTE parser.
    fn process(&mut self, bytes: &[u8]);

    /// Resize the terminal grid.
    fn resize(&mut self, cols: u16, rows: u16);

    /// Terminal dimensions.
    fn cols(&self) -> u16;
    fn rows(&self) -> u16;

    /// Iterate visible rows (the current screen).
    fn visible_rows(&self) -> Box<dyn Iterator<Item = &Row> + '_>;

    /// Iterate scrollback rows (history above the screen).
    fn scrollback_rows(&self) -> Box<dyn Iterator<Item = &Row> + '_>;
    fn scrollback_len(&self) -> usize;

    /// Cursor state.
    fn cursor_position(&self) -> (u16, u16);
    fn cursor_visible(&self) -> bool;

    /// Resolve a cell's style_id to a full Style.
    fn resolve_style(&self, id: StyleId) -> Style;

    /// Whether the terminal is in alt screen mode (vim, htop, etc.)
    fn in_alt_screen(&self) -> bool;

    /// Responses to send back to PTY/SSH (DA, DSR queries).
    fn take_responses(&mut self) -> Vec<Vec<u8>>;

    /// Window title (OSC 0/2).
    fn title(&self) -> &str;
}
```

Screen implements this trait. Consumers depend on `&dyn TerminalEmulator` or
`impl TerminalEmulator`.

## Trait: TerminalRenderer

Abstraction over rendering strategy. Associated type for output format.

```rust
pub trait TerminalRenderer {
    /// The output type produced by rendering.
    /// ANSI renderer: Vec<u8>
    /// Native UI renderer: () or a draw command list
    type Output;

    /// Render the current emulator state.
    /// `full` = true for complete redraw, false for incremental.
    fn render(&mut self, emulator: &dyn TerminalEmulator, full: bool) -> Self::Output;
}
```

Current ANSI renderer (RenderCache + render logic) becomes `AnsiRenderer`
implementing this trait.

## Concrete Types (no traits)

These are data objects — no behavioral polymorphism needed:

- `Cell` — 8-byte struct: char + style_id + width
- `Row` — Vec<Cell> + combining marks, Deref<Target=[Cell]>
- `Style` — SGR attributes (bold, italic, colors, etc.)
- `StyleId` — interned style table index
- `Color`, `UnderlineStyle` — enums

These must be `pub` for external consumers to read cell data.

## File Organization

- `src/screen/traits.rs` — trait definitions
- `src/screen/mod.rs` — `impl TerminalEmulator for Screen`
- `src/screen/render.rs` — `AnsiRenderer` struct + `impl TerminalRenderer`
- Existing files otherwise unchanged

## Library Exposure

- Add `src/lib.rs` with `pub mod screen`
- `Cargo.toml` gets both `[[bin]]` and `[lib]` targets
- `main.rs` uses `retach::screen::Screen` etc.

## Usage Example

```rust
use retach::screen::{Screen, TerminalEmulator, Style};

let mut screen = Screen::new(80, 24, 1000);
screen.process(b"Hello \x1b[1mBold\x1b[0m World");

for row in screen.visible_rows() {
    for cell in row.iter() {
        let style: Style = screen.resolve_style(cell.style_id);
        // draw cell.c with style using native widgets
    }
}

let (cx, cy) = screen.cursor_position();
```
