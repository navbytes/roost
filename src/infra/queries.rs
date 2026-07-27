//! The pane-side query responder: roost is the *terminal* for each pane, and
//! terminals get interrogated. Every modern TUI stack probes its terminal at
//! startup — crossterm sends `CSI ?u`+`CSI c` and blocks on the answer, yazi
//! runs DA1-terminated capability rounds, atuin waits on a cursor-position
//! report — and a terminal that stays silent stalls them for seconds per
//! probe (SPEC-parity P4 measured: atuin aborts at ~2 s, yazi flashes its
//! timeout warning, crossterm-0.28 clients hang, roost-inside-roost
//! deadlocks). This module watches each pane's *output* stream for those
//! queries and answers them, in stream-encounter order, with the honest state
//! of that pane's vt100 screen.
//!
//! Answered (replies modeled on tmux 3.5a's `input.c`):
//! - kitty keyboard protocol (`CSI ?u` query, `CSI > flags u` push,
//!   `CSI < n u` pop) — the original tenant of this module (see below).
//! - DA1 `CSI c` / `CSI 0 c` → `CSI ?1;2c` (VT100 with AVO). The single
//!   highest-value reply: it terminates crossterm's enhancement probe and
//!   every "answer DA1 or time out" round in the wild.
//! - DA2 `CSI > c` → `CSI >84;0;0c` (tmux's identity — 84 = 'T'; apps
//!   special-case it as "conservative multiplexer", which is what roost is).
//! - DSR 5 (status) → `CSI 0n`; DSR 6 (cursor position) →
//!   `CSI {row+1};{col+1}R` from the pane screen's current cursor.
//! - DECRQM `CSI ? Pd $p` → `CSI ? Pd;{value} $y` for the modes the vt100
//!   screen actually tracks (1 DECCKM, 25 DECTCEM, 2004 bracketed paste);
//!   every untracked mode honestly reports 0 ("not recognized") rather than
//!   affirming state roost doesn't hold.
//! - XTVERSION `CSI > q` → `DCS >|roost {version} ST`.
//! - XTWINOPS `CSI 18 t` (text-area size in cells) → `CSI 8;{rows};{cols}t`;
//!   `CSI 14 t` (text area in pixels) and `CSI 16 t` (cell size in pixels)
//!   only when real pixel geometry was plumbed in — never invented.
//!
//! Deliberately NOT answered (SPEC-parity Appendix B's rationale):
//! - the kitty graphics APC probe: roost cannot composite images, and with
//!   DA1 answered yazi & co. pick their block-cell fallback instantly and
//!   honestly instead of timing out.
//! - XTGETTCAP: a terminfo-string oracle roost would have to lie through;
//!   apps fall back to `TERM=xterm-256color`, which is already the contract.
//! - DECRQSS: presentation state (DECSCUSR, SGR, margins) roost doesn't
//!   track yet — a wrong answer is worse than silence here.
//! - OSC 10/11 color queries: roost doesn't know the host's real palette;
//!   inventing one would poison apps' light/dark-background detection.
//!
//! Scanning: a minimal Ground/Esc/CSI state machine that persists across
//! chunks, so sequences split across reads still match. Anything that isn't
//! a recognized *query* falls through untouched — including the byte-exact
//! shapes of our own replies (`CSI ?1;2c`, `CSI {r};{c}R`, `CSI ?{f}u`…), so
//! a pane that echoes its stdin back (a parked `cat`) can never trap roost
//! in a reply loop. DCS/OSC/APC bodies aren't tracked as string state (same
//! simplification the kitty-only ancestor made): their introducers drop the
//! scanner to Ground and their payloads are ignored.
//!
//! -- kitty keyboard protocol (the original module) --------------------------
//!
//! A kitty-aware app (pi, Claude Code, Bubbletea v2, …) probes and configures
//! its terminal by emitting, in the pane's *output* stream:
//!
//!   CSI ? u          query current progressive-enhancement flags
//!   CSI > <flags> u  push flags onto a stack (omitted ⇒ 0)
//!   CSI < <n> u      pop n stack entries (omitted ⇒ 1)
//!
//! We maintain the flag stack and answer the query with `CSI ? <flags> u`.
//! Once a pane has pushed flags with bit 1 (disambiguate) set, roost encodes
//! modified Enter as the CSI-u form the app asked for. If a pane never opts
//! in, `disambiguate()` stays false and the input layer falls back to ESC+CR.
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
/// Wide enough for the longest legitimate query (`?2004$` for DECRQM).
const MAX_PARAMS: usize = 16;

