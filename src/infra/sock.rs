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
//! "exited" is still accepted for stale extensions but is advisory only —
//! `StatusTracker::set_extension_status` demotes it to Waiting. Process death
//! has exactly one ground truth (the pane's PTY EOF): the pane's env is
//! inherited by every descendant, so a nested pi finishing its work would
//! otherwise report *its* shutdown as the pane's death.

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

use crate::core::control::{Reply, Request};

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

/// Max bytes accepted for one status line. A well-formed message is well under
/// this; a client that streams without a newline is dropped instead of being
/// allowed to grow an unbounded buffer (local DoS).
const MAX_LINE: u64 = 64 * 1024;

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
    reader.get_mut().write_all(json.as_bytes()).is_ok()
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
            Some(AppEvent::Status(pane, token, status))
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
}

impl Limits {
    fn new() -> Self {
        Limits {
            global: AtomicUsize::new(0),
            per_principal: Mutex::new(HashMap::new()),
            buckets: Mutex::new(HashMap::new()),
            global_bucket: Mutex::new(Bucket::new(GLOBAL_BUCKET_CAPACITY, GLOBAL_REFILL_PER_SEC)),
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

/// RAII: releases whatever connection-accounting slots this connection holds
/// when dropped — including during a panic unwind (audit finding L1).
/// Without this, a panic partway through the read loop (e.g. a
/// poisoned-mutex `.unwrap()` after some *other* thread already panicked —
/// see `lock` above) would skip the release-at-the-bottom calls entirely and
/// leak the slot forever. `principal` is set at most once, the moment this
/// connection is admitted under a real principal (see the `Limits` doc
/// comment — before that, a connection is bounded by time, not a shared
/// slot, so there's nothing else here to release).
struct ConnGuard<'a> {
    limits: &'a Limits,
    principal: Option<String>,
}

impl Drop for ConnGuard<'_> {
    fn drop(&mut self) {
        self.limits.release_global();
        if let Some(token) = &self.principal {
            self.limits.release_principal(token);
        }
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

/// Bind the socket and pump parsed events into the main loop. Returns the
/// bound path (exported to panes as ROOST_SOCK).
pub fn spawn_listener(tx: SyncSender<AppEvent>) -> Result<PathBuf> {
    let path = socket_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
        // The socket is the control plane (it can set session ids / status);
        // keep its directory private to the owner...
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        // ...and refuse to run if it isn't actually ours and private (an
        // attacker may have pre-created it to intercept the socket).
        if !dir_is_private_and_ours(dir) {
            bail!("roost: socket directory {} has unsafe ownership/permissions", dir.display());
        }
    }
    let _ = fs::remove_file(&path); // stale socket from a previous run
    let listener = UnixListener::bind(&path)?;
    // Restrict the socket to the owner so another local user can't connect and
    // poison session ids / spoof status.
    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));

    spawn_accept_loop(listener, tx);
    Ok(path)
}

