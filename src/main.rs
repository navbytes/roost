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
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
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
    // Negotiate the enhanced (kitty) keyboard protocol so Shift+Enter and
    // Ctrl+Enter arrive as distinct key events — a bare terminal collapses
    // both to a plain CR, making "newline vs submit" impossible to tell apart.
    // Only push the flag if the terminal actually supports it, and remember
    // that so we can pop it on the way out (and in the panic hook).
    let kbd_enhanced = matches!(crossterm::terminal::supports_keyboard_enhancement(), Ok(true));
    if kbd_enhanced {
        let _ = execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    let result = run(&mut terminal);
    if kbd_enhanced {
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
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
        // Pop keyboard enhancement unconditionally: if none was pushed the
        // terminal ignores it, and leaving it set would wedge the user's shell
        // into the kitty protocol after a crash.
        let _ = execute!(
            std::io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableMouseCapture
        );
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

    let loop_result: Result<()> = (|| {
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
    Ok(())
    })();

    // Always run shutdown — even if the loop bailed with an error via `?` — so
    // agents are killed/reaped and the workspace saved, never left orphaned.
    app.shutdown();
    loop_result
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

    // Copy mode owns the mouse: drag selects text, release copies.
    if app.in_copy_mode() {
        handle_copy_mouse(app, me);
        return;
    }

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

    // Alt+click a URL to open it in the browser (roost owns the Alt layer).
    if matches!(me.kind, MouseEventKind::Down(MouseButton::Left))
        && me.modifiers.contains(crossterm::event::KeyModifiers::ALT)
        && !pane.collapsed
    {
        let (r, c) = inner_cell(pane.rect, me.column, me.row);
        if let Some(url) = app.url_at(pane.id, r, c) {
            infra::open::open_url(&url);
            return;
        }
    }

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

/// Copy-mode mouse: left-drag selects text within the pane it started in;
/// release extracts the selection and copies it to the system clipboard.
fn handle_copy_mouse<B: PaneBackend>(app: &mut App<B>, me: crossterm::event::MouseEvent) {
    use crossterm::event::{MouseButton, MouseEventKind};
    let rects = app.rects();
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
                infra::clipboard::copy(&text);
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
