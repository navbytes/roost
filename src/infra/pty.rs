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
use std::time::{Duration, Instant};

use crate::agents::CommandSpec;
use crate::core::event::AppEvent;
use crate::core::status::{AgentStatus, StatusTracker};
use crate::core::workspace::PaneId;
use crate::infra::inspect;
use crate::infra::queries::QueryResponder;
use crate::ports::{MouseProto, Observation, PaneBackend, PaneEffects};

const SCROLLBACK_LINES: usize = 5000;

/// P2: at most one OSC 9 re-emission per pane per this window. An agent that
/// notifies in a loop (or a `cat` of a log full of them) must not turn the
/// host terminal into a notification firehose the user has to force-quit.
const HOST_NOTIFY_INTERVAL: Duration = Duration::from_secs(1);

/// P2: how much of a notification body is re-emitted to the host. Long
/// enough for a real "needs your approval to run X" line, short enough that
/// a pane can't push a megabyte through roost's own stdout.
const HOST_NOTIFY_CAP: usize = 200;

/// P2/P3: make text safe to embed in a sequence roost writes to its OWN
/// terminal. A pane's payload is untrusted: left alone, an embedded ESC/BEL/
/// ST could close roost's sequence early and have the rest interpreted as
/// host commands (a pane repainting the user's real terminal). C0 controls
/// are dropped outright and the result truncated to `cap` characters —
/// truncation is on char boundaries, so a multi-byte glyph is never split.
fn sanitize_for_host(text: &str, cap: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(cap).collect()
}

/// P2: the OSC 9 roost re-emits to its own terminal for a pane
/// notification, or `None` when this one must be dropped — too soon after
/// the last (`interval`), or nothing left after sanitizing. Pure (clock and
/// limits are parameters) so the rate limit is proven without sleeping.
fn host_notify_bytes(
    body: &str,
    last: Option<Instant>,
    now: Instant,
    interval: Duration,
    cap: usize,
) -> Option<Vec<u8>> {
    if last.is_some_and(|t| now.duration_since(t) < interval) {
        return None;
    }
    let body = sanitize_for_host(body, cap);
    if body.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(body.len() + 5);
    out.extend_from_slice(b"\x1b]9;");
    out.extend_from_slice(body.as_bytes());
    out.push(0x07);
    Some(out)
}

/// P1: how long an open synchronized-output bracket (mode 2026) may keep the
/// pre-bracket frame on screen. Real brackets close within a frame or two; a
/// stuck one — an app killed mid-redraw, a bug that never sends `?2026l` —
/// must never freeze the pane, so past the cap the live grid is presented
/// again. Torn beats frozen.
const SYNC_STALE_CAP: Duration = Duration::from_millis(150);

/// P1: which screen to *present* — the last complete frame while a
/// synchronized-output bracket is open and fresh, else the live grid. Pure
/// (the cap is a parameter) so the stuck-bracket expiry is unit-testable
/// without sleeping for real.
fn sync_presented<'a>(
    live: &'a vt100::Screen,
    view: Option<&'a (vt100::Screen, Instant)>,
    cap: Duration,
) -> &'a vt100::Screen {
    match view {
        // A scrolled-back view is already frozen on history the snapshot
        // doesn't carry (`Screen::snapshot` drops the scrollback), so
        // presenting it would yank the pane to the live tail. While the user
        // reads history, tearing in the tail is invisible anyway — U3's
        // frozen view wins.
        Some(_) if live.scrollback() > 0 => live,
        Some((snap, opened)) if opened.elapsed() < cap => snap,
        _ => live,
    }
}

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
    /// Answers the pane's terminal queries (DA1, DSR, DECRQM, XTWINOPS, …)
    /// and tracks the kitty keyboard flags it negotiated, so roost can
    /// forward modified keys (Shift/Ctrl+Enter) in the encoding it asked for.
    queries: QueryResponder,
    /// The pane's pixel geometry (width, height), derived by the core from
    /// the host window's proportionally; (0, 0) = unknown. Fed to the PTY's
    /// winsize and to the XTWINOPS 14/16t replies.
    pixels: (u16, u16),
    /// P1: while the pane's app holds a synchronized-output bracket (mode
    /// 2026) open, the last frame it declared complete — captured by the
    /// parser at the exact stream position of the `?2026h` — plus when the
    /// bracket opened (for `SYNC_STALE_CAP`). This is what `screen()` and
    /// `grab_text` present: the app asked for its in-progress redraw not to
    /// be shown, and a half-drawn grid is exactly what P1 measured leaking
    /// into both the TUI and `roost read`.
    ///
    /// Deliberately NOT consulted by scroll state (`scroll_offset`/
    /// `scroll_total`/`set_scrollback`/`scroll_by`), full-history reads
    /// (`grab_all_text`), or the input-mode accessors: those answer for the
    /// live grid, which stays the single source of truth. The snapshot is a
    /// ≤150 ms presentation veneer over the visible frame, not a second
    /// terminal state.
    sync_view: Option<(vt100::Screen, Instant)>,
    /// W3: effects routed out of the pane's escape stream and waiting for
    /// the core to drain them (`take_effects`).
    effects: PaneEffects,
    /// P2: when this pane last re-emitted an OSC 9 to the host, for
    /// `HOST_NOTIFY_INTERVAL`. `None` until the first one.
    last_host_notify: Option<Instant>,
    /// Per-spawn liveness flag shared with the reader thread. `kill()` clears
    /// it so the (now-doomed) reader stops emitting Output/Exit for this pane
    /// id. Without this, a pane id that is reused (close→new) or respawned
    /// (relaunch) could receive a stale `Exit` from the *old* child's reader
    /// and be flipped straight back to "dead", or get old bytes rendered into
    /// the new pane.
    alive: Arc<AtomicBool>,
}

