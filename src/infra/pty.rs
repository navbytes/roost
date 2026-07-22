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

    fn write_input(&mut self, bytes: &[u8]) {
        // Typing means "I'm back" — snap to the live tail.
        if self.scroll != 0 {
            self.scroll = 0;
            self.parser.set_scrollback(0);
        }
        self.write_input_raw(bytes);
    }

    fn write_input_raw(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
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
        self.scroll = lines;
        self.parser.set_scrollback(lines);
    }

    fn scroll_by(&mut self, delta: i32) {
        self.scroll =
            (self.scroll as i64 + delta as i64).clamp(0, SCROLLBACK_LINES as i64) as usize;
        self.parser.set_scrollback(self.scroll);
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
    use super::extract_selection;

    fn screen_with(text: &str, rows: u16, cols: u16) -> vt100::Parser {
        let mut p = vt100::Parser::new(rows, cols, 0);
        p.process(text.as_bytes());
        p
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
