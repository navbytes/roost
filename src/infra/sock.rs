//! Status socket (design doc §6.1): agent-side extensions/hooks report exact
//! status and session ids as newline-delimited JSON over a unix socket.
//!
//! Message shape (pane comes from the ROOST_PANE env var roost sets; token
//! from ROOST_TOKEN — roost drops any message whose token doesn't match the
//! one it issued to that pane, so panes can't spoof each other):
//!   { "pane": "3", "token": "<hex>", "event": "session", "session": "<uuid>" }
//!   { "pane": "3", "token": "<hex>", "event": "status",  "status": "working"
//!                                    | "waiting" | "needs_input" | "exited" }
//!
//! A "needs_input" status may carry an optional "message" — the question the
//! agent is asking (an ask-tool's prompt, a hook's notification reason).
//! Accepted on needs_input only, dropped at parse for every other status;
//! sanitized app-side before it reaches any UI surface.
//!
//! "exited" is still accepted for stale extensions but is advisory only —
//! `StatusTracker::set_extension_status` demotes it to Waiting. Process death
//! has exactly one ground truth (the pane's PTY EOF): the pane's env is
//! inherited by every descendant, so a nested pi finishing its work would
//! otherwise report *its* shutdown as the pane's death.
//!
//! D2: no new wire message — this file also tracks, per connection, which
//! pane(s) it has reported a status/session line for, and turns that into
//! `AppEvent::ExtLink` up/down events for `StatusTracker::set_ext_link`
//! (`ConnGuard`, `link_pane_token`). A connection *is* the liveness signal:
//! the pi extension holds one open for a pane's whole process lifetime, so
//! whether it's still open is itself evidence the reporting hook isn't dead.

use anyhow::{bail, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::core::control::{Reply, Request, TokenReader, UNAUTHORIZED_MSG};

/// Wall-clock deadline for a connection that has *not yet* sent a
/// well-formed control request (audit finding C1 — a second review pass on
/// the H1 fix). A separate, smaller *shared* pre-auth pool made this cheaper
/// to attack than the bug it replaced: 16 idle connections (one keepalive
/// byte each per <30s, no valid token needed) permanently shed every *new*
/// connection, including the human's CLI. The lesson: any shared pool sized
/// to bound an attacker also bounds the victim, because sock.rs can't tell
/// them apart — per-connection resources are the only ones an attacker
/// can't share with someone else. Bounding pre-auth connections by *time*
/// instead means a pile of squatters recycles on its own; nobody else is
/// ever blocked from getting in.
///
/// This must be enforced as real elapsed time since the connection was
/// accepted, not merely as `SO_RCVTIMEO` (`set_read_timeout`, below) — a
/// first cut of this fix used only the socket-level read timeout and
/// `read_until`, and that bounds a single `read()` *syscall*, not the
/// connection: a client drip-feeding one byte slower than a full line but
/// faster than the timeout (repro: one byte every 1.2s against a 2s
/// timeout) never trips it, and `read_until` loops internally with no
/// wall-clock check of its own, so it just kept accumulating — charged
/// nothing, never promoted, never timed out. `read_line_deadlined` drives
/// `fill_buf`/`consume` by hand instead and checks an `Instant` recorded at
/// connection start against this constant on every iteration, independent
/// of the socket timeout. The socket timeout is still set (it's what makes
/// that loop tick on a truly silent connection rather than block forever on
/// one `read()`), but it's no longer what *bounds* pre-auth admission.
/// `READ_TIMEOUT` (the generous ceiling) only applies once a connection is
/// promoted past this.
const PRE_AUTH_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// P0: no client read may block forever. Applies once a connection is
/// promoted past `PRE_AUTH_READ_TIMEOUT` (its first well-formed request) —
/// generous, since a legitimate caller may pause between commands. The
/// existing `Err(_) => break` below already exits without logging, so a
/// timeout firing is silent, not spammy.
///
/// Exception (P1): a connection promoted only by a status/session line,
/// never a control request (`guard.principal.is_none()` in
/// `spawn_accept_loop`), retries past this instead of ending the
/// connection — see the comment on that retry clause. That's the pi
/// extension's actual shape: it dials once and holds the socket for the
/// pane's whole process lifetime with no keepalive and no reconnect on
/// error, so a `READ_TIMEOUT` firing on it is an ordinary gap between an
/// agent's status transitions (a human reading the output for a while is
/// the normal case), not a hung client — the P0 wedge this constant exists
/// to prevent. A connection that *did* send a control request is
/// unaffected: it's still reaped here, unchanged.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Wall-clock budget for handing one whole reply to the client, enforced by
/// `write_reply`'s own loop. The write-side twin of `READ_TIMEOUT`, and the
/// only thing that bounds a connection parked in `write_reply`: P0 above says
/// no client *read* may block forever, nothing said the same about the reply,
/// and a blocking `write_all` on a peer that has stopped reading is reached
/// by none of the reaping paths here — `PRE_AUTH_READ_TIMEOUT` and
/// `READ_TIMEOUT` only ever fire on a thread that is in a `read()`. Repro:
/// `MAX_CONN` connections that each send one request and never read the
/// answer (pipelining more replies than their receive buffer holds does it
/// too); every connection thread wedges in `write_all`, the global pool sits
/// at `MAX_CONN` for as long as those peers live, and the accept loop sheds
/// every new client with nothing left that can ever free a slot — a black
/// hole only a restart clears. The identical flood with a *readable* peer is
/// reaped on `READ_TIMEOUT` and recovers, which is what isolates the write.
///
/// Wall-clock, not `SO_SNDTIMEO`, for exactly the reason `PRE_AUTH_READ_
/// TIMEOUT` is: a socket option bounds one `write()` syscall, not the reply.
/// A first cut used only `set_write_timeout` and `write_all`, and a peer that
/// accepts a few bytes per timeout window — `write_all` loops with no
/// wall-clock check of its own — simply stretched one reply across as many
/// windows as the payload has chunks. That is the same drip the C1 fix closed
/// on the read side, pointed the other way, and it showed up first as this
/// file's own regression test taking 10s alone but blowing a 30s deadline
/// under a loaded parallel suite run: with the bound proportional to the
/// payload rather than fixed, there was no honest number to assert.
///
/// Five seconds is enormous for a *local* socket, where any live client
/// drains a multi-megabyte `read` in milliseconds; a peer that cannot take
/// 256 KiB in five seconds is suspended, hung, or hostile, not slow. A reply
/// cut off here is truncated on the wire, which is why the connection ends
/// immediately after: like the oversize path, there is no framing left to
/// resynchronise a next request on.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// `SO_SNDTIMEO` for a client socket. Not a budget of its own — it exists so
/// a write to a peer that has stopped reading *returns* instead of blocking
/// forever, letting `write_reply` get back around to its `WRITE_TIMEOUT`
/// check. Exactly the role `set_read_timeout` plays for
/// `read_line_deadlined`. Short, so the enforced bound is `WRITE_TIMEOUT`
/// plus at most one tick rather than plus a whole second timeout window.
const WRITE_TICK: Duration = Duration::from_millis(250);

/// Max bytes accepted for one status line. A well-formed message is well under
/// this; a client that streams without a newline past the cap gets
/// `OVERSIZE_LINE_MSG` back instead of being allowed to grow an unbounded
/// buffer (local DoS) — then the connection is closed regardless, since
/// there is no newline left in it to resynchronise a next request on. Before
/// that reply existed the caller just saw the connection vanish (EPIPE),
/// which `cli.rs` reported as "cannot reach a running roost" — exactly the
/// wrong diagnosis, and the neighbouring size to the one that used to freeze
/// the whole process (PR #67), so it's exactly where someone lands right
/// after hitting that.
const MAX_LINE: u64 = 64 * 1024;

/// The exact reply for a line that hits `MAX_LINE`. `cli.rs` matches this
/// verbatim to choose the usage-error exit code (2) instead of the generic
/// runtime one (1) every other `err` reply here gets — this is the caller's
/// own oversized input, not a roost failure. A `pub const` shared by both
/// sides rather than hand-typed twice, same reasoning as
/// `UNSAFE_SOCKET_DIR_MSG` above: two copies would let a reword here quietly
/// stop matching what `cli.rs` checks.
pub const OVERSIZE_LINE_MSG: &str = "message too large (max 64 KiB); split it or use a file";

/// Cap concurrent client connections so a buggy/looping extension that
/// reconnects rapidly can't spawn unbounded threads/FDs.
const MAX_CONN: usize = 64;

/// Per-principal share of `MAX_CONN`, for connections that *have* sent a
/// well-formed request (DESIGN-control.md §5.6 / audit M3). Must be at least
/// `app.rs`'s `MAX_WAITS` (16, private to that file — a cross-file invariant
/// kept in sync by hand; NEED: consider making it `pub(crate)`): each parked
/// `wait` holds one connection open for its deferred reply, so a cap below
/// MAX_WAITS refuses a fleet-wide wait the rest of the system already
/// considers and bounds — a self-inflicted product regression, not a
/// security property. 20 leaves a few spare connections for other commands
/// (list/spawn/send) alongside a fully-parked 16-wait fleet, while still
/// bounding one principal under a third of the global pool.
const PRINCIPAL_MAX_CONN: usize = 20;

/// Token-bucket capacity for control *commands* (not connections, not raw
/// lines — see `LINE_BUCKET_CAPACITY`) from one principal — see
/// `Bucket`/`Limits::take_principal`. A real fan-out is two commands per
/// pane (`spawn` then `wait`); DESIGN-control.md §7 illustrates this with 10
/// panes for brevity, but a realistic fleet is closer to 20 panes = 40
/// commands back-to-back (audit finding M3). 64 covers that with room for a
/// dozen more interleaved `list`/`status`/`read` calls.
const PRINCIPAL_BUCKET_CAPACITY: f64 = 64.0;

/// Steady-state refill for one principal once its burst is spent. Real
/// control actions (spawn/send/read/close) are issued one at a time by a
/// human or an orchestrator reacting to a `wait` reply — `wait` exists
/// precisely so a caller never has to poll in a tight loop, so 5/s is
/// generous headroom above any legitimate cadence. This bucket is *not* by
/// itself an effective deterrent against the audit-log-rotation flood — see
/// `GLOBAL_BUCKET_CAPACITY` and DESIGN-control.md §5.6 for the corrected
/// (audit finding H3) arithmetic.
const PRINCIPAL_REFILL_PER_SEC: f64 = 5.0;

/// Per-*connection* token bucket for every line read, well-formed or not
/// (audit finding C2 — a second review pass on the H1/M2 per-line charge).
/// That charge originally landed on the *shared* aggregate bucket: one
/// connection spewing blank lines emptied it in milliseconds and held it at
/// zero, `rate limited`-ing every other connection on the socket — cheaper
/// than the M3 flood it replaced, and it serialized all control traffic on
/// the aggregate's mutex besides. A per-connection bucket is plain local
/// state (no lock, no shared map): the cost of a line flood lands on
/// whichever connection sent it, never on anyone else's. 128 is double
/// `PRINCIPAL_BUCKET_CAPACITY` so a legitimate connection's own request
/// burst is never throttled here first; 20/s matches the old aggregate
/// rate, now scoped to one connection instead of the whole control plane.
const LINE_BUCKET_CAPACITY: f64 = 128.0;
const LINE_REFILL_PER_SEC: f64 = 20.0;

/// Aggregate backstop across *all* principals, charged only once a request
/// has passed its own connection's line bucket and per-principal bucket and
/// is genuinely about to be dispatched to the main loop (audit finding C2)
/// — never for raw line volume, which is bounded per-connection instead.
/// This still bounds total command throughput regardless of how many
/// admitted identities a flood claims (finding H2). 256 (4x the
/// per-principal capacity) covers one principal's full burst with headroom
/// for a couple more concurrent principals bursting at once.
///
/// Audit finding H3 correction: `app.rs`'s audit-log write is bounded by
/// *bytes* attacker-controlled fields contribute (`sanitize`, app.rs, does
/// not truncate — NEED: fix there, out of this file's lane), not by call
/// count. With `MAX_LINE` (64 KiB) against a 4 MiB log kept at one
/// generation, as few as ~140 *allowed* requests roll it twice and erase
/// everything. At this bucket's 20/s steady state that's ~7s; at one
/// principal's 5/s alone it's ~22s. These buckets slow a flood from "as
/// fast as the wire allows" to that — they do not make the log-rotation
/// attack impractical by themselves; only truncating the audit line
/// (app.rs) does.
const GLOBAL_BUCKET_CAPACITY: f64 = 256.0;
const GLOBAL_REFILL_PER_SEC: f64 = 20.0;

/// Reject any control request whose token exceeds this, in `parse_control`,
/// before it can reach per-principal bookkeeping (audit finding M1). Real
/// tokens are 32 hex chars; 128 is generous headroom. Without this cap, a
/// same-uid attacker could set an arbitrarily long (up to `MAX_LINE`, 64 KiB)
/// token and make `evict_one`'s scan (bounded to `MAX_TRACKED_TOKENS`
/// entries) clone up to 64 KiB *per entry scanned* — tens of MiB of
/// alloc+memcpy, on the hot path, before the aggregate check even runs.
/// Cheapest fix: refuse it at the source rather than pay for it later.
const MAX_TOKEN_LEN: usize = 128;

/// Cap on distinct tokens tracked in `Limits::buckets` at once (see
/// `evict_one`). A same-uid attacker fully controls the wire `token` field,
/// so nothing stops it minting a fresh one per request; without a bound the
/// map — and with it, roost's memory — grows without limit. `MAX_TOKEN_LEN`
/// bounds the cost *per* tracked entry; this bounds how many entries there
/// can be. Real principal counts here are tiny (the fleet token plus however
/// many panes exist, itself accidentally bounded by terminal size —
/// MIN_SPLIT_COLS, per the audit); 512 is generous headroom above that, so
/// eviction only ever engages under actual token-rotation abuse, never
/// legitimate use.
const MAX_TRACKED_TOKENS: usize = 512;

/// The exact substring `main.rs`'s `is_unsafe_socket_dir` matches a
/// `spawn_listener` failure against to decide whether it's the one fatal
/// case (an attacker may have pre-created the socket directory) versus
/// every other failure, which degrades to "no control plane" instead (P2).
/// A `pub const` shared by the `bail!` below and that check, rather than two
/// hand-typed copies of the same wording: with two copies, rewording this
/// message here left nothing to notice the check in `main.rs` — or its
/// test, which built its own `anyhow!` strings instead of driving this
/// code — no longer matching it. Sharing the identifier makes that
/// divergence impossible to introduce by editing wording alone.
pub const UNSAFE_SOCKET_DIR_MSG: &str = "unsafe ownership/permissions";

/// Is `dir` owned by us with no group/other access? Refusing otherwise stops
/// an attacker who pre-created the runtime dir from hosting our control socket
/// (tmux does the same for its socket dir).
fn dir_is_private_and_ours(dir: &Path) -> bool {
    match fs::metadata(dir) {
        Ok(m) => m.uid() == unsafe { libc::geteuid() } && (m.mode() & 0o077) == 0,
        Err(_) => false,
    }
}

/// Remove the socket file on clean exit so a stale socket isn't left behind.
pub fn cleanup(path: &Path) {
    let _ = fs::remove_file(path);
}

use crate::core::event::AppEvent;
use crate::core::status::AgentStatus;
use crate::core::workspace::PaneId;

pub fn socket_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("ROOST_STATE") {
        return PathBuf::from(dir).join("roost.sock");
    }
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::state_dir()
                .or_else(dirs::data_local_dir)
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("roost")
        })
        .join("roost.sock")
}

#[derive(Deserialize)]
struct Msg {
    pane: serde_json::Value, // tolerate string or number
    event: String,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    status: Option<String>,
    /// Optional human-readable detail riding a status report — the question
    /// an ask-tool is asking on `needs_input`. Sanitized (control chars,
    /// length) app-side before display, like every other socket string.
    #[serde(default)]
    message: Option<String>,
}

