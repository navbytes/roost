//! Text editing shared by the three text-input modes (Rename, PaneEdit,
//! Broadcast). The state stays in each `Mode` variant (renderers and hints
//! read it there); `Field` is a borrowed view over it that owns every edit
//! and motion, so a mode keeps only its own commit/cancel rules.
//!
//! Rows are unified: with a `head` (PaneEdit's name, Rename's buffer) row 0
//! is the head and rows 1.. are `lines`; without one, row i is `lines[i]`.
//! Motion crosses the head/lines seam, **edits never do** — Backspace at the
//! first line's left edge and Delete at the head's end are walls.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub struct Field<'a> {
    head: Option<&'a mut String>,
    lines: &'a mut Vec<String>,
    row: &'a mut usize,
    /// A **char** index into the current row.
    col: &'a mut usize,
    /// Line-break / paste-overflow cap; a break at the cap is refused whole.
    max_lines: usize,
}

impl<'a> Field<'a> {
    /// Rename is the degenerate case: a head, no `lines`, `max_lines` 0.
    pub fn new(
        head: Option<&'a mut String>,
        lines: &'a mut Vec<String>,
        row: &'a mut usize,
        col: &'a mut usize,
        max_lines: usize,
    ) -> Self {
        Self { head, lines, row, col, max_lines }
    }

    fn off(&self) -> usize {
        usize::from(self.head.is_some())
    }

    fn last_row(&self) -> usize {
        (self.lines.len() + self.off()).saturating_sub(1)
    }

    fn row_ref(&self, row: usize) -> &str {
        match &self.head {
            Some(h) if row == 0 => h,
            _ => &self.lines[row - self.off()],
        }
    }

    fn row_len(&self, row: usize) -> usize {
        self.row_ref(row).chars().count()
    }

    fn row_mut(&mut self, row: usize) -> &mut String {
        let off = usize::from(self.head.is_some());
        match self.head.as_deref_mut() {
            Some(h) if row == 0 => h,
            _ => &mut self.lines[row - off],
        }
    }

    /// Stale-point hygiene: clamp, never slice bad.
    fn clamp(&mut self) {
        *self.row = (*self.row).min(self.last_row());
        *self.col = (*self.col).min(self.row_len(*self.row));
    }

    /// Everything a mode's shared keys do: Ctrl+U/W (readline's line-discard
    /// and word-rubout, relative to the point), insert, Backspace/Delete,
    /// ←→↑↓, Home/End. Any other key is ignored — Enter, Tab and Esc are the
    /// mode's own. U16: a modified char other than Ctrl+U/W is discarded, so
    /// a chord we don't implement never leaves its letter behind.
    pub fn edit(&mut self, key: KeyEvent) {
        self.clamp();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let (row, col, off) = (*self.row, *self.col, self.off());
        let len = self.row_len(row);
        match key.code {
            KeyCode::Char('u') if ctrl => {
                let cur = self.row_mut(row);
                *cur = cur[byte_at(cur, col)..].to_string();
                *self.col = 0;
            }
            KeyCode::Char('w') if ctrl => {
                let cur = self.row_mut(row);
                let at = byte_at(cur, col);
                let head = erase_word(&cur[..at]);
                let new_col = head.chars().count();
                *cur = head + &cur[at..];
                *self.col = new_col;
            }
            // Shift is how an uppercase letter arrives (kitty CSI-u spells
            // `A` as Shift+`a`), so it is the one modifier an insert may carry.
            KeyCode::Char(c) if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
                let cur = self.row_mut(row);
                cur.insert(byte_at(cur, col), c);
                *self.col += 1;
            }
            KeyCode::Char(_) => {}
            KeyCode::Backspace => {
                if col > 0 {
                    let cur = self.row_mut(row);
                    cur.remove(byte_at(cur, col - 1));
                    *self.col -= 1;
                } else if row > off {
                    let cur = self.lines.remove(row - off);
                    *self.row -= 1;
                    *self.col = self.lines[row - off - 1].chars().count();
                    self.lines[row - off - 1].push_str(&cur);
                }
            }
            KeyCode::Delete => {
                if col < len {
                    let cur = self.row_mut(row);
                    cur.remove(byte_at(cur, col));
                } else if row >= off && row - off + 1 < self.lines.len() {
                    let next = self.lines.remove(row - off + 1);
                    self.lines[row - off].push_str(&next);
                }
            }
            KeyCode::Left => {
                if col > 0 {
                    *self.col -= 1;
                } else if row > 0 {
                    *self.row -= 1;
                    *self.col = self.row_len(row - 1);
                }
            }
            KeyCode::Right => {
                if col < len {
                    *self.col += 1;
                } else if row < self.last_row() {
                    *self.row += 1;
                    *self.col = 0;
                }
            }
            KeyCode::Up if row > 0 => {
                *self.row -= 1;
                *self.col = col.min(self.row_len(row - 1));
            }
            KeyCode::Down if row < self.last_row() => {
                *self.row += 1;
                *self.col = col.min(self.row_len(row + 1));
            }
            KeyCode::Home => *self.col = 0,
            KeyCode::End => *self.col = len,
            _ => {}
        }
    }

    /// A line break (Shift/Ctrl/Alt+Enter). On the head it *descends* into
    /// the lines, landing at the first line's END — the head is single-line
    /// by contract, so "break" there means "start writing the note". On a
    /// line it splits at the point, refused whole at `max_lines` (losing the
    /// split is recoverable, losing the text under it is not).
    pub fn brk(&mut self) {
        self.clamp();
        let off = self.off();
        if *self.row < off {
            *self.row = 1;
            *self.col = self.lines[0].chars().count();
        } else if self.lines.len() < self.max_lines {
            let i = *self.row - off;
            let at = byte_at(&self.lines[i], *self.col);
            let tail = self.lines[i].split_off(at);
            self.lines.insert(i + 1, tail);
            *self.row += 1;
            *self.col = 0;
        }
    }

    /// U8(b): a paste lands at the point and leaves it after the insertion.
    /// On the head only printables survive (no control byte may reach a
    /// title); on a line, newlines do too (CR/CRLF normalize to LF), and
    /// overflow past `max_lines` folds into the last line, space-joined,
    /// rather than vanishing.
    pub fn paste(&mut self, text: &str) {
        let (row, off) = (*self.row, self.off());
        if row < off {
            let clean = strip_control(text, false);
            let at = (*self.col).min(self.row_len(row));
            let cur = self.row_mut(row);
            cur.insert_str(byte_at(cur, at), &clean);
            *self.col = at + clean.chars().count();
            return;
        }
        let clean = text.replace("\r\n", "\n").replace('\r', "\n");
        let clean = strip_control(&clean, true);
        let mut n = (row - off).min(self.lines.len().saturating_sub(1));
        let at = byte_at(&self.lines[n], *self.col);
        let tail = self.lines[n].split_off(at);
        let mut parts = clean.split('\n');
        if let Some(first) = parts.next() {
            self.lines[n].push_str(first);
        }
        for part in parts {
            if self.lines.len() < self.max_lines {
                n += 1;
                self.lines.insert(n, part.to_string());
            } else {
                if !self.lines[n].is_empty() && !part.is_empty() {
                    self.lines[n].push(' ');
                }
                self.lines[n].push_str(part);
            }
        }
        *self.col = self.lines[n].chars().count();
        self.lines[n].push_str(&tail);
        *self.row = n + off;
    }
}

