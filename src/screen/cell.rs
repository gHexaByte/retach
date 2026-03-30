use std::hash::{Hash, Hasher};
use std::ops::{Deref, DerefMut};

use super::style::StyleId;

/// Single character cell in the terminal grid, with style and display width.
/// Combining marks are stored at the Row level (>99.99% of cells have none).
#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub c: char,
    /// Interned style ID — look up via StyleTable.
    pub style_id: StyleId,
    /// Display width: 1 for normal, 2 for wide char first cell, 0 for wide char continuation
    pub width: u8,
}

impl Cell {
    /// Creates a new cell.
    #[inline]
    pub fn new(c: char, style_id: StyleId, width: u8) -> Self {
        Self { c, style_id, width }
    }
}

impl Hash for Cell {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.c.hash(state);
        self.style_id.hash(state);
        self.width.hash(state);
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self { c: ' ', style_id: StyleId::default(), width: 1 }
    }
}

/// A terminal row: a vector of cells plus sparse combining marks storage.
/// Combining marks (diacritics, etc.) are stored separately because >99.99%
/// of cells never have them. This keeps Cell at 8 bytes instead of 24.
#[derive(Clone, Debug)]
pub struct Row {
    cells: Vec<Cell>,
    /// Combining marks per column. Empty in the vast majority of rows.
    combining: Vec<(u16, Vec<char>)>,
}

impl Row {
    /// Create a new row with `cols` default cells.
    pub fn new(cols: usize) -> Self {
        Self {
            cells: vec![Cell::default(); cols],
            combining: Vec::new(),
        }
    }

    /// Create a Row from an existing Vec<Cell> (for migration).
    pub fn from_cells(cells: Vec<Cell>) -> Self {
        Self {
            cells,
            combining: Vec::new(),
        }
    }

    /// Returns combining marks for the given column (empty if none).
    #[inline]
    pub fn combining(&self, col: u16) -> &[char] {
        for &(c, ref marks) in &self.combining {
            if c == col {
                return marks;
            }
        }
        &[]
    }

    /// Push a combining mark onto the given column.
    pub fn push_combining(&mut self, col: u16, mark: char) {
        for &mut (c, ref mut marks) in &mut self.combining {
            if c == col {
                marks.push(mark);
                return;
            }
        }
        self.combining.push((col, vec![mark]));
    }

    /// Number of combining marks on the given column.
    #[inline]
    pub fn combining_len(&self, col: u16) -> usize {
        for &(c, ref marks) in &self.combining {
            if c == col {
                return marks.len();
            }
        }
        0
    }

    /// Clear combining marks for the given column.
    pub fn clear_combining(&mut self, col: u16) {
        self.combining.retain(|&(c, _)| c != col);
    }

    /// Clear all combining marks for this row.
    pub fn clear_all_combining(&mut self) {
        self.combining.clear();
    }

    /// Clear combining marks for columns in the range [from, to).
    pub fn clear_combining_range(&mut self, from: u16, to: u16) {
        self.combining.retain(|&(c, _)| c < from || c >= to);
    }

    /// Number of cells in this row.
    #[inline]
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// Whether this row has no cells.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Resize the row to `new_len` cells, filling with `value`.
    pub fn resize(&mut self, new_len: usize, value: Cell) {
        if new_len < self.cells.len() {
            // Remove combining marks for truncated columns
            let limit = new_len as u16;
            self.combining.retain(|&(c, _)| c < limit);
        }
        self.cells.resize(new_len, value);
    }

    /// Remove a cell at index, shifting subsequent cells left.
    /// Adjusts combining mark column indices accordingly.
    pub fn remove(&mut self, index: usize) -> Cell {
        let col = index as u16;
        // Remove combining marks for the deleted column
        self.combining.retain(|&(c, _)| c != col);
        // Shift combining marks for columns after the deleted one
        for &mut (ref mut c, _) in &mut self.combining {
            if *c > col {
                *c -= 1;
            }
        }
        self.cells.remove(index)
    }

    /// Insert a cell at index, shifting subsequent cells right.
    /// Adjusts combining mark column indices accordingly.
    pub fn insert(&mut self, index: usize, cell: Cell) {
        let col = index as u16;
        // Shift combining marks for columns at or after the insertion point
        for &mut (ref mut c, _) in &mut self.combining {
            if *c >= col {
                *c = c.saturating_add(1);
            }
        }
        self.cells.insert(index, cell);
    }

    /// Push a cell at the end.
    pub fn push(&mut self, cell: Cell) {
        self.cells.push(cell);
    }