/// A control request is any message carrying a `method` field (status/session
/// reports don't).
///
/// - `None` — not a control request at all (not JSON, or JSON with no
///   `method` key): falls through to the one-way `parse_line` path, exactly
///   as before.
/// - `Some(Err(_))` — it WAS addressed to the control plane (has `method`)
///   but failed to deserialize into a `Request`, or its token is longer than
///   `MAX_TOKEN_LEN` (audit finding M1). P0: the caller must reply with this
///   rather than dropping the line — a client's read has no timeout of its
///   own (`src/cli.rs`), so silently dropping a malformed request used to
///   hang it forever, and 64 of them (`MAX_CONN`) wedged the whole control
///   plane. The deserialization message is `serde_json`'s own: it already
///   names the offending field/variant and carries nothing more internal.
/// - `Some(Ok(_))` — parsed clean; dispatch as usual.
fn parse_control(line: &str) -> Option<Result<Request, String>> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v.get("method").is_none() {
        return None;
    }
    let req: Request = match serde_json::from_value(v) {
        Ok(req) => req,
        Err(e) => return Some(Err(e.to_string())),
    };
    if req.token.len() > MAX_TOKEN_LEN {
        return Some(Err(format!("token too long (max {MAX_TOKEN_LEN} bytes)")));
    }
    Some(Ok(req))
}

/// Serialize `reply` and write it back down `reader`'s stream, newline-
/// terminated (the wire framing every control reply uses). Returns false if
/// the client had already hung up; a reply that somehow fails to serialize
/// (Reply only ever holds a `String`/`serde_json::Value`) is treated as
/// nothing to send rather than a dead connection.
fn write_reply(reader: &mut BufReader<UnixStream>, reply: &Reply) -> bool {
    let Ok(mut json) = serde_json::to_string(reply) else { return true };
    json.push('\n');
    let mut buf = json.as_bytes();
    let deadline = Instant::now() + WRITE_TIMEOUT;
    // `write_all` by hand so the wall-clock deadline is checked between
    // syscalls (see `WRITE_TIMEOUT`) — `write_all`'s own loop has no notion of
    // one, so a peer taking a few bytes per `WRITE_TICK` would otherwise
    // stretch a single reply for as long as the payload is large.
    while !buf.is_empty() {
        if Instant::now() >= deadline {
            return false;
        }
        match reader.get_mut().write(buf) {
            Ok(0) => return false,
            Ok(n) => buf = &buf[n..],
            // A tick elapsed with the peer's buffer full, or a signal: both
            // just mean "come back and re-check the deadline".
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return false, // client hung up, or a real error
        }
    }
    true
}

fn parse_line(line: &str) -> Option<AppEvent> {
    let msg: Msg = serde_json::from_str(line).ok()?;
    let pane: PaneId = match &msg.pane {
        serde_json::Value::String(s) => s.parse().ok()?,
        serde_json::Value::Number(n) => n.as_u64()?,
        _ => return None,
    };
    // Missing token → empty string, which App rejects (fails closed).
    let token = msg.token.unwrap_or_default();
    // [F2] `parse_control` has enforced this on the control-request token
    // since M1; status/session lines never did. Same reasoning applies here
    // now that a token also becomes a `link_panes`/`AppEvent::ExtLink` key
    // (D2) — an oversized one is dropped exactly like any other malformed
    // line (`None` → the caller's `log_dropped`), before it can reach that
    // bookkeeping at all.
    if token.len() > MAX_TOKEN_LEN {
        return None;
    }
    match msg.event.as_str() {
        "session" => Some(AppEvent::Session(pane, token, msg.session?)),
        "status" => {
            let status = match msg.status?.as_str() {
                "working" => AgentStatus::Working,
                "waiting" => AgentStatus::Waiting,
                "needs_input" => AgentStatus::NeedsInput,
                "exited" => AgentStatus::Exited,
                _ => return None,
            };
            // Only needs_input carries a message (the ask-tool's question);
            // dropping it from other statuses here means no stale question
            // can ever ride a working/waiting report into the app.
            let message = if status == AgentStatus::NeedsInput {
                msg.message.filter(|m| !m.trim().is_empty())
            } else {
                None
            };
            Some(AppEvent::Status(pane, token, status, message))
        }
        _ => None,
    }
}

/// A token bucket: refills continuously at `refill_per_sec`, capped at
/// `capacity`; each command costs one token. `Instant`-based so it needs no
/// background timer thread — refill is computed lazily on each `take`.
struct Bucket {
    tokens: f64,
    last: Instant,
    capacity: f64,
    refill_per_sec: f64,
}

impl Bucket {
    fn new(capacity: f64, refill_per_sec: f64) -> Self {
        Bucket { tokens: capacity, last: Instant::now(), capacity, refill_per_sec }
    }

    /// Refill for elapsed wall-clock time since the last touch.
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
    }

    /// Refill, then take one token if available.
    fn take(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Refill, then report the resulting token count without consuming one —
    /// used by eviction (`evict_one`) to find the cheapest entry to drop.
    fn refill_and_peek(&mut self) -> f64 {
        self.refill();
        self.tokens
    }
}

/// `lock().unwrap()` that survives a poisoned mutex (audit finding L1). These
/// mutexes only ever guard plain counters/maps; a panic on *some other*
/// thread leaving one "poisoned" is not a reason for *this* thread to also
/// lose the ability to release its own slots — recovering the (still
/// structurally valid) inner state and proceeding beats a cascading panic
/// that leaks every connection behind it.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Connection and command accounting shared by the listener's accept loop
/// and every per-connection thread it spawns (DESIGN-control.md §5.6).
///
/// Every cap here is keyed on the caller's raw wire `token` string, not a
/// resolved `Actor` — sock.rs has no access to `App::resolve_actor` (that's
/// core/app.rs, a different owner) and doesn't need it: `resolve_actor`
/// itself is exact-string-match against the fleet token or one pane token
/// per pane, so distinct token strings already mean distinct principals.
/// Peer credentials (`SO_PEERCRED`/`getpeereid`) were considered instead and
/// rejected: in this same-uid threat model the uid is always the operator's
/// own, and the pid is the *client* process on the other end of one
/// connection — for the one-shot `roost <verb>` CLI that's a fresh pid every
/// single invocation, so a `for i in (seq 200000); roost list; end` flood
/// would never repeat a pid and a pid-keyed cap would never trip. The token
/// is the one thing that *is* stable across exactly that loop (one
/// `ROOST_TOKEN` env var, inherited by every re-exec), so it is not merely
/// "all we have" — it's the only stable handle on the actual attack shape.
///
/// A design constraint from a second review pass, worth stating plainly:
/// **any *shared* pool sized to bound an attacker also bounds the victim,
/// because sock.rs cannot tell them apart. Per-connection resources are the
/// only ones an attacker can't share with a victim.** Two mitigations here
/// used to be shared pools and got cheaper to attack than the bug they
/// replaced as a result (findings C1/C2); neither is any more:
/// - Pre-auth admission is bounded by a short read timeout
///   (`PRE_AUTH_READ_TIMEOUT`, see `spawn_accept_loop`), not a shared
///   counter — a pile of squatters can never prevent a fresh connection
///   from being accepted and promoted.
/// - Every line a connection sends costs one token from a *per-connection*
///   `Bucket` (`spawn_accept_loop`'s `line_bucket` — plain local state, no
///   lock, no map) before it's even parsed. The *shared* aggregate bucket
///   (`take_global`) is charged only once a request has already passed the
///   per-principal check and is genuinely about to be dispatched.
///
/// A connection only ever charges the identity it was *admitted* under,
/// never a token that merely appears in a later request's body.
/// `ConnGuard::principal` is set once, from the first well-formed request's
/// token, and every later charge on that connection uses that same value —
/// so varying the `token` field request-to-request on one already-open
/// connection can't mint a fresh per-principal bucket (audit finding H2). An
/// "unresolved" token — one that never admitted a connection — therefore
/// never becomes a `buckets` key at all.
struct Limits {
    /// Race-free global connection count.
    global: AtomicUsize,
    /// Open connections per token, so one principal can't eat the whole
    /// `MAX_CONN` pool. Entries are removed at 0, so this can never hold
    /// more entries than there are currently-open admitted connections.
    per_principal: Mutex<HashMap<String, usize>>,
    /// Command-rate bucket per *admitted* principal (see the struct doc
    /// comment above). Entries persist for the life of the listener — a
    /// flood-by-reconnect must not reset its budget. Bounded at
    /// `MAX_TRACKED_TOKENS` (see `evict_one`): sock.rs can't validate
    /// tokens, so leaving this unbounded would let the same attacker this
    /// map exists to throttle instead exhaust memory by minting a fresh
    /// admitted identity per connection — a security fix that opens a
    /// (smaller) security hole is not a deferral worth taking here.
    buckets: Mutex<HashMap<String, Bucket>>,
    /// Aggregate command-rate backstop — see `GLOBAL_BUCKET_CAPACITY`.
    global_bucket: Mutex<Bucket>,
    /// promotion-auth-gate: open *authenticated reporter* connections per
    /// pane — `REPORTER_MAX_CONN_PER_PANE` (8), a separate pool from
    /// `per_principal` (decision 8: a pane's own status links must not be
    /// able to starve its control budget, or vice versa). Entries removed
    /// at 0, same convention as `per_principal`. Reserved iff a
    /// `link_panes` entry is actually inserted — see `ConnGuard`'s doc
    /// comment for why that pairing is load-bearing.
    reporters: Mutex<HashMap<PaneId, usize>>,
}

impl Limits {
    fn new() -> Self {
        Limits {
            global: AtomicUsize::new(0),
            per_principal: Mutex::new(HashMap::new()),
            buckets: Mutex::new(HashMap::new()),
            global_bucket: Mutex::new(Bucket::new(GLOBAL_BUCKET_CAPACITY, GLOBAL_REFILL_PER_SEC)),
            reporters: Mutex::new(HashMap::new()),
        }
    }

    /// Atomically check-and-increment the global connection cap in one step.
    /// Was load-then-add (audit Info (a)): two threads could both observe
    /// `< MAX_CONN` and both add, landing a few over. `fetch_update` makes
    /// the check and the increment one atomic step — no window between them.
    fn try_reserve_global(&self) -> bool {
        self.global
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| (c < MAX_CONN).then_some(c + 1))
            .is_ok()
    }

    fn release_global(&self) {
        self.global.fetch_sub(1, Ordering::Relaxed);
    }

    /// Called once, for the first well-formed request seen on a connection:
    /// admit it under `token`'s per-principal connection cap, or refuse.
    fn try_reserve_principal(&self, token: &str) -> bool {
        let mut map = lock(&self.per_principal);
        let count = map.entry(token.to_string()).or_insert(0);
        if *count >= PRINCIPAL_MAX_CONN {
            false
        } else {
            *count += 1;
            true
        }
    }

    fn release_principal(&self, token: &str) {
        let mut map = lock(&self.per_principal);
        if let Some(count) = map.get_mut(token) {
            *count -= 1;
            if *count == 0 {
                map.remove(token);
            }
        }
    }

    /// The aggregate charge for one genuinely-dispatched command (audit
    /// finding C2) — called only after a request has already passed its
    /// connection's own line bucket and per-principal bucket, never for raw
    /// line volume (see the struct doc comment).
    fn take_global(&self) -> bool {
        lock(&self.global_bucket).take()
    }

    /// One command already admitted past `take_global`, charged against
    /// `principal` — the connection's *admitted* identity, not a raw
    /// per-request token (audit finding H2; see the struct doc comment).
    fn take_principal(&self, principal: &str) -> bool {
        let mut map = lock(&self.buckets);
        if map.len() >= MAX_TRACKED_TOKENS && !map.contains_key(principal) {
            evict_one(&mut map);
        }
        map.entry(principal.to_string())
            .or_insert_with(|| Bucket::new(PRINCIPAL_BUCKET_CAPACITY, PRINCIPAL_REFILL_PER_SEC))
            .take()
    }

    /// Admit one more authenticated reporter connection for `pane`, or
    /// refuse at `REPORTER_MAX_CONN_PER_PANE`. Mirrors `try_reserve_principal`
    /// exactly. Callers MUST only call this exactly once per eventual
    /// `link_panes` entry — see the "slot ⟺ entry" doc on the door's status/
    /// session handling for why (`MC-overflow-no-strand`).
    fn try_reserve_reporter(&self, pane: PaneId) -> bool {
        let mut map = lock(&self.reporters);
        let count = map.entry(pane).or_insert(0);
        if *count >= REPORTER_MAX_CONN_PER_PANE {
            false
        } else {
            *count += 1;
            true
        }
    }

    fn release_reporter(&self, pane: PaneId) {
        let mut map = lock(&self.reporters);
        if let Some(count) = map.get_mut(&pane) {
            *count -= 1;
            if *count == 0 {
                map.remove(&pane);
            }
        }
    }
}

/// Free one slot in `map` for a new token, at `MAX_TRACKED_TOKENS`. Picks the
/// entry with the most tokens *after* refilling it for elapsed idle time,
/// cloning a key only when it becomes the new leader — not, as an earlier
/// version did, unconditionally for every entry scanned (audit finding M1;
/// `MAX_TOKEN_LEN` bounds what a clone costs, this bounds how often one
/// happens).
///
/// The single "most tokens" comparison covers both cases the policy cares
/// about: a bucket idle long enough to have fully refilled (tokens ==
/// capacity) carries no enforcement state, so dropping it is free — and
/// since every entry here shares the same capacity, "full" is simply the
/// maximum possible token count, so it's always chosen first when one
/// exists. If nothing is full (an active flood of single-use identities,
/// none idle yet), the same comparison falls back to "evict the fullest",
/// exactly as it should. A principal still being actively throttled sits at
/// the *low* end of this ordering, so it's the last one picked, not the
/// first.
fn evict_one(map: &mut HashMap<String, Bucket>) {
    let mut best: Option<(String, f64)> = None;
    for (token, bucket) in map.iter_mut() {
        let tokens = bucket.refill_and_peek();
        if best.as_ref().map(|(_, best_tokens)| tokens > *best_tokens).unwrap_or(true) {
            best = Some((token.clone(), tokens));
        }
    }
    if let Some((token, _)) = best {
        map.remove(&token);
    }
}

/// [F2] Cap on distinct panes one connection's `link_panes` will track. A
/// real reporter — the pi extension, a Claude Code hook — serves exactly
/// one pane for its whole life; this is a defensive bound against a
/// buggy/malicious connection claiming to report for an unbounded number of
/// panes (each entry is small — a `PaneId` and a token — but nothing else
/// here caps how many distinct ones a single connection could otherwise
/// mint). Past the cap, a line for a *new* pane on that connection still
/// forwards its underlying status/session event exactly as before (App's
/// own token check is what actually judges it) — only this connection's own
/// link-liveness bookkeeping for that extra pane is skipped.
const MAX_LINK_PANES_PER_CONN: usize = 4;

/// promotion-auth-gate: cap on concurrent *authenticated reporter*
/// connections one pane will admit, enforced in `Limits.reporters` at the
/// same site as `link_panes` insertion (one mechanism, so the cap count,
/// `link_panes`, and App's `ext_link_counts` all count the same thing by
/// construction — no double-counting). Sizing: legit worst case is one
/// long-lived pi connection + one dying old-generation connection
/// mid-respawn + a burst of transient claude-hook one-shots (`roost
/// __status`, each lives milliseconds) — 8 is ~2x that. Reporter
/// connections do NOT share `PRINCIPAL_MAX_CONN`: that pool is sized
/// against control traffic (`MAX_WAITS` in app.rs), and charging reporters
/// there would let a pane's own status links starve its control budget.
///
/// ponytail: accepted edge — a >8-wide parallel claude PreToolUse hook
/// batch can exceed this. Zero operational cost: every parallel hook in
/// one batch reports the same `working`, the refused one-shot exits 0
/// (cli.rs's `status_hook` is silent-by-contract either way), and it's
/// refused before any `link_panes` insert (no strand, no false negative
/// on a real status change). Keep the cap at 8.
const REPORTER_MAX_CONN_PER_PANE: usize = 8;

