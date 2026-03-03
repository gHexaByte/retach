use std::hash::{Hash, Hasher};

use super::style::Style;

/// Single character cell in the terminal grid, with style and display width.
#[derive(Clone, Debug)]
pub struct Cell {
    pub c: char,
    /// Combining marks (e.g. diacritics). Supports multiple stacked marks.
    pub combining: Vec<char>,
    pub style: Style,
    /// Display width: 1 for normal, 2 for wide char first cell, 0 for wide char continuation
    pub width: u8,
}

impl Hash for Cell {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.c.hash(state);
        self.combining.hash(state);
        self.style.hash(state);
        self.width.hash(state);
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self { c: ' ', combining: Vec::new(), style: Style::default(), width: 1 }
    }
}
