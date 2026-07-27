//! UX/navigation evidence drive (SPEC-ux.md Appendix A): a scripted live
//! session against the real roost binary in a real PTY — multiple panes,
//! stacks, layout cycling, zoom, tabs, every mode, mouse events — probing
//! each gap catalogued in SPEC-ux.md (U1..U25, N1..N4). Prints [PASS]/
//! [BUG]/[OBS] lines plus evidence frames and never panics mid-run, so the
//! full session always completes; the summary counts desired-behavior
//! checks that currently fail. As SPEC-ux items get fixed, their [BUG]
//! lines flip to [PASS] here. Run explicitly:
//!   cargo test --test ux_nav_qa -- --ignored --nocapture

#[allow(dead_code)]
mod harness;

use std::process::Command;
use std::time::Duration;

use harness::Harness;

fn fixture(cwd: &str) -> String {
    serde_json::json!({
        "version": 1,
        "active_tab": 0,
        "tabs": [{
            "name": "main",
            "layout": { "pane": 1 },
            "panes": { "1": {"adapter": "shell", "cwd": cwd} }
        }],
        "next_pane_id": 2
    })
    .to_string()
}

fn alt(c: u8) -> Vec<u8> {
    vec![0x1b, c]
}
// xterm modified arrows: A=Up B=Down C=Right D=Left; mods 3=Alt, 4=Alt+Shift.
fn alt_arrow(ch: u8) -> Vec<u8> {
    format!("\x1b[1;3{}", ch as char).into_bytes()
}
fn alt_shift_arrow(ch: u8) -> Vec<u8> {
    format!("\x1b[1;4{}", ch as char).into_bytes()
}
fn sgr_press(col: u16, row: u16) -> Vec<u8> {
    format!("\x1b[<0;{};{}M", col + 1, row + 1).into_bytes()
}
fn sgr_release(col: u16, row: u16) -> Vec<u8> {
    format!("\x1b[<0;{};{}m", col + 1, row + 1).into_bytes()
}
fn sgr_wheel_up(col: u16, row: u16) -> Vec<u8> {
    format!("\x1b[<64;{};{}M", col + 1, row + 1).into_bytes()
}

/// Column every wheel probe below aims at: the right half of the 120-col
/// harness, i.e. the FOCUSED pane of the two-way split the scroll workload
/// (`seq 1 300`) runs in. It must track the focused pane, not a fixed half:
/// the probes used to sit at column 30 and only found history there because
/// U8's click-during-rename bug had moved focus to the left pane for them.
const WHEEL_COL: u16 = 90;

/// Control-plane ground truth: `roost list` against this instance.
fn cli_list(state_dir: &std::path::Path) -> serde_json::Value {
    let out = Command::new(env!("CARGO_BIN_EXE_roost"))
        .args(["list"])
        .env("ROOST_STATE", state_dir)
        .env_remove("ROOST_SOCK")
        .env_remove("ROOST_TOKEN")
        .env_remove("ROOST_CONTROL_TOKEN")
        .output()
        .expect("run roost list");
    serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null)
}

fn focused_pane(state_dir: &std::path::Path) -> u64 {
    cli_list(state_dir)
        .as_array()
        .and_then(|a| a.iter().find(|p| p["focused"] == true))
        .and_then(|p| p["pane"].as_u64())
        .unwrap_or(0)
}

fn title_of(state_dir: &std::path::Path, pane: u64) -> String {
    cli_list(state_dir)
        .as_array()
        .and_then(|a| a.iter().find(|p| p["pane"] == pane))
        .and_then(|p| p["title"].as_str().map(String::from))
        .unwrap_or_default()
}

/// Which tab the focused pane lives in (U7's reach probes).
fn focused_tab(state_dir: &std::path::Path) -> u64 {
    cli_list(state_dir)
        .as_array()
        .and_then(|a| a.iter().find(|p| p["focused"] == true))
        .and_then(|p| p["tab"].as_u64())
        .unwrap_or(0)
}

fn pane_count(state_dir: &std::path::Path) -> usize {
    cli_list(state_dir).as_array().map(|a| a.len()).unwrap_or(0)
}

fn frame(h: &mut Harness, label: &str) -> String {
    h.settle(Duration::from_secs(2));
    let s = h.screen().contents();
    println!("──── {label} ────");
    let mut blanks = 0;
    for row in s.lines() {
        let r = row.trim_end();
        if r.is_empty() {
            blanks += 1;
            if blanks == 1 {
                println!("│");
            }
            continue;
        }
        blanks = 0;
        println!("│{r}");
    }
    s
}

