#[derive(Clone, Debug)]
pub struct Grid {
    size: Size,
    pos: Pos,
    saved_pos: Pos,
    rows: Vec<crate::row::Row>,
    scroll_top: u16,
    scroll_bottom: u16,
    origin_mode: bool,
    saved_origin_mode: bool,
    scrollback: std::collections::VecDeque<crate::row::Row>,
    scrollback_len: usize,
    scrollback_offset: usize,
}

impl Grid {
    pub fn new(size: Size, scrollback_len: usize) -> Self {
        // roost hardening: a 0-row/0-col grid makes every `rows - 1` /
        // `cols - width` below underflow. Never allow a degenerate size.
        let size = Size { rows: size.rows.max(1), cols: size.cols.max(1) };
        Self {
            size,
            pos: Pos::default(),
            saved_pos: Pos::default(),
            rows: vec![],
            scroll_top: 0,
            scroll_bottom: size.rows - 1,
            origin_mode: false,
            saved_origin_mode: false,
            scrollback: std::collections::VecDeque::new(),
            scrollback_len,
            scrollback_offset: 0,
        }
    }

    pub fn allocate_rows(&mut self) {
        if self.rows.is_empty() {
            self.rows.extend(
                std::iter::repeat_with(|| {
                    crate::row::Row::new(self.size.cols)
                })
                .take(usize::from(self.size.rows)),
            );
        }
    }

    /// A copy of this grid holding only its *visible* rows: the banked
    /// scrollback is dropped and the view offset reset to the live tail.
    /// Backs `Screen::snapshot` (SPEC-parity P1) — cloning the history too
    /// would copy up to `scrollback_len` rows on every synchronized-output
    /// bracket, and a snapshot is only ever presented as the current frame.
    /// Fields are listed explicitly so a new one fails to compile here
    /// rather than silently vanishing from snapshots.
    // roost: added for SPEC-parity P1.
    pub fn snapshot(&self) -> Self {
        Self {
            size: self.size,
            pos: self.pos,
            saved_pos: self.saved_pos,
            rows: self.rows.clone(),
            scroll_top: self.scroll_top,
            scroll_bottom: self.scroll_bottom,
            origin_mode: self.origin_mode,
            saved_origin_mode: self.saved_origin_mode,
            scrollback: std::collections::VecDeque::new(),
            scrollback_len: self.scrollback_len,
            scrollback_offset: 0,
        }
    }

    fn new_row(&self) -> crate::row::Row {
        crate::row::Row::new(self.size.cols)
    }

    pub fn clear(&mut self) {
        self.pos = Pos::default();
        self.saved_pos = Pos::default();
        for row in self.drawing_rows_mut() {
            row.clear(crate::attrs::Attrs::default());
        }
        self.scroll_top = 0;
        self.scroll_bottom = self.size.rows - 1;
        self.origin_mode = false;
        self.saved_origin_mode = false;
    }

    pub fn size(&self) -> Size {
        self.size
    }

    /// Resize without reflowing: rows are truncated/padded in place and every
    /// wrap flag is dropped. This is what the *alternate* grid gets — an
    /// alternate-screen application repaints on SIGWINCH and a rewrap would
    /// only fight the redraw it is about to send (SPEC-parity P5).
    pub fn set_size(&mut self, size: Size) {
        self.resize_to(size, false);
    }

    /// roost (SPEC-parity P5): resize *and* rewrap.
    ///
    /// The rows are read back into the logical lines the application actually
    /// printed (a row's `wrapped` flag means "this line continues below"),
    /// each line is laid out again at the new width — attributes, wide glyphs
    /// and all — and the result is written back. Narrowing therefore wraps a
    /// long line instead of hard-truncating it, and widening rejoins the
    /// pieces, so a zoom round-trip is lossless.
    ///
    /// Deliberately **live grid only**: rows already banked in the scrollback
    /// keep the width they were banked at, and rows this rewrap pushes off the
    /// top are banked at the new one. That is the same veneer-vs-second-state
    /// split `Screen::snapshot` draws for P1 — it buys a lossless round-trip
    /// without taking on a full history rewrap (and the mixed-width history it
    /// leaves behind is the nuance P5 already documents).
    pub fn set_size_reflowing(&mut self, size: Size) {
        self.resize_to(size, true);
    }

