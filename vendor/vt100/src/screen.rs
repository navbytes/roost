use unicode_width::UnicodeWidthChar as _;

const MODE_APPLICATION_KEYPAD: u8 = 0b0000_0001;
const MODE_APPLICATION_CURSOR: u8 = 0b0000_0010;
const MODE_HIDE_CURSOR: u8 = 0b0000_0100;
const MODE_ALTERNATE_SCREEN: u8 = 0b0000_1000;
const MODE_BRACKETED_PASTE: u8 = 0b0001_0000;
// roost: synchronized output, DEC private mode 2026 (SPEC-parity P1).
const MODE_SYNCHRONIZED_OUTPUT: u8 = 0b0010_0000;
// roost: focus reporting, DEC private mode 1004 (SPEC-parity P10). Purely a
// subscription flag — nothing about the grid changes when it is set; the
// embedder reads it to decide whether this pane is owed `CSI I`/`CSI O`.
const MODE_FOCUS_EVENT: u8 = 0b0100_0000;

/// A side effect the processed byte stream asked for that an in-memory
/// screen cannot carry out itself: it needs the *embedder* — the thing that
/// owns a real host terminal, a clipboard, a notification daemon.
///
/// Accumulated in stream order while `process` runs and drained with
/// [`Screen::take_effects`] (or [`crate::Parser::take_effects`]).
// roost: the vendored parser's event surface (SPEC-parity W3). Upstream's
// `Perform` impl can only mutate grid state, so every sequence that means
// "do something out there" died in an unhandled arm. This is the one piece
// of plumbing P2 (notifications), P3 (clipboard) and P7 (cursor shape) all
// needed; the arms that produce each variant are documented at their
// `osc_dispatch`/`csi_dispatch` sites.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    /// A desktop notification: OSC 9 (`9 ; body`) or OSC 777
    /// (`777 ; notify ; title ; body`). SPEC-parity P2.
    Notify {
        /// Present only for OSC 777, which carries an explicit title.
        title: Option<String>,
        body: String,
    },
    /// An OSC 52 clipboard *write* (`52 ; <selection> ; <base64>`).
    /// SPEC-parity P3. Read requests (`52 ; <selection> ; ?`) are
    /// deliberately never surfaced — answering one would hand the
    /// application the host clipboard's contents (a paste-theft vector).
    Osc52Write {
        selection: String,
        payload_base64: String,
    },
    /// DECSCUSR (`CSI Ps SP q`) — the cursor shape the application wants:
    /// 0/1 blinking block, 2 steady block, 3 blinking underline, 4 steady
    /// underline, 5 blinking bar, 6 steady bar. SPEC-parity P7.
    CursorShape(u8),
}

/// The xterm mouse handling mode currently in use.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MouseProtocolMode {
    /// Mouse handling is disabled.
    None,

    /// Mouse button events should be reported on button press. Also known as
    /// X10 mouse mode.
    Press,

    /// Mouse button events should be reported on button press and release.
    /// Also known as VT200 mouse mode.
    PressRelease,

    // Highlight,
    /// Mouse button events should be reported on button press and release, as
    /// well as when the mouse moves between cells while a button is held
    /// down.
    ButtonMotion,

    /// Mouse button events should be reported on button press and release,
    /// and mouse motion events should be reported when the mouse moves
    /// between cells regardless of whether a button is held down or not.
    AnyMotion,
    // DecLocator,
}

impl Default for MouseProtocolMode {
    fn default() -> Self {
        Self::None
    }
}

/// The encoding to use for the enabled `MouseProtocolMode`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MouseProtocolEncoding {
    /// Default single-printable-byte encoding.
    Default,

    /// UTF-8-based encoding.
    Utf8,

    /// SGR-like encoding.
    Sgr,
    // Urxvt,
}

impl Default for MouseProtocolEncoding {
    fn default() -> Self {
        Self::Default
    }
}

/// Represents the overall terminal state.
#[derive(Clone, Debug)]
pub struct Screen {
    grid: crate::grid::Grid,
    alternate_grid: crate::grid::Grid,

    attrs: crate::attrs::Attrs,
    saved_attrs: crate::attrs::Attrs,

    title: String,
    icon_name: String,

    modes: u8,
    mouse_protocol_mode: MouseProtocolMode,
    mouse_protocol_encoding: MouseProtocolEncoding,

    audible_bell_count: usize,
    visual_bell_count: usize,

    errors: usize,

    // roost: pending stream effects (SPEC-parity W3), drained by the
    // embedder via `take_effects` after each `process` call.
    effects: Vec<Effect>,
    // roost: the screen as it was when the currently-open synchronized-output
    // bracket (mode 2026) opened — captured at the exact stream position of
    // the `?2026h`, so an embedder can keep presenting the last *complete*
    // frame while the application redraws (SPEC-parity P1). `None` outside a
    // bracket, and once the embedder has taken it. Deliberately built with
    // `snapshot` rather than `clone`: no banked history rides along (see
    // `Screen::snapshot`), and the copy's own `sync_snapshot`/`effects` are
    // always empty — no recursion, no double-delivered effects.
    sync_snapshot: Option<Box<Screen>>,
    // roost: the last graphic character actually printed, for REP
    // (`CSI Ps b`) — SPEC-parity P19. `None` until something is printed, and
    // a REP in that state is a no-op, as the spec requires.
    last_graphic: Option<char>,
}

impl Screen {
    pub(crate) fn new(
        size: crate::grid::Size,
        scrollback_len: usize,
    ) -> Self {
        let mut grid = crate::grid::Grid::new(size, scrollback_len);
        grid.allocate_rows();
        Self {
            grid,
            alternate_grid: crate::grid::Grid::new(size, 0),

            attrs: crate::attrs::Attrs::default(),
            saved_attrs: crate::attrs::Attrs::default(),

            title: String::default(),
            icon_name: String::default(),

            modes: 0,
            mouse_protocol_mode: MouseProtocolMode::default(),
            mouse_protocol_encoding: MouseProtocolEncoding::default(),

            audible_bell_count: 0,
            visual_bell_count: 0,

            errors: 0,

            effects: Vec::new(),
            sync_snapshot: None,
            last_graphic: None,
        }
    }

    /// A copy of this screen carrying only what it takes to *present* the
    /// current frame: the visible grids, cursor, attrs, title and modes.
    /// The banked scrollback is deliberately left behind — a full `clone`
    /// would deep-copy up to `scrollback_len` rows (megabytes) every time an
    /// application opens a synchronized-output bracket, i.e. once per redraw
    /// on a spinner. Every field is spelled out so that a new one added to
    /// `Screen` fails to compile here rather than being silently dropped.
    ///
    /// Public since roost's C29 selection-freeze amendment: a native-
    /// selection gesture (`infra::pty::PtyPane::freeze_view`) uses this same
    /// cheap, scrollback-free copy to hold the presented frame steady for
    /// the gesture's duration, exactly the way the sync-output bracket below
    /// already does — one snapshot mechanism, two callers.
    // roost: added for SPEC-parity P1; made `pub` for DESIGN-ui.md C29's
    // selection-freeze amendment.
    #[must_use]
    pub fn snapshot(&self) -> Self {
        Self {
            grid: self.grid.snapshot(),
            alternate_grid: self.alternate_grid.snapshot(),

            attrs: self.attrs,
            saved_attrs: self.saved_attrs,

            title: self.title.clone(),
            icon_name: self.icon_name.clone(),

            modes: self.modes,
            mouse_protocol_mode: self.mouse_protocol_mode,
            mouse_protocol_encoding: self.mouse_protocol_encoding,

            audible_bell_count: self.audible_bell_count,
            visual_bell_count: self.visual_bell_count,

            errors: self.errors,

            // A snapshot is a presentation artifact, never a second event
            // source: effects belong to the live screen alone.
            effects: Vec::new(),
            sync_snapshot: None,
            last_graphic: self.last_graphic,
        }
    }

    /// roost (SPEC-parity P5): the primary grid *reflows* — its logical lines
    /// are rewrapped to the new width, in both directions, so nothing is lost
    /// when a pane is resized (zoom deliberately resizes a pane's PTY to the
    /// full body and back).
    ///
    /// The alternate grid deliberately does not. An alternate-screen
    /// application owns every cell of its canvas and repaints it on SIGWINCH;
    /// rewrapping underneath would fight the redraw it is already sending, and
    /// no real terminal reflows there either. The rule is per grid, not per
    /// mode: the primary grid holds the shell's output whether or not an
    /// application is currently borrowing the screen, and it rewraps either
    /// way — so what the shell sees on `?1049l` is the same content it would
    /// have had if the app had never run.
    pub(crate) fn set_size(&mut self, rows: u16, cols: u16) {
        self.grid
            .set_size_reflowing(crate::grid::Size { rows, cols });
        self.alternate_grid
            .set_size(crate::grid::Size { rows, cols });
    }

