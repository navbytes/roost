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
mod sock;
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
    // Status socket: agent extensions/hooks report exact status + session ids.
    let sock_path = sock::spawn_listener(tx.clone()).ok();
    let size = terminal.size()?;
    let mut app = App::new(adapters::registry(), tx, size, sock_path)?;
    app.relayout();

    loop {
        terminal.draw(|f| render::draw(f, &mut app))?;

        // Terminal input (with a frame-rate timeout)...
        if crossterm::event::poll(Duration::from_millis(33))? {
            match crossterm::event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
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
                        notify(&msg);
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

/// "A pane needs you": terminal bell always; native notification on macOS.
fn notify(msg: &str) {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x07");
    let _ = out.flush();
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification \"{}\" with title \"roost\"",
            msg.replace('\\', "").replace('"', "'")
        );
        let _ = std::process::Command::new("osascript").arg("-e").arg(script).spawn();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = msg;
}
