# Design: promotion-auth-gate

> **Shipped & archived 2026-08-08.** Implemented, reviewed (architect pass →
> fable design review → adversarial review), verified black-box, and released
> in **v0.1.2** (PR #82). This design doc is kept as the record of what was
> built and why. See ROADMAP.md "Shipped since best-in-class" and
> DESIGN-control.md §5.6.

Verdict up front: **GO.** All eight hard acceptance criteria are satisfiable;
the ordering invariant (criterion 4) holds in code today (`B::spawn` has
exactly one call site, inside `spawn_pane` — all five spawn paths route
through it). Verified against main @ fd854a8. **No new dependencies.**

Criterion 2 as amended by design review: *"read path takes no lock the main
loop can contend on for more than nanoseconds; no new threads/timers/
back-channels."*

Line numbers below are current-main; the brief's were slightly stale
(PRE_AUTH_READ_TIMEOUT is sock.rs:67 not :60; READ_TIMEOUT sock.rs:86 not
:79; the status-only retry clause is sock.rs:890-899, not ~814-823).

## Problem

`src/infra/sock.rs` promotes a connection out of the 2 s pre-auth guillotine
(`PRE_AUTH_READ_TIMEOUT`, sock.rs:67) on **grammar, not identity**:

- **D-A** — a control request under any token: promotion at sock.rs:989-992
  happens after `try_reserve_principal` admits the raw wire token
  (sock.rs:973-982) but with **no validity check**; App refuses the request
  later (app.rs:1561-1562), yet the connection keeps the generous
  `READ_TIMEOUT` (30 s, sock.rs:86).
- **D-B** — a status/session line that merely parses (`parse_line`,
  sock.rs:302-334, validates shape only; comment at sock.rs:1055-1057 says so
  explicitly): promotion at sock.rs:1058-1061, **no** per-principal
  reservation at all (the status path never touches `try_reserve_principal`),
  and because `guard.principal` stays `None`, the P1 retry clause
  (sock.rs:890-899) lets the connection live forever.

Consequence: an unauthenticated same-user process says one well-formed
sentence per connection and squats a `MAX_CONN` (64, sock.rs:111) slot
indefinitely; 64 of them wedge the control plane. Root constraint: sock.rs
cannot judge tokens — the table lives in `App` (`tokens`, app.rs:459;
`socket_authorized`, app.rs:1412-1414; `resolve_actor`, app.rs:1425-1433)
and today sock.rs only forwards events for App to judge (main.rs:462-482).

## Approach

Publish the token table as a read-only snapshot that sock.rs consults
synchronously on every line **before** promotion. Promote only on successful
authentication:

- control request → token is the fleet token or some pane's token
  (exactly `resolve_actor`'s domain);
- status/session line → token matches **that pane's** token
  (exactly `socket_authorized`'s predicate).

Everything else stays under the 2 s guillotine and dies there. Authenticated
reporter connections additionally take a new per-pane cap (8).

### Decisions (choice — why — revisit-when)

1. **Std-only snapshot: `RwLock<Arc<TokenSnapshot>>`; readers clone the
   `Arc` under a read lock held for nanoseconds** — door traffic is bounded
   by the line buckets to ~tens of loads/sec (worst case ~1.3 k/s across 64
   connections), the writer is the main thread mutating on human-timescale
   events (spawn/respawn/close), and the new map is built *outside* the
   write lock, which is held only for a pointer store. "Lock-free" was a
   self-imposed criterion no workload justifies (amended criterion 2, above);
   roost's identity is a tiny dep tree, and zero deps is worth more than
   unmeasurable latency. Poisoning is moot with one writer; use
   `unwrap_or_else(|e| e.into_inner())` regardless, matching sock.rs's
   existing `lock` helper convention (sock.rs:384-386). — Revisit if a
   profiler ever shows door-load contention: swap the internals for
   `arc-swap` with the API unchanged.
2. **Single storage: `TokenTable` replaces both `App.tokens` and
   `App.control_token`** — App *reads through* the snapshot; the write
   methods ARE the publish (RCU: clone small map, mutate, swap the `Arc`).
   There is no second copy and no separate "publish" step to forget. —
   Revisit never; this is the criterion-1 mechanism.
3. **Write handle / read handle split (`TokenTable` vs `TokenReader`)** —
   `TokenTable` (not `Clone`, owned by App, mutators on it) vs `TokenReader`
   (Clone, load-only, handed to sock.rs). sock.rs structurally *cannot*
   write; App's main loop is the only writer, so single-writer RCU is sound
   by type design, not by convention. — Revisit if a second writer ever
   appears (would need a CAS/`rcu` loop).
4. **Shared predicates live on the snapshot** — `pane_authorized(id, tok)`
   and `is_principal(tok)` are methods of `TokenSnapshot`; App's
   `socket_authorized`/`resolve_actor` and the sock.rs door call the *same*
   functions, so the door check and App's check cannot drift. — Revisit
   never.
5. **Unauthenticated control request: refuse at the door, do not dispatch**
   — reply the exact string App uses today (`"unauthorized: unknown or
   missing token"`, app.rs:1532/1562 — promoted to a shared `pub const`, same
   pattern as `OVERSIZE_LINE_MSG`, sock.rs:107, so CLI-visible wording cannot
   fork), stay unpromoted. Not dispatching removes the unauthenticated
   flavor of the H3 audit-log-rotation attack entirely (today ~140 requests
   roll the 4 MiB log; see sock.rs:168-177). — Revisit if forensic logging
   of unauthorized attempts is later wanted: add a byte-bounded counter/line,
   not per-request audit writes.
6. **Unauthenticated-but-parseable status line: drop silently
   (`log_dropped`), no forward** — App would reject it anyway
   (main.rs:462-482); not forwarding saves a main-loop hop and keeps the
   connection unpromoted. — Revisit never.
7. **Per-pane reporter cap = 8, enforced in sock.rs `Limits`, charged at the
   same site as `link_panes` insertion** — one mechanism, so the cap count,
   `link_panes`, and App's `ext_link_counts` count the same thing by
   construction (no double-counting). Sizing: legit worst case is 1
   long-lived pi connection + 1 dying old-generation connection mid-respawn
   + a burst of transient claude-hook one-shots (`roost __status`,
   cli.rs:66-80, each lives milliseconds); 8 is ~2× that. — Revisit if a
   legit reporter shape with >3 concurrent long-lived connections per pane
   appears.
8. **Reporter connections do NOT take per-principal slots** —
   `PRINCIPAL_MAX_CONN` (20, sock.rs:123) is sized against `MAX_WAITS` (16,
   app.rs:273) for control traffic; charging reporters there would let a
   pane's own status links starve its control budget. Separate pools, each
   sized to its shape. — Revisit if the two shapes ever converge.
9. **`gen_secret` moves with table construction to before
   `spawn_listener`** — main.rs:298 starts the listener before `App::new`
   runs, so main.rs builds `TokenTable` first (fleet token inside), passes
   the reader to `spawn_listener` and the table to `App::new`. The
   CSPRNG-unavailable hard refusal (currently app.rs:591-592) moves to
   main.rs, still before any UI. — Revisit never.

### Type design (contract, not code)

New module content in `src/core/control.rs` (it already owns
`Request`/`Reply`/`Actor`; no new file):

```rust
pub struct TokenSnapshot { control: String, panes: HashMap<PaneId, String> }
impl TokenSnapshot {
    pub fn pane_authorized(&self, id: PaneId, token: &str) -> bool; // app.rs:1413 logic
    pub fn is_principal(&self, token: &str) -> bool;   // fleet OR any pane token
    pub fn resolve_actor(&self, token: &str) -> Option<Actor>; // app.rs:1425-1433 logic
    pub fn control(&self) -> &str;
}

/// Sole owner of token state. NOT Clone. Held by value in App.
/// Internals: Arc<RwLock<Arc<TokenSnapshot>>> — readers clone the inner Arc
/// under a read lock held for nanoseconds; writers build the new snapshot
/// outside the lock and hold the write lock only for the pointer store.
pub struct TokenTable { /* Arc<RwLock<Arc<TokenSnapshot>>> */ }
impl TokenTable {
    pub fn new() -> Option<TokenTable>;      // gens fleet token; None = no CSPRNG (fatal in main)
    pub fn reader(&self) -> TokenReader;
    pub fn load(&self) -> Arc<TokenSnapshot>;                 // read-lock, clone Arc, drop lock
    pub fn set_pane_token(&mut self, id: PaneId, token: String);  // RCU swap = the publish
    pub fn remove_pane_token(&mut self, id: PaneId);              // RCU swap = the publish
}

/// Load-only view for sock.rs. Clone. No mutators exist on this type.
pub struct TokenReader { /* same Arc<RwLock<..>> */ }
impl TokenReader { pub fn load(&self) -> Arc<TokenSnapshot>; }
```

All lock acquisitions recover from poisoning via
`unwrap_or_else(|e| e.into_inner())` (single writer; a panicked reader can't
leave the state torn — same reasoning as sock.rs:378-386).

`App` field changes: `tokens: HashMap<PaneId, String>` (app.rs:459) and
`control_token: String` (app.rs:477) are **deleted**, replaced by
`tokens: TokenTable`. Every current accessor keeps its signature and
delegates: `socket_authorized` → `self.tokens.load().pane_authorized(..)`,
`resolve_actor` → snapshot, `control_token()` → snapshot (returns owned
`String` now — one call site, main.rs:333).

**Post-review fix (landed):** `set_pane_token`/`remove_pane_token` (and the
private `publish` helper both funnel through) take `&mut self`, not `&self`
as an earlier cut of this design had them. See I3 below — this is what
turns single-writer from a documented convention into a compiler-checked
fact.

### Door logic in `spawn_accept_loop` (sock.rs)

Signature: `spawn_listener(tx, tokens: TokenReader)`;
`spawn_accept_loop(listener, tx, tokens)`.

Per line, after `parse_control`/`parse_line`, before any promotion:

- `Some(Ok(req))` (control): `tokens.load().is_principal(&req.token)`?
  - **yes** → existing flow unchanged: `try_reserve_principal`, set
    `guard.principal`, promote (sock.rs:973-992), buckets, dispatch.
  - **no** → `write_reply(err UNAUTHORIZED_MSG)`; `continue`. No principal
    reservation, no promotion, no dispatch. The connection remains under the
    pre-auth wall clock (measured from accept, sock.rs:827) and dies at 2 s.
    Side benefit: junk tokens can no longer enter `per_principal` or mint
    `buckets` entries — `MAX_TRACKED_TOKENS` eviction (sock.rs:201) becomes
    a belt-and-braces bound instead of a live surface.
- `Some(ev)` (status/session): `tokens.load().pane_authorized(pane, &token)`?
  - **yes, first authenticated pane on an unpromoted connection** → reserve a
    per-pane reporter slot (`Limits.reporters`, below); if the pane is at
    cap, `log_debug` and `break` (close — the pane already has 8 live
    reporters; roost.ts retry covers the transient case). Otherwise insert
    into `link_panes` + emit `ExtLink` up (sock.rs:1081-1087, gated), promote
    (sock.rs:1058-1061), forward `ev`.
  - **yes, already promoted, new pane on this connection** → same
    reserve-then-link path, but on cap-full: skip link tracking, still
    forward `ev` (mirrors today's `MAX_LINK_PANES_PER_CONN` overflow
    behavior, sock.rs:1072-1080). Don't kill an authenticated connection for
    a second pane's cap.
  - **no** → `log_dropped(line)`; `continue`. No promotion, no forward, no
    link entry.

**Slot ⟺ entry, pinned:** a reporter slot is reserved **iff** a `link_panes`
insert actually happens — explicitly including the
`MAX_LINK_PANES_PER_CONN` overflow skip path (sock.rs:1082-1087): when the
per-connection pane cap skips the insert, the reserve must be skipped (or
immediately released) too. Reserving on the overflow path would strand a
slot with no `link_panes` entry to release it at drop, silently eating the
pane's cap of 8 over time — a false-rejection bug with no visible error.
The implementation shape that makes this hard to get wrong: one function
owns the sequence *check conn cap → reserve pane slot → insert entry → emit
link-up*, with the reserve rolled back on any later refusal in that
sequence. Release at drop iterates `link_panes` (sock.rs:600-612), so
entries and slots stay 1:1 by construction.

`Limits` gains `reporters: Mutex<HashMap<PaneId, usize>>` +
`const REPORTER_MAX_CONN_PER_PANE: usize = 8`, with reserve/release exactly
mirroring `per_principal` (entries removed at 0).

The P1 retry clause (sock.rs:890-899) is **unchanged in text** but its
meaning tightens: `promoted && guard.principal.is_none()` now implies
"authenticated reporter", so the never-expire privilege requires identity.

## Rejected alternatives

- **Promote provisionally, demote on App's rejection** — the rejection takes
  a main-loop round trip; during that window the connection holds a promoted
  slot, and a reconnect loop lives in that window permanently (64 loops =
  the same wedge, just noisier). Also needs a back-channel from App to a
  specific connection thread, which doesn't exist and criterion 2 forbids.
- **Copy of the token map pushed to sock.rs on change (channel/message)** —
  two storages, and every future mint/remove site must remember to push; a
  missed publish site is exactly what criterion 1 rules out. The RCU table
  makes write-and-publish one operation.
- **`arc-swap` dependency for genuinely lock-free loads** — rejected by
  design review: the door's load rate (bounded by line buckets to ~1.3 k/s
  worst case) cannot contend a nanosecond read lock in any measurable way,
  and roost deliberately keeps a tiny dependency tree. The
  `TokenTable`/`TokenReader` API is internals-agnostic, so this remains a
  drop-in upgrade if profiling ever disagrees.
- **`SO_PEERCRED`-based identity** — already considered and rejected in the
  codebase for good reason (sock.rs:397-404): same-uid threat model, pids
  are per-invocation.
- **Refuse-and-close on unauthenticated control instead of reply-then-2s** —
  closing without a reply regresses P0 (sock.rs:270-274): the CLI reads with
  no timeout and a silent close reproduces the old hang-diagnosis problem.
  Reply first, let the guillotine do the closing.

## Invariants

- **I1 (single storage):** the swapped snapshot is the only token storage;
  App reads through it. Enforced by deleting `App.tokens`/`App.control_token`
  — code that wants a token *must* go through `TokenTable`, whose mutators
  swap atomically. Write sites are exactly the current three:
  mint app.rs:1022-1024 (`spawn_pane` — the single insert site in the crate;
  `B::spawn` has one call site and all five spawn paths — spawn, respawn,
  float-spawn, undo-restore, restore — route through it), remove
  app.rs:2072 (`close_pane_id`), remove app.rs:4554 (`close_float`).
- **I2 (mint-before-child):** a pane's token is published before its child
  process exists. Evidence: `spawn_pane` inserts the token at app.rs:1024
  and calls `B::spawn` at app.rs:1048, same function, same (main) thread;
  the write-lock release before `B::spawn` and the reader's later lock
  acquisition give the required happens-before, and the child cannot
  connect before `B::spawn` returns. Therefore no legitimate client can
  ever race the snapshot. **Verified — not a no-go.**
- **I3 (single writer):** enforced by the compiler, not by convention.
  `TokenTable::set_pane_token`/`remove_pane_token`/`publish` take `&mut
  self`, so calling one requires exclusive access to the whole table —
  and since `TokenTable` is owned by value in `App` (not `Arc`-shared,
  not `Clone`), the only path to `&mut` is through `App`'s own `&mut
  self`. Two overlapping `publish` calls (load → clone → mutate → write,
  not atomic as a whole) therefore cannot interleave; the borrow checker
  rejects it before runtime locking is even in question. sock.rs holds
  `TokenReader`, which has no mutators at all, so it cannot write
  regardless. Plain RCU (clone-mutate-store) is race-free under exactly
  this condition. (Landed as a post-review fix — the mutators originally
  took `&self`, relying on there being only one caller in practice; the
  type now makes that structural.)
- **I4 (gate is admission-only; App stays authoritative):** every forwarded
  event/request is still judged by App at dispatch (main.rs:462-482,
  app.rs dispatch) against the same storage. A token rotated between door
  and dispatch (respawn race) is rejected by App exactly as today — the
  gate only narrows what gets in; it never widens what's trusted.
- **I5 (refcount balance):** `ExtLink` up is emitted iff a `link_panes`
  entry was inserted (now auth-gated, slot⟺entry pinned above); down is
  emitted per entry at drop. A stale-token up that App drops has its
  matching stale-token down also dropped (same token travels in both,
  main.rs:478-482), and respawn zeroes the count anyway (app.rs:1034).
  `ext_link_counts` (app.rs:473, 2277-2288) stays a sound refcount.

## File-by-file plan (complete blast radius)

1. `src/core/control.rs` — `TokenSnapshot`/`TokenTable`/`TokenReader`
   (std `RwLock` internals); `pub const UNAUTHORIZED_MSG`; move
   `gen_secret`/`gen_token` here from app.rs:5285/5300 (pub(crate)).
2. `src/core/app.rs` — field swap (delete app.rs:459, :477); `App::new`
   takes `TokenTable`, drops its own gen (app.rs:588-592);
   `socket_authorized`/`resolve_actor`/`control_token()` delegate to
   snapshot; mint/remove via table methods (app.rs:1022-1024, :2072,
   :4554); tests that poke `app.tokens.insert(..)` (app.rs:7362, 7432,
   7935, 7991-…, 8094-8122) use `set_pane_token`.
3. `src/infra/sock.rs` — `spawn_listener`/`spawn_accept_loop` take
   `TokenReader`; door checks as specced; `Limits.reporters` + cap with
   slot⟺entry pinning; `ConnGuard` release; test churn (below).
4. `src/main.rs` — build `TokenTable` before `spawn_listener` (main.rs:298),
   fatal on `None`; pass reader to listener, table to `App::new`; the
   `control.token` write (main.rs:332-333) and event-loop auth
   (main.rs:462-482) are unchanged in behavior.
5. `extensions/roost.ts` — retry with backoff + send-kick (below).
6. `DESIGN-control.md` — amend §5.6 (admission is authenticated; per-pane
   reporter cap).
7. Tests: `src/infra/sock.rs` mod tests, `src/core/app.rs` mod tests,
   `tests/socket_status.rs` / `tests/status_hook.rs` (should pass
   unchanged — they use real tokens; that IS evidence).

`Cargo.toml` is **not** touched. **Also not touched:** `src/infra/pty.rs`,
pane lifecycle beyond the three existing token lines, persistence
(`store.rs`), all of `src/ui/**`, adapters. No PTY/pane/persistence/UI
behavior changes.

## roost.ts retry policy (criterion 6)

Today: dial once, `sock = null` on any error, dead for the session
(roost.ts:43-57). Spec:

- Reconnect on `error` and on `close` (unless `session_shutdown` initiated
  it): attempts 1..=5, delay `250ms * 2^(n-1)` capped at 4 s (250, 500,
  1000, 2000, 4000), ±25 % jitter. After 5 failures in a burst, stop
  retrying on close/error alone.
- **A `send()` that finds `sock === null` kicks a reconnect attempt**
  (starting a fresh burst if none is in flight) — it must not silently
  no-op. Rationale: a >15 s pi cold start would otherwise burn all five
  retries on idle connections (five pre-auth 2 s kills, since nothing is
  sent before `session_start` fires) before `session_start` ever fires,
  leaving the session permanently silent. The message itself rides the
  replay (next bullet), so nothing is lost.
- Keep `last = { session?, status? }`, updated by every `send()` even while
  disconnected; on successful reconnect, replay session then status once.
  This re-establishes App's link refcount (link-up requires a line), makes
  every reconnected socket authenticate on its first line (so it is never
  idle-killed at 2 s again), and heals the "one false rejection costs a
  session of exact statuses" failure. Duplicate status/session lines are
  idempotent App-side (app.rs:2262-2263, :2292-2296).
- Reset the attempt counter after a connection survives 30 s (a later
  mid-session drop gets a fresh budget).
- No thundering herd: one extension per pane, ≤ a dozen panes, jittered.

**The claude hook path gets NO retry — deliberately.** `roost __status`
(src/cli.rs:80-110) is a one-shot invocation, silent by contract (never
writes to stdout/stderr, never exits nonzero — cli.rs:74-79), and
self-healing: hooks re-fire on the next Claude Code lifecycle event, so a
dropped report costs one stale status for seconds, not a session. Do not
"fix" this later; retry loops inside a hook would hold Claude Code's hook
execution hostage to socket latency.

## Test plan with mutation checks

Mutation checks named first — each names the code change that must turn it
red:

- **MC-gate-status** (sock tests): seed reader with pane 1 → `T`; connect,
  send `{"pane":1,"token":"junk","event":"status",...}`; assert the
  connection is closed within ~`PRE_AUTH_READ_TIMEOUT` + margin and no
  `ExtLink` up was emitted and `Limits.reporters` is empty. *Reds if the
  door check on the status path is removed* (grammar-promotion restored).
- **MC-gate-control** (sock tests): junk-token control request → exact
  `UNAUTHORIZED_MSG` reply, then EOF by ~2 s; assert `per_principal` is
  empty and no `AppEvent::Command` reached the capturing rx. *Reds if the
  door check on the control path is removed, or if the door starts
  dispatching to App.*
- **MC-ordering** (app.rs tests): test `PaneBackend` whose `spawn` asserts
  `table.load().pane_authorized(id, &env_token)` for the token in `cmd.env`
  at spawn time. *Reds if anyone reorders the mint (app.rs:1024) after
  `B::spawn` (app.rs:1048) or adds a deferred publish.*
- **MC-survives-silence** (sock tests): existing
  `a_status_only_connection_survives_an_idle_gap_past_read_timeout`
  (sock.rs:1622) reseeded with a valid pane token — the E2E-shaped proof
  that a VALID reporter outlives 30 s+ of silence. *Reds if promotion or the
  P1 retry clause breaks for authenticated reporters.*
- **MC-pane-cap** (sock tests): 8 valid reporter connections for pane 1 all
  admitted; the 9th gets EOF promptly, no link-up; close one, a new one is
  admitted (slot released via `ConnGuard`). *Reds if the cap or its release
  is removed.*
- **MC-overflow-no-strand** (sock tests): one connection already linked to
  `MAX_LINK_PANES_PER_CONN` panes (valid tokens) sends a valid line for a
  further pane P — the event still forwards, **and** `Limits.reporters[P]`
  is unchanged (no slot consumed); repeat N times, then verify a fresh
  connection for P is still admitted. *Reds if the overflow skip path
  reserves (strands) a slot — the silent cap-erosion bug pinned above.*
- **MC-refcount** (existing, must stay green): F1 tests —
  `claude_one_shot_connections_flick_the_link_up_then_back_down`
  (sock.rs:1807), `connection_eof_emits_link_down_for_that_pane_only`
  (sock.rs:1718), `respawn_clears_a_refcount…` (app.rs:8086). *Reds if cap
  accounting double-counts or breaks link tracking.*

No-false-rejection per legit shape:

- **pi long-lived**: MC-survives-silence.
- **claude one-shot**: `tests/status_hook.rs` + sock.rs:1807 (reseeded) —
  connect, one valid line, EOF; status lands, link flicks up/down.
- **nested claude in a pi pane**: two concurrent valid connections for one
  pane (one held, one one-shot) both admitted (< 8), refcount 2→1→0 —
  extends the existing F1 test.
- **control CLI**: `tests/cli.rs` and sock roundtrip tests seeded with the
  fleet token — list/spawn/wait all still answer; a fleet `wait` still
  parks past 30 s (existing `an_authenticated_connection_gone_silent…`,
  sock.rs:1539, reseeded).

Known test churn (existing sock tests that used arbitrary tokens and now
must seed the reader): sock.rs:1424, 1539, 1576, 1622, 1686, 1718, 1807,
1861, 1963, 2000, 2022 (H2 test now rotates between two *valid* tokens),
2062, 2091, 2111, 2141. The two flood tests at sock.rs:1465
(`squatting_connections…`) and :1499 (`dripping_connections…`) split down
the middle: their **flood clients stay unauthenticated** — that is now the
point — but their fresh-caller **probes** (`"newcomer"` token,
sock.rs:1483, :1507) must use a seeded valid token or both go red at the
door. So amended, they assert a strictly *stronger* property than before:
an **authenticated** newcomer still gets through while the pool is under
unauthenticated flood. Only sock.rs:1514 (idle pre-auth recycling) is
genuinely unaffected. The `spawn_accept_loop` test helper gains a
`TokenReader`-builder.

## Interactions ("also address")

- **MAX_TOKEN_LEN / parse caps:** both run *before* the door —
  `parse_control` rejects >128-byte tokens with a reply (sock.rs:285-287),
  `parse_line` drops them (sock.rs:317-319) — so the gate never compares a
  string longer than 128 bytes. `MAX_LINE`/oversize handling
  (sock.rs:915-925) is upstream of everything and unchanged.
- **Audit log:** door-rejected traffic writes **nothing** to `control.log`.
  Today an unauthorized control request costs an audit line
  (app.rs:1561-1562) — attacker-writable bytes into a 4 MiB log kept at one
  generation (the H3 rotation-erase vector, sock.rs:168-177). After this
  change, only *authenticated* requests reach audit; the unauthenticated
  flood erases nothing. Rejected-at-door lines are visible under
  `ROOST_DEBUG` via `log_debug` (volume bounded per connection by
  `line_bucket` + the 2 s lifetime; same exposure class as today's
  `log_dropped`).
- **ExtLink / main.rs:** `ExtLink` events now originate only from
  door-authenticated lines; the `socket_authorized` re-check at
  main.rs:478-482 stays (I4/I5 — covers door-to-dispatch token rotation).
  No change to `on_status_link` (app.rs:2277-2288).
- **Mixed-use connections:** a valid *control* request on an authenticated
  reporter connection sets `guard.principal`, which forfeits the P1
  never-expire clause (it keys on `principal.is_none()`). No real client
  mixes modes (roost.ts is status-only; CLI is control-only); documented,
  accepted.

## Risks & mitigations

- **Respawn overlap eats reporter slots** — old-generation connections hold
  slots until the kill closes their sockets; 8 has explicit headroom for
  1 old + 1 new + hook bursts (decision 7).
- **Stranded reporter slots via the overflow path** — pinned to slot⟺entry
  above; MC-overflow-no-strand exists precisely to keep it impossible.
- **Snapshot staleness at the door** — bounded by I4: worst case a
  just-revoked token's line is admitted at the door and rejected by App;
  identical to today's exposure, and the connection is then an
  *authenticated-then-revoked* reporter counted against its pane's cap.
- **Test-suite churn regressing coverage** — every reseeded test is listed
  above, including the split treatment of the two flood tests (flood
  unauthenticated, probe seeded).
- **Non-constant-time token compare** — pre-existing (`resolve_actor` does
  string ==); same-uid timing attacks are out of the threat model here.
  Not changed, noted for honesty.

## What this does NOT fix (for PR/README language — do not overclaim)

- An **authenticated** hostile pane (it holds its own real token) can still
  open its capped share: 8 reporter connections + up to 20 control
  connections + its command-rate budget. The gate bounds squatting to
  authenticated, capped identities; it does not make a compromised pane
  harmless.
- Any same-user process that can read `<state>/control.token` is fully
  trusted — the 0600 file/dir is the boundary, unchanged.
- The H3 audit-byte-bounding NEED for *authenticated* callers
  (`sanitize` caps, app.rs:5248-5259) — separate work, unchanged here.
- Status-line floods from an authenticated pane — still bounded only by
  per-connection line buckets, as today.

## Rollback / compat

- **Wire format unchanged** in both directions — the gate changes *when*
  promotion happens, never the protocol. No new fields, no new message.
- **New roost + old roost.ts:** the extension's lines carry the valid pane
  token, so it authenticates and is promoted exactly as before. On a
  transient failure it stays silent for the session — today's behavior, not
  worse. Never wedges.
- **Old roost + new roost.ts:** retry/backoff and send-kick only engage on
  error/close/disconnected-send; old roost accepts grammar-promotion, so
  fewer errors fire at all; replayed status/session lines are idempotent
  App-side. Never wedges.
- **Mid-upgrade skew** is the natural state: roost rewrites the extension at
  startup (`ensure_ext_install`), so pairs converge on next launch.
- **Code rollback:** revert the sock.rs door + `Limits.reporters` and the
  grammar-promotion behavior returns; the `TokenTable` refactor is
  behavior-neutral on its own and can stay. Suggested implementation order
  exploits this: land the table refactor first (pure refactor, full suite
  green), then the door, then the cap, then roost.ts.

## First step for the coder

Step 1 of 4: the behavior-neutral `TokenTable` refactor —
`src/core/control.rs` types + `App` field swap + `main.rs` wiring, entire
suite green with **zero** sock.rs behavior change and zero new
dependencies. Then the door (MC-gate tests first, red→green), then the cap,
then roost.ts.

## Implementation status (post-review)

All four phases shipped on `harden/promotion-auth-gate` (4 phase commits +
1 fix commit). Adversarial review verdict: SHIP — could not construct a
false rejection of any legitimate client shape (pi cold start, claude
one-shots, nested claude, control CLI, respawn overlap, startup restore).
One should-fix applied: `TokenTable`'s three mutators changed from `&self`
to `&mut self` so I3 is compiler-enforced (see I3 above) — zero production
call sites broke, ~8 test-local bindings gained `mut`. Full suite green
throughout (770 unit + 47 integration, 0 failures); `cargo clippy` at the
4 pre-existing baseline warnings.

**Sequencing note:** a separate, more severe pre-existing bug (~30 held-open
connections permanently wedging the control plane, unrelated to auth,
already shipped in v0.1.0/v0.1.1) is being root-caused on
`fix/control-plane-wedge` off `main`. That fix touches the same
`Limits`/`ConnGuard` machinery this change's Phase 3 lives in, and lands
first. This branch does not merge until rebased onto the fixed `main` and
re-verified (full suite + all four mutation checks re-run against the new
base).
