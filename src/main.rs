//! roost — a session-native terminal multiplexer for AI agent CLIs.
//!
//! No daemon: quitting kills the agent processes; the (layout × session-id)
//! mapping persists, and every pane resumes its exact session on relaunch.
//!
//! This file is the composition root: it wires the core (`core::app`) to the
//! production adapters (`infra::*`) and runs the event loop. Everything
//! below `run()` is thin glue; behavior lives in the core and is unit-tested
//! there against fakes.

mod agents;
mod cli;
mod core;
mod infra;
mod ports;
mod ui;

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange, Event,
    KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use std::io::IsTerminal;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::core::app::{App, Mode};
use crate::core::control::TokenTable;
use crate::core::event::AppEvent;
use crate::core::layout::PaneRect;
use crate::core::workspace::PaneId;
use crate::infra::notify::TermNotifier;
use crate::infra::pty::PtyPane;
use crate::infra::store::FsStore;
use crate::ports::{MouseProto, Notifier, PaneBackend, StateStore};
use crate::ui::input::{self, Action, InputResult};
use crate::ui::mouse::{self, MouseAction};

/// The one `spawn_listener` failure that must stay fatal: the socket
/// directory exists but isn't privately ours, which is what an attacker
/// pre-creating it looks like. Every other failure degrades to "no control
/// plane, and roost says so". Matches against `sock::UNSAFE_SOCKET_DIR_MSG`
/// rather than a hand-typed copy of `sock.rs`'s `bail!` wording (P2) — see
/// that const's doc comment.
fn is_unsafe_socket_dir(e: &anyhow::Error) -> bool {
    format!("{e:#}").contains(crate::infra::sock::UNSAFE_SOCKET_DIR_MSG)
}

fn main() -> Result<()> {
    // Client mode: `roost <verb> ...` talks to a running instance and exits,
    // before any terminal/lock setup. No args → launch the multiplexer.
    if let Some(code) = cli::maybe_run() {
        std::process::exit(code);
    }

    // Reaching here means no args at all: launch the TUI. It needs a real
    // terminal on stdout — ratatui::init() below enables raw mode, which
    // panics with a raw Rust backtrace (not a message) when stdout is
    // redirected: piped, backgrounded, or driven by a script/CI runner with
    // no pty. Fail the same clean way every rejected `cli::maybe_run` path
    // does instead — message on stderr, nonzero exit — before touching the
    // terminal at all.
    //
    // Exit 2 (P3), not 1: cli.rs's USAGE documents 1 as a runtime error (an
    // invocation that was fine but failed when actually attempted, e.g.
    // "cannot reach a running roost") and 2 as a usage error (this
    // invocation itself is wrong; retrying it unchanged won't help). A bare
    // `roost` off a tty is the latter — the fix is calling it differently
    // (`roost <verb> ...`, per the message below), exactly the class
    // `cli::maybe_run`'s own hard errors (unrecognized verb, an unknown
    // flag) already exit 2 for. That the specific *reason* here is
    // environmental rather than a bad argument doesn't change which bucket
    // a scripted caller needs to sort it into.
    if !std::io::stdout().is_terminal() {
        eprintln!(
            "roost: stdout is not a terminal; the multiplexer needs one to run.\n\
             For scripting, use `roost <verb> ...` — see `roost --help`."
        );
        std::process::exit(2);
    }

    // One roost per state dir: two instances sharing a workspace.json race
    // and corrupt each other's panes. Hold an exclusive lock for the whole
    // run (released automatically on exit). Do this before touching the
    // terminal so a refusal prints cleanly.
    let _lock = match acquire_instance_lock() {
        Ok(lock) => lock,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(1);
        }
    };

    // Restore the terminal on panic — otherwise a crash (even one deep in a
    // dependency) leaves the user in raw mode / the alternate screen with
    // mouse capture on, i.e. a wrecked terminal. Do this before init.
    install_panic_hook();

    let mut terminal = ratatui::init();
    // Without mouse capture the hosting terminal consumes wheel events and
    // scrolls its own buffer — content *outside* the TUI. Capture them.
    // C29: a deliberate subset of crossterm's blanket `EnableMouseCapture` —
    // see `mouse::MOUSE_CAPTURE_ENABLE` for which modes and why.
    write_raw(mouse::MOUSE_CAPTURE_ENABLE.as_bytes());
    // Ask the host terminal to wrap pastes in the 200~/201~ guards so a paste
    // arrives as one Event::Paste instead of a keystroke flood — without
    // this, every newline in pasted text lands as a pressed Enter (a
    // multi-line prompt pasted into an agent pane submits line by line).
    // The guards are re-applied per pane in `App::forward_paste`, but only
    // for panes whose app actually switched mode 2004 on.
    let _ = execute!(std::io::stdout(), EnableBracketedPaste);
    // P10: ask the host to report when roost's own window gains/loses focus
    // (DEC private mode 1004). roost forwards that to the focused pane, and
    // synthesizes the same reports when focus moves *between* panes, so a
    // pane app that subscribed sees focus changes at both levels — vim's
    // autoread, a TUI that dims when unfocused, the agent CLIs. A host that
    // doesn't support the mode simply never sends the events; nothing else
    // depends on them (see `App::host_focused`).
    let _ = execute!(std::io::stdout(), EnableFocusChange);
    // Negotiate the enhanced (kitty) keyboard protocol so Shift+Enter and
    // Ctrl+Enter arrive as distinct key events — a bare terminal collapses
    // both to a plain CR, making "newline vs submit" impossible to tell apart.
    // Only push the flag if the terminal actually supports it.
    //
    // P4 (client side): the probe must never block first paint. crossterm's
    // supports_keyboard_enhancement() writes `CSI ?u`+`CSI c` and waits up
    // to a hard-coded 2 s for an answer — under a terminal that answers
    // neither (a bare PTY, a pre-W2 roost) that 2 s used to land between
    // launch and the first frame (measured: 2014 ms vs 23 ms). Run the probe
    // on a helper thread with a short budget: real terminals answer in a few
    // milliseconds, so the enhanced path is unchanged; past the budget we
    // paint immediately without enhancement and let the main loop adopt a
    // late "supported" answer if one ever arrives (`run` polls the channel).
    let (kbd_tx, kbd_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = kbd_tx
            .send(matches!(crossterm::terminal::supports_keyboard_enhancement(), Ok(true)));
    });
    let kbd_pending = match kbd_rx.recv_timeout(KBD_PROBE_BUDGET) {
        Ok(true) => {
            push_kbd_enhancement();
            None
        }
        Ok(false) => None,          // answered: no support — settled
        Err(_) => Some(kbd_rx),     // still probing: run() watches for the answer
    };
    let result = run(&mut terminal, kbd_pending);
    // Pop unconditionally: the probe may have resolved (and pushed) after
    // the budget expired, so a simple "did we push" bool can't be trusted
    // here — and popping with nothing pushed is a no-op terminals ignore
    // (the panic hook already relies on exactly that).
    let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    let _ = execute!(std::io::stdout(), DisableBracketedPaste, DisableFocusChange);
    // C29: symmetric with the subset write at startup — see MOUSE_CAPTURE_ENABLE.
    write_raw(mouse::MOUSE_CAPTURE_DISABLE.as_bytes());
    // P7: hand the cursor back the way the user's terminal had it — a pane's
    // insert bar must not outlive roost.
    let _ = execute!(std::io::stdout(), cursor_style(None));
    reset_host_title();
    ratatui::restore();
    result
}

/// P6: put the host terminal's title back to a plain `roost` on the way out.
/// While running, roost publishes `roost · <focused pane>`; leaving that
/// behind would strand the user's tab under a pane name that no longer
/// exists. (There is no "restore what was there before" — a terminal offers
/// no way to read its own title back, so the honest reset is roost's own
/// name, which is what the user launched.)
fn reset_host_title() {
    write_raw(b"\x1b]2;roost\x07");
}

/// Write raw bytes straight to the host terminal, best-effort. Shared by
/// `reset_host_title` above and C29's own mouse-capture sequences
/// (`mouse::MOUSE_CAPTURE_ENABLE`/`_DISABLE`) — plain escape bytes crossterm
/// has no dedicated `Command` for.
fn write_raw(bytes: &[u8]) {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = out.write_all(bytes);
    let _ = out.flush();
}

/// P7: a pane's DECSCUSR parameter as the crossterm cursor style to apply to
/// the host. `None` — and anything outside the 1..=6 xterm defines — is the
/// terminal's own default, which is also what roost restores when the
/// focused pane asks for no shape, on exit, and in the panic hook.
fn cursor_style(shape: Option<u8>) -> crossterm::cursor::SetCursorStyle {
    use crossterm::cursor::SetCursorStyle as S;
    match shape {
        Some(1) => S::BlinkingBlock,
        Some(2) => S::SteadyBlock,
        Some(3) => S::BlinkingUnderScore,
        Some(4) => S::SteadyUnderScore,
        Some(5) => S::BlinkingBar,
        Some(6) => S::SteadyBar,
        _ => S::DefaultUserShape,
    }
}

/// How long `main` waits synchronously for the keyboard-enhancement probe
/// before painting without it. Generous for a real terminal round-trip
/// (locals answer in single-digit ms; this survives a slow SSH hop) while
/// staying far under crossterm's internal 2 s give-up.
const KBD_PROBE_BUDGET: Duration = Duration::from_millis(250);

/// Push the kitty keyboard enhancement flags roost uses. Paired with the
/// unconditional pop on exit (and in the panic hook).
fn push_kbd_enhancement() {
    let _ = execute!(
        std::io::stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
}

/// Acquire an exclusive lock on `<state>/roost.lock`. Returns the held file
/// (keep it alive for the process lifetime) or a user-facing error message.
fn acquire_instance_lock() -> std::result::Result<std::fs::File, String> {
    use fs2::FileExt;
    let path = FsStore::default_path().with_extension("lock");
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = std::fs::File::create(&path)
        .map_err(|e| format!("roost: cannot open lock file {}: {e}", path.display()))?;
    file.try_lock_exclusive().map_err(|_| {
        let dir = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
        format!(
            "roost is already running for this workspace ({dir}).\n\
             Close the other instance, or set ROOST_STATE=<dir> to run an isolated one."
        )
    })?;
    Ok(file)
}

fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Pop keyboard enhancement unconditionally: if none was pushed the
        // terminal ignores it, and leaving it set would wedge the user's shell
        // into the kitty protocol after a crash.
        let _ = execute!(
            std::io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableBracketedPaste,
            // P10: and the focus reporting roost switched on, for the same
            // reason — a crash must not leave the user's shell being told
            // about every focus change by a terminal nobody is listening to.
            DisableFocusChange
        );
        // C29: same subset-disable used on the normal exit path.
        write_raw(mouse::MOUSE_CAPTURE_DISABLE.as_bytes());
        // P7: and the cursor shape a pane may have installed, for the same
        // reason the flags above are popped — a crash must not leave the
        // user's shell wearing a pane's insert bar.
        let _ = execute!(std::io::stdout(), cursor_style(None));
        // P6: and the window title roost took over, so a crash doesn't
        // strand the user's tab named after a pane that no longer exists.
        reset_host_title();
        ratatui::restore();
        original(info);
    }));
}

