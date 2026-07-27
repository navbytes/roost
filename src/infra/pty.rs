//! Production `PaneBackend`: a real PTY child + vt100 terminal state.
//! This is the only module that touches portable-pty. Killing a PtyPane
//! loses nothing precious — the agent's session file is the ground truth,
//! and the adapter knows how to resume it.

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;

use crate::agents::CommandSpec;
use crate::core::event::AppEvent;
use crate::core::status::{AgentStatus, StatusTracker};
use crate::core::workspace::PaneId;
use crate::infra::inspect;
use crate::infra::kitty::KittyKeyboard;
use crate::ports::{MouseProto, Observation, PaneBackend};

const SCROLLBACK_LINES: usize = 5000;

/// P11: host-identity env vars that must never leak into a pane. The hosting
/// terminal (iTerm2, kitty, WezTerm, VS Code) and any outer multiplexer
/// (tmux, zellij) advertise themselves through these; a pane child that sees
/// them sniffs the *host's* identity and negotiates proprietary protocols
/// (iTerm2 inline images, kitty graphics, tmux DCS passthrough) that roost
/// swallows. roost is the pane's terminal — it scrubs these at spawn and
/// presents its own identity instead (`scrub_host_identity`).
const HOST_IDENTITY_VARS: &[&str] = &[
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "KITTY_WINDOW_ID",
    "KITTY_PID",
    "ITERM_SESSION_ID",
    "ITERM_PROFILE",
    "WEZTERM_PANE",
    "WEZTERM_UNIX_SOCKET",
    "TMUX",
    "TMUX_PANE",
    "ZELLIJ",
    "ZELLIJ_SESSION_NAME",
    "VSCODE_INJECTION",
];

/// P11: drop the host terminal's identity from a pane child's environment and
/// present roost's own, so apps adapt to the terminal they are *actually*
/// talking to. TERM and the ROOST_* vars are deliberately not touched here —
/// `spawn` owns those, unchanged.
fn scrub_host_identity(cmd: &mut CommandBuilder) {
    for var in HOST_IDENTITY_VARS {
        cmd.env_remove(var);
    }
    cmd.env("TERM_PROGRAM", "roost");
    cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
}

pub struct PtyPane {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    parser: vt100::Parser,
    status: StatusTracker,
    /// Roost-side scrollback offset (wheel / scroll mode).
    scroll: usize,
    /// The pane's child pid, for OS observation (live cwd / running agent).
    pid: Option<u32>,
    /// Tracks the kitty keyboard flags this pane negotiated, so roost can
    /// forward modified keys (Shift/Ctrl+Enter) in the encoding it asked for.
    kitty: KittyKeyboard,
    /// Per-spawn liveness flag shared with the reader thread. `kill()` clears
    /// it so the (now-doomed) reader stops emitting Output/Exit for this pane
    /// id. Without this, a pane id that is reused (close→new) or respawned
    /// (relaunch) could receive a stale `Exit` from the *old* child's reader
    /// and be flipped straight back to "dead", or get old bytes rendered into
    /// the new pane.
    alive: Arc<AtomicBool>,
}

impl PaneBackend for PtyPane {
    /// Spawn the command in a fresh PTY. A reader thread pumps output into
    /// the main loop via `tx`; the parser is fed on the main thread.
    fn spawn(
        id: PaneId,
        spec: &CommandSpec,
        rows: u16,
        cols: u16,
        tx: SyncSender<AppEvent>,
    ) -> Result<Self> {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("openpty")?;

        let mut cmd = CommandBuilder::new(&spec.program);
        for a in &spec.args {
            cmd.arg(a);
        }
        cmd.cwd(&spec.cwd);
        // P11: no host-identity leaks — the pane's terminal is roost.
        scrub_host_identity(&mut cmd);
        cmd.env("TERM", "xterm-256color");
        // Pane identity for the status socket (roost.ts pi extension /
        // Claude Code hooks) — design doc §6.1.
        cmd.env("ROOST_PANE", id.to_string());
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("spawning {}", spec.program))?;
        let pid = child.process_id();
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().context("clone pty reader")?;
        let writer = pair.master.take_writer().context("take pty writer")?;

