//! Production `PaneBackend`: a real PTY child + vt100 terminal state.
//! This is the only module that touches portable-pty. Killing a PtyPane
//! loses nothing precious — the agent's session file is the ground truth,
//! and the adapter knows how to resume it.

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::mpsc::Sender;

use crate::agents::CommandSpec;
use crate::core::event::AppEvent;
use crate::core::status::{AgentStatus, StatusTracker};
use crate::core::workspace::PaneId;
use crate::ports::{MouseProto, PaneBackend};

const SCROLLBACK_LINES: usize = 5000;

pub struct PtyPane {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    parser: vt100::Parser,
    status: StatusTracker,
    /// Roost-side scrollback offset (wheel / scroll mode).
    scroll: usize,
}

impl PaneBackend for PtyPane {
    /// Spawn the command in a fresh PTY. A reader thread pumps output into
    /// the main loop via `tx`; the parser is fed on the main thread.
    fn spawn(
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
            master: pair.master,
            child,
            writer,
            parser: vt100::Parser::new(rows, cols, SCROLLBACK_LINES),
            status: StatusTracker::new(),
            scroll: 0,
        })
    }

    fn process_output(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
        self.status.on_output();
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

    fn kill(&mut self) {
        let _ = self.child.kill();
    }

    fn status(&self) -> AgentStatus {
        self.status.current()
    }

    fn set_extension_status(&mut self, s: AgentStatus) {
        self.status.set_extension_status(s);
    }

    fn on_exit(&mut self) {
        self.status.on_exit();
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
}
