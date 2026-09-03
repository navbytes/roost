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
use std::sync::{mpsc, Arc, Mutex};
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
    /// Every byte roost has written to its host terminal this session, in
    /// order, *before* any parsing. `screen()` answers "what does the user
    /// see"; this answers "what did roost actually emit" — the only way to
    /// assert on sequences that leave no mark on the grid (SPEC-parity W3:
    /// the re-emitted OSC 9/52, the host title, cursor-shape mirroring).
    /// Written by the reader thread, so it stays current without draining.
    raw: Arc<Mutex<Vec<u8>>>,
}

impl Harness {
    /// Spawn `CARGO_BIN_EXE_roost` in a `COLS`x`ROWS` PTY, with a fresh
    /// `ROOST_STATE` dir seeded with `workspace_json`. Returns `Err(reason)`
    /// instead of panicking when the environment has no functional PTY (e.g.
    /// a sandboxed runner with no `/dev/ptmx`) — callers should skip, not
    /// fail, in that case.
    pub fn try_spawn(workspace_json: &str) -> Result<Self, String> {
        Self::try_spawn_with_env(workspace_json, &[])
    }

    /// Like `try_spawn`, but with extra env vars set on the roost process
    /// itself — for scenarios asserting what a pane child does (or doesn't)
    /// inherit from its host, e.g. the SPEC-parity P11 identity scrub.
    pub fn try_spawn_with_env(workspace_json: &str, envs: &[(&str, &str)]) -> Result<Self, String> {
        Self::try_spawn_sized(workspace_json, envs, ROWS, COLS)
    }

    /// Like `try_spawn_with_env`, but at an explicit PTY geometry instead of
    /// the shared `ROWS`x`COLS` default — the seam a golden-frame scenario
    /// needs to drive a size no other tenant uses (e.g. C30's sub-two-row
    /// notice, which only pre-empts chrome below a 2-row floor).
    pub fn try_spawn_sized(
        workspace_json: &str,
        envs: &[(&str, &str)],
        rows: u16,
        cols: u16,
    ) -> Result<Self, String> {
        let state_dir = fresh_state_dir();
        std::fs::write(state_dir.join("workspace.json"), workspace_json)
            .map_err(|e| format!("writing fixture workspace.json: {e}"))?;
        Self::spawn_in(state_dir, true, envs, rows, cols)
    }

    /// Like `try_spawn`, but also seeds `config.json` — the key-bindings
    /// escape hatch, `src/ui/input.rs` — into the fresh `ROOST_STATE` dir
    /// before roost ever starts, so a scenario can prove the file was
    /// actually read from *there* (and not, say, silently ignored, or read
    /// from the developer's real XDG state dir instead).
    pub fn try_spawn_with_config(workspace_json: &str, config_json: &str) -> Result<Self, String> {
        let state_dir = fresh_state_dir();
        std::fs::write(state_dir.join("workspace.json"), workspace_json)
            .map_err(|e| format!("writing fixture workspace.json: {e}"))?;
        std::fs::write(state_dir.join("config.json"), config_json)
            .map_err(|e| format!("writing fixture config.json: {e}"))?;
        Self::spawn_in(state_dir, true, &[], ROWS, COLS)
    }

