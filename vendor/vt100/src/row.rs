#[derive(Clone, Debug)]
pub struct Row {
    cells: Vec<crate::cell::Cell>,
    wrapped: bool,
}

impl Row {
    pub fn new(cols: u16) -> Self {
        Self {
            cells: vec![crate::cell::Cell::default(); usize::from(cols)],
            wrapped: false,
        }
    }

    fn cols(&self) -> u16 {
        self.cells
            .len()
            .try_into()
            // we limit the number of cols to a u16 (see Size)
            .unwrap()
    }

    pub fn clear(&mut self, attrs: crate::attrs::Attrs) {
        for cell in &mut self.cells {
            cell.clear(attrs);
        }
        self.wrapped = false;
    }

    fn cells(&self) -> impl Iterator<Item = &crate::cell::Cell> {
        self.cells.iter()
    }

    pub fn get(&self, col: u16) -> Option<&crate::cell::Cell> {
        self.cells.get(usize::from(col))
    }

    pub fn get_mut(&mut self, col: u16) -> Option<&mut crate::cell::Cell> {
        self.cells.get_mut(usize::from(col))
    }

    pub fn insert(&mut self, i: u16, cell: crate::cell::Cell) {
        self.cells.insert(usize::from(i), cell);
        self.wrapped = false;
    }

    pub fn remove(&mut self, i: u16) {
        self.clear_wide(i);
        self.cells.remove(usize::from(i));
        self.wrapped = false;
    }

    pub fn erase(&mut self, i: u16, attrs: crate::attrs::Attrs) {
        let wide = self.cells[usize::from(i)].is_wide();
        self.clear_wide(i);
        self.cells[usize::from(i)].clear(attrs);
        if i == self.cols() - if wide { 2 } else { 1 } {
            self.wrapped = false;
        }
    }

    pub fn truncate(&mut self, len: u16) {
        self.cells.truncate(usize::from(len));
        self.wrapped = false;
        self.repair_last_wide();
    }

    pub fn resize(&mut self, len: u16, cell: crate::cell::Cell) {
        self.cells.resize(usize::from(len), cell);
        self.wrapped = false;
        self.repair_last_wide();
    }

    /// roost hardening: a wide cell keeps its other half in the next column,
    /// so a shrink that leaves the base in the new *last* column leaves a
    /// half-glyph whose continuation no longer exists. `Screen::text` reads
    /// that continuation back through `.unwrap()` whenever it draws over
    /// such a cell, so the next character written there panicked the parser
    /// — and a panic on the event loop unwinds past `App::shutdown`, taking
    /// roost down and leaving every agent running.
    ///
    /// `truncate` already did this repair and `resize` did not, which is the
    /// whole bug: `Grid::resize_to` narrows its rows through `resize`. Any
    /// pane that does not reflow — the alternate screen, or an app holding a
    /// scroll region — kept the literal truncation, so `"se中"` at three
    /// columns plus one more keystroke was enough.
    ///
    /// Truncating the half-glyph is exactly what the shrink already did to
    /// its other half.
    fn repair_last_wide(&mut self) {
        if let Some(last) = self.cells.last_mut() {
            if last.is_wide() {
                last.clear(*last.attrs());
            }
        }
    }

    pub fn wrap(&mut self, wrap: bool) {
        self.wrapped = wrap;
    }

    /// roost (SPEC-parity P5): a row built from cells a reflow has already
    /// laid out, padded to `cols` with blanks. `wrapped` says whether the
    /// logical line continues on the row below — the flag the whole rewrap
    /// turns on, and the reason this can't go through `resize` (which clears
    /// it).
    pub fn from_cells(
        mut cells: Vec<crate::cell::Cell>,
        cols: u16,
        wrapped: bool,
    ) -> Self {
        cells.resize(usize::from(cols), crate::cell::Cell::default());
        Self { cells, wrapped }
    }

