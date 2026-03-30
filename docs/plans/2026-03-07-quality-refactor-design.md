# Quality Refactor Design — 2026-03-07

## Overview

Address 8 architectural issues identified in code review. Three independent clusters executed in parallel.

## Cluster A: Session/Bridge (Issues 1, 3, 7, 8)

### A1. Session Encapsulation + RAII ClientGuard

**Problem:** Session has pub fields (`has_client`, `evict_tx`, `screen_notify`, `reader_alive`). `has_client.store(false)` appears in 6 places across session_bridge.rs. TOCTOU race on eviction.

**Solution:**

- **`ClientGuard`** — RAII struct owning `evict_rx`. On Drop:
  - checks `*evict_rx.borrow()` (still true = not evicted)
  - if not evicted → `has_client.store(false, Release)`
  - drops evict_rx

- **Session API** — private fields, public methods:
  - `connect(&mut self, cols, rows) -> Result<(ClientGuard, SessionHandles, WatchReceiver<bool>)>` — atomic: set has_client → evict old → create channel → resize
  - `handles(&self) -> SessionHandles` — clone shared handles for relay
  - `is_alive(&self) -> bool` — unchanged
  - Remove direct access to `evict_tx`, `has_client`, `screen_notify`, `reader_alive`

- **SessionHandles** created inside `Session::connect()`, not externally

- **TOCTOU fix:** `connect()` runs under manager lock. Atomic: set has_client → evict → resize → create handles. Race eliminated.

- All 6 `has_client.store(false)` in session_bridge replaced by single ClientGuard Drop.

### A2. Session Bridge Split

**Problem:** session_bridge.rs is 615 lines with 5 responsibilities.

**Solution:** Split into 3 files:

- **`session_setup.rs`** — `setup_session()` function. Calls `Session::connect()`. Returns `SessionSetup`.
- **`session_relay.rs`** — `screen_to_client()` and `client_to_pty()` relay loops.
- **`session_bridge.rs`** — orchestrator: `handle_session()` calls setup → initial_state → spawn relays. Shared utilities (`lock_mutex`, `prepend_passthrough`, `render_and_send`).

Clone patterns standardized: SessionHandles created once in `Session::connect()`.

### A3. Session Bridge Tests

**After split**, add tests:
- `session_setup.rs` — unit tests with mock SessionManager
- `session_relay.rs` — relay tests with tokio DuplexStream (in-memory channels)
- Integration: setup → send_initial_state → screen_to_client with real Screen

## Cluster B: Screen/Grid (Issues 4, 5, 6)

### B1. Grid Safe API + Wide-char Auto-fixup

**Problem:** 10 manual `fixup_wide_char()` calls in performer.rs. `visible_row()` has no bounds check. Three moving indices without invariant checks.

**Solution:**

- **`set_cell(x, y, cell)`** — Grid method that automatically:
  - bounds check (debug_assert + graceful return on out-of-bounds)
  - calls fixup_wide_char before writing
  - if cell.width == 2, writes continuation cell
  - clears combining marks at position

- **`erase_cells(y, from, to, blank)`** — Grid method for erase operations, auto-fixup at boundaries

- **`visible_row()` / `visible_row_mut()`** — add `debug_assert!(y < self.rows as usize)` with informative message

- **`check_invariants()`** — debug-only method called after mutations:
  - `pending_start <= scrollback_len`
  - `scrollback_len + rows as usize <= cells.len()`
  - no orphaned wide-char continuations in visible area

- Remove `fixup_wide_char()` from performer entirely.

### B2. GridMutator Trait

**Problem:** Performer calls 40+ Grid methods directly. Cannot mock Grid for testing.

**Solution:**

```rust
pub trait GridMutator {
    // Dimensions
    fn rows(&self) -> u16;
    fn cols(&self) -> u16;
    fn visible_row_count(&self) -> usize;

    // Cursor
    fn cursor_x(&self) -> u16;
    fn cursor_y(&self) -> u16;
    fn set_cursor(&mut self, x: u16, y: u16);
    fn set_cursor_x_unclamped(&mut self, x: u16);
    fn set_cursor_y_unclamped(&mut self, y: u16);
    fn set_wrap_pending(&mut self, pending: bool);
    fn wrap_pending(&self) -> bool;

    // Cell operations (safe, with auto-fixup)
    fn set_cell(&mut self, x: usize, y: usize, cell: Cell);
    fn cell(&self, x: usize, y: usize) -> &Cell;
    fn erase_cells(&mut self, y: usize, from: usize, to: usize, blank: Cell);
    fn erase_rows(&mut self, from_y: usize, to_y: usize, blank: Cell);

    // Row operations
    fn visible_row(&self, y: usize) -> &Row;
    fn visible_row_mut(&mut self, y: usize) -> &mut Row;
    fn new_blank_row(&self) -> Row;
    fn remove_visible_row(&mut self, y: usize) -> Row;
    fn insert_visible_row(&mut self, y: usize, row: Row);

    // Scroll
    fn scroll_up(&mut self, in_alt_screen: bool, fill: Cell);
    fn scroll_down(&mut self, in_alt_screen: bool, fill: Cell);
    fn scroll_top(&self) -> u16;
    fn scroll_bottom(&self) -> u16;
    fn set_scroll_region(&mut self, top: u16, bottom: u16);
    fn reset_scroll_region(&mut self);

    // Modes
    fn modes(&self) -> &TerminalModes;
    fn modes_mut(&mut self) -> &mut TerminalModes;

    // Style
    fn style_table_mut(&mut self) -> &mut StyleTable;

    // Tab stops
    fn next_tab_stop(&self, from: u16) -> u16;
    fn set_tab_stop(&mut self, col: u16);
    fn clear_tab_stop(&mut self, col: u16);
    fn clear_all_tab_stops(&mut self);
    fn reset_tab_stops(&mut self);

    // Combining marks (delegated to Row)
    fn push_combining(&mut self, x: usize, y: usize, mark: char);
    fn clear_combining(&mut self, x: usize, y: usize);

    // Alt screen
    fn drain_visible(&mut self) -> Vec<Row>;
    fn fill_visible_blank(&mut self);
    fn replace_visible(&mut self, rows: Vec<Row>);
    fn adjust_visible_to_fit(&mut self);
    fn set_scrollback_limit(&mut self, limit: usize);
    fn scrollback_limit(&self) -> usize;

    // Scrollback
    fn clear_scrollback(&mut self);
}
```

- `ScreenPerformer<'a, G: GridMutator>` — generic over GridMutator
- Grid implements GridMutator
- Tests can use MockGrid

This is mechanical: each trait method maps 1:1 to current Grid API. Implementation is trivial forwarding.

## Cluster C: Independent (Issue 2)

### C1. Deferred Responses Bounding

**Problem:** `deferred_responses: Vec<Vec<u8>>` in persistent_reader_loop() is unbounded. try_lock + 1ms sleep is fragile.

**Solution:**
- Replace with `VecDeque<Vec<u8>>` bounded to `MAX_DEFERRED = 64`
- On overflow: drop oldest (DA/DSR responses go stale quickly)
- Remove 1ms sleep retry: try_lock once, on failure just defer. Next iteration will retry.
- Simpler, no wasted latency.

## Execution Order

Clusters A, B, C are independent and can be implemented in parallel.

Within each cluster:
- **A:** A1 (Session encapsulation) → A2 (bridge split) → A3 (tests)
- **B:** B1 (safe Grid API) → B2 (GridMutator trait)
- **C:** C1 (standalone)

All existing tests must pass after each step.
