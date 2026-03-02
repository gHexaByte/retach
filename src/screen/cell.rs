use super::style::Style;

/// Single character cell in the terminal grid, with style and display width.
#[derive(Copy, Clone, Debug, Hash)]
pub struct Cell {
    pub c: char,
    pub style: Style,
    /// Display width: 1 for normal, 2 for wide char first cell, 0 for wide char continuation
    pub width: u8,
}

impl Default for Cell {
    fn default() -> Self {
        Self { c: ' ', style: Style::default(), width: 1 }
    }
}
