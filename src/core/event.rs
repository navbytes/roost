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
    /// is applied so one pane can't spoof another's. The last field is the
    /// optional human-readable detail (`needs_input` only — the question the
    /// agent is asking), surfaced in the feed line and the notification.
    Status(PaneId, String, AgentStatus, Option<String>),
    /// Session id reported by an agent-side extension (status socket). Middle
    /// field is the pane's `ROOST_TOKEN` (verified before use).
    Session(PaneId, String, String),
    /// A status-socket connection reporting for this pane went up (accepted
    /// its first status/session line) or down (closed) — see
    /// `infra::sock`'s `ConnGuard`. Middle field is the pane's `ROOST_TOKEN`
    /// as last seen on that connection, verified before use exactly like
    /// `Status`/`Session`; a link-down for a connection that never
    /// authenticated for this pane must not clear a different pane's link.
    ExtLink(PaneId, String, bool),
}