    /// Spawn with **no `ROOST_STATE`**, so roost resolves its own
    /// directories the way a real install does — from the XDG variables,
    /// every one of them pointed into a fresh temp tree so a test run can
    /// never read or write the developer's real `~`.
    ///
    /// The seam for asserting *where roost looks* for `config.json`.
    /// `try_spawn_with_config` structurally cannot answer that: it sets
    /// `ROOST_STATE`, whose entire contract is to end the search at one
    /// directory.
    ///
    /// `config_json`, when given, is written to the **XDG config dir**
    /// (`$XDG_CONFIG_HOME/roost/config.json`) — the location under test.
    /// Nothing is ever written to the state dir here, so a file found at all
    /// can only have come from the fallback.
    pub fn try_spawn_xdg(workspace_json: &str, config_json: Option<&str>) -> Result<Self, String> {
        let root = fresh_state_dir();
        // roost's own state dir is `$XDG_STATE_HOME/roost` (src/infra/store.rs).
        let state_dir = root.join("state").join("roost");
        std::fs::create_dir_all(&state_dir)
            .map_err(|e| format!("creating fixture state dir: {e}"))?;
        std::fs::write(state_dir.join("workspace.json"), workspace_json)
            .map_err(|e| format!("writing fixture workspace.json: {e}"))?;

        let config_dir = root.join("cfg").join("roost");
        std::fs::create_dir_all(&config_dir)
            .map_err(|e| format!("creating fixture config dir: {e}"))?;
        if let Some(config_json) = config_json {
            std::fs::write(config_dir.join("config.json"), config_json)
                .map_err(|e| format!("writing fixture config.json: {e}"))?;
        }

        let home = root.to_string_lossy().into_owned();
        let xdg_state = root.join("state").to_string_lossy().into_owned();
        let xdg_config = root.join("cfg").to_string_lossy().into_owned();
        // `XDG_RUNTIME_DIR` is where the control socket lands
        // (src/infra/sock.rs) and would otherwise be *inherited* — pointing
        // every concurrent test at one real runtime dir. `root` itself, not a
        // subdirectory: `sockaddr_un.sun_path` is 104 bytes on macOS, which
        // is the same budget `fresh_state_dir` is kept short for.
        let runtime = root.to_string_lossy().into_owned();
        Self::spawn_in(
            state_dir,
            false,
            &[
                ("HOME", home.as_str()),
                ("XDG_STATE_HOME", xdg_state.as_str()),
                ("XDG_CONFIG_HOME", xdg_config.as_str()),
                ("XDG_DATA_HOME", xdg_state.as_str()),
                ("XDG_RUNTIME_DIR", runtime.as_str()),
            ],
            ROWS,
            COLS,
        )
    }

    /// Shared spawn logic once `state_dir` already holds whatever fixture
    /// files the caller wants roost to see at startup.
    ///
    /// `pin_roost_state` is what separates the two spawn styles. Almost every
    /// scenario wants `true`: `ROOST_STATE` is one variable that isolates the
    /// whole instance. A scenario asserting *where roost looks* for a file
    /// cannot use it — that variable exists precisely to short-circuit the
    /// search — and passes `false`, taking responsibility for isolating
    /// `$HOME` and the XDG variables itself (see `try_spawn_xdg`).
    fn spawn_in(
        state_dir: PathBuf,
        pin_roost_state: bool,
        envs: &[(&str, &str)],
        rows: u16,
        cols: u16,
    ) -> Result<Self, String> {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| format!("no functional PTY available: {e}"))?;

        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_roost"));
        cmd.cwd(env!("CARGO_MANIFEST_DIR"));
        if pin_roost_state {
            cmd.env("ROOST_STATE", &state_dir);
        }
        // Deterministic pane shell: no user rc files / prompt themes to
        // confuse content-based screen assertions.
        cmd.env("SHELL", "/bin/sh");
        cmd.env("TERM", "xterm-256color");
        // ...and a deterministic *character* environment, for the same
        // reason. Inherited, this is whatever the developer's shell exports:
        // under `LC_CTYPE=C` the pane's shell reads the wide-glyph payload in
        // tests/pane_wide_glyphs.rs a byte at a time instead of a character
        // at a time, and that test fails on that machine alone while passing
        // on every CI runner — which is exactly the kind of "works on my
        // box" the rest of this file exists to prevent. LC_ALL rather than
        // LANG so a developer who exports LC_ALL doesn't silently win.
        cmd.env("LC_ALL", "C.UTF-8");
        // Note: a pane's *prompt* cannot be pinned from here. Shell panes are
        // login shells (P18), and macOS's /etc/profile sources /etc/bashrc,
        // which assigns PS1 unconditionally — an inherited one loses. A test
        // that needs a short prompt has to set it in-band; tests/firehose.rs
        // does, and says why.
        // Never let a test run mutate the developer's real ~/.pi extension.
        cmd.env("ROOST_NO_EXT_INSTALL", "1");
        // B2 round 2 (PR #46 review): never let a test run touch the
        // developer's real system clipboard or open their real browser —
        // `#[cfg(test)]` inside src/infra/{clipboard,open}.rs only covers
        // this crate's own unit tests; the binary spawned right below is
        // built without it, so this is the runtime half of that same
        // fix. A scenario that genuinely needs the real channel overrides
        // this back to "0" via `envs` below, which runs after this default.
        cmd.env("ROOST_TEST_NO_HOST_IO", "1");
        // Scenario-specific extras last, so a scenario can override any of
        // the defaults above.
        for (k, v) in envs {
            cmd.env(k, v);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("spawning CARGO_BIN_EXE_roost: {e}"))?;
        drop(pair.slave);

        let mut reader =
            pair.master.try_clone_reader().map_err(|e| format!("clone pty reader: {e}"))?;
        let writer = pair.master.take_writer().map_err(|e| format!("take pty writer: {e}"))?;