    /// Clean up orphaned wide-char halves at a column boundary during resize/restore.
    /// `new_cols` is the number of columns the row will be truncated to.
    /// Must be called BEFORE `resize()` when the row is longer than `new_cols`.
    pub fn fix_wide_char_orphan_at_boundary(&mut self, new_cols: usize) {
        if new_cols == 0 || self.cells.len() <= new_cols {
            return;
        }
        let last = new_cols - 1;
        if self.cells[last].width == 2 {
            self.cells[last] = Cell::default();
        } else if last > 0 && self.cells[last].width == 0 {
            self.cells[last] = Cell::default();
            self.cells[last - 1] = Cell::default();
        }
    }

    /// Pop the last cell.
    pub fn pop(&mut self) -> Option<Cell> {
        if let Some(cell) = self.cells.pop() {
            let col = self.cells.len() as u16;
            self.combining.retain(|&(c, _)| c != col);
            Some(cell)
        } else {
            None
        }
    }

    /// Iterate over cells.
    pub fn iter(&self) -> std::slice::Iter<'_, Cell> {
        self.cells.iter()
    }

    /// Mutably iterate over cells.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, Cell> {
        self.cells.iter_mut()
    }
}

impl Hash for Row {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.cells.hash(state);
        self.combining.hash(state);
    }
}

impl Deref for Row {
    type Target = [Cell];

    fn deref(&self) -> &[Cell] {
        &self.cells
    }
}

impl DerefMut for Row {
    fn deref_mut(&mut self) -> &mut [Cell] {
        &mut self.cells
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_size() {
        let size = std::mem::size_of::<Cell>();
        assert!(size <= 8, "Cell should be <= 8 bytes, got {size}");
    }

    #[test]
    fn row_combining_basic() {
        let mut row = Row::new(10);
        assert_eq!(row.combining(0), &[] as &[char]);
        row.push_combining(0, '\u{0301}');
        assert_eq!(row.combining(0), &['\u{0301}']);
        assert_eq!(row.combining_len(0), 1);
        row.push_combining(0, '\u{0308}');
        assert_eq!(row.combining_len(0), 2);
        assert_eq!(row.combining(0), &['\u{0301}', '\u{0308}']);
    }

    #[test]
    fn row_clear_combining() {
        let mut row = Row::new(10);
        row.push_combining(3, '\u{0301}');
        assert_eq!(row.combining_len(3), 1);
        row.clear_combining(3);
        assert_eq!(row.combining_len(3), 0);
    }

    #[test]
    fn row_remove_shifts_combining() {
        let mut row = Row::new(10);
        row.push_combining(5, '\u{0301}');
        row.remove(3);
        // Column 5 should now be at column 4
        assert_eq!(row.combining(4), &['\u{0301}']);
        assert_eq!(row.combining(5), &[] as &[char]);
    }

    #[test]
    fn row_insert_shifts_combining() {
        let mut row = Row::new(10);
        row.push_combining(3, '\u{0301}');
        row.insert(2, Cell::default());
        // Column 3 should now be at column 4
        assert_eq!(row.combining(4), &['\u{0301}']);
        assert_eq!(row.combining(3), &[] as &[char]);
    }

    #[test]
    fn row_resize_truncates_combining() {
        let mut row = Row::new(10);
        row.push_combining(8, '\u{0301}');
        row.resize(5, Cell::default());
        assert_eq!(row.combining(8), &[] as &[char]);
    }

    #[test]
    fn row_fix_wide_char_orphan_at_boundary_width2() {
        let mut row = Row::new(10);
        row[4].width = 2;
        row[5].width = 0;
        row.fix_wide_char_orphan_at_boundary(5);
        assert_eq!(row[4].c, ' ');
        assert_eq!(row[4].width, 1);
    }

    #[test]
    fn row_fix_wide_char_orphan_at_boundary_continuation() {
        let mut row = Row::new(10);
        row[3].width = 2;
        row[4].width = 0;
        row.fix_wide_char_orphan_at_boundary(5);
        // Cell at last (4) is a continuation — blank it and its base (3)
        assert_eq!(row[3].c, ' ');
        assert_eq!(row[3].width, 1);
        assert_eq!(row[4].c, ' ');
        assert_eq!(row[4].width, 1);
    }

    #[test]
    fn row_fix_wide_char_orphan_noop_when_short() {
        let mut row = Row::new(5);
        row[2].c = 'A';
        row.fix_wide_char_orphan_at_boundary(10); // row shorter than boundary
        assert_eq!(row[2].c, 'A'); // unchanged
    }
}
