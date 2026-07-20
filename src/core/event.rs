//! Events flowing into the main loop from PTY reader threads (and, in M3,
//! from the status socket listener).

use crate::core::status::AgentStatus;
use crate::core::workspace::PaneId;

pub enum AppEvent {
    /// Raw bytes from a pane's PTY.
    Output(PaneId, Vec<u8>),
    /// The pane's child process exited (EOF on the PTY).
    Exit(PaneId),
    /// Exact status pushed by an agent-side extension/hook (status socket).
    Status(PaneId, AgentStatus),
    /// Session id reported by an agent-side extension (status socket).
    Session(PaneId, String),
}
