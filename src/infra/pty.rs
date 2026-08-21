//! Production `PaneBackend`: a real PTY child + vt100 terminal state.
//! This is the only module that touches portable-pty. Killing a PtyPane
//! loses nothing precious — the agent's session file is the ground truth,
//! and the adapter knows how to resume it.

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender, SyncSender};
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

/// How long a pane is given to act on SIGHUP before the guaranteed SIGKILL.
///
/// Long enough for an agent to flush its final turn to its session file —
/// the whole reason `hangup` exists — and short enough that closing a pane
/// still feels like a keystroke. Shared with `App::kill_fleet`, which spends
/// it **once** for the whole fleet rather than once per pane.
pub const HANGUP_GRACE: Duration = Duration::from_millis(200);

/// How long `kill` will wait for a SIGKILLed child to become reapable before
/// leaving the zombie for process exit to collect.
///
/// A wall-clock deadline, not a spin count: `sleep(1ms)` on macOS returns
/// after 3–4 ms, so the previous `for _ in 0..100 { sleep(1ms) }` was a
/// documented "~100 ms cap" that measured 380 ms — per pane, serially, on
/// the event loop.
///
/// And short, because the long version bought nothing. A SIGKILLed child
/// that is going to be reapable promptly is so within a step or two; one
/// that is not — a pane blocked writing to a pty roost has stopped draining
/// — was measured still unreapable after 380 ms, so waiting longer only
/// makes quit slower without collecting anything. The zombie it leaves is
/// harmless and goes when roost's own process does.
const REAP_BUDGET: Duration = Duration::from_millis(20);

/// Polling step for both waits above.
const REAP_STEP: Duration = Duration::from_millis(2);

/// How many bytes of input may be waiting for one pane's child before roost
/// refuses more.
///
/// **Why a queue exists at all.** A write to a PTY master blocks whenever the
/// kernel's tty input queue is full and the child isn't draining it — the
/// child may be paused (SIGSTOP), stuck in a foreground command that never
/// reads stdin, or a line editor mid-redraw, and none of it is guaranteed to
/// end. Done on the event loop, that write takes the whole process down: no
/// render, no keystrokes, no control requests, no recovery short of killing
/// the process — see `tests/send_backpressure.rs`. So the write moves to a
/// per-pane thread and the event loop only ever enqueues, never blocking for
/// any amount of time.
///
/// The queue is **bounded** because the child genuinely may never drain, and
/// an unbounded one would trade a freeze for an OOM: past this much pending
/// input `write_input_raw` reports `false`, which is already the codebase's
/// "this pane did not take your input" signal (`ctl_send` turns it into an
/// error reply rather than a dishonest `sent` count).
///
/// **A byte budget, deliberately not a message count.** Every keystroke is its
/// own `write_input` call (`App::forward_bytes`, one per key event), so a
/// message-counted queue is bounded by *typing speed*, not by memory — a
/// fixed slot count would shed the tail of any command longer than that many
/// characters, silently corrupting it mid-line. Bytes are what the bound is
/// actually protecting, and 1 MiB of them is unreachable by any real burst —
/// a typed command is ~100 bytes, a paste or a `roost send` at the control
/// plane's own cap is 64 KiB — while still capping a wedged pane at a
/// megabyte.
const WRITE_QUEUE_BYTES: usize = 1024 * 1024;

/// P2: at most one OSC 9 re-emission per pane per this window. An agent that
/// notifies in a loop (or a `cat` of a log full of them) must not turn the
/// host terminal into a notification firehose the user has to force-quit.
const HOST_NOTIFY_INTERVAL: Duration = Duration::from_secs(1);

/// P2: how much of a notification body is re-emitted to the host. Long
/// enough for a real "needs your approval to run X" line, short enough that
/// a pane can't push a megabyte through roost's own stdout.
const HOST_NOTIFY_CAP: usize = 200;

/// P3: largest OSC 52 payload roost relays to the host, in base64
/// characters (~75 KB of decoded text). Generous for any real "copy this
/// file/diff" action; bounded so a pane can't push arbitrary volume through
/// roost's own stdout. tmux caps the same path for the same reason.
const OSC52_PAYLOAD_CAP: usize = 100_000;

/// P2 (this is the OSC 52 side of the same problem): at most one clipboard
/// relay per pane per this window. `OSC52_PAYLOAD_CAP` bounds one write, not
/// how often one arrives — a measured shell loop relayed 5000 sequences
/// (129 KB) to the operator's real system clipboard with nothing to stop it.
/// A real "copy this file/diff" action happens at human cadence, seconds
/// apart at most; one relay per second is silent for that case and caps the
/// abuse case at the same rate OSC 9 already caps notifications
/// (`HOST_NOTIFY_INTERVAL`) — a separate constant because the two are
/// unrelated features that just happen to want the same cadence, not
/// because they must move together.
const OSC52_INTERVAL: Duration = Duration::from_secs(1);

/// P3: the selection targets xterm defines for OSC 52 — clipboard, primary,
/// secondary, select, and cut-buffers 0–7. Anything outside this set is not
/// a selection roost will name in a sequence it writes to its own terminal.
const OSC52_SELECTIONS: &str = "cpqs01234567";

/// P2/P3: make text safe to embed in a sequence roost writes to its OWN
/// terminal. A pane's payload is untrusted: left alone, an embedded ESC/BEL/
/// ST could close roost's sequence early and have the rest interpreted as
/// host commands (a pane repainting the user's real terminal). C0 controls
/// are dropped outright and the result truncated to `cap` characters —
/// truncation is on char boundaries, so a multi-byte glyph is never split.
fn sanitize_for_host(text: &str, cap: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(cap).collect()
}

/// D5: parse an agent-published terminal title into a status. Claude Code
/// sets `⠧ <task>` (a braille spinner frame, U+2800–U+28FF) while a turn is
/// running and `✳ <name>` once at rest — machine-readable state on a channel
/// that already crosses the PTY, and the only exact-ish signal a claude pane
/// has between its one-shot hook connections. Titles matching neither shape
/// (a shell's cwd title, some other TUI's) are `None`: no signal, never
/// "rest". The second char must be a space (or absent) so an ordinary title
/// that merely *starts* with one of these glyphs doesn't read as state.
fn agent_title_status(title: &str) -> Option<AgentStatus> {
    let mut chars = title.chars();
    let first = chars.next()?;
    if !matches!(chars.next(), None | Some(' ')) {
        return None;
    }
    if ('\u{2800}'..='\u{28FF}').contains(&first) {
        Some(AgentStatus::Working)
    } else if first == '✳' {
        Some(AgentStatus::Waiting)
    } else {
        None
    }
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

/// ux P1-6: the raw BEL roost relays to its own terminal for the bell
/// *heuristic* — the fallback attention path that has no OSC 9 text to
/// relay, only the pane's own bell (already consumed by the vt100 parser,
/// see `process_output`) to echo. `None` when the last relay was too
/// recent (`interval`) — shares `queue_host_notify`'s own gate, so a pane
/// alternating BEL and OSC 9 in a loop still gets one relay per window, not
/// two independent budgets to burn through.
fn host_bell_bytes(last: Option<Instant>, now: Instant, interval: Duration) -> Option<Vec<u8>> {
    if last.is_some_and(|t| now.duration_since(t) < interval) {
        return None;
    }
    Some(vec![0x07])
}

/// P3: the OSC 52 roost relays to its own terminal for a pane's clipboard
/// write, or `None` when the write must be dropped.
///
/// Three gates, all about roost's terminal/host rather than the pane's intent:
/// * rate — at most one relay per pane per `interval`, mirroring OSC 9's
///   `queue_host_notify`/`host_notify_bytes` gate: a runaway pane must not
///   turn every clipboard write it makes into a write to the operator's real
///   system clipboard;
/// * the payload must be *actual base64* (`A–Z a–z 0–9 + / =`) and within
///   `cap` — a payload carrying ESC/BEL would close roost's own sequence
///   early and have its tail read as host commands, and an unbounded one is
///   a pane pushing arbitrary volume through roost's stdout;
/// * the selection must name xterm's real targets, for the same reason.
///
/// A rejected write is dropped in silence: the pane already believes it
/// copied (the lie SPEC-ux U14 documents), and roost has no channel to
/// correct it — a rejected relay is logged at the parser boundary, and the
/// dropped case is only reachable by a payload that isn't a clipboard write
/// in the first place (or arrived too soon after the last one).
///
/// **Not gated by `ROOST_TEST_NO_HOST_IO`** (B2 round 2, PR #46 review's
/// runtime hatch for `infra::clipboard::copy`/`infra::open::open_url`) —
/// deliberately a different question. This function only produces bytes
/// roost queues for its *own* stdout (`host_writes`, flushed once per
/// frame in `main.rs`); where those bytes actually land depends entirely
/// on what roost's stdout is connected to — the operator's real terminal
/// in normal use, or a PTY-harness test's own private pty when spawned
/// under `tests/`, which is exactly what `tests/pane_clipboard.rs`
/// exercises and asserts on (the relay reaching "the host" is the test).
/// Gating this with the same var would silently break that test's own
/// premise for no safety gain: unlike `native_copy`'s `pbcopy` subprocess,
/// this never reaches the *system* clipboard service directly — only
/// through whatever terminal (real or harness-contained) is already
/// listening on the other end of roost's own output.
fn host_clipboard_bytes(
    selection: &str,
    payload_base64: &str,
    cap: usize,
    last: Option<Instant>,
    now: Instant,
    interval: Duration,
) -> Option<Vec<u8>> {
    if last.is_some_and(|t| now.duration_since(t) < interval) {
        return None;
    }
    if payload_base64.len() > cap {
        return None;
    }
    if !payload_base64.bytes().all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b)) {
        return None;
    }
    let sel: String = selection.chars().filter(|c| OSC52_SELECTIONS.contains(*c)).collect();
    if sel.len() != selection.chars().count() {
        return None; // a selection field carrying anything else
    }
    let mut out = Vec::with_capacity(payload_base64.len() + sel.len() + 6);
    out.extend_from_slice(b"\x1b]52;");
    out.extend_from_slice(sel.as_bytes());
    out.push(b';');
    out.extend_from_slice(payload_base64.as_bytes());
    out.push(0x07);
    Some(out)
}

/// P1: how long an open synchronized-output bracket (mode 2026) may keep the
/// pre-bracket frame on screen. Real brackets close within a frame or two; a
/// stuck one — an app killed mid-redraw, a bug that never sends `?2026l` —
/// must never freeze the pane, so past the cap the live grid is presented
/// again. Torn beats frozen.
const SYNC_STALE_CAP_DEFAULT: Duration = Duration::from_millis(150);