/// Bound on the event channel. A runaway pane (a `yes`, a firehose build log)
/// produces PTY output faster than the main loop parses and draws it; an
/// unbounded channel would let that queue grow without limit and OOM the whole
/// multiplexer. A bounded channel makes the reader thread's `send` block once
/// the queue is full, so the pane's PTY buffer fills and the child is throttled
/// at the OS level — real backpressure. Sized generously so normal bursts never
/// block.
const EVENT_CHANNEL_BOUND: usize = 1024;

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    mut kbd_pending: Option<mpsc::Receiver<bool>>,
) -> Result<()> {
    // This thread is the whole visible product — keyboard poll, pane
    // parsing, drawing — so it gets macOS's user-interactive QoS: when the
    // fleet's own agents saturate every core, the keystroke path wins the
    // contention instead of queueing behind them (infra::qos module doc; the
    // agents themselves are deliberately NOT demoted).
    infra::qos::promote_input_loop_thread();
    let (tx, rx) = mpsc::sync_channel::<AppEvent>(EVENT_CHANNEL_BOUND);

    // Wire production adapters to the core's ports.
    let store = FsStore::default();
    let ws = store.load()?.unwrap_or_else(|| {
        core::workspace::Workspace::default_in(
            std::env::current_dir().unwrap_or_else(|_| "/".into()),
        )
    });
    // config.json: the key-bindings escape hatch (design doc). Read once,
    // here, alongside workspace.json — never fatal (`Keymap::parse`); any
    // problem becomes a startup diagnostic below instead of blocking launch.
    let (keymap, keymap_diagnostics) = infra::config::load_keymap();
    // promotion-auth-gate decision 9: built before the listener (ordering
    // that stays correct once sock.rs's door consults a reader onto this
    // same table) and passed into `App::new` below instead of `App::new`
    // minting its own. Same fatal refusal `App::new` used to make on its own
    // (a weak, time-seeded fleet token is not an acceptable fallback — it
    // authorizes driving the whole workspace) — just relocated to where the
    // table it's now part of is actually built.
    let tokens = TokenTable::new()
        .ok_or_else(|| anyhow::anyhow!("cannot read /dev/urandom for the control token"))?;
    // `.ok()` here used to swallow three very different failures — the state
    // dir being uncreatable, `dir_is_private_and_ours` refusing (a *security*
    // bail: someone else's directory sitting where our socket goes), and
    // bind() failing (e.g. a state dir path past macOS's 104-byte sun_path
    // limit). roost then ran looking entirely normal with no control plane at
    // all: no $ROOST_SOCK in panes, so the pi extension and the Claude hooks
    // silently never report status, nothing is written to the audit log, and
    // every `roost <verb>` fails. Surface it instead. The security refusal
    // stays fatal: degrading "someone may be intercepting the control socket"
    // into "quietly no socket" is the wrong default.
    let (sock_path, sock_err) = match infra::sock::spawn_listener(tx.clone(), tokens.reader()) {
        Ok(p) => (Some(p), None),
        Err(e) if is_unsafe_socket_dir(&e) => return Err(e),
        Err(e) => (None, Some(format!("no control plane: {e:#}"))),
    };
    let sock_cleanup = sock_path.clone();
    let mut notifier = TermNotifier;
    let size = terminal.size()?;
    let mut app: App<PtyPane> = App::new(
        ws,
        agents::registry(),
        Box::new(store),
        tx,
        size,
        host_pixels(),
        sock_path,
        tokens,
    )?;
    app.relayout();
    app.set_keymap(keymap);
    // Perf telemetry (infra::perf): the event loop's scheduling-stall
    // histogram, one JSON line a minute into <state>/perf.jsonl — the
    // isolated measurement behind the QoS keep-or-delete decision.
    let mut perf =
        infra::perf::PerfLog::new(infra::store::FsStore::state_dir(), infra::qos::enabled());
    // Surface every config.json problem on the activity feed, so none are
    // silently lost, and the first as a toast — same non-fatal contract as
    // everything else here: roost already started fine, with its defaults.
    for diag in &keymap_diagnostics {
        app.note_config_issue(diag.clone());
    }
    if let Some(first) = keymap_diagnostics.first() {
        let msg = if keymap_diagnostics.len() == 1 {
            first.clone()
        } else {
            format!(
                "{first} (+{} more — see the activity feed)",
                keymap_diagnostics.len() - 1
            )
        };
        app.set_flash(msg);
    }
    if let Some(msg) = sock_err {
        app.set_flash(msg);
    }

    // Keep the pi status extension in sync with this build so it can't silently
    // go stale (a stale one's socket messages are now dropped for lacking the
    // per-pane token). No-op when pi isn't installed or ROOST_NO_EXT_INSTALL set.
    // A missing control plane matters more than either extension notice, so it
    // is flashed first and therefore last-written wins the visible slot.
    if let Some(msg) = infra::extension::ensure_pi_extension() {
        app.set_flash(msg);
    }
    // Same idea for Claude Code: merge roost's status hooks into
    // ~/.claude/settings.json. No-op when Claude Code isn't set up or
    // ROOST_NO_EXT_INSTALL is set — see infra::extension module docs.
    if let Some(msg) = infra::extension::ensure_claude_hooks() {
        app.set_flash(msg);
    }

    // Write the fleet control token where an external `roost <verb>` client can
    // read it (0600, owner-only, next to the socket). Never placed in a pane's
    // env — that's the boundary between "a pane reports itself" and "a client
    // drives the fleet". Cleaned up on exit.
    let control_token_path = FsStore::default_path().with_file_name("control.token");
    write_control_token(&control_token_path, &app.control_token());

    // P7: the cursor shape currently applied to the host terminal, so the
    // mirror only writes on a real change.
    let mut host_cursor_shape: Option<u8> = None;

    let loop_result: Result<()> = (|| {
    loop {
        // P4: adopt a keyboard-enhancement answer that arrived after the
        // startup budget (the probe thread keeps waiting up to crossterm's
        // own 2 s). One-shot: any answer (or a dead channel) settles it.
        if let Some(rx) = &kbd_pending {
            match rx.try_recv() {
                Ok(supported) => {
                    if supported {
                        push_kbd_enhancement();
                    }
                    kbd_pending = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => kbd_pending = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }

        terminal.draw(|f| ui::render::draw(f, &mut app))?;

        // Drain ALL pending terminal events this tick, not just one. During a
        // resize storm (dragging the window edge) several events queue up
        // faster than a one-event-per-iteration loop can consume; processing
        // one at a time leaves roost's geometry lagging the true terminal size
        // and stale intermediate frames on screen. We coalesce resizes to a
        // single post-drain reconciliation.
        let mut resized = false;
        // Perf telemetry (infra::perf): how much later than the 33ms budget
        // the poll actually returned is this thread's scheduling stall —
        // the isolated measurement behind the QoS keep-or-delete decision.
        // An event arriving early returns sooner than the budget; the
        // saturating_sub reads that as zero stall, which it is.
        let poll_started = Instant::now();
        let had_event = crossterm::event::poll(Duration::from_millis(33))?;
        perf.record_iteration(
            poll_started.elapsed().saturating_sub(Duration::from_millis(33)),
        );
        if had_event {
            let mut keys_drained: u64 = 0;
            loop {
                match crossterm::event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        keys_drained += 1;
                        // F1 (exit UX audit 2026-08-07): note_key_seen looks
                        // at the key itself now — evidence is a specific
                        // swallowed-Alt character, not "a key arrived".
                        app.note_key_seen(key);
                        if key.modifiers.contains(crossterm::event::KeyModifiers::ALT) {
                            app.note_alt_seen();
                        }
                        if !app.handle_mode_key(key) {
                            handle_key(&mut app, key);
                        }
                        // C24: a keyboard-copy `y`/Enter stashes its yanked
                        // text on the app (core has no clipboard I/O of its
                        // own); hand it to the OS clipboard here, same as
                        // the mouse path does inline in `handle_copy_mouse`.
                        // U14: flash what the clipboard actually did, once
                        // it has answered — never an optimistic "copied".
                        if let Some(text) = app.take_pending_yank() {
                            let outcome = infra::clipboard::copy(&text);
                            app.flash_copy(text.chars().count(), outcome);
                        }
                        // U19: and copy mode's `o` stashes a URL the same
                        // way — the browser is I/O, so it happens out here.
                        if let Some(url) = app.take_pending_open() {
                            infra::open::open_url(&url);
                        }
                    }
                    Event::Mouse(me) => handle_mouse(&mut app, me),
                    // P10: roost's window changed focus — the focused pane
                    // owns that event (inside roost, exactly one pane has
                    // focus), and only if it subscribed.
                    Event::FocusGained => app.on_host_focus(true),
                    Event::FocusLost => app.on_host_focus(false),
                    // Coalesce: act on the true size once, after draining.
                    Event::Resize(..) => resized = true,
                    // U8(b): a modal owns the paste — into the rename buffer,
                    // or swallowed — else it forwards to the focused pane.
                    Event::Paste(s) => app.handle_paste(&s),
                    _ => {}
                }
                if !crossterm::event::poll(Duration::ZERO)? {
                    break;
                }
            }
            perf.record_keys(keys_drained);
        }
        if resized {
            // Trust the terminal's current size, not a possibly-stale value
            // carried on an intermediate coalesced event, then hard-clear so
            // no leftover cells from an in-between frame survive. Pixel
            // geometry travels with it (P4) — same ioctl, re-read together.
            let sz = terminal.size()?;
            app.on_resize(sz, host_pixels());
            // Deliberately NOT `?`. ratatui-core 0.1.2 made `clear()` snapshot
            // the cursor first, which writes `ESC[6n` and blocks up to 2 s
            // waiting for the terminal to answer — and this arrived under the
            // `ratatui = "0.30"` pin via a patch bump, with no roost change.
            // A host that never answers DSR (a nested multiplexer that
            // swallows it, some CI and serial terminals, an ssh hop dropped
            // mid-resize) therefore turned a *cosmetic* clear into `run()`
            // returning Err, which sends `shutdown()` through every pane with
            // SIGHUP then SIGKILL. Resizing a window must never be able to
            // destroy the fleet: a failed clear costs one stale frame.
            let _ = terminal.clear();
        }

        // ...then drain PTY output and socket events — but cap how many per
        // tick. A firehose pane (an agent dumping megabytes) could otherwise
        // keep this loop busy indefinitely and starve draw / input / the
        // wait-registry poll below. The channel is bounded, so events past the
        // cap simply wait for the next tick: no loss, bounded latency.
        // ponytail: fixed cap; make it adaptive only if a real workload starves.
        const MAX_EVENTS_PER_TICK: usize = 512;
        for _ in 0..MAX_EVENTS_PER_TICK {
            let Ok(ev) = rx.try_recv() else { break };
            match ev {
                AppEvent::Command(req, reply) => {
                    app.handle_control_msg(req, reply);
                }
                // P2: a pane's OSC 9/777 notification pulls attention the
                // same way a bell or an extension "needs you" does.
                AppEvent::Output(id, bytes) => {
                    if let Some(msg) = app.on_pty_output(id, &bytes) {
                        notifier.notify(&msg);
                    }
                }
                AppEvent::Exit(id) => {
                    if let Some(msg) = app.on_pty_exit(id) {
                        notifier.notify(&msg);
                    }
                }
                // Socket-sourced events must present the pane's token; a
                // mismatch means another pane (or process) is trying to spoof
                // this one — drop it silently.
                AppEvent::Session(id, token, s) => {
                    if app.socket_authorized(id, &token) {
                        app.on_session(id, s);
                    }
                }
                AppEvent::Status(id, token, s, detail) => {
                    if app.socket_authorized(id, &token) {
                        if let Some(msg) = app.on_status(id, s, detail) {
                            notifier.notify(&msg);
                        }
                    }
                }
                // D2: same auth gate as Status/Session above — a link-down
                // must only be honored for the pane its connection actually
                // authenticated for, never let an unrelated connection's
                // close clear another pane's link.
                AppEvent::ExtLink(id, token, up) => {
                    if app.socket_authorized(id, &token) {
                        app.on_status_link(id, up);
                    }
                }
            }
        }

        // W3: hand the host terminal what the panes asked roost to forward
        // on their behalf — P2's re-emitted OSC 9 notifications, P3's OSC 52
        // clipboard writes. Deliberately here, between draws: a write landing
        // mid-frame would interleave with ratatui's own output and could be
        // parsed as part of a cell run. The core has already rate-limited,
        // capped and sanitized every byte.
        let host_bytes = app.take_host_output();
        if !host_bytes.is_empty() {
            use std::io::Write;
            let mut out = std::io::stdout();
            let _ = out.write_all(&host_bytes);
            let _ = out.flush();
        }

        // P7: mirror the FOCUSED pane's DECSCUSR shape onto the host's one
        // real cursor — an editor's insert bar should look like a bar. Only
        // on change, so this costs nothing per frame; moving focus to a pane
        // that asked for no shape restores roost's default with no special
        // case, because that pane simply reports `None`.
        let want_shape = app.focused_cursor_shape();
        if want_shape != host_cursor_shape {
            host_cursor_shape = want_shape;
            let _ = execute!(std::io::stdout(), cursor_style(want_shape));
        }

        // S4 (PR #46 code review): fire a double-click's deferred copy once
        // its window has passed with nothing superseding it — checked every
        // iteration (not just after an event) since it fires on a deadline,
        // not a keypress. See `App::due_copy`.
        if let Some(text) = app.due_copy() {
            let outcome = infra::clipboard::copy(&text); // U14
            app.flash_copy(text.chars().count(), outcome);
        }
        // Periodic housekeeping (filesystem session detection).
        app.tick();
        perf.maybe_flush();
        // Fire any parked `wait` control requests whose panes hit their target
        // status (or timed out) this iteration.
        app.poll_waiters();

        if app.quit {
            break;
        }
    }
    Ok(())
    })();

    // Always run shutdown — even if the loop bailed with an error via `?` — so
    // agents are killed/reaped and the workspace saved, never left orphaned.
    app.shutdown();
    if let Some(p) = &sock_cleanup {
        infra::sock::cleanup(p);
    }
    let _ = std::fs::remove_file(&control_token_path);
    loop_result
}