/// The accept loop, split out from `spawn_listener` so tests can drive it
/// against a scratch `UnixListener` bound at a throwaway path instead of the
/// real one (which is only reachable via the process-global `ROOST_STATE`/
/// `XDG_RUNTIME_DIR` env vars — racy to poke from tests that run in parallel
/// in the same process). Returns the shared `Limits` so tests can also
/// inspect accounting state directly instead of only through the wire.
fn spawn_accept_loop(listener: UnixListener, tx: SyncSender<AppEvent>) -> Arc<Limits> {
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
            std::thread::spawn(move || {
                // From here on, `guard` owns releasing every slot this
                // connection holds — including on an early return or a
                // panic unwind (audit finding L1).
                let mut guard = ConnGuard { limits: &limits, principal: None };

                // Short until promoted (audit finding C1): an idle or
                // slow-to-identify connection is recycled in seconds rather
                // than squatting a global slot for the full `READ_TIMEOUT`.
                // Extended once this connection proves itself with a
                // well-formed request, below.
                let _ = stream.set_read_timeout(Some(PRE_AUTH_READ_TIMEOUT));
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
                // Promoted the instant this connection sends *any* well-formed
                // line — a control request (below) or a one-way status/session
                // report (`parse_line`), not only the former. A status-only
                // connection (the pi extension: connects once, holds the
                // socket for the pane's process lifetime, sends nothing but
                // status/session lines) never sets `guard.principal` — it
                // isn't a "principal" for connection-cap/rate-limit purposes —
                // so gating promotion on that alone left every such connection
                // permanently pre-auth: `pre_auth_start` never cleared, so it
                // was killed ~`PRE_AUTH_READ_TIMEOUT` after connecting and
                // never reconnects (the extension dials once, on load).
                // Promoting it here is no new exposure: an attacker sending
                // one *shape*-valid status/session line (nothing here
                // validates its token — `parse_line` doesn't, App does) to
                // earn promotion is the same already-open, already-documented
                // path 2 as a garbage-token control request.
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
                    // Hit the cap without terminating — oversized line; drop the
                    // connection rather than trying to resync.
                    if buf.last() != Some(&b'\n') && n as u64 == MAX_LINE {
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
                            // First well-formed request on this connection:
                            // charge it against `req.token`'s per-principal
                            // connection cap and, if admitted, promote this
                            // connection out of the pre-auth timeout. A later
                            // request on this same connection that claims a
                            // *different* token is never re-attributed —
                            // every charge below uses `guard.principal`, the
                            // identity this connection was actually admitted
                            // under (audit finding H2).
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
                            // A well-formed status/session report promotes
                            // this connection exactly like a control request
                            // does, above — see the `promoted` doc comment at
                            // its declaration. Nothing here validates the
                            // token; App does, on every event this forwards.
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

    #[test]
    fn parses_string_and_numeric_pane_ids() {
        let ev = parse_line(r#"{"pane":"7","event":"status","token":"tok","status":"working"}"#);
        assert!(matches!(ev, Some(AppEvent::Status(7, ref t, AgentStatus::Working)) if t == "tok"));
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
        assert!(matches!(ev, Some(AppEvent::Status(7, ref t, _)) if t.is_empty()));
    }

    #[test]
    fn ignores_garbage() {
        assert!(parse_line("not json").is_none());
        assert!(parse_line(r#"{"pane":"x","event":"status","status":"working"}"#).is_none());
        assert!(parse_line(r#"{"pane":"1","event":"status","status":"???"}"#).is_none());
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
        let result = std::thread::spawn(move || {
            let _guard = ConnGuard { limits: &l, principal: None };
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
        spawn_accept_loop(listener, fake_app());

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
        let limits = spawn_accept_loop(listener, fake_app());

        // A full MAX_CONN worth of connections that never send anything —
        // M3's original attack shape. There is no shared pre-auth counter
        // for these to fill any more.
        let mut silent = Vec::new();
        for _ in 0..MAX_CONN {
            silent.push(connect(&path));
        }
        poll_until_admitted(&limits, MAX_CONN);

        // A fresh, well-formed caller must still connect and get a reply —
        // squatters recycle on the pre-auth deadline rather than holding
        // the cap forever, so this must succeed well within a couple of
        // `PRE_AUTH_READ_TIMEOUT` cycles, not eventually.
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
        let limits = spawn_accept_loop(listener, fake_app());

        spawn_drippers(&path, MAX_CONN);
        poll_until_admitted(&limits, MAX_CONN);

        let deadline = Instant::now() + Duration::from_secs(10);
        let reply = try_request(&path, r#"{"token":"newcomer","method":"list"}"#, deadline);
        assert!(reply.get("ok").is_some(), "a drip flood must never lock out a fresh caller: {reply}");

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn an_idle_pre_auth_connection_is_recycled_well_under_read_timeout() {
        let (path, listener) = scratch_listener("recycle");
        spawn_accept_loop(listener, fake_app());

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
        spawn_accept_loop(listener, fake_app());
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
        spawn_accept_loop(listener, fake_app());
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
    /// (`agent_end` → `waiting`), the human reads the output for more than
    /// 30s — the *normal* operating condition, not an edge case — the
    /// server silently closed the connection (`Err(_) => break` on the
    /// `READ_TIMEOUT` firing), and the extension's next write EPIPEs and
    /// nulls its socket handle for good (`sock.on("error", () => sock =
    /// null)`, no reconnect anywhere in it): every later status/session
    /// report for that pane is dropped for the rest of its life. Sleeps past
    /// the real constant on purpose — this is the timescale that matters,
    /// not a shortened stand-in for it.
    #[test]
    fn a_status_only_connection_survives_an_idle_gap_past_read_timeout() {
        let (path, listener) = scratch_listener("status-survives-read-timeout");
        spawn_accept_loop(listener, fake_app());
        let mut c = connect(&path);

        c.get_mut().write_all(br#"{"pane":"1","token":"t","event":"status","status":"working"}"#).unwrap();
        c.get_mut().write_all(b"\n").unwrap();

        // The gap size the P1 finding is actually about — past READ_TIMEOUT
        // itself, not just the pre-auth deadline.
        std::thread::sleep(READ_TIMEOUT + Duration::from_secs(1));

        let reply = roundtrip(&mut c, r#"{"token":"t","method":"list"}"#);
        assert!(
            reply.get("ok").is_some(),
            "a status-only connection must survive an idle gap past READ_TIMEOUT: {reply}"
        );

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    // --- §5.6 / M3: token-bucket command rate limit ------------------------

    #[test]
    fn a_flood_is_throttled_not_dropped_or_hung() {
        let (path, listener) = scratch_listener("flood");
        spawn_accept_loop(listener, fake_app());
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
        spawn_accept_loop(listener, fake_app());
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
    #[test]
    fn rotating_the_bodys_token_does_not_mint_a_fresh_bucket_on_one_connection() {
        let (path, listener) = scratch_listener("rotate");
        let limits = spawn_accept_loop(listener, fake_app());
        let mut c = connect(&path);

        let total = PRINCIPAL_BUCKET_CAPACITY as usize + 20;
        let mut throttled = 0;
        for i in 0..total {
            let token = if i == 0 { "admitted".to_string() } else { format!("rotated-{i}") };
            let reply = roundtrip(&mut c, &format!(r#"{{"token":"{token}","method":"list"}}"#));
            if reply.get("err").and_then(|v| v.as_str()).is_some() {
                throttled += 1;
            }
        }
        assert!(throttled > 0, "varying the token per request must still get throttled eventually");

        // The only thing ever charged is the connection's real admission
        // identity — none of the "rotated" in-body tokens minted their own.
        let map = limits.buckets.lock().unwrap();
        assert!(map.contains_key("admitted"), "the connection's admitted identity should have a bucket");
        assert_eq!(
            map.len(),
            1,
            "an in-body token that never admitted this connection must not get its own bucket: {:?}",
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
        spawn_accept_loop(listener, fake_app());

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
        spawn_accept_loop(listener, fake_app());
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
        // app.rs's actual wording (core/app.rs `handle_control_msg`), out of
        // scope here to touch or invoke directly — this pins that sock.rs's
        // own strings never collide with it.
        const UNAUTHORIZED: &str = "unauthorized: unknown or missing token";

        let (path, listener) = scratch_listener("distinguish");
        spawn_accept_loop(listener, fake_app());

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
            assert_ne!(s.as_str(), UNAUTHORIZED, "{name} reply must not equal app.rs's unauthorized text");
        }
        assert_ne!(malformed, cap_err, "malformed and connection-limit must read differently");
        assert_ne!(malformed, throttled, "malformed and rate-limit must read differently");
        assert_ne!(cap_err, throttled, "connection-limit and rate-limit must read differently");
        assert!(cap_err.contains("connection limit"));
        assert!(throttled.contains("rate limited"));

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