/// The shipped cap, overridable via `ROOST_SYNC_CAP_MS`. Not user config: it
/// exists so the end-to-end gate (tests/pane_sync_output.rs) can hold the cap
/// out of reach on a loaded CI runner, where a stretched in-bracket sleep
/// blowing past 150 ms turns the honest expiry path into a false torn-frame
/// failure. Unit tests pin expiry itself through `sync_presented`'s cap
/// parameter; this override is only for the spawned-binary test.
fn sync_stale_cap() -> Duration {
    static CAP: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *CAP.get_or_init(|| {
        std::env::var("ROOST_SYNC_CAP_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(Duration::from_millis)
            .unwrap_or(SYNC_STALE_CAP_DEFAULT)
    })
}

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

/// C29 (selection-freeze amendment, DESIGN-ui.md): the longest a native-
/// selection gesture's frozen frame may outlive its own `Down` before
/// `presented()` stops honoring it and falls back to live. Bounds a lost
/// `Up` — the button released outside the window, focus moved to another
/// application, the terminal simply never delivered the event — so a
/// vanished gesture cannot freeze a pane forever; torn beats frozen here
/// too (same principle as `SYNC_STALE_CAP_DEFAULT`, a much longer cap
/// because this one bounds a human drag rather than a redraw — the
/// ordinary case, `Up` or a wheel tick or a fresh `Down`, clears the freeze
/// long before this ever matters).
const GESTURE_FREEZE_STALE_CAP: Duration = Duration::from_secs(30);

/// C29: does the gesture-freeze snapshot win presentation right now? `None`
/// means "no — fall through to the ordinary `sync_presented` rule": either
/// no gesture is in progress, the frame has outlived `cap` (see
/// `GESTURE_FREEZE_STALE_CAP`), or the pane is scrolled into its own
/// history, where a snapshot cannot follow (`Screen::snapshot` always
/// resets to the live tail — same reason `sync_presented` itself defers to
/// live there; a scrolled-back drag also has nothing moving under it to
/// protect against, since banked rows are already immutable). Pure (the
/// cap is a parameter), so the staleness cap is unit-tested without
/// sleeping for real — the same shape as `sync_presented`'s own check.
fn gesture_presented<'a>(
    live: &vt100::Screen,
    freeze: Option<&'a (vt100::Screen, Instant)>,
    cap: Duration,
) -> Option<&'a vt100::Screen> {
    let (snap, at) = freeze?;
    (live.scrollback() == 0 && at.elapsed() < cap).then_some(snap)
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

/// L3: the vars that hand a child a *ready-made* credential for this roost —
/// ROOST_SOCK (targeting) and both credential tiers, `ROOST_CONTROL_TOKEN`
/// (checked *first* by `resolve_token`, and it's the fleet-wide one:
/// unscoped, unlike a pane's own `ROOST_TOKEN`) and `ROOST_TOKEN` itself.
///
/// `ROOST_STATE` is deliberately **not** here, and the distinction is not
/// "config vs authority" — it is targeting too: `socket_path` reads it first
/// (`sock.rs`), and `resolve_token`'s file fallback resolves through it to
/// `$ROOST_STATE/control.token` (`store.rs`). It is left alone because
/// scrubbing it would obscure a path, not remove a capability: under this
/// threat model the pane is same-uid and can compute the default state dir
/// itself, so a token it could already read is not protected by hiding the
/// variable that names it. The genuinely inert ones — `ROOST_NO_EXT_INSTALL`,
/// `ROOST_DEBUG`, `ROOST_PANE` — are left alone because they carry no
/// authority at all.
///
/// Keep that split honest when `resolve_token` changes: anything that becomes
/// a *carried* credential belongs in this list.
const CONTROL_ENV_VARS: &[&str] = &["ROOST_SOCK", "ROOST_CONTROL_TOKEN", "ROOST_TOKEN"];

/// L3: drop roost's own control-plane credentials from the base env a pane
/// child would otherwise inherit. Kept separate from `scrub_host_identity`
/// on purpose — that one's contract is explicitly that TERM/ROOST_* vars
/// are `spawn`'s to own, unchanged; this is `spawn` exercising that
/// ownership.
fn scrub_control_env(cmd: &mut CommandBuilder) {
    for var in CONTROL_ENV_VARS {
        cmd.env_remove(var);
    }
}

/// Every live pid in session `sid`, **excluding the leader itself** (that one
/// is already covered by the process-group kill).
///
/// A pane is spawned through `setsid`, so its pid *is* its session id, and
/// everything it goes on to spawn inherits that session — job control moves a
/// process between *groups*, never out of the session. That makes "same
/// session" the one net wide enough to catch a backgrounded job, which is in
/// neither the pane's process group nor the terminal's foreground group.
///
/// Linux reads `/proc` directly and macOS asks the kernel
/// (`proc_listpids` + `getsid`): no subprocess in a teardown path, and no
/// dependency on an external binary. Only a third platform still shells out
/// to `pgrep -s`. Either way a failure yields an empty list — the group kill
/// above still happened, and a sweep that cannot enumerate must not block
/// the quit.
///
/// **macOS used to shell out here too, and it never once worked**: BSD
/// `pgrep` has a `-s`, but Apple's does not (`pgrep: illegal option -- s`,
/// exit 2, nothing on stdout), so the sweep silently enumerated nothing on
/// the one platform roost is developed on. Every backgrounded job outlived
/// its pane — closing it, respawning over it, and quitting roost entirely —
/// which is what `tests/orphan_after_exit.rs` was failing on.
fn session_members(sid: u32) -> Vec<u32> {
    // A sid of 0 or 1 is not a pane's session and never can be: `setsid`
    // makes the pane's own child the leader, so the sid is its pid. Were one
    // to arrive anyway (a `process_id()` that answered 1, a future caller
    // passing a default), the scans below would happily return *init's whole
    // session* — i.e. the machine — and every caller here follows this list
    // straight into `SIGKILL`. Refuse at the one place they all route
    // through rather than at each of them.
    if sid <= 1 {
        return Vec::new();
    }
    #[cfg(target_os = "linux")]
    {
        let Ok(entries) = std::fs::read_dir("/proc") else { return Vec::new() };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            if pid == sid {
                continue;
            }
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else { continue };
            // Fields are space-separated, but field 2 is the executable name
            // in parentheses and may itself contain spaces *and* ')' — so the
            // only safe split point is the LAST ')'. After it: state(3),
            // ppid(4), pgrp(5), session(6).
            let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r) else { continue };
            let mut fields = rest.split_whitespace();
            let session = fields.nth(3).and_then(|s| s.parse::<u32>().ok());
            if session == Some(sid) {
                out.push(pid);
            }
        }
        out
    }
    #[cfg(target_os = "macos")]
    {
        // `getsid(2)` answers the session question for any pid we can see,
        // dead leader or not — so the net is: every live pid, keep the ones
        // whose session is ours. ~1900 pids on a busy Mac, one cheap syscall
        // each, and only on a pane teardown.
        all_pids()
            .into_iter()
            .filter(|p| *p != sid)
            // SAFETY: getsid(2) with a plain pid; -1 on a pid that vanished
            // between the listing and here, which simply doesn't match.
            .filter(|p| unsafe { libc::getsid(*p as libc::pid_t) } == sid as libc::pid_t)
            .collect()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let Ok(out) = std::process::Command::new("pgrep").arg("-s").arg(sid.to_string()).output()
        else {
            return Vec::new();
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.trim().parse::<u32>().ok())
            .filter(|p| *p != sid)
            .collect()
    }
}

/// Every pid on the machine, via libproc. Empty on any failure — a sweep
/// that cannot enumerate must not block the quit.
#[cfg(target_os = "macos")]
fn all_pids() -> Vec<u32> {
    /// `PROC_ALL_PIDS` from `<libproc.h>`; the libc crate binds the function
    /// but not this selector.
    const PROC_ALL_PIDS: u32 = 1;
    // A NULL buffer asks for the size, in bytes, the kernel would fill.
    // SAFETY: the documented sizing call.
    let want = unsafe { libc::proc_listpids(PROC_ALL_PIDS, 0, std::ptr::null_mut(), 0) };
    if want <= 0 {
        return Vec::new();
    }
    // Slack for processes that start between the sizing call and the fill.
    let mut buf = vec![0i32; want as usize / std::mem::size_of::<i32>() + 64];
    // SAFETY: out-buffer of exactly the byte length we declare.
    let got = unsafe {
        libc::proc_listpids(
            PROC_ALL_PIDS,
            0,
            buf.as_mut_ptr() as *mut std::ffi::c_void,
            std::mem::size_of_val(buf.as_slice()) as libc::c_int,
        )
    };
    if got <= 0 {
        return Vec::new();
    }
    // This call reports *bytes* written (unlike `proc_listchildpids`, which
    // reports a count) — but the `> 0` filter makes either reading safe,
    // since an over-read only ever reaches the zeroed tail.
    let n = (got as usize / std::mem::size_of::<i32>()).min(buf.len());
    buf[..n].iter().filter(|p| **p > 0).map(|p| *p as u32).collect()
}