/// RAII: releases whatever connection-accounting slots this connection holds
/// when dropped — including during a panic unwind (audit finding L1).
/// Without this, a panic partway through the read loop (e.g. a
/// poisoned-mutex `.unwrap()` after some *other* thread already panicked —
/// see `lock` above) would skip the release-at-the-bottom calls entirely and
/// leak the slot forever. `principal` is set at most once, the moment this
/// connection is admitted under a real principal (see the `Limits` doc
/// comment — before that, a connection is bounded by time, not a shared
/// slot, so there's nothing else here to release).
///
/// D2: also the one place that emits link-down. `link_panes` accumulates the
/// panes (and the token last seen for each) this connection has reported a
/// status/session line for — see the `promoted` doc comment in
/// `spawn_accept_loop` for where entries are added and link-up emitted.
/// Draining it here, in `Drop`, means every exit from the connection's read
/// loop — a clean EOF, a real read error, an oversized line, hitting a rate
/// limit and hanging up, *or* a panic mid-read — reaches the same one place,
/// instead of every `break`/`return` needing its own copy of "and tell App
/// this pane's link just went down". The benign `READ_TIMEOUT` retry for a
/// status-only connection (P1, `spawn_accept_loop`) is a `continue`, not a
/// `break`, so it never reaches here at all — no flap from that path.
///
/// [F1] `link_panes` being a map (keyed on pane, entry inserted at most once
/// per connection — see the `Entry::Vacant` guard below) is exactly what
/// makes `App::ext_link_counts` a sound refcount rather than a bool: this
/// connection contributes at most one link-up and, here, at most one
/// matching link-down *per pane*, no matter how many status/session lines
/// it actually sent. A pane with two open connections (e.g. a nested
/// claude's one-shot `roost __status` hooks alongside the pane's own
/// long-lived pi extension link) nets to +1/-1 per connection, never more —
/// so App's count always equals the number of connections genuinely still
/// open for that pane.
///
/// promotion-auth-gate: `link_panes` is ALSO now exactly the set of panes
/// this connection holds a `Limits.reporters` slot for — one is reserved
/// iff an entry is inserted (the door's status/session handling), so
/// draining the map here and releasing a reporter slot per entry keeps
/// slot and entry 1:1 by construction, the same way this struct already
/// keeps link-up and link-down 1:1 (`MC-overflow-no-strand`).
struct ConnGuard<'a> {
    limits: &'a Limits,
    principal: Option<String>,
    tx: SyncSender<AppEvent>,
    link_panes: HashMap<PaneId, String>,
}

impl Drop for ConnGuard<'_> {
    fn drop(&mut self) {
        self.limits.release_global();
        if let Some(token) = &self.principal {
            self.limits.release_principal(token);
        }
        for (pane, token) in self.link_panes.drain() {
            // Slot ⟺ entry: every `link_panes` entry was inserted alongside
            // a successful `try_reserve_reporter` (never on the
            // MAX_LINK_PANES_PER_CONN overflow-skip path — that path never
            // reserves in the first place), so releasing one per entry here
            // is exactly right, no more no less.
            self.limits.release_reporter(pane);
            // Best-effort: if the main loop is already gone the process is
            // shutting down anyway, and there is nothing left to tell.
            let _ = self.tx.send(AppEvent::ExtLink(pane, token, false));
        }
    }
}

/// `ev`'s pane/token, if it's a status or session report — the two event
/// kinds `parse_line` can produce that identify a pane the way D2's link
/// tracking keys on. Shared by the link-up and link-down (via `ConnGuard`)
/// paths so they agree on exactly what "this connection reports for pane P"
/// means.
fn link_pane_token(ev: &AppEvent) -> Option<(PaneId, String)> {
    match ev {
        AppEvent::Status(pane, token, ..) => Some((*pane, token.clone())),
        AppEvent::Session(pane, token, _) => Some((*pane, token.clone())),
        _ => None,
    }
}

/// Read one line (through the trailing `\n`, or up to EOF) into `buf`
/// (already cleared by the caller), capped at `MAX_LINE` bytes — same
/// return shape as `BufRead::read_until` (`Ok(0)` is EOF, `Ok(n)` is `n`
/// bytes appended). Drives `fill_buf`/`consume` by hand instead of calling
/// `read_until` so a wall-clock deadline can be enforced independently of
/// `set_read_timeout` (audit finding C1): `SO_RCVTIMEO` bounds a single
/// `read()` syscall, not the connection, and `read_until` loops internally
/// with no wall-clock check of its own, so a client drip-feeding one byte
/// slower than a full line but faster than the per-read timeout never trips
/// it and never returns. `pre_auth_start` — the `Instant` the connection was
/// accepted, or `None` once it's been promoted past pre-auth — is checked on
/// every iteration instead, closing that regardless of how the drip is
/// paced.
///
/// The per-read timeout set via `set_read_timeout` is still required
/// alongside this: without it, `fill_buf` would block on a silent
/// connection's `read()` syscall forever and this loop would never get back
/// around to checking the deadline at all. Once `pre_auth_start` is `None`
/// (promoted), a read-timeout error propagates immediately instead of being
/// retried here — same as `read_until` always did. For a connection that has
/// authenticated (has a `guard.principal`), that's still what bounds a fully
/// idle promoted connection (P0, `READ_TIMEOUT`); `spawn_accept_loop`'s call
/// site adds one more retry clause, past this function, for a promoted
/// connection that never has — a status-only reporter (P1).
fn read_line_deadlined(
    reader: &mut BufReader<UnixStream>,
    buf: &mut Vec<u8>,
    pre_auth_start: Option<Instant>,
) -> std::io::Result<usize> {
    loop {
        if let Some(start) = pre_auth_start {
            if start.elapsed() > PRE_AUTH_READ_TIMEOUT {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "pre-auth read deadline exceeded",
                ));
            }
        }
        let (done, used) = {
            let available = match reader.fill_buf() {
                Ok(chunk) => chunk,
                // Only retry past a per-read timeout while still pre-auth —
                // that's what lets the loop come back and re-check the
                // deadline above on a silent connection. Once promoted, any
                // read error (including a genuine idle READ_TIMEOUT) is
                // reported immediately, exactly as `read_until` always did.
                Err(e)
                    if pre_auth_start.is_some()
                        && matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
                {
                    continue;
                }
                Err(e) => return Err(e),
            };
            if available.is_empty() {
                return Ok(buf.len()); // EOF
            }
            match available.iter().position(|&b| b == b'\n') {
                Some(pos) => {
                    buf.extend_from_slice(&available[..=pos]);
                    (true, pos + 1)
                }
                None => {
                    // Cap bytes accepted for this line so a newline-less
                    // flood can't grow the buffer without bound.
                    let take = available.len().min((MAX_LINE as usize).saturating_sub(buf.len()));
                    buf.extend_from_slice(&available[..take]);
                    (false, take)
                }
            }
        };
        reader.consume(used);
        if done || buf.len() as u64 >= MAX_LINE {
            return Ok(buf.len());
        }
    }
}

/// After the oversized-line reply below, discard whatever this connection
/// sends next until it goes quiet (or `deadline` passes) — otherwise a
/// client still mid-`write()` of the oversized payload (this connection
/// stopped reading at `MAX_LINE`) is blocked on its own kernel send buffer
/// filling up, never reaches the `read()` that would see our reply, and gets
/// the same misleading EPIPE the reply exists to replace. A local socket's
/// default kernel buffer is small enough for this to matter well before
/// megabyte-sized payloads: 8 KiB each way is the macOS default. Draining
/// unblocks that write the same way the client finishing it naturally would.
/// Nothing here is buffered or parsed — this connection is already
/// committed to closing (no newline left to resynchronise a next request
/// on) — so a sender that never goes quiet just holds this one thread until
/// `deadline`, the same bound every other unproven connection gets, not
/// forever.
///
/// Each individual read is capped at `IDLE_GAP`, not the full remaining
/// budget — a first cut used `remaining` directly and blocked for nearly the
/// whole `deadline` on the *last*, data-free read before returning, since a
/// client that finishes writing and moves straight to its own read (the
/// common case, not an attack) never sends EOF here. That made every
/// oversized reply take ~`deadline` to close for no reason and, under a
/// loaded host, raced a test's own client-side read timeout of the same
/// order. `IDLE_GAP` only has to clear real scheduling jitter on a local
/// socket, not a network RTT, so it can stay far below `deadline` while
/// `deadline` still bounds a sender that never stops.
fn drain_until_quiet(reader: &mut BufReader<UnixStream>, deadline: Instant) {
    const IDLE_GAP: Duration = Duration::from_millis(200);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        let _ = reader.get_ref().set_read_timeout(Some(remaining.min(IDLE_GAP)));
        match reader.fill_buf() {
            Ok([]) => return, // EOF: peer fully done
            Ok(chunk) => {
                let n = chunk.len();
                reader.consume(n);
            }
            Err(_) => return, // quiet for IDLE_GAP (or past `deadline`), or a real error
        }
    }
}

/// Bind the socket and pump parsed events into the main loop. Returns the
/// bound path (exported to panes as ROOST_SOCK). `tokens` is a read-only
/// handle onto App's `TokenTable` (promotion-auth-gate) — the accept loop's
/// per-connection threads consult it synchronously, before promoting a
/// connection past the pre-auth guillotine, and never write it.
pub fn spawn_listener(tx: SyncSender<AppEvent>, tokens: TokenReader) -> Result<PathBuf> {
    let path = socket_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
        // The socket is the control plane (it can set session ids / status);
        // keep its directory private to the owner...
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        // ...and refuse to run if it isn't actually ours and private (an
        // attacker may have pre-created it to intercept the socket).
        if !dir_is_private_and_ours(dir) {
            bail!("roost: socket directory {} has {UNSAFE_SOCKET_DIR_MSG}", dir.display());
        }
    }
    let _ = fs::remove_file(&path); // stale socket from a previous run
    let listener = UnixListener::bind(&path)?;
    // Restrict the socket to the owner so another local user can't connect and
    // poison session ids / spoof status.
    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));

    spawn_accept_loop(listener, tx, tokens);
    Ok(path)
}