struct Qa {
    bugs: Vec<String>,
}
impl Qa {
    fn check(&mut self, ok: bool, what: &str) {
        if ok {
            println!("[PASS] {what}");
        } else {
            println!("[BUG]  {what}");
            self.bugs.push(what.to_string());
        }
    }
    fn obs(&self, what: &str) {
        println!("[OBS]  {what}");
    }
}

#[test]
#[ignore = "evidence drive: long, interactive; run with --ignored --nocapture"]
fn ux_navigation_session() {
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("utf8");
    let mut h = match Harness::try_spawn(&fixture(cwd)) {
        Ok(h) => h,
        Err(reason) => {
            eprintln!("SKIP ux nav QA: {reason}");
            return;
        }
    };
    let sd = h.state_dir().to_path_buf();
    let mut qa = Qa { bugs: Vec::new() };
    // Readiness gate: wait for the first REAL frame (hint bar drawn) before
    // sending any input. Since the P4 client-side fix this frame arrives in
    // ~250 ms — long before crossterm's keyboard-enhancement probe (which
    // this harness never answers) releases the shared event reader at its
    // internal ~2 s give-up, so a frame alone no longer proves keys flow.
    h.wait_for(Duration::from_secs(15), |s| s.contents().contains("NORMAL"))
        .expect("roost never drew its first frame");
    // ...and until the control socket answers, so cli probes are trustworthy.
    let cli_deadline = std::time::Instant::now() + Duration::from_secs(10);
    while cli_list(&sd).as_array().is_none() {
        assert!(std::time::Instant::now() < cli_deadline, "control CLI never came up");
        std::thread::sleep(Duration::from_millis(100));
    }
    // ...and until pane input actually round-trips — chords sent while the
    // probe still owns the reader would be delivered late, in one burst,
    // and desync the whole script.
    h.write_bytes(b"echo dr''ive-up\r");
    h.wait_for(Duration::from_secs(10), |s| s.contents().contains("drive-up"))
        .expect("pane input never went live");
    let settle = |h: &mut Harness| {
        h.settle(Duration::from_secs(2));
    };

    // ---- A. panes & spatial focus -------------------------------------
    for _ in 0..3 {
        h.write_bytes(&alt(b'n'));
        settle(&mut h);
    }
    qa.check(pane_count(&sd) == 4, "Alt+n x3 -> 4 panes exist");
    frame(&mut h, "A1: four panes");
    let mut path = vec![focused_pane(&sd)];
    for mv in [b'D', b'A', b'C', b'B'] {
        h.write_bytes(&alt_arrow(mv));
        settle(&mut h);
        path.push(focused_pane(&sd));
    }
    qa.obs(&format!("focus path via Alt+Left/Up/Right/Down: {path:?}"));
    let before = focused_pane(&sd);
    h.write_bytes(&alt(b'h'));
    settle(&mut h);
    let after_h = focused_pane(&sd);
    h.write_bytes(&alt(b'l'));
    settle(&mut h);
    qa.obs(&format!("Alt+h from {before} -> {after_h}; Alt+l back -> {}", focused_pane(&sd)));
    // boundary: walk left twice from leftmost — does focus wrap or stay?
    h.write_bytes(&alt(b'h'));
    settle(&mut h);
    h.write_bytes(&alt(b'h'));
    settle(&mut h);
    let leftmost = focused_pane(&sd);
    h.write_bytes(&alt(b'h'));
    settle(&mut h);
    qa.obs(&format!(
        "focus at left boundary: Alt+h at leftmost {leftmost} -> {} (wrap or stay?)",
        focused_pane(&sd)
    ));

    // resize: ratios must change in workspace.json
    let ws_path = sd.join("workspace.json");
    let ratios_before = std::fs::read_to_string(&ws_path).unwrap_or_default();
    h.write_bytes(&alt_shift_arrow(b'C'));
    h.write_bytes(&alt_shift_arrow(b'C'));
    settle(&mut h);
    let ratios_after = std::fs::read_to_string(&ws_path).unwrap_or_default();
    qa.check(ratios_before != ratios_after, "Alt+Shift+Right x2 changes persisted layout ratios");

    // ---- B. stack, layout cycle, zoom, flip ---------------------------
    h.write_bytes(&alt(b's'));
    settle(&mut h);
    let ws = std::fs::read_to_string(&ws_path).unwrap_or_default();
    qa.obs(&format!("after Alt+s, layout contains 'stack': {}", ws.contains("stack")));
    frame(&mut h, "B1: after Alt+s (stack toggle)");
    let f0 = focused_pane(&sd);
    h.write_bytes(&alt_arrow(b'B'));
    settle(&mut h);
    let f1 = focused_pane(&sd);
    h.write_bytes(&alt_arrow(b'A'));
    settle(&mut h);
    qa.obs(&format!("stack member cycle: {f0} -Alt+Down-> {f1} -Alt+Up-> {}", focused_pane(&sd)));
    for i in 0..3 {
        h.write_bytes(&alt(b'g'));
        settle(&mut h);
        let ws = std::fs::read_to_string(&ws_path).unwrap_or_default();
        let kind = if ws.contains("stack") { "has-stack" } else { "splits-only" };
        qa.obs(&format!("Alt+g cycle #{i}: layout {kind}"));
    }
    frame(&mut h, "B2: after layout cycling");
    // zoom
    h.write_bytes(&alt(b'z'));
    let s = frame(&mut h, "B3: zoomed");
    qa.check(s.contains("ZOOM"), "zoom shows ZOOM mode word on hint bar");
    let zf = focused_pane(&sd);
    h.write_bytes(&alt_arrow(b'C'));
    settle(&mut h);
    let s = h.screen().contents();
    qa.obs(&format!(
        "Alt+Right while zoomed: focus {zf} -> {}, still shows ZOOM: {}",
        focused_pane(&sd),
        s.contains("ZOOM")
    ));
    h.write_bytes(&alt(b'z'));
    settle(&mut h);
    // flip split orientation
    let before = std::fs::read_to_string(&ws_path).unwrap_or_default();
    h.write_bytes(&alt(b'o'));
    settle(&mut h);
    let after = std::fs::read_to_string(&ws_path).unwrap_or_default();
    qa.obs(&format!("Alt+o flip changed persisted layout: {}", before != after));

    // ---- C. tabs: memory, same-tab digit, zoom interaction ------------
    let tabs_before = cli_list(&sd)
        .as_array()
        .map(|a| {
            let mut t: Vec<u64> = a.iter().filter_map(|p| p["tab"].as_u64()).collect();
            t.sort_unstable();
            t.dedup();
            t.len()
        })
        .unwrap_or(0);
    h.write_bytes(&alt(b't'));
    settle(&mut h);
    let tabs_after = cli_list(&sd)
        .as_array()
        .map(|a| {
            let mut t: Vec<u64> = a.iter().filter_map(|p| p["tab"].as_u64()).collect();
            t.sort_unstable();
            t.dedup();
            t.len()
        })
        .unwrap_or(0);
    qa.check(
        tabs_after == tabs_before + 1,
        &format!("Alt+t creates a second tab ({tabs_before} -> {tabs_after})"),
    );
    h.write_bytes(&alt(b'1'));
    settle(&mut h);
    // pick a non-first pane in tab 1, remember it, round-trip to tab 2
    h.write_bytes(&alt_arrow(b'C'));
    settle(&mut h);
    let remembered = focused_pane(&sd);
    h.write_bytes(&alt(b'2'));
    settle(&mut h);
    h.write_bytes(&alt(b'1'));
    settle(&mut h);
    let back = focused_pane(&sd);
    qa.check(
        back == remembered,
        &format!("tab focus memory: left tab1 focused on {remembered}, returned to {back}"),
    );
    // same-tab digit: does Alt+1 while ON tab 1 reset focus?
    h.write_bytes(&alt_arrow(b'C'));
    settle(&mut h);
    let g = focused_pane(&sd);
    h.write_bytes(&alt(b'1'));
    settle(&mut h);
    let after_digit = focused_pane(&sd);
    qa.check(
        after_digit == g,
        &format!("same-tab Alt+1 keeps focus (was {g}, now {after_digit})"),
    );
    // zoom killed by same-tab digit?
    h.write_bytes(&alt(b'z'));
    settle(&mut h);
    let zoomed_before = h.screen().contents().contains("ZOOM");
    h.write_bytes(&alt(b'1'));
    settle(&mut h);
    let zoomed_after = h.screen().contents().contains("ZOOM");
    qa.check(
        !zoomed_before || zoomed_after,
        &format!("same-tab Alt+1 preserves zoom (before {zoomed_before}, after {zoomed_after})"),
    );
    if h.screen().contents().contains("ZOOM") {
        h.write_bytes(&alt(b'z'));
        settle(&mut h);
    }
    // U7: tab reach. Alt+9 still names a tab that doesn't exist (silent
    // no-op); Alt+0 is the last tab whatever its number, and Alt+m/Alt+i
    // step with wrap — the routes to tabs the digit row can't name.
    h.write_bytes(&alt(b'1'));
    settle(&mut h);
    let first = focused_tab(&sd);
    h.write_bytes(&alt(b'9'));
    settle(&mut h);
    qa.check(
        focused_tab(&sd) == first,
        &format!("Alt+9 with no ninth tab stays put (tab {first}) (U7)"),
    );
    h.write_bytes(&alt(b'0'));
    settle(&mut h);
    let last = focused_tab(&sd);
    qa.check(last > first, &format!("Alt+0 jumps to the last tab ({first} -> {last}) (U7)"));
    h.write_bytes(&alt(b'm'));
    settle(&mut h);
    let wrapped = focused_tab(&sd);
    qa.check(
        wrapped == first,
        &format!("Alt+m past the last tab wraps to the first ({last} -> {wrapped}) (U7)"),
    );
    h.write_bytes(&alt(b'i'));
    settle(&mut h);
    let back = focused_tab(&sd);
    qa.check(
        back == last,
        &format!("Alt+i past the first tab wraps to the last ({wrapped} -> {back}) (U7)"),
    );

    // ---- D. rename, picker, modal ownership ---------------------------
    // rename swallows modifiers: Ctrl+W / Ctrl+U insert literal w / u
    h.write_bytes(&alt(b'2'));
    settle(&mut h);
    let target = focused_pane(&sd);
    h.write_bytes(&alt(b'r'));
    settle(&mut h);
    h.write_bytes(b"abc");
    h.write_bytes(&[0x17]); // Ctrl+W
    h.write_bytes(&[0x15]); // Ctrl+U
    h.write_bytes(b"\r");
    settle(&mut h);
    let t = title_of(&sd, target);
    qa.check(
        t == "abc",
        &format!("rename honors Ctrl+W/Ctrl+U as edits (title became {t:?}, wanted \"abc\")"),
    );
    // paste while rename open: where does it land?
    h.write_bytes(&alt(b'r'));
    settle(&mut h);
    h.write_bytes(b"\x1b[200~PSTX\x1b[201~");
    settle(&mut h);
    h.write_bytes(&[0x1b]); // Esc: close rename
    settle(&mut h);
    let s = frame(&mut h, "D1: after paste-during-rename (pane below shows PSTX?)");
    let title_now = title_of(&sd, target);
    qa.check(
        title_now.contains("PSTX") || !s.contains("PSTX"),
        &format!("paste during rename goes to the dialog, not the hidden pane (title {title_now:?}, pane shows PSTX: {})", s.contains("PSTX")),
    );
    h.write_bytes(&[0x15]); // clear the shell line under
    settle(&mut h);

    // modal mouse ownership: rename pane A, click pane B, Enter
    h.write_bytes(&alt(b't'));
    settle(&mut h);
    h.write_bytes(&alt(b'n'));
    settle(&mut h); // tab 3: two panes side by side
    let right = focused_pane(&sd);
    h.write_bytes(&alt(b'r'));
    settle(&mut h);
    h.write_bytes(b"ZZZ");
    h.write_bytes(&sgr_press(10, 5));
    h.write_bytes(&sgr_release(10, 5)); // click LEFT pane while dialog open
    settle(&mut h);
    h.write_bytes(b"\r");
    settle(&mut h);
    let left = focused_pane(&sd) == right;
    let right_title = title_of(&sd, right);
    qa.check(
        right_title == "ZZZ",
        &format!(
            "rename commits to the pane it opened on (pane {right} title {right_title:?}; click-during-modal moved focus: {})",
            !left
        ),
    );

    // picker: open, navigate, escape
    h.write_bytes(&alt(b'\r'));
    let s = frame(&mut h, "D2: quick-launch picker");
    qa.obs(&format!("picker visible: {}", s.contains("pi") && s.contains("shell")));
    h.write_bytes(b"j");
    h.write_bytes(b"k");
    h.write_bytes(&[0x1b]);
    settle(&mut h);

    // ---- E. scroll & copy ---------------------------------------------
    h.write_bytes(b"seq 1 300\r");
    h.settle(Duration::from_secs(3));
    h.write_bytes(b"\x1b[5;3~"); // Alt+PageUp -> scroll mode
    settle(&mut h);
    let s = h.screen().contents();
    qa.check(s.contains("SCROLL"), "Alt+PageUp enters SCROLL (mode word shown)");
    for _ in 0..5 {
        h.write_bytes(b"\x1b[A");
    }
    settle(&mut h);
    let scrolled5 = h.screen().contents();
    // overshoot: bank a huge offset past the top, then one Down
    for _ in 0..25 {
        h.write_bytes(b"\x1b[5~"); // PageUp inside scroll mode
    }
    settle(&mut h);
    let at_top = h.screen().contents();
    h.write_bytes(b"\x1b[B"); // one Down
    settle(&mut h);
    let after_one_down = h.screen().contents();
    qa.check(
        after_one_down != at_top,
        "scroll overshoot: one Down after paging past the top moves the view immediately",
    );
    if after_one_down == at_top {
        let mut burns = 1;
        loop {
            h.write_bytes(b"\x1b[B");
            burns += 1;
            if burns % 20 == 0 {
                settle(&mut h);
                if h.screen().contents() != at_top || burns > 900 {
                    break;
                }
            }
        }
        qa.obs(&format!("burned ~{burns} Down presses before the view moved (phantom offset)"));
    }
    h.write_bytes(b"q");
    settle(&mut h);
    let _ = scrolled5;

    // wheel/key desync: wheel up, then enter scroll mode (offset starts 0)
    for _ in 0..8 {
        h.write_bytes(&sgr_wheel_up(WHEEL_COL, 10));
    }
    settle(&mut h);
    let wheeled = h.screen().contents();
    h.write_bytes(b"\x1b[5;3~"); // scroll mode (U9: seeds from the wheeled offset)
    h.write_bytes(b"\x1b[A"); // Up -> one line further back, not a snap to 1
    settle(&mut h);
    let after_key = h.screen().contents();
    qa.check(
        after_key.contains("298") == wheeled.contains("298") || !after_key.contains("300"),
        "entering scroll mode after wheeling continues from the wheeled position (no snap toward tail)",
    );
    qa.obs(&format!(
        "wheel-then-key: wheeled view shows line 280: {}, after one scroll-mode Up shows 299/300: {}",
        wheeled.contains("280 "),
        after_key.contains("299")
    ));
    h.write_bytes(b"q");
    settle(&mut h);

    // wheel-scrolled pane: any indicator? (U3/N1 — search everything except
    // the hint bar, whose "Alt+←↓↑→ focus" pair would false-match '↑')
    for _ in 0..8 {
        h.write_bytes(&sgr_wheel_up(WHEEL_COL, 10));
    }
    let s = frame(&mut h, "E1: wheel-scrolled pane (any indicator?)");
    let body: String = {
        let lines: Vec<&str> = s.lines().collect();
        let n = lines.len().saturating_sub(1); // drop the hint bar row
        lines[..n].join("\n")
    };
    qa.check(
        body.contains("scrolled") || body.contains('↑') || body.contains('▼'),
        "a wheel-scrolled pane shows some scrollback indicator (U3)",
    );
    // scroll-mode -> copy-mode: offset preserved?
    h.write_bytes(b"\x1b[5;3~");
    for _ in 0..30 {
        h.write_bytes(b"\x1b[A");
    }
    settle(&mut h);
    let in_scroll = h.screen().contents();
    h.write_bytes(&alt(b'c'));
    settle(&mut h);
    let in_copy = h.screen().contents();
    let preserved = in_copy.lines().take(20).collect::<String>()
        == in_scroll.lines().take(20).collect::<String>();
    qa.check(preserved, "entering copy mode from scroll mode keeps the scrolled view (no snap to tail)");
    // copy keys: v + motions + y, then V/w probes
    h.write_bytes(b"v");
    h.write_bytes(b"lll");
    h.write_bytes(b"y");
    settle(&mut h);
    let s = h.screen().contents();
    qa.check(s.contains("copied"), "copy-mode v+motions+y flashes 'copied N chars'");
    h.write_bytes(&alt(b'c'));
    settle(&mut h);
    h.write_bytes(b"V");
    h.write_bytes(b"w");
    settle(&mut h);
    qa.obs("copy mode: V (line select) and w (word motion) are unbound no-ops");
    h.write_bytes(&[0x1b]);
    settle(&mut h);

    // ---- F. attention, feed, help, hints ------------------------------
    // bell in an unfocused pane -> heuristic needs-input -> Alt+a jumps
    let bell_pane = focused_pane(&sd);
    h.write_bytes(b"sh -c 'sleep 2; printf \"\\a\"'\r");
    h.write_bytes(&alt_arrow(b'D'));
    settle(&mut h); // focus away
    std::thread::sleep(Duration::from_secs(4));
    let s = frame(&mut h, "F1: after bell in unfocused pane");
    qa.check(s.contains('◆'), "bell in unfocused pane surfaces the needs-you diamond");
    qa.obs(&format!("hint bar shows needs-you segment: {}", s.contains("needs you")));
    h.write_bytes(&alt(b'a'));
    settle(&mut h);
    qa.check(
        focused_pane(&sd) == bell_pane,
        &format!("Alt+a jumps to the bell pane ({bell_pane})"),
    );
    // feed overlay + wheel-under test
    h.write_bytes(&alt(b'e'));
    let s = frame(&mut h, "F2: activity feed");
    // U2: feed entries must carry `{id} {display_name}` — a digit-led label
    // ahead of the name — instead of four indistinguishable bare "shell"
    // lines. Scoped to real feed rows (they contain a transition arrow or
    // "spawned"): the corner badges elsewhere on the frame also carry the
    // cwd tag and (post-U2) ids, and must not satisfy this check for the
    // feed.
    let ided = s.lines().any(|l| {
        (l.contains('→') || l.contains("spawned"))
            && (0..10).any(|d| l.contains(&format!(" {d} shell")))
    });
    qa.check(ided, "feed entries carry the pane id and disambiguated name (U2)");
    // U8(c): with the feed up, the wheel belongs to the FEED — the pane
    // under the overlay must stay at its live tail. Evidence after closing
    // the feed: no pane carries U3's `↑N` frozen-view badge token (before
    // the modal gate, two notches froze whatever pane sat under (30, 10)).
    h.write_bytes(&sgr_wheel_up(WHEEL_COL, 10)); // wheel over the pane area, feed open
    h.write_bytes(&sgr_wheel_up(WHEEL_COL, 10));
    settle(&mut h);
    h.write_bytes(&alt(b'e')); // close the feed
    let s = frame(&mut h, "F2b: after wheel-under-feed (panes still live?)");
    let body: String = {
        let lines: Vec<&str> = s.lines().collect();
        let n = lines.len().saturating_sub(1); // drop the hint bar's own '↑'
        lines[..n].join("\n")
    };
    qa.check(
        !body.contains('↑'),
        "wheel with the feed open scrolls the feed, not the pane under it (U8)",
    );
    // help overlay content
    h.write_bytes(&alt(b'?'));
    let s = frame(&mut h, "F3: help overlay");
    qa.check(
        s.contains('●') && s.contains('✕'),
        "help overlay includes a status-glyph legend",
    );
    qa.check(s.to_lowercase().contains("mouse") || s.contains("wheel"), "help overlay documents mouse behavior");
    h.write_bytes(b" ");
    settle(&mut h);
    // hints hidden + zoom: any mode indication left?
    h.write_bytes(&alt(b'/'));
    h.write_bytes(&alt(b'z'));
    let s = frame(&mut h, "F4: zoomed with hints hidden");
    qa.check(s.contains("ZOOM"), "zoom is still indicated somewhere with hints hidden");
    h.write_bytes(&alt(b'z'));
    h.write_bytes(&alt(b'/'));
    settle(&mut h);

    // ---- G. unbound alt, death, undo, quit ----------------------------
    h.write_bytes(b"cat\r");
    settle(&mut h);
    h.write_bytes(&alt(b'b'));
    settle(&mut h);
    let s = h.screen().contents();
    // U5 FIXED: an unbound Alt+printable belongs to the pane — cooked
    // routing must deliver Alt+b as meta-ESC (`^[b` under cat's tty echo).
    qa.check(s.contains("^[b"), "U5: cooked pane forwards unbound Alt+b as ^[b");
    // Raw-mode probe is an OBS, not a check: the uppercase-delivery form
    // ESC+'P' is byte-identical to the DCS introducer, so on a terminal
    // without kitty disambiguation (this harness included) the toggle is
    // inherently racy — SPEC-ux N3. Both outcomes are recorded. (Since U5,
    // Alt+b forwards in cooked and raw alike, so forwarding can no longer
    // discriminate — the badge's ` · raw ` token is the probe now: unlike
    // the hint-bar RAW mode word, a live flash can't hide it.)
    let raw_on = |h: &mut Harness| h.screen().contents().contains("· raw");
    h.write_bytes(&alt(b'P'));
    settle(&mut h);
    let raw_engaged = raw_on(&mut h);
    qa.obs(&format!("raw toggle via ESC+'P' (DCS-ambiguous, N3): engaged={raw_engaged}"));
    if raw_engaged {
        h.write_bytes(&alt(b'P'));
        settle(&mut h);
        if raw_on(&mut h) {
            // The exit chord is the same DCS-ambiguous byte pair — retry
            // once so a swallowed toggle can't leave the rest of the drive
            // (Alt+w close, Alt+q quit) forwarding into a raw pane.
            h.write_bytes(&alt(b'P'));
            settle(&mut h);
        }
    }
    h.write_bytes(&[0x03]); // Ctrl+C ends cat
    settle(&mut h);
    // dead pane: f respawns fresh with zero confirmation
    h.write_bytes(b"exit\r");
    settle(&mut h);
    let s = frame(&mut h, "G1: dead pane bar");
    qa.check(s.contains("exited"), "dead pane shows the relaunch bar");
    h.write_bytes(b"f");
    settle(&mut h);
    std::thread::sleep(Duration::from_millis(2600)); // let it go quiet
    qa.obs("dead-pane 'f' (fresh, drops resume) acted immediately with no confirmation");
    // close + undo
    let n = pane_count(&sd);
    h.write_bytes(&alt(b'w'));
    settle(&mut h);
    let closed = n > 0 && pane_count(&sd) == n - 1;
    if !closed {
        // busy-confirm armed (recent output): second press
        h.write_bytes(&alt(b'w'));
        settle(&mut h);
        qa.obs("Alt+w needed a second press (heuristic Working from recent shell output)");
    }
    qa.check(pane_count(&sd) == n - 1, "Alt+w closed the pane");
    h.write_bytes(&alt(b'u'));
    settle(&mut h);
    qa.check(pane_count(&sd) == n, "Alt+u restored the closed pane");

    // quit guard (U1): with a pane actively producing output, an Alt+q must
    // ARM a second-press confirm — roost stays up and the prompt is on the
    // bar — and the second press within the window quits. (Before the
    // guard, one press killed the fleet in ~318 ms.) Delivery hardening,
    // not behavior fudging: the chord's ESC+'q' form can straddle reads
    // under full-tilt firehose load and degrade to Esc + a literal q (the
    // same split N3 documents for ESC+'P'), so the arming press retries
    // until the prompt is visibly up — bailing out the moment roost dies,
    // which is exactly the U1 bug. The prompt poll sits well inside the
    // confirm window, which now equals the flash window (U22).
    h.write_bytes(&[0x15]); // clear any stray chars off the shell line
    h.write_bytes(b"seq 1 100000\r");
    std::thread::sleep(Duration::from_millis(300));
    let mut armed = false;
    for _ in 0..3 {
        h.write_bytes(&alt(b'q'));
        if h
            .wait_for(Duration::from_millis(1200), |s| s.contents().contains("again to quit"))
            .is_some()
        {
            armed = true;
            break;
        }
        if !harness::is_alive(h.pid()) {
            break; // quit on a lone press: the U1 regression itself
        }
    }
    qa.check(
        harness::is_alive(h.pid()),
        "Alt+q mid-firehose arms a confirm instead of quitting (U1)",
    );
    qa.check(armed, "the armed quit shows its second-press prompt on the bar (U1/U22)");
    let quit = h.quit_and_wait(Duration::from_secs(5));
    qa.check(
        quit.is_some(),
        &format!("second Alt+q within the window quits cleanly ({quit:?})"),
    );

    println!("\n==== QA SUMMARY: {} bug(s) ====", qa.bugs.len());
    for b in &qa.bugs {
        println!("  BUG: {b}");
    }
}
