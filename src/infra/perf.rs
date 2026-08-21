//! Local perf telemetry: the numbers behind the QoS keep-or-delete call.
//!
//! `infra::qos` promotes the event loop to user-interactive QoS on the
//! honest record that the end-to-end firehose gate could not isolate its
//! effect. This module produces the isolated measurement that gate can't:
//! the event loop's own **scheduling stall** — how much later than asked
//! the input poll actually returned. The loop polls the keyboard with a
//! fixed timeout; on a calm machine the poll returns on time, and under
//! CPU starvation the thread simply isn't scheduled when the timer fires.
//! That oversleep is precisely the latency QoS promotion targets, measured
//! on the promoted thread itself, with no dependence on pane shells, echo
//! round-trips, or an external harness.
//!
//! Telemetry is aggregates, never events: one JSON line per minute into
//! `<state>/perf.jsonl` — a bucketed stall histogram, the window max, how
//! many loop iterations and keystrokes the window saw, the 1-minute load
//! average (context: stalls only mean something at load), and whether QoS
//! promotion was active (`ROOST_NO_QOS=1` disables it, which is both the
//! kill switch and the A/B lever: run days with and without, compare the
//! stall distributions at similar load, keep or delete `infra::qos` on the
//! result). ~200 bytes a minute; rotation caps the file (one kept
//! generation, same rename-is-atomic reasoning as `control.log`'s).
//!
//! Nothing sensitive is recorded: timings, counters, a load average.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

/// Flush cadence: one line per minute keeps a fortnight of data in ~2 MiB
/// while still resolving "was it bad while claude was crunching at 3pm".
const FLUSH_EVERY: Duration = Duration::from_secs(60);

/// Rotate `perf.jsonl` past this size; one generation kept (`.jsonl.1`),
/// so the telemetry costs at most ~4 MiB on disk — about a month of
/// always-on windows, which comfortably brackets any A/B the QoS decision
/// needs.
const PERF_LOG_MAX: u64 = 2 * 1024 * 1024;

/// Histogram bucket upper bounds, in milliseconds of stall. The last
/// bucket is unbounded (">500"). Chosen around the numbers that matter:
/// sub-frame stalls (≤25ms) are invisible, ~50–100ms is "felt", ≥250ms is
/// the firehose gate's own budget breached.
const BUCKET_MS: [u64; 8] = [1, 5, 10, 25, 50, 100, 250, 500];

/// One flush window's worth of loop-stall aggregates.
pub struct PerfLog {
    path: PathBuf,
    window_start: Instant,
    iters: u64,
    keys: u64,
    buckets: [u64; BUCKET_MS.len() + 1],
    stall_max: Duration,
    qos: bool,
}

impl PerfLog {
    /// `qos` is whether promotion is active this run (`infra::qos::enabled`)
    /// — recorded on every line so an analysis never has to guess which
    /// mode produced a window.
    pub fn new(state_dir: PathBuf, qos: bool) -> Self {
        Self {
            path: state_dir.join("perf.jsonl"),
            window_start: Instant::now(),
            iters: 0,
            keys: 0,
            buckets: [0; BUCKET_MS.len() + 1],
            stall_max: Duration::ZERO,
            qos,
        }
    }

    /// One event-loop iteration completed its input poll `stall` later than
    /// the timeout asked for (zero when the poll returned on time or early
    /// — an event arriving before the timeout is not a stall).
    pub fn record_iteration(&mut self, stall: Duration) {
        self.iters += 1;
        self.buckets[bucket_index(stall)] += 1;
        if stall > self.stall_max {
            self.stall_max = stall;
        }
    }

    /// `n` keystrokes were drained this iteration — so an analysis can
    /// weight windows where typing actually happened.
    pub fn record_keys(&mut self, n: u64) {
        self.keys += n;
    }

    /// Append the window's line and reset, once `FLUSH_EVERY` has passed.
    /// Best-effort and silent, same stance as the control-log append: a
    /// telemetry line that cannot be written must never cost the loop.
    pub fn maybe_flush(&mut self) {
        if self.window_start.elapsed() < FLUSH_EVERY || self.iters == 0 {
            return;
        }
        let line = self.format_line();
        rotate_log(&self.path, PERF_LOG_MAX);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = f.write_all(line.as_bytes());
        }
        self.window_start = Instant::now();
        self.iters = 0;
        self.keys = 0;
        self.buckets = [0; BUCKET_MS.len() + 1];
        self.stall_max = Duration::ZERO;
    }

    /// The JSON line for the current window (without resetting) — split out
    /// so tests can pin the shape without touching the filesystem clock.
    fn format_line(&self) -> String {
        let ts = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let buckets: Vec<String> = self.buckets.iter().map(u64::to_string).collect();
        format!(
            "{{\"ts\":{ts},\"qos\":{},\"load1\":{:.2},\"iters\":{},\"keys\":{},\"stall_ms_buckets\":[{}],\"stall_max_ms\":{:.1}}}\n",
            self.qos,
            load1(),
            self.iters,
            self.keys,
            buckets.join(","),
            self.stall_max.as_secs_f64() * 1000.0,
        )
    }
}