pub struct PtyPane {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    /// Input queued for the child, drained by this pane's writer thread. The
    /// event loop must never touch the PTY writer directly — see
    /// `WRITE_QUEUE_BYTES`. Unbounded as a channel; the bound that matters is
    /// `queued` below, so enqueuing can never block the loop even for an
    /// instant.
    writes: Sender<Vec<u8>>,
    /// Bytes currently sitting in `writes` — added on enqueue, subtracted by
    /// the writer thread once actually written. This is the real queue bound
    /// (`WRITE_QUEUE_BYTES`).
    queued: Arc<AtomicUsize>,
    /// Set by the writer thread once a write to the child has actually failed
    /// (broken pipe: it exited). Keeps `write_input_raw`'s "did this pane take
    /// your input" answer honest now that the write itself happens off-thread.
    write_failed: Arc<AtomicBool>,
    parser: vt100::Parser,
    status: StatusTracker,
    /// D5: the pane's terminal title as of the last processed chunk, so
    /// `process_output` pushes `StatusTracker::set_title_status` only when
    /// the title *string* changed — for an animating spinner that's every
    /// frame, which is exactly what makes the tracker's `title_at` a
    /// liveness heartbeat (a frozen spinner stops refreshing it).
    last_title: String,
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
    /// bracket opened (for `SYNC_STALE_CAP_DEFAULT`). This is what
    /// `screen()` and `grab_text` present by default: the app asked for its
    /// in-progress redraw not to be shown.
    ///
    /// Deliberately NOT consulted by scroll state (`scroll_offset`/
    /// `scroll_total`/`set_scrollback`/`scroll_by`), full-history reads
    /// (`grab_all_text`), or the input-mode accessors: those answer for the
    /// live grid, which stays the single source of truth. A ≤150 ms
    /// presentation veneer over the visible frame, not a second terminal
    /// state — `gesture_freeze` below is a second tenant of `presented()`
    /// that wins ahead of this one and can legitimately outlive it by 200×
    /// (`GESTURE_FREEZE_STALE_CAP` vs `SYNC_STALE_CAP_DEFAULT`). `roost
    /// read`'s screen mode reads through this veneer via `read_screen_text`
    /// but deliberately skips `gesture_freeze` — a control client polling a
    /// pane is not the human dragging a mouse over it.
    sync_view: Option<(vt100::Screen, Instant)>,
    /// C29 (selection-freeze amendment): the frame `presented()` was
    /// showing at a native-selection gesture's `Down`, held through `Up` —
    /// `PaneBackend::freeze_view`/`unfreeze_view`, driven from
    /// `main.rs::handle_native_selection`. `None` outside a gesture.
    /// Same split as `sync_view` above: never consulted by scroll state,
    /// full-history reads, or the input-mode accessors — only the
    /// presentation surface freezes. Stale-capped independently
    /// (`GESTURE_FREEZE_STALE_CAP`, not `SYNC_STALE_CAP`): a lost `Up` must
    /// not freeze the pane forever, but a real drag runs far longer than a
    /// redraw.
    gesture_freeze: Option<(vt100::Screen, Instant)>,
    /// W3: effects routed out of the pane's escape stream and waiting for
    /// the core to drain them (`take_effects`).
    effects: PaneEffects,
    /// P2: when this pane last re-emitted an OSC 9 to the host, for
    /// `HOST_NOTIFY_INTERVAL`. `None` until the first one.
    last_host_notify: Option<Instant>,
    /// P2: when this pane's OSC 9 last reached the **desktop** channel —
    /// `PaneEffects::notifications`, which the composition root turns into a
    /// host bell plus an `osascript`. Same `HOST_NOTIFY_INTERVAL`, its own
    /// clock; see `route_effect` for why it is not `last_host_notify`.
    last_desktop_notify: Option<Instant>,
    /// P2/P3: when this pane last relayed an OSC 52 clipboard write to the
    /// host, for `OSC52_INTERVAL`. `None` until the first one.
    last_host_clipboard: Option<Instant>,
    /// P7: the cursor shape this pane asked for via DECSCUSR (`CSI Ps SP q`),
    /// 1..=6; `None` when it wants roost's own default. Mirrored to the host
    /// only while the pane is focused — one terminal, one cursor.
    cursor_shape: Option<u8>,
    /// Per-spawn liveness flag shared with the reader thread. `kill()` clears
    /// it so the (now-doomed) reader stops emitting Output/Exit for this pane
    /// id. Without this, a pane id that is reused (close→new) or respawned
    /// (relaunch) could receive a stale `Exit` from the *old* child's reader
    /// and be flipped straight back to "dead", or get old bytes rendered into
    /// the new pane.
    alive: Arc<AtomicBool>,
    /// Has roost already sent this pane SIGHUP and waited for it? Set by
    /// `hangup`, read by `kill` so the grace is spent exactly once: the
    /// shutdown path hangs the whole fleet up and waits once
    /// (`App::kill_fleet`), while the close/respawn path arrives at `kill`
    /// cold and has to give the same chance itself.
    hung_up: bool,
}

impl PtyPane {
    /// P1: the screen roost *presents* for this pane — the last complete
    /// frame while a synchronized-output bracket is open (and fresh), else
    /// the live grid. Every surface that shows the user what the pane looks
    /// like right now goes through here: `screen()` (blit + host cursor) and
    /// `grab_text` (copy-mode selection, native selection — the surface P1
    /// measured 31/50 torn samples on). History and state surfaces
    /// deliberately do not; see `sync_view`.
    ///
    /// C29 (selection-freeze amendment): a native-selection gesture's own
    /// frozen frame (`gesture_freeze`) takes priority over the sync-output
    /// veneer — one presentation path, checked in one place, so `screen()`
    /// and `grab_text` can never disagree about what the user is looking
    /// at. Falls through to `presented_live` (the unchanged `sync_presented`
    /// rule) the instant there is no gesture, or it has gone stale, or the
    /// pane is scrolled into history (`gesture_presented`'s own doc).
    fn presented(&self) -> &vt100::Screen {
        let live = self.parser.screen();
        gesture_presented(live, self.gesture_freeze.as_ref(), GESTURE_FREEZE_STALE_CAP)
            .unwrap_or_else(|| self.presented_live())
    }

