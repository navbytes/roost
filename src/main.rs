//! roost — a session-native terminal multiplexer for AI agent CLIs.
//!
//! No daemon: quitting kills the agent processes; the (layout × session-id)
//! mapping persists, and every pane resumes its exact session on relaunch.
//! See DESIGN.md for the full picture.

mod adapters;
mod app;
mod event;
mod input;
mod pane;
mod render;
mod status;
mod workspace;

use anyhow::Result;
use crossterm::event::{Event, KeyEventKind};
use std::sync::mpsc;
use std::time::Duration;

use app::App;
use event::AppEvent;
use input::InputResult;

fn main() -> Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal);
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
    let (tx, rx) = mpsc::channel::<AppEvent>();
    let size = terminal.size()?;
    let mut app = App::new(adapters::registry(), tx, size)?;
    app.relayout();

    loop {
        terminal.draw(|f| render::draw(f, &mut app))?;

        // Terminal input (with a frame-rate timeout)...
        if crossterm::event::poll(Duration::from_millis(33))? {
            match crossterm::event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    match input::translate(key) {
                        InputResult::Action(a) => app.apply(a),
                        InputResult::Forward(bytes) => app.forward_bytes(&bytes),
                        InputResult::Ignore => {}
                    }
                }
                Event::Resize(w, h) => app.on_resize(ratatui::layout::Size::new(w, h)),
                Event::Paste(s) => app.forward_bytes(s.as_bytes()),
                _ => {}
            }
        }

        // ...then drain PTY output.
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AppEvent::Output(id, bytes) => app.on_pty_output(id, &bytes),
                AppEvent::Exit(id) => app.on_pty_exit(id),
            }
        }

        if app.quit {
            break;
        }
    }

    app.shutdown();
    Ok(())
}
