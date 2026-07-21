//! A minimal kitty-keyboard-protocol *terminal* side, so roost can honestly
//! forward Shift+Enter / Ctrl+Enter (and other modified keys) to a pane the
//! way zellij and wezterm do — rather than the ESC+CR hack, which downstream
//! actually reads as Alt+Enter.
//!
//! roost is the *terminal* for each pane. A kitty-aware app (pi, Claude Code,
//! Bubbletea v2, …) probes and configures its terminal by emitting, in the
//! pane's *output* stream:
//!
//!   CSI ? u          query current progressive-enhancement flags
//!   CSI > <flags> u  push flags onto a stack (omitted ⇒ 0)
//!   CSI < <n> u      pop n stack entries (omitted ⇒ 1)
//!
//! We watch each pane's output for these, maintain the flag stack, and answer
//! the query with `CSI ? <flags> u`. Once a pane has pushed flags with bit 1
//! (disambiguate) set, roost encodes modified Enter as the CSI-u form the app
//! asked for. If a pane never opts in, `disambiguate()` stays false and the
//! input layer falls back to ESC+CR — a non-kitty app can't receive a distinct
//! Shift+Enter anyway.
//!
//! Ref: https://sw.kovidgoyal.net/kitty/keyboard-protocol/
//!
//! Simplifications vs. the full spec (adequate for the newline use case):
//! a single flag stack rather than separate main/alt-screen stacks, and the
//! in-place `CSI = flags ; mode u` set form is ignored (apps use push/pop).

/// Progressive-enhancement flag: "disambiguate escape codes" (bit 0). This is
/// the bit that makes an app want modified keys as CSI-u.
const DISAMBIGUATE: u8 = 0x1;
/// Cap the stack depth, matching kitty, so a misbehaving app can't grow it
/// without bound.
const MAX_STACK: usize = 16;
/// Cap the CSI parameter buffer so a never-terminated sequence can't grow.
const MAX_PARAMS: usize = 16;

enum Scan {
    Ground,
    Esc,
    Csi,
}

pub struct KittyKeyboard {
    stack: Vec<u8>,
    scan: Scan,
    buf: Vec<u8>,
}

impl KittyKeyboard {
    pub fn new() -> Self {
        Self { stack: Vec::new(), scan: Scan::Ground, buf: Vec::new() }
    }

    /// Current (top-of-stack) flags; 0 when nothing is pushed.
    fn flags(&self) -> u8 {
        self.stack.last().copied().unwrap_or(0)
    }

    /// Does the pane want modified keys in the CSI-u encoding right now?
    pub fn disambiguate(&self) -> bool {
        self.flags() & DISAMBIGUATE != 0
    }

    /// Feed a chunk of the pane's output. Returns any bytes roost must write
    /// *back* to the pane (currently only the reply to a `CSI ? u` query).
    /// The state machine persists across calls, so sequences may split across
    /// chunk boundaries.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for &b in bytes {
            self.byte(b, &mut out);
        }
        out
    }

    fn byte(&mut self, b: u8, out: &mut Vec<u8>) {
        match self.scan {
            Scan::Ground => {
                if b == 0x1b {
                    self.scan = Scan::Esc;
                }
            }
            Scan::Esc => match b {
                b'[' => {
                    self.scan = Scan::Csi;
                    self.buf.clear();
                }
                0x1b => {} // a run of ESCs — stay armed
                _ => self.scan = Scan::Ground,
            },
            Scan::Csi => match b {
                0x1b => self.scan = Scan::Esc, // aborted, new sequence
                0x40..=0x7e => {
                    self.finish(b, out);
                    self.scan = Scan::Ground;
                }
                _ => {
                    self.buf.push(b);
                    if self.buf.len() > MAX_PARAMS {
                        self.scan = Scan::Ground; // give up on an overlong CSI
                    }
                }
            },
        }
    }

    fn finish(&mut self, final_byte: u8, out: &mut Vec<u8>) {
        if final_byte != b'u' {
            return; // only the kitty-keyboard `... u` sequences concern us
        }
        match self.buf.first().copied() {
            // CSI ? u — query: reply with the current flags.
            Some(b'?') => out.extend_from_slice(format!("\x1b[?{}u", self.flags()).as_bytes()),
            // CSI > <flags> u — push (omitted flags default to 0).
            Some(b'>') => {
                let flags = parse_u8(&self.buf[1..]).unwrap_or(0);
                if self.stack.len() < MAX_STACK {
                    self.stack.push(flags);
                } else {
                    // Overflow: evict the oldest, per the spec.
                    self.stack.remove(0);
                    self.stack.push(flags);
                }
            }
            // CSI < <n> u — pop n entries (omitted defaults to 1).
            Some(b'<') => {
                let n = parse_u8(&self.buf[1..]).unwrap_or(1).max(1);
                for _ in 0..n {
                    self.stack.pop();
                }
            }
            _ => {}
        }
    }
}

fn parse_u8(bytes: &[u8]) -> Option<u8> {
    if bytes.is_empty() {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_before_any_push_replies_zero_flags() {
        let mut k = KittyKeyboard::new();
        assert_eq!(k.feed(b"\x1b[?u"), b"\x1b[?0u");
        assert!(!k.disambiguate());
    }

    #[test]
    fn push_enables_disambiguate_and_query_reflects_it() {
        let mut k = KittyKeyboard::new();
        // pi pushes flags 7 (disambiguate | report-event-types | alternate-keys).
        assert!(k.feed(b"\x1b[>7u").is_empty());
        assert!(k.disambiguate());
        // A subsequent query now reports the pushed flags.
        assert_eq!(k.feed(b"\x1b[?u"), b"\x1b[?7u");
    }

    #[test]
    fn pop_restores_previous_flags_and_empty_pop_resets() {
        let mut k = KittyKeyboard::new();
        k.feed(b"\x1b[>1u");
        assert!(k.disambiguate());
        k.feed(b"\x1b[<u"); // pop 1 (default) → stack empty → flags 0
        assert!(!k.disambiguate());
    }

    #[test]
    fn omitted_push_flags_default_to_zero_not_stale() {
        // zellij #4333 pitfall: a bare `CSI > u` must mean flags = 0, not
        // "keep whatever was active".
        let mut k = KittyKeyboard::new();
        k.feed(b"\x1b[>1u");
        k.feed(b"\x1b[>u"); // push with no flags ⇒ 0
        assert!(!k.disambiguate());
    }

    #[test]
    fn sequence_split_across_feeds_is_handled() {
        let mut k = KittyKeyboard::new();
        assert!(k.feed(b"\x1b[>").is_empty());
        assert!(k.feed(b"1").is_empty());
        assert!(k.feed(b"u").is_empty());
        assert!(k.disambiguate());
    }

    #[test]
    fn ignores_unrelated_csi_and_plain_output() {
        let mut k = KittyKeyboard::new();
        // cursor hide, an SGR, a device-attributes query, plain text
        assert!(k.feed(b"\x1b[?25lhello\x1b[0m\x1b[c world").is_empty());
        assert!(!k.disambiguate());
    }
}