/// Byte offset of char index `at` in `s` (clamped to the end) — a name can
/// hold multi-byte chars, and slicing one down the middle panics.
fn byte_at(s: &str, at: usize) -> usize {
    s.char_indices().nth(at).map_or(s.len(), |(b, _)| b)
}

/// readline's `unix-word-rubout`: drop trailing whitespace, then the one
/// whitespace-delimited word in front of it.
fn erase_word(buffer: &str) -> String {
    let mut chars: Vec<char> = buffer.chars().collect();
    while chars.last().is_some_and(|c| c.is_whitespace()) {
        chars.pop();
    }
    while chars.last().is_some_and(|c| !c.is_whitespace()) {
        chars.pop();
    }
    chars.into_iter().collect()
}

fn strip_control(s: &str, keep_newline: bool) -> String {
    s.chars().filter(|&c| !c.is_control() || (keep_newline && c == '\n')).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn erase_word_drops_trailing_space_then_one_word() {
        assert_eq!(erase_word("abc def"), "abc ");
        assert_eq!(erase_word("abc def  "), "abc ");
        assert_eq!(erase_word("abc"), "");
        assert_eq!(erase_word("   "), "");
        assert_eq!(erase_word(""), "");
        assert_eq!(erase_word(&erase_word("one two three")), "one ");
        assert_eq!(erase_word(&erase_word(&erase_word("one two three"))), "");
    }

    #[test]
    fn strip_control_drops_controls_and_optionally_keeps_newline() {
        let s = "a\tb\x1bc\nd";
        assert_eq!(strip_control(s, false), "abcd");
        assert_eq!(strip_control(s, true), "abc\nd");
        assert_eq!(strip_control("plain 日本", false), "plain 日本");
    }

    #[test]
    fn head_seam_is_a_wall_but_plain_lines_join() {
        let (mut head, mut lines, mut row, mut col) =
            ("ab".to_string(), vec!["cd".to_string()], 1, 0);
        Field::new(Some(&mut head), &mut lines, &mut row, &mut col, 8)
            .edit(key(KeyCode::Backspace));
        assert_eq!((head.as_str(), lines.as_slice(), row), ("ab", &["cd".to_string()][..], 1));
        row = 0;
        col = 2;
        Field::new(Some(&mut head), &mut lines, &mut row, &mut col, 8).edit(key(KeyCode::Delete));
        assert_eq!((head.as_str(), lines.len()), ("ab", 1));

        let (mut lines, mut row, mut col) = (vec!["ab".to_string(), "cd".to_string()], 1, 0);
        Field::new(None, &mut lines, &mut row, &mut col, 8).edit(key(KeyCode::Backspace));
        assert_eq!((lines.as_slice(), row, col), (&["abcd".to_string()][..], 0, 2));
    }

    #[test]
    fn single_line_head_ignores_vertical_motion_and_modified_chars() {
        let (mut head, mut none, mut row, mut col) = ("ab".to_string(), vec![], 0, 1);
        let mut f = Field::new(Some(&mut head), &mut none, &mut row, &mut col, 0);
        f.edit(key(KeyCode::Down));
        f.edit(key(KeyCode::Up));
        f.edit(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
        f.edit(key(KeyCode::Char('é')));
        assert_eq!((head.as_str(), row, col), ("aéb", 0, 2));
    }
}