        let (tx, rx) = mpsc::channel();
        let raw = Arc::new(Mutex::new(Vec::new()));
        let reader_raw = raw.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break, // PTY closed (child exited) or read error
                    Ok(n) => {
                        // Bank the raw bytes first: a scenario asserting on
                        // what roost *emitted* must see them even if nobody
                        // ever drains the parser channel.
                        if let Ok(mut r) = reader_raw.lock() {
                            r.extend_from_slice(&buf[..n]);
                        }
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break; // Harness dropped, nobody left to read
                        }
                    }
                }
            }
        });

        Ok(Self { child, writer, rx, parser: vt100::Parser::new(rows, cols, 0), state_dir, raw })
    }

    /// Everything roost has written to its host terminal so far, unparsed.
    /// See the `raw` field: this is the seam for asserting on sequences that
    /// never reach the grid.
    pub fn host_bytes(&self) -> Vec<u8> {
        self.raw.lock().map(|r| r.clone()).unwrap_or_default()
    }

    /// Poll until roost's raw host output satisfies `pred`, or bail out after
    /// `timeout`. Returns whether it matched.
    pub fn wait_for_host_bytes(
        &mut self,
        timeout: Duration,
        mut pred: impl FnMut(&[u8]) -> bool,
    ) -> bool {
        let start = Instant::now();
        loop {
            if pred(&self.host_bytes()) {
                return true;
            }
            if start.elapsed() >= timeout {
                return false;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
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
    ///
    /// **Two reads of a screen nothing has been written to also agree.**
    /// Every caller opens with `assert!(h.settle(...), "initial frame never
    /// settled")` and reads the answer as "roost has painted"; without the
    /// wait below it could return `true` in 40 ms having seen roost emit
    /// nothing at all, and any assertion made straight afterwards races the
    /// first frame. (Caught by `tests/panic_shutdown.rs`, whose next line
    /// asks whether roost is in the alternate screen — it was, and the
    /// harness had simply not seen the sequence yet.) Existing scenarios all
    /// happen to follow `settle` with a `wait_for`, which hid it.
    ///
    /// The wait is on **raw output**, not on the screen: roost's first bytes
    /// are mode-setting sequences that leave no mark on the grid, and a
    /// scenario whose settled screen is legitimately blank must still be
    /// able to settle.
    pub fn settle(&mut self, timeout: Duration) -> bool {
        let start = Instant::now();
        while self.host_bytes().is_empty() {
            if start.elapsed() >= timeout {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
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

    /// Wait for roost to exit on its own — no keystroke, no signal. The
    /// seam a scenario needs when what ends the process is roost itself
    /// (`tests/panic_shutdown.rs`), rather than a quit the harness sent.
    pub fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let start = Instant::now();
        loop {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return true;
            }
            if start.elapsed() >= timeout {
                return false;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Quit roost the way a user would and wait for it to exit. Returns the
    /// elapsed time on a clean exit; force-kills the process and returns
    /// `None` if it's still alive after `timeout` (the historical
    /// quit-freeze regression — ROADMAP "Alt+q freeze fix").
    ///
    /// U1 (SPEC-ux): a busy fleet arms a second-press confirm instead of
    /// quitting, so after a grace window with roost still alive the harness
    /// presses Alt+q once more — exactly what a user does at the prompt. A
    /// quiet fleet exits on the first press and never reaches the second.
    pub fn quit_and_wait(&mut self, timeout: Duration) -> Option<Duration> {
        self.write_bytes(ALT_Q);
        let start = Instant::now();
        let mut confirmed = false;
        loop {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return Some(start.elapsed());
            }
            if !confirmed && start.elapsed() >= Duration::from_millis(700) {
                self.write_bytes(ALT_Q); // answer the U1 busy-quit confirm
                confirmed = true;
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

/// Spawn `workspace_json`, or print `SKIP <what>: <reason>` and return
/// `None` — the preamble every call site used to repeat by hand. `what`
/// names the scenario in the SKIP line, which is the load-bearing part for
/// CI logs (a sandboxed runner with no `/dev/ptmx` skips loudly, it never
/// fails silently).
pub fn spawn_or_skip(what: &str, workspace_json: &str) -> Option<Harness> {
    match Harness::try_spawn(workspace_json) {
        Ok(h) => Some(h),
        Err(reason) => {
            eprintln!("SKIP {what}: {reason}");
            None
        }
    }
}

/// Like `spawn_or_skip`, but with extra env vars on the roost process
/// itself (`Harness::try_spawn_with_env`).
pub fn spawn_or_skip_with_env(
    what: &str,
    workspace_json: &str,
    envs: &[(&str, &str)],
) -> Option<Harness> {
    match Harness::try_spawn_with_env(workspace_json, envs) {
        Ok(h) => Some(h),
        Err(reason) => {
            eprintln!("SKIP {what}: {reason}");
            None
        }
    }
}

/// Like `spawn_or_skip`, but at an explicit PTY geometry
/// (`Harness::try_spawn_sized`).
pub fn spawn_or_skip_sized(
    what: &str,
    workspace_json: &str,
    envs: &[(&str, &str)],
    rows: u16,
    cols: u16,
) -> Option<Harness> {
    match Harness::try_spawn_sized(workspace_json, envs, rows, cols) {
        Ok(h) => Some(h),
        Err(reason) => {
            eprintln!("SKIP {what}: {reason}");
            None
        }
    }
}

/// Like `spawn_or_skip`, but also seeds `config.json`
/// (`Harness::try_spawn_with_config`).
pub fn spawn_or_skip_with_config(
    what: &str,
    workspace_json: &str,
    config_json: &str,
) -> Option<Harness> {
    match Harness::try_spawn_with_config(workspace_json, config_json) {
        Ok(h) => Some(h),
        Err(reason) => {
            eprintln!("SKIP {what}: {reason}");
            None
        }
    }
}

/// One shell pane, one tab. The fixture every single-pane scenario that
/// doesn't care about layout used to redefine byte-for-byte.
pub fn one_pane(cwd: &str) -> String {
    serde_json::json!({
        "version": 1,
        "active_tab": 0,
        "tabs": [{
            "name": "main",
            "layout": { "pane": 1 },
            "panes": { "1": {"adapter": "shell", "cwd": cwd} }
        }]
    })
    .to_string()
}

/// Two side-by-side shell panes, one tab. Pane 1 (left) is focused by
/// default (`App::new` focuses the first pane in DFS order).
pub fn two_panes(cwd: &str) -> String {
    serde_json::json!({
        "version": 1,
        "active_tab": 0,
        "tabs": [{
            "name": "main",
            "layout": {
                "split": {
                    "dir": "vertical",
                    "ratios": [0.5, 0.5],
                    "children": [{ "pane": 1 }, { "pane": 2 }]
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

/// Two tabs: `main` with one shell pane, `api` with two side by side —
/// enough surface for a scenario that moves or reports panes across tabs.
pub fn two_tabs(cwd: &str) -> String {
    serde_json::json!({
        "version": 1,
        "active_tab": 0,
        "tabs": [
            {
                "name": "main",
                "layout": { "pane": 1 },
                "panes": { "1": {"adapter": "shell", "cwd": cwd} }
            },
            {
                "name": "api",
                "layout": {
                    "split": {
                        "dir": "vertical",
                        "ratios": [0.5, 0.5],
                        "children": [{ "pane": 2 }, { "pane": 3 }]
                    }
                },
                "panes": {
                    "2": {"adapter": "shell", "cwd": cwd},
                    "3": {"adapter": "shell", "cwd": cwd}
                }
            }
        ],
        "next_pane_id": 4
    })
    .to_string()
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

/// Whether `pid` still names a **running** process.
///
/// `kill -0` alone is not that question: it succeeds for a zombie, which is
/// a process that has already died and is only waiting for its parent to
/// collect the exit status. A zombie holds no PTY, no memory and no CPU —
/// calling one a survivor makes the orphan gates fail on any host whose PID 1
/// reaps lazily, regardless of what roost did. (Measured here: PID 1 is a
/// container shim, and a killed grandchild sat in state `Z` well past the
/// gate's 800ms grace.) So the state is checked too, and only a genuinely
/// live process counts.
///
/// `output()` rather than `status()` so a "No such process" — the expected,
/// common case — doesn't spam the test's own stderr.
pub fn is_alive(pid: u32) -> bool {
    let exists = Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    exists && !is_zombie(pid)
}

/// Is `pid` a corpse awaiting reaping? Linux answers from `/proc` (state is
/// the field right after the last `)`, since the executable name in field 2
/// can contain spaces and parens); elsewhere, from `ps`. An unreadable
/// answer means "not a zombie", which keeps `is_alive` conservative — a gate
/// should fail loudly on a real survivor rather than quietly excuse one.
fn is_zombie(pid: u32) -> bool {
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        return stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|state| state == "Z");
    }
    Command::new("ps")
        .args(["-o", "state=", "-p", &pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().starts_with('Z'))
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
