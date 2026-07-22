//! Shared PTY test harness (DESIGN-ui.md §6): spawns the built `roost`
//! binary inside a real PTY, drives it with raw bytes exactly as a terminal
//! would, and parses its output with `vt100` so tests can assert on the
//! rendered screen instead of guessing at internals.
//!
//! This is the seam for the deferred golden-frame harness: `tests/firehose.rs`
//! is its first tenant (input-latency/starvation/clean-exit only, no color
//! assertions). A future golden-frame test reuses `Harness::try_spawn` +
//! `Harness::settle` + `Harness::screen` and adds its own cell/coordinate
//! checks — nothing here is firehose-specific.
//!
//! macOS/portable-pty notes (see DESIGN-ui.md §6 + PLAN.md F7):
//! - The PTY reader must live on its own thread and never block the caller;
//!   `drain` only does non-blocking `try_recv` against a channel it feeds.
//! - `ROOST_STATE` doubles as the directory for roost's control socket, and
//!   `sockaddr_un.sun_path` is 104 bytes on macOS — it must stay SHORT. We
//!   build it directly under `std::env::temp_dir()` with a short suffix, not
//!   nested under (e.g.) the cargo target dir.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, CommandBuilder, PtySize};

/// Fixed geometry for every scenario (DESIGN-ui.md §6).
pub const ROWS: u16 = 40;
pub const COLS: u16 = 120;

/// Alt+q, meta-ESC encoding: the bytes a real terminal sends for Alt+<letter>
/// and what roost's raw-mode crossterm reader parses as the modifier (see
/// DESIGN-ui.md §6/§8, and `src/ui/input.rs`'s Alt-chord table).
pub const ALT_Q: &[u8] = b"\x1bq";

/// A running roost instance driven through a real PTY.
pub struct Harness {
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    rx: mpsc::Receiver<Vec<u8>>,
    parser: vt100::Parser,
    state_dir: PathBuf,
}