        let alive = Arc::new(AtomicBool::new(true));
        let reader_alive = alive.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        // Suppress the Exit if this pane was deliberately killed
                        // (respawn/close): the id may already belong to a new
                        // child, and reporting Exit would wrongly mark it dead.
                        if reader_alive.load(Ordering::Relaxed) {
                            let _ = tx.send(AppEvent::Exit(id));
                        }
                        break;
                    }
                    Ok(n) => {
                        // Same guard for output: don't feed a killed pane's
                        // trailing bytes into whatever now holds this id.
                        if !reader_alive.load(Ordering::Relaxed) {
                            break;
                        }
                        if tx.send(AppEvent::Output(id, buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        Ok(Self {
            master: pair.master,
            child,
            writer,
            parser: vt100::Parser::new(rows, cols, SCROLLBACK_LINES),
            status: StatusTracker::new(),
            scroll: 0,
            pid,
            kitty: KittyKeyboard::new(),
            alive,
        })
    }

    fn process_output(&mut self, bytes: &[u8]) {
        // Answer the pane's kitty-keyboard queries and track the flags it
        // pushes, so modified keys reach it in the encoding it negotiated.
        let reply = self.kitty.feed(bytes);
        if !reply.is_empty() {
            self.write_input_raw(&reply);
        }
        // vt100 counts *parsed* bells, so a 0x07 consumed as an OSC string
        // terminator (ESC ] … BEL) doesn't count — only a real bell does.
        let bells_before = self.parser.screen().audible_bell_count();
        self.parser.process(bytes);
        if self.parser.screen().audible_bell_count() != bells_before {
            self.status.on_bell();
        }
        self.status.on_output();
    }

    fn kitty_disambiguate(&self) -> bool {
        self.kitty.disambiguate()
    }

    fn app_cursor_keys(&self) -> bool {
        // vt100 tracks DECCKM (`CSI ?1h` / `?1l`) from the pane's own output.
        self.parser.screen().application_cursor()
    }

    fn bracketed_paste(&self) -> bool {
        // vt100 tracks mode 2004 (`CSI ?2004h` / `?2004l`) the same way.
        self.parser.screen().bracketed_paste()
    }

    fn write_input(&mut self, bytes: &[u8]) -> bool {
        // Typing means "I'm back" — snap to the live tail.
        if self.scroll != 0 {
            self.scroll = 0;
            self.parser.set_scrollback(0);
        }
        self.write_input_raw(bytes)
    }

    /// Returns whether the write actually reached the child — a dead/broken
    /// pipe must not be reported as delivered by a caller that counts
    /// successful sends (`ctl_send`/`ctl_broadcast`).
    fn write_input_raw(&mut self, bytes: &[u8]) -> bool {
        let ok = self.writer.write_all(bytes).is_ok();
        let _ = self.writer.flush();
        ok
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        if rows == 0 || cols == 0 {
            return;
        }
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        self.parser.set_size(rows, cols);
    }

    fn hangup(&mut self) {
        // SIGHUP the child so it exits the way a closed terminal would —
        // giving pi/claude a chance to flush their final turn to the session
        // file — before shutdown escalates to the guaranteed SIGKILL. Mark the
        // spawn dead first so a resulting EOF doesn't emit a stale event.
        self.alive.store(false, Ordering::Relaxed);
        if let Some(pid) = self.pid {
            // Safety: kill(2) with a pid we own and a plain signal number.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGHUP);
            }
        }
    }

    fn kill(&mut self) {
        // Mark this spawn dead *before* killing so the reader thread, which
        // will see EOF the moment the child dies, doesn't emit a stale
        // Exit/Output for an id that may be reused or respawned.
        self.alive.store(false, Ordering::Relaxed);
        let _ = self.child.kill();
        // The child is a session/process-group leader (portable-pty setsid's
        // it), so also SIGKILL the whole group — otherwise pi/claude's own
        // subprocesses linger as orphans, and a child that's blocked waiting on
        // one of them can itself fail to exit. Signalling -pgid (== -pid for a
        // leader) is a no-op if there's no such group.
        if let Some(pid) = self.pid {
            unsafe {
                libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
            }
        }
        // Reap WITHOUT blocking the UI thread indefinitely. A bare
        // `child.wait()` runs on the event-loop thread (close/quit), so if it
        // ever fails to return promptly — a wedged child, a PTY reaping edge
        // case — it freezes the *whole* app: no input, no render, no quit.
        // SIGKILL is normally reaped within a millisecond; poll `try_wait`
        // briefly (~100ms cap), then move on. A lingering zombie is harmless
        // (reaped when roost exits) and infinitely preferable to a frozen UI.
        for _ in 0..100 {
            match self.child.try_wait() {
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(1)),
                _ => break, // reaped, or errored (already gone)
            }
        }
    }

    fn status(&self) -> AgentStatus {
        self.status.current()
    }

    fn set_extension_status(&mut self, s: AgentStatus) {
        self.status.set_extension_status(s);
    }

    fn on_exit(&mut self) {
        self.status.on_exit();
        // The PTY hit EOF because the child closed it — almost always because
        // it exited. Reap it now (non-blocking) so a pane left sitting in its
        // "exited" state doesn't hold a zombie. If the child somehow closed the
        // PTY without exiting, try_wait returns Ok(None) and we don't block;
        // kill() will reap it definitively when the pane is finally cleaned up.
        // ponytail: once try_wait confirms the reap, `pid` is a dead
        // reference the OS is free to recycle for an unrelated process —
        // clear it so hangup()/kill(), which both already gate their raw
        // libc::kill on `self.pid.is_some()` (and run again unconditionally
        // on every runtime during App::shutdown), can't signal it.
        if let Ok(Some(_)) = self.child.try_wait() {
            self.pid = None;
        }
    }

    fn screen(&self) -> Option<&vt100::Screen> {
        Some(self.parser.screen())
    }

    fn set_scrollback(&mut self, lines: usize) {
        self.parser.set_scrollback(lines);
        // U9: mirror the grid-clamped value back, never the caller's ask —
        // a stored offset past the banked history is a phantom the view
        // ignores, which is exactly what burned ~240 keypresses of
        // overshoot before the screen moved.
        self.scroll = self.parser.screen().scrollback();
    }

    fn scroll_by(&mut self, delta: i32) {
        // U9: base the arithmetic on the grid's CURRENT offset, not the
        // last value we stored — while scrolled, the grid auto-advances
        // the offset as new lines bank (the view stays pinned on content),
        // so a stale base would yank the view toward the tail on the next
        // wheel click. Then read the clamp back, same as set_scrollback.
        let cur = self.parser.screen().scrollback() as i64;
        let want = (cur + i64::from(delta)).max(0) as usize;
        self.parser.set_scrollback(want);
        self.scroll = self.parser.screen().scrollback();
    }

    /// The grid-clamped offset, NOT `self.scroll`: the roost-side counter
    /// can exceed what the grid actually banked (U9's overshoot), and U3's
    /// honesty surfaces must reflect the view, never the phantom intent.
    fn scroll_offset(&self) -> usize {
        self.parser.screen().scrollback()
    }

    fn scroll_total(&self) -> usize {
        self.parser.screen().scrollback_rows()
    }

    /// Forward mouse events only when the inner app speaks SGR encoding —
    /// the modern protocol every current agent TUI uses. Apps in legacy
    /// X10 encoding fall back to roost-side scrolling.
    fn mouse_proto(&self) -> MouseProto {
        let screen = self.parser.screen();
        if screen.mouse_protocol_mode() == vt100::MouseProtocolMode::None {
            return MouseProto::None;
        }
        match screen.mouse_protocol_encoding() {
            vt100::MouseProtocolEncoding::Sgr => MouseProto::Sgr,
            _ => MouseProto::None,
        }
    }

    fn observe(&self, known: &[String]) -> Option<Observation> {
        inspect::observe(self.pid?, known)
    }

    fn grab_text(&self, start: (u16, u16), end: (u16, u16)) -> String {
        extract_selection(self.parser.screen(), start, end)
    }

    fn grab_all_text(&self) -> String {
        self.parser.screen().all_contents()
    }
}