/// The accept loop, split out from `spawn_listener` so tests can drive it
/// against a scratch `UnixListener` bound at a throwaway path instead of the
/// real one (which is only reachable via the process-global `ROOST_STATE`/
/// `XDG_RUNTIME_DIR` env vars — racy to poke from tests that run in parallel
/// in the same process). Returns the shared `Limits` so tests can also
/// inspect accounting state directly instead of only through the wire.
fn spawn_accept_loop(listener: UnixListener, tx: SyncSender<AppEvent>, tokens: TokenReader) -> Arc<Limits> {
    let limits = Arc::new(Limits::new());
    let accept_limits = limits.clone();
    std::thread::spawn(move || {
        let limits = accept_limits;
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            // Shed load past the connection cap rather than spawning threads
            // without bound.
            if !limits.try_reserve_global() {
                drop(stream);
                // A wedged control plane (all 64 slots stuck) used to be
                // silent right here too — log the shed so it's diagnosable
                // instead of only visible as a client-side hang.
                log_debug("shed a connection: MAX_CONN (64) already open");
                continue;
            }
            let limits = limits.clone();
            let tx = tx.clone();
            let tokens = tokens.clone();
            std::thread::spawn(move || {
                // From here on, `guard` owns releasing every slot this
                // connection holds — including on an early return or a
                // panic unwind (audit finding L1).
                let mut guard = ConnGuard {
                    limits: &limits,
                    principal: None,
                    tx: tx.clone(),
                    link_panes: HashMap::new(),
                };

                // Short until promoted (audit finding C1): an idle or
                // slow-to-identify connection is recycled in seconds rather
                // than squatting a global slot for the full `READ_TIMEOUT`.
                // Extended once this connection proves itself with a
                // well-formed request, below.
                let _ = stream.set_read_timeout(Some(PRE_AUTH_READ_TIMEOUT));
                // Never extended on promotion, unlike the read timeout: this
                // is not a trust budget, just the tick that lets
                // `write_reply` re-check its wall-clock deadline (see
                // `WRITE_TICK`) instead of blocking forever on a peer that
                // stopped reading. Every reply on this connection —
                // dispatched, rate-limited, over-cap, oversize — goes through
                // that one function, so this covers all of them.
                let _ = stream.set_write_timeout(Some(WRITE_TICK));
                let mut reader = BufReader::new(stream);
                let mut buf = Vec::new();
                // Per-connection, unlocked (audit finding C2): every line
                // this connection sends costs one token here before the
                // shared aggregate is ever touched, so a flood's cost lands
                // on the connection that sent it, never on anyone else's.
                let mut line_bucket = Bucket::new(LINE_BUCKET_CAPACITY, LINE_REFILL_PER_SEC);
                // Wall-clock start of this connection (audit finding C1): the
                // deadline `read_line_deadlined` enforces while unpromoted is
                // measured from here, independent of the per-read
                // `SO_RCVTIMEO` set above.
                let start = Instant::now();
                // promotion-auth-gate: promoted on *authentication*, not
                // grammar. A control request needs `tokens.load().is_principal`
                // (fleet token or some pane's own); a status/session line
                // needs `tokens.load().pane_authorized` for the exact pane it
                // claims — see the two door checks below, right after
                // `parse_control`/`parse_line`. A connection that never sends
                // an authenticated line stays pre-auth and dies at
                // `PRE_AUTH_READ_TIMEOUT`, however many well-formed-but-wrong-
                // token lines it sends.
                //
                // A status-only connection (the pi extension: connects once,
                // holds the socket for the pane's process lifetime, sends
                // nothing but status/session lines) never sets
                // `guard.principal` — it isn't a "principal" for
                // connection-cap/rate-limit purposes — so gating promotion on
                // that alone would leave every such connection permanently
                // pre-auth: killed ~`PRE_AUTH_READ_TIMEOUT` after connecting
                // and never reconnecting (the extension dials once, on load).
                // `promoted` tracks "authenticated by *either* door", so this
                // shape is promoted on its first correctly-authenticated
                // status/session line same as a control request would be.
                let mut promoted = false;
                'conn: loop {
                    buf.clear();
                    // `None` once promoted — from then on only the per-read
                    // timeout applies (except the status-only retry just
                    // below).
                    let pre_auth_start = (!promoted).then_some(start);
                    let n = loop {
                        match read_line_deadlined(&mut reader, &mut buf, pre_auth_start) {
                            Ok(0) => break 'conn, // EOF
                            Ok(n) => break n,
                            // P1: a promoted connection that has never sent a
                            // control request (no `guard.principal`) is
                            // status-only — the pi extension's actual shape
                            // (extensions/roost.ts): dials once at pane
                            // start, holds the socket for the pane's whole
                            // process lifetime, no keepalive, no reconnect on
                            // error. A `READ_TIMEOUT` firing on it is the
                            // *normal* gap between an agent's status
                            // transitions (a human reading the output for a
                            // while — routinely >30s — is the everyday case,
                            // not an edge one), not a hung client, so retry
                            // past it exactly like a pre-auth connection
                            // retries past `PRE_AUTH_READ_TIMEOUT` above.
                            // Without this, the read timeout silently ended
                            // the connection here (`Err(_) => break`); the
                            // extension's next write then EPIPEs, nulls its
                            // socket handle for good (no reconnect anywhere
                            // in it), and every status/session report for
                            // that pane is dropped for the rest of its life.
                            //
                            // Retrying the inner loop (not `continue 'conn`)
                            // matters: `'conn`'s top clears `buf`, which
                            // would silently drop a line that had already
                            // partly arrived before this read timed out.
                            //
                            // A connection that DID authenticate
                            // (`guard.principal.is_some()`) is untouched:
                            // P0's wedge — reap a silent *control* client
                            // after `READ_TIMEOUT` — still applies to it
                            // unchanged. So is the pre-auth wall-clock
                            // deadline: it's enforced inside
                            // `read_line_deadlined` independently of
                            // `promoted`, and only ever returned while
                            // `pre_auth_start` is `Some` — before `promoted`
                            // (required below) can even be true.
                            Err(e)
                                if promoted
                                    && guard.principal.is_none()
                                    && matches!(
                                        e.kind(),
                                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                                    ) =>
                            {
                                continue;
                            }
                            Err(_) => break 'conn, // read error, or the pre-auth deadline
                        }
                    };
                    // Hit the cap without terminating — oversized line. Reply
                    // with the true diagnosis (see `OVERSIZE_LINE_MSG`), then
                    // close regardless: there is no newline left in this
                    // connection to resynchronise a next request on, so
                    // trying to keep parsing it would corrupt the framing.
                    // This sits ahead of `line_bucket`/principal/global —
                    // deliberately: an oversized line is never parsed, let
                    // alone dispatched, and it costs the sender their entire
                    // connection (one of only MAX_CONN) for a single
                    // attempt, a harsher toll than any of those buckets
                    // charge a well-formed line, so there is no cheaper flood
                    // to be had by skipping them here.
                    if buf.last() != Some(&b'\n') && n as u64 == MAX_LINE {
                        if write_reply(&mut reader, &Reply::err(OVERSIZE_LINE_MSG)) {
                            // The client may still be mid-write of the
                            // oversized payload itself — drain what's left,
                            // bounded like any other unproven connection, so
                            // that write can finish and the client actually
                            // reaches the read() that sees this reply (see
                            // `drain_until_quiet`).
                            drain_until_quiet(&mut reader, Instant::now() + PRE_AUTH_READ_TIMEOUT);
                        }
                        break;
                    }
                    let Ok(line) = std::str::from_utf8(&buf) else { continue };
                    let line = line.trim_end();

                    let control = parse_control(line);

                    // Audit finding C2: every line costs one token from
                    // this connection's own bucket first — a flood of
                    // blank/garbage/status lines must not be free just
                    // because it's never dispatched to App, but the cost
                    // must land here, not on a resource other connections
                    // share.
                    if !line_bucket.take() {
                        if control.is_some() {
                            // Control-shaped: the caller expects exactly one
                            // reply per line — never drop it silently.
                            let msg = "rate limited: too many commands too fast; \
                                       slow down and retry"
                                .to_string();
                            if !write_reply(&mut reader, &Reply::err(msg)) {
                                break; // client hung up
                            }
                        } else {
                            // Status/session report or garbage: no reply is
                            // expected either way — drop this line's effects
                            // and keep reading, same as a malformed one.
                            log_dropped(line);
                        }
                        continue;
                    }

                    // Control request (has a `method`): execute on the main loop
                    // and write the reply back down this connection. P0: a
                    // request that parsed as JSON-with-a-method but not into a
                    // `Request` still gets a reply (the error arm) instead of
                    // being dropped — see `parse_control`.
                    match control {
                        Some(Ok(req)) => {
                            // promotion-auth-gate door: is `req.token` the
                            // fleet token or some pane's own — *some*
                            // legitimate identity, not yet which one (App's
                            // `resolve_actor`, at dispatch below, resolves
                            // that and is what actually authorizes the verb).
                            // Checked on *every* control request on this
                            // connection, not only its first: a connection
                            // already promoted by an earlier valid request
                            // gets no free pass for a later one that claims a
                            // different, invalid token. Refuse here — reply,
                            // never dispatch, never reserve a principal slot,
                            // never promote — and let the pre-auth wall clock
                            // (measured from `start`, independent of this
                            // check) close it at `PRE_AUTH_READ_TIMEOUT` if
                            // this was its only line. Not dispatching also
                            // means an unauthenticated flood can no longer
                            // cost the audit log a single line (I4/§ H3).
                            if !tokens.load().is_principal(&req.token) {
                                let _ = write_reply(&mut reader, &Reply::err(UNAUTHORIZED_MSG));
                                continue;
                            }
                            // First well-formed *authenticated* request on
                            // this connection: charge it against `req.token`'s
                            // per-principal connection cap and, if admitted,
                            // promote this connection out of the pre-auth
                            // timeout. A later request on this same connection
                            // that claims a *different* token is never
                            // re-attributed — every charge below uses
                            // `guard.principal`, the identity this connection
                            // was actually admitted under (audit finding H2).
                            if guard.principal.is_none() {
                                if !limits.try_reserve_principal(&req.token) {
                                    let msg = format!(
                                        "connection limit: this token already has \
                                         {PRINCIPAL_MAX_CONN} open connections; close one and retry"
                                    );
                                    let _ = write_reply(&mut reader, &Reply::err(msg));
                                    break; // over cap: free the slot now
                                }
                                guard.principal = Some(req.token.clone());
                            }
                            // Promoted: this connection identified itself, so
                            // it earns the generous timeout (audit finding
                            // C1). Idempotent — a second/third request on an
                            // already-promoted connection must not re-touch
                            // the socket option every time.
                            if !promoted {
                                promoted = true;
                                let _ = reader.get_ref().set_read_timeout(Some(READ_TIMEOUT));
                            }
                            let principal = guard.principal.clone().expect("just set above");
                            // Rate limit: a command this connection is
                            // otherwise allowed to make can still be
                            // throttled if it's coming too fast. Checked
                            // (and charged) before the request ever reaches
                            // the main loop / audit log — a throttled
                            // request must cost nothing there (M2).
                            //
                            // NOTE (low-severity ordering inversion, not
                            // fixed here): this per-principal gate runs
                            // *before* the shared `take_global` one below,
                            // so a request that's ultimately refused by the
                            // global backstop still burns one of the
                            // caller's own principal tokens first — a
                            // victim sharing the aggregate with a genuine
                            // flood pays for requests it never got to make.
                            // Checking the shared gate first would avoid
                            // that, but reordering is a behavior change and
                            // out of scope for this pass.
                            if !limits.take_principal(&principal) {
                                let msg = "rate limited: too many commands too fast; \
                                           slow down and retry"
                                    .to_string();
                                if !write_reply(&mut reader, &Reply::err(msg)) {
                                    break; // client hung up
                                }
                                continue; // stay connected; this is not a ban
                            }
                            // Only a request that's actually about to be
                            // dispatched charges the shared aggregate
                            // (audit finding C2) — never raw line volume.
                            if !limits.take_global() {
                                let msg = "rate limited: too many commands too fast; \
                                           slow down and retry"
                                    .to_string();
                                if !write_reply(&mut reader, &Reply::err(msg)) {
                                    break; // client hung up
                                }
                                continue;
                            }
                            let (rtx, rrx) = std::sync::mpsc::channel();
                            if tx.send(AppEvent::Command(req, rtx)).is_err() {
                                break; // main gone
                            }
                            let Ok(reply) = rrx.recv() else { break };
                            if !write_reply(&mut reader, &reply) {
                                break; // client hung up
                            }
                            continue;
                        }
                        Some(Err(msg)) => {
                            if !write_reply(&mut reader, &Reply::err(msg)) {
                                break; // client hung up
                            }
                            continue;
                        }
                        None => {}
                    }
                    match parse_line(line) {
                        Some(ev) => {
                            // `parse_line` only ever produces `Status`/
                            // `Session` (see its match on `msg.event`), both
                            // of which `link_pane_token` matches — this is
                            // the single place both the door check and the
                            // link-tracking below get `(pane, token)` from,
                            // so they can never read it two different ways.
                            let Some((pane, token)) = link_pane_token(&ev) else {
                                log_dropped(line);
                                continue;
                            };
                            // promotion-auth-gate door: does `token`
                            // authenticate *this exact pane* — the same
                            // predicate (`TokenSnapshot::pane_authorized`)
                            // App's own `socket_authorized` re-checks at
                            // dispatch (main.rs), so the two can't drift.
                            // Unlike the control door above, a failure here
                            // doesn't even reply: a status/session line has
                            // no request/reply shape on the wire, so there is
                            // nothing this caller reads back either way —
                            // App would reject it downstream regardless
                            // (I4), so dropping here just saves that hop and
                            // leaves the connection unpromoted.
                            if !tokens.load().pane_authorized(pane, &token) {
                                log_dropped(line);
                                continue;
                            }
                            // D2 + promotion-auth-gate reporter cap: the
                            // first accepted status/session line for a given
                            // pane on this connection makes it that pane's
                            // live reporting link — tell App (link-up) and
                            // remember it so `ConnGuard`'s `Drop` can tell
                            // App again (link-down) whenever this connection
                            // ends, however it ends. A later line for a pane
                            // already in the map doesn't re-emit or re-check
                            // any cap; the connection's liveness hasn't
                            // changed, only its status has (the `ev` send
                            // below).
                            //
                            // Slot ⟺ entry, pinned (MC-overflow-no-strand): a
                            // `Limits.reporters` slot is reserved iff a
                            // `link_panes` insert is about to happen — the
                            // two calls are adjacent, on purpose, so there is
                            // no window where one could succeed without the
                            // other. [F2]'s existing MAX_LINK_PANES_PER_CONN
                            // (this CONNECTION's own cap on distinct panes)
                            // is checked *first*: when IT is what's full, the
                            // reserve is never even attempted — reserving
                            // there would strand a slot with no entry to ever
                            // release it, slowly eating the pane's cap. Both
                            // checks are one `&&`-condition (not nested
                            // `if`s), so there is no separate branch that
                            // could reach the reserve while the CONN cap
                            // check is what's failing.
                            if !guard.link_panes.contains_key(&pane)
                                && guard.link_panes.len() < MAX_LINK_PANES_PER_CONN
                            {
                                if limits.try_reserve_reporter(pane) {
                                    guard.link_panes.insert(pane, token.clone());
                                    let _ = tx.send(AppEvent::ExtLink(pane, token, true));
                                } else if !promoted {
                                    // This pane is this connection's first
                                    // successful authentication, and it's
                                    // already at REPORTER_MAX_CONN_PER_PANE:
                                    // there is nothing this connection is
                                    // authenticated to report on, so there's
                                    // no reason to hold it open. No reply —
                                    // same as every other status/session-path
                                    // refusal, this wire shape has none;
                                    // roost.ts's retry covers the transient
                                    // case.
                                    log_debug(&format!(
                                        "shed a reporter: pane {pane} already at \
                                         REPORTER_MAX_CONN_PER_PANE ({REPORTER_MAX_CONN_PER_PANE})"
                                    ));
                                    break;
                                }
                                // else: already promoted via an earlier pane
                                // on this same connection — don't kill it for
                                // a second pane's cap. Skip link tracking for
                                // `pane` only; `ev` still forwards below,
                                // same as the MAX_LINK_PANES_PER_CONN
                                // overflow case.
                            }
                            // else (either guard above false): a pane already
                            // tracked needs no new reserve/insert, and the
                            // MAX_LINK_PANES_PER_CONN overflow case ([F2],
                            // unchanged) never attempts one — either way `ev`
                            // still forwards below.
                            // A well-formed, authenticated status/session
                            // report promotes this connection exactly like a
                            // control request does, above — see the
                            // `promoted` doc comment at its declaration.
                            // Unaffected by the cap branches above: a pane
                            // past its own cap on an already-promoted
                            // connection must not un-promote it, and the
                            // close-on-cap-at-first-pane path already `break`s
                            // before reaching here.
                            if !promoted {
                                promoted = true;
                                let _ = reader.get_ref().set_read_timeout(Some(READ_TIMEOUT));
                            }
                            if tx.send(ev).is_err() {
                                break;
                            }
                        }
                        // A malformed line usually means a broken extension /
                        // hook integration — log it (ROOST_DEBUG) so it's
                        // debuggable instead of silently vanishing.
                        None => log_dropped(line),
                    }
                }
                // `guard` drops here (or during an unwind), releasing the
                // global slot plus the per-principal one if this connection
                // was ever admitted — exactly once.
            });
        }
    });
    limits
}

/// Append `line` to `<state>/roost.log` when ROOST_DEBUG is set. No-op
/// otherwise (and never touches the TUI's stdout) — shared by every socket
/// event that's silent by default but must be diagnosable on request.
fn log_debug(line: &str) {
    if std::env::var_os("ROOST_DEBUG").is_none() {
        return;
    }
    use std::io::Write;
    let log = socket_path().with_file_name("roost.log");
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(log) {
        let _ = writeln!(f, "{line}");
    }
}