    /// Returns the current size of the terminal.
    ///
    /// The return value will be (rows, cols).
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        let size = self.grid().size();
        (size.rows, size.cols)
    }

    /// Returns the current position in the scrollback.
    ///
    /// This position indicates the offset from the top of the screen, and is
    /// `0` when the normal screen is in view.
    #[must_use]
    pub fn scrollback(&self) -> usize {
        self.grid().scrollback()
    }

    /// Returns how many rows of scrollback are currently stored.
    ///
    /// This counts banked history rows, not the configured capacity;
    /// `scrollback()` can never exceed it.
    // roost: added for the scroll position hint (SPEC-ux U3) — the offset's
    // honest upper bound (`↑N/M`'s M).
    #[must_use]
    pub fn scrollback_rows(&self) -> usize {
        self.grid().scrollback_rows()
    }

    pub(crate) fn set_scrollback(&mut self, rows: usize) {
        self.grid_mut().set_scrollback(rows);
    }

    /// Returns the text contents of the terminal.
    ///
    /// This will not include any formatting information, and will be in plain
    /// text format.
    #[must_use]
    pub fn contents(&self) -> String {
        let mut contents = String::new();
        self.write_contents(&mut contents);
        contents
    }

    fn write_contents(&self, contents: &mut String) {
        self.grid().write_contents(contents);
    }

    /// Returns the text contents of the terminal by row, restricted to the
    /// given subset of columns.
    ///
    /// This will not include any formatting information, and will be in plain
    /// text format.
    ///
    /// Newlines will not be included.
    pub fn rows(
        &self,
        start: u16,
        width: u16,
    ) -> impl Iterator<Item = String> + '_ {
        self.grid().visible_rows().map(move |row| {
            let mut contents = String::new();
            row.write_contents(&mut contents, start, width, false);
            contents
        })
    }

    /// Returns the text contents of the entire buffer: scrollback history
    /// followed by the current screen (`rows`/`contents` only cover the
    /// visible window). Trailing spaces are trimmed per line, and wholly
    /// blank trailing lines are trimmed from the end so a mostly-empty
    /// screen doesn't return hundreds of blank lines.
    ///
    /// This will not include any formatting information, and will be in plain
    /// text format.
    #[must_use]
    pub fn all_contents(&self) -> String {
        let (_, cols) = self.size();
        let mut lines: Vec<String> = self
            .grid()
            .all_rows()
            .map(|row| {
                let mut contents = String::new();
                row.write_contents(&mut contents, 0, cols, false);
                while contents.ends_with(' ') {
                    contents.pop();
                }
                contents
            })
            .collect();
        while lines.last().map_or(false, String::is_empty) {
            lines.pop();
        }
        lines.join("\n")
    }

    /// Returns the current cursor position of the terminal.
    ///
    /// The return value will be (row, col).
    #[must_use]
    pub fn cursor_position(&self) -> (u16, u16) {
        let pos = self.grid().pos();
        (pos.row, pos.col)
    }

    /// Returns the `Cell` object at the given location in the terminal, if it
    /// exists.
    #[must_use]
    pub fn cell(&self, row: u16, col: u16) -> Option<&crate::cell::Cell> {
        self.grid().visible_cell(crate::grid::Pos { row, col })
    }

    /// Returns whether the text in row `row` should wrap to the next line.
    #[must_use]
    pub fn row_wrapped(&self, row: u16) -> bool {
        self.grid()
            .visible_row(row)
            .map_or(false, crate::row::Row::wrapped)
    }

    /// Returns the terminal's window title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns a value which changes every time an audible bell is received.
    ///
    /// Typically you would store this number after each call to `process`,
    /// and trigger an audible bell whenever it changes.
    #[must_use]
    pub fn audible_bell_count(&self) -> usize {
        self.audible_bell_count
    }

    /// Returns whether the alternate screen is currently in use.
    #[must_use]
    pub fn alternate_screen(&self) -> bool {
        self.mode(MODE_ALTERNATE_SCREEN)
    }

    /// Returns whether the terminal should be in application cursor mode.
    #[must_use]
    pub fn application_cursor(&self) -> bool {
        self.mode(MODE_APPLICATION_CURSOR)
    }

    /// Returns whether the terminal should be in hide cursor mode.
    #[must_use]
    pub fn hide_cursor(&self) -> bool {
        self.mode(MODE_HIDE_CURSOR)
    }

    /// Returns whether the terminal should be in bracketed paste mode.
    #[must_use]
    pub fn bracketed_paste(&self) -> bool {
        self.mode(MODE_BRACKETED_PASTE)
    }

    /// Returns whether a synchronized-output bracket is open (DEC private
    /// mode 2026, `CSI ?2026h` … `CSI ?2026l`): the application has asked
    /// for its in-progress redraw not to be presented until it closes.
    // roost: added for SPEC-parity P1.
    #[must_use]
    pub fn synchronized_output(&self) -> bool {
        self.mode(MODE_SYNCHRONIZED_OUTPUT)
    }

    /// Returns whether the application asked to be told about focus changes
    /// (DEC private mode 1004, `CSI ?1004h` … `CSI ?1004l`) — i.e. whether
    /// it expects `CSI I` on focus-in and `CSI O` on focus-out.
    ///
    /// The screen only *records* the subscription: it has no idea what has
    /// focus, so producing the reports is the embedder's job. An application
    /// that never asked must never be sent them (the bytes would land in its
    /// stdin as garbage input).
    // roost: added for SPEC-parity P10.
    #[must_use]
    pub fn focus_events(&self) -> bool {
        self.mode(MODE_FOCUS_EVENT)
    }

    /// Takes (clearing) the screen as it stood the instant the current
    /// synchronized-output bracket opened. `Some` exactly once per bracket
    /// that opened in the bytes processed since the last call; the caller
    /// then owns that last-complete frame and may present it until
    /// `synchronized_output` reports the bracket closed — with its own
    /// staleness policy for a bracket that never does.
    // roost: added for SPEC-parity P1.
    #[must_use]
    pub fn take_sync_snapshot(&mut self) -> Option<Self> {
        self.sync_snapshot.take().map(|s| *s)
    }

    /// Drains the side effects (notifications, clipboard writes, cursor
    /// shapes) requested by the bytes processed since the last call, in
    /// stream order.
    // roost: added for SPEC-parity W3.
    #[must_use]
    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    /// Returns the currently active `MouseProtocolMode`
    #[must_use]
    pub fn mouse_protocol_mode(&self) -> MouseProtocolMode {
        self.mouse_protocol_mode
    }

    /// Returns the currently active `MouseProtocolEncoding`
    #[must_use]
    pub fn mouse_protocol_encoding(&self) -> MouseProtocolEncoding {
        self.mouse_protocol_encoding
    }

    /// Returns the currently active foreground color.
    #[must_use]
    pub fn fgcolor(&self) -> crate::attrs::Color {
        self.attrs.fgcolor
    }

    /// Returns the currently active background color.
    #[must_use]
    pub fn bgcolor(&self) -> crate::attrs::Color {
        self.attrs.bgcolor
    }

    /// Returns whether newly drawn text should be rendered with the bold text
    /// attribute.
    #[must_use]
    pub fn bold(&self) -> bool {
        self.attrs.bold()
    }

    /// Returns whether newly drawn text should be rendered with the italic
    /// text attribute.
    #[must_use]
    pub fn italic(&self) -> bool {
        self.attrs.italic()
    }

    /// Returns whether newly drawn text should be rendered with the
    /// underlined text attribute.
    #[must_use]
    pub fn underline(&self) -> bool {
        self.attrs.underline()
    }

    /// Returns whether newly drawn text should be rendered with the inverse
    /// text attribute.
    #[must_use]
    pub fn inverse(&self) -> bool {
        self.attrs.inverse()
    }

    fn grid(&self) -> &crate::grid::Grid {
        if self.mode(MODE_ALTERNATE_SCREEN) {
            &self.alternate_grid
        } else {
            &self.grid
        }
    }

    fn grid_mut(&mut self) -> &mut crate::grid::Grid {
        if self.mode(MODE_ALTERNATE_SCREEN) {
            &mut self.alternate_grid
        } else {
            &mut self.grid
        }
    }

    fn enter_alternate_grid(&mut self) {
        self.grid_mut().set_scrollback(0);
        self.set_mode(MODE_ALTERNATE_SCREEN);
        self.alternate_grid.allocate_rows();
    }

    fn exit_alternate_grid(&mut self) {
        self.clear_mode(MODE_ALTERNATE_SCREEN);
    }

    fn save_cursor(&mut self) {
        self.grid_mut().save_cursor();
        self.saved_attrs = self.attrs;
    }

    fn restore_cursor(&mut self) {
        self.grid_mut().restore_cursor();
        self.attrs = self.saved_attrs;
    }

    fn set_mode(&mut self, mode: u8) {
        self.modes |= mode;
    }

    fn clear_mode(&mut self, mode: u8) {
        self.modes &= !mode;
    }

    fn mode(&self, mode: u8) -> bool {
        self.modes & mode != 0
    }

    fn set_mouse_mode(&mut self, mode: MouseProtocolMode) {
        self.mouse_protocol_mode = mode;
    }

    fn clear_mouse_mode(&mut self, mode: MouseProtocolMode) {
        if self.mouse_protocol_mode == mode {
            self.mouse_protocol_mode = MouseProtocolMode::default();
        }
    }

    fn set_mouse_encoding(&mut self, encoding: MouseProtocolEncoding) {
        self.mouse_protocol_encoding = encoding;
    }

    fn clear_mouse_encoding(&mut self, encoding: MouseProtocolEncoding) {
        if self.mouse_protocol_encoding == encoding {
            self.mouse_protocol_encoding = MouseProtocolEncoding::default();
        }
    }
}

impl Screen {
    fn text(&mut self, c: char) {
        let pos = self.grid().pos();
        let size = self.grid().size();
        let attrs = self.attrs;

        let width = c.width();
        if width.is_none() && (u32::from(c)) < 256 {
            // don't even try to draw control characters
            return;
        }
        let width: u16 = width
            .unwrap_or(1)
            .try_into()
            // width() can only return 0, 1, or 2
            .unwrap();
        // roost hardening: a wide char needs a continuation cell to its
        // right, and every invariant below (and `Cell::set`, which marks the
        // cell wide from the char alone) assumes that cell exists. In a
        // 1-column grid it does not: `col_wrap` cannot make room, so the
        // draw ran off the end and `drawing_cell(pos).unwrap()` panicked —
        // which unwinds out of the event loop past `App::shutdown`, killing
        // roost and leaving every agent running. A pane one column wide is
        // reachable by dragging a terminal narrow, and any emoji or CJK
        // glyph in it was enough. Real terminals cannot show the glyph
        // either; dropping it is the honest degradation.
        if width > size.cols {
            return;
        }

        // it doesn't make any sense to wrap if the last column in a row
        // didn't already have contents. don't try to handle the case where a
        // character wraps because there was only one column left in the
        // previous row - literally everything handles this case differently,
        // and this is tmux behavior (and also the simplest). i'm open to
        // reconsidering this behavior, but only with a really good reason
        // (xterm handles this by introducing the concept of triple width
        // cells, which i really don't want to do).
        let mut wrap = false;
        // roost hardening: saturating — `cols - width` underflows for a wide
        // char in a 1-col grid (grid size is already clamped to >= 1x1).
        if pos.col > size.cols.saturating_sub(width) {
            let last_cell = self
                .grid()
                .drawing_cell(crate::grid::Pos {
                    row: pos.row,
                    col: size.cols - 1,
                })
                // pos.row is valid, since it comes directly from
                // self.grid().pos() which we assume to always have a valid
                // row value. size.cols - 1 is also always a valid column.
                .unwrap();
            if last_cell.has_contents() || last_cell.is_wide_continuation() {
                wrap = true;
            }
        }
        self.grid_mut().col_wrap(width, wrap);
        let pos = self.grid().pos();

        if width == 0 {
            if pos.col > 0 {
                // roost (SPEC-parity P17): track which column the appended
                // codepoint actually landed in — a completed emoji-
                // presentation sequence has to widen *that* cell.
                let mut base_col = pos.col - 1;
                let mut prev_cell = self
                    .grid_mut()
                    .drawing_cell_mut(crate::grid::Pos {
                        row: pos.row,
                        col: pos.col - 1,
                    })
                    // pos.row is valid, since it comes directly from
                    // self.grid().pos() which we assume to always have a
                    // valid row value. pos.col - 1 is valid because we just
                    // checked for pos.col > 0.
                    .unwrap();
                if prev_cell.is_wide_continuation() {
                    base_col = pos.col - 2;
                    prev_cell = self
                        .grid_mut()
                        .drawing_cell_mut(crate::grid::Pos {
                            row: pos.row,
                            col: pos.col - 2,
                        })
                        // pos.row is valid, since it comes directly from
                        // self.grid().pos() which we assume to always have a
                        // valid row value. we know pos.col - 2 is valid
                        // because the cell at pos.col - 1 is a wide
                        // continuation character, which means there must be
                        // the first half of the wide character before it.
                        .unwrap();
                }
                let wants_wide = prev_cell.append(c);
                // roost (SPEC-parity P17): the sequence just became two
                // columns wide. Claim the continuation and step the cursor
                // past it, exactly as a natively-wide char does.
                if wants_wide && self.widen_cell(pos.row, base_col) {
                    self.grid_mut().col_inc(1);
                }
            } else if pos.row > 0 {
                let prev_row = self
                    .grid()
                    .drawing_row(pos.row - 1)
                    // pos.row is valid, since it comes directly from
                    // self.grid().pos() which we assume to always have a
                    // valid row value. pos.row - 1 is valid because we just
                    // checked for pos.row > 0.
                    .unwrap();
                if prev_row.wrapped() {
                    let mut prev_cell = self
                        .grid_mut()
                        .drawing_cell_mut(crate::grid::Pos {
                            row: pos.row - 1,
                            col: size.cols - 1,
                        })
                        // pos.row is valid, since it comes directly from
                        // self.grid().pos() which we assume to always have a
                        // valid row value. pos.row - 1 is valid because we
                        // just checked for pos.row > 0. col of size.cols - 1
                        // is always valid.
                        .unwrap();
                    if prev_cell.is_wide_continuation() {
                        prev_cell = self
                            .grid_mut()
                            .drawing_cell_mut(crate::grid::Pos {
                                row: pos.row - 1,
                                col: size.cols - 2,
                            })
                            // pos.row is valid, since it comes directly from
                            // self.grid().pos() which we assume to always
                            // have a valid row value. pos.row - 1 is valid
                            // because we just checked for pos.row > 0. col of
                            // size.cols - 2 is valid because the cell at
                            // size.cols - 1 is a wide continuation character,
                            // so it must have the first half of the wide
                            // character before it.
                            .unwrap();
                    }
                    // roost (SPEC-parity P17): a promotion is refused here by
                    // construction — the base cell is the previous row's last
                    // column, so there is no cell to its right to claim. The
                    // sequence stays one column, which is the pre-P17
                    // behavior and the same pragmatism upstream applies to
                    // wide chars that don't fit at a row's end.
                    let _ = prev_cell.append(c);
                }
            }
        } else {
            // roost (SPEC-parity P19): REP repeats the last *graphic*
            // character, so it is recorded here — on the path that actually
            // occupies a cell — and not for the zero-width branch above.
            self.last_graphic = Some(c);
            if self
                .grid()
                .drawing_cell(pos)
                // pos.row is valid because we assume self.grid().pos() to
                // always have a valid row value. pos.col is valid because we
                // called col_wrap() immediately before this, which ensures
                // that self.grid().pos().col has a valid value.
                .unwrap()
                .is_wide_continuation()
            {
                let prev_cell = self
                    .grid_mut()
                    .drawing_cell_mut(crate::grid::Pos {
                        row: pos.row,
                        col: pos.col - 1,
                    })
                    // pos.row is valid because we assume self.grid().pos() to
                    // always have a valid row value. pos.col is valid because
                    // we called col_wrap() immediately before this, which
                    // ensures that self.grid().pos().col has a valid value.
                    // pos.col - 1 is valid because the cell at pos.col is a
                    // wide continuation character, so it must have the first
                    // half of the wide character before it.
                    .unwrap();
                prev_cell.clear(attrs);
            }

            if self
                .grid()
                .drawing_cell(pos)
                // pos.row is valid because we assume self.grid().pos() to
                // always have a valid row value. pos.col is valid because we
                // called col_wrap() immediately before this, which ensures
                // that self.grid().pos().col has a valid value.
                .unwrap()
                .is_wide()
            {
                let next_cell = self
                    .grid_mut()
                    .drawing_cell_mut(crate::grid::Pos {
                        row: pos.row,
                        col: pos.col + 1,
                    })
                    // pos.row is valid because we assume self.grid().pos() to
                    // always have a valid row value. pos.col is valid because
                    // we called col_wrap() immediately before this, which
                    // ensures that self.grid().pos().col has a valid value.
                    // pos.col + 1 is valid because the cell at pos.col is a
                    // wide character, so it must have the second half of the
                    // wide character after it.
                    .unwrap();
                next_cell.set(' ', attrs);
            }

            let cell = self
                .grid_mut()
                .drawing_cell_mut(pos)
                // pos.row is valid because we assume self.grid().pos() to
                // always have a valid row value. pos.col is valid because we
                // called col_wrap() immediately before this, which ensures
                // that self.grid().pos().col has a valid value.
                .unwrap();
            cell.set(c, attrs);
            self.grid_mut().col_inc(1);
            if width > 1 {
                let pos = self.grid().pos();
                if self
                    .grid()
                    .drawing_cell(pos)
                    // pos.row is valid because we assume self.grid().pos() to
                    // always have a valid row value. pos.col is valid because
                    // we called col_wrap() earlier, which ensures that
                    // self.grid().pos().col has a valid value. this is true
                    // even though we just called col_inc, because this branch
                    // only happens if width > 1, and col_wrap takes width
                    // into account.
                    .unwrap()
                    .is_wide()
                {
                    let next_next_pos = crate::grid::Pos {
                        row: pos.row,
                        col: pos.col + 1,
                    };
                    let next_next_cell = self
                        .grid_mut()
                        .drawing_cell_mut(next_next_pos)
                        // pos.row is valid because we assume
                        // self.grid().pos() to always have a valid row value.
                        // pos.col is valid because we called col_wrap()
                        // earlier, which ensures that self.grid().pos().col
                        // has a valid value. this is true even though we just
                        // called col_inc, because this branch only happens if
                        // width > 1, and col_wrap takes width into account.
                        // pos.col + 1 is valid because the cell at pos.col is
                        // wide, and so it must have the second half of the
                        // wide character after it.
                        .unwrap();
                    next_next_cell.clear(attrs);
                    if next_next_pos.col == size.cols - 1 {
                        self.grid_mut()
                            .drawing_row_mut(pos.row)
                            // we assume self.grid().pos().row is always valid
                            .unwrap()
                            .wrap(false);
                    }
                }
                let next_cell = self
                    .grid_mut()
                    .drawing_cell_mut(pos)
                    // pos.row is valid because we assume self.grid().pos() to
                    // always have a valid row value. pos.col is valid because
                    // we called col_wrap() earlier, which ensures that
                    // self.grid().pos().col has a valid value. this is true
                    // even though we just called col_inc, because this branch
                    // only happens if width > 1, and col_wrap takes width
                    // into account.
                    .unwrap();
                next_cell.clear(crate::attrs::Attrs::default());
                next_cell.set_wide_continuation(true);
                self.grid_mut().col_inc(1);
            }
        }
    }

