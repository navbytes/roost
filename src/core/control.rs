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
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

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
    /// Send text (+ CR when `submit`) to every *running* pane the actor may
    /// target — Fleet: every spawned pane; a pane actor: its own spawned
    /// subtree, itself included. A distinct wire method from `Send` (not a
    /// `Send` with an omitted `pane`) so authz and audit stay explicit about
    /// a fan-out. CLI/API only — no TUI key (fat-finger safety: this is the
    /// one verb that can touch the whole fleet at once).
    Broadcast {
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
    /// Block until any of `panes` reaches status `until` (e.g. "waiting" =
    /// finished its turn), or `timeout_ms` elapses. A deferred reply: the reply
    /// is sent later by the event loop, not synchronously. This is what turns
    /// "spawn then poll" into "spawn then await".
    Wait {
        panes: Vec<PaneId>,
        until: String,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
}

/// Parse a status name (as sent by a `wait` client) into an `AgentStatus`.
pub fn parse_status(s: &str) -> Option<crate::core::status::AgentStatus> {
    use crate::core::status::AgentStatus::*;
    Some(match s {
        "working" => Working,
        "needs_input" => NeedsInput,
        "waiting" => Waiting,
        "idle" => Idle,
        "exited" => Exited,
        _ => return None,
    })
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

// -- promotion-auth-gate: the token table -----------------------------------
//
// sock.rs's accept-loop threads judge a caller's token synchronously, on
// their own thread, before promoting a connection out of the pre-auth
// guillotine (`PRE_AUTH_READ_TIMEOUT`) — see infra::sock's module doc and
// the promotion-auth-gate design doc. They have no access to `App` (a
// different owner, and reachable only via the main-loop channel, which is
// exactly the round trip a synchronous door check can't afford), so the
// token table itself is published here as a read-only snapshot sock.rs can
// load without ever touching `App`.
//
// `TokenTable` (below) replaces both of `App`'s old `tokens`/`control_token`
// fields with one storage: every mutator clone-mutates a NEW snapshot and
// swaps it into the shared `Arc` (RCU) — the write IS the publish, so there
// is exactly one storage and no second copy for a write site to forget to
// update. `App` holds the only `TokenTable` (the only writer); sock.rs holds
// a `TokenReader`, which has no mutators at all — structurally unable to
// write, not merely disciplined not to.

/// Reply text for a request whose token authenticates nobody — neither the
/// fleet token nor any pane's own. Shared by `App::handle_control`/
/// `handle_control_msg` (the dispatch-time check) and sock.rs's door (the
/// pre-dispatch admission check) so the two can never say something
/// different for the same failure — same pattern as `sock::OVERSIZE_LINE_MSG`.
pub const UNAUTHORIZED_MSG: &str = "unauthorized: unknown or missing token";

/// 16 CSPRNG bytes from /dev/urandom, hex-encoded. `None` if urandom is
/// unreadable — the caller decides whether that's fatal (`TokenTable::new`
/// does, for the fleet token; `gen_token` below tolerates it for a pane's).
pub(crate) fn gen_secret() -> Option<String> {
    use std::io::Read;
    let mut buf = [0u8; 16];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)).ok()?;
    let mut s = String::with_capacity(32);
    for b in buf {
        s.push_str(&format!("{b:02x}"));
    }
    Some(s)
}

/// A per-pane status token. Unlike the fleet control token, a weak fallback is
/// tolerable here: it only authenticates a pane's *own* status/session reports
/// and sits behind the 0600 socket. The control token, which can drive the
/// whole fleet, hard-fails instead (see `TokenTable::new`).
pub(crate) fn gen_token() -> String {
    gen_secret().unwrap_or_else(|| {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{n:032x}")
    })
}

/// Immutable, cheap-to-clone (`Arc`) view of every live token: the one
/// fleet/control token plus each pane's own `ROOST_TOKEN`. What `TokenTable`
/// publishes on every mutation and what a `TokenReader` loads.
pub struct TokenSnapshot {
    control: String,
    panes: HashMap<PaneId, String>,
}

impl TokenSnapshot {
    /// Does `token` authenticate pane `id`'s own status/session reports?
    /// Fails closed: unknown pane or empty token never match (former
    /// `App::socket_authorized`'s logic).
    pub fn pane_authorized(&self, id: PaneId, token: &str) -> bool {
        !token.is_empty() && self.panes.get(&id).map(|t| t == token).unwrap_or(false)
    }

    /// Is `token` *some* legitimate identity — the fleet token, or any
    /// pane's own — without resolving *which*? Exactly what a control
    /// request's door check needs: at that point sock.rs doesn't know, and
    /// doesn't need to know, which pane (if any) the caller claims to be.
    pub fn is_principal(&self, token: &str) -> bool {
        !token.is_empty() && (token == self.control || self.panes.values().any(|t| t == token))
    }

