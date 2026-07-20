//! The disposable state: a live PTY + vt100 terminal state per pane.
//! Killing a PaneRuntime loses nothing precious — the agent's session file
//! is the ground truth, and the adapter knows how to resume it.

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::mpsc::Sender;

use crate::adapters::CommandSpec;
use crate::event::AppEvent;
use crate::status::StatusTracker;
use crate::workspace::PaneId;

const SCROLLBACK_LINES: usize = 5000;

pub struct PaneRuntime {
    #[allow(dead_code)] // useful in logs/debugging; identity lives in App maps
    pub id: PaneId,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    pub parser: vt100::Parser,
    pub status: StatusTracker,
}

impl PaneRuntime {
    /// Spawn the command in a fresh PTY. A reader thread pumps output into
    /// the main loop via `tx`; the parser is fed on the main thread.
    pub fn spawn(
        id: PaneId,
        spec: &CommandSpec,
        rows: u16,
        cols: u16,
        tx: Sender<AppEvent>,
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
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().context("clone pty reader")?;
        let writer = pair.master.take_writer().context("take pty writer")?;

        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        let _ = tx.send(AppEvent::Exit(id));
                        break;
                    }
                    Ok(n) => {
                        if tx.send(AppEvent::Output(id, buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        Ok(Self {
            id,
            master: pair.master,
            child,
            writer,
            parser: vt100::Parser::new(rows, cols, SCROLLBACK_LINES),
            status: StatusTracker::new(),
        })
    }

    /// Feed PTY output into the terminal state machine (main thread).
    pub fn process_output(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
        self.status.on_output();
    }

    pub fn write_input(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        if rows == 0 || cols == 0 {
            return;
        }
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        self.parser.set_size(rows, cols);
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }
}
