//! Stall watchdog: catches the main event loop going quiet and gets a real
//! stack trace before the moment is gone. Unrelated to `infra::signals`'
//! *hangup* watchdog (which forces teardown after SIGHUP if the main thread
//! never gets there) — this one runs the whole time roost is up, watching
//! for the loop itself wedging rather than failing to exit.
//!
//! Motivated by a live report (opencode-context-tree, 2026-09-04): roost
//! wedged twice while driving a spawned `opencode` pane through the control
//! socket — sustained ~100% CPU, every thread parked, the socket accepting
//! connections but never answering them ("connected, but no reply within
//! 30s") for 30+ seconds at a stretch. Both times, by the time a human (or
//! an agent) could react and run `sample`, the window to catch the *actual*
//! stuck state was mostly or entirely gone — one attempt landed on a process
//! that had, in the meantime, exited on its own. Watching for it is the only
//! way to reliably capture a report of *this specific hang*, wherever the
//! real bug turns out to live: opted in (`ROOST_WATCHDOG=1`), it runs
//! unattended for the whole session, so the next occurrence leaves evidence
//! with nobody at the keyboard. Opt-in rather than always-on because it is
//! a bug-chasing tool: the people chasing the bug set the variable, and
//! everyone else keeps a sample-free state dir.
//!
//! Design constraint that drives everything here: the watcher must not be
//! able to get stuck the same way as what it is watching for. So it is its
//! own OS thread with its own sleep, reading a single `AtomicU64` the main
//! loop updates once per iteration (a store, nothing that can block) — never
//! a lock, a channel, or anything the main loop could be holding when it
//! wedges.
//!
//! Silent while healthy. On a stall it writes one line to
//! `<state>/watchdog.log` (best-effort, never-break-the-app stance — same as
//! `perf`'s and `control.log`'s writes) and, on macOS, shells out to
//! `/usr/bin/sample` for a real all-threads backtrace into
//! `<state>/watchdog-<ts>.sample.txt` (newest `SAMPLE_KEEP` kept). One
//! report per stall episode, not one per check — a 30-second freeze must not
//! become 30 near-identical lines.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// How often the watchdog checks the heartbeat. Cheap either way — this is
/// a `thread::sleep`, not a spin — but coarser than this would add real
/// latency to noticing a fresh stall.
const CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// How long the main loop can go without a tick before it counts as
/// stalled. The loop's own worst-case healthy cadence is bounded by
/// `CALM_REPAINT` (500ms, main.rs) plus the terminal poll's 33ms timeout —
/// so 3s is generous headroom against one slow iteration (a big resize, a
/// giant paste) while still catching a real freeze (observed: unresponsive
/// 30s+) within a few seconds of it starting.
const STALL_THRESHOLD: Duration = Duration::from_secs(3);

/// Cap on `watchdog.log`, same reasoning and same one-generation-kept shape
/// as `perf::PERF_LOG_MAX` — this file should stay at zero bytes for a
/// healthy run, so the cap is about a pathological flapping case, not
/// normal growth.
const WATCHDOG_LOG_MAX: u64 = 1024 * 1024;

/// How many `watchdog-<ts>.sample.txt` captures to keep; the oldest go when
/// a new one is about to be written. `watchdog.log` is size-capped, and
/// without this the companion files would not be — a flapping session could
/// leave hundreds behind. The newest is the one anyone will look at.
const SAMPLE_KEEP: usize = 5;

/// Is the watchdog on for this run? `ROOST_WATCHDOG=1` (any value) — same
/// any-value-enables shape as `ROOST_DEBUG`. Off by default; see the module
/// doc for why.
pub fn enabled() -> bool {
    std::env::var_os("ROOST_WATCHDOG").is_some()
}

/// The main loop's liveness signal. `Clone` is cheap (an `Instant` plus an
/// `Arc`): the loop keeps one, the watchdog thread keeps another, and
/// `Relaxed` is correct throughout — this is "is the number moving", never
/// a synchronization point for anything else in memory.
#[derive(Clone)]
pub struct Heartbeat {
    start: Instant,
    last_ms: Arc<AtomicU64>,
}

impl Heartbeat {
    pub fn new() -> Self {
        Self { start: Instant::now(), last_ms: Arc::new(AtomicU64::new(0)) }
    }