    /// Resolve a token to the caller it represents: the fleet control token,
    /// or a pane acting via its own `ROOST_TOKEN`. Fails closed on
    /// empty/unknown (former `App::resolve_actor`'s logic).
    pub fn resolve_actor(&self, token: &str) -> Option<Actor> {
        if token.is_empty() {
            return None;
        }
        if token == self.control {
            return Some(Actor::Fleet);
        }
        self.panes.iter().find(|(_, t)| t.as_str() == token).map(|(id, _)| Actor::Pane(*id))
    }

    /// The fleet control token (written to `<state>/control.token` by startup).
    pub fn control(&self) -> &str {
        &self.control
    }
}

/// Sole owner of token state — the fleet control token plus every live
/// pane's own. NOT `Clone`; held by value in `App`, the only writer.
/// Internals: `Arc<RwLock<Arc<TokenSnapshot>>>`. A reader clones the inner
/// `Arc` under a read lock held for nanoseconds (see the free `load` fn
/// below); a writer builds the *next* snapshot entirely outside any lock
/// (`publish`) and holds the write lock only for the pointer store — so the
/// lock a door thread might contend on is never held across an allocation.
///
/// I3 (single writer) is enforced by the compiler, not by convention: the
/// three mutators (`set_pane_token`, `remove_pane_token`, `publish`) take
/// `&mut self`, so calling one requires exclusive access to this whole
/// value — and since `TokenTable` is owned by `App`, not `Arc`-shared, the
/// only way to get that is through `App`'s own `&mut self`. Two overlapping
/// `publish` calls (load → clone → mutate → write, not atomic as a whole)
/// can therefore never interleave; the borrow checker rejects it at compile
/// time, before the question of runtime locking even arises.
pub struct TokenTable {
    inner: Arc<RwLock<Arc<TokenSnapshot>>>,
}

impl TokenTable {
    /// Mints the fleet token from the OS CSPRNG. `None` means `/dev/urandom`
    /// is unreadable — the caller (`main.rs`, before any UI exists) decides
    /// that's fatal, the same refusal `App::new` used to make on its own.
    pub fn new() -> Option<TokenTable> {
        let control = gen_secret()?;
        let snapshot = TokenSnapshot { control, panes: HashMap::new() };
        Some(TokenTable { inner: Arc::new(RwLock::new(Arc::new(snapshot))) })
    }

    /// A load-only handle for sock.rs: `Clone`, no mutators — sock.rs is
    /// structurally unable to write the table it reads.
    pub fn reader(&self) -> TokenReader {
        TokenReader { inner: self.inner.clone() }
    }

    pub fn load(&self) -> Arc<TokenSnapshot> {
        load(&self.inner)
    }

    /// (Re)issue pane `id`'s token — the mint, on every (re)spawn. I2: the
    /// caller publishes this *before* starting the pane's child process, so
    /// no legitimate connection can ever race the snapshot.
    pub fn set_pane_token(&mut self, id: PaneId, token: String) {
        self.publish(|panes| {
            panes.insert(id, token);
        });
    }

    /// Forget pane `id`'s token — on close, so a stale connection's later
    /// (e.g. link-down) message carrying the old token can never
    /// authenticate again.
    pub fn remove_pane_token(&mut self, id: PaneId) {
        self.publish(|panes| {
            panes.remove(&id);
        });
    }

    /// Clone-mutate-store (RCU): build the next generation's pane map by
    /// applying `edit` to a clone of the current one — entirely outside any
    /// lock — then swap it into the shared `Arc` under the write lock, held
    /// only for that one pointer store. `&mut self`, not `&self`: see the
    /// I3 note on the struct doc above — this is what makes the sequence
    /// (load, clone, mutate, swap) safe despite not being atomic as a whole.
    fn publish(&mut self, edit: impl FnOnce(&mut HashMap<PaneId, String>)) {
        let cur = self.load();
        let mut panes = cur.panes.clone();
        edit(&mut panes);
        let next = Arc::new(TokenSnapshot { control: cur.control.clone(), panes });
        *self.inner.write().unwrap_or_else(|e| e.into_inner()) = next;
    }
}

/// Load-only view handed to sock.rs. `Clone`; no mutators exist on this type
/// at all — App's main loop is the sole writer, so single-writer RCU is
/// sound by type design, not by convention.
#[derive(Clone)]
pub struct TokenReader {
    inner: Arc<RwLock<Arc<TokenSnapshot>>>,
}

impl TokenReader {
    pub fn load(&self) -> Arc<TokenSnapshot> {
        load(&self.inner)
    }
}

