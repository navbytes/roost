//! The control interface: the verb set through which an LLM/CLI/MCP client
//! manages roost, plus its ownership-scoped authorization model. This module is
//! transport-agnostic — the socket, the CLI, and a future MCP bridge all build
//! a `Request` and hand it to `App::handle_control`, which returns a `Reply`.
//!
//! Authorization (see DESIGN-control.md §5): a request carries a token.
//! - The fleet control token (from `<state>/control.token`, never in any pane's
//!   env) resolves to `Actor::Fleet` — may act on any pane.
//! - A pane's own `ROOST_TOKEN` resolves to `Actor::Pane(id)` — may spawn/fork
//!   freely, and may drive only the panes in its own spawned subtree.

use crate::core::workspace::PaneId;
use serde::{Deserialize, Serialize};

/// How much of a pane to read back.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadMode {
    /// The current visible grid (default) — bounded, usually the "answer" region.
    Screen,
    /// The last N non-empty lines.
    Tail(usize),
    /// The full scrollback buffer (opt-in; can be large).
    Full,
}

impl Default for ReadMode {
    fn default() -> Self {
        ReadMode::Screen
    }
}

/// A control verb. Deserialized from the socket/CLI; `Wait` is handled by the
/// transport layer (deferred reply) and is intentionally not here yet.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Method {
    /// Panes visible to the actor (subtree for a pane actor, all for fleet).
    List,
    /// Status of one pane, or all visible panes.
    Status {
        #[serde(default)]
        pane: Option<PaneId>,
    },
    /// Spawn a new pane running `adapter` in `cwd`, optionally typing
    /// `initial_input` (+ Enter). Returns the new pane id.
    Spawn {
        adapter: String,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        initial_input: Option<String>,
    },
    /// Fork a sibling of `pane` (default: the actor's own pane): same adapter +
    /// cwd. (Session-branching lands with the bidirectional pi extension.)
    Fork {
        #[serde(default)]
        pane: Option<PaneId>,
    },
    /// Send text to a pane; `submit` appends a carriage return.
    Send {
        pane: PaneId,
        text: String,
        #[serde(default)]
        submit: bool,
    },
    /// Read a pane's contents.
    Read {
        pane: PaneId,
        #[serde(default)]
        mode: ReadMode,
    },
    /// Close a pane. `force` is required to close a *working* pane (no human is
    /// there to confirm as the interactive Alt+w does).
    Close {
        pane: PaneId,
        #[serde(default)]
        force: bool,
    },
}

/// A control request as received from a transport: the caller's token + a verb.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub token: String,
    #[serde(flatten)]
    pub method: Method,
}

/// The resolved caller, from the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    /// Holder of the fleet control token — may act on any pane.
    Fleet,
    /// A pane acting via its own `ROOST_TOKEN` — subtree-scoped.
    Pane(PaneId),
}

/// The result of a control request, serialized back to the client.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Reply {
    Ok { ok: serde_json::Value },
    Err { err: String },
}

impl Reply {
    pub fn ok(v: serde_json::Value) -> Self {
        Reply::Ok { ok: v }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        Reply::Err { err: msg.into() }
    }
}