    /// REP — repeat the preceding graphic character (`CSI Ps b`).
    ///
    /// roost (SPEC-parity P19): `TERM=xterm-256color` advertises `rep` and
    /// ncurses 6 emits it, so having no `b` arm silently swallowed whole runs
    /// of repeated glyphs — `"ab"` + `CSI 5 b` rendered `"ab"` where every
    /// other terminal shows `"abbbbbb"`.
    ///
    /// Deliberately implemented by replaying the character through `text`
    /// rather than by writing cells directly: that is what makes the repeat
    /// inherit the *current* attributes and obey wrapping, scroll regions,
    /// insert mode and wide-char placement for free, instead of duplicating
    /// (and eventually diverging from) those rules here. A REP before anything
    /// has been printed is a no-op, as the spec requires. The count is a `u16`
    /// by construction — vte's parameters are `u16` — which bounds the work a
    /// single sequence can ask for.
    ///
    /// Known limitation, shared with xterm: the repeated unit is the last base
    /// character, not the last *cell*, so repeating a combining sequence
    /// (`e` + U+0301) or an emoji-presentation sequence repeats only the base
    /// char. Applications that use REP emit it for runs of plain glyphs.
    fn rep(&mut self, count: u16) {
        let Some(c) = self.last_graphic else { return };
        for _ in 0..count {
            self.text(c);
        }
    }

    /// roost (SPEC-parity P17): promote an already-drawn cell from one column
    /// to two, claiming the cell to its right as the wide continuation.
    ///
    /// Emoji-presentation sequences (base char + VS16) measure two columns
    /// under unicode-width 0.2 — the table roost and ratatui use — but reach
    /// the parser as a printable char followed by a zero-width one, so the
    /// grid only learns the true width when the VS16 lands. Returns whether
    /// the promotion happened: it is refused when the row has no cell to the
    /// right to give up, leaving the sequence one column wide (the pre-P17
    /// behavior). Refusing rather than half-promoting is load-bearing — a
    /// wide flag without a continuation cell is an invariant the grid later
    /// dereferences unconditionally.
    fn widen_cell(&mut self, row: u16, col: u16) -> bool {
        let size = self.grid().size();
        if col + 1 >= size.cols {
            return false;
        }
        let next_pos = crate::grid::Pos { row, col: col + 1 };

        // A wide char may already straddle the cell we are claiming; its own
        // continuation would be orphaned, so clear it first (the same repair
        // `text` performs when a narrow char lands on a wide one).
        let next_was_wide = self
            .grid()
            .drawing_cell(next_pos)
            .is_some_and(crate::cell::Cell::is_wide);
        if next_was_wide {
            let orphan_pos = crate::grid::Pos { row, col: col + 2 };
            if let Some(orphan) = self.grid_mut().drawing_cell_mut(orphan_pos)
            {
                orphan.clear(crate::attrs::Attrs::default());
                orphan.set_wide_continuation(false);
            }
        }

        if let Some(next) = self.grid_mut().drawing_cell_mut(next_pos) {
            next.clear(crate::attrs::Attrs::default());
            next.set_wide_continuation(true);
        }
        if let Some(base) =
            self.grid_mut().drawing_cell_mut(crate::grid::Pos { row, col })
        {
            base.promote_wide();
        }
        if next_pos.col == size.cols - 1 {
            if let Some(r) = self.grid_mut().drawing_row_mut(row) {
                r.wrap(false);
            }
        }
        true
    }

    // control codes

    fn bel(&mut self) {
        self.audible_bell_count += 1;
    }

    fn bs(&mut self) {
        self.grid_mut().col_dec(1);
    }

    fn tab(&mut self) {
        self.grid_mut().col_tab();
    }

    fn lf(&mut self) {
        self.grid_mut().row_inc_scroll(1);
    }

    fn vt(&mut self) {
        self.lf();
    }

    fn ff(&mut self) {
        self.lf();
    }

    fn cr(&mut self) {
        self.grid_mut().col_set(0);
    }

    // escape codes

    // ESC 7
    fn decsc(&mut self) {
        self.save_cursor();
    }

    // ESC 8
    fn decrc(&mut self) {
        self.restore_cursor();
    }

    // ESC =
    fn deckpam(&mut self) {
        self.set_mode(MODE_APPLICATION_KEYPAD);
    }

    // ESC >
    fn deckpnm(&mut self) {
        self.clear_mode(MODE_APPLICATION_KEYPAD);
    }

    // ESC M
    fn ri(&mut self) {
        self.grid_mut().row_dec_scroll(1);
    }

    // ESC c
    fn ris(&mut self) {
        let title = self.title.clone();
        let icon_name = self.icon_name.clone();
        let audible_bell_count = self.audible_bell_count;
        let visual_bell_count = self.visual_bell_count;
        let errors = self.errors;
        // roost: pending effects survive a reset the way the bell counts do
        // — they are signals already handed to the embedder's queue, not
        // screen state. The sync snapshot does NOT survive: a full reset
        // ends any open bracket, so there is no frame left to present.
        let effects = std::mem::take(&mut self.effects);

        *self = Self::new(self.grid.size(), self.grid.scrollback_len());

        self.title = title;
        self.icon_name = icon_name;
        self.audible_bell_count = audible_bell_count;
        self.visual_bell_count = visual_bell_count;
        self.errors = errors;
        self.effects = effects;
    }

    // ESC g
    fn vb(&mut self) {
        self.visual_bell_count += 1;
    }

    // csi codes

    // CSI @
    fn ich(&mut self, count: u16) {
        self.grid_mut().insert_cells(count);
    }

    // CSI A
    fn cuu(&mut self, offset: u16) {
        self.grid_mut().row_dec_clamp(offset);
    }

    // CSI B
    fn cud(&mut self, offset: u16) {
        self.grid_mut().row_inc_clamp(offset);
    }

    // CSI C
    fn cuf(&mut self, offset: u16) {
        self.grid_mut().col_inc_clamp(offset);
    }

    // CSI D
    fn cub(&mut self, offset: u16) {
        self.grid_mut().col_dec(offset);
    }

    // CSI G
    fn cha(&mut self, col: u16) {
        self.grid_mut().col_set(col - 1);
    }

    // CSI H
    fn cup(&mut self, (row, col): (u16, u16)) {
        self.grid_mut().set_pos(crate::grid::Pos {
            row: row - 1,
            col: col - 1,
        });
    }

    // CSI J
    fn ed(&mut self, mode: u16) {
        let attrs = self.attrs;
        match mode {
            0 => self.grid_mut().erase_all_forward(attrs),
            1 => self.grid_mut().erase_all_backward(attrs),
            2 => self.grid_mut().erase_all(attrs),
            n => {
                log::debug!("unhandled ED mode: {n}");
            }
        }
    }

    // CSI ? J
    fn decsed(&mut self, mode: u16) {
        self.ed(mode);
    }

    // CSI K
    fn el(&mut self, mode: u16) {
        let attrs = self.attrs;
        match mode {
            0 => self.grid_mut().erase_row_forward(attrs),
            1 => self.grid_mut().erase_row_backward(attrs),
            2 => self.grid_mut().erase_row(attrs),
            n => {
                log::debug!("unhandled EL mode: {n}");
            }
        }
    }

