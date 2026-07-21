//! Events flowing into the main loop from PTY reader threads (and, in M3,
//! from the status socket listener).

use crate::core::control::{Reply, Request};
use crate::core::status::AgentStatus;
use crate::core::workspace::PaneId;
use std::sync::mpsc::Sender;

pub enum AppEvent {
    /// Raw bytes from a pane's PTY.
    Output(PaneId, Vec<u8>),
    /// The pane's child process exited (EOF on the PTY).
    Exit(PaneId),
    /// A control-interface request from a client, with a reply channel the main
    /// loop fills after executing it. The reply Sender is unbounded so the main
    /// thread never blocks sending it back.
    Command(Request, Sender<Reply>),
    /// Exact status pushed by an agent-side extension/hook (status socket).
    /// The middle field is the pane's `ROOST_TOKEN`, verified before the status
    /// is applied so one pane can't spoof another's.
    Status(PaneId, String, AgentStatus),
    /// Session id reported by an agent-side extension (status socket). Middle
    /// field is the pane's `ROOST_TOKEN` (verified before use).
    Session(PaneId, String, String),
}