/// The host window's pixel geometry (width, height) via the winsize ioctl —
/// a cheap local read, no terminal round-trip. (0, 0) when the host doesn't
/// report pixels; panes then keep the honest zero (P4).
fn host_pixels() -> (u16, u16) {
    crossterm::terminal::window_size().map(|ws| (ws.width, ws.height)).unwrap_or((0, 0))
}

/// Write the control token 0600, owner-only. `.mode(0o600)` only governs
/// the mode a *new* file is created with — a token file a crash left
/// behind (so `open()` here reuses it instead of creating fresh) keeps
/// whatever mode it already had. `set_permissions` after opening acts on
/// the file we now hold either way, so a stale file's mode is reset too.
fn write_control_token(path: &std::path::Path, token: &str) {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
    {
        let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
        let _ = f.write_all(token.as_bytes());
    }
}

/// Handle a key that a UI mode did not consume: a global action, or bytes
/// forwarded to the focused pane (dead panes intercept relaunch keys).
fn handle_key<B: PaneBackend>(app: &mut App<B>, key: crossterm::event::KeyEvent) {
    // C23: raw pass-through. `App::raw_routing_active` is the routing
    // predicate verbatim (Normal mode, focused pane raw, alive). Every key
    // except the toggle bypasses `translate_with()`'s Action/swallow
    // semantics and forwards straight to the pane as bytes instead — "the
    // key that got you in gets you out" is the one exception, so it's still
    // detected via `translate_with()` (the single, config.json-aware source
    // of the Alt+Shift+p / Alt+'P' tolerance rule) rather than a second,
    // drift-prone copy of it here.
    if app.raw_routing_active() {
        if let InputResult::Action(Action::ToggleRaw) = input::translate_with(key, app.keymap()) {
            app.apply(Action::ToggleRaw);
        } else {
            let bytes = input::kitty_upgrade(key, input::encode_raw(key), app.focused_kitty());
            let bytes = input::app_cursor_upgrade(key, bytes, app.focused_app_cursor());
            if !bytes.is_empty() {
                app.forward_bytes(&bytes);
            }
        }
        return;
    }
    match input::translate_with(key, app.keymap()) {
        InputResult::Action(a) => app.apply(a),
        InputResult::Forward(bytes) if app.focused_dead() => match bytes.as_slice() {
            b"\r" => app.respawn_focused(false), // retry/resume
            b"f" => app.respawn_focused(true),   // fresh session
            // Copy the pasteable resume command (`cd <cwd> && …`) for use in
            // a plain terminal. No-op when there's no session to resume —
            // matching the bars, which only advertise `y` when there is one.
            b"y" => {
                if let Some(line) = app.resume_command_line(app.focused) {
                    let outcome = infra::clipboard::copy(&line); // U14
                    app.flash_copy(line.chars().count(), outcome);
                }
            }
            _ => {}
        },
        InputResult::Forward(bytes) => {
            // If the focused pane negotiated the kitty keyboard protocol, upgrade
            // modified Enter from the ESC+CR fallback to the CSI-u form it asked
            // for (Shift+Enter → CSI 13;2u, Ctrl+Enter → CSI 13;5u); if it set
            // DECCKM, upgrade cursor keys to their SS3 application encodings.
            // The two touch disjoint keys, so the order is immaterial.
            let bytes = input::kitty_upgrade(key, bytes, app.focused_kitty());
            let bytes = input::app_cursor_upgrade(key, bytes, app.focused_app_cursor());
            app.forward_bytes(&bytes);
        }
        InputResult::Ignore => {}
    }
}

/// Route mouse events. Tab-bar clicks switch tabs. Over a pane: a left press
/// focuses it; wheel and (for mouse-aware apps) clicks/drags are forwarded to
/// the inner app, otherwise the wheel scrolls roost's own scrollback.
fn handle_mouse<B: PaneBackend>(app: &mut App<B>, me: crossterm::event::MouseEvent) {
    use crossterm::event::{MouseButton, MouseEventKind};

    // Copy mode owns the mouse: drag selects text, release copies.
    if app.in_copy_mode() {
        // Selection-freeze design audit D1: copy mode can be entered
        // mid-drag (a keypress interleaves with the still-held button) and
        // this early return means a Normal-mode gesture's Up never reaches
        // the P20 latch code below at all — nothing else will ever release
        // it. `release_mouse_gesture` is a no-op when nothing is latched,
        // so this costs nothing on every other copy-mode mouse event.
        app.release_mouse_gesture();
        handle_copy_mouse(app, me);
        return;
    }

    // U8(a): so does a modal (Rename/Picker/Help/Feed) — gated here, ahead
    // of every pane and tab-bar path below, so a click can't move focus or
    // switch a tab beneath the dialog and the wheel can't scroll the pane
    // under an overlay. `modal_rect` hands over the dialog's *drawn* rect
    // so the hit-test matches what's on screen.
    if app.modal_active() {
        // Selection-freeze design audit D1: same reasoning as copy mode
        // above — a modal opened mid-drag (RenamePane, QuickLaunch, Help,
        // ToggleFeed, ToggleRoster) swallows every further mouse event for
        // this pane too.
        app.release_mouse_gesture();
        let dialog = ui::render::modal_rect(app);
        app.handle_modal_mouse(me, dialog);
        return;
    }

    // Tab bar (top row): click a tab to switch to it. Clicks past the last
    // visible tab — the `…` overflow marker, the gap, or the right-aligned
    // status area — hit no tab; `mouse::tab_at_x` clamps to what's actually
    // drawn (C2).
    if me.row == 0 {
        if matches!(me.kind, MouseEventKind::Down(MouseButton::Left)) {
            let names: Vec<String> = app.ws.tabs.iter().map(|t| t.name.clone()).collect();
            let bar_width = app.body_area().width;
            // U15: the status area may now carry a mode word, which widens
            // it — hit-testing reads the exact same fit the renderer drew
            // (§4/§5 lockstep) so the clickable span can't drift.
            let cwd = app.focused_cwd();
            let status_w = mouse::status_fit(
                ui::render::tab_status_word(app),
                cwd.as_deref(),
                app.last_save_ok(),
                &names,
                bar_width,
            )
            .map(|f| f.width)
            .unwrap_or(0);
            if let Some(i) =
                mouse::tab_at_x(&names, bar_width, status_w, app.ws.active_tab, me.column)
            {
                app.apply(input::Action::GoToTab(i));
            }
        }
        return;
    }

    // U21: a left press on the seam between two panes drags that border
    // instead of focusing. Checked ahead of the pane paths below because a
    // pane's rect *includes* its border (C3), so hit-testing would happily
    // claim the seam for one of the two panes and forward the drag into it.
    // Only a *shared* border is a seam — a pane's outer edge, against the
    // body, still focuses like any other cell of it.
    match me.kind {
        MouseEventKind::Down(MouseButton::Left) if app.begin_seam_drag(me.column, me.row) => {
            return;
        }
        MouseEventKind::Drag(_) if app.seam_dragging() => {
            app.drag_seam(me.column, me.row);
            return;
        }
        MouseEventKind::Up(_) if app.seam_dragging() => {
            app.end_seam_drag();
            return;
        }
        _ => {}
    }

    // C21/§5: zoom-aware — while zoomed, the display list is just the
    // zoomed pane, so body clicks/wheel can only ever hit it.
    let rects = app.display_rects();
    // P20: a gesture belongs to the pane its button-down landed in, for as
    // long as it lasts. Re-hit-testing every event meant a drag that crossed
    // a border switched target mid-gesture — the origin app never saw its
    // release (left stuck mid-selection) while the neighbour got orphan
    // drag/up events for a press it never received. Copy mode already
    // latched this way (`handle_copy_mouse` follows `selection.pane`); this
    // is the same rule for the forwarding path. Coordinates outside the
    // latched pane are clamped to its grid by `mouse::route_mouse`.
    let pane = match me.kind {
        MouseEventKind::Down(_) => {
            let hit = mouse::hit_test(&rects, me.column, me.row);
            app.set_mouse_latch(hit.map(|p| p.id));
            hit
        }
        // Follow the latch wherever the pointer wandered. No latch means the
        // press never landed in a pane (the tab bar, a modal, outside the
        // body) — an orphan drag/release belongs to nobody.
        MouseEventKind::Drag(_) | MouseEventKind::Up(_) => {
            app.mouse_latch().and_then(|id| rects.iter().find(|p| p.id == id).copied())
        }
        // Wheel and bare motion aren't part of a button gesture.
        _ => mouse::hit_test(&rects, me.column, me.row),
    };
    let Some(pane) = pane else {
        // Selection-freeze design audit D1: an Up whose latched pane no
        // longer resolves in `rects` — a tab switch or C31 cross-tab focus
        // move mid-drag, `rects`/`display_rects` are active-tab-only — is
        // an orphan release. Nothing past this point will ever see it, so
        // release here: there is no extraction to protect (that only
        // happens once `pane` resolves below), so it's safe to clear the
        // latch and unfreeze together.
        if matches!(me.kind, MouseEventKind::Up(_)) {
            app.release_mouse_gesture();
        }
        return;
    };
    if matches!(me.kind, MouseEventKind::Up(_)) {
        app.set_mouse_latch(None); // the gesture ends here, whatever it hit
    }

    // Alt+click a URL to open it in the browser (roost owns the Alt layer).
    if matches!(me.kind, MouseEventKind::Down(MouseButton::Left))
        && me.modifiers.contains(crossterm::event::KeyModifiers::ALT)
        && !pane.collapsed
    {
        let (r, c) = inner_cell(pane.rect, me.column, me.row);
        if let Some(url) = app.url_at(pane.id, r, c) {
            // S3 (PR #46 code review): opening a URL is its own complete
            // gesture over *this* pane, latched by the P20 logic above like
            // any other press — it must not leave a different pane's
            // lingering native-selection highlight around for a later,
            // unrelated drag/release on this latch to extend or re-copy.
            // (A miss falls through to the ordinary click path below, whose
            // `on_click` already clears a foreign-pane selection — see the
            // "Alt-click that misses" bullet in C29.)
            app.selection = None;
            infra::open::open_url(&url);
            return;
        }
    }

    // A left press focuses the pane under the cursor (expands stack members).
    if matches!(me.kind, MouseEventKind::Down(MouseButton::Left)) {
        app.on_click(pane.id);
    }

    // P9: the wheel's destination depends on more than the mouse protocol —
    // an alternate-screen app with no protocol gets arrow keys, since its
    // grid has no scrollback for roost to move.
    let state = app
        .runtimes
        .get(&pane.id)
        .map(|rt| mouse::PaneMouseState {
            proto: rt.mouse_proto(),
            alternate_screen: rt.alternate_screen(),
            app_cursor_keys: rt.app_cursor_keys(),
        })
        .unwrap_or_default();

    // C29: native text selection — drag-select, double/triple-click,
    // shift-click-extend — over a pane whose app never asked for the mouse.
    // Rides the exact `pane` the P20 latch above already resolved, so a
    // selection drag clamps to the originating pane for free. A mouse-aware
    // pane (`state.proto != None`) is untouched: `route_mouse` below still
    // owns it completely, exactly as it did before this feature existed.
    // D1 (PR #46 design audit): scoped to `Mode::Normal` explicitly — the
    // contract always said "in Normal mode" but nothing enforced it, so a
    // drag during Scroll mode (neither of the two early returns above
    // catches it: it isn't Copy mode and isn't a C12 modal) silently
    // selected and copied too, while the keypress-clear stayed
    // `Mode::Normal`-gated and so couldn't clean it up until Scroll mode
    // was left. Normal-only is the one the contract already promised, and
    // it's what makes the clear-on-keypress guard's own gate consistent
    // with what could ever create a selection in the first place — the
    // alternative (allow it in Scroll, since a frozen view is exactly what
    // Copy mode already permits selecting from via its own Alt+c handoff)
    // would need contracting *and* would need the clear path widened too,
    // for a mode nobody asked native selection to cover.
    if !pane.collapsed && state.proto == MouseProto::None && matches!(app.mode, Mode::Normal) {
        handle_native_selection(app, &pane, &me);
    }

    // Selection-freeze design audit D1: `pane` unfreezes on every Up that
    // reaches this point, whether or not the gate above let
    // `handle_native_selection` run — Scroll/Search mode, an SGR flip, or
    // the pane collapsing mid-drag all fail it but must not leave the pane
    // frozen just because none of those conditions held any more. Runs
    // *after* `handle_native_selection`'s own Up arm, so a completing
    // gesture's `grab_text` still read the frozen frame it started on
    // before this drops it — extraction, then release, never the other way.
    if matches!(me.kind, MouseEventKind::Up(_)) {
        unfreeze_native_selection(app, pane.id);
    }

    match mouse::route_mouse(state, &pane, &me) {
        MouseAction::Forward(bytes) => app.forward_mouse(pane.id, &bytes),
        MouseAction::Scroll(delta) => app.wheel_scroll(pane.id, delta),
        MouseAction::None => {}
    }
}