    // CSI ? K
    fn decsel(&mut self, mode: u16) {
        self.el(mode);
    }

    // CSI L
    fn il(&mut self, count: u16) {
        self.grid_mut().insert_lines(count);
    }

    // CSI M
    fn dl(&mut self, count: u16) {
        self.grid_mut().delete_lines(count);
    }

    // CSI P
    fn dch(&mut self, count: u16) {
        self.grid_mut().delete_cells(count);
    }

    // CSI S
    fn su(&mut self, count: u16) {
        self.grid_mut().scroll_up(count);
    }

    // CSI T
    fn sd(&mut self, count: u16) {
        self.grid_mut().scroll_down(count);
    }

    // CSI X
    fn ech(&mut self, count: u16) {
        let attrs = self.attrs;
        self.grid_mut().erase_cells(count, attrs);
    }

    // CSI d
    fn vpa(&mut self, row: u16) {
        self.grid_mut().row_set(row - 1);
    }

    // CSI h
    #[allow(clippy::unused_self)]
    fn sm(&mut self, params: &vte::Params) {
        // nothing, i think?
        if log::log_enabled!(log::Level::Debug) {
            log::debug!("unhandled SM mode: {}", param_str(params));
        }
    }

    // CSI ? h
    fn decset(&mut self, params: &vte::Params) {
        for param in params {
            match param {
                &[1] => self.set_mode(MODE_APPLICATION_CURSOR),
                &[6] => self.grid_mut().set_origin_mode(true),
                &[9] => self.set_mouse_mode(MouseProtocolMode::Press),
                &[25] => self.clear_mode(MODE_HIDE_CURSOR),
                &[47] => self.enter_alternate_grid(),
                &[1000] => {
                    self.set_mouse_mode(MouseProtocolMode::PressRelease);
                }
                &[1002] => {
                    self.set_mouse_mode(MouseProtocolMode::ButtonMotion);
                }
                &[1003] => self.set_mouse_mode(MouseProtocolMode::AnyMotion),
                // roost: focus reporting (SPEC-parity P10) — the app wants
                // `CSI I`/`CSI O` when it gains/loses focus. Recorded here,
                // acted on by the embedder (see `focus_events`).
                &[1004] => self.set_mode(MODE_FOCUS_EVENT),
                &[1005] => {
                    self.set_mouse_encoding(MouseProtocolEncoding::Utf8);
                }
                &[1006] => {
                    self.set_mouse_encoding(MouseProtocolEncoding::Sgr);
                }
                &[1049] => {
                    self.decsc();
                    self.alternate_grid.clear();
                    self.enter_alternate_grid();
                }
                &[2004] => self.set_mode(MODE_BRACKETED_PASTE),
                // roost: synchronized output (SPEC-parity P1). Opening a
                // bracket captures the screen at this exact stream position
                // — the last frame the application considered complete —
                // for the embedder to keep presenting while the redraw
                // runs. A redundant `?2026h` *inside* an open bracket must
                // NOT re-capture: the screen is mid-redraw (torn) by then.
                &[2026] => {
                    if !self.mode(MODE_SYNCHRONIZED_OUTPUT) {
                        self.sync_snapshot = Some(Box::new(self.snapshot()));
                    }
                    self.set_mode(MODE_SYNCHRONIZED_OUTPUT);
                }
                ns => {
                    if log::log_enabled!(log::Level::Debug) {
                        let n = if ns.len() == 1 {
                            format!(
                                "{}",
                                // we just checked that ns.len() == 1, so 0
                                // must be valid
                                ns[0]
                            )
                        } else {
                            format!("{ns:?}")
                        };
                        log::debug!("unhandled DECSET mode: {n}");
                    }
                }
            }
        }
    }

    // CSI l
    #[allow(clippy::unused_self)]
    fn rm(&mut self, params: &vte::Params) {
        // nothing, i think?
        if log::log_enabled!(log::Level::Debug) {
            log::debug!("unhandled RM mode: {}", param_str(params));
        }
    }

    // CSI ? l
    fn decrst(&mut self, params: &vte::Params) {
        for param in params {
            match param {
                &[1] => self.clear_mode(MODE_APPLICATION_CURSOR),
                &[6] => self.grid_mut().set_origin_mode(false),
                &[9] => self.clear_mouse_mode(MouseProtocolMode::Press),
                &[25] => self.set_mode(MODE_HIDE_CURSOR),
                &[47] => {
                    self.exit_alternate_grid();
                }
                &[1000] => {
                    self.clear_mouse_mode(MouseProtocolMode::PressRelease);
                }
                &[1002] => {
                    self.clear_mouse_mode(MouseProtocolMode::ButtonMotion);
                }
                &[1003] => {
                    self.clear_mouse_mode(MouseProtocolMode::AnyMotion);
                }
                // roost: end of the focus-reporting subscription (P10).
                &[1004] => self.clear_mode(MODE_FOCUS_EVENT),
                &[1005] => {
                    self.clear_mouse_encoding(MouseProtocolEncoding::Utf8);
                }
                &[1006] => {
                    self.clear_mouse_encoding(MouseProtocolEncoding::Sgr);
                }
                &[1049] => {
                    self.exit_alternate_grid();
                    self.decrc();
                }
                &[2004] => self.clear_mode(MODE_BRACKETED_PASTE),
                // roost: closing the synchronized-output bracket
                // (SPEC-parity P1) — the redraw finished, so the live grid
                // *is* the frame to present; drop any capture the embedder
                // hasn't taken rather than leave a stale one behind.
                &[2026] => {
                    self.clear_mode(MODE_SYNCHRONIZED_OUTPUT);
                    self.sync_snapshot = None;
                }
                ns => {
                    if log::log_enabled!(log::Level::Debug) {
                        let n = if ns.len() == 1 {
                            format!(
                                "{}",
                                // we just checked that ns.len() == 1, so 0
                                // must be valid
                                ns[0]
                            )
                        } else {
                            format!("{ns:?}")
                        };
                        log::debug!("unhandled DECRST mode: {n}");
                    }
                }
            }
        }
    }

    // CSI Ps SP q
    // roost: DECSCUSR (SPEC-parity P7). Reported faithfully, including
    // shapes outside 0..=6 — deciding what an unknown one means belongs to
    // the embedder that owns the real terminal, not to the parser.
    fn decscusr(&mut self, shape: u16) {
        if let Some(shape) = u16_to_u8(shape) {
            self.effects.push(Effect::CursorShape(shape));
        }
    }

    // CSI m
    fn sgr(&mut self, params: &vte::Params) {
        // XXX really i want to just be able to pass in a default Params
        // instance with a 0 in it, but vte doesn't allow creating new Params
        // instances
        if params.is_empty() {
            self.attrs = crate::attrs::Attrs::default();
            return;
        }

        let mut iter = params.iter();

        macro_rules! next_param {
            () => {
                match iter.next() {
                    Some(n) => n,
                    _ => return,
                }
            };
        }

        macro_rules! to_u8 {
            ($n:expr) => {
                if let Some(n) = u16_to_u8($n) {
                    n
                } else {
                    return;
                }
            };
        }

        macro_rules! next_param_u8 {
            () => {
                if let &[n] = next_param!() {
                    to_u8!(n)
                } else {
                    return;
                }
            };
        }

        loop {
            match next_param!() {
                &[0] => self.attrs = crate::attrs::Attrs::default(),
                &[1] => self.attrs.set_bold(true),
                // roost (SPEC-parity P16): faint/dim and strikethrough.
                &[2] => self.attrs.set_dim(true),
                &[3] => self.attrs.set_italic(true),
                &[4] => self.attrs.set_underline(true),
                &[7] => self.attrs.set_inverse(true),
                &[9] => self.attrs.set_strikethrough(true),
                // SGR 22 is "normal intensity" in ECMA-48: it ends *both*
                // bold and faint, which is why there is no separate reset for
                // dim alone. Matching the existing 23/24/27 shape otherwise.
                &[22] => {
                    self.attrs.set_bold(false);
                    self.attrs.set_dim(false);
                }
                &[23] => self.attrs.set_italic(false),
                &[24] => self.attrs.set_underline(false),
                &[27] => self.attrs.set_inverse(false),
                &[29] => self.attrs.set_strikethrough(false),
                &[n] if (30..=37).contains(&n) => {
                    self.attrs.fgcolor =
                        crate::attrs::Color::Idx(to_u8!(n) - 30);
                }
                &[38, 2, r, g, b] => {
                    self.attrs.fgcolor = crate::attrs::Color::Rgb(
                        to_u8!(r),
                        to_u8!(g),
                        to_u8!(b),
                    );
                }
                &[38, 5, i] => {
                    self.attrs.fgcolor = crate::attrs::Color::Idx(to_u8!(i));
                }
                &[38] => match next_param!() {
                    &[2] => {
                        let r = next_param_u8!();
                        let g = next_param_u8!();
                        let b = next_param_u8!();
                        self.attrs.fgcolor =
                            crate::attrs::Color::Rgb(r, g, b);
                    }
                    &[5] => {
                        self.attrs.fgcolor =
                            crate::attrs::Color::Idx(next_param_u8!());
                    }
                    ns => {
                        if log::log_enabled!(log::Level::Debug) {
                            let n = if ns.len() == 1 {
                                format!(
                                    "{}",
                                    // we just checked that ns.len() == 1, so
                                    // 0 must be valid
                                    ns[0]
                                )
                            } else {
                                format!("{ns:?}")
                            };
                            log::debug!("unhandled SGR mode: 38 {n}");
                        }
                        return;
                    }
                },
                &[39] => {
                    self.attrs.fgcolor = crate::attrs::Color::Default;
                }
                &[n] if (40..=47).contains(&n) => {
                    self.attrs.bgcolor =
                        crate::attrs::Color::Idx(to_u8!(n) - 40);
                }
                &[48, 2, r, g, b] => {
                    self.attrs.bgcolor = crate::attrs::Color::Rgb(
                        to_u8!(r),
                        to_u8!(g),
                        to_u8!(b),
                    );
                }
                &[48, 5, i] => {
                    self.attrs.bgcolor = crate::attrs::Color::Idx(to_u8!(i));
                }
                &[48] => match next_param!() {
                    &[2] => {
                        let r = next_param_u8!();
                        let g = next_param_u8!();
                        let b = next_param_u8!();
                        self.attrs.bgcolor =
                            crate::attrs::Color::Rgb(r, g, b);
                    }
                    &[5] => {
                        self.attrs.bgcolor =
                            crate::attrs::Color::Idx(next_param_u8!());
                    }
                    ns => {
                        if log::log_enabled!(log::Level::Debug) {
                            let n = if ns.len() == 1 {
                                format!(
                                    "{}",
                                    // we just checked that ns.len() == 1, so
                                    // 0 must be valid
                                    ns[0]
                                )
                            } else {
                                format!("{ns:?}")
                            };
                            log::debug!("unhandled SGR mode: 48 {n}");
                        }
                        return;
                    }
                },
                &[49] => {
                    self.attrs.bgcolor = crate::attrs::Color::Default;
                }
                &[n] if (90..=97).contains(&n) => {
                    self.attrs.fgcolor =
                        crate::attrs::Color::Idx(to_u8!(n) - 82);
                }
                &[n] if (100..=107).contains(&n) => {
                    self.attrs.bgcolor =
                        crate::attrs::Color::Idx(to_u8!(n) - 92);
                }
                ns => {
                    if log::log_enabled!(log::Level::Debug) {
                        let n = if ns.len() == 1 {
                            format!(
                                "{}",
                                // we just checked that ns.len() == 1, so 0
                                // must be valid
                                ns[0]
                            )
                        } else {
                            format!("{ns:?}")
                        };
                        log::debug!("unhandled SGR mode: {n}");
                    }
                }
            }
        }
    }