impl PtyPane {
    /// P1: the screen roost *presents* for this pane — the last complete
    /// frame while a synchronized-output bracket is open (and fresh), else
    /// the live grid. Every surface that shows the user what the pane looks
    /// like right now goes through here: `screen()` (blit + host cursor) and
    /// `grab_text` (copy-mode selection, `roost read`'s screen mode — the
    /// surface P1 measured 31/50 torn samples on). History and state
    /// surfaces deliberately do not; see `sync_view`.
    fn presented(&self) -> &vt100::Screen {
        sync_presented(self.parser.screen(), self.sync_view.as_ref(), SYNC_STALE_CAP)
    }

    /// W3: turn one parser effect into roost-side consequences — attention
    /// state here, texts and host bytes queued for the core to drain.
    fn route_effect(&mut self, effect: vt100::Effect) {
        match effect {
            // P2: a notification is an explicit "I need you". It takes the
            // same attention path as a bell (the heuristic that surfaces ◆
            // once the pane goes quiet) — the whole reason OSC 9 vanishing
            // was a P0: the OSC-terminating BEL is deliberately not counted
            // as a bell, so nothing else could ever notice.
            vt100::Effect::Notify { title, body } => {
                self.status.on_bell();
                let text = match &title {
                    Some(t) if !t.is_empty() => format!("{t}: {body}"),
                    _ => body.clone(),
                };
                self.effects.notifications.push(text);
                self.queue_host_notify(&body);
            }
            // Declared by the W3 effects surface, routed by the items that
            // own them: P3 (clipboard forwarding) and P7 (cursor fidelity).
            // Spelled out rather than swallowed by a catch-all so the
            // compiler keeps pointing at this match until they land.
            vt100::Effect::Osc52Write { .. } | vt100::Effect::CursorShape(_) => {}
        }
    }

    /// P2: re-emit the notification to the HOST terminal as an OSC 9, so the
    /// native desktop notification the app asked for actually fires. Rate-
    /// limited per pane and length-capped; the body is sanitized because it
    /// is untrusted text about to ride inside a sequence roost writes to its
    /// own terminal.
    fn queue_host_notify(&mut self, body: &str) {
        let now = Instant::now();
        let Some(bytes) =
            host_notify_bytes(body, self.last_host_notify, now, HOST_NOTIFY_INTERVAL, HOST_NOTIFY_CAP)
        else {
            return;
        };
        self.last_host_notify = Some(now);
        self.effects.host_writes.extend_from_slice(&bytes);
    }
}