    fn resize_to(&mut self, size: Size, reflow: bool) {
        // roost hardening: clamp to a 1x1 floor (see Grid::new).
        let size = Size { rows: size.rows.max(1), cols: size.cols.max(1) };
        // A scroll region means an application is driving this grid as a
        // fixed-geometry canvas (a status line pinned outside a scrolling
        // area). Like an alternate-screen app it repaints on SIGWINCH, and
        // rewrapping under it would fight that redraw — so it resizes the old
        // way. Checked before `scroll_bottom` is retargeted below, i.e.
        // against the region as the application set it.
        let reflowing = reflow
            && size != self.size
            && !self.rows.is_empty()
            && !self.scroll_region_active();

        if size.cols != self.size.cols && !reflowing {
            for row in &mut self.rows {
                row.wrap(false);
            }
        }

        if self.scroll_bottom == self.size.rows - 1 {
            self.scroll_bottom = size.rows - 1;
        }

        if reflowing {
            // Rewrites `rows` to exactly `size` and maps the cursor with it.
            self.rewrap(size);
        }

        self.size = size;
        if !reflowing {
            for row in &mut self.rows {
                row.resize(size.cols, crate::cell::Cell::default());
            }
        }
        self.rows.resize(usize::from(size.rows), self.new_row());

        if self.scroll_bottom >= size.rows {
            self.scroll_bottom = size.rows - 1;
        }
        if self.scroll_bottom < self.scroll_top {
            self.scroll_top = 0;
        }

        self.row_clamp_top(false);
        self.row_clamp_bottom(false);
        self.col_clamp();
        // roost hardening: a saved cursor (DECSC) that a resize left outside
        // the grid is dereferenced unconditionally the moment DECRC restores
        // it. Clamping keeps a shrunken pane from panicking the multiplexer.
        self.saved_pos.row = self.saved_pos.row.min(size.rows - 1);
        self.saved_pos.col = self.saved_pos.col.min(size.cols - 1);
        // roost (SPEC-ux U3/U9): the view offset must stay inside the history
        // it points at, so the `↑N` badge keeps telling the truth across a
        // resize. Callers read the clamp back rather than trusting a cache.
        self.scrollback_offset =
            self.scrollback_offset.min(self.scrollback.len());
    }

    /// SPEC-parity P5: rebuild `rows` as `new` by rewrapping the live grid's
    /// logical lines. Also updates `pos` (and banks whatever no longer fits).
    fn rewrap(&mut self, new: Size) {
        // 1. Read the rows back into logical lines, noting which one the
        //    cursor is in and how far along it sits.
        let cursor = self.pos;
        let mut lines: Vec<(Vec<crate::cell::Cell>, bool)> = Vec::new();
        let mut line: Vec<crate::cell::Cell> = Vec::new();
        let mut open = false;
        let mut cursor_line = 0;
        let mut cursor_off = 0;
        for (i, row) in self.rows.iter().enumerate() {
            if i == usize::from(cursor.row) {
                cursor_line = lines.len();
                cursor_off = line.len() + usize::from(cursor.col);
            }
            line.extend_from_slice(row.content_cells());
            open = row.wrapped();
            if !open {
                lines.push((std::mem::take(&mut line), false));
            }
        }
        if open {
            // The bottom row is still wrapping: the line runs past the grid,
            // so its last row keeps the flag.
            lines.push((line, true));
        }

        // 2. Lay every line out again at the new width, tracking the cursor.
        let mut rows: Vec<crate::row::Row> = Vec::with_capacity(self.rows.len());
        let mut cursor_row = 0;
        let mut cursor_col = 0;
        for (i, (cells, wrapped_tail)) in lines.into_iter().enumerate() {
            let track = (i == cursor_line).then_some(cursor_off);
            let base = rows.len();
            let (laid, at) = lay_out(&cells, new.cols, wrapped_tail, track);
            if let Some((row, col)) = at {
                cursor_row = base + row;
                cursor_col = col;
            }
            rows.extend(laid);
        }

        // 3. Fit the result to the grid. Blank rows below the cursor are the
        //    screen's own empty tail — shedding those first is what keeps a
        //    narrowing from pushing live content into a history that is never
        //    rewrapped (and so could never rejoin on the way back).
        let want = usize::from(new.rows);
        while rows.len() > want
            && rows.len() > cursor_row + 1
            && rows.last().is_some_and(|r| r.is_blank() && !r.wrapped())
        {
            rows.pop();
        }
        // Whatever still doesn't fit scrolls off the top exactly like ordinary
        // output does — into the scrollback, oldest first.
        let banked = rows.len().saturating_sub(want);
        if banked > 0 {
            for row in rows.drain(..banked) {
                self.bank(row);
            }
            if self.scrollback_offset > 0 {
                // A scrolled-back view stays pinned on the rows it was
                // reading, exactly as `scroll_up` keeps it there.
                self.scrollback_offset =
                    self.scrollback.len().min(self.scrollback_offset + banked);
            }
            // A rewrap that pushed the cursor above the top of the grid
            // clamps into it rather than addressing history.
            cursor_row = cursor_row.saturating_sub(banked);
        }
        rows.resize(want, crate::row::Row::new(new.cols));
        self.rows = rows;
        self.pos = Pos {
            // Both are already in range; the clamps are the load-bearing
            // guarantee that `pos` names a real cell after any rewrap.
            row: u16::try_from(cursor_row.min(want - 1)).unwrap_or(0),
            col: cursor_col.min(new.cols - 1),
        };
    }

