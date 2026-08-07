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
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::core::control::{Reply, Request};

/// P0: no client read may block forever. An accepted connection that goes
/// idle (never sends a line, or a slow/looping client that outlasts its
/// welcome) is treated the same as a hangup rather than pinning a slot in
/// `MAX_CONN` indefinitely. The existing `Err(_) => break` below already
/// exits without logging, so a timeout firing is silent, not spammy.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Max bytes accepted for one status line. A well-formed message is well under
/// this; a client that streams without a newline is dropped instead of being
/// allowed to grow an unbounded buffer (local DoS).
const MAX_LINE: u64 = 64 * 1024;

/// Cap concurrent client connections so a buggy/looping extension that
/// reconnects rapidly can't spawn unbounded threads/FDs.
const MAX_CONN: usize = 64;

/// Per-principal share of `MAX_CONN` (DESIGN-control.md §5.6 / audit M3). The
/// threat this closes: one pane (or the fleet token — same-uid-readable off
/// `<state>/control.token`, see the design doc's §5.2 correction) opening
/// enough connections/parked `wait`s to starve the human's CLI and any real
/// orchestrator. 8 is the audit's own number: generous for one caller (a
/// handful of concurrent `wait`s on a small fleet) while leaving most of the
/// 64-connection pool for everyone else — a single flooding principal can
/// never fill more than 8/64 of it by itself.
const PRINCIPAL_MAX_CONN: usize = 8;

/// Token-bucket capacity for control *commands* (not connections) from one
/// principal — see `Bucket`/`Limits::take_command`. DESIGN-control.md §7's
/// hello-world fans out as `spawn` x N then `wait` x N; 32 comfortably covers
/// the documented "spawn x10 then wait x10" burst (20) plus room for a few
/// interleaved `list`/`status` calls, without being anywhere near the ~200k
/// calls M2 needs to force two log rotations.
const PRINCIPAL_BUCKET_CAPACITY: f64 = 32.0;

/// Steady-state refill for one principal once its burst is spent. Real
/// control actions (spawn/send/read/close) are issued one at a time by a
/// human or an orchestrator reacting to a `wait` reply — `wait` exists
/// precisely so a caller never has to poll in a tight loop. At 5/s, M2's
/// ~200k-call flood takes on the order of 11 hours of sustained traffic from
/// a single principal: a real deterrent, while an order of magnitude above
/// any legitimate command cadence.
const PRINCIPAL_REFILL_PER_SEC: f64 = 5.0;