/// C29: native text selection over a pane whose app never asked for the
/// mouse (`MouseProto::None`) — the macOS reflexes that were dead gestures
/// before: drag-select, double/triple-click word/line, shift-click-extend.
/// Paints through the same `app.selection` / `highlight_selection` (C17)
/// Copy mode uses, and releases through the same U14 clipboard-outcome
/// flash; the full gesture contract is C29. Only ever called for a
/// `MouseProto::None` pane — the call site in `handle_mouse` gates that.
///
/// Double/triple-click interval and tolerance: crossterm reports no click
/// count at all, so `App::click_count` derives it from timing (macOS's
/// standard 500ms double-click window, `DOUBLE_CLICK_INTERVAL`) and a
/// 1-cell position tolerance (`CLICK_TOLERANCE`) — a real press drifts a
/// pixel even when the user means the same character.
///
/// Selection-freeze amendment (C29, DESIGN-ui.md): every `Down` below
/// freezes the pane's presented view (`PaneBackend::freeze_view`) *before*
/// touching `Selection`, so double/triple-click's own word/line lookups
/// and every later `Drag`/`Up` in this gesture read the identical still
/// frame — one freeze, armed once, not a special case per gesture shape.
/// A wheel tick mid-drag drops it immediately (handled below), since
/// scrolling and freezing disagree about whether the view may move.
///
/// **[Amended, design audit D1]** Release is deliberately NOT this
/// function's job for the ordinary Up (see that arm's own doc) — the
/// freeze's lifetime is the P20 latch's lifetime (`App::mouse_latch`), and
/// the latch is cleared in exactly one place regardless of mode, protocol
/// or collapse state (`handle_mouse`), so that's also where the unfreeze
/// lives: an unconditional call right after this function returns for a
/// resolved `pane`, plus two more call sites in `handle_mouse` for the
/// gestures this function never even gets a chance to see the Up of —
/// copy mode or a modal taking the mouse mid-drag (`App::release_mouse_gesture`,
/// which also covers an orphan Up whose latched pane no longer resolves in
/// `rects`: a tab switch or C31 cross-tab focus move). Before this, the
/// freeze only ever released from *inside* this function, which
/// `handle_mouse` only reaches under several gates (`!collapsed`,
/// `MouseProto::None`, `Mode::Normal`, the pane found in `rects`) — every
/// one of them a way to leave a pane frozen with no release in sight.
/// `GESTURE_FREEZE_STALE_CAP` (`infra::pty`) remains a pure backstop for a
/// gesture that is neither released nor abandoned through any of those
/// paths — a genuinely stuck `Up` — not the mechanism that makes release
/// happen in the first place.
fn handle_native_selection<B: PaneBackend>(
    app: &mut App<B>,
    pane: &PaneRect,
    me: &crossterm::event::MouseEvent,
) {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
    let (r, c) = inner_cell(pane.rect, me.column, me.row);
    match me.kind {
        MouseEventKind::Down(MouseButton::Left) if me.modifiers.contains(KeyModifiers::SHIFT) => {
            freeze_native_selection(app, pane.id);
            app.extend_selection_to(pane.id, r, c);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            freeze_native_selection(app, pane.id);
            match app.click_count(pane.id, r, c) {
                2 => app.select_word_at(pane.id, r, c),
                3 => app.select_line_at(pane.id, r),
                _ => app.begin_selection(pane.id, r, c),
            }
        }
        // S3 (PR #46 code review): only ever touch a selection that
        // belongs to *this* gesture's pane. The Alt+click-URL branch above
        // latches a new pane and can `return` before `on_click` gets a
        // chance to clear a lingering selection from a different one
        // (fixed there too); guarding here as well means a Drag/Up can
        // never extend or re-copy a pane's selection it was never about,
        // regardless of how it got left stale.
        MouseEventKind::Drag(MouseButton::Left)
            if app.selection.is_some_and(|s| s.pane == pane.id) =>
        {
            app.extend_selection(r, c);
        }
        MouseEventKind::Up(MouseButton::Left) => {
            // release_native_selection (D3/S4, PR #46) owns the "did this
            // gesture actually select anything" and "should this commit
            // now or wait for a possible 3rd click" decisions — see its
            // doc. Highlight intentionally left showing on a commit: the
            // contract says it stays lit until the next click
            // (`App::on_click`) or keypress (`App::handle_mode_key`).
            //
            // Selection-freeze design audit D1: the freeze is deliberately
            // NOT dropped here — `handle_mouse`'s own unconditional
            // post-dispatch unfreeze (right after this function returns)
            // is the one release site for a resolved `pane`'s Up, so every
            // abandonment path (Scroll mode, an SGR flip, a collapse) that
            // skips this whole function still releases it the same way a
            // clean release does. Extraction above still reads the frozen
            // frame either way — that unfreeze can only run after this
            // function has already returned.
            if app.selection.is_some_and(|s| s.pane == pane.id) {
                if let Some(text) = app.release_native_selection() {
                    let outcome = infra::clipboard::copy(&text); // U14
                    app.flash_copy(text.chars().count(), outcome);
                }
            }
        }
        // Selection-freeze amendment: a wheel tick mid-drag means the view
        // is about to move on purpose — drop the freeze rather than define
        // what a frozen pane scrolling would even mean.
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            unfreeze_native_selection(app, pane.id);
        }
        _ => {}
    }
}

/// Selection-freeze amendment (C29): arm the presentation freeze for `id`
/// at the start of a native-selection gesture. A no-op if the pane has
/// since exited.
fn freeze_native_selection<B: PaneBackend>(app: &mut App<B>, id: PaneId) {
    if let Some(rt) = app.runtimes.get_mut(&id) {
        rt.freeze_view();
    }
}

/// Selection-freeze amendment (C29): release it — the gesture's `Up`, a
/// wheel tick mid-drag, or (lazily, inside `presented()`) staleness.
fn unfreeze_native_selection<B: PaneBackend>(app: &mut App<B>, id: PaneId) {
    if let Some(rt) = app.runtimes.get_mut(&id) {
        rt.unfreeze_view();
    }
}