/// Pull the text between two inclusive cell coords (row, col) from a vt100
/// screen, in reading order: from `start` to end-of-line, whole middle lines,
/// and start-of-line to `end`. Trailing spaces are trimmed per line and lines
/// joined with '\n'. `start`/`end` are normalized (either order accepted).
pub fn extract_selection(screen: &vt100::Screen, a: (u16, u16), b: (u16, u16)) -> String {
    let (rows, cols) = screen.size();
    if rows == 0 || cols == 0 {
        return String::new();
    }
    // Normalize so `start` precedes `end` in reading order.
    let (start, end) = if (a.0, a.1) <= (b.0, b.1) { (a, b) } else { (b, a) };
    let mut lines: Vec<String> = Vec::new();
    for row in start.0..=end.0.min(rows - 1) {
        let first = if row == start.0 { start.1 } else { 0 };
        let last = if row == end.0 { end.1 } else { cols - 1 };
        let mut line = String::new();
        for col in first..=last.min(cols - 1) {
            match screen.cell(row, col) {
                Some(c) if !c.contents().is_empty() => line.push_str(&c.contents()),
                _ => line.push(' '),
            }
        }
        while line.ends_with(' ') {
            line.pop();
        }
        lines.push(line);
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{extract_selection, scrub_host_identity, HOST_IDENTITY_VARS};
    use portable_pty::CommandBuilder;
    use std::ffi::OsStr;

    fn screen_with(text: &str, rows: u16, cols: u16) -> vt100::Parser {
        let mut p = vt100::Parser::new(rows, cols, 0);
        p.process(text.as_bytes());
        p
    }

    /// P11: every known host-identity var is removed (whether it came from
    /// the inherited base env or was set on the builder — same map), and
    /// roost's own identity is presented in its place.
    #[test]
    fn scrub_host_identity_removes_leaks_and_presents_roost() {
        let mut cmd = CommandBuilder::new("true");
        for var in HOST_IDENTITY_VARS {
            cmd.env(var, "leaked-from-host");
        }
        scrub_host_identity(&mut cmd);
        for var in HOST_IDENTITY_VARS {
            match *var {
                "TERM_PROGRAM" => {
                    assert_eq!(cmd.get_env(var), Some(OsStr::new("roost")));
                }
                "TERM_PROGRAM_VERSION" => {
                    assert_eq!(cmd.get_env(var), Some(OsStr::new(env!("CARGO_PKG_VERSION"))));
                }
                _ => assert!(cmd.get_env(var).is_none(), "{var} must be scrubbed"),
            }
        }
    }

    /// P11 keeps TERM/ROOST_* behavior unchanged: the scrub itself never
    /// touches them (`spawn` sets TERM=xterm-256color and ROOST_PANE after,
    /// exactly as before).
    #[test]
    fn scrub_host_identity_leaves_term_and_roost_vars_alone() {
        let mut cmd = CommandBuilder::new("true");
        cmd.env("TERM", "host-term");
        cmd.env("ROOST_PANE", "7");
        scrub_host_identity(&mut cmd);
        assert_eq!(cmd.get_env("TERM"), Some(OsStr::new("host-term")));
        assert_eq!(cmd.get_env("ROOST_PANE"), Some(OsStr::new("7")));
    }

    #[test]
    fn extracts_single_line_range() {
        let p = screen_with("hello world", 3, 20);
        // "hello world": cols 0..=10; select "world" = cols 6..=10
        assert_eq!(extract_selection(p.screen(), (0, 6), (0, 10)), "world");
    }

    #[test]
    fn extracts_multi_line_and_trims_trailing() {
        let p = screen_with("abc\r\ndef", 3, 20);
        // from (0,0) to (1,2) → "abc\ndef"
        assert_eq!(extract_selection(p.screen(), (0, 0), (1, 2)), "abc\ndef");
    }

    #[test]
    fn normalizes_reversed_coords() {
        let p = screen_with("hello", 2, 10);
        assert_eq!(extract_selection(p.screen(), (0, 4), (0, 0)), "hello");
    }

    #[test]
    fn parser_scrollback_offset_reads_back_grid_clamped() {
        // U3/U9: the vendored grid clamps `set_scrollback` to the banked
        // history; `screen().scrollback()` (PtyPane::scroll_offset's source)
        // reads that clamp back, and `scrollback_rows()` is its exact upper
        // bound — the pair the ↑N badge token and ↑N/M hint are built on.
        let mut p = vt100::Parser::new(5, 10, 100);
        for i in 0..20 {
            p.process(format!("line{i}\r\n").as_bytes());
        }
        let banked = p.screen().scrollback_rows();
        assert!(banked > 0 && banked <= 100);
        p.set_scrollback(5000); // overshoot far past the banked history
        assert_eq!(p.screen().scrollback(), banked);
        p.set_scrollback(0);
        assert_eq!(p.screen().scrollback(), 0);
    }

    #[test]
    fn zero_dollar_multiline_selection_trims_each_lines_trailing_whitespace() {
        // C24's `0`/`$` keyboard motions drive a realistic `0 v j $ y` flow:
        // row 0 has real trailing spaces printed by the shell (not just
        // unwritten cell padding beyond the row's own content), row 1 is
        // shorter than the screen width. Each line must trim independently
        // and join with '\n' — a per-line trim, not one global trim.
        let p = screen_with("hi   \r\nbye", 3, 10);
        // (0,0) = `0` on row 0; (1,9) = `$` (last column) on row 1 — exactly
        // what pressing 0 then j then $ drives.
        assert_eq!(extract_selection(p.screen(), (0, 0), (1, 9)), "hi\nbye");
    }
}
