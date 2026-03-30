//! Trait abstracting grid mutations for testability.
//!
//! ScreenPerformer is generic over this trait, allowing mock grids in tests.

use std::collections::VecDeque;

use super::cell::{Cell, Row};
use super::grid::{SavedGrid, TerminalModes};
use super::style::StyleTable;

/// Abstract interface for terminal grid mutations.
///
/// Grid implements this trait directly. Tests can provide mock implementations.
pub(crate) trait GridMutator {
    // Dimensions
    fn cols(&self) -> u16;
    fn rows(&self) -> u16;
    fn visible_row_count(&self) -> usize;

    // Cursor
    fn cursor_x(&self) -> u16;
    fn cursor_y(&self) -> u16;
    fn set_cursor_x_unclamped(&mut self, x: u16);
    fn set_cursor_y_unclamped(&mut self, y: u16);
    fn wrap_pending(&self) -> bool;
    fn set_wrap_pending(&mut self, val: bool);
    fn set_cursor_visible(&mut self, visible: bool);

    // Scroll region
    fn scroll_top(&self) -> u16;
    fn scroll_bottom(&self) -> u16;
    fn set_scroll_region(&mut self, top: u16, bottom: u16);
    fn reset_scroll_region(&mut self);

    // Row access
    fn visible_row(&self, y: usize) -> &Row;
    fn visible_row_mut(&mut self, y: usize) -> &mut Row;
    fn new_blank_row(&self, fill: Cell) -> Row;
    fn remove_visible_row(&mut self, y: usize) -> Row;
    fn insert_visible_row(&mut self, y: usize, row: Row);

    // Safe cell/row operations (with auto-fixup)
    fn set_cell(&mut self, x: usize, y: usize, cell: Cell);
    fn erase_cells(&mut self, y: usize, from: usize, to: usize, blank: Cell);
    fn erase_rows(&mut self, from_y: usize, to_y: usize, blank: Cell);
    fn fixup_wide_char_at(&mut self, x: usize, y: usize);

    // Scroll operations
    fn scroll_up(&mut self, in_alt_screen: bool, fill: Cell);
    fn scroll_down(&mut self, fill: Cell);

    // Modes
    fn modes(&self) -> &TerminalModes;
    fn modes_mut(&mut self) -> &mut TerminalModes;
    fn set_modes(&mut self, modes: TerminalModes);

    // Style table
    fn style_table(&self) -> &StyleTable;
    fn style_table_mut(&mut self) -> &mut StyleTable;

    // Tab stops
    fn next_tab_stop(&self, col: u16) -> u16;
    fn set_tab_stop(&mut self, col: u16);
    fn clear_tab_stop(&mut self, col: u16);
    fn clear_all_tab_stops(&mut self);
    fn reset_tab_stops(&mut self);

    // Alt screen support
    fn drain_visible(&mut self) -> VecDeque<Row>;
    fn fill_visible_blank(&mut self);
    fn replace_visible(&mut self, rows: VecDeque<Row>);
    fn adjust_visible_to_fit(&mut self);
    fn set_scrollback_limit(&mut self, limit: usize);
    fn scrollback_limit(&self) -> usize;

    // Scrollback
    fn clear_scrollback(&mut self);

    // Style GC
    fn compact_styles(&mut self, saved_grid: Option<&SavedGrid>);
}