    pub fn pos(&self) -> Pos {
        self.pos
    }

    pub fn set_pos(&mut self, mut pos: Pos) {
        if self.origin_mode {
            pos.row = pos.row.saturating_add(self.scroll_top);
        }
        self.pos = pos;
        self.row_clamp_top(self.origin_mode);
        self.row_clamp_bottom(self.origin_mode);
        self.col_clamp();
    }

    pub fn save_cursor(&mut self) {
        self.saved_pos = self.pos;
        self.saved_origin_mode = self.origin_mode;
    }

    pub fn restore_cursor(&mut self) {
        self.pos = self.saved_pos;
        self.origin_mode = self.saved_origin_mode;
    }

    pub fn visible_rows(&self) -> impl Iterator<Item = &crate::row::Row> {
        let scrollback_len = self.scrollback.len();
        let rows_len = self.rows.len();
        self.scrollback
            .iter()
            .skip(scrollback_len - self.scrollback_offset)
            .chain(
                self.rows
                    .iter()
                    .take(rows_len.saturating_sub(self.scrollback_offset)),
            )
    }

    pub fn drawing_rows(&self) -> impl Iterator<Item = &crate::row::Row> {
        self.rows.iter()
    }

    // roost: full-history read (scrollback + current screen), for the
    // control interface's `read --full`/`--tail` (visible_rows above only
    // covers the on-screen window).
    pub fn all_rows(&self) -> impl Iterator<Item = &crate::row::Row> + '_ {
        self.scrollback.iter().chain(self.rows.iter())
    }

    pub fn drawing_rows_mut(
        &mut self,
    ) -> impl Iterator<Item = &mut crate::row::Row> {
        self.rows.iter_mut()
    }

    pub fn visible_row(&self, row: u16) -> Option<&crate::row::Row> {
        self.visible_rows().nth(usize::from(row))
    }

    pub fn drawing_row(&self, row: u16) -> Option<&crate::row::Row> {
        self.drawing_rows().nth(usize::from(row))
    }

    pub fn drawing_row_mut(
        &mut self,
        row: u16,
    ) -> Option<&mut crate::row::Row> {
        self.drawing_rows_mut().nth(usize::from(row))
    }

    pub fn current_row_mut(&mut self) -> &mut crate::row::Row {
        self.drawing_row_mut(self.pos.row)
            // we assume self.pos.row is always valid
            .unwrap()
    }

    pub fn visible_cell(&self, pos: Pos) -> Option<&crate::cell::Cell> {
        self.visible_row(pos.row).and_then(|r| r.get(pos.col))
    }

    pub fn drawing_cell(&self, pos: Pos) -> Option<&crate::cell::Cell> {
        self.drawing_row(pos.row).and_then(|r| r.get(pos.col))
    }

    pub fn drawing_cell_mut(
        &mut self,
        pos: Pos,
    ) -> Option<&mut crate::cell::Cell> {
        self.drawing_row_mut(pos.row)
            .and_then(|r| r.get_mut(pos.col))
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback_len
    }

    // roost: how many history rows are actually banked right now (distinct
    // from scrollback_len, the configured cap) — the M of the scroll
    // position hint `↑N/M` (SPEC-ux U3).
    pub fn scrollback_rows(&self) -> usize {
        self.scrollback.len()
    }

    pub fn scrollback(&self) -> usize {
        self.scrollback_offset
    }

    pub fn set_scrollback(&mut self, rows: usize) {
        self.scrollback_offset = rows.min(self.scrollback.len());
    }

    pub fn write_contents(&self, contents: &mut String) {
        let mut wrapping = false;
        for row in self.visible_rows() {
            row.write_contents(contents, 0, self.size.cols, wrapping);
            if !row.wrapped() {
                contents.push('\n');
            }
            wrapping = row.wrapped();
        }

        while contents.ends_with('\n') {
            contents.truncate(contents.len() - 1);
        }
    }

    pub fn erase_all(&mut self, attrs: crate::attrs::Attrs) {
        for row in self.drawing_rows_mut() {
            row.clear(attrs);
        }
    }

    pub fn erase_all_forward(&mut self, attrs: crate::attrs::Attrs) {
        let pos = self.pos;
        for row in self.drawing_rows_mut().skip(usize::from(pos.row) + 1) {
            row.clear(attrs);
        }

        self.erase_row_forward(attrs);
    }

    pub fn erase_all_backward(&mut self, attrs: crate::attrs::Attrs) {
        let pos = self.pos;
        for row in self.drawing_rows_mut().take(usize::from(pos.row)) {
            row.clear(attrs);
        }

        self.erase_row_backward(attrs);
    }

    pub fn erase_row(&mut self, attrs: crate::attrs::Attrs) {
        self.current_row_mut().clear(attrs);
    }

    pub fn erase_row_forward(&mut self, attrs: crate::attrs::Attrs) {
        let size = self.size;
        let pos = self.pos;
        let row = self.current_row_mut();
        for col in pos.col..size.cols {
            row.erase(col, attrs);
        }
    }

    pub fn erase_row_backward(&mut self, attrs: crate::attrs::Attrs) {
        let size = self.size;
        let pos = self.pos;
        let row = self.current_row_mut();
        for col in 0..=pos.col.min(size.cols - 1) {
            row.erase(col, attrs);
        }
    }

    pub fn insert_cells(&mut self, count: u16) {
        let size = self.size;
        let pos = self.pos;
        let wide = pos.col < size.cols
            && self
                .drawing_cell(pos)
                // we assume self.pos.row is always valid, and we know we are
                // not off the end of a row because we just checked pos.col <
                // size.cols
                .unwrap()
                .is_wide_continuation();
        let row = self.current_row_mut();
        for _ in 0..count {
            if wide {
                row.get_mut(pos.col).unwrap().set_wide_continuation(false);
            }
            row.insert(pos.col, crate::cell::Cell::default());
            if wide {
                row.get_mut(pos.col).unwrap().set_wide_continuation(true);
            }
        }
        row.truncate(size.cols);
    }

    pub fn delete_cells(&mut self, count: u16) {
        let size = self.size;
        let pos = self.pos;
        let row = self.current_row_mut();
        for _ in 0..(count.min(size.cols - pos.col)) {
            row.remove(pos.col);
        }
        row.resize(size.cols, crate::cell::Cell::default());
    }

    pub fn erase_cells(&mut self, count: u16, attrs: crate::attrs::Attrs) {
        let size = self.size;
        let pos = self.pos;
        let row = self.current_row_mut();
        for col in pos.col..((pos.col.saturating_add(count)).min(size.cols)) {
            row.erase(col, attrs);
        }
    }

    pub fn insert_lines(&mut self, count: u16) {
        for _ in 0..count {
            self.rows.remove(usize::from(self.scroll_bottom));
            self.rows.insert(usize::from(self.pos.row), self.new_row());
            // self.scroll_bottom is maintained to always be a valid row
            self.rows[usize::from(self.scroll_bottom)].wrap(false);
        }
    }

    pub fn delete_lines(&mut self, count: u16) {
        for _ in 0..(count.min(self.size.rows - self.pos.row)) {
            self.rows
                .insert(usize::from(self.scroll_bottom) + 1, self.new_row());
            self.rows.remove(usize::from(self.pos.row));
        }
    }

    /// roost: push one row into history, trimmed and capped.
    ///
    /// The single door into the scrollback, so the trim can never be
    /// forgotten on one of the two paths that bank rows (ordinary scrolling
    /// here, and the rewrap in `resize_to`). See
    /// `Row::shrink_to_contents` for what the trim is and why a banked row
    /// can afford it.
    fn bank(&mut self, mut row: crate::row::Row) {
        if self.scrollback_len == 0 {
            return;
        }
        row.shrink_to_contents();
        self.scrollback.push_back(row);
        while self.scrollback.len() > self.scrollback_len {
            self.scrollback.pop_front();
        }
    }

    pub fn scroll_up(&mut self, count: u16) {
        for _ in 0..(count.min(self.size.rows - self.scroll_top)) {
            self.rows
                .insert(usize::from(self.scroll_bottom) + 1, self.new_row());
            let removed = self.rows.remove(usize::from(self.scroll_top));
            if self.scrollback_len > 0 && !self.scroll_region_active() {
                self.bank(removed);
                if self.scrollback_offset > 0 {
                    self.scrollback_offset =
                        self.scrollback.len().min(self.scrollback_offset + 1);
                }
            }
        }
    }

    pub fn scroll_down(&mut self, count: u16) {
        for _ in 0..count {
            self.rows.remove(usize::from(self.scroll_bottom));
            self.rows
                .insert(usize::from(self.scroll_top), self.new_row());
            // self.scroll_bottom is maintained to always be a valid row
            self.rows[usize::from(self.scroll_bottom)].wrap(false);
        }
    }

    pub fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        let bottom = bottom.min(self.size().rows - 1);
        if top < bottom {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
        } else {
            self.scroll_top = 0;
            self.scroll_bottom = self.size().rows - 1;
        }
        self.pos.row = self.scroll_top;
        self.pos.col = 0;
    }

    fn in_scroll_region(&self) -> bool {
        self.pos.row >= self.scroll_top && self.pos.row <= self.scroll_bottom
    }

    fn scroll_region_active(&self) -> bool {
        self.scroll_top != 0 || self.scroll_bottom != self.size.rows - 1
    }

    pub fn set_origin_mode(&mut self, mode: bool) {
        self.origin_mode = mode;
        self.set_pos(Pos { row: 0, col: 0 });
    }

    pub fn row_inc_clamp(&mut self, count: u16) {
        let in_scroll_region = self.in_scroll_region();
        self.pos.row = self.pos.row.saturating_add(count);
        self.row_clamp_bottom(in_scroll_region);
    }

    pub fn row_inc_scroll(&mut self, count: u16) -> u16 {
        let in_scroll_region = self.in_scroll_region();
        self.pos.row = self.pos.row.saturating_add(count);
        let lines = self.row_clamp_bottom(in_scroll_region);
        if in_scroll_region {
            self.scroll_up(lines);
            lines
        } else {
            0
        }
    }

    pub fn row_dec_clamp(&mut self, count: u16) {
        let in_scroll_region = self.in_scroll_region();
        self.pos.row = self.pos.row.saturating_sub(count);
        self.row_clamp_top(in_scroll_region);
    }

    pub fn row_dec_scroll(&mut self, count: u16) {
        let in_scroll_region = self.in_scroll_region();
        // need to account for clamping by both row_clamp_top and by
        // saturating_sub
        let extra_lines = if count > self.pos.row {
            count - self.pos.row
        } else {
            0
        };
        self.pos.row = self.pos.row.saturating_sub(count);
        let lines = self.row_clamp_top(in_scroll_region);
        self.scroll_down(lines + extra_lines);
    }

    pub fn row_set(&mut self, i: u16) {
        self.pos.row = i;
        self.row_clamp();
    }

    pub fn col_inc(&mut self, count: u16) {
        self.pos.col = self.pos.col.saturating_add(count);
    }

    pub fn col_inc_clamp(&mut self, count: u16) {
        self.pos.col = self.pos.col.saturating_add(count);
        self.col_clamp();
    }

    pub fn col_dec(&mut self, count: u16) {
        self.pos.col = self.pos.col.saturating_sub(count);
    }

    pub fn col_tab(&mut self) {
        self.pos.col -= self.pos.col % 8;
        self.pos.col += 8;
        self.col_clamp();
    }

    pub fn col_set(&mut self, i: u16) {
        self.pos.col = i;
        self.col_clamp();
    }

    pub fn col_wrap(&mut self, width: u16, wrap: bool) {
        // roost hardening: `cols - width` underflows when a wide (width 2)
        // char lands in a 1-col grid; `prev_pos.row -= scrolled` underflows
        // when scrolling a 1-row grid. Saturating keeps a degenerate pane
        // from panicking the whole multiplexer.
        if self.pos.col > self.size.cols.saturating_sub(width) {
            let mut prev_pos = self.pos;
            self.pos.col = 0;
            let scrolled = self.row_inc_scroll(1);
            prev_pos.row = prev_pos.row.saturating_sub(scrolled);
            let new_pos = self.pos;
            self.drawing_row_mut(prev_pos.row)
                // we assume self.pos.row is always valid, and so prev_pos.row
                // must be valid because it is always less than or equal to
                // self.pos.row
                .unwrap()
                .wrap(wrap && prev_pos.row + 1 == new_pos.row);
        }
    }

    fn row_clamp_top(&mut self, limit_to_scroll_region: bool) -> u16 {
        if limit_to_scroll_region && self.pos.row < self.scroll_top {
            let rows = self.scroll_top - self.pos.row;
            self.pos.row = self.scroll_top;
            rows
        } else {
            0
        }
    }

    fn row_clamp_bottom(&mut self, limit_to_scroll_region: bool) -> u16 {
        let bottom = if limit_to_scroll_region {
            self.scroll_bottom
        } else {
            self.size.rows - 1
        };
        if self.pos.row > bottom {
            let rows = self.pos.row - bottom;
            self.pos.row = bottom;
            rows
        } else {
            0
        }
    }

    fn row_clamp(&mut self) {
        if self.pos.row > self.size.rows - 1 {
            self.pos.row = self.size.rows - 1;
        }
    }

    fn col_clamp(&mut self) {
        if self.pos.col > self.size.cols - 1 {
            self.pos.col = self.size.cols - 1;
        }
    }
}

