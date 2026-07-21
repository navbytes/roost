//! Ports — the trait boundaries between roost's core and the outside world.
//!
//! The core (`core::app`) is generic over these traits and never touches a
//! real PTY, filesystem, or terminal. Production adapters live in `infra/`;
//! test fakes live in `ports::fakes` and let every core behavior run in a
//! plain unit test.
//!
//! One conscious shim: `PaneBackend::screen()` exposes the `vt100::Screen`
//! grid directly instead of wrapping it. The renderer needs the whole cell
//! grid; re-wrapping ~15 accessor methods would be ceremony without safety.
//! Fakes return `None` and the renderer must tolerate that.

use anyhow::Result;
use std::path::PathBuf;
use std::sync::mpsc::SyncSender;

use crate::agents::CommandSpec;
use crate::core::event::AppEvent;
use crate::core::status::AgentStatus;
use crate::core::workspace::{PaneId, Workspace};

/// What a pane is actually running, read from the OS — its live working
/// directory and any known agent CLI in its process subtree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observation {
    pub cwd: Option<PathBuf>,
    /// Adapter id of a known agent running in the pane, if any.
    pub agent: Option<String>,
}

/// What the pane's inner application asked for, mouse-wise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseProto {
    /// App doesn't listen for mouse events → roost handles the wheel itself.
    None,
    /// App enabled SGR mouse reporting → forward encoded events to the PTY.
    Sgr,
}

/// A running pane: process + terminal state machine. Implemented by
/// `infra::pty::PtyPane` in production.
pub trait PaneBackend: Sized {
    fn spawn(
        id: PaneId,
        cmd: &CommandSpec,
        rows: u16,
        cols: u16,
        tx: SyncSender<AppEvent>,
    ) -> Result<Self>;

    /// Feed process output into the terminal state machine.
    fn process_output(&mut self, bytes: &[u8]);
    /// User keystrokes → process stdin. Implementations should snap
    /// scrollback to the live tail (typing means "I'm back").
    fn write_input(&mut self, bytes: &[u8]);
    /// Bytes → process stdin without touching scrollback (forwarded mouse
    /// events must not yank the view to the live tail).
    fn write_input_raw(&mut self, bytes: &[u8]);
    fn resize(&mut self, rows: u16, cols: u16);
    /// Ask the child to exit cleanly (SIGHUP, as if its terminal closed) so it
    /// can flush a final turn. Best-effort; `kill()` is the guaranteed stop.
    /// Default no-op for backends without a real process.
    fn hangup(&mut self) {}
    fn kill(&mut self);

    fn status(&self) -> AgentStatus;
    fn set_extension_status(&mut self, s: AgentStatus);
    fn on_exit(&mut self);

    /// Has the pane negotiated the kitty "disambiguate" keyboard flag? When
    /// true, roost forwards modified Enter (and friends) in the CSI-u encoding
    /// the app asked for; otherwise it uses a legacy fallback. Default false.
    fn kitty_disambiguate(&self) -> bool {
        false
    }

    /// Terminal grid for rendering. `None` for fakes (renderer must cope).
    fn screen(&self) -> Option<&vt100::Screen>;
    fn set_scrollback(&mut self, lines: usize);
    /// Wheel scrolling: positive = further into history.
    fn scroll_by(&mut self, delta: i32);
    fn mouse_proto(&self) -> MouseProto;

    /// Observe the pane's live working directory and any known agent running
    /// in it (`known_agents` are adapter ids). None = not inspectable (dead
    /// process / unsupported platform); the caller then leaves persisted
    /// state untouched. Default None for backends that can't inspect.
    fn observe(&self, _known_agents: &[String]) -> Option<Observation> {
        None
    }

    /// Extract the visible text between two inclusive cell coords (row, col),
    /// in pane-inner space, for copy mode. Reading order, trailing spaces
    /// trimmed per line, lines joined with '\n'. Default empty.
    fn grab_text(&self, _start: (u16, u16), _end: (u16, u16)) -> String {
        String::new()
    }
}