    /// SPEC-parity P1 gap (selection-freeze amendment): `presented()` minus
    /// the gesture freeze — the sync-output veneer still applies (a torn
    /// mid-redraw frame is wrong for every consumer), but a native-selection
    /// gesture's frozen frame never does. Backs `read_screen_text`: `roost
    /// read` is a control client, not the human whose mouse drag this is
    /// protecting, and must see live content while one is in progress.
    fn presented_live(&self) -> &vt100::Screen {
        sync_presented(self.parser.screen(), self.sync_view.as_ref(), sync_stale_cap())
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
                // P2: at most one *desktop* notification per pane per
                // `HOST_NOTIFY_INTERVAL` — the identical limit
                // `queue_host_notify` right below already applies to the
                // host relay, on the identical reasoning, for the identical
                // event. It was missing here, and this channel is the
                // expensive one: every notification that reaches the
                // composition root rings the host bell and (on macOS) forks
                // an `osascript`. Measured before this gate: 500 OSC 9
                // sequences in a second produced 500 desktop notifications
                // while the relay beside it produced one — i.e. a `for i in
                // $(seq 1 10000); do printf '\e]9;hi\a'; done` in any pane
                // forked ten thousand processes.
                //
                // Its own clock rather than `last_host_notify`: that one is
                // shared with the audible-bell relay, so borrowing it would
                // let a bell silence a genuine notification (and a
                // notification whose body sanitizes away would leave the
                // clock unstamped, which is exactly the input an abuser
                // picks). Same interval, independently enforced.
                let now = Instant::now();
                let quiet = self
                    .last_desktop_notify
                    .is_none_or(|t| now.duration_since(t) >= HOST_NOTIFY_INTERVAL);
                if quiet {
                    self.last_desktop_notify = Some(now);
                    let text = match &title {
                        Some(t) if !t.is_empty() => format!("{t}: {body}"),
                        _ => body.clone(),
                    };
                    self.effects.notifications.push(text);
                }
                self.queue_host_notify(&body);
            }
            // P3: an app inside the pane set the clipboard. roost's own copy
            // mode already proves the host OSC 52 path works; forward the
            // pane's write down it so "copied" stops being a lie for inner
            // apps too (the other half of SPEC-ux U14). Reads never arrive
            // here — the parser refuses to surface them at all.
            vt100::Effect::Osc52Write { selection, payload_base64 } => {
                self.queue_host_clipboard(&selection, &payload_base64);
            }
            // P7: DECSCUSR. Remembered per pane; only the *focused* pane's
            // shape is mirrored to the host, by the composition root. Shape
            // 0 is the explicit "back to the terminal default", which is
            // exactly "this pane asks for nothing" — and so is any shape
            // outside the range xterm defines.
            vt100::Effect::CursorShape(shape) => {
                self.cursor_shape = (1..=6).contains(&shape).then_some(shape);
            }
        }
    }

    /// P2: re-emit the notification to the HOST terminal as an OSC 9, so the
    /// native desktop notification the app asked for actually fires. Rate-
    /// limited per pane and length-capped; the body is sanitized because it
    /// is untrusted text about to ride inside a sequence roost writes to its
    /// own terminal.
    fn queue_host_notify(&mut self, body: &str) {
        let now = Instant::now();
        let Some(bytes) = host_notify_bytes(
            body,
            self.last_host_notify,
            now,
            HOST_NOTIFY_INTERVAL,
            HOST_NOTIFY_CAP,
        ) else {
            return;
        };
        self.last_host_notify = Some(now);
        self.effects.host_writes.extend_from_slice(&bytes);
    }

    /// ux P1-6: ring the HOST terminal's own bell for the fallback attention
    /// path — see `host_bell_bytes`. Shares `last_host_notify`/
    /// `HOST_NOTIFY_INTERVAL` with `queue_host_notify` rather than a gate of
    /// its own: both exist to stop a pane from machine-gunning the
    /// operator's real terminal, so one per-pane "how often may this pane
    /// ping the host" budget for the two of them is the simpler rule, not a
    /// weaker one.
    fn queue_host_bell(&mut self) {
        let now = Instant::now();
        let Some(bytes) = host_bell_bytes(self.last_host_notify, now, HOST_NOTIFY_INTERVAL) else {
            return;
        };
        self.last_host_notify = Some(now);
        self.effects.host_writes.extend_from_slice(&bytes);
    }

    /// P2/P3: relay a pane's clipboard write to the host, rate-gated the
    /// same shape as `queue_host_notify` — the timestamp only advances on an
    /// actual emission, so a tight loop still gets one relay per interval
    /// instead of resetting its own clock and being starved indefinitely.
    fn queue_host_clipboard(&mut self, selection: &str, payload_base64: &str) {
        let now = Instant::now();
        let Some(bytes) = host_clipboard_bytes(
            selection,
            payload_base64,
            OSC52_PAYLOAD_CAP,
            self.last_host_clipboard,
            now,
            OSC52_INTERVAL,
        ) else {
            return;
        };
        self.last_host_clipboard = Some(now);
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
        // L3: a `CommandBuilder` otherwise inherits *this* process's own
        // environment. Two ways that bites: if roost's own control listener
        // never bound (main.rs's listener setup is `.ok()`), `spec.env`
        // below carries no ROOST_SOCK/ROOST_TOKEN, and without this the
        // child would silently inherit roost's own live ones instead of
        // simply having none — worst case a roost nested inside a roost
        // pane, handing its children the *outer* pane's socket and token.
        // And separately, ROOST_CONTROL_TOKEN (cli.rs's fleet-wide, checked-
        // first credential) is never in `spec.env` at all — it only ever
        // reaches a pane by inheritance, from an operator's shell or an
        // outer roost, so it must be scrubbed unconditionally too. Removed
        // here, before `spec.env` conditionally re-sets the two it does own.
        scrub_control_env(&mut cmd);
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }

        let child =
            pair.slave.spawn_command(cmd).with_context(|| format!("spawning {}", spec.program))?;
        let pid = child.process_id();
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().context("clone pty reader")?;
        let mut writer = pair.master.take_writer().context("take pty writer")?;

        let alive = Arc::new(AtomicBool::new(true));
        let reader_alive = alive.clone();

        // The write half gets its own thread for the reason `PENDING_WRITES`
        // documents: `write_all` here can block forever on a child that isn't
        // reading, and on the event loop that is a whole-process deadlock.
        // Single thread, single queue, so input still reaches the child in the
        // exact order it was produced.
        let (writes, pending) = channel::<Vec<u8>>();
        let queued = Arc::new(AtomicUsize::new(0));
        let write_failed = Arc::new(AtomicBool::new(false));
        let writer_queued = queued.clone();
        let writer_failed = write_failed.clone();
        let writer_alive = alive.clone();
        std::thread::spawn(move || {
            // This thread carries the user's typed bytes to the child —
            // promoted so a machine saturated by the fleet's own agents
            // still delivers keystrokes promptly (infra::qos module doc).
            crate::infra::qos::promote_input_delivery_thread();
            // Ends when the pane drops its sender, or when this spawn is
            // killed — a write blocked on a doomed child unblocks by itself
            // once the child dies and the slave side closes.
            for chunk in pending {
                if !writer_alive.load(Ordering::Relaxed) {
                    break;
                }
                let wrote = writer.write_all(&chunk).is_ok();
                // Released only once the bytes are actually gone, so the
                // budget tracks memory really held, not merely accepted.
                writer_queued.fetch_sub(chunk.len(), Ordering::Relaxed);
                if !wrote {
                    writer_failed.store(true, Ordering::Relaxed);
                    break;
                }
                let _ = writer.flush();
            }
        });
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
            writes,
            queued,
            write_failed,
            parser: vt100::Parser::new(rows, cols, SCROLLBACK_LINES),
            status: StatusTracker::new(),
            last_title: String::new(),
            pid,
            queries: QueryResponder::new(),
            pixels,
            sync_view: None,
            gesture_freeze: None,
            effects: PaneEffects::default(),
            last_host_notify: None,
            last_desktop_notify: None,
            last_host_clipboard: None,
            cursor_shape: None,
            alive,
            hung_up: false,
        })
    }

    fn process_output(&mut self, bytes: &[u8]) {
        // vt100 counts *parsed* bells, so a 0x07 consumed as an OSC string
        // terminator (ESC ] … BEL) doesn't count — only a real bell does.
        let bells_before = self.parser.screen().audible_bell_count();
        self.parser.process(bytes);
        if self.parser.screen().audible_bell_count() != bells_before {
            self.status.on_bell();
            // ux P1-6: relay only when no *live* extension report is
            // covering this pane (`recently_reported()`) — an adapter/TUI
            // genuinely ringing for a "needs you" its own hook can't see,
            // not pi (which never emits an audible bell at all — see
            // `StatusTracker::bell_after_ext`). Checked here rather than
            // against `current()` (whose quiet-window grace would mask most
            // real cases) or `vouched_live` (which trusts a stale live link
            // past the point `current()` itself stops trusting it, and
            // would swallow a real audible bell with no compensating
            // signal).
            if !self.status.recently_reported() {
                self.queue_host_bell();
            }
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
        // D5: the pane's terminal title doubles as a status channel — see
        // `agent_title_status`. A &str compare per chunk; pushed on
        // title-string change only (every frame for an animating spinner —
        // the tracker reads that as a liveness heartbeat).
        if self.parser.screen().title() != self.last_title {
            let title = self.parser.screen().title().to_string();
            let parsed = agent_title_status(&title);
            self.last_title = title;
            self.status.set_title_status(parsed);
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

    /// P9: vt100 already tracks modes 1049/47, so this is a read, not new
    /// state. Answered from the LIVE screen rather than the presented frame,
    /// like every other input-routing accessor here: a ≤150 ms presentation
    /// veneer (P1) must never decide where a keystroke or a wheel tick goes.
    fn alternate_screen(&self) -> bool {
        self.parser.screen().alternate_screen()
    }

    /// P10: read off the *live* grid, not the presented frame, for exactly
    /// the reason above — a focus subscription is input-protocol state the
    /// app owns right now, and a stale picture of the screen could deny a
    /// pane the reports it just asked for.
    fn focus_events(&self) -> bool {
        self.parser.screen().focus_events()
    }

    fn write_input(&mut self, bytes: &[u8]) -> bool {
        // Typing means "I'm back" — snap to the live tail. [P14] Asked of
        // the grid, not of a roost-side counter: the grid auto-advances the
        // offset as new rows bank while the view is scrolled, so any cached
        // copy is a second answer to a question that has exactly one.
        if self.parser.screen().scrollback() != 0 {
            self.parser.set_scrollback(0);
        }
        self.write_input_raw(bytes)
    }

    /// Hand `bytes` to the pane's writer thread. Returns whether this pane
    /// accepted them for delivery — a caller that counts successful sends
    /// (`ctl_send`/`ctl_broadcast`) must not report a refusal as delivered.
    ///
    /// **Never blocks**, which is the whole point (`WRITE_QUEUE_BYTES`): the
    /// event loop calls this, and the only thing standing between a PTY write
    /// and an indefinite block is a child that chooses to read. `false` means
    /// one of the two honest failures — the child's pipe is already dead, or
    /// this pane already has a megabyte of input waiting, i.e. it has stopped
    /// consuming input entirely. Both are exactly what "pane did not accept
    /// input" is meant to say, and neither is reachable by a pane that is
    /// merely busy.
    fn write_input_raw(&mut self, bytes: &[u8]) -> bool {
        if self.write_failed.load(Ordering::Relaxed) {
            return false;
        }
        // Checked before adding, so a single chunk larger than the whole
        // budget still goes rather than being refused forever.
        if self.queued.load(Ordering::Relaxed) >= WRITE_QUEUE_BYTES {
            return false;
        }
        self.queued.fetch_add(bytes.len(), Ordering::Relaxed);
        if self.writes.send(bytes.to_vec()).is_err() {
            self.queued.fetch_sub(bytes.len(), Ordering::Relaxed);
            return false;
        }
        true
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
        // U3/U9 × P5: a resize reflows the live grid, which can bank rows the
        // rewrap pushed off the top — so the view's offset moves with them.
        // [P14] Nothing to re-read: every reader of the offset asks the grid,
        // so the reflowed value is already the answer they get.
        self.parser.set_size(rows, cols);
        // D2 (selection-freeze design audit): a resize mid-gesture leaves
        // the frozen frame at the OLD size while `grab_text` would go on
        // extracting from it against the NEW grid's rect/coordinates for
        // the rest of the gesture — wrong cells, silently. There is no
        // correct way to keep the freeze through a resize (the frame itself
        // is invalidated, not just stale), so drop it; `presented()` falls
        // back to live, exactly like a gesture that never started.
        self.gesture_freeze = None;
    }

    fn hangup(&mut self) {
        self.hung_up = true;
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
        // Give the child its chance to flush *only if nobody already has*.
        // `App::kill_fleet` hangs the whole fleet up and waits `HANGUP_GRACE`
        // once before it kills anything; the close/respawn path comes
        // straight here instead, and an agent closed with Alt+w deserves the
        // same window to write its final turn as one closed by quitting.
        //
        // Polled rather than slept flat, so the common case — a shell, which
        // dies on the hangup at once — costs a step and not the budget.
        if !self.hung_up {
            self.hangup();
            let deadline = Instant::now() + HANGUP_GRACE;
            while Instant::now() < deadline {
                if !matches!(self.child.try_wait(), Ok(None)) {
                    break;
                }
                std::thread::sleep(REAP_STEP);
            }
        }
        // SIGKILL the child directly rather than through `Child::kill`.
        //
        // portable-pty's `ChildKiller for std::process::Child` is not the
        // syscall its name suggests: it sends **SIGHUP**, then sleeps in
        // four 50 ms steps waiting for the process to take the hint, and
        // only then escalates to SIGKILL. That is a perfectly reasonable
        // default for a library — and it is the grace period roost has
        // already given, once, in `App::kill_fleet` (`hangup()` on every
        // pane, then one shared 200 ms wait). Paying it again, per pane and
        // serially on the event loop, made quit grow linearly with the
        // fleet: measured 1.3 s / 1.7 s / 2.5 s / 4.0 s for 1 / 2 / 4 / 8
        // busy panes, against DESIGN-ui §6's 2 s budget, and it is what
        // failed that gate on CI (run 32398616684, 2.66 s).
        //
        // This is also what this function's own doc has always claimed to
        // be — "the guaranteed SIGKILL", the escalation after `hangup()`'s
        // polite one. The hidden second grace was an accident of the
        // library, not a decision roost made.
        if let Some(pid) = self.pid {
            if pid > 1 {
                // Safety: kill(2) with a pid we own and a plain signal number.
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGKILL);
                }
            }
        }
        // The child is a session/process-group leader (portable-pty setsid's
        // it), so also SIGKILL the whole group — otherwise pi/claude's own
        // subprocesses linger as orphans, and a child that's blocked waiting on
        // one of them can itself fail to exit. Signalling -pgid (== -pid for a
        // leader) is a no-op if there's no such group.
        if let Some(pid) = self.pid {
            // P3: `process_id()` isn't documented to promise > 1, and
            // `-(pid)` as a killpg target would be 0 (roost's own process
            // group) if it were ever 0, or -1 — kill(2)'s "every process the
            // caller may signal", pid 1 being init — if it were ever 1.
            // Neither is "the pane's own process tree," so guard rather than
            // assume.
            if pid > 1 {
                // SAFETY: `kill(2)` with a plain signal number and a target
                // the `pid > 1` guard above has already established is a
                // real process group of ours and not 0/-1. No pointers.
                unsafe {
                    libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
                }
            }
            // ...and then sweep the whole **session**, because the group kill
            // above provably does not cover it. An interactive shell with job
            // control puts every job in a *new* process group, so a
            // backgrounded one (`sleep 600 &`) is in neither the pane's group
            // nor — being background — the foreground group the PTY's hangup
            // would reach. Measured before this sweep existed: the `sleep`
            // outlived roost's quit, reparented to init and still running.
            //
            // The session is the right net: `setsid` makes the pane its own
            // session leader (so its sid == this pid), a job-control fork
            // changes only the group, and nothing a pane spawns can leave the
            // session without asking for it. roost's own process is in the
            // terminal's session, never a pane's, so this cannot reach it —
            // and the `!= pid` guard keeps the leader on the group path above.
            //
            // Pid reuse between the scan and the signal is theoretically
            // possible; the window is microseconds and the group kill carries
            // the identical hazard, so it is accepted rather than papered over.
            for member in session_members(pid) {
                // SAFETY: as above — `kill(2)` on a plain pid, taken from
                // `session_members`, which never returns 0, 1 or the
                // leader. Worst case the pid is already gone and this is
                // ESRCH, which is why the return value is not checked.
                unsafe {
                    libc::kill(member as libc::pid_t, libc::SIGKILL);
                }
            }
        }
        // Reap WITHOUT blocking the UI thread indefinitely. A bare
        // `child.wait()` runs on the event-loop thread (close/quit), so if it
        // ever fails to return promptly — a wedged child, a PTY reaping edge
        // case — it freezes the *whole* app: no input, no render, no quit.
        // SIGKILL is normally reaped within a millisecond; poll `try_wait`
        // briefly (~100ms cap), then move on. A lingering zombie is harmless
        // (reaped when roost exits) and infinitely preferable to a frozen UI.
        let deadline = Instant::now() + REAP_BUDGET;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(None) => std::thread::sleep(REAP_STEP),
                _ => break, // reaped, or errored (already gone)
            }
        }
    }

    fn status(&self) -> AgentStatus {
        self.status.current()
    }

    fn status_reported(&self) -> bool {
        self.status.vouched_live()
    }

    fn set_extension_status(&mut self, s: AgentStatus) {
        self.status.set_extension_status(s);
    }

    fn set_ext_link(&mut self, up: bool) {
        self.status.set_ext_link(up);
    }

    fn set_title_signal(&mut self, enabled: bool) {
        self.status.set_title_signal(enabled);
    }

    fn on_exit(&mut self) {
        self.status.on_exit();
        // Sweep the session **first, and unconditionally** — before the reap
        // below, not inside it.
        //
        // This used to sit inside `if let Ok(Some(_)) = try_wait()`, i.e. it
        // only ran if the child had already become reapable by the time
        // roost got round to the EOF. It very often has not: the master
        // reaches EOF when the last *slave fd* closes, which happens while
        // the child is still exiting, and on a loaded machine the event loop
        // lands squarely inside that window. The sweep was then skipped
        // outright and everything the pane had backgrounded lived on —
        // exactly the failure the sweep was added for, just intermittent
        // instead of total. It made `tests/orphan_after_exit.rs` flaky on
        // CI (two failures in eleven runs of main) and is reproducible on
        // demand with a pane that `exec`s into a child holding no tty fd.
        //
        // Doing it here is also the *safer* half of the old branch, not a
        // relaxation. At this instant the pane's process either still exists
        // (as in that repro) or is a zombie roost has not yet reaped —
        // either way the pid is unambiguously still the pane's. Inside the
        // old branch the reap had just completed, which is the one moment
        // the OS is free to recycle it.
        //
        // EOF is roost's definition of a dead pane everywhere else (the
        // status flips to Exited, the exited bar appears), so a child that
        // closed its terminal and kept running is one roost has already
        // called dead; sweeping what it left behind is that same call,
        // applied consistently.
        if let Some(pid) = self.pid {
            for member in session_members(pid) {
                if member != pid {
                    // Safety: kill(2) with a pid from this pane's own
                    // session and a plain signal number.
                    unsafe {
                        libc::kill(member as libc::pid_t, libc::SIGKILL);
                    }
                }
            }
        }
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
            // The sweep above already ran; this branch now only lets go of
            // the pid.
            self.pid = None;
        }
    }

    fn take_effects(&mut self) -> PaneEffects {
        std::mem::take(&mut self.effects)
    }

    fn cursor_shape(&self) -> Option<u8> {
        self.cursor_shape
    }

    /// P1: the *presentation* view (see `PtyPane::presented`), so the
    /// renderer's blit and cursor placement can never show a frame the app
    /// declared incomplete. Transparent to the caller — the renderer asks
    /// for "the pane's screen" exactly as before.
    fn screen(&self) -> Option<&vt100::Screen> {
        Some(self.presented())
    }

    fn set_scrollback(&mut self, lines: usize) {
        // [P14] Nothing to mirror: `scroll_offset` reads the grid, so the
        // caller's ask never becomes state of its own. U9's overshoot — a
        // stored offset past the banked history that the view ignores, and
        // that burned ~240 keypresses before the screen moved — is not
        // representable now that there is nowhere to store it.
        self.parser.set_scrollback(lines);
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
    }

    /// [P14] The grid, and only the grid — no roost-side `scroll` counter
    /// that could drift from what the grid actually banked, so U3's honesty
    /// surfaces cannot report anything but the view.
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
    /// user sees, the exact surface P1 measured mid-bracket tearing on.
    /// Copy-mode and native-selection extraction only; `roost read`'s
    /// screen mode uses `read_screen_text` instead (SPEC-parity P1 gap,
    /// selection-freeze amendment) so a control client is never handed a
    /// mouse gesture's frozen frame.
    fn grab_text(&self, start: (u16, u16), end: (u16, u16)) -> String {
        extract_selection(self.presented(), start, end)
    }

    /// SPEC-parity P1 gap (selection-freeze amendment): `roost read`'s
    /// screen mode — `presented_live`, not `presented`, so a native-
    /// selection gesture in progress never reaches a control client.
    fn read_screen_text(&self, start: (u16, u16), end: (u16, u16)) -> String {
        extract_selection(self.presented_live(), start, end)
    }

    /// P1 (the other side of the split): the full history read stays on the
    /// live grid. The presentation snapshot carries no scrollback by design,
    /// and `read --full`/`--tail` is a question about the pane's whole
    /// recorded output, not about the frame currently on screen.
    fn grab_all_text(&self) -> String {
        self.parser.screen().all_contents()
    }

    /// U19: read the wrapped flag off the *presented* frame, for the same
    /// reason `grab_text` does — the join has to agree with the rows the
    /// user is looking at and clicking on, not with a grid mid-redraw.
    fn row_wrapped(&self, row: u16) -> bool {
        self.presented().row_wrapped(row)
    }

    /// C29 (selection-freeze amendment): snapshot whatever is *currently
    /// presented* — so a gesture that starts while a sync-output veneer is
    /// already up freezes that, not the half-drawn live grid underneath —
    /// via `Screen::snapshot`, the same scrollback-free copy `sync_view`
    /// itself is built with (`vendor/vt100/src/grid.rs:49-53`).
    fn freeze_view(&mut self) {
        self.gesture_freeze = Some((self.presented().snapshot(), Instant::now()));
    }

    fn unfreeze_view(&mut self) {
        self.gesture_freeze = None;
    }
}