/// An unparseable socket line usually means a broken extension/hook
/// integration — log it so it's debuggable instead of silently vanishing.
fn log_dropped(line: &str) {
    log_debug(&format!("dropped malformed socket line: {line}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::control::TokenTable;
    use std::io::Read;

    #[test]
    fn parses_string_and_numeric_pane_ids() {
        let ev = parse_line(r#"{"pane":"7","event":"status","token":"tok","status":"working"}"#);
        assert!(matches!(ev, Some(AppEvent::Status(7, ref t, AgentStatus::Working, _)) if t == "tok"));
        let ev = parse_line(r#"{"pane":7,"event":"session","token":"tok","session":"abc-123"}"#);
        match ev {
            Some(AppEvent::Session(7, t, s)) => {
                assert_eq!(t, "tok");
                assert_eq!(s, "abc-123");
            }
            _ => panic!("expected session event"),
        }
    }

    #[test]
    fn missing_token_parses_as_empty_and_is_rejected_downstream() {
        // A message without a token still parses (empty token), but App's
        // socket_authorized fails closed on an empty token.
        let ev = parse_line(r#"{"pane":"7","event":"status","status":"working"}"#);
        assert!(matches!(ev, Some(AppEvent::Status(7, ref t, _, _)) if t.is_empty()));
    }

    #[test]
    fn ignores_garbage() {
        assert!(parse_line("not json").is_none());
        assert!(parse_line(r#"{"pane":"x","event":"status","status":"working"}"#).is_none());
        assert!(parse_line(r#"{"pane":"1","event":"status","status":"???"}"#).is_none());
    }

    /// The optional question text rides `needs_input` only — on any other
    /// status (or as pure whitespace) it's dropped at the door, so no stale
    /// or sneaky message can enter through a working/waiting report.
    #[test]
    fn status_message_rides_needs_input_only() {
        let ev = parse_line(
            r#"{"pane":7,"token":"tok","event":"status","status":"needs_input","message":"pick a database"}"#,
        );
        assert!(
            matches!(ev, Some(AppEvent::Status(7, _, AgentStatus::NeedsInput, Some(ref m))) if m == "pick a database")
        );
        let ev = parse_line(
            r#"{"pane":7,"token":"tok","event":"status","status":"working","message":"sneaky"}"#,
        );
        assert!(matches!(ev, Some(AppEvent::Status(7, _, AgentStatus::Working, None))));
        let ev = parse_line(
            r#"{"pane":7,"token":"tok","event":"status","status":"needs_input","message":"  "}"#,
        );
        assert!(matches!(ev, Some(AppEvent::Status(7, _, AgentStatus::NeedsInput, None))));
    }

    // P0: a line that carries a `method` key was addressed to the control
    // plane, so a deserialization failure must come back as `Some(Err(_))`
    // (the caller replies `{"err": ...}`) rather than `None` (the caller
    // drops it, and the client's read — which has no timeout — hangs
    // forever). One case per failure class the report reproduced live.

    #[test]
    fn control_request_missing_token_is_an_err_not_a_drop() {
        let err = parse_control(r#"{"method":"list"}"#)
            .expect("has a `method` key: must not fall through as a non-control line")
            .expect_err("token is a required field");
        assert!(err.contains("token"), "error should name the offending field: {err}");
    }

    #[test]
    fn control_request_unknown_method_is_an_err_not_a_drop() {
        let err = parse_control(r#"{"token":"t","method":"frobnicate"}"#)
            .expect("has a `method` key")
            .expect_err("frobnicate is not a real verb");
        assert!(err.contains("frobnicate"), "error should name the offending value: {err}");
    }

    #[test]
    fn control_request_missing_required_field_is_an_err_not_a_drop() {
        // `send` requires `text`.
        let err = parse_control(r#"{"token":"t","method":"send","pane":1}"#)
            .expect("has a `method` key")
            .expect_err("text is required for send");
        assert!(err.contains("text"), "error should name the offending field: {err}");
    }

    #[test]
    fn control_request_wrong_field_type_is_an_err_not_a_drop() {
        // `pane` is a numeric PaneId; a string must not silently coerce.
        let err = parse_control(r#"{"token":"t","method":"send","pane":"three","text":"hi"}"#)
            .expect("has a `method` key")
            .expect_err("pane must be numeric");
        assert!(err.contains("pane") || err.to_lowercase().contains("u64"), "error should name the offending field/type: {err}");
    }

    /// The tri-state split itself: well-formed still dispatches, and a line
    /// with no `method` key is still left to `parse_line` untouched.
    #[test]
    fn control_request_well_formed_parses_ok_and_non_control_lines_stay_none() {
        let req = parse_control(r#"{"token":"t","method":"list"}"#).unwrap().unwrap();
        assert!(matches!(req.method, crate::core::control::Method::List));
        assert!(parse_control("not json").is_none());
        assert!(parse_control(r#"{"pane":"1","event":"status","status":"working"}"#).is_none());
    }

    /// Audit finding M1: an oversized token must be rejected as a malformed
    /// request, before it can ever reach per-principal bookkeeping.
    #[test]
    fn parse_control_rejects_an_oversized_token() {
        let long_token = "a".repeat(MAX_TOKEN_LEN + 1);
        let line = format!(r#"{{"token":"{long_token}","method":"list"}}"#);
        let err = parse_control(&line)
            .expect("has a `method` key")
            .expect_err("a token past MAX_TOKEN_LEN must be rejected");
        assert!(err.contains("token"), "error should name the offending field: {err}");

        // Right at the boundary is still fine.
        let ok_token = "a".repeat(MAX_TOKEN_LEN);
        let line = format!(r#"{{"token":"{ok_token}","method":"list"}}"#);
        assert!(parse_control(&line).unwrap().is_ok(), "exactly MAX_TOKEN_LEN must still be accepted");
    }

    #[test]
    fn global_connection_cap_reservation_is_race_free() {
        // Info (a): the old load-then-add could let two threads both pass
        // the check before either incremented. Hammer the same `Limits` from
        // far more threads than MAX_CONN, concurrently, and require the
        // count of successful reservations to land exactly on MAX_CONN —
        // never more (a race letting extra through) and never fewer (a bug
        // serializing when it shouldn't).
        let limits = Arc::new(Limits::new());
        let handles: Vec<_> = (0..MAX_CONN * 4)
            .map(|_| {
                let limits = limits.clone();
                std::thread::spawn(move || limits.try_reserve_global())
            })
            .collect();
        let successes = handles.into_iter().map(|h| h.join().unwrap()).filter(|&ok| ok).count();
        assert_eq!(successes, MAX_CONN, "exactly MAX_CONN reservations should win under contention");
        assert_eq!(limits.global.load(Ordering::Relaxed), MAX_CONN, "counter must not exceed MAX_CONN");
    }

    /// Audit finding L1: a panic between reserving and releasing (e.g. a
    /// poisoned-mutex `.unwrap()` after some *other* thread already
    /// panicked) must not leak the slot forever.
    #[test]
    fn a_panic_mid_connection_still_releases_its_slots() {
        let limits = Arc::new(Limits::new());
        assert!(limits.try_reserve_global());

        let l = limits.clone();
        let (tx, _rx) = std::sync::mpsc::sync_channel(1);
        let result = std::thread::spawn(move || {
            let _guard = ConnGuard { limits: &l, principal: None, tx, link_panes: HashMap::new() };
            panic!("simulated poisoned-mutex unwind mid-connection");
        })
        .join();

        assert!(result.is_err(), "setup bug: the thread should have panicked");
        assert_eq!(limits.global.load(Ordering::Relaxed), 0, "a panic must not leak the global slot");
    }

    /// Audit finding L1: `lock()` must recover a poisoned mutex rather than
    /// propagate the poison into a second panic (which is worse than the
    /// leak it would be guarding against).
    #[test]
    fn lock_survives_a_poisoned_mutex() {
        let m = Mutex::new(5);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = m.lock().unwrap();
            panic!("poison it");
        }));
        assert!(m.is_poisoned());
        assert_eq!(*lock(&m), 5, "lock() must still recover the (structurally valid) inner value");
    }

    // --- promotion-auth-gate: the door itself -------------------------------
    //
    // MC-gate-control and MC-gate-status: named mutation checks (design doc
    // "Test plan with mutation checks"). Both use a pane/token that's
    // genuinely REGISTERED (not merely well-formed) so a passing run proves
    // the door judges identity, not grammar — reverting either door check
    // (restoring grammar-only promotion) must turn the matching test red.

    /// MC-gate-control: a control request whose token matches nobody (fleet
    /// or any pane) gets back the exact shared `UNAUTHORIZED_MSG` — never an
    /// `ok`, which is what `fake_app`/`capturing_app` would have replied had
    /// this actually been dispatched, so the reply's own shape is already
    /// the proof of "never dispatched". `per_principal` staying empty and
    /// nothing landing on `rx` are the same claim from two more angles:
    /// no admission bookkeeping, no stray event. The connection then dies at
    /// the pre-auth deadline, exactly like one that sent nothing at all.
    /// *Reds if the door check on the control path is removed, or if the
    /// door starts dispatching to App regardless.*
    #[test]
    fn mc_gate_control_junk_token_is_refused_at_the_door_not_dispatched() {
        let (path, listener) = scratch_listener("mc-gate-control");
        let (tx, rx) = capturing_app();
        let limits = spawn_accept_loop(listener, tx, seeded_reader(&[(1, "real-token")]));
        let mut c = connect(&path);

        let reply = roundtrip(&mut c, r#"{"token":"junk","method":"list"}"#);
        let err = reply.get("err").and_then(|v| v.as_str()).expect("junk token must error");
        assert_eq!(err, UNAUTHORIZED_MSG, "must be the exact shared unauthorized string");

        assert!(
            limits.per_principal.lock().unwrap().is_empty(),
            "an unauthenticated control request must never reserve a principal slot"
        );
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "an unauthenticated control request must never emit anything to the main loop"
        );

        // Never promoted: dies at the pre-auth wall clock, not the generous
        // post-promotion READ_TIMEOUT.
        c.get_ref().set_read_timeout(Some(PRE_AUTH_READ_TIMEOUT + Duration::from_secs(3))).unwrap();
        let mut discard = String::new();
        let n = c.read_line(&mut discard).expect("must be closed at the pre-auth deadline, not hang");
        assert_eq!(n, 0, "an unauthenticated control connection must die at the 2s deadline: {discard:?}");

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// MC-gate-status: pane 1 has a real, seeded token — but this line
    /// presents a different (`"junk"`) one, the parseable-but-wrong-token
    /// case decision 6 says must drop silently: no reply (status lines have
    /// none to give), no promotion, no forward, no link-up. *Reds if the
    /// door check on the status path is removed* (grammar-promotion
    /// restored) — a merely-parseable line would then promote and forward
    /// exactly as it used to.
    #[test]
    fn mc_gate_status_wrong_token_for_a_real_pane_never_promotes() {
        let (path, listener) = scratch_listener("mc-gate-status");
        let (tx, rx) = capturing_app();
        spawn_accept_loop(listener, tx, seeded_reader(&[(1, "T")]));
        let mut c = connect(&path);
        c.get_mut()
            .write_all(br#"{"pane":1,"token":"junk","event":"status","status":"working"}"#)
            .unwrap();
        c.get_mut().write_all(b"\n").unwrap();

        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "an unauthenticated status line must never emit anything to the main loop — no link-up, no status"
        );

        // Never promoted: dies at the pre-auth wall clock.
        c.get_ref().set_read_timeout(Some(PRE_AUTH_READ_TIMEOUT + Duration::from_secs(3))).unwrap();
        let mut discard = String::new();
        let n = c.read_line(&mut discard).expect("must be closed at the pre-auth deadline, not hang");
        assert_eq!(n, 0, "an unauthenticated-but-parseable status connection must die at the 2s deadline: {discard:?}");

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    // --- promotion-auth-gate: the per-pane reporter cap ---------------------

    /// MC-pane-cap: `REPORTER_MAX_CONN_PER_PANE` (8) valid reporter
    /// connections for one pane are all admitted (each earns a link-up); a
    /// 9th is refused promptly — no link-up, connection closed well before
    /// `READ_TIMEOUT` — and closing one of the eight then frees a slot for a
    /// fresh one, proving the release actually happens at `ConnGuard` drop,
    /// not just that the reserve works. *Reds if the cap or its release is
    /// removed.*
    #[test]
    fn mc_pane_cap_admits_exactly_eight_reporters_then_recycles_on_close() {
        let (path, listener) = scratch_listener("mc-pane-cap");
        let (tx, rx) = capturing_app();
        spawn_accept_loop(listener, tx, seeded_reader(&[(1, "t")]));

        let line = br#"{"pane":1,"token":"t","event":"status","status":"working"}"#;
        let mut conns = Vec::new();
        for i in 0..REPORTER_MAX_CONN_PER_PANE {
            let mut c = connect(&path);
            c.get_mut().write_all(line).unwrap();
            c.get_mut().write_all(b"\n").unwrap();
            let up = rx.recv_timeout(Duration::from_secs(2)).expect("setup: link-up never arrived");
            assert!(
                matches!(up, AppEvent::ExtLink(1, ref t, true) if t == "t"),
                "reporter {i} (of {REPORTER_MAX_CONN_PER_PANE}) should be admitted"
            );
            rx.recv_timeout(Duration::from_secs(2)).expect("setup: status report never arrived");
            conns.push(c);
        }

        // The 9th is refused: no link-up ever, and the connection closes
        // promptly — the door's cap-full/close path, well before
        // `READ_TIMEOUT` (this is its FIRST line, so it was never promoted).
        let mut over = connect(&path);
        over.get_ref().set_read_timeout(Some(PRE_AUTH_READ_TIMEOUT + Duration::from_secs(3))).unwrap();
        over.get_mut().write_all(line).unwrap();
        over.get_mut().write_all(b"\n").unwrap();
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "the 9th reporter for one pane must never get a link-up"
        );
        let mut discard = String::new();
        let n = over.read_line(&mut discard).expect("the 9th reporter must be closed, not hang");
        assert_eq!(n, 0, "the 9th reporter must be closed promptly, not held open: {discard:?}");

        // Closing one of the eight frees a slot for a fresh reporter.
        conns.pop();
        let down = rx.recv_timeout(Duration::from_secs(5)).expect("link-down for the closed reporter never arrived");
        assert!(matches!(down, AppEvent::ExtLink(1, ref t, false) if t == "t"));

        let mut fresh = connect(&path);
        fresh.get_mut().write_all(line).unwrap();
        fresh.get_mut().write_all(b"\n").unwrap();
        let up = rx.recv_timeout(Duration::from_secs(2)).expect("a fresh reporter after a close must be admitted");
        assert!(matches!(up, AppEvent::ExtLink(1, ref t, true) if t == "t"), "the freed slot must be usable");

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// MC-overflow-no-strand: a connection already tracking
    /// `MAX_LINK_PANES_PER_CONN` distinct panes (its own, unrelated,
    /// per-CONNECTION cap) sends a further pane's line — [F2]'s existing
    /// behavior (the report still forwards, no link tracking) is unchanged,
    /// but the pinned addition is that NO reporter slot is consumed for it
    /// either: the reserve must never even be attempted on that overflow-skip
    /// path. Repeating well past `REPORTER_MAX_CONN_PER_PANE` must never
    /// erode the pane's real cap — a fresh, independent connection for it is
    /// still admitted afterward. *Reds if the overflow-skip path reserves
    /// (strands) a slot — the silent cap-erosion bug slot⟺entry pinning
    /// exists to prevent.*
    #[test]
    fn mc_overflow_no_strand_the_conn_cap_overflow_path_never_reserves_a_reporter_slot() {
        let (path, listener) = scratch_listener("mc-overflow-no-strand");
        let (tx, rx) = capturing_app();
        let extra_pane = MAX_LINK_PANES_PER_CONN as u64 + 1;
        let mut panes: Vec<(PaneId, &str)> =
            (1..=MAX_LINK_PANES_PER_CONN as u64).map(|p| (p, "t")).collect();
        panes.push((extra_pane, "t"));
        let limits = spawn_accept_loop(listener, tx, seeded_reader(&panes));

        // Fill this ONE connection's own MAX_LINK_PANES_PER_CONN cap first.
        let mut c = connect(&path);
        for pane in 1..=MAX_LINK_PANES_PER_CONN as u64 {
            c.get_mut()
                .write_all(format!(r#"{{"pane":{pane},"token":"t","event":"status","status":"working"}}"#).as_bytes())
                .unwrap();
            c.get_mut().write_all(b"\n").unwrap();
        }
        for _ in 0..(MAX_LINK_PANES_PER_CONN * 2) {
            rx.recv_timeout(Duration::from_secs(2)).expect("setup: link-up/status never arrived");
        }

        // Send the same OVERFLOW pane's line repeatedly, well past
        // REPORTER_MAX_CONN_PER_PANE attempts on this one connection alone —
        // if the overflow path reserved even once per call, this would
        // already have exhausted (and kept exhausting) pane P's real cap.
        for _ in 0..(REPORTER_MAX_CONN_PER_PANE * 2) {
            c.get_mut()
                .write_all(format!(r#"{{"pane":{extra_pane},"token":"t","event":"status","status":"working"}}"#).as_bytes())
                .unwrap();
            c.get_mut().write_all(b"\n").unwrap();
            let ev = rx.recv_timeout(Duration::from_secs(2)).expect("the overflow pane's report must still forward");
            assert!(matches!(ev, AppEvent::Status(p, ..) if p == extra_pane));
        }
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "past MAX_LINK_PANES_PER_CONN, no link tracking must ever happen — no link-up, ever"
        );

        // Direct proof: `Limits.reporters` has no entry for the overflow
        // pane at all — not even a lingering zero-then-removed one.
        assert!(
            !limits.reporters.lock().unwrap().contains_key(&extra_pane),
            "the overflow path must never reserve a slot for the pane it skips tracking"
        );

        // The property that actually matters operationally: a FRESH,
        // independent connection for that same pane is still admitted.
        let mut fresh = connect(&path);
        fresh
            .get_mut()
            .write_all(format!(r#"{{"pane":{extra_pane},"token":"t","event":"status","status":"working"}}"#).as_bytes())
            .unwrap();
        fresh.get_mut().write_all(b"\n").unwrap();
        let up = rx.recv_timeout(Duration::from_secs(2)).expect("a fresh connection for the overflow pane must still be admitted");
        assert!(matches!(up, AppEvent::ExtLink(p, ref t, true) if p == extra_pane && t == "t"));

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// No-false-rejection, nested claude inside a pi pane: the pane's own
    /// long-lived pi connection stays open throughout while a one-shot
    /// claude-hook-shaped connection (same pane, same token — both inherit
    /// the one `ROOST_TOKEN`) reports and disconnects alongside it. Both are
    /// admitted (well under `REPORTER_MAX_CONN_PER_PANE`), each gets its own
    /// independent link-up, and the one-shot's own close flicks only ITS
    /// link back down — the live pi connection is untouched. sock.rs's half
    /// of the property; the refcount arithmetic itself
    /// (`ext_link_counts` 2 -> 1 -> keeps `ext_link` true) is app.rs's
    /// `two_overlapping_links_for_one_pane_the_first_to_close_leaves_it_live`
    /// / `a_one_shot_link_during_a_live_connection_never_reverts_it_down`
    /// (F1) — unchanged, still green.
    #[test]
    fn nested_claude_hook_alongside_a_live_pi_connection_are_both_admitted() {
        let (path, listener) = scratch_listener("nested-claude");
        let (tx, rx) = capturing_app();
        spawn_accept_loop(listener, tx, seeded_reader(&[(1, "t")]));

        // The pane's own long-lived pi extension connection.
        let mut pi = connect(&path);
        pi.get_mut()
            .write_all(br#"{"pane":1,"token":"t","event":"status","status":"working"}"#)
            .unwrap();
        pi.get_mut().write_all(b"\n").unwrap();
        let up1 = rx.recv_timeout(Duration::from_secs(2)).expect("pi's link-up never arrived");
        assert!(matches!(up1, AppEvent::ExtLink(1, ref t, true) if t == "t"));
        rx.recv_timeout(Duration::from_secs(2)).expect("pi's status report never arrived");

        // A nested claude's one-shot hook: connect, one line, disconnect —
        // exactly `status_hook`'s shape (cli.rs).
        let mut claude = UnixStream::connect(&path).expect("connect");
        claude
            .write_all(br#"{"pane":1,"token":"t","event":"status","status":"needs_input"}"#)
            .unwrap();
        claude.write_all(b"\n").unwrap();
        drop(claude);
        let up2 = rx.recv_timeout(Duration::from_secs(2)).expect("claude's link-up never arrived");
        assert!(
            matches!(up2, AppEvent::ExtLink(1, ref t, true) if t == "t"),
            "the nested one-shot must ALSO be admitted — 2 reporters is well under the cap of 8"
        );
        rx.recv_timeout(Duration::from_secs(2)).expect("claude's status report never arrived");
        let down1 = rx.recv_timeout(Duration::from_secs(2)).expect("claude's own link-down never arrived");
        assert!(matches!(down1, AppEvent::ExtLink(1, ref t, false) if t == "t"));

        // The live pi connection is completely unaffected by the nested
        // one-shot's close.
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "the live pi connection's link must not be touched by the nested one-shot closing"
        );
        drop(pi);
        let down2 = rx.recv_timeout(Duration::from_secs(2)).expect("pi's own eventual close must still emit its link-down");
        assert!(matches!(down2, AppEvent::ExtLink(1, ref t, false) if t == "t"));

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    // --- §5.6 / M3: per-principal connection cap ---------------------------
    //
    // These drive the real `spawn_accept_loop` over a scratch socket (never
    // the process-global `ROOST_STATE` path — racy across tests running in
    // parallel in one process) against a fake "App" that acks every command
    // instantly, so the caps under test are sock.rs's alone.

    /// A scratch socket path + bound listener, unique per test.
    fn scratch_listener(tag: &str) -> (PathBuf, UnixListener) {
        let dir = std::env::temp_dir().join(format!("roost-sock-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.sock");
        let listener = UnixListener::bind(&path).expect("bind scratch socket");
        (path, listener)
    }

    /// Stands in for the main loop: replies `ok` to every `Command`
    /// immediately, mirroring a trivial verb like `list` (the one M2's flood
    /// used) — sock.rs's caps must not depend on what App does with a
    /// request, only on how many/how fast they arrive.
    fn fake_app() -> SyncSender<AppEvent> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<AppEvent>(256);
        std::thread::spawn(move || {
            while let Ok(ev) = rx.recv() {
                if let AppEvent::Command(_req, reply) = ev {
                    let _ = reply.send(Reply::ok(serde_json::json!({})));
                }
            }
        });
        tx
    }

    /// Like `fake_app`, but also forwards every non-`Command` event (D2's
    /// `ExtLink`, `Status`, `Session`, …) onto a second channel the test can
    /// inspect — `fake_app` itself silently drops those, which is fine for
    /// the connection-cap/rate-limit tests above but not for D2's link
    /// tests below, which assert on exactly what reaches "the main loop".
    fn capturing_app() -> (SyncSender<AppEvent>, std::sync::mpsc::Receiver<AppEvent>) {
        let (tx, rx) = std::sync::mpsc::sync_channel::<AppEvent>(256);
        let (cap_tx, cap_rx) = std::sync::mpsc::channel::<AppEvent>();
        std::thread::spawn(move || {
            while let Ok(ev) = rx.recv() {
                match ev {
                    AppEvent::Command(_req, reply) => {
                        let _ = reply.send(Reply::ok(serde_json::json!({})));
                    }
                    other => {
                        let _ = cap_tx.send(other);
                    }
                }
            }
        });
        (tx, cap_rx)
    }

    /// promotion-auth-gate: a `TokenReader` for `spawn_accept_loop`'s door,
    /// seeded with `pane_tokens` (pane id -> that pane's real token) — what
    /// every test below that wants a request/line to actually authenticate
    /// now has to build instead of handing the door an arbitrary string.
    /// `is_principal` accepts the fleet token *or* any pane's, so a test
    /// exercising only the control-request door can seed any pane id it
    /// likes; a status/session test must match the exact pane id its line
    /// claims. Deliberately NOT used to seed a flood/squat test's attacking
    /// connections — staying unauthenticated is the property those tests
    /// assert; only their fresh-caller probe needs a seeded token.
    fn seeded_reader(pane_tokens: &[(PaneId, &str)]) -> TokenReader {
        let mut table = TokenTable::new().expect("urandom must be available in tests");
        for &(id, tok) in pane_tokens {
            table.set_pane_token(id, tok.to_string());
        }
        table.reader()
    }

    /// Connect with a client-side read timeout — a real hang (the old P0 bug
    /// class) fails the test fast instead of wedging `cargo test`.
    fn connect(path: &Path) -> BufReader<UnixStream> {
        let stream = UnixStream::connect(path).expect("connect to scratch socket");
        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        BufReader::new(stream)
    }

    /// Send one line, read exactly one reply line back.
    fn roundtrip(r: &mut BufReader<UnixStream>, line: &str) -> serde_json::Value {
        r.get_mut().write_all(line.as_bytes()).unwrap();
        r.get_mut().write_all(b"\n").unwrap();
        let mut resp = String::new();
        r.read_line(&mut resp).expect("no reply within the client timeout — dropped or hung");
        serde_json::from_str(&resp).unwrap_or_else(|e| panic!("reply {resp:?} not JSON: {e}"))
    }

    /// Poll until `limits.global` reads at least `want`, so a test that
    /// fills the connection cap doesn't race the accept loop: each
    /// squatter's `connect()` returning on the test side doesn't mean the
    /// server has accepted *and counted* it yet — that happens on a
    /// separately spawned thread per connection.
    fn poll_until_admitted(limits: &Limits, want: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while limits.global.load(Ordering::Relaxed) < want {
            assert!(Instant::now() < deadline, "accept loop never reached {want} admitted connections");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Like `roundtrip`, but retries the connect-and-request from scratch
    /// until it succeeds or `deadline` passes (panicking then, same as
    /// `roundtrip`'s `.expect`), instead of panicking on the very first
    /// failure. A caller landing exactly while the global cap is full (the
    /// scenario the tests below deliberately create) is *expected* to be
    /// shed at least once by `try_reserve_global` — the property under test
    /// is that a slot frees up and a retry succeeds well within `deadline`,
    /// not that the very first attempt does. Every fallible step here
    /// (including `set_read_timeout`, which can transiently error under a
    /// full-suite run's fd/thread churn — unrelated to the property under
    /// test) is tolerated the same way: fall through to the retry rather
    /// than panic.
    fn try_request(path: &Path, line: &str, deadline: Instant) -> serde_json::Value {
        loop {
            if let Ok(stream) = UnixStream::connect(path) {
                if stream.set_read_timeout(Some(Duration::from_secs(1))).is_ok() {
                    let mut r = BufReader::new(stream);
                    let sent =
                        r.get_mut().write_all(line.as_bytes()).and_then(|_| r.get_mut().write_all(b"\n"));
                    if sent.is_ok() {
                        let mut resp = String::new();
                        if r.read_line(&mut resp).is_ok() && !resp.is_empty() {
                            if let Ok(v) = serde_json::from_str(&resp) {
                                return v;
                            }
                        }
                    }
                }
            }
            assert!(Instant::now() < deadline, "fresh caller never got a reply before the deadline — locked out");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Spawn `n` connections that each write one byte every
    /// `PRE_AUTH_READ_TIMEOUT / 2` forever, never a newline — the actual C1
    /// exploit shape: every individual `read()` the server does completes
    /// well inside its own `SO_RCVTIMEO`, but the connection as a whole
    /// never completes a line. Each thread exits on its first write error,
    /// which happens naturally once the server recycles it.
    fn spawn_drippers(path: &Path, n: usize) {
        for _ in 0..n {
            let path = path.to_path_buf();
            std::thread::spawn(move || {
                let Ok(mut stream) = UnixStream::connect(&path) else { return };
                loop {
                    std::thread::sleep(PRE_AUTH_READ_TIMEOUT / 2);
                    if stream.write_all(b"x").is_err() {
                        return;
                    }
                }
            });
        }
    }

    #[test]
    fn one_principal_cannot_exceed_its_connection_cap_while_another_still_connects() {
        let (path, listener) = scratch_listener("conncap");
        spawn_accept_loop(listener, fake_app(), seeded_reader(&[(1, "tok-a"), (2, "tok-b")]));

        // Principal A opens exactly its cap's worth of connections, kept
        // open (held in `_a_conns`) so they still count.
        let mut _a_conns = Vec::new();
        for i in 0..PRINCIPAL_MAX_CONN {
            let mut c = connect(&path);
            let reply = roundtrip(&mut c, r#"{"token":"tok-a","method":"list"}"#);
            assert!(reply.get("ok").is_some(), "connection {i} under cap must succeed: {reply}");
            _a_conns.push(c);
        }

        // One more from the SAME token: refused with a clear, actionable
        // reply — not a silent drop, not a hang (the `connect`/`roundtrip`
        // timeout would catch either).
        let mut over = connect(&path);
        let reply = roundtrip(&mut over, r#"{"token":"tok-a","method":"list"}"#);
        let err = reply.get("err").and_then(|v| v.as_str()).expect("over cap must error, not ok");
        assert!(err.contains("connection limit"), "must name the connection cap: {err}");

        // A different principal is unaffected — the whole point of the cap.
        let mut b = connect(&path);
        let reply = roundtrip(&mut b, r#"{"token":"tok-b","method":"list"}"#);
        assert!(reply.get("ok").is_some(), "a different principal must still connect: {reply}");

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    // --- audit finding C1: pre-auth admission is time-bounded, not pooled --
    //
    // A second review pass found the original H1 fix (a small *shared*
    // pre-auth pool) was a *cheaper* lockout than the bug it replaced — see
    // the `Limits` doc comment. These assert the property that fix was
    // supposed to provide: squatters can never lock out someone else. Both
    // tests fill the *actual* `MAX_CONN` (a prior version of the idle test
    // only opened half of it, so the global cap was never full and the test
    // could not have caught either bug below).

    #[test]
    fn squatting_connections_cannot_lock_out_a_fresh_caller() {
        let (path, listener) = scratch_listener("squat");
        // promotion-auth-gate: the squatters below send nothing at all, so
        // whether they'd authenticate is moot — only the fresh-caller probe
        // needs a seeded, valid token; this is now the *stronger* property
        // the design calls for: an authenticated newcomer still gets through
        // while the pool is squatted by an unauthenticated flood.
        let limits = spawn_accept_loop(listener, fake_app(), seeded_reader(&[(1, "newcomer")]));

        // A full MAX_CONN worth of connections that never send anything —
        // M3's original attack shape. There is no shared pre-auth counter
        // for these to fill any more.
        let mut silent = Vec::new();
        for _ in 0..MAX_CONN {
            silent.push(connect(&path));
        }
        poll_until_admitted(&limits, MAX_CONN);

        // A fresh, well-formed, AUTHENTICATED caller must still connect and
        // get a reply — squatters recycle on the pre-auth deadline rather
        // than holding the cap forever, so this must succeed well within a
        // couple of `PRE_AUTH_READ_TIMEOUT` cycles, not eventually.
        let deadline = Instant::now() + Duration::from_secs(10);
        let reply = try_request(&path, r#"{"token":"newcomer","method":"list"}"#, deadline);
        assert!(reply.get("ok").is_some(), "squatters must never lock out a fresh caller: {reply}");

        drop(silent);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// The actual C1 (second-pass) exploit shape: idle connections above
    /// already trip `SO_RCVTIMEO` on their own and were never the gap — a
    /// connection that drip-feeds one byte slower than a full line but
    /// faster than the per-read timeout never triggers a read error at all,
    /// so `read_until` (pre-fix) just kept accumulating forever: charged
    /// nothing, never promoted, never timed out. This fills `MAX_CONN` with
    /// exactly that and asserts the wall-clock deadline (not the per-read
    /// timeout) still recycles them.
    #[test]
    fn dripping_connections_cannot_lock_out_a_fresh_caller() {
        let (path, listener) = scratch_listener("drip");
        // Same split as the squat test above: drippers never complete a
        // line, so they never reach the door either way — only the probe
        // needs a seeded, valid token.
        let limits = spawn_accept_loop(listener, fake_app(), seeded_reader(&[(1, "newcomer")]));

        spawn_drippers(&path, MAX_CONN);
        poll_until_admitted(&limits, MAX_CONN);

        let deadline = Instant::now() + Duration::from_secs(10);
        let reply = try_request(&path, r#"{"token":"newcomer","method":"list"}"#, deadline);
        assert!(reply.get("ok").is_some(), "a drip flood must never lock out a fresh caller: {reply}");

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// Like `fake_app`, but its reply is far larger than a local socket's
    /// buffers (macOS defaults to 8 KiB each way) — enough that handing one
    /// back to a client that never reads parks the connection thread in
    /// `write_all`. That's `roost read` of a real scrollback, not a synthetic
    /// size: the point is a reply that cannot be dumped into the peer's
    /// buffer and forgotten.
    fn fat_app() -> SyncSender<AppEvent> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<AppEvent>(256);
        std::thread::spawn(move || {
            let blob = "x".repeat(256 * 1024);
            while let Ok(ev) = rx.recv() {
                if let AppEvent::Command(_req, reply) = ev {
                    let _ = reply.send(Reply::ok(serde_json::json!({ "out": blob })));
                }
            }
        });
        tx
    }

    /// The write-side twin of the two floods above, and the one shape that
    /// was a permanent black hole rather than a recoverable shed: a client
    /// that asks for something big and then stops reading. Both read
    /// deadlines only ever fire on a thread sitting in `read()`, so a
    /// connection parked in `write_all` was reached by neither and held its
    /// global slot for as long as the peer stayed alive. `MAX_CONN` of them
    /// (trivially: one `roost read` per pane from a client that stopped
    /// reading, or any pipelining caller) therefore locked the control plane
    /// out permanently — the accept loop shedding every new connection with
    /// nothing left that could ever free a slot.
    ///
    /// Asserts on the slot itself rather than staging the full lockout: it is
    /// the same defect 64 times over, and one connection keeps this test off
    /// the fd/thread contention that a `MAX_CONN` version of it collides with
    /// under a full parallel suite run. Without `WRITE_TIMEOUT` the count
    /// never returns to zero.
    ///
    /// One request (not a pipelined burst) keeps the test's own `write_all`
    /// far inside the *server's* receive buffer: a client flooding requests
    /// while the server has stopped reading would deadlock the test itself
    /// rather than assert anything.
    #[test]
    fn a_connection_whose_peer_never_reads_the_reply_still_frees_its_slot() {
        let (path, listener) = scratch_listener("noread");
        let limits = spawn_accept_loop(listener, fat_app(), seeded_reader(&[(1, "noread")]));

        let mut stuck = UnixStream::connect(&path).expect("connect to scratch socket");
        stuck.write_all(br#"{"token":"noread","method":"list"}"#).unwrap();
        stuck.write_all(b"\n").unwrap();
        // ...and never read a byte of the reply: the server is now blocked in
        // `write_all` with a payload far larger than either socket buffer.
        poll_until_admitted(&limits, 1);

        // Doubled to absorb scheduling under a loaded parallel run, and no
        // more: the bound is wall-clock and payload-independent, so anything
        // beyond it is the bug, not a slow host. (With `SO_SNDTIMEO` alone it
        // was proportional to the reply size — 10s here alone, past 30s under
        // the full suite — which is why that isn't the fix.)
        let deadline = Instant::now() + WRITE_TIMEOUT * 2;
        while limits.global.load(Ordering::Relaxed) > 0 {
            assert!(
                Instant::now() < deadline,
                "a peer that never reads its reply must not hold a connection slot indefinitely"
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        drop(stuck);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// The write-side `dripping_connections_...`: a peer that *does* read,
    /// just slower than the reply is long. `SO_SNDTIMEO` alone can never
    /// catch this — every individual `write()` makes progress and completes
    /// well inside its own window, so `write_all` keeps looping and the bound
    /// becomes "payload size ÷ drip rate", i.e. unbounded from an attacker's
    /// side and merely unpredictable from a test's. Exactly the C1 lesson
    /// pointed the other way, which is why `write_reply` enforces an
    /// `Instant` deadline of its own and this asserts the same fixed bound as
    /// the not-reading-at-all case above.
    #[test]
    fn a_peer_that_drip_reads_its_reply_cannot_stretch_the_write_bound() {
        let (path, listener) = scratch_listener("dripread");
        let limits = spawn_accept_loop(listener, fat_app(), seeded_reader(&[(1, "dripread")]));

        let mut stuck = UnixStream::connect(&path).expect("connect to scratch socket");
        stuck.write_all(br#"{"token":"dripread","method":"list"}"#).unwrap();
        stuck.write_all(b"\n").unwrap();
        poll_until_admitted(&limits, 1);
        // One byte per WRITE_TICK-ish, forever: enough that the server's
        // writes keep succeeding, nowhere near enough to finish the reply.
        std::thread::spawn(move || {
            let mut byte = [0u8; 1];
            loop {
                std::thread::sleep(WRITE_TICK);
                if !matches!(stuck.read(&mut byte), Ok(1)) {
                    return; // server gave up and closed, as it must
                }
            }
        });

        let deadline = Instant::now() + WRITE_TIMEOUT * 2;
        while limits.global.load(Ordering::Relaxed) > 0 {
            assert!(
                Instant::now() < deadline,
                "a drip-reading peer must not stretch one reply past WRITE_TIMEOUT"
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn an_idle_pre_auth_connection_is_recycled_well_under_read_timeout() {
        let (path, listener) = scratch_listener("recycle");
        // This connection never sends a line at all — genuinely unaffected
        // by the door; no token needs seeding.
        spawn_accept_loop(listener, fake_app(), seeded_reader(&[]));

        let mut squatter = connect(&path);
        // Override the shared helper's 2s client timeout — it must not race
        // the server's own (also 2s) PRE_AUTH_READ_TIMEOUT.
        squatter.get_ref().set_read_timeout(Some(PRE_AUTH_READ_TIMEOUT + Duration::from_secs(3))).unwrap();
        let mut discard = String::new();
        let n = squatter.read_line(&mut discard).expect("must recycle, not hang for the full READ_TIMEOUT");
        assert_eq!(n, 0, "an idle pre-auth connection must eventually be closed: {discard:?}");

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// P1's "does this reopen anything" question, half one: a connection
    /// that DID authenticate (sent a well-formed control request, so
    /// `guard.principal` is set) must still be reaped after `READ_TIMEOUT`
    /// of silence — the P0 wedge this constant exists to prevent. The new
    /// retry clause is gated on `guard.principal.is_none()` specifically so
    /// it never reaches this case; this pins that it doesn't. Mirrors
    /// `an_idle_pre_auth_connection_is_recycled_well_under_read_timeout`
    /// above, but for the promoted/authenticated side, at `READ_TIMEOUT`'s
    /// own scale rather than the pre-auth one.
    #[test]
    fn an_authenticated_connection_gone_silent_is_still_reaped_after_read_timeout() {
        let (path, listener) = scratch_listener("control-still-reaped");
        spawn_accept_loop(listener, fake_app(), seeded_reader(&[(1, "t")]));
        let mut c = connect(&path);
        c.get_ref().set_read_timeout(Some(READ_TIMEOUT + Duration::from_secs(10))).unwrap();

        let reply = roundtrip(&mut c, r#"{"token":"t","method":"list"}"#);
        assert!(reply.get("ok").is_some(), "setup: the control request should succeed");

        let mut discard = String::new();
        let n = c.read_line(&mut discard).expect("must be reaped, not hang forever");
        assert_eq!(
            n, 0,
            "an authenticated connection gone silent must still be closed after READ_TIMEOUT: {discard:?}"
        );

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// A connection that only ever sends status/session lines (no `method`
    /// key, so never a control `Request`) must not be treated as forever
    /// pre-auth: `guard.principal` never gets set for these (they aren't a
    /// "principal" for connection-cap/rate-limit purposes), so gating
    /// promotion on that alone left every such connection permanently
    /// pre-auth, killed the first time it went quiet past
    /// `PRE_AUTH_READ_TIMEOUT`.
    ///
    /// This only proves promotion itself happens — it sleeps 2.5s, just past
    /// that 2s deadline, deliberately not the larger gap that actually
    /// matters in practice.
    /// `a_status_only_connection_survives_an_idle_gap_past_read_timeout`
    /// below is the real pi-extension shape (`extensions/roost.ts`: dials
    /// once, holds the socket for the pane's whole process lifetime, no
    /// keepalive, no reconnect) at the timescale that matters —
    /// `READ_TIMEOUT` (30s), not `PRE_AUTH_READ_TIMEOUT` (2s), since a
    /// *promoted* connection is bound by the former, not the latter (P1).
    #[test]
    fn a_status_only_connection_survives_past_the_pre_auth_deadline() {
        let (path, listener) = scratch_listener("status-survives");
        spawn_accept_loop(listener, fake_app(), seeded_reader(&[(1, "t")]));
        let mut c = connect(&path);

        // One status line — exactly what the extension sends. No `method`
        // key, so `parse_control` returns `None` and this never touches
        // `guard.principal`.
        c.get_mut().write_all(br#"{"pane":"1","token":"t","event":"status","status":"working"}"#).unwrap();
        c.get_mut().write_all(b"\n").unwrap();

        // Idle well past the pre-auth deadline, sending nothing — the real
        // gap between an agent's status transitions.
        std::thread::sleep(PRE_AUTH_READ_TIMEOUT + Duration::from_millis(500));

        // Still alive and still being served: a request on the same
        // connection must get a normal reply, not a hang or a dropped
        // connection (the bug: the server would already have closed its end
        // by now, so this would come back as EOF, and `roundtrip` panics).
        let reply = roundtrip(&mut c, r#"{"token":"t","method":"list"}"#);
        assert!(reply.get("ok").is_some(), "a status-only connection must survive past the pre-auth deadline: {reply}");

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// P1, the real bug (the finding the test above's doc points to): a
    /// promoted status-only connection must survive an idle gap past
    /// `READ_TIMEOUT` (30s) itself, not merely past the much smaller
    /// pre-auth deadline. Before this fix: a pi pane finishes a turn
    /// (`agent_settled` → `waiting`), the human reads the output for more than
    /// 30s — the *normal* operating condition, not an edge case — the
    /// server silently closed the connection (`Err(_) => break` on the
    /// `READ_TIMEOUT` firing), and the extension's next write EPIPEs and
    /// nulls its socket handle for good (`sock.on("error", () => sock =
    /// null)`, no reconnect anywhere in it): every later status/session
    /// report for that pane is dropped for the rest of its life. Sleeps past
    /// the real constant on purpose — this is the timescale that matters,
    /// not a shortened stand-in for it.
    ///
    /// D2 rides the same 30s wait to pin its own requirement for free rather
    /// than pay for a second sleep elsewhere: the retry that lets this
    /// connection survive must be a `continue`, never a `break` that
    /// `ConnGuard::drop` would see — so no link-down for pane 1 may appear
    /// at any point across that whole idle gap, only the link-up the initial
    /// status line earns.
    /// MC-survives-silence: the E2E-shaped proof that a VALID reporter
    /// outlives 30s+ of silence — reds if promotion or the P1 retry clause
    /// breaks for authenticated reporters (e.g. the door check moved to the
    /// wrong side of the `promoted`/retry logic, or pane 1's seeded token
    /// stops matching).
    #[test]
    fn a_status_only_connection_survives_an_idle_gap_past_read_timeout() {
        let (path, listener) = scratch_listener("status-survives-read-timeout");
        let (tx, rx) = capturing_app();
        spawn_accept_loop(listener, tx, seeded_reader(&[(1, "t")]));
        let mut c = connect(&path);

        c.get_mut()
            .write_all(br#"{"pane":"1","token":"t","event":"status","status":"working"}"#)
            .unwrap();
        c.get_mut().write_all(b"\n").unwrap();

        // Setup: the one line above earns exactly one link-up and one status
        // report, and nothing past that — drained before the idle gap so
        // they can't be mistaken for a spurious link-down during it.
        let mut got_link_up = false;
        for _ in 0..2 {
            match rx
                .recv_timeout(Duration::from_secs(2))
                .expect("setup: link-up/status never arrived")
            {
                AppEvent::ExtLink(1, ref t, true) if t == "t" => got_link_up = true,
                AppEvent::Status(1, ref t, AgentStatus::Working, _) if t == "t" => {}
                _ => panic!("unexpected setup event (wanted exactly one link-up and one status)"),
            }
        }
        assert!(
            got_link_up,
            "setup: the status line must have earned a link-up"
        );

        // The gap size the P1 finding is actually about — past READ_TIMEOUT
        // itself, not just the pre-auth deadline. The retry clause that
        // keeps this connection alive across it must never fire a link-down
        // (D2): confirmed by there being nothing at all to receive here.
        std::thread::sleep(READ_TIMEOUT + Duration::from_secs(1));
        assert!(
            rx.try_recv().is_err(),
            "the READ_TIMEOUT retry must not emit a link-down — the connection never really closed"
        );

        let reply = roundtrip(&mut c, r#"{"token":"t","method":"list"}"#);
        assert!(
            reply.get("ok").is_some(),
            "a status-only connection must survive an idle gap past READ_TIMEOUT: {reply}"
        );

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    // --- D2: per-connection pane liveness (link-up/link-down) --------------
    //
    // The pi extension holds one long-lived connection per pane; whether
    // that connection is currently open is itself a liveness signal
    // `StatusTracker` uses to decide how much to trust a resting/Working
    // report over PTY output (see core::status). These tests drive that
    // accounting through the real `spawn_accept_loop`, same harness as
    // above, but via `capturing_app` so the `AppEvent::ExtLink` events
    // themselves (not just control replies) are observable.

    /// The first accepted status line for a pane on a connection associates
    /// that connection with the pane and emits link-up — *before* the status
    /// event itself, so a consumer sees "the link is live" and then "here's
    /// what it reported", never the other way around.
    #[test]
    fn accepted_status_line_associates_conn_to_pane_and_emits_link_up() {
        let (path, listener) = scratch_listener("link-up");
        let (tx, rx) = capturing_app();
        spawn_accept_loop(listener, tx, seeded_reader(&[(1, "tok")]));
        let mut c = connect(&path);
        c.get_mut()
            .write_all(br#"{"pane":"1","token":"tok","event":"status","status":"working"}"#)
            .unwrap();
        c.get_mut().write_all(b"\n").unwrap();

        let first = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("no event within the deadline");
        assert!(
            matches!(first, AppEvent::ExtLink(1, ref t, true) if t == "tok"),
            "expected link-up first"
        );
        let second = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("no second event within the deadline");
        assert!(
            matches!(second, AppEvent::Status(1, ref t, AgentStatus::Working, _) if t == "tok"),
            "expected the status report itself second"
        );

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// A connection's close (EOF) emits link-down for exactly the pane(s) it
    /// reported for — never a different pane's, even one reporting over a
    /// separate connection that stays open.
    #[test]
    fn connection_eof_emits_link_down_for_that_pane_only() {
        let (path, listener) = scratch_listener("link-down-isolated");
        let (tx, rx) = capturing_app();
        spawn_accept_loop(listener, tx, seeded_reader(&[(1, "tok-a"), (2, "tok-b")]));

        let mut a = connect(&path);
        a.get_mut()
            .write_all(br#"{"pane":"1","token":"tok-a","event":"status","status":"working"}"#)
            .unwrap();
        a.get_mut().write_all(b"\n").unwrap();
        let mut b = connect(&path);
        b.get_mut()
            .write_all(br#"{"pane":"2","token":"tok-b","event":"status","status":"working"}"#)
            .unwrap();
        b.get_mut().write_all(b"\n").unwrap();

        // Drain the setup: one link-up + one status per connection, in
        // whatever order the two threads happen to land in — only the
        // count is deterministic here, so wait for exactly that many before
        // touching either connection further.
        for _ in 0..4 {
            rx.recv_timeout(Duration::from_secs(2))
                .expect("setup: link-up/status never arrived");
        }

        drop(a); // pane 1's connection closes; pane 2's (`b`) stays open

        let ev = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("link-down for pane 1 never arrived");
        assert!(
            matches!(ev, AppEvent::ExtLink(1, ref t, false) if t == "tok-a"),
            "expected link-down for pane 1"
        );

        // Nothing else shows up — in particular no link-down for pane 2,
        // whose connection is untouched.
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "pane 2's link must not be touched by pane 1's connection closing"
        );

        drop(b);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// A line that never resolves to a pane at all (garbage JSON, an
    /// unparseable pane id — anything `parse_line` returns `None` for, see
    /// `ignores_garbage`) never gets associated with anything at the sock.rs
    /// layer in the first place, so its connection's close has nothing to
    /// report: no link-down (or link-up) for any pane.
    ///
    /// This is the sock.rs-native half of "an unauthenticated/garbage-token
    /// line's close must not emit a link-down for the pane it claimed": this
    /// file has no way to judge a *token*'s legitimacy (only App does, via
    /// `socket_authorized` — see the `Limits` doc comment on why that's
    /// deliberate), so the other, already-tested half of that property is
    /// `socket_authorized` itself gating `on_status_link` in main.rs exactly
    /// like it gates `on_status`/`on_session`.
    #[test]
    fn a_line_that_never_resolves_to_a_pane_emits_no_link_event_on_close() {
        let (path, listener) = scratch_listener("garbage-link");
        let (tx, rx) = capturing_app();
        // `parse_line` returns `None` before the door is ever consulted
        // (the pane id itself fails to parse) — genuinely unaffected; no
        // token needs seeding.
        spawn_accept_loop(listener, tx, seeded_reader(&[]));
        let mut c = connect(&path);
        c.get_mut()
            .write_all(br#"{"pane":"not-a-number","event":"status","status":"working"}"#)
            .unwrap();
        c.get_mut().write_all(b"\n").unwrap();
        // Give the server a moment to (fail to) process the line before the
        // close under test.
        std::thread::sleep(Duration::from_millis(200));
        drop(c);

        assert!(
            rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "a line that never parsed to a pane must never produce a link event, up or down"
        );

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// The Claude Code hooks shape (`cli.rs`'s `__status`, one connection per
    /// report — see `status_hook`/`write_line`): connect, send exactly one
    /// status line, disconnect. Two of those back to back must each flick
    /// the pane's link up then immediately back down, leaving it exactly
    /// where it always ends up — down — so `current()`'s D1 gate falls back
    /// to output promotion for Claude precisely as it did before D2 existed.
    #[test]
    fn claude_one_shot_connections_flick_the_link_up_then_back_down() {
        let (path, listener) = scratch_listener("claude-one-shot");
        let (tx, rx) = capturing_app();
        spawn_accept_loop(listener, tx, seeded_reader(&[(1, "t")]));

        // Fully process one one-shot connection (link-up, its status report,
        // then link-down on EOF) before starting the next. Those three
        // arrive in exactly that order because they're all sent from that
        // one connection's own thread — but *across* two separate
        // connections' threads there is no such guarantee, so this
        // synchronizes on each round's link-down before moving on rather
        // than assuming a cross-connection interleaving.
        for round in 0..2 {
            let mut c = UnixStream::connect(&path).expect("connect");
            c.write_all(br#"{"pane":"1","token":"t","event":"status","status":"working"}"#)
                .unwrap();
            c.write_all(b"\n").unwrap();
            // Exactly `status_hook`'s shape: fire-and-forget, dropped the
            // instant the write is queued, no reply ever read.
            drop(c);

            let up = rx
                .recv_timeout(Duration::from_secs(2))
                .expect("link-up never arrived");
            assert!(
                matches!(up, AppEvent::ExtLink(1, ref t, true) if t == "t"),
                "round {round}: expected link-up"
            );
            let status = rx
                .recv_timeout(Duration::from_secs(2))
                .expect("status report never arrived");
            assert!(
                matches!(status, AppEvent::Status(1, ref t, AgentStatus::Working, _) if t == "t"),
                "round {round}: expected the status report"
            );
            let down = rx
                .recv_timeout(Duration::from_secs(2))
                .expect("link-down never arrived");
            assert!(
                matches!(down, AppEvent::ExtLink(1, ref t, false) if t == "t"),
                "round {round}: expected link-down right after the connection closed"
            );
        }

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// [F2] A connection claiming to report for more than
    /// `MAX_LINK_PANES_PER_CONN` distinct panes gets no further link
    /// tracking past the cap — no entry, no link-up. The underlying
    /// status report itself is unaffected (App's own token check judges
    /// it, not this cap), and closing the connection proves the extra pane
    /// really was never tracked: only the capped panes get a link-down.
    #[test]
    fn a_connections_link_tracking_is_capped_per_connection() {
        let (path, listener) = scratch_listener("link-cap");
        let (tx, rx) = capturing_app();
        // One seeded pane per id this test will claim, 1..=MAX_LINK_PANES_PER_CONN
        // plus the one past the cap — all sharing the literal token "t" is
        // fine here (this test is about DISTINCT-PANE tracking, not token
        // uniqueness).
        let panes: Vec<(PaneId, &str)> =
            (1..=MAX_LINK_PANES_PER_CONN as u64 + 1).map(|p| (p, "t")).collect();
        spawn_accept_loop(listener, tx, seeded_reader(&panes));
        let mut c = connect(&path);

        // Fill the cap: MAX_LINK_PANES_PER_CONN distinct panes, each earning
        // a link-up + its status report.
        for pane in 1..=MAX_LINK_PANES_PER_CONN as u64 {
            c.get_mut()
                .write_all(
                    format!(
                        r#"{{"pane":"{pane}","token":"t","event":"status","status":"working"}}"#
                    )
                    .as_bytes(),
                )
                .unwrap();
            c.get_mut().write_all(b"\n").unwrap();
        }
        for _ in 0..(MAX_LINK_PANES_PER_CONN * 2) {
            rx.recv_timeout(Duration::from_secs(2))
                .expect("setup: link-up/status never arrived");
        }

        // One more pane, past the cap: the underlying report still forwards...
        let extra_pane = MAX_LINK_PANES_PER_CONN as u64 + 1;
        c.get_mut()
            .write_all(
                format!(
                    r#"{{"pane":"{extra_pane}","token":"t","event":"status","status":"working"}}"#
                )
                .as_bytes(),
            )
            .unwrap();
        c.get_mut().write_all(b"\n").unwrap();
        let ev = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("the underlying status report must still forward past the cap");
        assert!(matches!(ev, AppEvent::Status(p, ..) if p == extra_pane));
        // ...but no link-up for it.
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "past the cap, a new pane must not get link tracking"
        );

        // Closing proves it was never inserted: exactly the capped panes'
        // link-downs arrive, never a 6th for the pane past the cap.
        drop(c);
        let mut downs = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        while downs.len() < MAX_LINK_PANES_PER_CONN && Instant::now() < deadline {
            if let Ok(AppEvent::ExtLink(p, _, false)) = rx.recv_timeout(Duration::from_millis(500))
            {
                downs.push(p);
            }
        }
        downs.sort();
        assert_eq!(
            downs,
            (1..=MAX_LINK_PANES_PER_CONN as u64).collect::<Vec<_>>()
        );
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "no link-down for the pane past the cap — it was never tracked"
        );

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// [F2] A status/session line with a token past `MAX_TOKEN_LEN` is
    /// dropped by `parse_line` itself, same as any other malformed line —
    /// it never reaches `link_panes`, and its underlying report never
    /// forwards either.
    #[test]
    fn an_oversized_token_status_line_is_dropped_before_touching_link_panes() {
        let (path, listener) = scratch_listener("link-token-cap");
        let (tx, rx) = capturing_app();
        // `parse_line` drops an oversized token itself, before the door —
        // genuinely unaffected; no token needs seeding.
        spawn_accept_loop(listener, tx, seeded_reader(&[]));
        let mut c = connect(&path);

        let long_token = "a".repeat(MAX_TOKEN_LEN + 1);
        c.get_mut()
            .write_all(
                format!(
                    r#"{{"pane":"1","token":"{long_token}","event":"status","status":"working"}}"#
                )
                .as_bytes(),
            )
            .unwrap();
        c.get_mut().write_all(b"\n").unwrap();

        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "an oversized-token status line must produce no event at all"
        );

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    // --- §5.6 / M3: token-bucket command rate limit ------------------------

    #[test]
    fn a_flood_is_throttled_not_dropped_or_hung() {
        let (path, listener) = scratch_listener("flood");
        spawn_accept_loop(listener, fake_app(), seeded_reader(&[(1, "flooder")]));
        let mut c = connect(&path);

        let total = PRINCIPAL_BUCKET_CAPACITY as usize + 20;
        let mut ok = 0;
        let mut throttled = 0;
        for _ in 0..total {
            let reply = roundtrip(&mut c, r#"{"token":"flooder","method":"list"}"#);
            match reply.get("err").and_then(|v| v.as_str()) {
                Some(err) => {
                    assert!(err.contains("rate limited"), "throttle reply must name itself: {err}");
                    throttled += 1;
                }
                None => {
                    assert!(reply.get("ok").is_some(), "unexpected reply shape: {reply}");
                    ok += 1;
                }
            }
        }
        assert_eq!(ok + throttled, total, "every one of {total} requests got exactly one reply");
        assert!(throttled > 0, "a flood past the bucket capacity must be throttled at least once");

        // Throttled, not banned: the connection is still alive and usable.
        let reply = roundtrip(&mut c, r#"{"token":"flooder","method":"list"}"#);
        assert!(reply.get("ok").is_some() || reply.get("err").is_some());

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// Audit finding M3: re-derived from a *real* 20-pane fleet (not the
    /// 10-pane hello-world DESIGN-control.md §7 uses for brevity) — spawn
    /// x20 then wait x20, 40 commands fired back-to-back. Regressing this
    /// (the burst getting throttled) would make the fix worse than the bug
    /// it closes.
    #[test]
    fn a_legitimate_bursty_sequence_is_not_throttled() {
        let (path, listener) = scratch_listener("burst");
        spawn_accept_loop(listener, fake_app(), seeded_reader(&[(1, "orch")]));
        let mut c = connect(&path);

        for i in 0..20 {
            let reply = roundtrip(&mut c, r#"{"token":"orch","method":"spawn","adapter":"shell"}"#);
            assert!(reply.get("ok").is_some(), "spawn #{i} of the 20-pane burst was throttled: {reply}");
        }
        for i in 0..20 {
            let reply = roundtrip(&mut c, r#"{"token":"orch","method":"list"}"#);
            assert!(reply.get("ok").is_some(), "op #{i} of the 20-pane burst was throttled: {reply}");
        }

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// Audit finding H2: varying the `token` field per request on one
    /// already-open connection must not mint an endless supply of fresh
    /// per-principal buckets — only the identity the connection was
    /// actually admitted under (its first request's token) is ever charged.
    ///
    /// promotion-auth-gate correction: an unauthenticated "rotated-N" body
    /// token would now be refused at the door on every single request
    /// (never reaching `take_principal` at all), which would make this test
    /// pass for the wrong reason — it wouldn't be proving H2's attribution
    /// property anymore, just that the door rejects junk. So the rotation is
    /// between exactly TWO real, seeded identities: "admitted" (this
    /// connection's real admission — the first request) and "rotated" (a
    /// different, equally valid principal). Both pass the door and reach
    /// dispatch; H2 is that only "admitted" is ever charged, never
    /// "rotated", regardless of which one a later request's body claims.
    #[test]
    fn rotating_the_bodys_token_does_not_mint_a_fresh_bucket_on_one_connection() {
        let (path, listener) = scratch_listener("rotate");
        let reader = seeded_reader(&[(1, "admitted"), (2, "rotated")]);
        let limits = spawn_accept_loop(listener, fake_app(), reader);
        let mut c = connect(&path);

        let total = PRINCIPAL_BUCKET_CAPACITY as usize + 20;
        let mut throttled = 0;
        for i in 0..total {
            let token = if i == 0 || i % 2 == 0 { "admitted" } else { "rotated" };
            let reply = roundtrip(&mut c, &format!(r#"{{"token":"{token}","method":"list"}}"#));
            if reply.get("err").and_then(|v| v.as_str()).is_some() {
                throttled += 1;
            }
        }
        assert!(throttled > 0, "varying the token per request must still get throttled eventually");

        // The only thing ever charged is the connection's real admission
        // identity — "rotated" is a genuinely valid principal (it passes
        // the door on its own) and still never mints its own bucket.
        let map = limits.buckets.lock().unwrap();
        assert!(map.contains_key("admitted"), "the connection's admitted identity should have a bucket");
        assert_eq!(
            map.len(),
            1,
            "an in-body token that never admitted this connection must not get its own bucket, \
             even though it's independently a valid principal: {:?}",
            map.keys().collect::<Vec<_>>()
        );

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    // --- audit finding C2: per-line cost is per-connection, not shared -----
    //
    // A second review pass found the original H1(a)/M2 fix (every line
    // charges the *shared* aggregate bucket) let one connection's blank-line
    // flood empty that bucket and `rate limited` every other connection —
    // cheaper than the M3 flood it replaced, and it serialized all control
    // traffic on the aggregate's mutex. These assert the property that fix
    // was supposed to provide: a flood on one connection can't deny another.

    #[test]
    fn a_line_flood_on_one_connection_cannot_deny_a_different_connection() {
        let (path, listener) = scratch_listener("lineflood");
        spawn_accept_loop(listener, fake_app(), seeded_reader(&[(1, "other")]));

        // Flood one connection with blank lines well past any reasonable
        // per-connection budget. Nothing here is control-shaped, so none of
        // it gets (or needs) a reply.
        let mut flooder = connect(&path);
        for _ in 0..(LINE_BUCKET_CAPACITY as usize * 3) {
            flooder.get_mut().write_all(b"\n").unwrap();
        }

        // A completely different connection must be unaffected: the
        // flood's cost lands on the flooder's own (per-connection,
        // unlocked) bucket, never on a resource a second connection
        // depends on.
        let mut other = connect(&path);
        let reply = roundtrip(&mut other, r#"{"token":"other","method":"list"}"#);
        assert!(reply.get("ok").is_some(), "a line flood on one connection must not throttle a different one: {reply}");

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// The flip side: removing the shared chokepoint didn't remove the
    /// limit, just relocated its cost. Uses requests missing a token (always
    /// "malformed", never reaching a per-principal bucket) so only the
    /// per-connection line bucket can be responsible for any throttling
    /// observed here.
    #[test]
    fn a_line_flood_still_throttles_its_own_connection_eventually() {
        let (path, listener) = scratch_listener("selfthrottle");
        // Every request here is missing its token field entirely — always a
        // `Some(Err(_))` parse failure, never reaching the door (or a
        // per-principal bucket); genuinely unaffected, no seeding needed.
        spawn_accept_loop(listener, fake_app(), seeded_reader(&[]));
        let mut c = connect(&path);

        let total = LINE_BUCKET_CAPACITY as usize + 20;
        let mut rate_limited = 0;
        for _ in 0..total {
            let reply = roundtrip(&mut c, r#"{"method":"list"}"#);
            let err = reply.get("err").and_then(|v| v.as_str()).unwrap_or_default();
            if err.contains("rate limited") {
                rate_limited += 1;
            }
        }
        assert!(rate_limited > 0, "a sustained flood must still throttle its own connection's line bucket eventually");

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn bucket_map_stays_bounded_and_never_evicts_an_actively_throttled_principal() {
        // Unit-level against `Limits` directly — deterministic and fast,
        // no need to round-trip real elapsed time through a socket.
        let limits = Limits::new();

        // "active" is genuinely being enforced: drain its bucket to exactly
        // its limit. This is the state that must survive eviction — if it's
        // evicted, the next call gets a *fresh* bucket and silently succeeds
        // instead of being throttled, handing the attacker a bypass (worse
        // than the memory leak this test is otherwise guarding against).
        for _ in 0..(PRINCIPAL_BUCKET_CAPACITY as usize) {
            assert!(limits.take_principal("active"));
        }
        assert!(!limits.take_principal("active"), "setup bug: active should be at its limit");

        // Rotate far more distinct, single-use identities than
        // MAX_TRACKED_TOKENS — exactly the "mint a fresh admitted identity"
        // evasion. Each is used once then abandoned, so it carries no
        // ongoing enforcement state (unlike "active", checked below).
        for i in 0..(MAX_TRACKED_TOKENS * 3) {
            limits.take_principal(&format!("flood-{i}"));
        }

        let map = limits.buckets.lock().unwrap();
        assert!(map.len() <= MAX_TRACKED_TOKENS, "bucket map grew past its bound: {}", map.len());
        let active = map.get("active").expect("an actively-throttled principal was evicted");
        assert!(active.tokens < 1.0, "active's drained state was reset by eviction: {} tokens", active.tokens);
    }

    #[test]
    fn throttle_and_cap_replies_are_distinguishable_from_unauthorized_and_malformed() {
        // promotion-auth-gate: `UNAUTHORIZED_MSG` is now sock.rs's OWN door
        // reply too, not just App's (both a `pub const` shared by the two,
        // precisely so they can't fork) — this pins that sock.rs's other
        // self-generated strings (malformed/cap/rate-limit) never collide
        // with it, using the real constant rather than a hand-typed copy.

        let (path, listener) = scratch_listener("distinguish");
        // Both identities under test here must be genuinely valid — this
        // test is about telling sock.rs's OWN cap/rate-limit replies apart
        // from the door's UNAUTHORIZED_MSG, which requires actually
        // reaching that logic (past the door) in the first place.
        spawn_accept_loop(listener, fake_app(), seeded_reader(&[(1, "cap-x"), (2, "rate-y")]));

        // Malformed: no token to even evaluate.
        let mut m = connect(&path);
        let reply = roundtrip(&mut m, r#"{"method":"list"}"#);
        let malformed = reply.get("err").and_then(|v| v.as_str()).expect("malformed must error").to_string();

        // Connection limit: fill one principal's cap, then go one over.
        let mut _held = Vec::new();
        for _ in 0..PRINCIPAL_MAX_CONN {
            let mut c = connect(&path);
            roundtrip(&mut c, r#"{"token":"cap-x","method":"list"}"#);
            _held.push(c);
        }
        let mut over = connect(&path);
        let reply = roundtrip(&mut over, r#"{"token":"cap-x","method":"list"}"#);
        let cap_err = reply.get("err").and_then(|v| v.as_str()).expect("over cap must error").to_string();

        // Rate limit: exhaust a fresh principal's bucket.
        let mut r = connect(&path);
        let mut throttled = None;
        for _ in 0..(PRINCIPAL_BUCKET_CAPACITY as usize + 5) {
            let reply = roundtrip(&mut r, r#"{"token":"rate-y","method":"list"}"#);
            if let Some(e) = reply.get("err").and_then(|v| v.as_str()) {
                throttled = Some(e.to_string());
                break;
            }
        }
        let throttled = throttled.expect("flood must throttle within the loop above");

        for (name, s) in [("malformed", &malformed), ("connection-limit", &cap_err), ("rate-limit", &throttled)] {
            assert!(!s.contains("unauthorized"), "{name} reply reads as unauthorized: {s}");
            assert_ne!(s.as_str(), UNAUTHORIZED_MSG, "{name} reply must not equal the shared unauthorized text");
        }
        assert_ne!(malformed, cap_err, "malformed and connection-limit must read differently");
        assert_ne!(malformed, throttled, "malformed and rate-limit must read differently");
        assert_ne!(cap_err, throttled, "connection-limit and rate-limit must read differently");
        assert!(cap_err.contains("connection limit"));
        assert!(throttled.contains("rate limited"));

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    // --- oversize-line follow-up: a true diagnosis, not a silent EPIPE -----
    //
    // Platform note (CI found this the hard way — PR #68, ubuntu-latest red,
    // macOS green): a local socket's kernel buffer is NOT the same size on
    // the two platforms roost ships for, and the gap is large. Measured
    // directly (Docker `rust:1-bookworm`, same reproduction used to debug
    // the CI failure) rather than assumed:
    //   - macOS: `net.local.stream.{recvspace,sendspace}` default to 8 KiB
    //     each way.
    //   - Linux: `net.core.{rmem,wmem}_default` default to 208 KiB, and a
    //     one-shot send/recv pair on top of that absorbed a payload up to
    //     ~290 KiB without the writer ever blocking (measured with a Python
    //     socket harness mirroring this exact accept/reply/drain shape).
    // `OVERSIZED_PAYLOAD` is chosen well past both, so a still-blocked
    // writer is what these tests exercise on *either* platform, not just
    // macOS's tighter one — not a bigger guess, a measured margin. That
    // said, every assertion below is about the observable contract (the
    // exact reply, the clean close, the instance staying alive) — none of
    // them assert anything about *whether* the write actually blocked, so
    // they hold even if some future platform's default buffer swallows this
    // whole.
    const OVERSIZED_PAYLOAD: usize = 1_000_000;

    /// `OVERSIZE_LINE_MSG` must actually name the cap `MAX_LINE` enforces —
    /// a guard against the two silently drifting apart if either is ever
    /// changed without the other.
    #[test]
    fn oversize_reply_names_the_actual_cap() {
        assert!(
            OVERSIZE_LINE_MSG.contains(&(MAX_LINE / 1024).to_string()),
            "OVERSIZE_LINE_MSG must name MAX_LINE's real KiB value: {OVERSIZE_LINE_MSG}"
        );
    }

    /// A line past `MAX_LINE` must get a true diagnosis, not the silent drop
    /// that used to EPIPE the client mid-write and print "cannot reach a
    /// running roost" — an alive instance misdiagnosed as unreachable.
    #[test]
    fn an_oversized_line_gets_a_true_diagnosis_reply_before_closing() {
        let (path, listener) = scratch_listener("oversize");
        // The oversize check runs before parse_control/parse_line even see
        // the line — genuinely unaffected; no token needs seeding.
        spawn_accept_loop(listener, fake_app(), seeded_reader(&[]));
        let mut c = connect(&path);

        let payload = "A".repeat(OVERSIZED_PAYLOAD);
        let reply = roundtrip(&mut c, &payload);
        let err = reply.get("err").and_then(|v| v.as_str()).expect("oversized line must error, not hang or drop silently");
        assert_eq!(err, OVERSIZE_LINE_MSG, "must be the exact shared message cli.rs matches on");

        // Still closes afterward: no newline left in this connection to
        // resynchronise a next request on.
        let mut discard = String::new();
        let n = c.read_line(&mut discard).expect("must close cleanly, not hang");
        assert_eq!(n, 0, "connection must be closed after the oversize reply: {discard:?}");

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// The actual regression risk: an oversized line on one connection must
    /// not take the listener down with it, or leak the slot it held. A
    /// fresh connection right after must still connect and be served
    /// normally, on a brand new connection.
    #[test]
    fn an_oversized_line_does_not_take_the_instance_down_with_it() {
        let (path, listener) = scratch_listener("oversize-survives");
        let limits = spawn_accept_loop(listener, fake_app(), seeded_reader(&[(1, "newcomer")]));
        let mut c = connect(&path);

        let payload = "A".repeat(OVERSIZED_PAYLOAD);
        let reply = roundtrip(&mut c, &payload);
        assert!(reply.get("err").is_some(), "setup: must be the oversize reply");
        drop(c);

        // The global slot the oversized connection held must be released,
        // not leaked — the same `ConnGuard::drop` accounting path every
        // other early `break` in the loop already goes through.
        let deadline = Instant::now() + Duration::from_secs(5);
        while limits.global.load(Ordering::Relaxed) != 0 {
            assert!(Instant::now() < deadline, "oversized connection's slot was never released");
            std::thread::sleep(Duration::from_millis(10));
        }

        let mut fresh = connect(&path);
        let reply = roundtrip(&mut fresh, r#"{"token":"newcomer","method":"list"}"#);
        assert!(reply.get("ok").is_some(), "a fresh connection after an oversized one must still be served: {reply}");

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