/// DA1: VT100 with Advanced Video Option — the conservative identity tmux
/// ships, promising nothing roost doesn't do.
const DA1_REPLY: &[u8] = b"\x1b[?1;2c";
/// DA2: terminal type 84 ('T', tmux's), version 0.
const DA2_REPLY: &[u8] = b"\x1b[>84;0;0c";

enum Scan {
    Ground,
    Esc,
    Csi,
}

pub struct QueryResponder {
    stack: Vec<u8>,
    scan: Scan,
    buf: Vec<u8>,
}

impl QueryResponder {
    pub fn new() -> Self {
        Self { stack: Vec::new(), scan: Scan::Ground, buf: Vec::new() }
    }

    /// Current (top-of-stack) kitty flags; 0 when nothing is pushed.
    fn flags(&self) -> u8 {
        self.stack.last().copied().unwrap_or(0)
    }

    /// Does the pane want modified keys in the CSI-u encoding right now?
    pub fn disambiguate(&self) -> bool {
        self.flags() & DISAMBIGUATE != 0
    }

    /// Feed a chunk of the pane's output. Returns the bytes roost must write
    /// *back* to the pane: every query's reply, in the exact order the
    /// queries appeared in the stream — crossterm's `CSI ?u`+`CSI c` burst
    /// misparses unless the kitty reply precedes the DA1 reply. The state
    /// machine persists across calls, so sequences may split across chunks.
    ///
    /// `screen` must be the pane's screen with this chunk *already parsed
    /// into it* (call `parser.process` first): DSR 6 and DECRQM answers must
    /// reflect the state the app just finished setting up, not the state
    /// from one chunk ago. `pixels` is the pane's (width, height) pixel
    /// geometry, (0, 0) when unknown — the 14/16t replies then stay silent.
    pub fn feed(&mut self, bytes: &[u8], screen: &vt100::Screen, pixels: (u16, u16)) -> Vec<u8> {
        let mut out = Vec::new();
        for &b in bytes {
            self.byte(b, screen, pixels, &mut out);
        }
        out
    }