/// A column index as the grid stores it. Every caller passes a column inside
/// a grid whose width is a `u16`, so the conversion can't actually fail.
fn col_index(col: usize) -> u16 {
    u16::try_from(col).unwrap_or(u16::MAX)
}

/// roost (SPEC-parity P5): lay one logical line out at `cols` columns.
///
/// Returns the rows it occupies — each padded to `cols`, each but the last
/// flagged wrapped (the last carries `wrapped_tail`, for a line that still
/// runs past the bottom of the grid) — and, when `cursor` names a column
/// offset into the line, where that offset landed, as `(row within the line,
/// column)`.
///
/// Two properties this function exists to guarantee:
///
/// * a cell is copied whole, so a rewrapped line keeps its colors, bold, dim
///   and every other attribute — reflow that preserved only characters would
///   repaint a colored log as plain text on every resize;
/// * a two-column glyph is never split across the boundary. When it doesn't
///   fit, the row ends one column short and the whole glyph moves down —
///   what every real terminal does, and half a glyph per row isn't
///   representable in the grid anyway. Continuation cells are dropped on the
///   way in and regenerated on the way out, so each one always sits beside
///   the glyph it belongs to at the *new* width.
fn lay_out(
    cells: &[crate::cell::Cell],
    cols: u16,
    wrapped_tail: bool,
    cursor: Option<usize>,
) -> (Vec<crate::row::Row>, Option<(usize, u16)>) {
    let width = usize::from(cols);
    let mut rows: Vec<crate::row::Row> = Vec::new();
    let mut cur: Vec<crate::cell::Cell> = Vec::new();
    let mut at: Option<(usize, u16)> = None;

    for (i, cell) in cells.iter().enumerate() {
        if cell.is_wide_continuation() {
            // Bookkeeping for the old width; the glyph before it gets a fresh
            // continuation below. A cursor parked on one stays on its glyph.
            if cursor == Some(i) && at.is_none() {
                at = Some((rows.len(), col_index(cur.len().saturating_sub(1))));
            }
            continue;
        }
        let cell_width = if cell.is_wide() { 2 } else { 1 };
        if cell_width > width {
            continue; // a wide glyph in a one-column grid has nowhere to go
        }
        if cur.len() + cell_width > width {
            rows.push(crate::row::Row::from_cells(
                std::mem::take(&mut cur),
                cols,
                true,
            ));
        }
        if cursor == Some(i) && at.is_none() {
            at = Some((rows.len(), col_index(cur.len())));
        }
        cur.push(cell.clone());
        if cell_width > 1 {
            let mut continuation = crate::cell::Cell::default();
            continuation.set_wide_continuation(true);
            cur.push(continuation);
        }
    }

    if at.is_none() {
        if let Some(off) = cursor {
            // The cursor sits past the line's text — a fresh prompt, or a
            // cursor parked in the blank tail of a row. It keeps its distance
            // from the end of the content, wrapping the way text does.
            let total = cur.len() + off.saturating_sub(cells.len());
            at = Some((rows.len() + total / width, col_index(total % width)));
        }
    }
    rows.push(crate::row::Row::from_cells(cur, cols, wrapped_tail));
    (rows, at)
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Size {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Pos {
    pub row: u16,
    pub col: u16,
}
