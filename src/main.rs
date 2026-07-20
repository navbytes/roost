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
mod core;
mod infra;
mod ports;
mod ui;

use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use crossterm::execute;
use std::sync::mpsc;
use std::time::Duration;

use crate::core::app::App;
use crate::core::event::AppEvent;
use crate::infra::notify::TermNotifier;
use crate::infra::pty::PtyPane;
use crate::infra::store::FsStore;
use crate::ports::{Notifier, PaneBackend, StateStore};
use crate::ui::input::{self, InputResult};
use crate::ui::mouse::{self, MouseAction};

fn main() -> Result<()> {
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
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let result = run(&mut terminal);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
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
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        ratatui::restore();
        original(info);
    }));
}

fn run(terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
    let (tx, rx) = mpsc::channel::<AppEvent>();

    // Wire production adapters to the core's ports.
    let store = FsStore::default();
    let ws = store.load()?.unwrap_or_else(|| {
        core::workspace::Workspace::default_in(
            std::env::current_dir().unwrap_or_else(|_| "/".into()),
        )
    });
    let sock_path = infra::sock::spawn_listener(tx.clone()).ok();
    let mut notifier = TermNotifier;
    let size = terminal.size()?;
    let mut app: App<PtyPane> =
        App::new(ws, agents::registry(), Box::new(store), tx, size, sock_path)?;
    app.relayout();

    loop {
        terminal.draw(|f| ui::render::draw(f, &mut app))?;

        // Drain ALL pending terminal events this tick, not just one. During a
        // resize storm (dragging the window edge) several events queue up
        // faster than a one-event-per-iteration loop can consume; processing
        // one at a time leaves roost's geometry lagging the true terminal size
        // and stale intermediate frames on screen. We coalesce resizes to a
        // single post-drain reconciliation.
        let mut resized = false;
        if crossterm::event::poll(Duration::from_millis(33))? {
            loop {
                match crossterm::event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        if key.modifiers.contains(crossterm::event::KeyModifiers::ALT) {
                            app.note_alt_seen();
                        }
                        if !app.handle_mode_key(key) {
                            handle_key(&mut app, key);
                        }
                    }
                    Event::Mouse(me) => handle_mouse(&mut app, me),
                    // Coalesce: act on the true size once, after draining.
                    Event::Resize(..) => resized = true,
                    Event::Paste(s) => app.forward_bytes(s.as_bytes()),
                    _ => {}
                }
                if !crossterm::event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        if resized {
            // Trust the terminal's current size, not a possibly-stale value
            // carried on an intermediate coalesced event, then hard-clear so
            // no leftover cells from an in-between frame survive.
            let sz = terminal.size()?;
            app.on_resize(sz);
            terminal.clear()?;
        }

        // ...then drain PTY output and socket events.
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AppEvent::Output(id, bytes) => app.on_pty_output(id, &bytes),
                AppEvent::Exit(id) => {
                    if let Some(msg) = app.on_pty_exit(id) {
                        notifier.notify(&msg);
                    }
                }
                AppEvent::Session(id, s) => app.on_session(id, s),
                AppEvent::Status(id, s) => {
                    if let Some(msg) = app.on_status(id, s) {
                        notifier.notify(&msg);
                    }
                }
            }
        }

        // Periodic housekeeping (filesystem session detection).
        app.tick();

        if app.quit {
            break;
        }
    }

    app.shutdown();
    Ok(())
}

/// Handle a key that a UI mode did not consume: a global action, or bytes
/// forwarded to the focused pane (dead panes intercept relaunch keys).
fn handle_key<B: PaneBackend>(app: &mut App<B>, key: crossterm::event::KeyEvent) {
    match input::translate(key) {
        InputResult::Action(a) => app.apply(a),
        InputResult::Forward(bytes) if app.focused_dead() => match bytes.as_slice() {
            b"\r" => app.respawn_focused(false), // retry/resume
            b"f" => app.respawn_focused(true),   // fresh session
            _ => {}
        },
        InputResult::Forward(bytes) => app.forward_bytes(&bytes),
        InputResult::Ignore => {}
    }
}

/// Route mouse events. Tab-bar clicks switch tabs. Over a pane: a left press
/// focuses it; wheel and (for mouse-aware apps) clicks/drags are forwarded to
/// the inner app, otherwise the wheel scrolls roost's own scrollback.
fn handle_mouse<B: PaneBackend>(app: &mut App<B>, me: crossterm::event::MouseEvent) {
    use crossterm::event::{MouseButton, MouseEventKind};

    // Tab bar (top row): click a tab to switch to it.
    if me.row == 0 {
        if matches!(me.kind, MouseEventKind::Down(MouseButton::Left)) {
            let names: Vec<String> = app.ws.tabs.iter().map(|t| t.name.clone()).collect();
            if let Some(i) = mouse::tab_at_x(&names, me.column) {
                app.apply(input::Action::GoToTab(i));
            }
        }
        return;
    }

    let rects = app.rects();
    let Some(pane) = mouse::hit_test(&rects, me.column, me.row) else { return };

    // A left press focuses the pane under the cursor (expands stack members).
    if matches!(me.kind, MouseEventKind::Down(MouseButton::Left)) {
        app.on_click(pane.id);
    }

    let proto = app
        .runtimes
        .get(&pane.id)
        .map(|rt| rt.mouse_proto())
        .unwrap_or(ports::MouseProto::None);
    match mouse::route_mouse(proto, &pane, &me) {
        MouseAction::Forward(bytes) => app.forward_mouse(pane.id, &bytes),
        MouseAction::Scroll(delta) => app.wheel_scroll(pane.id, delta),
        MouseAction::None => {}
    }
}