/// Copy-mode mouse: left-drag selects text within the pane it started in;
/// release extracts the selection and copies it to the system clipboard.
fn handle_copy_mouse<B: PaneBackend>(app: &mut App<B>, me: crossterm::event::MouseEvent) {
    use crossterm::event::{MouseButton, MouseEventKind};
    let rects = app.display_rects(); // C21/§5: zoom-aware, same as handle_mouse
    match me.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(pane) = mouse::hit_test(&rects, me.column, me.row) {
                if !pane.collapsed {
                    let (r, c) = inner_cell(pane.rect, me.column, me.row);
                    app.begin_selection(pane.id, r, c);
                }
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(sel) = app.selection {
                if let Some(pane) = rects.iter().find(|p| p.id == sel.pane) {
                    let (r, c) = inner_cell(pane.rect, me.column, me.row);
                    app.extend_selection(r, c);
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if let Some(text) = app.finish_selection() {
                let outcome = infra::clipboard::copy(&text); // U14
                app.flash_copy(text.chars().count(), outcome);
            }
        }
        _ => {}
    }
}

/// Screen (col, row) → 0-based cell inside a pane's border-excluded area,
/// clamped to the inner bounds.
fn inner_cell(rect: ratatui::layout::Rect, col: u16, row: u16) -> (u16, u16) {
    let iw = rect.width.saturating_sub(2).max(1);
    let ih = rect.height.saturating_sub(2).max(1);
    let c = col.saturating_sub(rect.x + 1).min(iw - 1);
    let r = row.saturating_sub(rect.y + 1).min(ih - 1);
    (r, c)
}

#[cfg(test)]
mod tests {

    /// rev P1-2: only the security refusal may be fatal. Every other
    /// listener failure has to degrade to "no control plane, and roost says
    /// so" — a bind() failure taking the whole session down would be a worse
    /// outcome than the missing socket it is reporting.
    ///
    /// P2: the fatal fixture is built from `sock::UNSAFE_SOCKET_DIR_MSG`,
    /// the same const the real `bail!` in `spawn_listener` uses, instead of
    /// a hand-typed copy of its wording. A hand-typed copy can never catch
    /// the check drifting from what `sock.rs` actually says — it would just
    /// as happily go on matching itself. Sharing the identifier makes that
    /// divergence impossible.
    #[test]
    fn only_an_unsafe_socket_dir_is_a_fatal_listener_failure() {
        use crate::infra::sock::UNSAFE_SOCKET_DIR_MSG;
        let unsafe_dir = anyhow::anyhow!("roost: socket directory /tmp/x has {UNSAFE_SOCKET_DIR_MSG}");
        assert!(super::is_unsafe_socket_dir(&unsafe_dir));

        for benign in [
            "No such file or directory (os error 2)",
            "path must be shorter than SUN_LEN",
            "Permission denied (os error 13)",
        ] {
            let e = anyhow::anyhow!(benign.to_string());
            assert!(
                !super::is_unsafe_socket_dir(&e),
                "{benign} must not be fatal — it should flash, not exit"
            );
        }

        // The real bail! is wrapped in context by the time main sees it; the
        // check reads the whole chain, so a wrapped one still counts.
        let wrapped = anyhow::anyhow!("roost: socket directory /tmp/x has {UNSAFE_SOCKET_DIR_MSG}")
            .context("starting the control listener");
        assert!(super::is_unsafe_socket_dir(&wrapped));
    }
    use super::*;
    use crate::core::app::Mode;
    use crate::core::workspace::Workspace;
    use crate::ports::fakes::{FakePane, MemStore};
    use crate::ui::input::Action;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::Size;
    use std::path::PathBuf;

    fn mk_app() -> App<FakePane> {
        let store = MemStore::default();
        let (tx, _rx) = std::sync::mpsc::sync_channel(64);
        let ws = Workspace::default_in(PathBuf::from("/tmp"));
        App::<FakePane>::new(
            ws,
            agents::registry(),
            Box::new(store),
            tx,
            Size::new(100, 30),
            (0, 0),
            None,
            TokenTable::new().unwrap(),
        )
        .unwrap()
    }

    fn alt(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::ALT)
    }
    fn alt_shift(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::ALT | KeyModifiers::SHIFT)
    }

    /// C23 table-driven proof: with the focused pane raw, every currently
    /// bound Alt chord (the toggle itself excluded) forwards as meta-ESC
    /// bytes instead of applying its normal action — `input::encode_raw` is
    /// the oracle (its own correctness is pinned exhaustively in
    /// `ui::input`'s tests); this test is the *routing* proof, that
    /// `handle_key` actually reaches it while raw instead of `translate()`'s
    /// Action/swallow path.
    #[test]
    fn raw_pane_forwards_every_current_action_chord_as_bytes() {
        let chords = [
            alt(KeyCode::Char('q')),
            alt(KeyCode::Char('n')),
            alt(KeyCode::Char('w')),
            alt(KeyCode::Char('t')),
            alt(KeyCode::Char('s')),
            alt(KeyCode::Char('o')),
            alt(KeyCode::Char('r')),
            alt_shift(KeyCode::Char('r')),
            alt(KeyCode::Char('R')),
            alt(KeyCode::Enter),
            alt(KeyCode::Char('/')),
            alt(KeyCode::Char('c')),
            alt(KeyCode::Char('u')),
            alt(KeyCode::Char('?')),
            alt(KeyCode::Char('a')),
            alt(KeyCode::Char('z')),
            alt(KeyCode::Char('g')),
            alt(KeyCode::Char('e')),
            alt(KeyCode::Char('f')),
            alt(KeyCode::PageUp),
            alt(KeyCode::Char('5')),
            alt(KeyCode::Char('0')),
            alt(KeyCode::Char('i')),
            alt(KeyCode::Char('m')),
            alt(KeyCode::Right),
            alt(KeyCode::Left),
            alt(KeyCode::Up),
            alt(KeyCode::Down),
            alt(KeyCode::Char('h')),
            alt(KeyCode::Char('j')),
            alt(KeyCode::Char('k')),
            alt(KeyCode::Char('l')),
            alt_shift(KeyCode::Right),
            alt_shift(KeyCode::Left),
            alt_shift(KeyCode::Up),
            alt_shift(KeyCode::Down),
        ];
        for key in chords {
            let mut app = mk_app();
            let focused = app.focused;
            app.apply(Action::ToggleRaw);
            assert!(app.raw_routing_active(), "setup: pane should be raw + Normal + alive");

            let expected = input::encode_raw(key);
            handle_key(&mut app, key);

            assert_eq!(
                app.runtimes.get(&focused).unwrap().input,
                expected,
                "key {key:?} must forward as meta-ESC bytes while raw, not apply its action"
            );
            // The action itself must never have fired (Quit is the sharpest
            // canary: if raw routing leaked to translate()'s Action arm,
            // Alt+q would set this instead of forwarding bytes).
            assert!(!app.quit);
        }
    }

    #[test]
    fn only_the_toggle_chord_exits_raw_mode_and_is_never_itself_forwarded() {
        let mut app = mk_app();
        let focused = app.focused;
        app.apply(Action::ToggleRaw);
        assert!(app.raw_routing_active());

        handle_key(&mut app, alt_shift(KeyCode::Char('p')));
        assert!(!app.raw_routing_active(), "Alt+Shift+p must exit raw");
        assert!(app.runtimes.get(&focused).unwrap().input.is_empty(), "the toggle chord itself must not forward");

        // Re-enter, then confirm the uppercase-delivery tolerance also exits.
        app.apply(Action::ToggleRaw);
        handle_key(&mut app, alt(KeyCode::Char('P')));
        assert!(!app.raw_routing_active(), "Alt+'P' tolerance must also exit raw");
    }

    #[test]
    fn dead_raw_pane_falls_back_to_dead_pane_keys_instead_of_forwarding() {
        let mut app = mk_app();
        let focused = app.focused;
        app.apply(Action::ToggleRaw);
        app.runtimes.get_mut(&focused).unwrap().kill(); // simulate the process exiting
        assert!(app.focused_dead());
        assert!(!app.raw_routing_active(), "a dead pane must not intercept via raw routing");

        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!app.focused_dead(), "Enter must relaunch the dead raw pane, not forward \\r to it");
    }

    /// `y` on a dead pane copies the pasteable resume command — and must
    /// never relaunch. Without a session pointer it is inert (no flash),
    /// matching the bars, which only advertise `y` when one exists.
    #[test]
    fn dead_pane_y_copies_resume_command_only_when_resumable() {
        let mut app = mk_app();
        let focused = app.focused;
        app.runtimes.get_mut(&focused).unwrap().kill();
        assert!(app.focused_dead());

        // Session-less (shell) pane: inert.
        handle_key(&mut app, KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(app.focused_dead(), "y must not relaunch");
        assert!(app.flash().is_none(), "nothing to copy, nothing to claim");

        // Plant a resume pointer, then y copies and flashes the outcome —
        // and still doesn't relaunch.
        {
            let spec = app.ws.tabs[0].panes.get_mut(&focused).unwrap();
            spec.adapter = "pi".into();
            spec.session = Some("019fe044".into());
        }
        handle_key(&mut app, KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(app.focused_dead(), "y copies, never relaunches");
        let flash = app.flash().expect("copy must flash its outcome");
        assert!(flash.starts_with("copied"), "got: {flash}");
    }

    #[test]
    fn cooked_panes_are_completely_unaffected_by_raw_routing() {
        let mut app = mk_app();
        assert!(!app.raw_routing_active());
        handle_key(&mut app, alt(KeyCode::Char('q')));
        assert!(app.quit, "Alt+q must still quit a cooked pane");
    }

    /// The atuin regression: zsh's line editor switches DECCKM on via `smkx`
    /// while the prompt is active and binds widgets to `$terminfo[kcuu1]` =
    /// `\EOA` (atuin's up-arrow search among them) — a pane in that state
    /// must receive SS3 arrows, not the normal-mode `\E[A`, or the binding
    /// never fires. Cooked and raw routing alike.
    #[test]
    fn arrows_follow_the_focused_panes_application_cursor_mode() {
        let mut app = mk_app();
        let focused = app.focused;
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);

        handle_key(&mut app, up);
        assert_eq!(app.runtimes.get(&focused).unwrap().input, b"\x1b[A");

        app.runtimes.get_mut(&focused).unwrap().app_cursor = true;
        app.runtimes.get_mut(&focused).unwrap().input.clear();
        handle_key(&mut app, up);
        assert_eq!(app.runtimes.get(&focused).unwrap().input, b"\x1bOA");

        // Raw routing forwards plain arrows through the same upgrade...
        app.apply(Action::ToggleRaw);
        app.runtimes.get_mut(&focused).unwrap().input.clear();
        handle_key(&mut app, up);
        assert_eq!(app.runtimes.get(&focused).unwrap().input, b"\x1bOA");
        // ...while Alt+Up keeps its meta-ESC + CSI form: a modified cursor
        // key is never sent as SS3, DECCKM or not.
        app.runtimes.get_mut(&focused).unwrap().input.clear();
        handle_key(&mut app, alt(KeyCode::Up));
        assert_eq!(app.runtimes.get(&focused).unwrap().input, b"\x1b\x1b[A");
    }

    // ---- U8: modals own the non-keyboard input surface -------------------

    fn click(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }
    fn wheel_up(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }
    fn wheel_down(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// U8(a), the live-QA reproduction: Alt+r on one pane, a click on
    /// another, Enter. The title must land on the pane the dialog opened on
    /// — before the gate the click moved focus and Enter committed `ZZZ` to
    /// the *clicked* pane (SPEC-ux frame E1).
    #[test]
    fn a_click_during_rename_neither_moves_focus_nor_redirects_the_commit() {
        let mut app = mk_app();
        app.apply(Action::NewPane); // panes 1 | 2, focus 2
        let target = app.focused;
        app.apply(Action::RenamePane);
        for c in "ZZZ".chars() {
            app.handle_mode_key(key(KeyCode::Char(c)));
        }
        handle_mouse(&mut app, click(5, 5)); // pane 1's area, outside the dialog

        assert_eq!(app.focused, target, "the click must not move focus beneath the modal");
        assert!(matches!(app.mode, Mode::Rename { .. }), "the dialog must still be up");

        app.handle_mode_key(key(KeyCode::Enter));
        assert_eq!(app.find_spec(target).and_then(|s| s.title.clone()), Some("ZZZ".into()));
        assert_eq!(app.find_spec(1).and_then(|s| s.title.clone()), None, "pane 1 must be untouched");
    }

    /// A modal click never reaches the tab bar either (row 0).
    #[test]
    fn a_click_on_the_tab_bar_during_a_modal_switches_nothing() {
        let mut app = mk_app();
        app.apply(Action::NewTab); // two tabs, active = 1
        app.apply(Action::Help);
        handle_mouse(&mut app, click(1, 0)); // tab 1's cells
        assert_eq!(app.ws.active_tab, 1, "tab switching must not happen under a modal");
    }

    /// U8(c): the wheel belongs to the feed while it's open — it pages the
    /// overlay and leaves every pane at its live tail.
    #[test]
    fn the_wheel_pages_the_feed_and_never_the_pane_under_it() {
        let mut app = mk_app();
        for _ in 0..3 {
            app.apply(Action::NewPane); // each spawn banks one feed entry
        }
        let pane = app.focused;
        app.apply(Action::ToggleFeed);
        handle_mouse(&mut app, wheel_up(5, 5)); // over a pane, feed open

        match app.mode {
            Mode::Feed { offset } => assert!(offset > 0, "the wheel must scroll the feed back"),
            _ => panic!("the feed must stay open"),
        }
        assert_eq!(app.runtimes.get(&pane).unwrap().scrollback, 0, "the pane must stay live");
    }

    /// C27/U8: the roster owns the wheel too — it moves the roster's own
    /// cursor and the pane underneath stays at its live tail.
    #[test]
    fn the_wheel_moves_the_roster_cursor_and_never_the_pane_under_it() {
        let mut app = mk_app();
        app.apply(Action::NewPane); // panes 1|2, focus 2
        app.apply(Action::ToggleRoster);
        let opened_on = match app.mode {
            Mode::Roster { cursor, .. } => cursor,
            _ => panic!("the roster must be open"),
        };
        handle_mouse(&mut app, wheel_up(5, 5)); // over a pane, roster open

        match app.mode {
            Mode::Roster { cursor, .. } => {
                assert_ne!(cursor, opened_on, "the wheel moves the roster's cursor")
            }
            _ => panic!("the roster must stay open"),
        }
        for id in [1u64, 2] {
            assert_eq!(app.runtimes.get(&id).unwrap().scrollback, 0, "pane {id} stays live");
        }
    }

    /// C27: a click on a pane row goes there in one press; a click on a group
    /// header does nothing (it is a label); a click outside dismisses.
    #[test]
    fn a_roster_row_click_jumps_and_a_header_click_does_nothing() {
        let mut app = mk_app();
        app.apply(Action::NewPane); // panes 1|2 in tab 0
        app.apply(Action::NewTab); // tab 1, pane 3, active
        app.apply(Action::ToggleRoster);
        let rect = ui::render::modal_rect(&app).expect("the roster draws a dialog");

        // Row 0 of the list is tab 0's group header.
        handle_mouse(&mut app, click(rect.x + 2, rect.y + 1));
        assert!(matches!(app.mode, Mode::Roster { .. }), "a header click does nothing");

        // Row 1 is tab 0's first pane — a pane in a tab that is NOT active.
        handle_mouse(&mut app, click(rect.x + 2, rect.y + 2));
        assert!(matches!(app.mode, Mode::Normal), "the jump closes the roster");
        assert_eq!(app.focused, 1);
        assert_eq!(app.ws.active_tab, 0, "and switched tabs to get there");

        app.apply(Action::ToggleRoster);
        let below = app.body_area().bottom() - 1;
        handle_mouse(&mut app, click(0, below));
        assert!(matches!(app.mode, Mode::Normal), "a click outside closes the roster");
    }

    /// Every other modal swallows the wheel outright.
    #[test]
    fn the_wheel_is_swallowed_by_the_other_modals() {
        for open in [Action::RenamePane, Action::QuickLaunch, Action::Help] {
            let mut app = mk_app();
            let pane = app.focused;
            app.apply(open);
            handle_mouse(&mut app, wheel_up(5, 5));
            assert_eq!(
                app.runtimes.get(&pane).unwrap().scrollback,
                0,
                "{open:?} must not let the wheel reach the pane"
            );
        }
    }

    /// C14 + U8: clicking a picker row selects and launches that adapter;
    /// clicking outside cancels, like Esc.
    #[test]
    fn a_picker_row_click_launches_it_and_an_outside_click_cancels() {
        let mut app = mk_app();
        app.apply(Action::QuickLaunch);
        let rect = ui::render::modal_rect(&app).expect("the picker draws a dialog");
        handle_mouse(&mut app, click(rect.x, rect.y)); // the border: neither row nor outside
        assert!(matches!(app.mode, Mode::Picker { .. }), "a border click does nothing");

        let below = app.body_area().bottom() - 1;
        handle_mouse(&mut app, click(0, below)); // outside
        assert!(matches!(app.mode, Mode::Normal), "an outside click cancels the picker");
        assert_eq!(app.rects().len(), 1, "cancelling launches nothing");

        app.apply(Action::QuickLaunch);
        let rect = ui::render::modal_rect(&app).expect("the picker draws a dialog");
        let items = crate::core::app::picker_items();
        handle_mouse(&mut app, click(rect.x + 1, rect.y + 1 + (items.len() - 1) as u16));
        assert!(matches!(app.mode, Mode::Normal), "launching leaves the modal");
        assert_eq!(app.rects().len(), 2, "the clicked row spawned a pane");
        let spawned = app.focused;
        assert_eq!(app.find_spec(spawned).map(|s| s.adapter.clone()), Some(items[items.len() - 1].into()));
    }

    // ---- U21: drag a shared border to resize -----------------------------

    fn drag_to(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }
    fn release_at(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// U21: press the seam between two side-by-side panes, drag right, and
    /// the border follows the pointer — the split really moves, and the
    /// pane that grew is the one on the left of the seam.
    #[test]
    fn dragging_a_shared_border_resizes_the_split() {
        let mut app = mk_app();
        app.apply(Action::NewPane); // two panes, side by side at 100 cols
        let order = app.pane_order();
        let (left, right) = (order[0], order[1]);
        let rects = app.display_rects();
        let lr = rects.iter().find(|p| p.id == left).unwrap().rect;
        let rr = rects.iter().find(|p| p.id == right).unwrap().rect;
        assert_eq!(rr.x, lr.x + lr.width, "the fixture really is a vertical split");
        let seam = rr.x;
        let before = lr.width;

        handle_mouse(&mut app, click(seam, lr.y + 2));
        assert!(app.seam_dragging(), "the press on the seam started a drag");
        handle_mouse(&mut app, drag_to(seam + 10, lr.y + 2));
        handle_mouse(&mut app, release_at(seam + 10, lr.y + 2));
        assert!(!app.seam_dragging(), "the release ended it");

        let after = app.display_rects().iter().find(|p| p.id == left).unwrap().rect.width;
        assert!(after > before, "the left pane grew: {before} → {after}");
        // Closed loop on the drawn geometry: the border should land on the
        // pointer, not somewhere proportionally short of it.
        let landed = app.display_rects().iter().find(|p| p.id == right).unwrap().rect.x;
        assert!(
            landed.abs_diff(seam + 10) <= 1,
            "the border tracked the pointer: wanted {}, got {landed}",
            seam + 10,
        );
    }

    /// …and it does not steal focus. Grabbing a border is an act on the
    /// layout, not a choice of which pane to type into.
    #[test]
    fn a_seam_drag_leaves_focus_where_it_was() {
        let mut app = mk_app();
        app.apply(Action::NewPane);
        let order = app.pane_order();
        let focused = app.focused;
        let other = order.iter().copied().find(|id| *id != focused).unwrap();
        let rects = app.display_rects();
        let seam = rects.iter().find(|p| p.id == other).unwrap().rect;
        let lr = rects.iter().find(|p| p.id == focused).unwrap().rect;
        // Whichever side the unfocused pane is on, its inner edge is a seam.
        let (col, row) = if seam.x > lr.x { (seam.x, seam.y + 2) } else { (lr.x, lr.y + 2) };
        handle_mouse(&mut app, click(col, row));
        handle_mouse(&mut app, drag_to(col + 3, row));
        handle_mouse(&mut app, release_at(col + 3, row));
        assert_eq!(app.focused, focused, "the drag moved the border, not the focus");
    }

    /// A pane's *outer* border — the one against the body edge, shared with
    /// nothing — is not a seam, so clicking it still focuses that pane like
    /// any other cell of it.
    #[test]
    fn an_outer_border_is_not_a_seam_and_still_focuses() {
        let mut app = mk_app();
        app.apply(Action::NewPane);
        let order = app.pane_order();
        let first = order[0];
        assert_ne!(app.focused, first, "NewPane focuses the pane it created");
        let body = app.body_area();
        let r = app.display_rects().iter().find(|p| p.id == first).unwrap().rect;
        // The far-left column of the leftmost pane touches the body edge.
        assert_eq!(r.x, body.x);
        handle_mouse(&mut app, click(r.x, r.y + 2));
        assert!(!app.seam_dragging(), "no drag started on an outer border");
        assert_eq!(app.focused, first, "the click focused the pane");
    }

    /// C21: a zoomed pane fills the body and has no seam to grab — the
    /// click must fall through to the ordinary pane path rather than
    /// latching a drag onto geometry that isn't there.
    #[test]
    fn a_zoomed_pane_has_no_seam() {
        let mut app = mk_app();
        app.apply(Action::NewPane);
        app.apply(Action::ToggleZoom);
        assert!(app.zoomed());
        let r = app.display_rects()[0].rect;
        handle_mouse(&mut app, click(r.x + r.width - 1, r.y + 2));
        assert!(!app.seam_dragging());
    }

    /// Help closes on any click (C15's "any key closes it", in mouse form);
    /// the feed closes on a click outside it and ignores one inside.
    #[test]
    fn help_closes_on_any_click_and_the_feed_only_on_an_outside_one() {
        let mut app = mk_app();
        app.apply(Action::Help);
        let rect = ui::render::modal_rect(&app).expect("help draws a dialog");
        // C15 (amended): the wheel reads on when the keymap is taller than
        // the overlay — it must not be mistaken for a dismissal, or the one
        // gesture for "show me more" would throw the list away.
        let (visible, total) = ui::render::help_scroll_extent(app.body_area());
        assert!(visible < total, "this fixture's keymap is scrolled");
        handle_mouse(&mut app, wheel_down(rect.x + 1, rect.y + 1));
        assert!(matches!(app.mode, Mode::Help { top } if top > 0), "the wheel scrolls the keymap");
        handle_mouse(&mut app, wheel_up(rect.x + 1, rect.y + 1));
        assert!(matches!(app.mode, Mode::Help { top: 0 }), "…and back up");
        handle_mouse(&mut app, click(rect.x + 1, rect.y + 1));
        assert!(matches!(app.mode, Mode::Normal), "any click dismisses help");

        app.apply(Action::ToggleFeed);
        let rect = ui::render::modal_rect(&app).expect("the feed draws a dialog");
        handle_mouse(&mut app, click(rect.x + 1, rect.y + 1));
        assert!(matches!(app.mode, Mode::Feed { .. }), "a click inside the feed keeps it open");
        let below = app.body_area().bottom() - 1;
        handle_mouse(&mut app, click(0, below));
        assert!(matches!(app.mode, Mode::Normal), "a click outside closes the feed");
    }

    // ---- P20: gestures latch to the pane they started in -----------------

    fn drag(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }
    fn release(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// P20: a drag that crosses a pane border stays with the pane it started
    /// in. Every event used to be re-hit-tested, so the origin app never saw
    /// its release (left stuck mid-selection) while the neighbour got orphan
    /// drag/up events for a press it never received.
    #[test]
    fn a_drag_that_crosses_a_border_stays_with_the_pane_it_started_in() {
        let mut app = mk_app();
        app.apply(Action::NewPane); // panes 1 | 2, side by side
        for id in [1u64, 2] {
            let rt = app.runtimes.get_mut(&id).unwrap();
            rt.proto = ports::MouseProto::Sgr;
            rt.input.clear();
        }
        let rects = app.display_rects();
        let a = rects.iter().find(|p| p.id == 1).unwrap().rect;
        let b = rects.iter().find(|p| p.id == 2).unwrap().rect;
        let row = a.y + 2;

        handle_mouse(&mut app, click(a.x + 2, row)); // press in A
        handle_mouse(&mut app, drag(b.x + 2, row)); // drag into B
        handle_mouse(&mut app, release(b.x + 4, row)); // release over B

        let a_input = String::from_utf8(app.runtimes.get(&1).unwrap().input.clone()).unwrap();
        assert_eq!(
            a_input.matches('\x1b').count(),
            3,
            "the origin pane sees the whole gesture, release included: {a_input:?}"
        );
        assert!(a_input.ends_with('m'), "...and the release really is a release");
        assert!(
            app.runtimes.get(&2).unwrap().input.is_empty(),
            "the neighbour sees nothing: it was never pressed in"
        );
        // The coordinates B's columns would have produced are clamped into
        // A's grid, so the origin app is never told about a cell it lacks.
        let inner_w = a.width - 2;
        assert!(
            !a_input.contains(&format!(";{};", inner_w + 1)),
            "forwarded columns stay inside A's {inner_w}-column grid: {a_input:?}"
        );
        assert_eq!(app.mouse_latch(), None, "the release ends the gesture");
    }

    /// P20: a drag or release with no press behind it belongs to nobody —
    /// it must not be hit-tested into whatever pane the pointer happens to
    /// be over. (The press that landed on the tab bar is the everyday case.)
    #[test]
    fn an_orphan_drag_or_release_is_delivered_to_no_pane() {
        let mut app = mk_app();
        app.apply(Action::NewPane);
        for id in [1u64, 2] {
            let rt = app.runtimes.get_mut(&id).unwrap();
            rt.proto = ports::MouseProto::Sgr;
            rt.input.clear();
        }
        let rects = app.display_rects();
        let a = rects.iter().find(|p| p.id == 1).unwrap().rect;
        handle_mouse(&mut app, click(1, 0)); // press on the tab bar
        handle_mouse(&mut app, drag(a.x + 2, a.y + 2));
        handle_mouse(&mut app, release(a.x + 2, a.y + 2));
        assert!(app.runtimes.get(&1).unwrap().input.is_empty());
        assert!(app.runtimes.get(&2).unwrap().input.is_empty());
    }

    // ---- C29: native selection over a mouse-unaware pane -------------------

    /// A plain left-drag over a pane that never asked for the mouse
    /// selects text and copies it on release — the gesture that was
    /// entirely dead before (`route_mouse` returned `MouseAction::None` for
    /// every Down/Drag/Up over a `MouseProto::None` pane). The highlight
    /// then stays lit: the contract clears it on the *next* click or
    /// keypress, not on the copy itself (`a_plain_click_clears...` below,
    /// and `core::app`'s own `finish_native_selection` tests). B2 (PR #46
    /// code review): pins the *exact* flash text — `infra::clipboard::copy`
    /// is a deterministic `#[cfg(test)]` stub (see that module), so this
    /// would fail identically for a broken clipboard before that fix.
    #[test]
    fn dragging_over_a_native_pane_selects_and_copies_on_release() {
        let mut app = mk_app();
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().grab = "dragged text".into();
        let r = app.display_rects()[0].rect;

        handle_mouse(&mut app, click(r.x + 2, r.y + 1));
        handle_mouse(&mut app, drag(r.x + 6, r.y + 1));
        let sel = app.selection.expect("the drag left a selection behind");
        assert_ne!(sel.anchor, sel.cursor, "a real drag, not a 0-length click");

        handle_mouse(&mut app, release(r.x + 6, r.y + 1));
        assert!(app.selection.is_some(), "the highlight stays lit after release");
        assert_eq!(app.flash(), Some("copied 12 chars"));
        assert_eq!(app.mouse_latch(), None, "the release still ends the P20 gesture");
    }

    /// Selection-freeze amendment (C29, DESIGN-ui.md): the pin. Output
    /// banked mid-drag — a shell, a build, `tail -f`, exactly the class the
    /// freeze exists for — must not change what release copies. Without
    /// `freeze_view`/`unfreeze_view` wired into the Down/Up arms of
    /// `handle_native_selection`, this fails: `FakePane::grab_text` reads
    /// `self.grab` live, so the release would copy "mutated text" (12
    /// chars) instead of the "original text" (13 chars) the drag actually
    /// highlighted — confirmed by mutating this test (commenting out the
    /// two `freeze`/`unfreeze` calls) and re-running it, which reproduces
    /// exactly that failure.
    #[test]
    fn output_banked_mid_drag_does_not_change_what_release_copies() {
        let mut app = mk_app();
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().grab = "original text".into();
        let r = app.display_rects()[0].rect;

        handle_mouse(&mut app, click(r.x + 2, r.y + 1));
        handle_mouse(&mut app, drag(r.x + 6, r.y + 1));
        // The pane prints while the gesture is still in flight — banked
        // mid-drag, same as a shell scrolling under an in-progress select.
        app.runtimes.get_mut(&id).unwrap().grab = "mutated text".into();

        handle_mouse(&mut app, release(r.x + 6, r.y + 1));
        assert_eq!(
            app.flash(),
            Some("copied 13 chars"),
            "release must copy what was highlighted at drag time (\"original text\"), \
             not what the pane now shows"
        );
    }

    /// Selection-freeze amendment: a wheel tick mid-drag drops the freeze —
    /// scrolling and freezing disagree about whether the view may move, so
    /// the freeze loses. Proven the same way as the mutation pin above, but
    /// inverted: a mutation *after* the wheel tick must now reach the copy.
    #[test]
    fn a_wheel_tick_mid_drag_drops_the_freeze() {
        let mut app = mk_app();
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().grab = "original text".into();
        let r = app.display_rects()[0].rect;

        handle_mouse(&mut app, click(r.x + 2, r.y + 1));
        handle_mouse(&mut app, drag(r.x + 6, r.y + 1));
        handle_mouse(&mut app, wheel_up(r.x + 2, r.y + 1));
        app.runtimes.get_mut(&id).unwrap().grab = "mutated text".into();

        handle_mouse(&mut app, release(r.x + 6, r.y + 1));
        assert_eq!(
            app.flash(),
            Some("copied 12 chars"),
            "the wheel tick dropped the freeze, so release reads the pane live again"
        );
    }

    // ---- Selection-freeze design audit D1: release must not depend on --
    // ---- handle_native_selection ever running again ---------------------
    //
    // Each of the five tests below drives a real Down/Drag, changes
    // something that stops `handle_native_selection` from ever seeing this
    // gesture's `Up` again, mutates the pane's content, and asserts a
    // *direct* `grab_text` read is already live — proving the freeze
    // released at the abandonment point, not merely that some later event
    // happened to clean it up.

    /// D1: copy mode can be entered mid-drag (a keypress interleaves with
    /// the still-held button) — its early return in `handle_mouse` means a
    /// Normal-mode gesture's `Up` never reaches the P20 latch code again.
    #[test]
    fn entering_copy_mode_mid_drag_releases_the_freeze() {
        let mut app = mk_app();
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().grab = "original text".into();
        let r = app.display_rects()[0].rect;

        handle_mouse(&mut app, click(r.x + 2, r.y + 1));
        handle_mouse(&mut app, drag(r.x + 6, r.y + 1));

        app.apply(Action::CopyMode);
        app.runtimes.get_mut(&id).unwrap().grab = "mutated text".into();
        // The button is still physically held — further events keep
        // arriving, now swallowed by copy mode's own mouse handling.
        handle_mouse(&mut app, drag(r.x + 8, r.y + 1));

        assert_eq!(
            app.runtimes.get(&id).unwrap().grab_text((0, 0), (0, 0)),
            "mutated text",
            "entering copy mode mid-drag must release the freeze"
        );
    }

    /// D1: Scroll mode fails `handle_native_selection`'s own `Mode::Normal`
    /// gate (D1/PR #46), so the pane's `Up` reaches `handle_mouse` but never
    /// reaches that function again.
    #[test]
    fn entering_scroll_mode_mid_drag_releases_the_freeze() {
        let mut app = mk_app();
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().grab = "original text".into();
        let r = app.display_rects()[0].rect;

        handle_mouse(&mut app, click(r.x + 2, r.y + 1));
        handle_mouse(&mut app, drag(r.x + 6, r.y + 1));

        app.apply(Action::ScrollMode);
        app.runtimes.get_mut(&id).unwrap().grab = "mutated text".into();
        handle_mouse(&mut app, release(r.x + 6, r.y + 1));

        assert_eq!(
            app.runtimes.get(&id).unwrap().grab_text((0, 0), (0, 0)),
            "mutated text",
            "entering scroll mode mid-drag must release the freeze"
        );
    }

    /// D1: `rects`/`display_rects` are active-tab-only, so a tab switch
    /// mid-drag makes the latched pane an orphan — the `Up` arrives but
    /// `pane` resolves to `None` before `handle_native_selection` ever runs.
    #[test]
    fn a_tab_switch_mid_drag_releases_the_freeze() {
        let mut app = mk_app();
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().grab = "original text".into();
        let r = app.display_rects()[0].rect;

        handle_mouse(&mut app, click(r.x + 2, r.y + 1));
        handle_mouse(&mut app, drag(r.x + 6, r.y + 1));

        app.apply(Action::NewTab); // switches to a fresh tab; `id` is left behind
        app.runtimes.get_mut(&id).unwrap().grab = "mutated text".into();
        handle_mouse(&mut app, release(r.x + 6, r.y + 1));

        assert_eq!(
            app.runtimes.get(&id).unwrap().grab_text((0, 0), (0, 0)),
            "mutated text",
            "a tab switch mid-drag must release the freeze on the pane left behind"
        );
    }

    /// D1: a pane collapsing mid-drag (its stack-mate gets expanded instead)
    /// fails `handle_native_selection`'s `!collapsed` gate on release.
    #[test]
    fn a_pane_collapsing_mid_drag_releases_the_freeze() {
        let mut app = mk_app();
        app.apply(Action::NewPane); // panes 1|2, focus 2
        app.apply(Action::ToggleStack); // stacks them: focused (2) expands, 1 collapses
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().grab = "original text".into();
        let r = app
            .display_rects()
            .iter()
            .find(|p| p.id == id)
            .unwrap()
            .rect;

        handle_mouse(&mut app, click(r.x + 2, r.y + 1));
        handle_mouse(&mut app, drag(r.x + 6, r.y + 1));

        let other = app.pane_order().into_iter().find(|&p| p != id).unwrap();
        app.on_click(other); // expands `other`, collapsing `id` mid-drag
        assert!(
            app.display_rects()
                .iter()
                .find(|p| p.id == id)
                .unwrap()
                .collapsed,
            "setup: id must now be the collapsed member"
        );

        app.runtimes.get_mut(&id).unwrap().grab = "mutated text".into();
        handle_mouse(&mut app, release(r.x + 6, r.y + 1));

        assert_eq!(
            app.runtimes.get(&id).unwrap().grab_text((0, 0), (0, 0)),
            "mutated text",
            "a pane collapsing mid-drag must release the freeze"
        );
    }

    /// D1: the pane's own app enabling SGR mouse reporting mid-gesture fails
    /// `handle_native_selection`'s `MouseProto::None` gate on release.
    #[test]
    fn an_sgr_flip_mid_drag_releases_the_freeze() {
        let mut app = mk_app();
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().grab = "original text".into();
        let r = app.display_rects()[0].rect;

        handle_mouse(&mut app, click(r.x + 2, r.y + 1));
        handle_mouse(&mut app, drag(r.x + 6, r.y + 1));

        app.runtimes.get_mut(&id).unwrap().proto = ports::MouseProto::Sgr;
        app.runtimes.get_mut(&id).unwrap().grab = "mutated text".into();
        handle_mouse(&mut app, release(r.x + 6, r.y + 1));

        assert_eq!(
            app.runtimes.get(&id).unwrap().grab_text((0, 0), (0, 0)),
            "mutated text",
            "an SGR flip mid-drag must release the freeze"
        );
    }

    /// SPEC-parity P1 gap (selection-freeze amendment): `roost read`'s
    /// screen mode is a different consumer than the human whose drag the
    /// freeze protects — it must see live content even mid-gesture.
    #[test]
    fn roost_read_screen_mode_bypasses_the_gesture_freeze() {
        use crate::core::control::{Method, ReadMode, Reply, Request};
        let mut app = mk_app();
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().grab = "original text".into();
        let r = app.display_rects()[0].rect;

        handle_mouse(&mut app, click(r.x + 2, r.y + 1));
        handle_mouse(&mut app, drag(r.x + 6, r.y + 1)); // freeze armed, gesture still open

        app.runtimes.get_mut(&id).unwrap().grab = "mutated text".into();
        let token = app.control_token();
        let reply = app.handle_control(Request {
            token,
            method: Method::Read {
                pane: id,
                mode: ReadMode::Screen,
            },
        });
        let text = match reply {
            Reply::Ok { ok } => ok["text"].as_str().unwrap().to_string(),
            Reply::Err { err } => panic!("expected ok, got err: {err}"),
        };
        assert_eq!(
            text, "mutated text",
            "roost read must see live content, not the drag's frozen frame"
        );
    }

    /// D1 (PR #46 design audit): the scope clause — native selection is
    /// specified for `Mode::Normal` only — actually holds in code. A drag
    /// during Scroll mode must not select or copy: before this, neither of
    /// `handle_mouse`'s two early returns (Copy mode, a C12 modal) caught
    /// Scroll, so it silently fell all the way through to
    /// `handle_native_selection` while the keypress-clear stayed
    /// `Mode::Normal`-gated and couldn't clean up the result until Scroll
    /// mode was left.
    #[test]
    fn a_drag_in_scroll_mode_does_not_select() {
        let mut app = mk_app();
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().grab = "dragged text".into();
        app.apply(Action::ScrollMode);
        assert!(matches!(app.mode, Mode::Scroll));
        let r = app.display_rects()[0].rect;

        handle_mouse(&mut app, click(r.x + 2, r.y + 1));
        handle_mouse(&mut app, drag(r.x + 6, r.y + 1));
        handle_mouse(&mut app, release(r.x + 6, r.y + 1));

        assert!(app.selection.is_none(), "Scroll mode must not start a native selection");
        assert!(app.flash().is_none(), "…and nothing was copied");
    }

    /// Double-click selects the whitespace-delimited word under the
    /// pointer. S4 (PR #46 code review): its own release *stages* the copy
    /// rather than committing it — a still-possible 3rd click would
    /// supersede it with the whole line, and committing immediately made
    /// every triple-click copy the word and then the line, two clipboard
    /// writes for one gesture. The deferred-fire/cancel mechanism itself is
    /// pinned at the `App` level (`core::app::tests::
    /// release_native_selection_stages_a_double_click_and_due_copy_fires_
    /// later`, `a_third_click_cancels_the_staged_double_click_copy`), since
    /// it needs to backdate the deadline rather than sleep for it.
    #[test]
    fn double_click_selects_the_word_and_stages_the_copy_on_release() {
        let mut app = mk_app();
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().rows = vec!["hello world".into()];
        let r = app.display_rects()[0].rect;
        let (x, y) = (r.x + 1 + 7, r.y + 1); // inner col 7 → inside "world"

        handle_mouse(&mut app, click(x, y));
        handle_mouse(&mut app, release(x, y));
        assert!(app.selection.is_none(), "a plain click-release selects nothing");

        handle_mouse(&mut app, click(x, y)); // 2nd click, same spot
        let sel = app.selection.expect("the double-click marked a word selection");
        assert_eq!((sel.anchor.1, sel.cursor.1), (6, 10), "the whole word \"world\"");

        handle_mouse(&mut app, release(x, y));
        assert!(app.selection.is_some(), "the highlight stays lit — staged, not yet committed");
        assert!(app.flash().is_none(), "a still-possible 3rd click must not be pre-empted");
    }

    /// D3 (PR #46 design audit): a double-click on a *one-character* word
    /// still marks (and, on the eventual due-copy, would still yield) a
    /// real selection — the old release logic inferred "nothing selected"
    /// from `anchor == cursor`, which a 1-char word also satisfies by
    /// construction, and silently dropped it instead of staging it.
    #[test]
    fn double_click_on_a_one_char_word_still_selects_it() {
        let mut app = mk_app();
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().rows = vec!["a bc".into()];
        let r = app.display_rects()[0].rect;
        let (x, y) = (r.x + 1, r.y + 1); // inner col 0 → the "a"

        handle_mouse(&mut app, click(x, y));
        handle_mouse(&mut app, release(x, y));
        handle_mouse(&mut app, click(x, y)); // 2nd click: double
        let sel = app.selection.expect("double-click marked the 1-char word, not \"nothing\"");
        assert_eq!((sel.anchor, sel.cursor), ((0, 0), (0, 0)));

        handle_mouse(&mut app, release(x, y));
        assert!(app.selection.is_some(), "staged like any other double-click, not dropped");
    }

    /// Triple-click selects the whole row and copies it on release
    /// immediately — unlike a double-click, nothing above triple-click is
    /// bound, so there is no further click to ever wait for (S4).
    #[test]
    fn triple_click_selects_the_line_and_copies_on_release() {
        let mut app = mk_app();
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().rows = vec!["hello world".into()];
        let r = app.display_rects()[0].rect;
        let (x, y) = (r.x + 3, r.y + 1);
        let last = r.width - 3; // inner width (r.width - 2) minus 1

        for _ in 0..2 {
            handle_mouse(&mut app, click(x, y));
            handle_mouse(&mut app, release(x, y));
        }
        handle_mouse(&mut app, click(x, y)); // 3rd click
        let sel = app.selection.expect("the triple-click marked the whole row");
        assert_eq!((sel.anchor, sel.cursor), ((0, 0), (0, last)));

        handle_mouse(&mut app, release(x, y));
        assert!(app.selection.is_some(), "copies and leaves the highlight lit");
        assert_eq!(app.flash(), Some("copied 11 chars"), "the row's real content, \"hello world\"");
    }

    /// Shift-click extends an existing selection to the pointer (keeping
    /// the anchor) and copies the extended range on release immediately —
    /// it never touches `click_count`, so it is never staged like a
    /// double-click's own release is (S4).
    #[test]
    fn shift_click_extends_the_selection_and_copies_on_release() {
        let mut app = mk_app();
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().grab = "dragged text".into();
        let r = app.display_rects()[0].rect;

        handle_mouse(&mut app, click(r.x + 2, r.y + 1));
        handle_mouse(&mut app, drag(r.x + 4, r.y + 1));
        let anchor = app.selection.unwrap().anchor;

        let mut shift_click = click(r.x + 8, r.y + 1);
        shift_click.modifiers = KeyModifiers::SHIFT;
        handle_mouse(&mut app, shift_click);
        let sel = app.selection.expect("shift-click extended the selection");
        assert_eq!(sel.anchor, anchor, "the anchor is unchanged by a shift-click");
        assert_ne!(sel.cursor, anchor, "the cursor moved to the shift-click point");

        handle_mouse(&mut app, release(r.x + 8, r.y + 1));
        assert!(app.selection.is_some(), "copies and leaves the highlight lit");
        assert_eq!(app.flash(), Some("copied 12 chars"));
    }

    /// S3 (PR #46 code review): an Alt+click that opens a URL is a complete
    /// gesture on its own pane and must not leave a *different* pane's
    /// lingering native selection around for a later, unrelated
    /// drag/release on the new latch to extend or re-copy.
    #[test]
    fn alt_click_opening_a_url_clears_a_different_panes_selection() {
        let mut app = mk_app();
        app.apply(Action::NewPane); // panes 1 | 2, side by side, focus 2
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().grab = "dragged text".into();
        let mine = app.display_rects().iter().find(|p| p.id == id).copied().unwrap();
        handle_mouse(&mut app, click(mine.rect.x + 2, mine.rect.y + 1));
        handle_mouse(&mut app, drag(mine.rect.x + 6, mine.rect.y + 1));
        handle_mouse(&mut app, release(mine.rect.x + 6, mine.rect.y + 1));
        assert!(app.selection.is_some(), "the drag left a highlight behind");

        let other = app.display_rects().iter().find(|p| p.id != id).copied().unwrap();
        app.runtimes.get_mut(&other.id).unwrap().rows = vec!["see https://a.co here".into()];
        let mut alt_click = click(other.rect.x + 1 + 4, other.rect.y + 1); // "https://a.co"
        alt_click.modifiers = KeyModifiers::ALT;
        handle_mouse(&mut app, alt_click);

        assert!(app.selection.is_none(), "the URL-open cleared the other pane's stale selection");
    }

    /// C29: a plain click — including on a *different* pane than the one
    /// holding the selection — dismisses a lingering native-selection
    /// highlight (`App::on_click`'s cross-pane rule).
    #[test]
    fn a_plain_click_clears_a_lingering_native_selection() {
        let mut app = mk_app();
        app.apply(Action::NewPane); // panes 1 | 2, side by side, focus 2
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().grab = "dragged text".into();
        let r = app.display_rects().iter().find(|p| p.id == id).unwrap().rect;

        handle_mouse(&mut app, click(r.x + 2, r.y + 1));
        handle_mouse(&mut app, drag(r.x + 6, r.y + 1));
        handle_mouse(&mut app, release(r.x + 6, r.y + 1));
        assert!(app.selection.is_some(), "the drag left a highlight behind");

        // The other pane is made SGR-mouse-aware, so this click cannot
        // start a fresh native selection of its own there — isolating
        // `on_click`'s cross-pane clearing rule from "a fresh press always
        // restarts the gesture", which a second `MouseProto::None` pane
        // would also exercise and confound this assertion with.
        let other = app.display_rects().iter().find(|p| p.id != id).copied().unwrap();
        app.runtimes.get_mut(&other.id).unwrap().proto = ports::MouseProto::Sgr;
        handle_mouse(&mut app, click(other.rect.x + 2, other.rect.y + 1));
        assert!(app.selection.is_none(), "clicking a different pane cleared the highlight");
    }

    /// C29: a pane that speaks SGR mouse reporting keeps the mouse entirely —
    /// every gesture above forwards to the app instead, and `app.selection`
    /// is never touched. Same shape as the existing P20 latch test above.
    #[test]
    fn a_mouse_aware_pane_is_untouched_by_native_selection_gestures() {
        let mut app = mk_app();
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().proto = ports::MouseProto::Sgr;
        let r = app.display_rects()[0].rect;

        handle_mouse(&mut app, click(r.x + 2, r.y + 1));
        handle_mouse(&mut app, drag(r.x + 6, r.y + 1));
        handle_mouse(&mut app, release(r.x + 6, r.y + 1));

        assert!(app.selection.is_none(), "an SGR pane never gets a native selection");
        let input = String::from_utf8(app.runtimes.get(&id).unwrap().input.clone()).unwrap();
        assert_eq!(input.matches('\x1b').count(), 3, "every event forwarded instead: {input:?}");
    }

    /// C22: float-first hit-test ordering (a click inside its centered rect
    /// hits it, even though real panes are also present) and click-outside-
    /// hides (a click elsewhere hides it and focuses what it hit) — both
    /// driven through the real `handle_mouse` entry point.
    #[test]
    fn click_outside_the_float_hides_it_click_inside_keeps_it() {
        let mut app = mk_app();
        app.apply(Action::NewPane); // panes 1|2 side by side, focus 2
        app.apply(Action::ToggleFloat); // float spawns, shows, focuses
        let float_id = app.focused;
        assert_ne!(float_id, 1);
        assert_ne!(float_id, 2);
        let float_rect = app.display_rects()[0].rect; // float first (topmost)

        let click = |col: u16, row: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };

        // Inside the float's own rect: hit_test picks it first, stays shown.
        handle_mouse(&mut app, click(float_rect.x + 1, float_rect.y + 1));
        assert_eq!(app.focused, float_id);

        // Outside it (pane 1's corner, definitely clear of the centered
        // float): hides the float, focuses what it actually hit.
        handle_mouse(&mut app, click(0, 1));
        assert_eq!(app.focused, 1);
        assert_eq!(app.display_rects().len(), app.rects().len(), "float no longer in the display list");
    }

    /// L2: `.mode(0o600)` on `OpenOptions` only applies when `open()`
    /// actually creates the file — a token left behind by a crash (so this
    /// call reuses it) must still end up owner-only, not whatever mode it
    /// had before.
    #[test]
    fn write_control_token_resets_the_mode_of_a_pre_existing_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("roost-token-mode-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("control.token");
        std::fs::write(&path, b"stale").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_control_token(&path, "fresh-token");

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "a pre-existing file's mode must be reset, not inherited");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fresh-token");
        let _ = std::fs::remove_dir_all(dir);
    }
}