    /// roost (SPEC-parity P5): the row's cells up to and including the last
    /// one with contents — the part a reflow treats as the line's text.
    ///
    /// Everything past it was never written (or has been erased), so it is
    /// padding at the row's *current* width: carrying it into a rewrapped
    /// logical line would turn every short line into a full-width block of
    /// blanks. A deliberately printed space has contents and survives; an
    /// untouched cell does not.
    pub fn content_cells(&self) -> &[crate::cell::Cell] {
        let end = self
            .cells
            .iter()
            .rposition(crate::cell::Cell::has_contents)
            .map_or(0, |i| i + 1);
        &self.cells[..end]
    }

    /// roost: shed the untouched tail of a row on its way into history.
    ///
    /// A live row is exactly `cols` cells wide because every draw indexes it
    /// by column. A **banked** row is never written to again — the
    /// scrollback is read-only from the moment `Grid` pushes into it — so
    /// the cells past its last visible one are pure padding, and at 36 bytes
    /// a `Cell` that padding is most of the memory roost holds. A
    /// 200-column pane was paying 7.2 KB for every line of history
    /// regardless of its length: 34 MB per pane once the 5,000-row
    /// scrollback filled, measured, and it scales with the window's width.
    ///
    /// "Last visible" is the last cell with contents **or** with non-default
    /// attributes. `ESC[K` erases to end of line *with the current attrs*,
    /// so an app painting a coloured bar leaves a run of cells that hold no
    /// character and are still real output; trimming on contents alone would
    /// drop that colour from history the moment the line scrolled off. A
    /// line whose tail was never touched has default attrs there and trims
    /// exactly as if the rule were `content_cells`'.
    ///
    /// It cannot cut a wide glyph in half either: a continuation cell
    /// carries the continuation bit in `len`, so `has_contents` is true for
    /// it and the trim stops after it, not between the halves.
    ///
    /// Readers must treat a column past the end as blank. `Row::get` already
    /// answers `None` there, which is what `extract_selection` and
    /// `write_contents` already do the right thing with; the pane blit is
    /// the one place that had to learn it.
    pub fn shrink_to_contents(&mut self) {
        let default = crate::attrs::Attrs::default();
        let end = self
            .cells
            .iter()
            .rposition(|c| c.has_contents() || *c.attrs() != default)
            .map_or(0, |i| i + 1);
        if end < self.cells.len() {
            // `to_vec` rather than `truncate`: truncating keeps the original
            // capacity, so nothing is actually freed. This reallocates at
            // the exact length, which is the entire point.
            self.cells = self.cells[..end].to_vec();
        }
    }

    /// roost (SPEC-parity P5): does this row hold nothing at all? A rewrap
    /// that has to shed rows sheds these — from below the cursor — before it
    /// banks anything into history.
    pub fn is_blank(&self) -> bool {
        self.cells.iter().all(|c| !c.has_contents())
    }

    pub fn wrapped(&self) -> bool {
        self.wrapped
    }

    pub fn clear_wide(&mut self, col: u16) {
        let cell = &self.cells[usize::from(col)];
        let other = if cell.is_wide() {
            &mut self.cells[usize::from(col + 1)]
        } else if cell.is_wide_continuation() {
            &mut self.cells[usize::from(col - 1)]
        } else {
            return;
        };
        other.clear(*other.attrs());
    }

    pub fn write_contents(
        &self,
        contents: &mut String,
        start: u16,
        width: u16,
        wrapping: bool,
    ) {
        let mut prev_was_wide = false;

        let mut prev_col = start;
        for (col, cell) in self
            .cells()
            .enumerate()
            .skip(usize::from(start))
            .take(usize::from(width))
        {
            if prev_was_wide {
                prev_was_wide = false;
                continue;
            }
            prev_was_wide = cell.is_wide();

            // we limit the number of cols to a u16 (see Size)
            let col: u16 = col.try_into().unwrap();
            if cell.has_contents() {
                for _ in 0..(col - prev_col) {
                    contents.push(' ');
                }
                prev_col += col - prev_col;

                cell.push_contents(contents);
                prev_col += if cell.is_wide() { 2 } else { 1 };
            }
        }
        if prev_col == start && wrapping {
            contents.push('\n');
        }
    }
}
