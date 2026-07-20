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
    DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseEventKind,
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
use crate::ui::mouse::{self, WheelRoute};

fn main() -> Result<()> {
    let mut terminal = ratatui::init();
    // Without mouse capture the hosting terminal consumes wheel events and
    // scrolls its own buffer — content *outside* the TUI. Capture them.
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let result = run(&mut terminal);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
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

        // Terminal input (with a frame-rate timeout)...
        if crossterm::event::poll(Duration::from_millis(33))? {
            match crossterm::event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if app.handle_mode_key(key) {
                        continue;
                    }
                    match input::translate(key) {
                        InputResult::Action(a) => app.apply(a),
                        InputResult::Forward(bytes) if app.focused_dead() => {
                            // Dead pane: roost handles the keys instead.
                            match bytes.as_slice() {
                                b"\r" => app.respawn_focused(false), // retry/resume
                                b"f" => app.respawn_focused(true),   // fresh session
                                _ => {}
                            }
                        }
                        InputResult::Forward(bytes) => app.forward_bytes(&bytes),
                        InputResult::Ignore => {}
                    }
                }
                Event::Mouse(me) => handle_mouse(&mut app, me),
                Event::Resize(w, h) => app.on_resize(ratatui::layout::Size::new(w, h)),
                Event::Paste(s) => app.forward_bytes(s.as_bytes()),
                _ => {}
            }
        }

        // ...then drain PTY output and socket events.
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AppEvent::Output(id, bytes) => app.on_pty_output(id, &bytes),
                AppEvent::Exit(id) => app.on_pty_exit(id),
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

/// Route mouse events: wheel scrolls the hovered pane (forwarded to
/// mouse-aware apps, roost-side scrollback otherwise); left click focuses.
fn handle_mouse<B: PaneBackend>(app: &mut App<B>, me: crossterm::event::MouseEvent) {
    let rects = app.rects();
    let Some(pane) = mouse::hit_test(&rects, me.column, me.row) else { return };
    match me.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let up = me.kind == MouseEventKind::ScrollUp;
            let proto = app
                .runtimes
                .get(&pane.id)
                .map(|rt| rt.mouse_proto())
                .unwrap_or(ports::MouseProto::None);
            match mouse::route_wheel(proto, &pane, me.column, me.row, up) {
                WheelRoute::Forward(bytes) => app.wheel_forward(pane.id, &bytes),
                WheelRoute::Scroll(delta) => app.wheel_scroll(pane.id, delta),
                WheelRoute::Ignore => {}
            }
        }
        MouseEventKind::Down(crossterm::event::MouseButton::Left) => app.on_click(pane.id),
        _ => {}
    }
}