    // CSI r
    fn decstbm(&mut self, (top, bottom): (u16, u16)) {
        self.grid_mut().set_scroll_region(top - 1, bottom - 1);
    }

    // osc codes

    fn osc0(&mut self, s: &[u8]) {
        self.osc1(s);
        self.osc2(s);
    }

    fn osc1(&mut self, s: &[u8]) {
        if let Ok(s) = std::str::from_utf8(s) {
            self.icon_name = s.to_string();
        }
    }

    fn osc2(&mut self, s: &[u8]) {
        if let Ok(s) = std::str::from_utf8(s) {
            self.title = s.to_string();
        }
    }
}

impl vte::Perform for Screen {
    fn print(&mut self, c: char) {
        if c == '\u{fffd}' || ('\u{80}'..'\u{a0}').contains(&c) {
            self.errors = self.errors.saturating_add(1);
        }
        self.text(c);
    }

    fn execute(&mut self, b: u8) {
        match b {
            7 => self.bel(),
            8 => self.bs(),
            9 => self.tab(),
            10 => self.lf(),
            11 => self.vt(),
            12 => self.ff(),
            13 => self.cr(),
            // we don't implement shift in/out alternate character sets, but
            // it shouldn't count as an "error"
            14 | 15 => {}
            _ => {
                self.errors = self.errors.saturating_add(1);
                log::debug!("unhandled control character: {b}");
            }
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, b: u8) {
        intermediates.first().map_or_else(
            || match b {
                b'7' => self.decsc(),
                b'8' => self.decrc(),
                b'=' => self.deckpam(),
                b'>' => self.deckpnm(),
                b'M' => self.ri(),
                b'c' => self.ris(),
                b'g' => self.vb(),
                _ => {
                    log::debug!("unhandled escape code: ESC {b}");
                }
            },
            |i| {
                log::debug!("unhandled escape code: ESC {i} {b}");
            },
        );
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        _ignore: bool,
        c: char,
    ) {
        match intermediates.first() {
            None => match c {
                '@' => self.ich(canonicalize_params_1(params, 1)),
                'A' => self.cuu(canonicalize_params_1(params, 1)),
                'B' => self.cud(canonicalize_params_1(params, 1)),
                'C' => self.cuf(canonicalize_params_1(params, 1)),
                'D' => self.cub(canonicalize_params_1(params, 1)),
                'G' => self.cha(canonicalize_params_1(params, 1)),
                'H' => self.cup(canonicalize_params_2(params, 1, 1)),
                'J' => self.ed(canonicalize_params_1(params, 0)),
                'K' => self.el(canonicalize_params_1(params, 0)),
                'L' => self.il(canonicalize_params_1(params, 1)),
                'M' => self.dl(canonicalize_params_1(params, 1)),
                'P' => self.dch(canonicalize_params_1(params, 1)),
                'S' => self.su(canonicalize_params_1(params, 1)),
                'T' => self.sd(canonicalize_params_1(params, 1)),
                'X' => self.ech(canonicalize_params_1(params, 1)),
                // roost (SPEC-parity P19): REP
                'b' => self.rep(canonicalize_params_1(params, 1)),
                'd' => self.vpa(canonicalize_params_1(params, 1)),
                'h' => self.sm(params),
                'l' => self.rm(params),
                'm' => self.sgr(params),
                'r' => self.decstbm(canonicalize_params_decstbm(
                    params,
                    self.grid().size(),
                )),
                _ => {
                    if log::log_enabled!(log::Level::Debug) {
                        log::debug!(
                            "unhandled csi sequence: CSI {} {}",
                            param_str(params),
                            c
                        );
                    }
                }
            },
            Some(b'?') => match c {
                'J' => self.decsed(canonicalize_params_1(params, 0)),
                'K' => self.decsel(canonicalize_params_1(params, 0)),
                'h' => self.decset(params),
                'l' => self.decrst(params),
                _ => {
                    if log::log_enabled!(log::Level::Debug) {
                        log::debug!(
                            "unhandled csi sequence: CSI ? {} {}",
                            param_str(params),
                            c
                        );
                    }
                }
            },
            // roost: the SP intermediate — DECSCUSR, `CSI Ps SP q`
            // (SPEC-parity P7). The shape an application wants for the
            // cursor; an embedder that owns a real terminal can mirror it.
            // This died in the catch-all below, which is why insert-bar
            // cursors rendered as blocks.
            Some(b' ') => match c {
                'q' => self.decscusr(canonicalize_params_1(params, 0)),
                _ => {
                    if log::log_enabled!(log::Level::Debug) {
                        log::debug!(
                            "unhandled csi sequence: CSI {} SP {}",
                            param_str(params),
                            c
                        );
                    }
                }
            },
            Some(i) => {
                if log::log_enabled!(log::Level::Debug) {
                    log::debug!(
                        "unhandled csi sequence: CSI {} {} {}",
                        i,
                        param_str(params),
                        c
                    );
                }
            }
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bel_terminated: bool) {
        match (params.get(0), params.get(1)) {
            (Some(&b"0"), Some(s)) => self.osc0(s),
            (Some(&b"1"), Some(s)) => self.osc1(s),
            (Some(&b"2"), Some(s)) => self.osc2(s),
            // roost: OSC 9;4 is ConEmu/Windows Terminal *progress*
            // (`9;4;state;percent`), not a notification — Claude Code emits
            // it throughout a long turn. Recognized here so it can never be
            // mistaken for an OSC 9 notification body, then deliberately
            // dropped: surfacing progress as a badge percentage is its own
            // item (SPEC-parity P2, deferred half).
            (Some(&b"9"), Some(&b"4")) => {}
            // roost: OSC 9 desktop notification, `9 ; body` (SPEC-parity
            // P2). A body containing `;` arrives pre-split, so rejoin.
            (Some(&b"9"), Some(_)) => {
                let body = osc_join(params.get(1..).unwrap_or(&[]));
                if !body.is_empty() {
                    self.effects.push(Effect::Notify { title: None, body });
                }
            }
            // roost: OSC 777 notification, `777 ; notify ; title ; body`
            // (the rxvt/urxvt form Claude Code and others also emit).
            (Some(&b"777"), Some(&b"notify")) => {
                let title = osc_join(params.get(2..3).unwrap_or(&[]));
                let body = osc_join(params.get(3..).unwrap_or(&[]));
                // Title-only is legal and common enough; a notification
                // with an empty body would say nothing, so promote it.
                let (title, body) = if body.is_empty() {
                    (None, title)
                } else {
                    (Some(title), body)
                };
                if !body.is_empty() {
                    self.effects.push(Effect::Notify { title, body });
                }
            }
            // roost: OSC 52 clipboard, `52 ; <selection> ; <payload>`
            // (SPEC-parity P3). Writes are surfaced; a *read* request
            // (payload `?`, or a truncated sequence with no payload field
            // at all) never is — answering one would hand the application
            // the host clipboard's contents, which is a paste-theft vector,
            // so the effect it would need simply does not exist.
            (Some(&b"52"), Some(sel)) => {
                let payload_base64 = osc_join(params.get(2..).unwrap_or(&[]));
                if params.len() < 3 || payload_base64 == "?" {
                    log::debug!("dropped OSC 52 clipboard read request");
                } else {
                    self.effects.push(Effect::Osc52Write {
                        selection: std::string::String::from_utf8_lossy(sel)
                            .into_owned(),
                        payload_base64,
                    });
                }
            }
            _ => {
                if log::log_enabled!(log::Level::Debug) {
                    log::debug!(
                        "unhandled osc sequence: OSC {}",
                        osc_param_str(params),
                    );
                }
            }
        }
    }

    fn hook(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        if log::log_enabled!(log::Level::Debug) {
            intermediates.first().map_or_else(
                || {
                    log::debug!(
                        "unhandled dcs sequence: DCS {} {}",
                        param_str(params),
                        action,
                    );
                },
                |i| {
                    log::debug!(
                        "unhandled dcs sequence: DCS {} {} {}",
                        i,
                        param_str(params),
                        action,
                    );
                },
            );
        }
    }
}

fn canonicalize_params_1(params: &vte::Params, default: u16) -> u16 {
    let first = params.iter().next().map_or(0, |x| *x.first().unwrap_or(&0));
    if first == 0 {
        default
    } else {
        first
    }
}

fn canonicalize_params_2(
    params: &vte::Params,
    default1: u16,
    default2: u16,
) -> (u16, u16) {
    let mut iter = params.iter();
    let first = iter.next().map_or(0, |x| *x.first().unwrap_or(&0));
    let first = if first == 0 { default1 } else { first };

    let second = iter.next().map_or(0, |x| *x.first().unwrap_or(&0));
    let second = if second == 0 { default2 } else { second };

    (first, second)
}

fn canonicalize_params_decstbm(
    params: &vte::Params,
    size: crate::grid::Size,
) -> (u16, u16) {
    let mut iter = params.iter();
    let top = iter.next().map_or(0, |x| *x.first().unwrap_or(&0));
    let top = if top == 0 { 1 } else { top };

    let bottom = iter.next().map_or(0, |x| *x.first().unwrap_or(&0));
    let bottom = if bottom == 0 { size.rows } else { bottom };

    (top, bottom)
}

fn u16_to_u8(i: u16) -> Option<u8> {
    if i > u16::from(u8::max_value()) {
        None
    } else {
        // safe because we just ensured that the value fits in a u8
        Some(i.try_into().unwrap())
    }
}

fn param_str(params: &vte::Params) -> String {
    let strs: Vec<_> = params
        .iter()
        .map(|subparams| {
            let subparam_strs: Vec<_> = subparams
                .iter()
                .map(std::string::ToString::to_string)
                .collect();
            subparam_strs.join(" : ")
        })
        .collect();
    strs.join(" ; ")
}

/// Rejoin OSC parameters that a `;`-bearing payload was split across.
/// vte splits every OSC parameter on `;`, but a notification body or a
/// base64 clipboard payload is one field that may legitimately contain the
/// separator — putting it back is what a lenient terminal does.
// roost: added for SPEC-parity W3 (P2's OSC 9/777, P3's OSC 52).
fn osc_join(params: &[&[u8]]) -> String {
    let strs: Vec<_> = params
        .iter()
        .map(|b| std::string::String::from_utf8_lossy(b))
        .collect();
    strs.join(";")
}

fn osc_param_str(params: &[&[u8]]) -> String {
    let strs: Vec<_> = params
        .iter()
        .map(|b| format!("\"{}\"", std::string::String::from_utf8_lossy(b)))
        .collect();
    strs.join(" ; ")
}

// roost: vendor-side tests for the arms this fork adds (SPEC-parity W3).
// Upstream keeps its tests in an out-of-crate `tests/` tree that the
// published crate this vendoring started from strips, so the fork's own
// coverage lives in-module: `cargo test -p vt100` exercises it with nothing
// but the crate's real dependencies.
#[cfg(test)]
mod roost_tests {
    fn parser() -> crate::Parser {
        crate::Parser::new(6, 20, 50)
    }

    // -- focus reporting, mode 1004 (SPEC-parity P10) ---------------------

    #[test]
    fn mode_1004_tracks_set_and_reset() {
        let mut p = parser();
        assert!(!p.screen().focus_events(), "nobody is subscribed by default");
        p.process(b"\x1b[?1004h");
        assert!(p.screen().focus_events());
        p.process(b"\x1b[?1004l");
        assert!(!p.screen().focus_events());
    }

    #[test]
    fn mode_1004_is_independent_of_the_other_modes_and_leaves_the_grid_alone() {
        let mut p = parser();
        p.process(b"hello\x1b[?1004h");
        // A subscription changes nothing about what is on screen...
        assert!(p.screen().contents().contains("hello"));
        // ...and shares no bit with its neighbours in the mode word.
        assert!(p.screen().focus_events());
        assert!(!p.screen().bracketed_paste());
        assert!(!p.screen().synchronized_output());
        assert!(!p.screen().application_cursor());
        p.process(b"\x1b[?2004h\x1b[?1004l");
        assert!(p.screen().bracketed_paste());
        assert!(!p.screen().focus_events(), "?1004l must not disturb ?2004");
    }

    // -- synchronized output, mode 2026 (SPEC-parity P1) ------------------

    #[test]
    fn mode_2026_tracks_set_and_reset() {
        let mut p = parser();
        assert!(!p.screen().synchronized_output());
        p.process(b"\x1b[?2026h");
        assert!(p.screen().synchronized_output());
        p.process(b"\x1b[?2026l");
        assert!(!p.screen().synchronized_output());
    }

    #[test]
    fn sync_snapshot_captures_the_exact_pre_bracket_screen() {
        let mut p = parser();
        p.process(b"complete frame");
        // One chunk: the bracket opens, the screen is cleared, the redraw
        // begins. The capture must hold the state at the `?2026h` — not the
        // chunk's start, not its end.
        p.process(b"\x1b[?2026h\x1b[2J\x1b[Htorn");
        assert!(p.screen().synchronized_output());
        assert!(p.screen().contents().contains("torn"));
        let snap = p.take_sync_snapshot().expect("an opened bracket captures");
        assert!(snap.contents().contains("complete frame"));
        assert!(!snap.contents().contains("torn"));
        // Taken means taken: the frame is the embedder's now.
        assert!(p.take_sync_snapshot().is_none());
    }

    #[test]
    fn redundant_sync_open_does_not_recapture_mid_redraw() {
        let mut p = parser();
        p.process(b"good");
        p.process(b"\x1b[?2026h\x1b[2J\x1b[Hbad\x1b[?2026h");
        let snap = p.take_sync_snapshot().expect("the first open captured");
        assert!(snap.contents().contains("good"));
        assert!(!snap.contents().contains("bad"));
    }

    #[test]
    fn sync_close_drops_an_untaken_snapshot() {
        let mut p = parser();
        p.process(b"old");
        // Bracket opens and closes inside one chunk: the redraw completed,
        // so the live screen is current and nothing needs presenting in its
        // stead.
        p.process(b"\x1b[?2026h\x1b[2J\x1b[Hnew\x1b[?2026l");
        assert!(!p.screen().synchronized_output());
        assert!(p.take_sync_snapshot().is_none());
        assert!(p.screen().contents().contains("new"));
    }

    #[test]
    fn sync_reopen_captures_the_frame_the_previous_bracket_completed() {
        let mut p = parser();
        p.process(b"\x1b[?2026h\x1b[2J\x1b[Hframe one\x1b[?2026l");
        p.process(b"\x1b[?2026h\x1b[2J\x1b[Hframe two");
        let snap = p.take_sync_snapshot().expect("the reopen captured");
        assert!(snap.contents().contains("frame one"));
        assert!(!snap.contents().contains("frame two"));
        assert!(p.screen().synchronized_output());
    }

    #[test]
    fn a_snapshot_presents_the_frame_without_carrying_history() {
        // `Screen::snapshot` deliberately drops the banked scrollback (a
        // full clone would copy megabytes per redraw). The visible frame,
        // cursor state and title all survive; the history does not.
        let mut p = parser();
        for i in 0..30 {
            p.process(format!("history{i}\r\n").as_bytes());
        }
        p.process(b"\x1b]2;the title\x07\x1b[?25l");
        assert!(p.screen().scrollback_rows() > 0);
        p.process(b"\x1b[?2026h\x1b[2J\x1b[Htorn");
        let snap = p.take_sync_snapshot().expect("captured");
        assert_eq!(snap.size(), p.screen().size());
        assert_eq!(snap.title(), "the title");
        assert!(snap.hide_cursor());
        assert!(snap.contents().contains("history29"));
        assert_eq!(snap.scrollback_rows(), 0, "history must not ride along");
        assert_eq!(snap.scrollback(), 0);
    }

    #[test]
    fn full_reset_ends_an_open_bracket() {
        let mut p = parser();
        p.process(b"\x1b[?2026h");
        assert!(p.screen().synchronized_output());
        p.process(b"\x1bc"); // RIS
        assert!(!p.screen().synchronized_output());
        assert!(p.take_sync_snapshot().is_none());
    }

    // -- the effects surface ----------------------------------------------

    #[test]
    fn effects_are_empty_for_plain_output_and_untracked_sequences() {
        let mut p = parser();
        p.process(b"hello\x1b[31mred\x1b[0m\x1b]2;title\x07\x07\x1b[?2026h");
        assert!(p.take_effects().is_empty());
    }

    // -- OSC 9 / 777 notifications (SPEC-parity P2) -----------------------

    fn notify(title: Option<&str>, body: &str) -> crate::Effect {
        crate::Effect::Notify {
            title: title.map(std::string::ToString::to_string),
            body: body.to_string(),
        }
    }

    #[test]
    fn osc9_and_osc777_become_notify_effects() {
        // (bytes, expected effect)
        let cases: &[(&[u8], crate::Effect)] = &[
            // OSC 9, BEL-terminated (the common form).
            (b"\x1b]9;NEEDS-YOU\x07", notify(None, "NEEDS-YOU")),
            // OSC 9, ST-terminated.
            (b"\x1b]9;done\x1b\\", notify(None, "done")),
            // A body carrying the parameter separator survives intact.
            (b"\x1b]9;build failed; see log\x07", notify(None, "build failed; see log")),
            // OSC 777 carries an explicit title.
            (
                b"\x1b]777;notify;claude;turn finished\x07",
                notify(Some("claude"), "turn finished"),
            ),
            // ...and a title-only 777 is promoted to the body, so the
            // notification never says nothing.
            (b"\x1b]777;notify;just a title\x07", notify(None, "just a title")),
        ];
        for (bytes, want) in cases {
            let mut p = parser();
            p.process(bytes);
            assert_eq!(
                p.take_effects(),
                vec![want.clone()],
                "input {:?}",
                std::string::String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn osc9_progress_and_empty_bodies_produce_nothing() {
        let cases: &[&[u8]] = &[
            b"\x1b]9;4;1;40\x07",     // OSC 9;4 progress (deferred, not a notification)
            b"\x1b]9;4;0;0\x07",      // progress cleared
            b"\x1b]9;\x07",           // empty body
            b"\x1b]777;notify\x07",   // no title, no body
            b"\x1b]777;notify;;\x07", // both empty
            b"\x1b]9\x07",            // bare OSC 9, no parameters at all
        ];
        for bytes in cases {
            let mut p = parser();
            p.process(bytes);
            assert!(
                p.take_effects().is_empty(),
                "input {:?} must not notify",
                std::string::String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn notifications_accumulate_in_stream_order_and_drain_once() {
        let mut p = parser();
        p.process(b"\x1b]9;first\x07between\x1b]777;notify;t;second\x07");
        assert_eq!(
            p.take_effects(),
            vec![notify(None, "first"), notify(Some("t"), "second")]
        );
        assert!(p.take_effects().is_empty(), "draining takes them");
        // The screen itself is untouched by the notification traffic.
        assert!(p.screen().contents().contains("between"));
    }

    // -- OSC 52 clipboard (SPEC-parity P3) --------------------------------

    fn osc52(selection: &str, payload: &str) -> crate::Effect {
        crate::Effect::Osc52Write {
            selection: selection.to_string(),
            payload_base64: payload.to_string(),
        }
    }

    #[test]
    fn osc52_writes_become_effects_with_selection_and_payload_intact() {
        let cases: &[(&[u8], crate::Effect)] = &[
            // The canonical form: base64 "hi" into the clipboard selection.
            (b"\x1b]52;c;aGk=\x07", osc52("c", "aGk=")),
            // ST-terminated, and a multi-selection target (clipboard+primary).
            (b"\x1b]52;cp;aGk=\x1b\\", osc52("cp", "aGk=")),
            // Empty selection means "the default" — passed through as-is.
            (b"\x1b]52;;aGk=\x07", osc52("", "aGk=")),
            // An empty payload is xterm's "clear the clipboard", a write.
            (b"\x1b]52;c;\x07", osc52("c", "")),
        ];
        for (bytes, want) in cases {
            let mut p = parser();
            p.process(bytes);
            assert_eq!(
                p.take_effects(),
                vec![want.clone()],
                "input {:?}",
                std::string::String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn osc52_read_requests_are_dropped_not_surfaced() {
        // Answering a read hands the application the host clipboard —
        // paste theft. There is deliberately no effect that could carry it.
        let cases: &[&[u8]] = &[
            b"\x1b]52;c;?\x07",  // the standard read
            b"\x1b]52;p;?\x1b\\", // primary selection read
            b"\x1b]52;c\x07",    // truncated: no payload field at all
        ];
        for bytes in cases {
            let mut p = parser();
            p.process(bytes);
            assert!(
                p.take_effects().is_empty(),
                "input {:?} must never surface",
                std::string::String::from_utf8_lossy(bytes)
            );
        }
    }

    // -- DECSCUSR cursor shape (SPEC-parity P7) ---------------------------

    #[test]
    fn decscusr_reports_the_requested_cursor_shape() {
        // (bytes, expected shape parameter)
        let cases: &[(&[u8], u8)] = &[
            (b"\x1b[0 q", 0), // explicit "back to the terminal default"
            (b"\x1b[ q", 0),  // omitted parameter means the same
            (b"\x1b[1 q", 1), // blinking block
            (b"\x1b[2 q", 2), // steady block
            (b"\x1b[3 q", 3), // blinking underline
            (b"\x1b[4 q", 4), // steady underline
            (b"\x1b[5 q", 5), // blinking bar — what an editor's insert mode asks for
            (b"\x1b[6 q", 6), // steady bar
            (b"\x1b[9 q", 9), // undefined: reported as asked, the embedder decides
        ];
        for (bytes, shape) in cases {
            let mut p = parser();
            p.process(bytes);
            assert_eq!(
                p.take_effects(),
                vec![crate::Effect::CursorShape(*shape)],
                "input {:?}",
                std::string::String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn shape_changes_are_reported_in_order_and_leave_the_grid_alone() {
        let mut p = parser();
        p.process(b"text\x1b[5 qmore\x1b[2 q");
        assert_eq!(
            p.take_effects(),
            vec![crate::Effect::CursorShape(5), crate::Effect::CursorShape(2)]
        );
        assert!(p.screen().contents().contains("textmore"));
    }

    #[test]
    fn other_sp_intermediate_sequences_are_not_shape_changes() {
        // `CSI Ps SP @` (SL, scroll left) and friends share the intermediate
        // but mean something else entirely; DECRQSS for DECSCUSR is a DCS,
        // not a CSI, and must not be mistaken for a set either.
        let mut p = parser();
        p.process(b"\x1b[2 @\x1b[1 A\x1bP$q q\x1b\\");
        assert!(p.take_effects().is_empty());
    }

    #[test]
    fn an_osc_terminating_bel_still_does_not_count_as_a_bell() {
        // roost's NeedsInput heuristic keys off vt100's *parsed* bell count,
        // so a BEL consumed as an OSC string terminator must not inflate it
        // — the notification effect is the signal, not the terminator.
        let mut p = parser();
        let before = p.screen().audible_bell_count();
        p.process(b"\x1b]9;hi\x07");
        assert_eq!(p.screen().audible_bell_count(), before);
        assert_eq!(p.take_effects().len(), 1);
    }

    // -- REP, `CSI Ps b` (SPEC-parity P19) --------------------------------

    fn row0(p: &crate::Parser) -> String {
        p.screen().contents().lines().next().unwrap_or_default().to_string()
    }

    #[test]
    fn rep_repeats_the_last_graphic_character() {
        // The measured case: before the `b` arm existed this rendered "ab".
        let mut p = parser();
        p.process(b"ab\x1b[5b");
        assert_eq!(row0(&p), "abbbbbb");
    }

    #[test]
    fn rep_defaults_to_one_and_treats_zero_as_one() {
        // ECMA-48: Pn defaults to 1, and an explicit 0 means the default.
        let mut p = parser();
        p.process(b"x\x1b[b");
        assert_eq!(row0(&p), "xx");
        let mut p = parser();
        p.process(b"x\x1b[0b");
        assert_eq!(row0(&p), "xx");
    }

    #[test]
    fn rep_with_no_preceding_graphic_character_is_a_no_op() {
        let mut p = parser();
        p.process(b"\x1b[5b");
        assert_eq!(row0(&p), "");
        assert_eq!(p.screen().cursor_position(), (0, 0));
        // Still a no-op after a control sequence has run but nothing printed.
        let mut p = parser();
        p.process(b"\x1b[2J\x1b[3;3H\x1b[4b");
        assert_eq!(p.screen().contents(), "");
        assert_eq!(p.screen().cursor_position(), (2, 2));
    }

    #[test]
    fn rep_uses_the_attributes_in_force_at_the_repeat() {
        // The repeat is new output, so it takes the *current* SGR state --
        // not whatever was in force when the original character was printed.
        let mut p = parser();
        p.process(b"P\x1b[31;2m\x1b[3b");
        let s = p.screen();
        assert_eq!(s.cell(0, 0).unwrap().fgcolor(), crate::attrs::Color::Default);
        assert!(!s.cell(0, 0).unwrap().dim());
        for col in 1..=3 {
            let cell = s.cell(0, col).unwrap();
            assert_eq!(cell.contents(), "P");
            assert_eq!(cell.fgcolor(), crate::attrs::Color::Idx(1));
            assert!(cell.dim(), "the repeat carries the live attrs");
        }
    }

    #[test]
    fn rep_wraps_at_the_row_edge_like_ordinary_output() {
        // 20 columns: `z` plus 24 repeats fills row 0 and continues on row 1.
        let mut p = parser();
        p.process(b"z\x1b[24b");
        let s = p.screen();
        for col in 0..20 {
            assert_eq!(s.cell(0, col).unwrap().contents(), "z", "row 0 col {col}");
        }
        for col in 0..5 {
            assert_eq!(s.cell(1, col).unwrap().contents(), "z", "row 1 col {col}");
        }
        assert!(s.cell(1, 5).unwrap().contents().is_empty());
        assert_eq!(s.cursor_position(), (1, 5));
        // A real grid wrap, so `contents()` reflows it back into one logical
        // line exactly as it would for a typed run of the same length.
        assert_eq!(s.contents(), "z".repeat(25));
    }

    #[test]
    fn rep_repeats_the_last_character_printed_not_the_last_one_on_screen() {
        // Moving the cursor does not change what REP repeats, and a repeat
        // lands wherever the cursor now is.
        let mut p = parser();
        p.process(b"abc\x1b[H\x1b[2b");
        assert_eq!(row0(&p), "ccc");
    }

    #[test]
    fn rep_repeats_a_wide_character_as_a_wide_character() {
        let mut p = parser();
        p.process("\u{65e5}\x1b[2b".as_bytes());
        let s = p.screen();
        assert_eq!(row0(&p), "\u{65e5}".repeat(3));
        assert!(s.cell(0, 4).unwrap().is_wide());
        assert!(s.cell(0, 5).unwrap().is_wide_continuation());
        assert_eq!(s.cursor_position(), (0, 6));
    }

    // -- one width table for the process (SPEC-parity P17) ----------------

    /// How many columns the grid actually gave a string: the cursor's advance
    /// from a known-empty row.
    fn grid_cols(bytes: &[u8]) -> u16 {
        let mut p = parser();
        p.process(bytes);
        p.screen().cursor_position().1
    }

    /// The embedder's measurement — literally the crate roost and ratatui
    /// call. If this and `grid_cols` ever disagree, the blit and the mouse
    /// hitboxes disagree with the grid, which is P17.
    fn embedder_cols(s: &str) -> u16 {
        u16::try_from(unicode_width::UnicodeWidthStr::width(s)).unwrap()
    }

    #[test]
    fn grid_and_embedder_measure_the_same_columns() {
        // VS16 emoji-presentation sequences are the pair that changed between
        // unicode-width 0.1.14 (grid, pre-P17) and 0.2 (renderer): measured
        // before the fix, "❤\u{fe0f}" was 2 cols to the renderer and 1 to the
        // grid, so the next glyph landed at col 1 and every hitbox after it
        // was off by one.
        for s in [
            "\u{2764}\u{fe0f}", // VS16 heart
            "\u{26a0}\u{fe0f}", // VS16 warning sign
            "\u{2714}\u{fe0f}", // VS16 check mark
            "\u{65e5}",         // CJK
            "\u{65e5}\u{672c}\u{8a9e}",
            "\u{1f600}",  // natively-wide emoji
            "\u{2764}",   // bare heart: still one column
            "a\u{0301}",  // combining acute: still one column
            "ascii",      //
            "\u{2764}\u{fe0e}", // VS15 text presentation: one column
        ] {
            assert_eq!(
                grid_cols(s.as_bytes()),
                embedder_cols(s),
                "column disagreement for {s:?}"
            );
        }
    }

    #[test]
    fn a_vs16_sequence_occupies_a_real_wide_cell() {
        let mut p = parser();
        p.process("\u{2764}\u{fe0f}X".as_bytes());
        let s = p.screen();
        let base = s.cell(0, 0).unwrap();
        assert_eq!(base.contents(), "\u{2764}\u{fe0f}");
        assert!(base.is_wide(), "the sequence must own two cells");
        assert!(s.cell(0, 1).unwrap().is_wide_continuation());
        assert!(s.cell(0, 1).unwrap().contents().is_empty());
        // the following glyph starts after the continuation, not on top of it
        assert_eq!(s.cell(0, 2).unwrap().contents(), "X");
        // and the row reads back with no injected filler
        assert_eq!(s.contents().lines().next().unwrap(), "\u{2764}\u{fe0f}X");
    }

    /// roost: the blit's allocation-free `push_contents` must produce byte
    /// for byte what `contents` does, for every cell shape the grid can
    /// hold — empty, ascii, a combining sequence, a wide glyph and its
    /// continuation.
    #[test]
    fn push_contents_matches_contents_for_every_cell_shape() {
        let mut p = parser();
        p.process("a\u{2764}\u{fe0f}e\u{301}\u{4e16}".as_bytes());
        let s = p.screen();
        let (rows, cols) = s.size();
        let mut seen_multi = false;
        let mut buf = String::new();
        for row in 0..rows {
            for col in 0..cols {
                let cell = s.cell(row, col).unwrap();
                buf.clear();
                cell.push_contents(&mut buf);
                assert_eq!(buf, cell.contents(), "cell ({row}, {col})");
                seen_multi |= cell.contents().chars().count() > 1;
            }
        }
        assert!(seen_multi, "the fixture must exercise a multi-codepoint cell");
        // and the buffer is genuinely reusable: no residue between cells
        let mut buf = String::from("stale");
        buf.clear();
        s.cell(0, 0).unwrap().push_contents(&mut buf);
        assert_eq!(buf, "a");
    }

    #[test]
    fn widening_a_vs16_sequence_repairs_the_wide_char_it_displaces() {
        // The cell claimed as the continuation may already hold a wide char
        // from an earlier draw; leaving its own continuation behind would
        // orphan a cell the grid later dereferences.
        let mut p = parser();
        p.process("\u{65e5}\u{672c}\x1b[H\u{2764}\u{fe0f}".as_bytes());
        let s = p.screen();
        assert!(s.cell(0, 0).unwrap().is_wide());
        assert!(s.cell(0, 1).unwrap().is_wide_continuation());
        assert!(!s.cell(0, 2).unwrap().is_wide_continuation());
    }

    // -- dim and strikethrough, SGR 2 / 9 (SPEC-parity P16) ---------------

    #[test]
    fn sgr_2_sets_dim_and_sgr_22_clears_it() {
        let mut p = parser();
        p.process(b"\x1b[2mfaint\x1b[22mplain");
        let s = p.screen();
        assert!(s.cell(0, 0).unwrap().dim());
        assert!(!s.cell(0, 5).unwrap().dim());
        // SGR 0 is the other reset.
        let mut p = parser();
        p.process(b"\x1b[2mfaint\x1b[0mplain");
        assert!(!p.screen().cell(0, 5).unwrap().dim());
    }

    #[test]
    fn sgr_9_sets_strikethrough_and_sgr_29_clears_it() {
        let mut p = parser();
        p.process(b"\x1b[9mgone\x1b[29mhere");
        let s = p.screen();
        assert!(s.cell(0, 0).unwrap().strikethrough());
        assert!(!s.cell(0, 4).unwrap().strikethrough());
        let mut p = parser();
        p.process(b"\x1b[9mgone\x1b[0mhere");
        assert!(!p.screen().cell(0, 4).unwrap().strikethrough());
    }

    #[test]
    fn dim_and_strikethrough_are_independent_of_the_other_attributes() {
        // A realistic agent-CLI run: everything on at once, then the two new
        // attributes cleared while bold/italic/underline/inverse stay.
        let mut p = parser();
        p.process(b"\x1b[1;2;3;4;7;9mall\x1b[29mno-strike");
        let s = p.screen();
        let all = s.cell(0, 0).unwrap();
        assert!(all.bold() && all.italic() && all.underline() && all.inverse());
        assert!(all.dim() && all.strikethrough());
        let rest = s.cell(0, 3).unwrap();
        assert!(rest.bold() && rest.italic() && rest.underline() && rest.inverse());
        assert!(rest.dim(), "SGR 29 must not touch intensity");
        assert!(!rest.strikethrough());
    }

    #[test]
    fn sgr_22_ends_both_halves_of_intensity() {
        // ECMA-48: 22 is "normal intensity", not "bold off". A pane that
        // dims, bolds, then sends 22 expects plain text on both counts.
        let mut p = parser();
        p.process(b"\x1b[1;2mboth\x1b[22mneither");
        let s = p.screen();
        assert!(s.cell(0, 0).unwrap().bold() && s.cell(0, 0).unwrap().dim());
        assert!(!s.cell(0, 4).unwrap().bold() && !s.cell(0, 4).unwrap().dim());
    }

    #[test]
    fn the_intensity_pair_tracks_independently_through_sgr_22() {
        // SGR 22 ("normal intensity") clears both bold and dim at once, so
        // a dim re-asserted in the same escape (`22;2`) must survive, and a
        // later, unrelated attribute (strikethrough) must leave it alone.
        let mut p = parser();
        p.process(b"\x1b[1;2mA\x1b[22;2mB\x1b[9mC");
        let s = p.screen();
        for (col, (bold, dim, strike)) in
            [(true, true, false), (false, true, false), (false, true, true)]
                .into_iter()
                .enumerate()
        {
            let cell = s.cell(0, u16::try_from(col).unwrap()).unwrap();
            assert_eq!(cell.bold(), bold, "bold at col {col}");
            assert_eq!(cell.dim(), dim, "dim at col {col}");
            assert_eq!(cell.strikethrough(), strike, "strike at col {col}");
        }
    }

    #[test]
    fn a_vs16_sequence_with_no_room_stays_one_column() {
        // Last column: there is no cell to claim, so the sequence stays
        // narrow rather than leaving a wide flag with no continuation.
        let mut p = crate::Parser::new(2, 3, 0);
        p.process("ab\u{2764}\u{fe0f}".as_bytes());
        let s = p.screen();
        assert!(!s.cell(0, 2).unwrap().is_wide());
        assert_eq!(s.cell(0, 2).unwrap().contents(), "\u{2764}\u{fe0f}");
        // and printing over it afterwards must not panic
        p.process(b"\x1b[Hz");
        assert_eq!(p.screen().cell(0, 0).unwrap().contents(), "z");
    }

    // -- wrapped rows (SPEC-ux U19) ---------------------------------------

    #[test]
    fn row_wrapped_marks_only_rows_the_cursor_ran_off_the_end_of() {
        // roost joins wrapped rows before hunting for a URL, so the flag has
        // to mean exactly "this row continues on the next one" — a row ended
        // by a newline, or merely full to its last column, is not wrapped.
        let mut p = parser(); // 20 columns
        p.process(b"12345678901234567890continued");
        assert!(p.screen().row_wrapped(0), "the cursor ran past column 20");
        assert!(!p.screen().row_wrapped(1), "the continuation row ends on its own");

        let mut p = parser();
        p.process(b"short\r\nnext");
        assert!(!p.screen().row_wrapped(0), "a newline is not a wrap");

        let mut p = parser();
        p.process(b"12345678901234567890"); // exactly full, nothing after
        assert!(!p.screen().row_wrapped(0), "full is not wrapped until a char follows");

        // Off the end of the grid answers false rather than panicking.
        assert!(!p.screen().row_wrapped(999));
    }

    // -- reflow (SPEC-parity P5) ------------------------------------------

    /// The row texts currently on screen, trailing blanks trimmed, so a test
    /// can assert exactly where a rewrapped line broke.
    fn rows_of(s: &crate::Screen) -> Vec<String> {
        let (_, cols) = s.size();
        s.rows(0, cols)
            .map(|mut r| {
                while r.ends_with(' ') {
                    r.pop();
                }
                r
            })
            .collect()
    }

    #[test]
    fn narrowing_wraps_a_long_line_and_widening_rejoins_it() {
        // P5's defect in miniature: a line printed at one width, then asked
        // to live at another (zoom resizes a pane's PTY both ways). Before,
        // `set_size` truncated the row in place and the tail was gone — from
        // the screen, from `roost read`, from re-zooming.
        let mut p = crate::Parser::new(6, 40, 100);
        let line = "0123456789".repeat(3); // 30 columns
        p.process(line.as_bytes());
        assert_eq!(rows_of(p.screen())[0], line);

        p.set_size(6, 20);
        let rows = rows_of(p.screen());
        assert_eq!(rows[0], "01234567890123456789", "the first 20 columns");
        assert_eq!(rows[1], "0123456789", "the tail wrapped, it did not vanish");
        assert!(p.screen().row_wrapped(0), "the break is a wrap, not a newline");
        assert!(p.screen().contents().contains(&line), "no characters lost");

        // ...and back. Widening rejoins what narrowing split.
        p.set_size(6, 40);
        let rows = rows_of(p.screen());
        assert_eq!(rows[0], line, "the halves rejoined onto one row");
        assert_eq!(rows[1], "", "and nothing was left behind on the second");
        assert!(!p.screen().row_wrapped(0));
    }

    #[test]
    fn a_rewrapped_line_keeps_its_attributes() {
        // Reflow copies cells, not characters: a colored/bold run that
        // rewraps onto the next row is still colored and bold there. A
        // characters-only rewrap would repaint every log as plain text on
        // each resize.
        let mut p = crate::Parser::new(6, 40, 100);
        p.process(b"\x1b[1;31m");
        p.process("x".repeat(30).as_bytes());
        p.process(b"\x1b[0m");

        p.set_size(6, 20);
        let tail = p.screen().cell(1, 0).expect("the wrapped tail is on row 1");
        assert_eq!(tail.contents(), "x");
        assert!(tail.bold(), "bold must survive the rewrap");
        assert_eq!(tail.fgcolor(), crate::Color::Idx(1), "and so must the color");
        // The rejoin carries them back the other way too.
        p.set_size(6, 40);
        let joined = p.screen().cell(0, 25).expect("all 30 columns are back on row 0");
        assert!(joined.bold() && joined.fgcolor() == crate::Color::Idx(1));
    }

    #[test]
    fn a_wide_glyph_at_the_wrap_boundary_moves_whole() {
        // Half a glyph on each row isn't representable in the grid (and no
        // real terminal does it): the row ends one column short instead.
        let mut p = crate::Parser::new(4, 10, 100);
        p.process("abc\u{65e5}\u{672c}\u{8a9e}".as_bytes()); // 3 + 3×2 columns

        p.set_size(4, 4);
        let s = p.screen();
        assert_eq!(s.cell(0, 0).unwrap().contents(), "a");
        assert_eq!(s.cell(0, 2).unwrap().contents(), "c");
        let boundary = s.cell(0, 3).expect("the last column of the wrapped row");
        assert!(
            !boundary.has_contents() && !boundary.is_wide_continuation(),
            "the 2-column glyph must not be split across the boundary, got {:?}",
            boundary.contents()
        );
        assert_eq!(s.cell(1, 0).unwrap().contents(), "\u{65e5}", "it moved whole");
        assert!(s.cell(1, 0).unwrap().is_wide());
        assert!(s.cell(1, 1).unwrap().is_wide_continuation(), "with its own continuation");
        assert_eq!(s.cell(1, 2).unwrap().contents(), "\u{672c}");
        assert_eq!(s.cell(2, 0).unwrap().contents(), "\u{8a9e}");
        assert!(p.screen().contents().contains("abc\u{65e5}\u{672c}\u{8a9e}"));
    }

    #[test]
    fn the_cursor_lands_on_the_same_logical_character() {
        // The cursor is a position in the *line*, not in the grid: after a
        // rewrap it must still name the character it named before.
        let mut p = crate::Parser::new(6, 40, 100);
        p.process(b"abcdefghij\x1b[1;4H"); // park it on 'd' (row 1, col 4)
        assert_eq!(p.screen().cursor_position(), (0, 3));
        assert_eq!(p.screen().cell(0, 3).unwrap().contents(), "d");

        p.set_size(6, 4); // rows become "abcd", "efgh", "ij"
        let (row, col) = p.screen().cursor_position();
        assert_eq!(p.screen().cell(row, col).unwrap().contents(), "d");
        assert_eq!((row, col), (0, 3));

        p.set_size(6, 6); // and again, to a width that splits differently
        let (row, col) = p.screen().cursor_position();
        assert_eq!(p.screen().cell(row, col).unwrap().contents(), "d");

        // A cursor sitting past the end of the text keeps its distance from
        // it — a shell prompt is exactly this case.
        let mut p = crate::Parser::new(6, 40, 100);
        p.process(b"$ ");
        assert_eq!(p.screen().cursor_position(), (0, 2));
        p.set_size(6, 20);
        assert_eq!(p.screen().cursor_position(), (0, 2));
    }

    #[test]
    fn the_alternate_screen_is_never_reflowed() {
        // An alternate-screen app owns its canvas and repaints on SIGWINCH;
        // rewrapping underneath would fight the redraw it is already sending.
        let mut p = crate::Parser::new(6, 40, 100);
        p.process(b"\x1b[?1049h"); // enter the alternate screen
        p.process("0123456789".repeat(3).as_bytes());
        assert!(p.screen().alternate_screen());

        p.set_size(6, 20);
        let rows = rows_of(p.screen());
        assert_eq!(rows[0], "01234567890123456789", "truncated in place");
        assert_eq!(rows[1], "", "no rewrap: nothing moved to the next row");
        assert!(!p.screen().row_wrapped(0));
    }

    #[test]
    fn the_primary_grid_reflows_even_while_an_app_borrows_the_screen() {
        // The rule is per grid, not per mode: what the shell gets back on
        // `?1049l` is the content it would have had if the app never ran.
        let mut p = crate::Parser::new(6, 40, 100);
        let line = "0123456789".repeat(3);
        p.process(line.as_bytes());
        p.process(b"\x1b[?1049h");
        p.set_size(6, 20);
        p.process(b"\x1b[?1049l");
        assert!(!p.screen().alternate_screen());
        assert!(p.screen().contents().contains(&line), "the shell's line survived");
        assert!(p.screen().row_wrapped(0));
    }

    #[test]
    fn a_rewrap_sheds_blank_rows_before_it_banks_live_content() {
        // The empty tail of a screen is room a narrowing may use for free.
        // Banking instead would push live content into a history that is
        // never rewrapped — so the widening trip back could not rejoin it,
        // and P5's round-trip would still lose the line.
        let mut p = crate::Parser::new(6, 40, 100);
        let line = "0123456789".repeat(3);
        p.process(line.as_bytes());
        assert_eq!(p.screen().scrollback_rows(), 0);

        p.set_size(6, 20); // one more row needed, five blank ones available
        assert_eq!(p.screen().scrollback_rows(), 0, "nothing had to be banked");
        p.set_size(6, 40);
        assert_eq!(rows_of(p.screen())[0], line, "so the round-trip is lossless");
    }

    #[test]
    fn rows_a_narrowing_pushes_off_the_top_go_to_the_scrollback() {
        // With no blank room left, a rewrap that needs more rows scrolls the
        // grid exactly like ordinary output does — into the history, oldest
        // first, cursor following its own line down.
        let mut p = crate::Parser::new(4, 20, 100);
        for i in 0..4 {
            p.process(format!("line{i}-{}", "x".repeat(13)).as_bytes());
            if i < 3 {
                p.process(b"\r\n");
            }
        }
        assert_eq!(p.screen().scrollback_rows(), 0);
        let (cursor_row, _) = p.screen().cursor_position();
        assert_eq!(cursor_row, 3);

        p.set_size(4, 10); // every line now needs two rows: 8 rows for 4
        assert_eq!(p.screen().scrollback_rows(), 4, "the overflow banked");
        assert_eq!(p.screen().cursor_position().0, 3, "the cursor stayed in the grid");
        // `all_contents` is row-by-row (history has no wrap flags to honor),
        // so put the rows back together to read the logical lines out of it.
        let all = p.screen().all_contents();
        let joined: String = all.lines().collect::<Vec<_>>().concat();
        for i in 0..4 {
            let want = format!("line{i}-{}", "x".repeat(13));
            assert!(joined.contains(&want), "line{i} lost columns to the rewrap: {all:?}");
        }
    }

    #[test]
    fn a_scrolled_view_stays_clamped_and_pinned_across_a_rewrap() {
        // U3/U9: the `↑N` badge reads the grid's own clamp, so a resize may
        // never leave the offset naming history that isn't there.
        let mut p = crate::Parser::new(4, 20, 100);
        for i in 0..20 {
            p.process(format!("row{i}-{}\r\n", "y".repeat(12)).as_bytes());
        }
        let banked = p.screen().scrollback_rows();
        p.set_scrollback(5);
        assert_eq!(p.screen().scrollback(), 5);

        p.set_size(4, 10);
        let offset = p.screen().scrollback();
        assert!(
            offset <= p.screen().scrollback_rows(),
            "offset {offset} past the {} banked rows",
            p.screen().scrollback_rows()
        );
        assert!(p.screen().scrollback_rows() >= banked, "history only grows here");
        // Snapping back to the tail still works, from either side.
        p.set_scrollback(0);
        assert_eq!(p.screen().scrollback(), 0);
    }

    #[test]
    fn a_rewrap_survives_degenerate_sizes() {
        // The 1x1 floor `Grid::new`/`set_size` clamp to is reachable from a
        // pane squeezed to nothing; a wide glyph there has nowhere to go.
        let mut p = crate::Parser::new(6, 20, 50);
        p.process("ab\u{65e5}cd".as_bytes());
        p.set_size(1, 1);
        assert_eq!(p.screen().size(), (1, 1));
        p.process(b"z");
        p.set_size(6, 20);
        assert_eq!(p.screen().size(), (6, 20));
        // Wide-flag bookkeeping stayed consistent through both trips.
        p.process(b"\x1b[Hq");
        assert_eq!(p.screen().cell(0, 0).unwrap().contents(), "q");
    }
}