    fn byte(&mut self, b: u8, screen: &vt100::Screen, pixels: (u16, u16), out: &mut Vec<u8>) {
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
                    self.finish(b, screen, pixels, out);
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

    /// A complete CSI sequence: `buf` holds its parameter/intermediate bytes,
    /// `final_byte` its command. Queries produce a reply; everything else —
    /// explicitly including the response-shaped sequences a stdin-echoing
    /// pane could feed back — produces nothing.
    fn finish(
        &mut self,
        final_byte: u8,
        screen: &vt100::Screen,
        pixels: (u16, u16),
        out: &mut Vec<u8>,
    ) {
        match final_byte {
            // -- kitty keyboard protocol ---------------------------------
            b'u' => match self.buf.as_slice() {
                // CSI ? u — query: reply with the current flags. Exactly
                // `?`: a flag *report* (`CSI ? Pn u` — our own reply shape)
                // is not a query.
                b"?" => out.extend_from_slice(format!("\x1b[?{}u", self.flags()).as_bytes()),
                // CSI > <flags> u — push (omitted/malformed flags ⇒ 0).
                [b'>', rest @ ..] => {
                    let flags =
                        parse_num(rest).and_then(|v| u8::try_from(v).ok()).unwrap_or(0);
                    if self.stack.len() >= MAX_STACK {
                        // Overflow: evict the oldest, per the spec.
                        self.stack.remove(0);
                    }
                    self.stack.push(flags);
                }
                // CSI < <n> u — pop n entries (omitted defaults to 1).
                [b'<', rest @ ..] => {
                    let n = parse_num(rest).unwrap_or(1).max(1);
                    for _ in 0..n {
                        self.stack.pop();
                    }
                }
                _ => {}
            },
            // -- device attributes ---------------------------------------
            b'c' => match self.buf.as_slice() {
                b"" | b"0" => out.extend_from_slice(DA1_REPLY),
                b">" | b">0" => out.extend_from_slice(DA2_REPLY),
                _ => {} // `?…c` is a DA1 *response* — never re-answered
            },
            // -- device status reports -----------------------------------
            b'n' => match self.buf.as_slice() {
                b"5" => out.extend_from_slice(b"\x1b[0n"), // "operating OK"
                b"6" => {
                    let (row, col) = screen.cursor_position();
                    out.extend_from_slice(
                        format!("\x1b[{};{}R", row + 1, col + 1).as_bytes(),
                    );
                }
                _ => {}
            },
            // -- DECRQM (DEC private mode query), CSI ? Pd $ p -----------
            b'p' => {
                let mode = self
                    .buf
                    .strip_prefix(b"?")
                    .and_then(|r| r.strip_suffix(b"$"))
                    .and_then(parse_num);
                if let Some(mode) = mode {
                    let val = decrqm_value(mode, screen);
                    out.extend_from_slice(format!("\x1b[?{mode};{val}$y").as_bytes());
                }
            }
            // -- XTVERSION, CSI > q --------------------------------------
            // (DECSCUSR is `CSI Ps SP q` — its SP intermediate lands in
            // `buf`, so it can never match here.)
            b'q' => {
                if matches!(self.buf.as_slice(), b">" | b">0") {
                    out.extend_from_slice(
                        format!("\x1bP>|roost {}\x1b\\", env!("CARGO_PKG_VERSION")).as_bytes(),
                    );
                }
            }
            // -- XTWINOPS reports, CSI Ps t ------------------------------
            b't' => match self.buf.as_slice() {
                // Text area size in cells: always known.
                b"18" => {
                    let (rows, cols) = screen.size();
                    out.extend_from_slice(format!("\x1b[8;{rows};{cols}t").as_bytes());
                }
                // Text area size in pixels: only when real geometry exists.
                b"14" if pixels.0 > 0 && pixels.1 > 0 => {
                    out.extend_from_slice(
                        format!("\x1b[4;{};{}t", pixels.1, pixels.0).as_bytes(),
                    );
                }
                // Cell size in pixels, derived from the text-area geometry.
                b"16" if pixels.0 > 0 && pixels.1 > 0 => {
                    let (rows, cols) = screen.size();
                    if rows > 0 && cols > 0 {
                        let (ch, cw) = (pixels.1 / rows, pixels.0 / cols);
                        if ch > 0 && cw > 0 {
                            out.extend_from_slice(format!("\x1b[6;{ch};{cw}t").as_bytes());
                        }
                    }
                }
                _ => {} // incl. 14/16 with unknown pixels: honest silence
            },
            _ => {}
        }
    }
}

/// DECRQM answer for one DEC private mode: 1 = set, 2 = reset for the modes
/// the pane's vt100 screen genuinely tracks; 0 = "not recognized" for
/// everything else. Mode 2026 (synchronized output) deliberately stays 0
/// until W3 actually implements it — never claim capacity that isn't there.
fn decrqm_value(mode: u16, screen: &vt100::Screen) -> u8 {
    match mode {
        // DECCKM — application cursor keys.
        1 => {
            if screen.application_cursor() {
                1
            } else {
                2
            }
        }
        // DECTCEM — text cursor enable: vt100 tracks the *hide* bit, so the
        // sense inverts.
        25 => {
            if screen.hide_cursor() {
                2
            } else {
                1
            }
        }
        // Bracketed paste.
        2004 => {
            if screen.bracketed_paste() {
                1
            } else {
                2
            }
        }
        _ => 0,
    }
}

/// Parse a decimal parameter; `None` when empty or malformed.
fn parse_num(bytes: &[u8]) -> Option<u16> {
    if bytes.is_empty() {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A blank screen for queries that don't consult terminal state.
    fn blank() -> vt100::Parser {
        vt100::Parser::new(24, 80, 0)
    }

    /// Feed against a given parser's screen with no pixel geometry.
    fn feed_on(r: &mut QueryResponder, p: &vt100::Parser, bytes: &[u8]) -> Vec<u8> {
        r.feed(bytes, p.screen(), (0, 0))
    }

    /// Feed with neither interesting screen state nor pixels.
    fn feed(r: &mut QueryResponder, bytes: &[u8]) -> Vec<u8> {
        feed_on(r, &blank(), bytes)
    }

    // -- kitty keyboard protocol (behavior carried over from kitty.rs) ----

    #[test]
    fn query_before_any_push_replies_zero_flags() {
        let mut k = QueryResponder::new();
        assert_eq!(feed(&mut k, b"\x1b[?u"), b"\x1b[?0u");
        assert!(!k.disambiguate());
    }

    #[test]
    fn push_enables_disambiguate_and_query_reflects_it() {
        let mut k = QueryResponder::new();
        // pi pushes flags 7 (disambiguate | report-event-types | alternate-keys).
        assert!(feed(&mut k, b"\x1b[>7u").is_empty());
        assert!(k.disambiguate());
        // A subsequent query now reports the pushed flags.
        assert_eq!(feed(&mut k, b"\x1b[?u"), b"\x1b[?7u");
    }

    #[test]
    fn pop_restores_previous_flags_and_empty_pop_resets() {
        let mut k = QueryResponder::new();
        feed(&mut k, b"\x1b[>1u");
        assert!(k.disambiguate());
        feed(&mut k, b"\x1b[<u"); // pop 1 (default) → stack empty → flags 0
        assert!(!k.disambiguate());
    }

    #[test]
    fn omitted_push_flags_default_to_zero_not_stale() {
        // zellij #4333 pitfall: a bare `CSI > u` must mean flags = 0, not
        // "keep whatever was active".
        let mut k = QueryResponder::new();
        feed(&mut k, b"\x1b[>1u");
        feed(&mut k, b"\x1b[>u"); // push with no flags ⇒ 0
        assert!(!k.disambiguate());
    }

    #[test]
    fn sequence_split_across_feeds_is_handled() {
        let mut k = QueryResponder::new();
        assert!(feed(&mut k, b"\x1b[>").is_empty());
        assert!(feed(&mut k, b"1").is_empty());
        assert!(feed(&mut k, b"u").is_empty());
        assert!(k.disambiguate());
    }

    #[test]
    fn ignores_unrelated_csi_and_plain_output() {
        // cursor hide, an SGR, plain text: silence. (The kitty-only ancestor
        // of this test also fed a bare `CSI c` here and expected silence —
        // answering that DA1 query is exactly what P4/W2 changed.)
        let mut k = QueryResponder::new();
        assert!(feed(&mut k, b"\x1b[?25lhello\x1b[0m world").is_empty());
        assert!(!k.disambiguate());
    }

    // -- the new queries, byte-exact and table-driven ---------------------

    #[test]
    fn stateless_queries_reply_byte_exact() {
        let ver = env!("CARGO_PKG_VERSION");
        let xtversion = format!("\x1bP>|roost {ver}\x1b\\");
        let cases: &[(&[u8], &[u8])] = &[
            (b"\x1b[c", DA1_REPLY),                      // DA1
            (b"\x1b[0c", DA1_REPLY),                     // DA1, explicit 0
            (b"\x1b[>c", DA2_REPLY),                     // DA2
            (b"\x1b[>0c", DA2_REPLY),                    // DA2, explicit 0
            (b"\x1b[5n", b"\x1b[0n"),                    // DSR 5: operating OK
            (b"\x1b[>q", xtversion.as_bytes()),          // XTVERSION
            (b"\x1b[>0q", xtversion.as_bytes()),         // XTVERSION, explicit 0
            (b"\x1b[18t", b"\x1b[8;24;80t"),             // text area in cells
        ];
        for (query, reply) in cases {
            let mut r = QueryResponder::new();
            assert_eq!(&feed(&mut r, query), reply, "query {:?}", String::from_utf8_lossy(query));
        }
    }

    #[test]
    fn dsr6_reports_the_post_chunk_cursor_position() {
        let mut p = blank();
        let mut r = QueryResponder::new();
        // The chunk moves the cursor and then asks where it is — process the
        // chunk into the parser first (as PtyPane does), then scan it.
        let chunk = b"ab\x1b[6n";
        p.process(chunk);
        assert_eq!(p.screen().cursor_position(), (0, 2));
        // 0-based (0, 2) → 1-based CPR `CSI 1;3R`.
        assert_eq!(feed_on(&mut r, &p, chunk), b"\x1b[1;3R");
    }

    #[test]
    fn decrqm_answers_tracked_modes_honestly_and_untracked_as_zero() {
        // (setup sequence, query, expected reply)
        let cases: &[(&[u8], &[u8], &[u8])] = &[
            // DECCKM: reset by default, set after ?1h.
            (b"", b"\x1b[?1$p", b"\x1b[?1;2$y"),
            (b"\x1b[?1h", b"\x1b[?1$p", b"\x1b[?1;1$y"),
            // DECTCEM: the screen tracks *hide*, so the sense inverts —
            // visible cursor (default) = set.
            (b"", b"\x1b[?25$p", b"\x1b[?25;1$y"),
            (b"\x1b[?25l", b"\x1b[?25$p", b"\x1b[?25;2$y"),
            // Bracketed paste: reset by default, set after ?2004h.
            (b"", b"\x1b[?2004$p", b"\x1b[?2004;2$y"),
            (b"\x1b[?2004h", b"\x1b[?2004$p", b"\x1b[?2004;1$y"),
            // Untracked modes — 2026 (sync output), 1004 (focus), 1049 (alt
            // screen is tracked by vt100 but not contracted here) → 0, "not
            // recognized": never affirm state roost doesn't hold.
            (b"", b"\x1b[?2026$p", b"\x1b[?2026;0$y"),
            (b"", b"\x1b[?1004$p", b"\x1b[?1004;0$y"),
        ];
        for (setup, query, reply) in cases {
            let mut p = blank();
            let mut r = QueryResponder::new();
            let mut chunk = setup.to_vec();
            chunk.extend_from_slice(query);
            p.process(&chunk);
            assert_eq!(
                &feed_on(&mut r, &p, &chunk),
                reply,
                "setup {:?}",
                String::from_utf8_lossy(setup)
            );
        }
    }

    #[test]
    fn pixel_reports_answer_only_with_known_geometry() {
        let p = blank(); // 24 rows x 80 cols
        // Unknown pixels: 14t and 16t stay silent; 18t still answers.
        let mut r = QueryResponder::new();
        assert!(r.feed(b"\x1b[14t\x1b[16t", p.screen(), (0, 0)).is_empty());
        assert_eq!(r.feed(b"\x1b[18t", p.screen(), (0, 0)), b"\x1b[8;24;80t");
        // Known pixels (800x480): 14t reports height;width, 16t the cell.
        assert_eq!(r.feed(b"\x1b[14t", p.screen(), (800, 480)), b"\x1b[4;480;800t");
        assert_eq!(r.feed(b"\x1b[16t", p.screen(), (800, 480)), b"\x1b[6;20;10t");
    }

    #[test]
    fn crossterm_burst_gets_kitty_reply_then_da1_in_stream_order() {
        // crossterm's supports_keyboard_enhancement sends both queries in
        // one write and needs the answers in the same order — DA1 first
        // would make it report "no kitty support" forever.
        let mut r = QueryResponder::new();
        assert_eq!(feed(&mut r, b"\x1b[?u\x1b[c"), b"\x1b[?0u\x1b[?1;2c");
    }

    #[test]
    fn queries_split_across_chunks_still_answer() {
        let mut r = QueryResponder::new();
        assert!(feed(&mut r, b"\x1b[6").is_empty());
        let p = blank();
        assert_eq!(feed_on(&mut r, &p, b"n"), b"\x1b[1;1R");
        // A DA1 split at the ESC boundary.
        assert!(feed(&mut r, b"\x1b").is_empty());
        assert_eq!(feed(&mut r, b"[c"), DA1_REPLY);
    }

    #[test]
    fn response_shaped_sequences_are_never_re_answered() {
        // A pane running `cat` echoes roost's own replies back into the
        // output stream; each must scan as inert or the responder feeds
        // itself forever.
        let mut r = QueryResponder::new();
        feed(&mut r, b"\x1b[>1u"); // give the kitty report a nonzero flag
        let echoes: &[&[u8]] = &[
            b"\x1b[?1;2c",   // DA1 response
            b"\x1b[>84;0;0c", // DA2 response
            b"\x1b[0n",      // DSR 5 response
            b"\x1b[3;1R",    // DSR 6 response (CPR)
            b"\x1b[?1u",     // kitty flag report
            b"\x1b[?2004;1$y", // DECRQM response
            b"\x1b[8;24;80t", // XTWINOPS 18t response
            b"\x1b[4;480;800t",
            b"\x1b[6;20;10t",
        ];
        for echo in echoes {
            assert!(
                feed(&mut r, echo).is_empty(),
                "echoed {:?} must not re-answer",
                String::from_utf8_lossy(echo)
            );
        }
    }

    #[test]
    fn deliberately_silent_probes_stay_silent() {
        let cases: &[&[u8]] = &[
            b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\", // kitty graphics probe
            b"\x1bP+q544e\x1b\\",                          // XTGETTCAP
            b"\x1bP$q q\x1b\\",                            // DECRQSS (DECSCUSR)
            b"\x1b]10;?\x07",                              // OSC 10 fg query
            b"\x1b]11;?\x1b\\",                            // OSC 11 bg query
            b"\x1b[5 q",                                   // DECSCUSR set (not XTVERSION)
            b"\x1b[?6n",                                   // DECXCPR (not contracted)
            b"\x1b[14t\x1b[16t",                           // pixel reports, no geometry
            b"\x1b[8;30;100t",                             // a resize *request*, not a query
        ];
        for probe in cases {
            let mut r = QueryResponder::new();
            assert!(
                feed(&mut r, probe).is_empty(),
                "probe {:?} must stay silent",
                String::from_utf8_lossy(probe)
            );
        }
    }
}
