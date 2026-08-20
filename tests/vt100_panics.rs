//! No sequence of bytes a pane emits may panic the terminal parser.
//!
//! This is a reliability gate, not a conformance one. `Screen::text` is full
//! of `.unwrap()`s on cell lookups, each justified by an invariant about
//! where a wide glyph's continuation cell must be — and a panic there does
//! not merely garble a frame. It unwinds out of `main.rs`'s event loop
//! *past* `App::shutdown`, so roost dies without killing or reaping a single
//! pane: every agent is left running, detached, and the workspace is never
//! saved. Exactly the failure `tests/signal_shutdown.rs` exists to prevent,
//! reached by printing an emoji.
//!
//! The parser is a `dev-dependency` as well as a real one (vendor/vt100),
//! so this drives it directly rather than through a pty — the bytes are the
//! whole input, and a hundred thousand of them a second is the point.

use std::time::Instant;

/// A wide glyph in a grid narrower than the glyph.
///
/// `col_wrap` cannot make room for the continuation cell in a one-column
/// grid, so the draw ran off the end of the row. A pane one column wide is
/// reachable by dragging a terminal narrow with a few splits open, and any
/// emoji or CJK character in it was enough to take roost down.
#[test]
fn a_wide_glyph_in_a_one_column_grid_does_not_panic() {
    for glyph in ["\u{4e2d}", "\u{1f600}", "\u{1f1e6}\u{1f1e8}"] {
        for rows in [1u16, 2, 40] {
            let mut p = vt100::Parser::new(rows, 1, 0);
            p.process(glyph.as_bytes());
            p.process(b"abc");
            p.process(glyph.as_bytes());
            // The glyph cannot be shown in one column — but the parser must
            // stay usable, which is the whole contract here.
            let _ = p.screen().contents();
        }
    }
}

/// A shrink that leaves a wide glyph's *base* in the new last column.
///
/// The alternate screen deliberately does not reflow (an app there repaints
/// on SIGWINCH), so the narrow is a literal truncation: `Row::resize` cut
/// the continuation cell away and left the base behind, marked wide, with
/// nothing after it. The next character drawn over it read that missing
/// continuation through an `.unwrap()`.
///
/// Found by the sweep below at three columns; the loop covers the same
/// shape at every width a pane can plausibly be narrowed to.
#[test]
fn narrowing_onto_a_wide_glyph_does_not_panic() {
    for cols in 1u16..12 {
        let mut p = vt100::Parser::new(24, 40, 100);
        p.process(b"\x1b[?1049h"); // alternate screen: no reflow on resize
        p.process("se\u{4e2d}xy".as_bytes());
        p.set_size(8, cols);
        p.process(b"A");
        let _ = p.screen().contents();
    }
    // ...and the same without the alternate screen, where a scroll region
    // suppresses the reflow instead.
    for cols in 1u16..12 {
        let mut p = vt100::Parser::new(24, 40, 100);
        p.process(b"\x1b[2;7r"); // DECSTBM — an app driving fixed geometry
        p.process("se\u{4e2d}xy".as_bytes());
        p.set_size(8, cols);
        p.process(b"A");
        let _ = p.screen().contents();
    }
}

/// Deterministic sweep: escape-heavy byte fragments, resizes, scrollback
/// moves and reads, replayed against a fresh parser per seed. Both bugs
/// above came out of this; it stays as the standing gate, seeded so a
/// failure is reproducible from the seed alone.
///
/// ponytail: fixed budget rather than a time-boxed loop — CI wants the same
/// work every run, and a deeper sweep is one argument away (the shrinking
/// driver that minimised the two repros above is not shipped; the seed and
/// this vocabulary reproduce anything it would find).
#[test]
fn no_byte_sequence_panics_the_parser() {
    const SEEDS: u64 = 400;
    const OPS: usize = 800;

    // Weighted toward the control/escape machinery: pure random bytes are
    // almost all printable, and printable bytes were never the risk.
    let vocab: &[&[u8]] = &[
        b"\x1b[", b"\x1b]", b"\x1b", b"\x07", b"\x9b", b"\x1bP", b"\x1b_", b"\x1b^", b"\x1bX",
        b";", b"?", b":", b"0", b"1", b"9", b"999999999", b"-1", b"2147483648",
        b"H", b"J", b"K", b"m", b"r", b"h", b"l", b"n", b"t", b"S", b"T", b"L", b"M", b"@",
        b"P", b"X", b"d", b"G", b"A", b"B", b"C", b"D", b"E", b"F", b"g", b"c", b"q", b"p",
        b"$", b"\"", b"b", b"Z", b"I", b"`", b"a", b"e", b"f", b"s", b"u", b"W",
        b"\x1b[?1049h", b"\x1b[?1049l", b"\x1b[?2004h", b"\x1b[?2026h", b"\x1b[?2026l",
        b"\x1b#8", b"\x1b(0", b"\x1b)B", b"\x1b7", b"\x1b8", b"\x1bM", b"\x1bD", b"\x1bE",
        b"\r", b"\n", b"\t", b"\x08", b"\x0b", b"\x0c", b"\x0e", b"\x0f", b"\x00",
        "\u{1f600}".as_bytes(),   // wide
        "\u{4e2d}".as_bytes(),    // wide
        "\u{1f1e6}".as_bytes(),   // regional indicator
        "\u{0301}".as_bytes(),    // combining
        "\u{fe0f}".as_bytes(),    // VS16 — promotes its base to two columns
        "\u{200d}".as_bytes(),    // ZWJ
        "\u{feff}".as_bytes(),
        b"\xff\xfe", b"\xc3", b"\xed\xa0\x80", // invalid UTF-8
        b"abc", b"                                        ",
        b"\x1b]0;title\x07", b"\x1b]52;c;aGk=\x07", b"\x1b]777;notify;a;b\x07",
    ];

    // xorshift64: reproducible everywhere, no dependency.
    let mut state: u64 = 0;
    let mut next = move |seed_init: Option<u64>| -> u64 {
        if let Some(s) = seed_init {
            state = s;
        }
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    let started = Instant::now();
    for seed in 1..=SEEDS {
        let mut r = |init: Option<u64>| next(init);
        r(Some(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1));
        let mut below = |n: u64| r(None) % n;

        let rows = 1 + below(60) as u16;
        let cols = 1 + below(200) as u16;
        let mut p = vt100::Parser::new(rows, cols, below(200) as usize);
        for _ in 0..OPS {
            match below(100) {
                0..=1 => p.set_size(1 + below(60) as u16, 1 + below(200) as u16),
                2 => p.set_scrollback(below(300) as usize),
                3 => drop(p.screen().contents()),
                4 => drop(p.screen().cell(below(60) as u16, below(200) as u16)),
                5 => drop(p.take_effects()),
                6 => drop(p.take_sync_snapshot()),
                7 => drop(p.screen().rows(below(50) as u16, below(200) as u16).count()),
                8 => drop((p.screen().title().len(), p.screen().audible_bell_count())),
                _ => p.process(vocab[below(vocab.len() as u64) as usize]),
            }
        }
    }
    eprintln!("vt100 sweep: {SEEDS} seeds x {OPS} ops in {:?}", started.elapsed());
}