impl Harness {
    /// Spawn `CARGO_BIN_EXE_roost` in a `COLS`x`ROWS` PTY, with a fresh
    /// `ROOST_STATE` dir seeded with `workspace_json`. Returns `Err(reason)`
    /// instead of panicking when the environment has no functional PTY (e.g.
    /// a sandboxed runner with no `/dev/ptmx`) — callers should skip, not
    /// fail, in that case.
    pub fn try_spawn(workspace_json: &str) -> Result<Self, String> {
        let state_dir = fresh_state_dir();
        std::fs::write(state_dir.join("workspace.json"), workspace_json)
            .map_err(|e| format!("writing fixture workspace.json: {e}"))?;

        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows: ROWS, cols: COLS, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| format!("no functional PTY available: {e}"))?;

        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_roost"));
        cmd.cwd(env!("CARGO_MANIFEST_DIR"));
        cmd.env("ROOST_STATE", &state_dir);
        // Deterministic pane shell: no user rc files / prompt themes to
        // confuse content-based screen assertions.
        cmd.env("SHELL", "/bin/sh");
        cmd.env("TERM", "xterm-256color");
        // Never let a test run mutate the developer's real ~/.pi extension.
        cmd.env("ROOST_NO_EXT_INSTALL", "1");

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("spawning CARGO_BIN_EXE_roost: {e}"))?;
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("clone pty reader: {e}"))?;
        let writer =
            pair.master.take_writer().map_err(|e| format!("take pty writer: {e}"))?;

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break, // PTY closed (child exited) or read error
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break; // Harness dropped, nobody left to read
                        }
                    }
                }
            }
        });

        Ok(Self { child, writer, rx, parser: vt100::Parser::new(ROWS, COLS, 0), state_dir })
    }

    /// Pull all currently-buffered PTY output into the parser without
    /// blocking (the reader thread does the actual, possibly-blocking read).
    fn drain(&mut self) {
        while let Ok(chunk) = self.rx.try_recv() {
            self.parser.process(&chunk);
        }
    }

    /// The parsed screen, current as of whatever output has arrived so far.
    pub fn screen(&mut self) -> &vt100::Screen {
        self.drain();
        self.parser.screen()
    }

    /// Send raw bytes to roost exactly as a terminal would (keystrokes,
    /// pastes, or escape sequences like `ALT_Q`).
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("write to pty");
        let _ = self.writer.flush();
    }

    /// Poll-parse until two consecutive reads of the screen agree, or bail
    /// out after `timeout`. Returns whether it actually settled.
    ///
    /// This is the golden-frame seam: a future frame/cell-color assertion
    /// calls this before reading `screen()`, exactly like this fn already
    /// does internally.
    pub fn settle(&mut self, timeout: Duration) -> bool {
        let start = Instant::now();
        let mut prev = self.screen().contents();
        loop {
            std::thread::sleep(Duration::from_millis(40));
            let cur = self.screen().contents();
            if cur == prev {
                return true;
            }
            prev = cur;
            if start.elapsed() >= timeout {
                return false;
            }
        }
    }

    /// Poll until `pred` matches the current screen, or bail out after
    /// `timeout`. Returns the elapsed time to success — the firehose gate's
    /// own latency measurement is just this fn's return value.
    pub fn wait_for(
        &mut self,
        timeout: Duration,
        mut pred: impl FnMut(&vt100::Screen) -> bool,
    ) -> Option<Duration> {
        let start = Instant::now();
        loop {
            if pred(self.screen()) {
                return Some(start.elapsed());
            }
            if start.elapsed() >= timeout {
                return None;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    /// The spawned roost process's own pid (for descendant/orphan checks).
    pub fn pid(&self) -> u32 {
        self.child.process_id().expect("roost has a pid")
    }

    /// The instance's `ROOST_STATE` dir (control socket, workspace.json,
    /// control.log) — lets a scenario assert on the audit log.
    pub fn state_dir(&self) -> &std::path::Path {
        &self.state_dir
    }

    /// Send Alt+q and wait for roost to exit on its own. Returns the elapsed
    /// time on a clean exit; force-kills the process and returns `None` if
    /// it's still alive after `timeout` (the historical quit-freeze
    /// regression — ROADMAP "Alt+q freeze fix").
    pub fn quit_and_wait(&mut self, timeout: Duration) -> Option<Duration> {
        self.write_bytes(ALT_Q);
        let start = Instant::now();
        loop {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return Some(start.elapsed());
            }
            if start.elapsed() >= timeout {
                let _ = self.child.kill();
                let _ = self.child.wait();
                return None;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }
}

impl Drop for Harness {
    /// Best-effort cleanup so a failing assertion (which unwinds before an
    /// explicit `quit_and_wait`) never leaves roost or a pane's spawned
    /// process running on the developer's machine.
    fn drop(&mut self) {
        if let Some(pid) = self.child.process_id() {
            for d in descendant_pids(pid) {
                kill9(d);
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.state_dir);
    }
}

/// PIDs of every live descendant of `pid` (children, grandchildren, ...)
/// right now, via `pgrep -P`. Used both for a test's own no-orphans
/// assertion and this module's `Drop` safety net.
///
/// Each pane roost spawns is its own session/process-group leader
/// (portable-pty calls `setsid` per spawn — see `src/infra/pty.rs`'s
/// group-wide `kill()`), so walking the pid tree from the outside — rather
/// than trusting a single captured pgid — is what actually proves "no child
/// of it survives" regardless of which pgid a given descendant ended up in.
pub fn descendant_pids(pid: u32) -> Vec<u32> {
    let mut all = Vec::new();
    let mut frontier = vec![pid];
    while let Some(p) = frontier.pop() {
        let Ok(out) = Command::new("pgrep").arg("-P").arg(p.to_string()).output() else { break };
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Ok(cpid) = line.trim().parse::<u32>() {
                all.push(cpid);
                frontier.push(cpid);
            }
        }
    }
    all
}

/// Whether `pid` still names a live process (`kill -0`). Uses `output()`
/// rather than `status()` so a "No such process" (the expected, common case)
/// doesn't spam the test's own stderr.
pub fn is_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn kill9(pid: u32) {
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
}

/// A short, unique `ROOST_STATE` directory directly under the system temp
/// dir. Must stay short: it also hosts roost's control socket, and
/// `sockaddr_un.sun_path` is only 104 bytes on macOS — nesting it any deeper
/// (e.g. under the cargo target dir) risks blowing that budget.
fn fresh_state_dir() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rst{}{:x}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create ROOST_STATE dir");
    dir
}