impl PaneBackend for PtyPane {
    /// Spawn the command in a fresh PTY. A reader thread pumps output into
    /// the main loop via `tx`; the parser is fed on the main thread.
    fn spawn(
        id: PaneId,
        spec: &CommandSpec,
        rows: u16,
        cols: u16,
        pixels: (u16, u16),
        tx: SyncSender<AppEvent>,
    ) -> Result<Self> {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows, cols, pixel_width: pixels.0, pixel_height: pixels.1 })
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
            queries: QueryResponder::new(),
            pixels,
            sync_view: None,
            effects: PaneEffects::default(),
            last_host_notify: None,
            alive,
        })
    }

    fn process_output(&mut self, bytes: &[u8]) {
        // vt100 counts *parsed* bells, so a 0x07 consumed as an OSC string
        // terminator (ESC ] … BEL) doesn't count — only a real bell does.
        let bells_before = self.parser.screen().audible_bell_count();
        self.parser.process(bytes);
        if self.parser.screen().audible_bell_count() != bells_before {
            self.status.on_bell();
        }
        // Answer the pane's terminal queries (and track its kitty keyboard
        // flags) — AFTER the chunk is parsed, so stateful replies (DSR 6
        // cursor position, DECRQM mode values) reflect the state the app
        // just finished establishing in this very chunk, not the state one
        // chunk ago. Replies land in stream-encounter order.
        let reply = self.queries.feed(bytes, self.parser.screen(), self.pixels);
        if !reply.is_empty() {
            self.write_input_raw(&reply);
        }
        // P1: adopt/retire the synchronized-output presentation view. The
        // parser hands back a capture exactly once per bracket that opened
        // in this chunk (a reopen replaces the previous one — the newer
        // capture is the frame the app just finished); once no bracket is
        // open, the live grid is current again and the veneer goes away.
        if let Some(snap) = self.parser.take_sync_snapshot() {
            self.sync_view = Some((snap, Instant::now()));
        }
        if !self.parser.screen().synchronized_output() {
            self.sync_view = None;
        }
        // W3: route everything the chunk asked roost to do out there.
        let effects = self.parser.take_effects();
        for effect in effects {
            self.route_effect(effect);
        }
        self.status.on_output();
    }

    fn kitty_disambiguate(&self) -> bool {
        self.queries.disambiguate()
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

    fn resize(&mut self, rows: u16, cols: u16, pixels: (u16, u16)) {
        if rows == 0 || cols == 0 {
            return;
        }
        self.pixels = pixels;
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: pixels.0,
            pixel_height: pixels.1,
        });
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

    fn take_effects(&mut self) -> PaneEffects {
        std::mem::take(&mut self.effects)
    }

    /// P1: the *presentation* view (see `PtyPane::presented`), so the
    /// renderer's blit and cursor placement can never show a frame the app
    /// declared incomplete. Transparent to the caller — the renderer asks
    /// for "the pane's screen" exactly as before.
    fn screen(&self) -> Option<&vt100::Screen> {
        Some(self.presented())
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

    /// P1: reads the *presented* frame, not the live grid — this is what the
    /// user sees (copy-mode selection) and what `roost read` reports for the
    /// visible screen, the exact surface P1 measured mid-bracket tearing on.
    fn grab_text(&self, start: (u16, u16), end: (u16, u16)) -> String {
        extract_selection(self.presented(), start, end)
    }

    /// P1 (the other side of the split): the full history read stays on the
    /// live grid. The presentation snapshot carries no scrollback by design,
    /// and `read --full`/`--tail` is a question about the pane's whole
    /// recorded output, not about the frame currently on screen.
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
    use super::{
        extract_selection, host_notify_bytes, sanitize_for_host, scrub_host_identity,
        sync_presented, HOST_IDENTITY_VARS, HOST_NOTIFY_CAP, HOST_NOTIFY_INTERVAL,
        SYNC_STALE_CAP,
    };
    use portable_pty::CommandBuilder;
    use std::ffi::OsStr;
    use std::time::{Duration, Instant};

    fn screen_with(text: &str, rows: u16, cols: u16) -> vt100::Parser {
        let mut p = vt100::Parser::new(rows, cols, 0);
        p.process(text.as_bytes());
        p
    }

    /// P1: with no bracket open there is nothing to present but the live
    /// grid — the veneer costs nothing in the overwhelmingly common case.
    #[test]
    fn sync_presents_the_live_grid_when_no_bracket_is_open() {
        let p = screen_with("live", 4, 20);
        let presented = sync_presented(p.screen(), None, SYNC_STALE_CAP);
        assert!(presented.contents().contains("live"));
    }

    /// P1: a fresh bracket presents the captured frame, not the half-drawn
    /// live grid — the whole point of mode 2026.
    #[test]
    fn sync_presents_the_captured_frame_while_the_bracket_is_fresh() {
        let mut p = vt100::Parser::new(4, 20, 0);
        p.process(b"complete");
        p.process(b"\x1b[?2026h\x1b[2J\x1b[Htorn");
        let snap = p.take_sync_snapshot().expect("bracket open captures");
        let view = Some((snap, Instant::now()));
        let presented = sync_presented(p.screen(), view.as_ref(), SYNC_STALE_CAP);
        assert!(presented.contents().contains("complete"));
        assert!(!presented.contents().contains("torn"));
    }

    /// P1's safety valve: a bracket that never closes (an app killed
    /// mid-redraw) must not freeze the pane forever. Past `SYNC_STALE_CAP`
    /// the live grid is presented again — torn beats frozen. Pure, so the
    /// expiry is proven without sleeping 150 ms.
    #[test]
    fn a_stuck_bracket_expires_at_the_staleness_cap() {
        let mut p = vt100::Parser::new(4, 20, 0);
        p.process(b"complete");
        p.process(b"\x1b[?2026h\x1b[2J\x1b[Hhalf-drawn");
        let snap = p.take_sync_snapshot().expect("bracket open captures");

        // One tick shy of the cap: still the captured frame.
        let fresh = Some((snap.clone(), Instant::now() - SYNC_STALE_CAP + Duration::from_millis(1)));
        assert!(sync_presented(p.screen(), fresh.as_ref(), SYNC_STALE_CAP)
            .contents()
            .contains("complete"));

        // Past it: the live grid, however torn — the pane keeps moving even
        // though the app never sent `?2026l`.
        let stale = Some((snap, Instant::now() - SYNC_STALE_CAP - Duration::from_millis(1)));
        let presented = sync_presented(p.screen(), stale.as_ref(), SYNC_STALE_CAP);
        assert!(presented.contents().contains("half-drawn"));
        assert!(!presented.contents().contains("complete"));
    }

    /// P1 × U3: a scrolled-back view is already frozen on history, which the
    /// snapshot deliberately doesn't carry — presenting it would yank the
    /// pane to the live tail mid-read. The live (scroll-offset) grid wins.
    #[test]
    fn a_scrolled_view_ignores_the_sync_snapshot() {
        let mut p = vt100::Parser::new(4, 20, 100);
        for i in 0..20 {
            p.process(format!("row{i}\r\n").as_bytes());
        }
        p.process(b"\x1b[?2026h\x1b[2J\x1b[Hredrawing");
        let snap = p.take_sync_snapshot().expect("bracket open captures");
        let view = Some((snap, Instant::now()));
        // Scrolled to the live tail: the snapshot presents as usual.
        assert!(sync_presented(p.screen(), view.as_ref(), SYNC_STALE_CAP)
            .contents()
            .contains("row19"));
        // Scrolled into history: the live grid's own scrolled view wins.
        p.set_scrollback(5);
        assert!(p.screen().scrollback() > 0);
        let presented = sync_presented(p.screen(), view.as_ref(), SYNC_STALE_CAP);
        assert_eq!(presented.contents(), p.screen().contents());
    }

    /// P2: the shape roost re-emits, and the two things that must never
    /// reach the host — a body that could break out of roost's own sequence,
    /// and an unbounded one.
    #[test]
    fn host_notify_is_bounded_and_cannot_break_out_of_its_sequence() {
        let now = Instant::now();
        let emit = |body: &str| host_notify_bytes(body, None, now, HOST_NOTIFY_INTERVAL, HOST_NOTIFY_CAP);

        assert_eq!(emit("NEEDS-YOU").unwrap(), b"\x1b]9;NEEDS-YOU\x07".to_vec());

        // A pane's payload is untrusted: an embedded BEL/ESC would close
        // roost's own OSC early and let the rest be read as host commands.
        let hostile = emit("safe\x07\x1b]0;PWNED\x07\x1b[2Jtail").unwrap();
        assert_eq!(hostile, b"\x1b]9;safe]0;PWNED[2Jtail\x07".to_vec());
        assert_eq!(hostile.iter().filter(|&&b| b == 0x07).count(), 1, "one terminator");
        assert_eq!(hostile.iter().filter(|&&b| b == 0x1b).count(), 1, "one introducer");

        // Length is capped in characters, on char boundaries.
        let long = emit(&"x".repeat(HOST_NOTIFY_CAP * 3)).unwrap();
        assert_eq!(long.len(), HOST_NOTIFY_CAP + 5);
        let wide = emit(&"日".repeat(HOST_NOTIFY_CAP * 2)).unwrap();
        assert!(std::str::from_utf8(&wide[4..wide.len() - 1]).is_ok(), "never split a glyph");

        // Nothing printable left ⇒ nothing emitted (no empty OSC 9).
        assert!(emit("\x07\x1b\x00").is_none());
        assert!(emit("").is_none());
    }

    /// P2: at most one host notification per pane per interval — an agent
    /// that notifies in a loop must not become a notification firehose.
    #[test]
    fn host_notify_is_rate_limited_per_pane() {
        let now = Instant::now();
        let cap = HOST_NOTIFY_CAP;
        // Just inside the window: dropped.
        let recent = Some(now - HOST_NOTIFY_INTERVAL + Duration::from_millis(1));
        assert!(host_notify_bytes("again", recent, now, HOST_NOTIFY_INTERVAL, cap).is_none());
        // Past it: emitted again.
        let old = Some(now - HOST_NOTIFY_INTERVAL - Duration::from_millis(1));
        assert!(host_notify_bytes("again", old, now, HOST_NOTIFY_INTERVAL, cap).is_some());
        // The first notification of a pane's life is never rate-limited.
        assert!(host_notify_bytes("first", None, now, HOST_NOTIFY_INTERVAL, cap).is_some());
    }

    /// P2/P3 share the sanitizer; pin that it keeps ordinary text intact.
    #[test]
    fn sanitize_keeps_printable_text_verbatim() {
        assert_eq!(sanitize_for_host("run `ls -la`? (y/n)", 100), "run `ls -la`? (y/n)");
        assert_eq!(sanitize_for_host("a\tb\nc", 100), "abc");
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