/// Pull the text between two inclusive cell coords (row, col) from a vt100
/// screen, in reading order: from `start` to end-of-line, whole middle lines,
/// and start-of-line to `end`. Trailing spaces are trimmed per line and lines
/// joined with '\n'. `start`/`end` are normalized (either order accepted).
///
/// P15: wide-character continuation cells are skipped, not spaced. The right
/// half of a CJK/emoji glyph holds no contents of its own — the whole glyph
/// was already emitted with its left half — so filling it with a space is what
/// turned a selection of `日本語` into `"日 本 語"`, corrupting every yanked
/// path, identifier, and code fragment containing wide text. The whole-history
/// path (`grab_all_text` → vt100's `write_contents`) has always skipped them;
/// these two now agree.
pub fn extract_selection(screen: &vt100::Screen, a: (u16, u16), b: (u16, u16)) -> String {
    let (rows, cols) = screen.size();
    if rows == 0 || cols == 0 {
        return String::new();
    }
    // Normalize so `start` precedes `end` in reading order.
    let (start, end) = if (a.0, a.1) <= (b.0, b.1) { (a, b) } else { (b, a) };
    let mut lines: Vec<String> = Vec::new();
    for row in start.0..=end.0.min(rows - 1) {
        let mut first = if row == start.0 { start.1 } else { 0 };
        // P15 boundary: a selection that begins on a wide glyph's right half
        // snaps back to its left half, so the glyph the user pointed at is
        // yanked whole instead of vanishing with the skipped continuation.
        if first > 0 && screen.cell(row, first).is_some_and(|c| c.is_wide_continuation()) {
            first -= 1;
        }
        let last = if row == end.0 { end.1 } else { cols - 1 };
        let mut line = String::new();
        for col in first..=last.min(cols - 1) {
            match screen.cell(row, col) {
                Some(c) if c.is_wide_continuation() => {}
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
        agent_title_status, extract_selection, gesture_presented, host_bell_bytes,
        host_clipboard_bytes, host_notify_bytes, sanitize_for_host, scrub_control_env,
        scrub_host_identity, sync_presented, AgentStatus, CONTROL_ENV_VARS,
        GESTURE_FREEZE_STALE_CAP, HOST_IDENTITY_VARS, HOST_NOTIFY_CAP, HOST_NOTIFY_INTERVAL,
        OSC52_INTERVAL, OSC52_PAYLOAD_CAP, SYNC_STALE_CAP_DEFAULT,
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
        let presented = sync_presented(p.screen(), None, SYNC_STALE_CAP_DEFAULT);
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
        let presented = sync_presented(p.screen(), view.as_ref(), SYNC_STALE_CAP_DEFAULT);
        assert!(presented.contents().contains("complete"));
        assert!(!presented.contents().contains("torn"));
    }

    /// P1's safety valve: a bracket that never closes (an app killed
    /// mid-redraw) must not freeze the pane forever. Past `SYNC_STALE_CAP_DEFAULT`
    /// the live grid is presented again — torn beats frozen. Pure, so the
    /// expiry is proven without sleeping 150 ms.
    #[test]
    fn a_stuck_bracket_expires_at_the_staleness_cap() {
        let mut p = vt100::Parser::new(4, 20, 0);
        p.process(b"complete");
        p.process(b"\x1b[?2026h\x1b[2J\x1b[Hhalf-drawn");
        let snap = p.take_sync_snapshot().expect("bracket open captures");

        // One tick shy of the cap: still the captured frame.
        let fresh = Some((
            snap.clone(),
            Instant::now() - SYNC_STALE_CAP_DEFAULT + Duration::from_millis(1),
        ));
        assert!(sync_presented(p.screen(), fresh.as_ref(), SYNC_STALE_CAP_DEFAULT)
            .contents()
            .contains("complete"));

        // Past it: the live grid, however torn — the pane keeps moving even
        // though the app never sent `?2026l`.
        let stale =
            Some((snap, Instant::now() - SYNC_STALE_CAP_DEFAULT - Duration::from_millis(1)));
        let presented = sync_presented(p.screen(), stale.as_ref(), SYNC_STALE_CAP_DEFAULT);
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
        assert!(sync_presented(p.screen(), view.as_ref(), SYNC_STALE_CAP_DEFAULT)
            .contents()
            .contains("row19"));
        // Scrolled into history: the live grid's own scrolled view wins.
        p.set_scrollback(5);
        assert!(p.screen().scrollback() > 0);
        let presented = sync_presented(p.screen(), view.as_ref(), SYNC_STALE_CAP_DEFAULT);
        assert_eq!(presented.contents(), p.screen().contents());
    }

    /// C29 (selection-freeze amendment): a fresh gesture presents the frame
    /// it froze at `Down`, not whatever the pane has printed since — the
    /// mechanism `main.rs::tests::
    /// output_banked_mid_drag_does_not_change_what_release_copies` proves
    /// end to end; this pins the pure decision in isolation.
    #[test]
    fn gesture_presents_the_frozen_frame_while_fresh() {
        let frozen = screen_with("original", 4, 20).screen().clone();
        let live = screen_with("mutated", 4, 20);
        let freeze = Some((frozen, Instant::now()));
        let presented = gesture_presented(live.screen(), freeze.as_ref(), GESTURE_FREEZE_STALE_CAP)
            .expect("a fresh freeze wins presentation");
        assert!(presented.contents().contains("original"));
        assert!(!presented.contents().contains("mutated"));
    }

    /// C29's safety valve: a lost `Up` — the button released outside the
    /// window, the terminal never delivered the event — must not freeze a
    /// pane forever. Past `GESTURE_FREEZE_STALE_CAP`, `presented()` falls
    /// back to live. Pure, so the expiry is proven without sleeping the
    /// real 30 seconds.
    #[test]
    fn a_lost_mouse_up_does_not_freeze_the_pane_forever() {
        let frozen = screen_with("original", 4, 20).screen().clone();
        let live = screen_with("mutated", 4, 20);

        // One tick shy of the cap: still frozen.
        let fresh = Some((
            frozen.clone(),
            Instant::now() - GESTURE_FREEZE_STALE_CAP + Duration::from_millis(1),
        ));
        assert!(
            gesture_presented(live.screen(), fresh.as_ref(), GESTURE_FREEZE_STALE_CAP).is_some(),
            "a freeze just under the cap still wins"
        );

        // Past it: no gesture is this long-lived for real — live wins.
        let stale =
            Some((frozen, Instant::now() - GESTURE_FREEZE_STALE_CAP - Duration::from_millis(1)));
        assert!(
            gesture_presented(live.screen(), stale.as_ref(), GESTURE_FREEZE_STALE_CAP).is_none(),
            "a stale freeze must not keep winning — a lost Up cannot freeze the pane forever"
        );
    }

    /// A pane already scrolled into its own history when the gesture starts
    /// is left live, not frozen: `Screen::snapshot` always resets to the
    /// live tail (`vendor/vt100/src/grid.rs:49-53`), so honoring the freeze
    /// there would silently yank a history-reading user to the tail
    /// instead of protecting anything — and banked rows are already
    /// immutable, so there is nothing moving under a scrolled-back drag to
    /// protect against in the first place. Same rule `sync_presented`
    /// already applies to the sync-bracket veneer, mirrored here rather
    /// than shared, so neither mechanism's own existing tests need to move.
    #[test]
    fn a_scrolled_back_view_ignores_the_gesture_freeze() {
        let mut live = vt100::Parser::new(4, 20, 100);
        for i in 0..20 {
            live.process(format!("row{i}\r\n").as_bytes());
        }
        let frozen = live.screen().clone();
        live.set_scrollback(5);
        assert!(live.screen().scrollback() > 0);

        let freeze = Some((frozen, Instant::now()));
        assert!(
            gesture_presented(live.screen(), freeze.as_ref(), GESTURE_FREEZE_STALE_CAP).is_none(),
            "scrolled into history, the gesture freeze must not override the live scrolled view"
        );
    }

    /// P2: the shape roost re-emits, and the two things that must never
    /// reach the host — a body that could break out of roost's own sequence,
    /// and an unbounded one.
    #[test]
    fn host_notify_is_bounded_and_cannot_break_out_of_its_sequence() {
        let now = Instant::now();
        let emit =
            |body: &str| host_notify_bytes(body, None, now, HOST_NOTIFY_INTERVAL, HOST_NOTIFY_CAP);

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

    /// The SIGHUP grace is spent exactly once per pane, whoever spends it.
    ///
    /// `App::kill_fleet` hangs the whole fleet up and waits `HANGUP_GRACE`
    /// once, then kills every pane; `close_pane_id` comes straight to `kill`
    /// with no hangup at all. Both must give the agent its window to flush a
    /// final turn, and neither must pay for it twice — which is exactly what
    /// used to happen, invisibly, because portable-pty's `Child::kill` is
    /// not a `kill(2)`: it sends SIGHUP and sleeps in four 50 ms steps
    /// before escalating. Quit therefore grew with the fleet — measured
    /// 1.3 / 1.7 / 2.5 / 4.0 s for 1 / 2 / 4 / 8 busy panes against
    /// DESIGN-ui §6's 2 s budget, and it is what failed that gate on CI.
    ///
    /// The child here ignores SIGHUP, so the grace is always spent in full
    /// when it is spent at all — no timing luck in either direction.
    #[test]
    fn the_hangup_grace_is_given_once_and_only_by_whoever_owes_it() {
        use super::HANGUP_GRACE;
        use crate::agents::CommandSpec;
        use crate::core::event::AppEvent;
        use crate::ports::PaneBackend;

        // The child has to have reached its `trap` before either half means
        // anything — a pane still mid-exec ignores SIGHUP for the wrong
        // reason.
        fn settle(pt: &mut super::PtyPane) {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
                if matches!(pt.child.try_wait(), Ok(None)) && pt.pid.is_some() {
                    // Running, with a pid to signal: far enough along.
                    std::thread::sleep(Duration::from_millis(100));
                    return;
                }
            }
        }

        let deaf = || {
            let (tx, rx) = std::sync::mpsc::sync_channel::<AppEvent>(64);
            std::thread::spawn(move || while rx.recv().is_ok() {});
            let spec = CommandSpec::new("sh", &std::env::temp_dir())
                .arg("-c")
                .arg("trap '' HUP; sleep 30");
            super::PtyPane::spawn(1, &spec, 24, 80, (0, 0), tx).ok()
        };

        // The close path owes the grace and pays it.
        let Some(mut cold) = deaf() else {
            eprintln!("SKIP hangup-grace gate: no pty available");
            return;
        };
        settle(&mut cold);
        let t = Instant::now();
        cold.kill();
        let cold_cost = t.elapsed();
        assert!(
            cold_cost >= HANGUP_GRACE,
            "closing a pane must still give it the hangup window: took {cold_cost:?}"
        );

        // The shutdown path has already paid it, and must not pay again.
        let Some(mut warm) = deaf() else { return };
        settle(&mut warm);
        warm.hangup();
        let t = Instant::now();
        warm.kill();
        let warm_cost = t.elapsed();
        assert!(
            warm_cost < HANGUP_GRACE / 2,
            "a pane roost has already hung up must go straight to SIGKILL: took {warm_cost:?} \
             (the grace is {HANGUP_GRACE:?}, and kill_fleet pays it once for the whole fleet)"
        );
    }

    /// The EOF sweep must not depend on winning the reap race.
    ///
    /// `on_exit` runs when the *master* hits EOF, which happens as the last
    /// slave fd closes — while the child is still exiting. The sweep used to
    /// sit inside `if let Ok(Some(_)) = try_wait()`, so whenever roost got
    /// there first it was skipped outright and every backgrounded job the
    /// pane had started lived on. Intermittent by construction: two failures
    /// in eleven runs of main on CI, always in `tests/orphan_after_exit.rs`,
    /// never reproducible on demand.
    ///
    /// This makes it deterministic. The pane `exec`s into a `sleep` whose
    /// three fds are redirected off the tty: no fd holds the slave open, so
    /// the master EOFs, and the pane's pid is unambiguously still running
    /// when `on_exit` fires — the exact state the old branch declined to
    /// sweep in.
    #[test]
    fn the_eof_sweep_runs_even_when_the_pane_has_not_been_reaped_yet() {
        use crate::agents::CommandSpec;
        use crate::core::event::AppEvent;
        use crate::ports::PaneBackend;

        let (tx, rx) = std::sync::mpsc::sync_channel::<AppEvent>(256);
        let spec = CommandSpec::new("sh", &std::env::temp_dir());
        let Ok(mut pt) = super::PtyPane::spawn(1, &spec, 24, 80, (0, 0), tx) else {
            eprintln!("SKIP eof-sweep gate: no pty available");
            return;
        };
        // A job in its own right with its fds off the pty — neither the
        // pane's process group nor the terminal's foreground group, so only
        // the session sweep reaches it — then `exec` into something that
        // holds no tty fd either, so the master EOFs with the pid alive.
        // `B''G=` so the shell's echo of the command line cannot be mistaken
        // for its output.
        pt.write_input_raw(
            b"sleep 60 </dev/null >/dev/null 2>&1 & echo B''G=$!\n              exec sleep 20 </dev/null >/dev/null 2>&1\n",
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut bg = None;
        while std::time::Instant::now() < deadline && bg.is_none() {
            std::thread::sleep(std::time::Duration::from_millis(50));
            while let Ok(AppEvent::Output(_, bytes)) = rx.try_recv() {
                pt.process_output(&bytes);
            }
            bg = pt
                .parser
                .screen()
                .contents()
                .split("BG=")
                .nth(1)
                .and_then(|t| t.split_whitespace().next())
                .and_then(|t| t.parse::<u32>().ok());
        }
        let Some(bg) = bg else {
            pt.kill();
            eprintln!("SKIP eof-sweep gate: the pane never reported its job");
            return;
        };
        // Let the `exec` land, then pin the precondition: this is the race,
        // not a pane that has already been reaped.
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert!(
            matches!(pt.child.try_wait(), Ok(None)),
            "setup: the pane's pid must still be running when on_exit fires"
        );

        pt.on_exit();

        // A killed process lingers as a zombie until its new parent reaps
        // it, and `kill -0` succeeds for a zombie — so the state is what is
        // asked, with the same short grace `tests/harness` uses.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
        let mut alive = true;
        while alive && std::time::Instant::now() < deadline {
            alive = running(bg);
            if alive {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        // Safety: SIGKILL to a pid this test created.
        unsafe { libc::kill(bg as libc::pid_t, libc::SIGKILL) };
        pt.kill();
        assert!(!alive, "the backgrounded job {bg} outlived the pane's EOF");
    }

    /// Is `pid` a *running* process? `kill -0` alone is not that question —
    /// it succeeds for a zombie, which holds nothing and is only waiting to
    /// be collected. Same reasoning (and the same fix) as
    /// `tests/harness::is_alive`.
    fn running(pid: u32) -> bool {
        let Ok(out) = std::process::Command::new("ps")
            .args(["-o", "state=", "-p", &pid.to_string()])
            .output()
        else {
            return false;
        };
        let state = String::from_utf8_lossy(&out.stdout);
        let state = state.trim();
        !state.is_empty() && !state.starts_with('Z')
    }

    /// P2: and the **desktop** channel is gated too, on the same interval.
    ///
    /// The relay above (`host_notify_bytes`) was rate-limited from the
    /// start; `PaneEffects::notifications` — which the composition root
    /// turns into a host bell plus an `osascript` fork — was not. Measured
    /// before the gate: this exact loop produced 500 notifications while
    /// the relay beside it produced one, so `for i in $(seq 1 10000); do
    /// printf '\e]9;hi\a'; done` in any pane forked ten thousand processes.
    ///
    /// Driven through a real `PtyPane` rather than a helper, because the
    /// gate has to hold for the path the event loop actually takes.
    #[test]
    fn the_desktop_notification_channel_is_rate_limited_per_pane_too() {
        use crate::agents::CommandSpec;
        use crate::core::event::AppEvent;
        use crate::ports::PaneBackend;

        let (tx, rx) = std::sync::mpsc::sync_channel::<AppEvent>(8);
        std::thread::spawn(move || while rx.recv().is_ok() {});
        let spec = CommandSpec::new("sleep", &std::env::temp_dir()).arg("30");
        let Ok(mut pt) = super::PtyPane::spawn(1, &spec, 24, 80, (0, 0), tx) else {
            eprintln!("SKIP desktop-notification gate: no pty available");
            return;
        };
        let mut fired = 0usize;
        for i in 0..500 {
            pt.process_output(format!("\x1b]9;ping {i}\x07").as_bytes());
            fired += pt.take_effects().notifications.len();
        }
        pt.kill();
        assert_eq!(
            fired, 1,
            "500 OSC 9 sequences inside one interval must yield one desktop notification"
        );
    }

    /// ux P1-6: the fallback bell relay is a single raw BEL, gated by the
    /// exact same rate window as `host_notify_bytes` — pinned separately so
    /// a future change to one gate can't silently desync the other.
    #[test]
    fn host_bell_is_a_single_bel_and_rate_limited_per_pane() {
        let now = Instant::now();
        assert_eq!(host_bell_bytes(None, now, HOST_NOTIFY_INTERVAL), Some(vec![0x07]));

        let recent = Some(now - HOST_NOTIFY_INTERVAL + Duration::from_millis(1));
        assert!(host_bell_bytes(recent, now, HOST_NOTIFY_INTERVAL).is_none());
        let old = Some(now - HOST_NOTIFY_INTERVAL - Duration::from_millis(1));
        assert!(host_bell_bytes(old, now, HOST_NOTIFY_INTERVAL).is_some());
    }

    /// P3: a pane's clipboard write is relayed verbatim to the host — and
    /// only when it really is a clipboard write. Each call passes `last:
    /// None` so the rate gate (pinned separately below) never interferes.
    #[test]
    fn host_clipboard_relays_writes_and_refuses_anything_else() {
        let cap = OSC52_PAYLOAD_CAP;
        let emit = |sel: &str, payload: &str| {
            host_clipboard_bytes(sel, payload, cap, None, Instant::now(), OSC52_INTERVAL)
        };

        // Verbatim relay, every xterm selection target.
        assert_eq!(emit("c", "aGk=").unwrap(), b"\x1b]52;c;aGk=\x07".to_vec());
        assert_eq!(emit("cp", "aGk=").unwrap(), b"\x1b]52;cp;aGk=\x07".to_vec());
        assert_eq!(emit("7", "aGk=").unwrap(), b"\x1b]52;7;aGk=\x07".to_vec());
        // An empty payload is xterm's "clear", and a legitimate write.
        assert_eq!(emit("c", "").unwrap(), b"\x1b]52;c;\x07".to_vec());
        // A payload right at the cap still goes; one byte over does not.
        assert!(emit("c", &"A".repeat(cap)).is_some());
        assert!(emit("c", &"A".repeat(cap + 1)).is_none());

        // Not base64 ⇒ not a clipboard write. Critically, a payload that
        // could close roost's own sequence and repaint the user's terminal.
        for hostile in ["aGk=\x07\x1b]0;PWNED\x07", "hi there", "a;b", "\x1b[2J"] {
            assert!(emit("c", hostile).is_none(), "must refuse {hostile:?}");
        }
        // Same for a selection field that isn't one.
        for sel in ["c\x07x", "clipboard", "c;p"] {
            assert!(emit(sel, "aGk=").is_none(), "must refuse selection {sel:?}");
        }
    }

    /// P2/P3: at most one OSC 52 relay per pane per interval — the measured
    /// abuse case (a shell loop relaying 5000 sequences to the operator's
    /// real clipboard) is exactly what `OSC52_PAYLOAD_CAP` alone does not
    /// stop, since it bounds one write's size, not how often one arrives.
    #[test]
    fn host_clipboard_is_rate_limited_per_pane() {
        let now = Instant::now();
        let cap = OSC52_PAYLOAD_CAP;
        let emit = |last| host_clipboard_bytes("c", "aGk=", cap, last, now, OSC52_INTERVAL);

        // Just inside the window: dropped, even though the payload is fine.
        let recent = Some(now - OSC52_INTERVAL + Duration::from_millis(1));
        assert!(emit(recent).is_none());
        // Past it: relayed again.
        let old = Some(now - OSC52_INTERVAL - Duration::from_millis(1));
        assert!(emit(old).is_some());
        // The first write of a pane's life is never rate-limited.
        assert!(emit(None).is_some());
    }

    /// P2/P3 share the sanitizer; pin that it keeps ordinary text intact.
    #[test]
    fn sanitize_keeps_printable_text_verbatim() {
        assert_eq!(sanitize_for_host("run `ls -la`? (y/n)", 100), "run `ls -la`? (y/n)");
        assert_eq!(sanitize_for_host("a\tb\nc", 100), "abc");
    }

    /// D5: the title→status mapping — spinner frame (any braille glyph)
    /// means working, `✳` means at rest, anything else is no signal. The
    /// second char must be a space (or absent) so a title that merely starts
    /// with one of these glyphs doesn't read as agent state.
    #[test]
    fn agent_title_status_maps_title_shapes() {
        assert_eq!(agent_title_status("⠧ fixing tests"), Some(AgentStatus::Working));
        assert_eq!(agent_title_status("⠧"), Some(AgentStatus::Working));
        assert_eq!(agent_title_status("✳ Claude Code"), Some(AgentStatus::Waiting));
        assert_eq!(agent_title_status("✳"), Some(AgentStatus::Waiting));
        assert_eq!(agent_title_status("✳x"), None);
        assert_eq!(agent_title_status("⠧x"), None);
        assert_eq!(agent_title_status("bash — ~/code"), None);
        assert_eq!(agent_title_status(""), None);
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

    /// L3: a pane child must never inherit roost's own live control-plane
    /// credentials. Set as real *process* env vars rather than directly on
    /// the builder — `CommandBuilder::new` materializes `std::env::vars_os()`
    /// into its base env, so only this proves `env_remove` suppresses true
    /// inheritance; setting them via `cmd.env()` first would only prove
    /// removal of an explicit override. Restores the real values before
    /// asserting, so a failure here can't leave them redirected for the
    /// rest of the test binary.
    #[test]
    fn scrub_control_env_removes_truly_inherited_vars() {
        let saved: Vec<_> = CONTROL_ENV_VARS.iter().map(|&v| (v, std::env::var_os(v))).collect();
        for &var in CONTROL_ENV_VARS {
            std::env::set_var(var, "leaked-from-outer-roost");
        }
        let mut cmd = CommandBuilder::new("true");
        scrub_control_env(&mut cmd);
        for (var, prev) in &saved {
            match prev {
                Some(v) => std::env::set_var(var, v),
                None => std::env::remove_var(var),
            }
        }
        for &var in CONTROL_ENV_VARS {
            assert!(cmd.get_env(var).is_none(), "{var} must not survive as inherited env");
        }
    }

    /// L3's other half: when this pane's spec *does* carry a real
    /// ROOST_SOCK/ROOST_TOKEN (the control listener bound), the scrub must
    /// not stand in the way of `spawn`'s own `spec.env` loop setting them —
    /// same ordering `spawn` uses (scrub, then apply `spec.env`).
    /// ROOST_CONTROL_TOKEN isn't part of this: nothing ever re-sets it
    /// after the scrub, in `spec.env` or anywhere else in `spawn` — the
    /// removal test above is its whole story.
    #[test]
    fn scrub_control_env_does_not_block_spec_env_from_setting_them_after() {
        let mut cmd = CommandBuilder::new("true");
        cmd.env("ROOST_SOCK", "/tmp/outer.sock");
        scrub_control_env(&mut cmd);
        cmd.env("ROOST_SOCK", "/tmp/this-pane.sock");
        cmd.env("ROOST_TOKEN", "this-pane-token");
        assert_eq!(cmd.get_env("ROOST_SOCK"), Some(OsStr::new("/tmp/this-pane.sock")));
        assert_eq!(cmd.get_env("ROOST_TOKEN"), Some(OsStr::new("this-pane-token")));
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

    /// P15: `日本語` occupies six columns, three of them continuations.
    /// Before, each continuation contributed a space and the yank came back
    /// `"日 本 語"` — unusable pasted into an agent.
    #[test]
    fn wide_cjk_yanks_without_injected_spaces() {
        let p = screen_with("日本語", 3, 20);
        assert_eq!(extract_selection(p.screen(), (0, 0), (0, 5)), "日本語");
        // Selecting past the text must still trim, not pad from the middle.
        assert_eq!(extract_selection(p.screen(), (0, 0), (0, 19)), "日本語");
    }

    /// P15/P17: an emoji-presentation sequence is two columns as of P17, so
    /// it has a continuation cell to skip like any other wide glyph — and
    /// mixed narrow/wide text keeps every column of its narrow half.
    #[test]
    fn wide_emoji_and_mixed_text_yank_verbatim() {
        let p = screen_with("ok \u{2764}\u{fe0f} \u{1f600} done", 3, 30);
        assert_eq!(
            extract_selection(p.screen(), (0, 0), (0, 29)),
            "ok \u{2764}\u{fe0f} \u{1f600} done"
        );
    }

    /// P15 boundary: pointing at a wide glyph's right half selects the glyph,
    /// not nothing. Without the snap the skipped continuation would silently
    /// drop the first character of the selection.
    #[test]
    fn selection_starting_on_a_continuation_cell_keeps_its_glyph() {
        let p = screen_with("日本語", 3, 20);
        // (0,1) is 日's right half; (0,3) is 本's.
        assert_eq!(extract_selection(p.screen(), (0, 1), (0, 5)), "日本語");
        assert_eq!(extract_selection(p.screen(), (0, 3), (0, 5)), "本語");
    }

    /// P15's stated contract: the copy path and the whole-history path must
    /// agree. `grab_all_text` goes through vt100's `write_contents`, which has
    /// always skipped continuations; a full-screen `extract_selection` now
    /// produces the same text.
    #[test]
    fn selection_and_history_extraction_agree_on_wide_text() {
        let p = screen_with("日本語 abc\r\n\u{1f600}x", 3, 20);
        let selected = extract_selection(p.screen(), (0, 0), (1, 19));
        let all = p.screen().all_contents();
        let history: Vec<&str> = all.lines().take(2).collect();
        assert_eq!(selected.lines().collect::<Vec<_>>(), history);
        assert_eq!(selected, "日本語 abc\n\u{1f600}x");
    }

    /// P0-3: a spawn that fails at `spawn_command` (~line 512) carries the
    /// real OS cause (ENOENT here) *underneath* the `.with_context(||
    /// format!("spawning {program}"))` this file adds — anyhow's default
    /// `Display` shows only that outer context, which is the bug (app.rs
    /// used to store exactly that truncated string). The alternate format
    /// walks the whole chain on one line. Real `PtyPane::spawn`, not a
    /// fake, so this pins the actual failure path finding #1 is about.
    #[test]
    fn spawn_failure_keeps_the_real_cause_reachable_via_alternate_format() {
        use crate::agents::CommandSpec;
        use crate::core::event::AppEvent;
        use crate::ports::PaneBackend;

        let (tx, _rx) = std::sync::mpsc::sync_channel::<AppEvent>(8);
        let spec =
            CommandSpec::new("roost-test-definitely-does-not-exist-9f3c", &std::env::temp_dir());
        // `PtyPane` has no `Debug` impl (it holds trait objects), so
        // `expect_err` can't be used here — match instead.
        let err = match super::PtyPane::spawn(1, &spec, 24, 80, (0, 0), tx) {
            Err(e) => e,
            Ok(_) => panic!("a nonexistent program must fail to spawn"),
        };
        let outer = format!("{err}");
        let full = format!("{err:#}");
        assert_eq!(outer, format!("spawning {}", spec.program), "the context this file adds");
        assert!(
            full.len() > outer.len() && full.starts_with(&outer),
            "{{:#}} must carry the real cause {{}} drops on the floor: {full:?}"
        );
    }

    /// D2 (selection-freeze design audit): a resize mid-gesture must drop
    /// the freeze outright — the frozen frame is invalidated at the OLD
    /// size, and `grab_text` would go on extracting from it against the
    /// NEW grid's coordinates for the rest of the gesture. There is no
    /// correct way to keep a frame through a resize, so `resize` clears it,
    /// same as a gesture that never started.
    #[test]
    fn a_resize_mid_gesture_drops_the_freeze() {
        use crate::agents::CommandSpec;
        use crate::core::event::AppEvent;
        use crate::ports::PaneBackend;

        let (tx, _rx) = std::sync::mpsc::sync_channel::<AppEvent>(8);
        let spec = CommandSpec::new("sh", &std::env::temp_dir());
        let mut pt = super::PtyPane::spawn(1, &spec, 24, 80, (0, 0), tx).expect("sh must spawn");

        pt.freeze_view();
        assert!(pt.gesture_freeze.is_some(), "setup: the freeze must be armed");

        pt.resize(30, 100, (0, 0));
        assert!(pt.gesture_freeze.is_none(), "a resize mid-gesture must drop the freeze");

        pt.kill();
    }

    /// No sequence of bytes a pane emits may panic the per-chunk path.
    ///
    /// `tests/vt100_panics.rs` sweeps the terminal parser; this sweeps
    /// everything `PtyPane::process_output` does *around* it on the same
    /// bytes, which is where roost's own code lives: the bell counter, the
    /// `QueryResponder` (which parses DA/DSR/DECRQM/XTWINOPS/kitty
    /// out of the same stream and writes replies back), the synchronized-
    /// output snapshot, W3 effect routing with its host-output caps, and the
    /// title→status channel. All of it runs on the event-loop thread, so a
    /// panic in any of it is the fleet incident `tests/panic_shutdown.rs`
    /// describes, not a bad frame.
    ///
    /// Drives a real `PtyPane` (over a `sleep` child, which reads nothing
    /// and writes nothing, so the only bytes in play are the ones fed here)
    /// rather than the pieces in isolation — the interactions are the point.
    /// Skips where no pty is available, like every other spawn-backed test.
    #[test]
    fn no_byte_sequence_panics_the_pane_output_path() {
        use crate::agents::CommandSpec;
        use crate::core::event::AppEvent;
        use crate::ports::PaneBackend;
        let vocab: &[&[u8]] = &[
            b"\x1b[",
            b"\x1b]",
            b"\x1b",
            b"\x07",
            b"\x9b",
            b"\x1bP",
            b"\x1b_",
            b"\x1b^",
            b"\x1bX",
            b";",
            b"?",
            b":",
            b"0",
            b"1",
            b"9",
            b"999999999",
            b"-1",
            b"2147483648",
            b"H",
            b"J",
            b"K",
            b"m",
            b"r",
            b"h",
            b"l",
            b"n",
            b"t",
            b"S",
            b"T",
            b"L",
            b"M",
            b"@",
            b"P",
            b"X",
            b"d",
            b"G",
            b"A",
            b"B",
            b"C",
            b"D",
            b"E",
            b"F",
            b"g",
            b"c",
            b"q",
            b"p",
            b"$",
            b"\"",
            b"b",
            b"Z",
            b"I",
            b"`",
            b"a",
            b"e",
            b"f",
            b"s",
            b"u",
            b"W",
            b"|",
            b"'",
            b"\x1b[c",
            b"\x1b[>c",
            b"\x1b[5n",
            b"\x1b[6n",
            b"\x1b[?6n",
            b"\x1b[?1$p",
            b"\x1b[?2026$p",
            b"\x1b[14t",
            b"\x1b[16t",
            b"\x1b[18t",
            b"\x1b[>0q",
            b"\x1bP+q544e\x1b\\",
            b"\x1b[?u",
            b"\x1b[=1;1u",
            b"\x1b[>1u",
            b"\x1b[<u",
            b"\x1b[?1049h",
            b"\x1b[?2026h",
            b"\x1b]0;t\x07",
            b"\x1b]2;\xe4\xb8\xad\x07",
            b"\x1b]52;c;aGk=\x07",
            b"\x1b]9;hi\x07",
            b"\x1b]777;notify;a;b\x07",
            b"\x1b]52;c;",
            b"\x1b]11;?\x07",
            b"\x1b]10;?\x1b\\",
            b"\r",
            b"\n",
            b"\t",
            b"\x08",
            b"\x0b",
            b"\x0c",
            b"\x0e",
            b"\x0f",
            b"\x00",
            "\u{1f600}".as_bytes(),
            "\u{4e2d}".as_bytes(),
            "\u{0301}".as_bytes(),
            "\u{fe0f}".as_bytes(),
            "\u{200d}".as_bytes(),
            b"\xff\xfe",
            b"\xc3",
            b"\xed\xa0\x80",
            b"abc",
            b"     ",
        ];
        let (tx, rx) = std::sync::mpsc::sync_channel::<AppEvent>(65536);
        std::thread::spawn(move || while rx.recv().is_ok() {});
        let spec = CommandSpec::new("sleep", &std::env::temp_dir()).arg("300");
        let mut pt = match super::PtyPane::spawn(1, &spec, 24, 80, (0, 0), tx) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("SKIP pane-output sweep: {e}");
                return;
            }
        };
        let mut st: u64 = 0x243F6A8885A308D3;
        let mut next = || {
            st ^= st << 13;
            st ^= st >> 7;
            st ^= st << 17;
            st
        };
        for _ in 0..120_000u64 {
            match next() % 400 {
                0 => {
                    let r = 1 + (next() % 60) as u16;
                    let c = 1 + (next() % 200) as u16;
                    pt.resize(r, c, ((next() % 2000) as u16, (next() % 2000) as u16));
                }
                1 => {
                    let _ = pt.screen().map(|s| s.contents());
                }
                2 => {
                    let _ = pt.take_effects();
                }
                3 => {
                    pt.set_scrollback((next() % 300) as usize);
                }
                4 => pt.scroll_by(((next() % 40) as i32) - 20),
                5 => {
                    let _ = pt.status();
                    let _ = pt.cursor_shape();
                    let _ = pt.alternate_screen();
                }
                _ => pt.process_output(vocab[(next() as usize) % vocab.len()]),
            }
        }
        pt.kill();
    }

    /// Every caller of `session_members` walks its result straight into
    /// `SIGKILL`, so a sid that names init's session — the one every daemon
    /// on the machine belongs to — must come back empty rather than come
    /// back enormous. (Without the guard this returns hundreds of pids on
    /// both supported platforms.)
    #[test]
    fn the_session_sweep_refuses_to_enumerate_inits_session() {
        assert!(super::session_members(1).is_empty(), "sid 1 is init's session");
        assert!(super::session_members(0).is_empty(), "sid 0 is not a session at all");
    }
}
