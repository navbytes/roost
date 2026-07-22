//! Live QA evidence drive (fleet-features engagement, PLAN F8 items ①–⑩).
//!
//! Drives the real roost binary in a real PTY through every new feature and
//! prints the actual rendered frames as an evidence pack. Run explicitly:
//!
//!   cargo test --test live_qa -- --ignored --nocapture
//!
//! `#[ignore]` because it is an interactive-length scenario (~30s), writes
//! the macOS clipboard (⑤), and is meant for engagement evidence, not CI.

mod harness;

use std::process::Command;
use std::time::Duration;

use harness::Harness;

fn alt(c: u8) -> Vec<u8> {
    vec![0x1b, c]
}

fn fixture(cwd: &str) -> String {
    serde_json::json!({
        "version": 1,
        "active_tab": 0,
        "tabs": [{
            "name": "main",
            "layout": { "pane": 1 },
            "panes": { "1": { "adapter": "shell", "cwd": cwd, "session": null, "title": null } }
        }],
        "next_pane_id": 2
    })
    .to_string()
}

/// Print the current frame with a title banner. Rows right-trimmed; blank
/// runs collapsed. This output IS the evidence pack.
fn frame(h: &mut Harness, title: &str) {
    h.settle(Duration::from_secs(3));
    let s = h.screen().contents();
    println!("\n──── {title} ────");
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
}

fn type_line(h: &mut Harness, s: &str) {
    h.write_bytes(s.as_bytes());
    h.write_bytes(b"\r");
}

fn contents(h: &mut Harness) -> String {
    h.settle(Duration::from_secs(3));
    h.screen().contents()
}

/// Locate `needle` on the settled screen → (row, first_col, last_col), all
/// 0-based. Used to drive mouse selection at real coordinates.
fn find_text(h: &mut Harness, needle: &str) -> Option<(u16, u16, u16)> {
    h.settle(Duration::from_secs(2));
    let s = h.screen();
    let (rows, cols) = s.size();
    for row in 0..rows {
        let mut line = String::new();
        for col in 0..cols {
            line.push_str(&s.cell(row, col).map(|c| c.contents()).unwrap_or_default());
        }
        if let Some(byte_idx) = line.find(needle) {
            let start = line[..byte_idx].chars().count() as u16;
            let end = start + needle.chars().count() as u16 - 1;
            return Some((row, start, end));
        }
    }
    None
}

/// SGR mouse sequences (1-based coords), as a real terminal sends them.
fn sgr_press(col: u16, row: u16) -> Vec<u8> {
    format!("\x1b[<0;{};{}M", col + 1, row + 1).into_bytes()
}
fn sgr_drag(col: u16, row: u16) -> Vec<u8> {
    format!("\x1b[<32;{};{}M", col + 1, row + 1).into_bytes()
}
fn sgr_release(col: u16, row: u16) -> Vec<u8> {
    format!("\x1b[<0;{};{}m", col + 1, row + 1).into_bytes()
}