    /// Call once per main-loop iteration. Nothing but an atomic store — the
    /// loop this exists to protect can never be the thing that slows it
    /// down.
    pub fn tick(&self) {
        self.last_ms.store(self.start.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    /// How long it has been since the last `tick()`.
    fn since_tick(&self) -> Duration {
        let last = self.last_ms.load(Ordering::Relaxed);
        let now = self.start.elapsed().as_millis() as u64;
        Duration::from_millis(now.saturating_sub(last))
    }
}

impl Default for Heartbeat {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawns the watchdog thread. Best-effort by construction: if the thread
/// itself fails to spawn (resource exhaustion, the one time this would ever
/// happen is a machine already in serious trouble), roost carries on without
/// one rather than failing to start over a diagnostics feature.
pub fn spawn(state_dir: PathBuf, heartbeat: Heartbeat) {
    let _ = std::thread::Builder::new()
        .name("roost-watchdog".into())
        .spawn(move || watch(state_dir, heartbeat));
}

fn watch(state_dir: PathBuf, heartbeat: Heartbeat) {
    let mut stalled = false;
    loop {
        std::thread::sleep(CHECK_INTERVAL);
        let gap = heartbeat.since_tick();
        let (report_now, next_stalled) = transition(stalled, gap);
        stalled = next_stalled;
        if report_now {
            report_stall(&state_dir, gap);
        }
    }
}

/// Pure edge-detector: report exactly once per stall episode (the moment
/// `gap` first crosses `STALL_THRESHOLD`), not once per `CHECK_INTERVAL`
/// while it stays crossed — split out from `watch` so the transition logic
/// is testable without a real thread or a real clock.
fn transition(was_stalled: bool, gap: Duration) -> (bool, bool) {
    let is_stalled = gap >= STALL_THRESHOLD;
    (is_stalled && !was_stalled, is_stalled)
}

/// One JSONL line plus, on macOS, a companion `sample` capture. Best-effort
/// throughout: a watchdog that could itself panic or hang roost over a
/// diagnostics write would be worse than none.
fn report_stall(state_dir: &Path, gap: Duration) {
    let ts =
        SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let pid = std::process::id();
    let sample_path = state_dir.join(format!("watchdog-{ts}.sample.txt"));
    prune_samples(state_dir, SAMPLE_KEEP - 1);
    let sampled = sample_stacks(pid, &sample_path);

    let log_path = state_dir.join("watchdog.log");
    let line = format!(
        "{{\"ts\":{ts},\"pid\":{pid},\"stall_gap_ms\":{},\"load1\":{:.2},\"sample\":{}}}\n",
        gap.as_millis(),
        super::perf::load1(),
        sampled.map(|p| format!("\"{}\"", p.display())).unwrap_or_else(|| "null".to_string()),
    );
    let _ = std::fs::create_dir_all(state_dir);
    super::perf::rotate_log(&log_path, WATCHDOG_LOG_MAX);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Delete every `watchdog-<ts>.sample.txt` but the newest `keep`. Names sort
/// chronologically (fixed-width unix seconds), so no metadata reads needed.
/// Best-effort like everything else here.
fn prune_samples(state_dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(state_dir) else { return };
    let mut samples: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("watchdog-") && n.ends_with(".sample.txt"))
        })
        .collect();
    samples.sort();
    for old in samples.iter().rev().skip(keep) {
        let _ = std::fs::remove_file(old);
    }
}

/// `/usr/bin/sample <pid> <secs> -f <out>` — macOS's own all-threads
/// backtrace sampler, ships on every Mac, needs no dependency and no
/// entitlement to sample a process the caller already owns. 2 seconds is
/// long enough to catch a genuine spin (100% CPU shows up immediately) or a
/// parked-forever wait (the stack is identical every sample either way) and
/// short enough not to make the diagnostics themselves felt. Nothing
/// equivalent ships by default on Linux — `perf`/`gdb` both need a install
/// step this crate cannot assume, so a Linux stall report has the JSONL
/// line and nothing else, which is still strictly better than nothing.
#[cfg(target_os = "macos")]
fn sample_stacks(pid: u32, out: &Path) -> Option<PathBuf> {
    let status = std::process::Command::new("/usr/bin/sample")
        .arg(pid.to_string())
        .arg("2")
        .arg("-f")
        .arg(out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    status.success().then(|| out.to_path_buf())
}

#[cfg(not(target_os = "macos"))]
fn sample_stacks(_pid: u32, _out: &Path) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_starts_at_zero_gap_and_grows_untouched() {
        let hb = Heartbeat::new();
        std::thread::sleep(Duration::from_millis(20));
        assert!(hb.since_tick() >= Duration::from_millis(20));
    }

    #[test]
    fn tick_resets_the_gap() {
        let hb = Heartbeat::new();
        std::thread::sleep(Duration::from_millis(20));
        hb.tick();
        assert!(hb.since_tick() < Duration::from_millis(20));
    }

    #[test]
    fn cloned_heartbeat_shares_the_same_counter() {
        let hb = Heartbeat::new();
        let clone = hb.clone();
        std::thread::sleep(Duration::from_millis(10));
        hb.tick();
        assert!(clone.since_tick() < Duration::from_millis(10));
    }

    #[test]
    fn transition_fires_once_at_the_threshold_crossing_not_on_every_check() {
        // healthy: no report, stays not-stalled
        assert_eq!(transition(false, Duration::from_millis(100)), (false, false));
        // crosses: reports exactly this once
        assert_eq!(transition(false, Duration::from_secs(4)), (true, true));
        // still stalled on the next check: no repeat report
        assert_eq!(transition(true, Duration::from_secs(5)), (false, true));
        // recovers: no report, and the next real stall can report again
        assert_eq!(transition(true, Duration::from_millis(50)), (false, false));
        assert_eq!(transition(false, Duration::from_secs(4)), (true, true));
    }

    #[test]
    fn threshold_boundary_is_inclusive() {
        assert_eq!(transition(false, STALL_THRESHOLD), (true, true));
        assert_eq!(transition(false, STALL_THRESHOLD - Duration::from_millis(1)), (false, false));
    }

    #[test]
    fn prune_samples_keeps_the_newest_and_leaves_other_files_alone() {
        let dir = std::env::temp_dir().join(format!("roost-watchdog-prune-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for ts in [1700000001u64, 1700000003, 1700000002, 1700000005, 1700000004] {
            std::fs::write(dir.join(format!("watchdog-{ts}.sample.txt")), "x").unwrap();
        }
        std::fs::write(dir.join("watchdog.log"), "x").unwrap();
        prune_samples(&dir, 2);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            ["watchdog-1700000004.sample.txt", "watchdog-1700000005.sample.txt", "watchdog.log"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn report_stall_never_panics_and_produces_a_readable_line() {
        let dir = std::env::temp_dir().join(format!("roost-watchdog-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        report_stall(&dir, Duration::from_secs(5));
        let written = std::fs::read_to_string(dir.join("watchdog.log")).unwrap();
        let v: serde_json::Value = serde_json::from_str(written.lines().next().unwrap()).unwrap();
        assert_eq!(v["stall_gap_ms"], 5000);
        assert!(v["pid"].as_u64().unwrap() > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