/// Aggregate backstop across *all* principals, keyed on nothing (one bucket).
/// sock.rs cannot tell a valid-but-unfamiliar token from a garbage one (that
/// needs `App::resolve_actor`, which lives in core/app.rs) — so a caller that
/// varies the `token` field per request gets a fresh per-principal bucket
/// every time and would otherwise dodge `PRINCIPAL_BUCKET_CAPACITY` entirely.
/// This bucket bounds total command throughput regardless of how many
/// identities a flood claims. 4x the per-principal numbers: generous enough
/// for a few distinct legitimate principals (fleet + a couple of panes) to
/// burst at once, still only ~2.8h to reach M2's 200k calls.
const GLOBAL_BUCKET_CAPACITY: f64 = 128.0;
const GLOBAL_REFILL_PER_SEC: f64 = 20.0;

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
///   but failed to deserialize into a `Request`. P0: the caller must reply
///   with this rather than dropping the line — a client's read has no
///   timeout of its own (`src/cli.rs`), so silently dropping a malformed
///   request used to hang it forever, and 64 of them (`MAX_CONN`) wedged the
///   whole control plane. The message is `serde_json`'s own: it already
///   names the offending field/variant and carries nothing more internal.
/// - `Some(Ok(_))` — parsed clean; dispatch as usual.
fn parse_control(line: &str) -> Option<Result<Request, String>> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v.get("method").is_none() {
        return None;
    }
    Some(serde_json::from_value(v).map_err(|e| e.to_string()))
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

    /// Refill for elapsed wall-clock time, then take one token if available.
    fn take(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Connection and command accounting shared by the listener's accept loop
/// and every per-connection thread it spawns (DESIGN-control.md §5.6).
///
/// The per-principal cap is keyed on the caller's raw wire `token` string,
/// not a resolved `Actor` — sock.rs has no access to `App::resolve_actor`
/// (that's core/app.rs, a different owner) and doesn't need it:
/// `resolve_actor` itself is exact-string-match against the fleet token or
/// one pane token per pane, so distinct token strings already mean distinct
/// principals. Peer credentials (`SO_PEERCRED`/`getpeereid`) were considered
/// instead and rejected: in this same-uid threat model the uid is always the
/// operator's own, and the pid is the *client* process on the other end of
/// one connection — for the one-shot `roost <verb>` CLI that's a fresh pid
/// every single invocation, so a `for i in (seq 200000); roost list; end`
/// flood would never repeat a pid and a pid-keyed cap would never trip. The
/// token is the one thing that *is* stable across exactly that loop (one
/// `ROOST_TOKEN` env var, inherited by every re-exec), so it is not merely
/// "all we have" — it's the only stable handle on the actual attack shape.
struct Limits {
    /// Race-free global connection count (replaces the old load-then-add —
    /// audit Info (a)).
    global: AtomicUsize,
    /// Open connections per token, so one principal can't eat the whole
    /// `MAX_CONN` pool. Entries are removed at 0, so this can never hold
    /// more entries than there are currently-open connections.
    per_principal: Mutex<HashMap<String, usize>>,
    /// Command-rate bucket per token. Unlike `per_principal`, entries persist
    /// for the life of the listener — a flood-by-reconnect must not reset its
    /// budget.
    // ponytail: unbounded map keyed by a client-supplied string. sock.rs
    // can't validate tokens (needs App), so a caller minting a fresh garbage
    // token per request grows this forever; `global_bucket` below bounds the
    // resulting throughput regardless, but not this map's memory. Add LRU
    // eviction if that ever gets exploited on its own.
    buckets: Mutex<HashMap<String, Bucket>>,
    /// Aggregate command-rate backstop, independent of principal identity —
    /// see the const doc comment above.
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

    /// Called once, for the first control request seen on a connection: admit
    /// it under `token`'s per-principal connection cap, or refuse. Every
    /// `true` must be matched by exactly one later `release_principal` call.
    fn try_reserve_principal(&self, token: &str) -> bool {
        let mut map = self.per_principal.lock().unwrap();
        let count = map.entry(token.to_string()).or_insert(0);
        if *count >= PRINCIPAL_MAX_CONN {
            false
        } else {
            *count += 1;
            true
        }
    }

    fn release_principal(&self, token: &str) {
        let mut map = self.per_principal.lock().unwrap();
        if let Some(count) = map.get_mut(token) {
            *count -= 1;
            if *count == 0 {
                map.remove(token);
            }
        }
    }

    /// One command from `token`: true if under both its own rate limit and
    /// the aggregate backstop (and a token is consumed from each); false if
    /// it should be throttled.
    fn take_command(&self, token: &str) -> bool {
        let principal_ok = {
            let mut map = self.buckets.lock().unwrap();
            map.entry(token.to_string())
                .or_insert_with(|| Bucket::new(PRINCIPAL_BUCKET_CAPACITY, PRINCIPAL_REFILL_PER_SEC))
                .take()
        };
        principal_ok && self.global_bucket.lock().unwrap().take()
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
/// `XDG_RUNTIME_DIR` env vars — racy to poke from tests that run in
/// parallel in the same process).
fn spawn_accept_loop(listener: UnixListener, tx: SyncSender<AppEvent>) {
    let limits = Arc::new(Limits::new());
    std::thread::spawn(move || {
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
                // No connection may hold its slot forever (see `READ_TIMEOUT`).
                let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
                let mut reader = BufReader::new(stream);
                let mut buf = Vec::new();
                // The token this connection reserved a per-principal
                // connection slot under (first control request only) —
                // remembered so cleanup releases exactly what was reserved.
                let mut principal: Option<String> = None;
                loop {
                    buf.clear();
                    // Cap the bytes read per line so a newline-less flood can't
                    // grow the buffer without bound.
                    let n = match reader.by_ref().take(MAX_LINE).read_until(b'\n', &mut buf) {
                        Ok(0) => break,       // EOF
                        Ok(n) => n,
                        Err(_) => break,
                    };
                    // Hit the cap without terminating — oversized line; drop the
                    // connection rather than trying to resync.
                    if buf.last() != Some(&b'\n') && n as u64 == MAX_LINE {
                        break;
                    }
                    let Ok(line) = std::str::from_utf8(&buf) else { continue };
                    let line = line.trim_end();
                    // Control request (has a `method`): execute on the main loop
                    // and write the reply back down this connection. P0: a
                    // request that parsed as JSON-with-a-method but not into a
                    // `Request` still gets a reply (the error arm) instead of
                    // being dropped — see `parse_control`.
                    match parse_control(line) {
                        Some(Ok(req)) => {
                            // First control request on this connection:
                            // charge it against `req.token`'s per-principal
                            // connection cap. A connection that later sends a
                            // *different* token isn't re-attributed — not a
                            // pattern either the CLI or an orchestrator uses,
                            // and not worth the bookkeeping (ponytail).
                            if principal.is_none() {
                                if !limits.try_reserve_principal(&req.token) {
                                    let msg = format!(
                                        "connection limit: this token already has \
                                         {PRINCIPAL_MAX_CONN} open connections; close one and retry"
                                    );
                                    let _ = write_reply(&mut reader, &Reply::err(msg));
                                    break; // over cap: free the slot now
                                }
                                principal = Some(req.token.clone());
                            }
                            // Rate limit: a command this connection is
                            // otherwise allowed to make can still be
                            // throttled if it's coming too fast. Checked
                            // (and charged) before the request ever reaches
                            // the main loop / audit log — a throttled
                            // request must cost nothing there (M2).
                            if !limits.take_command(&req.token) {
                                let msg = "rate limited: too many commands too fast; \
                                           slow down and retry"
                                    .to_string();
                                if !write_reply(&mut reader, &Reply::err(msg)) {
                                    break; // client hung up
                                }
                                continue; // stay connected; this is not a ban
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
                limits.release_global();
                if let Some(token) = principal {
                    limits.release_principal(&token);
                }
            });
        }
    });
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

    // --- §5.6 / M3: per-principal connection cap --------------------------
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

    #[test]
    fn a_legitimate_bursty_sequence_is_not_throttled() {
        let (path, listener) = scratch_listener("burst");
        spawn_accept_loop(listener, fake_app());
        let mut c = connect(&path);

        // DESIGN-control.md §7's documented pattern: spawn x10 then wait x10,
        // fired back-to-back exactly as a real fan-out/fan-in would.
        // Regressing this (the burst getting throttled) would make the fix
        // worse than the bug it closes.
        for i in 0..10 {
            let reply = roundtrip(&mut c, r#"{"token":"orch","method":"spawn","adapter":"shell"}"#);
            assert!(reply.get("ok").is_some(), "spawn #{i} of the burst was throttled: {reply}");
        }
        for i in 0..10 {
            let reply = roundtrip(&mut c, r#"{"token":"orch","method":"list"}"#);
            assert!(reply.get("ok").is_some(), "op #{i} of the burst was throttled: {reply}");
        }

        let _ = fs::remove_dir_all(path.parent().unwrap());
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
