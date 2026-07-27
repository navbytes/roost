use unicode_width::UnicodeWidthChar as _;
use unicode_width::UnicodeWidthStr;

const CODEPOINTS_IN_CELL: usize = 6;

/// Enough room for every codepoint a cell can hold, UTF-8 encoded.
const CELL_UTF8_CAP: usize = CODEPOINTS_IN_CELL * 4;

/// Represents a single terminal cell.
#[derive(Clone, Debug, Default, Eq)]
pub struct Cell {
    contents: [char; CODEPOINTS_IN_CELL],
    len: u8,
    attrs: crate::attrs::Attrs,
}

impl PartialEq<Self> for Cell {
    fn eq(&self, other: &Self) -> bool {
        if self.len != other.len {
            return false;
        }
        if self.attrs != other.attrs {
            return false;
        }
        let len = self.len();
        // self.len() always returns a valid value
        self.contents[..len] == other.contents[..len]
    }
}

impl Cell {
    #[inline]
    fn len(&self) -> usize {
        usize::from(self.len & 0x0f)
    }

    pub(crate) fn set(&mut self, c: char, a: crate::attrs::Attrs) {
        self.contents[0] = c;
        self.len = 1;
        // strings in this context should always be an arbitrary character
        // followed by zero or more zero-width characters, so we should only
        // have to look at the first character
        self.set_wide(c.width().unwrap_or(1) > 1);
        self.attrs = a;
    }

    /// roost (SPEC-parity P17): measure the cell the way the *embedder* does
    /// — `UnicodeWidthStr` over the whole cell, not `UnicodeWidthChar` over
    /// the base char. unicode-width 0.2 scores an emoji-presentation sequence
    /// (base char + VS16) as two columns, a fact only visible once the whole
    /// cell is measured as a string. Renderers (roost's blit, ratatui) measure
    /// exactly this string, so this is the one measurement that cannot
    /// disagree with them. Allocation-free: a cell holds at most
    /// `CODEPOINTS_IN_CELL` codepoints.
    fn contents_width(&self) -> usize {
        let mut buf = [0u8; CELL_UTF8_CAP];
        let mut len = 0;
        for c in self.contents.iter().take(self.len()) {
            // `contents` holds at most CODEPOINTS_IN_CELL chars and each
            // encodes to at most 4 bytes, so `buf` always has room.
            len += c.encode_utf8(&mut buf[len..]).len();
        }
        // every byte was written by `char::encode_utf8`, so this is UTF-8
        std::str::from_utf8(&buf[..len]).map_or(0, UnicodeWidthStr::width)
    }

    /// Appends a zero-width codepoint (combining mark, variation selector).
    ///
    /// roost (SPEC-parity P17): returns `true` when the append pushed a cell
    /// that is still flagged narrow past one column — an emoji-presentation
    /// sequence completing. The flag is deliberately **not** set here: only
    /// the caller owns the grid, and a cell may not be marked wide unless the
    /// cell to its right can be claimed as the continuation (a wide flag with
    /// no continuation cell is an invariant break the grid would later
    /// dereference). See `Screen::widen_cell`.
    #[must_use]
    pub(crate) fn append(&mut self, c: char) -> bool {
        let len = self.len();
        if len >= CODEPOINTS_IN_CELL {
            return false;
        }
        if len == 0 {
            // 0 is always less than 6
            self.contents[0] = ' ';
            self.len += 1;
        }

        let len = self.len();
        // we already checked that len < CODEPOINTS_IN_CELL
        self.contents[len] = c;
        self.len += 1;

        !self.is_wide() && self.contents_width() > 1
    }

    /// roost (SPEC-parity P17): mark a cell wide after the fact, once the
    /// caller has claimed its continuation cell. Only ever widens — a cell
    /// never narrows, so the grid's continuation bookkeeping stays consistent.
    pub(crate) fn promote_wide(&mut self) {
        self.set_wide(true);
    }

    pub(crate) fn clear(&mut self, attrs: crate::attrs::Attrs) {
        self.len = 0;
        self.attrs = attrs;
    }

    /// Returns the text contents of the cell.
    ///
    /// Can include multiple unicode characters if combining characters are
    /// used, but will contain at most one character with a non-zero character
    /// width.
    #[must_use]
    pub fn contents(&self) -> String {
        let mut s = String::with_capacity(CODEPOINTS_IN_CELL * 4);
        for c in self.contents.iter().take(self.len()) {
            s.push(*c);
        }
        s
    }

    /// Returns whether the cell contains any text data.
    #[must_use]
    pub fn has_contents(&self) -> bool {
        self.len > 0
    }

    /// Returns whether the text data in the cell represents a wide character.
    #[must_use]
    pub fn is_wide(&self) -> bool {
        self.len & 0x80 == 0x80
    }

    /// Returns whether the cell contains the second half of a wide character
    /// (in other words, whether the previous cell in the row contains a wide
    /// character)
    #[must_use]
    pub fn is_wide_continuation(&self) -> bool {
        self.len & 0x40 == 0x40
    }

    fn set_wide(&mut self, wide: bool) {
        if wide {
            self.len |= 0x80;
        } else {
            self.len &= 0x7f;
        }
    }

    pub(crate) fn set_wide_continuation(&mut self, wide: bool) {
        if wide {
            self.len |= 0x40;
        } else {
            self.len &= 0xbf;
        }
    }

    pub(crate) fn attrs(&self) -> &crate::attrs::Attrs {
        &self.attrs
    }

    /// Returns the foreground color of the cell.
    #[must_use]
    pub fn fgcolor(&self) -> crate::attrs::Color {
        self.attrs.fgcolor
    }

    /// Returns the background color of the cell.
    #[must_use]
    pub fn bgcolor(&self) -> crate::attrs::Color {
        self.attrs.bgcolor
    }

    /// Returns whether the cell should be rendered with the bold text
    /// attribute.
    #[must_use]
    pub fn bold(&self) -> bool {
        self.attrs.bold()
    }

    /// Returns whether the cell should be rendered with the italic text
    /// attribute.
    #[must_use]
    pub fn italic(&self) -> bool {
        self.attrs.italic()
    }

    /// Returns whether the cell should be rendered with the underlined text
    /// attribute.
    #[must_use]
    pub fn underline(&self) -> bool {
        self.attrs.underline()
    }

    /// Returns whether the cell should be rendered with the inverse text
    /// attribute.
    #[must_use]
    pub fn inverse(&self) -> bool {
        self.attrs.inverse()
    }

    /// Returns whether the cell should be rendered with the dim (faint, SGR
    /// 2) text attribute. roost (SPEC-parity P16).
    #[must_use]
    pub fn dim(&self) -> bool {
        self.attrs.dim()
    }

    /// Returns whether the cell should be rendered with the strikethrough
    /// (SGR 9) text attribute. roost (SPEC-parity P16).
    #[must_use]
    pub fn strikethrough(&self) -> bool {
        self.attrs.strikethrough()
    }
}