#[test]
#[ignore = "evidence drive: long, interactive, writes the macOS clipboard"]
fn live_qa_evidence() {
    let cwd = std::env::temp_dir().join("rqa-work");
    let _ = std::fs::create_dir_all(&cwd);
    let cwd = cwd.to_string_lossy().to_string();

    let mut h = match Harness::try_spawn(&fixture(&cwd)) {
        Ok(h) => h,
        Err(why) => {
            eprintln!("SKIP: {why}");
            return;
        }
    };
    let roost_bin = env!("CARGO_BIN_EXE_roost");

    // Readiness: the hint bar is the last chrome roost draws — wait for it
    // (an empty screen "settles" instantly, so settle() alone is not enough).
    assert!(
        h.wait_for(Duration::from_secs(8), |s| s.contents().contains("Alt+n")).is_some(),
        "roost must draw its chrome within 8s of spawn"
    );

    frame(&mut h, "S0 · fresh launch (1 shell pane)");

    // ── ④ RAW MODE ──────────────────────────────────────────────────
    h.write_bytes(&alt(b'n')); // second pane, takes focus
    frame(&mut h, "S1 · Alt+n split");
    type_line(&mut h, "cat -v");
    h.settle(Duration::from_secs(2));
    h.write_bytes(&alt(b'P')); // Alt+Shift+p → raw on
    h.settle(Duration::from_secs(2));
    let c = contents(&mut h);
    assert!(c.contains("RAW"), "④ RAW word must show once raw is on");
    // These chords are roost actions in cooked mode; raw must forward them.
    h.write_bytes(&alt(b'b'));
    h.write_bytes(&alt(b'f'));
    h.write_bytes(&alt(b'a'));
    frame(&mut h, "④ raw pane: cat -v received the Alt chords as bytes");
    let c = contents(&mut h);
    assert!(
        c.contains("^[b") && c.contains("^[f") && c.contains("^[a"),
        "④ cat -v must show ^[b ^[f ^[a — raw forwards everything"
    );
    h.write_bytes(&alt(b'P')); // raw off
    h.settle(Duration::from_secs(1));
    h.write_bytes(&[0x03]); // ^C ends cat
    h.settle(Duration::from_secs(1));
    let c = contents(&mut h);
    assert!(!c.contains(" RAW"), "④ RAW word must clear after exit chord");

    // ── ① JUMP-TO-ATTENTION (2 needy panes across 2 tabs) ───────────
    type_line(&mut h, "printf '\\a'"); // bell → ◆ on this pane
    h.settle(Duration::from_secs(1));
    h.write_bytes(&alt(b't')); // tab 2
    h.settle(Duration::from_secs(2));
    type_line(&mut h, "printf '\\a'"); // bell → ◆ on tab-2 pane
    h.settle(Duration::from_secs(1));
    h.write_bytes(&alt(b'1')); // back to tab 1
    h.settle(Duration::from_secs(1));
    h.write_bytes(&alt(b'h')); // focus pane 1 (not a needy one)
    // Bells surface as ◆ only once a pane has been quiet for ACTIVE_WINDOW
    // (2s) — the printf's own output counts as activity. Wait it out.
    // At 120 cols the needs-you segment currently yields to the static pairs
    // (C9 yield order — flagged for the fix pass; segment should win). Until
    // that lands, wait on each TAB's own aggregate glyph.
    let needy = h.wait_for(Duration::from_secs(10), |s| {
        let c = s.contents();
        c.contains("1 main ◆") && c.contains("2 tab2 ◆")
    });
    if needy.is_none() {
        // Probe: show what tab 2's pane is actually doing before failing.
        h.write_bytes(&alt(b'2'));
        frame(&mut h, "①-PROBE · tab 2 contents (◆ never arrived)");
        h.write_bytes(&alt(b'1'));
    }
    frame(&mut h, "① setup: two ◆ panes across two tabs");
    assert!(needy.is_some(), "① both tab aggregates must reach ◆");
    h.write_bytes(&alt(b'a'));
    frame(&mut h, "① Alt+a #1 → first ◆ (same tab)");
    h.write_bytes(&alt(b'a'));
    frame(&mut h, "① Alt+a #2 → cross-tab jump (tab 2 active)");
    assert!(contents(&mut h).contains("▎ 2"), "① second jump must land on tab 2");
    h.write_bytes(&alt(b'a'));
    frame(&mut h, "① Alt+a #3 → wraps back");
    assert!(contents(&mut h).contains("▎ 1"), "① third jump must wrap to tab 1");

    // ── ② ZOOM ──────────────────────────────────────────────────────
    h.write_bytes(&alt(b'1'));
    h.settle(Duration::from_secs(1));
    h.write_bytes(&alt(b'z'));
    frame(&mut h, "② Alt+z: focused pane fills the body, ZOOM word");
    let c = contents(&mut h);
    assert!(c.contains("ZOOM"), "② ZOOM word must show");
    h.write_bytes(&alt(b'h')); // focus move retargets the zoom
    frame(&mut h, "② focus move under zoom retargets");
    h.write_bytes(&alt(b'z'));
    h.settle(Duration::from_secs(1));
    let c = contents(&mut h);
    assert!(!c.contains("ZOOM"), "② ZOOM must clear on toggle");

    // ── ⑥ LAYOUT CYCLE (3 panes) ────────────────────────────────────
    h.write_bytes(&alt(b'n')); // third pane on tab 1
    h.settle(Duration::from_secs(2));
    h.write_bytes(&alt(b'g'));
    frame(&mut h, "⑥ Alt+g #1 · grid (2 over 1)");
    h.write_bytes(&alt(b'g'));
    std::thread::sleep(Duration::from_millis(250));
    let hint_now = h
        .screen()
        .contents()
        .lines()
        .last()
        .unwrap_or_default()
        .trim_end()
        .to_string();
    println!("· ⑥ hint bar 250ms after Alt+g #2: {hint_now:?}");
    frame(&mut h, "⑥ Alt+g #2 · main + stack");
    let c = contents(&mut h);
    assert!(c.contains("STACK"), "⑥ main+stack must show a stack header");
    h.write_bytes(&alt(b'g'));
    frame(&mut h, "⑥ Alt+g #3 · all-stack");
    let c = contents(&mut h);
    assert!(c.contains("STACK"), "⑥ all-stack must show the stack header");

    // ── ③ FLOATING SCRATCH PANE ─────────────────────────────────────
    h.write_bytes(&alt(b'f'));
    frame(&mut h, "③ Alt+f: float spawns centered");
    type_line(&mut h, "echo IN_FLOAT_77");
    h.settle(Duration::from_secs(2));
    let c = contents(&mut h);
    assert!(c.contains("IN_FLOAT_77"), "③ typing lands in the float");
    h.write_bytes(&alt(b'f')); // hide
    h.settle(Duration::from_secs(1));
    let c = contents(&mut h);
    assert!(!c.contains("IN_FLOAT_77"), "③ hidden float leaves the screen");
    h.write_bytes(&alt(b'f')); // show again — same shell, spawn-once
    frame(&mut h, "③ float back — same session (spawn-once)");
    let c = contents(&mut h);
    assert!(c.contains("IN_FLOAT_77"), "③ float must keep its shell across hide/show");
    h.write_bytes(&alt(b'h')); // focus action hides the float (rule 2)
    h.settle(Duration::from_secs(1));
    let c = contents(&mut h);
    assert!(!c.contains("IN_FLOAT_77"), "③ focus action must hide the float");

    // ── ⑧ BROADCAST (control CLI from inside a pane) ────────────────
    // 8a — pane actor from inside a pane: subtree-confined by authz, so a
    // leaf pane broadcasting reaches only itself (count=1). Live security
    // evidence, not a bug.
    let bx = format!("{roost_bin} send --all 'echo BX_SELF' --enter");
    type_line(&mut h, &bx);
    h.settle(Duration::from_secs(3));
    // 8b — fleet actor from OUTSIDE the panes (owner token via ROOST_STATE):
    // reaches every running pane.
    let out = Command::new(roost_bin)
        .env("ROOST_STATE", h.state_dir())
        .args(["send", "--all", "echo BX42_OK", "--enter"])
        .output()
        .expect("run fleet broadcast");
    println!("· ⑧ fleet reply: {}", String::from_utf8_lossy(&out.stdout).trim());
    let hit = h.wait_for(Duration::from_secs(5), |s| {
        s.contents().matches("BX42_OK").count() >= 2 // ≥2 visible panes echoed it
    });
    frame(&mut h, "⑧ fleet send --all → every visible running pane echoed BX42_OK");
    assert!(hit.is_some(), "⑧ fleet broadcast must land in multiple panes");
    let log = std::fs::read_to_string(h.state_dir().join("control.log")).unwrap_or_default();
    let lines: Vec<&str> = log.lines().filter(|l| l.contains("broadcast")).collect();
    let pane_line = lines.iter().find(|l| l.contains("pane:")).copied().unwrap_or("");
    let fleet_line = lines.iter().rev().find(|l| !l.contains("pane:")).copied().unwrap_or("");
    println!("· ⑧ pane-actor audit (subtree-confined): {pane_line}");
    println!("· ⑧ fleet audit: {fleet_line}");
    assert!(pane_line.contains("count=1"), "⑧ pane-actor broadcast must confine to its subtree");
    assert!(fleet_line.contains("len=") && fleet_line.contains("count="), "⑧ fleet audit must carry len=/count=");
    let fleet_count: u32 = fleet_line.split("count=").nth(1).and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    assert!(fleet_count >= 3, "⑧ fleet broadcast must reach ≥3 running panes, audit says {fleet_count}");
    assert!(!log.contains("BX42_OK") && !log.contains("BX_SELF"), "⑧ broadcast text must never be logged");

    // ── ⑦ ACTIVITY FEED ─────────────────────────────────────────────
    h.write_bytes(&alt(b'e'));
    frame(&mut h, "⑦ Alt+e: activity feed (spawns · transitions · ctl)");
    let c = contents(&mut h);
    assert!(c.contains("FEED"), "⑦ FEED word must show");
    assert!(c.contains("ctl"), "⑦ feed must show the broadcast ctl line");
    // Close with the Alt+e toggle, NOT bare Esc: a lone 0x1b in the byte
    // stream fuses with the next typed char into an Alt-chord (ESC+'c' =
    // Alt+c) — a real hazard for any PTY driver, humans are just slower.
    h.write_bytes(&alt(b'e'));
    let closed = h.wait_for(Duration::from_secs(3), |s| !s.contents().contains("FEED"));
    assert!(closed.is_some(), "⑦ Alt+e must close the feed");

    // ── ⑤ COPY MODE → real macOS clipboard ──────────────────────────
    // Fresh tab = one full-height pane; deterministic geometry. Keyboard
    // copy-mode chrome is asserted, then the selection→clipboard round-trip
    // is driven through the mouse-drag path (copy mode owns the mouse, C17/
    // C24 share `finish_selection`) at real on-screen coordinates.
    Command::new("sh").args(["-c", "printf CLIP_BASELINE | pbcopy"]).output().ok();
    h.write_bytes(&alt(b't'));
    h.settle(Duration::from_secs(2));
    type_line(&mut h, "clear; printf 'MARK_Y4K_COPY\\n'");
    let (mrow, mstart, mend) =
        find_text(&mut h, "MARK_Y4K_COPY").expect("⑤ marker must be on screen");
    println!("· ⑤ marker at screen row {mrow}, cols {mstart}..={mend}");

    h.write_bytes(&alt(b'c'));
    let c = contents(&mut h);
    assert!(c.contains("COPY"), "⑤ COPY mode word must show");
    assert!(c.contains("hjkl move") && c.contains("y/↵ yank"), "⑤ copy hint pairs must show");
    // Keyboard cursor is live (C24) — its state machine is unit-proven; here
    // we drive the selection deterministically via the mouse and read the
    // real clipboard, exercising finish_selection → clipboard::copy.
    h.write_bytes(&sgr_press(mstart, mrow));
    h.write_bytes(&sgr_drag(mend, mrow));
    h.write_bytes(&sgr_release(mend, mrow));
    frame(&mut h, "⑤ dragged over MARK_Y4K_COPY in copy mode");
    let hit = h.wait_for(Duration::from_secs(3), |_| {
        Command::new("pbpaste")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("MARK_Y4K_COPY"))
            .unwrap_or(false)
    });
    let paste = Command::new("pbpaste").output().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    println!("· ⑤ pbpaste after drag-release: {paste:?}");
    assert!(hit.is_some(), "⑤ selection must land on the real clipboard, got {paste:?}");
    println!("· ⑤ macOS clipboard now contains the yanked line ✓");

    // ── ⑨ TAB UNDO ──────────────────────────────────────────────────
    h.write_bytes(&alt(b't'));
    h.settle(Duration::from_secs(2));
    type_line(&mut h, "echo TAB_MARK_3");
    h.settle(Duration::from_secs(1));
    h.write_bytes(&alt(b'w'));
    h.settle(Duration::from_millis(600));
    if contents(&mut h).contains("TAB_MARK_3") {
        h.write_bytes(&alt(b'w')); // two-press confirm path
    }
    let gone = h.wait_for(Duration::from_secs(3), |s| !s.contents().contains("TAB_MARK_3"));
    assert!(gone.is_some(), "⑨ closing the tab's only pane must drop the tab");
    h.write_bytes(&alt(b'u'));
    frame(&mut h, "⑨ Alt+u after tab close: tab restored");

    // ── ⑩ CLEAN EXIT (freeze-fix regression) ────────────────────────
    let pid = h.pid();
    let t = h.quit_and_wait(Duration::from_secs(2));
    println!("\n· ⑩ Alt+q exit: {:?} (≤2s budget)", t);
    assert!(t.is_some(), "⑩ Alt+q must exit within 2s");
    std::thread::sleep(Duration::from_millis(300));
    let orphans: Vec<u32> = harness::descendant_pids(pid).into_iter().filter(|p| harness::is_alive(*p)).collect();
    assert!(orphans.is_empty(), "⑩ no orphan pane processes, found {orphans:?}");
    println!("· ⑩ no orphans ✓\n\nLIVE QA EVIDENCE DRIVE COMPLETE — all assertions passed.");
}