/// Workspace persistence. Implemented by `infra::store::FsStore`.
pub trait StateStore {
    fn load(&self) -> Result<Option<Workspace>>;
    fn save(&self, ws: &Workspace) -> Result<()>;
}

/// "A pane needs you" side-channel. Implemented by `infra::notify`.
pub trait Notifier {
    fn notify(&mut self, msg: &str);
}

#[cfg(test)]
pub mod fakes {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// In-memory pane: records the spawn command and all input; status is
    /// settable. `cmd.program == "spawn-fail"` simulates a spawn error.
    pub struct FakePane {
        pub cmd: CommandSpec,
        pub input: Vec<u8>,
        pub scrollback: i64,
        status: AgentStatus,
        ext: Option<AgentStatus>,
        exited: bool,
        pub proto: MouseProto,
        /// Test-settable observation returned by `observe`.
        pub observation: Option<Observation>,
        /// Test-settable text returned by `grab_text`.
        pub grab: String,
    }

    impl PaneBackend for FakePane {
        fn spawn(
            _id: PaneId,
            cmd: &CommandSpec,
            _rows: u16,
            _cols: u16,
            _tx: SyncSender<AppEvent>,
        ) -> Result<Self> {
            if cmd.program == "spawn-fail" {
                anyhow::bail!("spawn-fail requested");
            }
            Ok(Self {
                cmd: cmd.clone(),
                input: vec![],
                scrollback: 0,
                status: AgentStatus::Idle,
                ext: None,
                exited: false,
                proto: MouseProto::None,
                observation: None,
                grab: String::new(),
            })
        }
        fn process_output(&mut self, _bytes: &[u8]) {
            self.status = AgentStatus::Working;
        }
        fn write_input(&mut self, bytes: &[u8]) {
            self.scrollback = 0;
            self.input.extend_from_slice(bytes);
        }
        fn write_input_raw(&mut self, bytes: &[u8]) {
            self.input.extend_from_slice(bytes);
        }
        fn resize(&mut self, _rows: u16, _cols: u16) {}
        fn kill(&mut self) {
            self.exited = true;
        }
        fn status(&self) -> AgentStatus {
            if self.exited {
                AgentStatus::Exited
            } else {
                self.ext.unwrap_or(self.status)
            }
        }
        fn set_extension_status(&mut self, s: AgentStatus) {
            if s == AgentStatus::Exited {
                self.exited = true;
            }
            self.ext = Some(s);
        }
        fn on_exit(&mut self) {
            self.exited = true;
        }
        fn screen(&self) -> Option<&vt100::Screen> {
            None
        }
        fn set_scrollback(&mut self, lines: usize) {
            self.scrollback = lines as i64;
        }
        fn scroll_by(&mut self, delta: i32) {
            self.scrollback = (self.scrollback + delta as i64).max(0);
        }
        fn mouse_proto(&self) -> MouseProto {
            self.proto
        }
        fn observe(&self, _known: &[String]) -> Option<Observation> {
            self.observation.clone()
        }
        fn grab_text(&self, _start: (u16, u16), _end: (u16, u16)) -> String {
            self.grab.clone()
        }
    }

    /// Shared in-memory store; clone to keep a handle for assertions.
    #[derive(Clone, Default)]
    pub struct MemStore(pub Arc<Mutex<Option<Workspace>>>);

    impl StateStore for MemStore {
        fn load(&self) -> Result<Option<Workspace>> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn save(&self, ws: &Workspace) -> Result<()> {
            *self.0.lock().unwrap() = Some(ws.clone());
            Ok(())
        }
    }

    /// Records notifications for assertions.
    #[derive(Clone, Default)]
    pub struct RecordingNotifier(pub Arc<Mutex<Vec<String>>>);

    impl Notifier for RecordingNotifier {
        fn notify(&mut self, msg: &str) {
            self.0.lock().unwrap().push(msg.to_string());
        }
    }
}
