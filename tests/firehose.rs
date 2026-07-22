//! DESIGN-ui.md §6 "Firehose input-latency gate": while one pane spews
//! sustained output, roost must keep (1) typed-echo latency in the *other*
//! (focused) pane bounded, (2) the firehose pane's own region visibly
//! flowing (no draw starvation), and (3) a clean, orphan-free exit under
//! load. This is the harness foundation's first tenant — no color/
//! golden-frame assertions here, those stay deferred per §6.

mod harness;

use std::time::{Duration, Instant};

use harness::Harness;

/// DESIGN-ui.md §6's own example filler (`printf "%0.sX" $(seq 200); echo`)
/// prints an IDENTICAL ~200-char line every iteration — a fine stress load,
/// but useless as a *change* signal: two 500ms samples of a steady-state
/// scroll of identical lines look the same even while the loop keeps
/// running. Appending a monotonic counter keeps it "deterministic filler,
/// ~200-char lines" while making genuine progress observable, which
/// assertion 2 below needs.
const SPEW_CMD: &str = r#"sh -c 'i=0; while :; do i=$((i+1)); printf "%0.sX" $(seq 1 200); printf " %s\n" "$i"; done'"#;

/// Two side-by-side shell panes (DESIGN-ui.md §6 scenario). Pane 1 (left) is
/// focused by default (`App::new` focuses the first pane in DFS order) and
/// becomes the firehose; pane 2 (right) stays a quiet interactive shell that
/// takes focus for the latency check.
fn fixture_workspace(cwd: &str) -> String {
    serde_json::json!({
        "version": 1,
        "active_tab": 0,
        "tabs": [{
            "name": "main",
            "layout": {
                "split": {
                    "dir": "vertical",
                    "ratios": [0.5, 0.5],
                    "children": [{"pane": 1}, {"pane": 2}]
                }
            },
            "panes": {
                "1": {"adapter": "shell", "cwd": cwd},
                "2": {"adapter": "shell", "cwd": cwd}
            }
        }]
    })
    .to_string()
}

/// Text of one half of the screen (all rows, columns restricted to that
/// half), newline-joined. A cheap way to scope an assertion to "pane A's
/// side" or "pane B's side" of the 50/50 split without hardcoding border/
/// gap math — geometry precision is explicitly out of scope for this gate.
fn half(screen: &vt100::Screen, right_side: bool) -> String {
    let (_, cols) = screen.size();
    let mid = cols / 2;
    let (start, width) = if right_side { (mid, cols - mid) } else { (0, mid) };
    screen.rows(start, width).collect::<Vec<_>>().join("\n")
}

