//! A line of history must not cost a full row of cells.
//!
//! A live row is exactly `cols` cells wide because every draw indexes it by
//! column. A row banked into the scrollback is never written to again, so
//! the untouched cells past its text are pure padding — and at 36 bytes a
//! `vt100::Cell`, that padding was almost all of roost's memory. A
//! 200-column pane paid 7.2 KB for every line of history regardless of its
//! length: 34 MB per pane once the 5,000-row scrollback filled, scaling with
//! the window's width, and measured at 148 MB of RSS for four such panes.
//!
//! Its own test binary because it installs a counting global allocator —
//! the only way to assert on memory rather than on a number someone read off
//! `ps` once and wrote into a comment.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static LIVE: AtomicUsize = AtomicUsize::new(0);

struct Counting;

// SAFETY: every method forwards to `System` unchanged and only adds
// bookkeeping; the pointers, layouts and null-handling are all System's.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        // SAFETY: `l` is forwarded untouched to the system allocator, which
        // is the one that gets to have requirements about it.
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            LIVE.fetch_add(l.size(), Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        // SAFETY: `p`/`l` are forwarded untouched — this allocator only ever
        // hands back what `System` gave it, so the pairing is System's own.
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        // SAFETY: same forwarding as `alloc`/`dealloc` above.
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            LIVE.fetch_add(new, Ordering::Relaxed);
            LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

const COLS: u16 = 200;
const SCROLLBACK: usize = 5000;

/// What one pane holds after its scrollback fills with `line`, and what the
/// oldest still-reachable line reads back as.
fn held_bytes(line: &str) -> (usize, String) {
    let before = LIVE.load(Ordering::Relaxed);
    let mut p = vt100::Parser::new(24, COLS, SCROLLBACK);
    // Comfortably past the cap, so the scrollback is full and steady.
    for _ in 0..(SCROLLBACK + 2000) {
        p.process(line.as_bytes());
        p.process(b"\r\n");
    }
    let held = LIVE.load(Ordering::Relaxed).saturating_sub(before);
    // Scroll all the way back and read the top visible row, so the assertion
    // covers the rows the trim actually touched — not just the live grid.
    p.set_scrollback(SCROLLBACK);
    let top = p.screen().rows(0, COLS).next().unwrap_or_default();
    (held, top)
}

/// The untrimmed cost of a full scrollback at this width: every row a fixed
/// `cols` cells wide, whatever it holds.
fn untrimmed_bytes() -> usize {
    SCROLLBACK * usize::from(COLS) * std::mem::size_of::<vt100::Cell>()
}

/// One test, not three: `LIVE` is a process-wide counter and libtest runs
/// tests in parallel, so anything allocating concurrently lands in the
/// middle of a measurement (measured: 15 MB of noise on a 1.3 MB figure).
/// All of it is here, in order, sharing the quiet process.
#[test]
fn banked_history_keeps_its_ink_and_sheds_its_padding() {
    // First, the fidelity half, before any measurement starts: `ESC[K`
    // erases to end of line *with the current attrs*, so an app painting a
    // coloured bar leaves a run of cells that hold no character and are
    // still visible output. Trimming on "has contents" alone would drop
    // that colour from history the moment the line scrolled off.
    {
        let mut p = vt100::Parser::new(4, 40, 100);
        p.process(b"\x1b[41mhi\x1b[K\r\n"); // red background, erase to EOL
        // Bank it: print past the bottom of a 4-row grid.
        for _ in 0..10 {
            p.process(b"x\r\n");
        }
        p.set_scrollback(100); // all the way back to the coloured line
        let cell = p.screen().cell(0, 30).expect("column 30 of the banked line");
        assert_eq!(
            cell.bgcolor(),
            vt100::Color::Idx(1),
            "an erased-with-attrs tail keeps its colour in history"
        );
    }

    let untrimmed = untrimmed_bytes();

    // Short lines: a full scrollback of 5-character lines must cost a small
    // fraction of a full grid of cells.
    let (short_held, short_top) = held_bytes("hello");
    eprintln!(
        "short lines: {:.1} MB held, untrimmed would be {:.1} MB",
        short_held as f64 / 1048576.0,
        untrimmed as f64 / 1048576.0
    );
    assert!(
        short_held < untrimmed / 4,
        "a full scrollback of 5-character lines holds {short_held} bytes; \
         untrimmed rows would be {untrimmed}"
    );
    // ...and the history is still there, unchanged, at the far end of it.
    assert!(short_top.starts_with("hello"), "the oldest banked line reads back: {short_top:?}");

    // The control: a line that really does fill the width still costs what
    // it costs. This is what says the saving comes from *padding* rather
    // than from content being dropped somewhere.
    let wide = "W".repeat(usize::from(COLS));
    let (wide_held, wide_top) = held_bytes(&wide);
    eprintln!(
        "full-width lines: {:.1} MB held, untrimmed would be {:.1} MB",
        wide_held as f64 / 1048576.0,
        untrimmed as f64 / 1048576.0
    );
    assert!(
        wide_held > untrimmed / 2,
        "a full scrollback of full-width lines should still cost about a full \
         grid ({untrimmed} bytes); it held {wide_held}"
    );
    assert_eq!(wide_top.trim_end(), wide, "a full-width banked line reads back whole");
}