/// Which histogram bucket a stall lands in — `BUCKET_MS` upper bounds,
/// last bucket unbounded.
fn bucket_index(stall: Duration) -> usize {
    let ms = stall.as_millis() as u64;
    BUCKET_MS.iter().position(|&b| ms <= b).unwrap_or(BUCKET_MS.len())
}

/// The 1-minute load average, or 0.0 where unavailable. Context for the
/// stall numbers: a stall histogram is only meaningful next to how
/// contended the machine was while it accumulated.
fn load1() -> f64 {
    let mut avg = [0f64; 1];
    // SAFETY: out-pointer to a local; getloadavg writes at most `1` entry.
    let n = unsafe { libc::getloadavg(avg.as_mut_ptr(), 1) };
    if n == 1 {
        avg[0]
    } else {
        0.0
    }
}

/// Size-based rotation, `perf.jsonl` → `perf.jsonl.1` — the same
/// atomic-rename, best-effort shape as `app.rs`'s `rotate_audit_log` (its
/// doc carries the full reasoning; kept separate because core must not
/// reach into infra for a 5-line helper, nor the reverse).
fn rotate_log(path: &std::path::Path, max: u64) {
    let too_big = std::fs::metadata(path).map(|m| m.len() >= max).unwrap_or(false);
    if too_big {
        let _ = std::fs::rename(path, path.with_extension("jsonl.1"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_boundaries_are_inclusive_upper_bounds() {
        assert_eq!(bucket_index(Duration::ZERO), 0);
        assert_eq!(bucket_index(Duration::from_millis(1)), 0);
        assert_eq!(bucket_index(Duration::from_millis(2)), 1);
        assert_eq!(bucket_index(Duration::from_millis(25)), 3);
        assert_eq!(bucket_index(Duration::from_millis(26)), 4);
        assert_eq!(bucket_index(Duration::from_millis(500)), 7);
        assert_eq!(bucket_index(Duration::from_millis(501)), 8);
        assert_eq!(bucket_index(Duration::from_secs(10)), 8);
    }

    /// The line is real JSON with the documented fields, and the counters
    /// land where the analysis expects them.
    #[test]
    fn window_line_shape_is_stable() {
        let mut p = PerfLog::new(std::env::temp_dir(), true);
        p.record_iteration(Duration::from_millis(3));
        p.record_iteration(Duration::from_millis(300));
        p.record_keys(7);
        let v: serde_json::Value = serde_json::from_str(p.format_line().trim()).unwrap();
        assert_eq!(v["qos"], true);
        assert_eq!(v["iters"], 2);
        assert_eq!(v["keys"], 7);
        assert_eq!(v["stall_ms_buckets"][1], 1, "3ms lands in the ≤5 bucket");
        assert_eq!(v["stall_ms_buckets"][7], 1, "300ms lands in the ≤500 bucket");
        assert!((v["stall_max_ms"].as_f64().unwrap() - 300.0).abs() < 1.0);
        assert!(v["load1"].as_f64().is_some());
    }

    /// Flush is a no-op inside the window and after an idle (zero-iteration)
    /// window — no empty lines, no clock-only writes.
    #[test]
    fn flush_waits_for_the_window_and_skips_idle_windows() {
        let dir = std::env::temp_dir().join(format!("roost-perf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut p = PerfLog::new(dir.clone(), false);
        p.record_iteration(Duration::from_millis(2));
        p.maybe_flush(); // window not elapsed: nothing written
        assert!(!p.path.exists());
        // Force the window boundary; the pending counters flush.
        p.window_start = Instant::now() - FLUSH_EVERY - Duration::from_secs(1);
        p.maybe_flush();
        let written = std::fs::read_to_string(&p.path).unwrap();
        assert_eq!(written.lines().count(), 1);
        // Idle window: boundary passes with zero iterations → still one line.
        p.window_start = Instant::now() - FLUSH_EVERY - Duration::from_secs(1);
        p.maybe_flush();
        assert_eq!(std::fs::read_to_string(&p.path).unwrap().lines().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotation_renames_past_the_cap() {
        let dir = std::env::temp_dir().join(format!("roost-rot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("perf.jsonl");
        std::fs::write(&path, vec![b'x'; 64]).unwrap();
        rotate_log(&path, 32);
        assert!(!path.exists());
        assert!(path.with_extension("jsonl.1").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