#[test]
fn firehose_latency_starvation_and_clean_exit() {
    // Short basename deliberately: C4's corner badge ("{adapter} · {cwd
    // basename}") is right-aligned on the pane's own first content row and
    // occludes it by design (DESIGN-ui.md C4) — a long cwd (e.g. this repo's
    // worktree dirname) pushes the badge left far enough to clobber the
    // typed echo we're asserting on a ~58-column-wide half-pane. The system
    // temp dir's basename ("T" on macOS) keeps the badge short and out of
    // the way; it isn't what's under test here.
    let cwd = std::env::temp_dir();
    let cwd = cwd.to_str().expect("temp dir is valid utf8");
    let mut h = match Harness::try_spawn(&fixture_workspace(cwd)) {
        Ok(h) => h,
        Err(reason) => {
            eprintln!("SKIP firehose gate: {reason}");
            return;
        }
    };

    // Let the initial frame (two spawned shell panes) settle before driving it.
    assert!(h.settle(Duration::from_secs(5)), "initial frame never settled");

    // Pane 1 (left, focused by default) becomes the firehose.
    h.write_bytes(SPEW_CMD.as_bytes());
    h.write_bytes(b"\r");
    h.wait_for(Duration::from_secs(3), |s| half(s, false).contains("XXXXXXXXXX"))
        .expect("firehose did not start producing output within 3s");

    // Alt+l: move focus to pane 2 (right) — same meta-ESC encoding as Alt+q
    // (`src/ui/input.rs`: Alt+l / Alt+Right both map to Focus(Dir::Right)).
    h.write_bytes(b"\x1bl");

    // --- Assertion 2 (checked first so its ~3.5s duration overlaps the
    // sustained-spew window assertion 1 also needs, keeping total wall time
    // down): no draw starvation. Pane A's region must keep changing across
    // consecutive 500ms samples for the whole run.
    let mut prev = half(h.screen(), false);
    for i in 0..7 {
        std::thread::sleep(Duration::from_millis(500));
        let cur = half(h.screen(), false);
        assert_ne!(
            cur, prev,
            "pane A's region did not change across 500ms sample #{i} — draw starvation"
        );
        prev = cur;
    }

    // --- Assertion 1: every echo in pane B (now focused) visible within
    // 250ms of being typed, while pane A keeps spewing. 20 keystrokes at
    // ~100ms intervals, per §6.
    let alphabet: Vec<u8> = (b'a'..=b't').collect(); // 20 distinct, printable, never emitted by pane A
    assert_eq!(alphabet.len(), 20);
    let mut typed = String::new();
    let mut latencies = Vec::with_capacity(alphabet.len());
    for &ch in &alphabet {
        typed.push(ch as char);
        let want = typed.clone();
        let tick = Instant::now();
        h.write_bytes(&[ch]);
        let elapsed = h.wait_for(Duration::from_millis(250), move |s| half(s, true).contains(&want));
        let elapsed = elapsed.unwrap_or_else(|| {
            let dump = half(h.screen(), true);
            panic!("echo of {:?} not visible in pane B within 250ms\n--- pane B region ---\n{dump}", ch as char)
        });
        latencies.push(elapsed);
        let pace = Duration::from_millis(100).saturating_sub(tick.elapsed());
        if !pace.is_zero() {
            std::thread::sleep(pace);
        }
    }
    latencies.sort();
    let p50 = latencies[latencies.len() / 2];
    let max = *latencies.last().unwrap();
    eprintln!(
        "firehose echo latency: p50={p50:?} max={max:?} (n={}, bound=250ms)",
        latencies.len()
    );
    for (i, lat) in latencies.iter().enumerate() {
        assert!(*lat <= Duration::from_millis(250), "keystroke #{i} echo took {lat:?}, budget is 250ms");
    }

    // --- Assertion 3: Alt+q exits within 2s under load, no orphaned panes.
    // Sent mid-spew — pane A's loop is still running at this point.
    let roost_pid = h.pid();
    let before = harness::descendant_pids(roost_pid);
    // Sanity check on the fixture itself: two shell panes (+ the spew
    // grandchild) should be roost's descendants. If this is empty, the
    // fixture workspace.json failed to parse and roost fell back to its
    // single-pane default — a bug in the fixture, not in roost.
    assert!(before.len() >= 2, "expected roost's two shell panes as descendants, got {before:?}");

    let exit_elapsed = h.quit_and_wait(Duration::from_secs(5));
    let exit_elapsed =
        exit_elapsed.expect("roost did not exit on its own within 5s (had to be force-killed)");
    eprintln!("firehose clean exit: {exit_elapsed:?} (bound=2s)");
    assert!(exit_elapsed <= Duration::from_secs(2), "exit took {exit_elapsed:?}, budget is 2s");

    // A just-killed process can briefly linger as a zombie before its new
    // parent reaps it; give the OS a short grace window, then require every
    // pre-quit descendant to be gone. Each pane is its own process-group
    // leader (see `harness::descendant_pids` doc comment), so "no live pid
    // from the pre-quit set" is exactly "process-group absence" for all of
    // them.
    let deadline = Instant::now() + Duration::from_millis(800);
    let mut lingering: Vec<u32> = before.clone();
    loop {
        lingering.retain(|&pid| harness::is_alive(pid));
        if lingering.is_empty() || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(lingering.is_empty(), "orphaned process(es) survived Alt+q: {lingering:?}");
}