/// Shared by `TokenTable::load`/`TokenReader::load`: read-lock, clone the
/// inner `Arc` (cheap — a pointer + refcount bump), drop the lock. Recovers
/// from poisoning the same way `sock.rs`'s own `lock` helper does
/// (sock.rs:384-386) — with exactly one writer (I3), a panicked reader can't
/// leave the state torn.
fn load(inner: &RwLock<Arc<TokenSnapshot>>) -> Arc<TokenSnapshot> {
    inner.read().unwrap_or_else(|e| e.into_inner()).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_deserializes_from_the_cli_json_shape() {
        // spawn with optional flags
        let r: Request =
            serde_json::from_str(r#"{"token":"t","method":"spawn","adapter":"pi","cwd":"/x"}"#)
                .unwrap();
        assert_eq!(r.token, "t");
        match r.method {
            Method::Spawn { adapter, cwd, initial_input } => {
                assert_eq!(adapter, "pi");
                assert_eq!(cwd.as_deref(), Some("/x"));
                assert!(initial_input.is_none());
            }
            _ => panic!("expected spawn"),
        }
        // read with a tail mode (tuple variant → {"tail": N})
        let r: Request =
            serde_json::from_str(r#"{"token":"t","method":"read","pane":3,"mode":{"tail":20}}"#)
                .unwrap();
        match r.method {
            Method::Read { pane, mode } => {
                assert_eq!(pane, 3);
                assert_eq!(mode, ReadMode::Tail(20));
            }
            _ => panic!("expected read"),
        }
        // bare list; and default read mode = screen
        assert!(matches!(
            serde_json::from_str::<Request>(r#"{"token":"t","method":"list"}"#).unwrap().method,
            Method::List
        ));
        let r: Request =
            serde_json::from_str(r#"{"token":"t","method":"read","pane":1}"#).unwrap();
        assert!(matches!(r.method, Method::Read { mode: ReadMode::Screen, .. }));
    }

    #[test]
    fn reply_serializes_untagged() {
        let s = serde_json::to_string(&Reply::ok(serde_json::json!({ "pane": 5 }))).unwrap();
        assert_eq!(s, r#"{"ok":{"pane":5}}"#);
        let s = serde_json::to_string(&Reply::err("nope")).unwrap();
        assert_eq!(s, r#"{"err":"nope"}"#);
    }

    // -- promotion-auth-gate: TokenTable/TokenSnapshot/TokenReader ----------

    #[test]
    fn snapshot_pane_authorized_matches_exactly_and_fails_closed() {
        let mut table = TokenTable::new().unwrap();
        table.set_pane_token(1, "secret-1".into());
        let snap = table.load();
        assert!(snap.pane_authorized(1, "secret-1"));
        assert!(!snap.pane_authorized(1, "wrong"));
        assert!(!snap.pane_authorized(1, ""));
        assert!(!snap.pane_authorized(99, "secret-1"), "unknown pane fails closed");
    }

    #[test]
    fn snapshot_is_principal_accepts_fleet_or_any_pane_token_only() {
        let mut table = TokenTable::new().unwrap();
        table.set_pane_token(1, "pane-1".into());
        let snap = table.load();
        assert!(snap.is_principal(snap.control()));
        assert!(snap.is_principal("pane-1"));
        assert!(!snap.is_principal("junk"));
        assert!(!snap.is_principal(""));
    }

    #[test]
    fn snapshot_resolve_actor_maps_fleet_and_pane_tokens_and_fails_closed() {
        let mut table = TokenTable::new().unwrap();
        table.set_pane_token(3, "pane-3".into());
        let snap = table.load();
        assert_eq!(snap.resolve_actor(snap.control()), Some(Actor::Fleet));
        assert_eq!(snap.resolve_actor("pane-3"), Some(Actor::Pane(3)));
        assert_eq!(snap.resolve_actor("nope"), None);
        assert_eq!(snap.resolve_actor(""), None);
    }

    /// The RCU contract: a snapshot already `load()`ed is a frozen,
    /// immutable view — a later mutation publishes a NEW snapshot; it never
    /// mutates the one a reader is still holding.
    #[test]
    fn mutating_the_table_never_changes_a_snapshot_already_loaded() {
        let mut table = TokenTable::new().unwrap();
        table.set_pane_token(1, "v1".into());
        let old = table.load();
        assert!(old.pane_authorized(1, "v1"));

        table.set_pane_token(1, "v2".into());
        assert!(old.pane_authorized(1, "v1"), "the old snapshot must not mutate in place");
        assert!(!old.pane_authorized(1, "v2"));

        let new = table.load();
        assert!(new.pane_authorized(1, "v2"));
        assert!(!new.pane_authorized(1, "v1"));
    }

    #[test]
    fn remove_pane_token_revokes_it_without_touching_other_panes() {
        let mut table = TokenTable::new().unwrap();
        table.set_pane_token(1, "v1".into());
        table.set_pane_token(2, "v2".into());
        table.remove_pane_token(1);
        let snap = table.load();
        assert!(!snap.pane_authorized(1, "v1"));
        assert!(snap.pane_authorized(2, "v2"));
    }

    /// The write/read split's whole point: a `TokenReader` is `Clone` and
    /// sees every publish made through the table, but has no mutator at all
    /// — sock.rs is structurally unable to write, not merely disciplined
    /// not to.
    #[test]
    fn reader_observes_writes_made_through_the_table() {
        let mut table = TokenTable::new().unwrap();
        let reader = table.reader();
        assert!(!reader.load().pane_authorized(1, "v1"));
        table.set_pane_token(1, "v1".into());
        assert!(reader.load().pane_authorized(1, "v1"));
    }
}
