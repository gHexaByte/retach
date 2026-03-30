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
