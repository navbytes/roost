# roost control interface — design synthesis

*A programmatic interface through which LLMs (and humans, and scripts) can
manage/control roost. Synthesized from a five-member design tribunal
(CLI-champion, socket-RPC-champion, MCP-champion, security/architecture
adversary, product/use-case lens).*

> **Status: shipped.** This is the design record, written before the build and
> kept in its original future tense — §2's "what already exists" and the
> phase plan describe the codebase as it was, not as it is. Phases 1 and 2 are
> done: `list/status/spawn/fork/send/read/close/wait`, ownership-scoped
> capabilities, a CSPRNG fleet token, and the audit log all shipped. Phase 3
> stayed unbuilt on purpose. For the verbs as they behave today see
> [README.md](README.md#controlling-roost-cli--llm) and `roost --help`; for
> what is left, [ROADMAP.md](ROADMAP.md).

---

## 1. The reframing

roost already has every primitive an orchestrator needs — `spawn_pane`,
`forward_bytes` (send input), `grab_text` (read a pane's screen), `on_status`
(exact per-pane status), `pane_order`/`tab_summary` (list), `close_pane`,
`undo`. They are methods on `App<B>`, reachable **only by a human pressing
`Alt`-keys.** roost also already runs a per-instance unix socket — but it is
*inbound-only*: agents report their status/session over it; nothing flows back.

So this is **not** "bolt an API onto roost." It is: *open a second door into the
same house* — expose the existing action surface, add a read/wait surface, and
send commands the inverse direction over conceptually the same wire.

That reframing is the most important conclusion of the tribunal, because it
means **the wire format is downstream of everything else.** The CLI, the socket
RPC, and the MCP server are three *skins* over one underlying capability. What
actually decides success is the credential model, the `wait` primitive, and
staying daemonless. Design those first; pick skins later (you can even ship two).

---

## 2. What already exists (the ~90%)

| Capability | Where | Status |
|---|---|---|
| Spawn a pane running an adapter in a cwd | `app.rs` `new_pane_with`/`spawn_pane` | exists; needs to *return the id* |
| Send input to a pane | `app.rs` `forward_bytes` | **only targets the focused pane** — the one real gap |
| Read a pane's screen / scrollback | `ports.rs` `grab_text`, `set_scrollback` | exists, per-pane, reading-order text |
| Exact status per pane | `status.rs` `AgentStatus`, `app.rs` `on_status` | exists; extension gives working/needs_input/waiting/exited |
| List panes/tabs | `app.rs` `pane_order`, `tab_summary`, `find_spec` | exists |
| Close / undo / rename / new-tab / focus | `app.rs` | exists |
| Per-instance socket, 0600, owner-only dir | `sock.rs` | exists |
| Per-pane auth token (`ROOST_TOKEN`) | `app.rs` `gen_token`, `socket_authorized` | exists (status-only) |
| Single-threaded command intake (mpsc) | `main.rs` event loop | exists (one-way today) |
| Bounded channel → real backpressure | `main.rs` | exists |

## 3. The gaps (identical regardless of transport)

1. **No reply path.** The socket→loop mpsc is fire-and-forget; a query like
   "read pane 3" has no way to return a value. Need `AppEvent::Command{req,
   reply}` carrying a one-shot reply channel the loop fills after applying the op.
2. **Focus-relative ops.** `forward_bytes`/`close_pane`/split act on
   `self.focused`. Control needs *pane-addressed* variants (`send_input_to(id)`,
   `close_pane_by_id(id)`, `spawn` returning the id). Mechanical but touches
   several methods + their tests. **Crucially, the API must never move the
   human's focus** — that would wreck the human-takeover story (§6).
3. **No `wait`/subscription.** The single most important ergonomic op. Built as
   a *deferred reply*: park the caller's request on the pane's next status
   transition (which `on_status` already computes), reply when it matches.

## 4. Recommended architecture — a layered core + thin skins

```
   ┌─────────────┐   ┌──────────────┐        skins (thin, swappable)
   │  roost CLI  │   │  roost-mcp   │
   │ (one-shot)  │   │ (stdio bridge)│
   └──────┬──────┘   └──────┬───────┘
          │  control token  │
          └────────┬────────┘
        ┌──────────▼───────────┐   CORE: the control protocol
        │  control socket       │   - ndjson request/response on the EXISTING socket
        │  (bidirectional)      │   - AppEvent::Command{req, reply}  → main loop
        └──────────┬───────────┘   - pane-addressed ops + wait registry
        ┌──────────▼───────────┐   - separate control credential (§5)
        │   single-threaded     │   - snapshot reads only in v1 (no passive stream)
        │   event loop (App)    │
        └───────────────────────┘
```

**Build the core once.** It is the request/response upgrade to the existing
socket, the pane-addressed ops, the `wait` registry, and the capability model.
Both skins are then a few hundred lines each of pure client code with no new
security model:

- **`roost <verb>` CLI** (tmux-style: `roost spawn`, `roost send-keys`,
  `roost capture-pane`, `roost wait`, `roost list`). Stateless, one-shot,
  reads the control token, connects, does one thing, exits. Best daemonless fit,
  auditable per-call, lowest capability barrier (LLMs run shell natively). The
  90% path.
- **`roost-mcp` stdio bridge** (separate process): exposes the verbs as MCP
  tools and panes/status as MCP resources, for LLM-native tool-calling. Talks to
  the same control socket; roost stays synchronous/tokio-free (rmcp's async
  weight lives in the bridge). roost can auto-write the agent's MCP config the
  same way it now auto-installs `roost.ts`.

**Two decisions baked into this shape, both siding with the adversary:**
- **Reads are snapshot-on-demand, not a passive stream, in v1.** A live output
  subscription (control-mode's `%output`) is a silent, continuous cross-pane
  keylogger — the single scariest capability and the hardest to consent per-byte.
  `read(pane, mode)` returns a bounded snapshot when asked. Streaming, if ever
  added, is opt-in and consented.
- **`wait` is a deferred reply, not an open-ended subscription.** This keeps the
  core request/response (dies with roost, no reattach) and avoids the
  persistent-subscriber machinery that would make roost a daemon in all but name.

## 5. Security & capability model (the actual hard part)

**The load-bearing fact:** `spawn_pane` injects both `ROOST_SOCK` *and*
`ROOST_TOKEN` into each child's environment, which the *entire process subtree*
inherits. So every semi-trusted, prompt-injectable agent already holds a valid
socket credential. Today that credential can do exactly one harmless thing —
report its *own* status. The instant a control verb honors that same token, every
injected agent gains: spawn-a-shell (**RCE**), `send-keys` into a sibling
(**cross-agent injection**), `capture-pane` a sibling (**secret exfiltration**),
`kill-pane` (**DoS**, bypassing the human busy-guard). This is the crown jewel to
protect.

**Non-negotiable constraints (any accepted design must meet all):**

1. **In-pane agents get *ownership-scoped* control by default; fleet-wide reach
   needs an explicit grant.** A pane may `spawn`/`fork` new panes and may
   `read`/`send`/`close` the panes *in its own spawned subtree* — but a pane it
   did **not** create (your other sessions, your shell) is off-limits unless the
   pane was launched with an explicit control grant. This is default scoping
   that makes self-orchestration ergonomic (an agent manages the workers it
   spawned, no ceremony) and serves as defense-in-depth — it is **not** a
   proven containment guarantee against a shell-capable pane (see the
   correction below). Track a `spawned_by: Option<PaneId>` parent pointer per
   pane; the capability check compares actor→target against subtree membership.
2. **Separate control credential from the status token.** A CSPRNG control token
   written to `<state>/control.token` at 0600, **never placed in any pane's
   environment.** External orchestrators / the human's CLI read the file (same
   trust boundary as the 0600 socket). The time-seeded `gen_token` fallback is
   *disqualifying* for a control secret — hard-fail instead.

   **Post-audit correction:** this stops *inheritance* and *other users*
   (0600), not the pane itself — pi and Claude Code both have a shell/exec
   tool, so a same-uid `cat <state>/control.token` hands any in-pane agent the
   fleet token. #1's subtree containment therefore does not hold against those
   agents: treat it as default scoping + defense-in-depth, not a security
   boundary. The boundary roost does enforce is **cross-UID** — 0600 inside an
   owner-verified 0700 dir stops other users, not other same-uid processes.
3. **Capability is per-verb, not per-principal.** A "set my status" credential
   must be structurally incapable of spawn/read/write/kill. Pane tokens are
   scoped to the pane's own subtree (see Open decisions §3 for the resolved
   in-pane trust model).
4. **Reports and commands do not share an authorization surface.**

   **[Amended 2026-08-19, DESIGN-ui.md C36 — a third actor.]** `Actor` gains
   `Local`: the human at the keyboard, reaching every pane exactly as `Fleet`
   does. It is **not an authorization change** — no token resolves to it,
   `TokenTable::resolve` never returns it, and it is unreachable from the
   socket. A chord pressed inside roost is already the strongest credential
   there is; the variant exists so the **audit trail can tell the two apart**.
   Logging a keyboard broadcast as `fleet` would make a human's action
   indistinguishable from a token holder's in `control.log` and the C20 feed,
   and attribution is the whole reason that log exists. Audited as `local`.
5. **Reads are scoped + consented.** Owner-created panes only; no in-pane read
   verb; no all-panes passive stream without explicit consent.
6. **Rate-limited + per-principal connection cap** (not just the global
   `MAX_CONN=64`, or one pane opening 64 connections starves the real
   orchestrator).

   **✅ Shipped** (`src/infra/sock.rs`, closing audit M3), **revised twice**
   after two security-review passes — the first caught six real gaps
   (findings H1–M3, L1 below); the second caught two more (C1/C2) in the
   *fixes themselves*: a shared pool sized to bound an attacker also bounds
   the victim, because sock.rs can't tell them apart, so two of the round-one
   mitigations had each quietly become a *cheaper* lockout than the bug they
   replaced. **Per-connection resources are the only ones an attacker can't
   share with a victim** — that's the constraint the design now follows
   wherever it can. All caps are keyed on the caller's raw wire token —
   sock.rs cannot resolve a token to an `Actor` (that needs
   `App::resolve_actor`, core/app.rs, a different file's lane); exact-string
   token identity is what `resolve_actor` itself keys on, so this loses
   nothing, and it beats peer credentials (`SO_PEERCRED`/`getpeereid`) for
   this job — in the same-uid threat model the uid never varies, and the pid
   is a fresh one-shot-CLI process every call, so a pid-keyed cap would never
   trip against the actual flood shape (a shell loop re-execing `roost
   <verb>`).

   **Connections — time-bounded pre-auth, then a per-principal pool:**
   - **Pre-auth: a wall-clock (2s) deadline from connection start, not a
     shared pool and not merely a read timeout.** *(Finding H1, revised as
     C1.)* The first cut only charged a connection once it sent a
     well-formed request — connect and never send anything held a full
     `MAX_CONN` slot for free, M3's original starvation. The immediate fix
     (a small *shared* pre-auth counter, capped at 16) was *worse*: 16 idle
     connections now permanently shed every new connection, including the
     human's CLI — cheaper than the 64 the original bug needed. Bounding by
     time instead of a shared counter was the right call — but a first
     implementation of that only set `SO_RCVTIMEO` (`set_read_timeout`) to
     2s and read lines with `read_until`, which bounds one `read()`
     *syscall*, not the connection: a client drip-feeding one byte slower
     than a full line but faster than the per-read timeout (repro: one byte
     every 1.2s against a 2s timeout accumulated for 9.66s before
     `read_until` returned) never trips it, and `read_until` loops
     internally with no wall-clock check of its own, so it just kept
     accumulating — charged nothing to `line_bucket`, never promoted, never
     timed out. Sixty-four of those still fill `MAX_CONN` for free; the
     starvation just moved from "connect and go silent" to "connect and
     drip." The fix (`sock.rs`'s `read_line_deadlined`) drives
     `fill_buf`/`consume` by hand instead of `read_until`, checking a real
     `Instant` recorded at connection-thread start against the 2s deadline
     on every iteration, independent of `SO_RCVTIMEO`. The per-read timeout
     is still set — it's what makes that loop tick on a genuinely silent
     connection rather than block forever inside one `read()` — but it no
     longer has to *be* the deadline. A pile of squatters, silent or
     dripping, now recycles within the 2s deadline of connecting either way.
   - **Pre-auth pool (16) with oldest-first displacement.** *(Finding from
     the 2026-08-20 reliability audit.)* The sentence that stood here —
     "nobody else is ever blocked from getting in" — was false, and
     measurably so. Recycling within 2s is not the same as never blocking:
     while 64 squatters hold the pool every other client is shed, so a
     legitimate `roost list` waits out the deadline. Measured against a real
     instance: 64 silent connections cost a legitimate client **2.07s and 21
     refused attempts**; a squatter that retook each slot as it recycled
     stretched that to **12.1s and 118 refused attempts**. The deadline
     bounds how long any *one* squatter is subsidised; it does not bound the
     denial, because reconnecting is free. Sustained, that was a
     control-plane denial of service available to any local process that can
     reach the socket — including a hostile process inside a pane, precisely
     the principal the per-pane tokens exist to contain. The tokens always
     held (nothing can be *driven* without one); what was deniable was access
     itself.

     The fix is two rules that only work together. **A connection that has
     not identified itself is evictable**, and at most `MAX_PENDING_CONN`
     (16) of `MAX_CONN` may be unidentified at once. Past that share — or
     with the pool full — an arrival displaces the OLDEST unpromoted
     connection and takes its slot, rather than being shed. A cap alone
     would repeat this file's own recorded mistake (16 idle connections
     permanently shedding every arrival, cheaper than the bug it replaced);
     displacement alone would leave squatters holding 64 threads. Together:
     a caller about to identify itself always gets in, because a legitimate
     client is the oldest-unpromoted for the microseconds it takes to write
     its request, while a squatter is by definition always older.
     **Promotion makes a connection un-displaceable** — work in flight, and
     parked `wait`s especially, are never sacrificed for a stranger.

     `Limits.pending` holds a `try_clone` of each unpromoted socket purely so
     the accept thread can `shutdown` it, which unblocks the victim's `read`
     at once instead of waiting out `PRE_AUTH_READ_TIMEOUT`; an
     `Arc<AtomicBool>` per connection is claimed with a `swap(false)` by
     whichever of eviction and `ConnGuard::drop` gets there first, so the
     slot is released exactly once. Displacement releases the slot
     synchronously, making it a transfer rather than a race.

     Same measurement after the fix: **27ms, first attempt**, under the
     sustained flood. Gated end to end by `tests/control_plane_squatters.rs`.

     What this does *not* do: distinguish a squatter from a first-time
     legitimate client before either has authenticated. Nothing can —
     same-uid peer credentials do not help, because a hostile pane has them
     too. A flood still costs arriving clients an extra connect attempt now
     and then. The denial is bounded, not abolished.
   - **Per-principal pool, 20**, alongside the unchanged global 64 — a
     connection is promoted here, and onto the generous `READ_TIMEOUT` (30s),
     the instant it sends its first well-formed request. Must be at least
     `app.rs`'s `MAX_WAITS` (16): each parked `wait` holds one connection
     open for its deferred reply, so a lower cap would refuse a fleet-wide
     wait the rest of the system already considers and bounds — an early
     version shipped at 8 and would have refused the documented 16-wait
     fleet outright (finding M3). 20 leaves headroom for a few other
     commands alongside a fully-parked wait set.
   - The old global-cap check-then-increment (load-then-add, audit Info (a))
     is race-free (atomic `fetch_update`).

   **Commands — a per-*connection* line charge, then a per-*admitted*-
   principal bucket, then the shared aggregate only on genuine dispatch:**
   - **Line bucket, 128 capacity / 20 per second, per connection — plain
     local state, no lock, no shared map.** *(Finding H1(a)/M2, revised as
     C2.)* The first cut charged every line against the *shared* aggregate
     bucket before parsing it, so it cost something even when never
     dispatched — but one connection spewing blank lines emptied that shared
     bucket in milliseconds and held every *other* connection at `rate
     limited`, cheaper than the M3 flood it replaced (and it serialized all
     control traffic on the aggregate's mutex besides). Moving the per-line
     charge to a bucket only this connection can touch means a flood's cost
     lands on the flooder, never on a caller sharing the socket with it.
   - **Per-principal bucket, 64 capacity / 5 per second**, charged only for
     a well-formed request that already passed its connection's line bucket
     — re-derived from a *real* 20-pane fleet (`spawn`×20 + `wait`×20 = 40
     commands), not the 10-pane hello-world §7 illustrates for brevity; an
     early version sized this from the smaller example and would have
     throttled ~8 calls of a real fan-out with no retry anywhere in
     `cli.rs` (finding M3) — worse than the bug it fixed. Keyed on the
     connection's *admitted* identity — the token its first request
     succeeded with — never a raw per-request token (finding H2): the first
     cut kept a bucket per `req.token`, so a connection could vary the token
     field on every subsequent request and mint an endless supply of fresh,
     full buckets, defeating per-principal rate limiting entirely from
     inside one already-open connection. An in-body token that never
     admitted a connection therefore never becomes a bucket map key at all.
   - **Aggregate bucket, 256 capacity / 20 per second**, charged only once a
     request has *already* passed its connection's line bucket and its
     per-principal bucket and is genuinely about to be dispatched — never
     for raw line volume, which is now bounded per-connection above. Still
     bounds total throughput regardless of how many admitted identities a
     flood claims (finding H2), at a cost now roughly proportional to what
     the attacker is genuinely spending, since reaching this check at all
     requires already having paid a real per-principal cost.
   - The bucket map is bounded at 512 distinct identities (`MAX_TRACKED_TOKENS`),
     evicting the fullest entry (idle long enough to have refilled — carries
     no enforcement state, so it's free to drop) to make room for a new one;
     an actively-throttled principal's drained bucket is never the one
     dropped out from under it. Tokens over 128 bytes are rejected outright
     in `parse_control` (finding M1): the first cut's eviction scan cloned
     every key under the map's mutex on the hot path, so an attacker-chosen
     64 KiB token made that scan tens of MiB of alloc+memcpy per request,
     run *before* the aggregate check.
   - A panic partway through a connection (including from a poisoned mutex —
     the accounting mutexes now recover from poisoning rather than
     propagate it) is handled by an RAII guard, so it can't leak a slot
     forever (finding L1).

   Any rejection is a distinct, actionable `err` reply — `"connection
   limit: ..."` / `"rate limited: ..."` — never a silent drop and never a
   hang (both were real bugs in this file; see `parse_control`), worded not
   to collide with `"unauthorized: ..."` or a malformed-request error.

   **Closed (promotion-auth-gate):** the gap described above — sock.rs
   promoting a connection past pre-auth on grammar alone, at either of the
   two doors — is fixed. sock.rs now holds a `TokenReader` (a read-only
   handle onto App's token table, `core/control.rs`'s `TokenTable`/
   `TokenSnapshot`; App remains the sole writer) and consults it
   synchronously, before promotion, at both doors: a control request
   promotes only if its token is the fleet token or some pane's own
   (`TokenSnapshot::is_principal`); a status/session line promotes only if
   its token matches *that exact pane's* own token
   (`TokenSnapshot::pane_authorized` — the same predicate App's
   `socket_authorized` uses at dispatch, so the two can't drift). Either
   door's failure means no promotion, no dispatch: a garbage-token control
   request gets the exact reply App would have given
   (`UNAUTHORIZED_MSG`, a shared `pub const`) and dies at the 2s pre-auth
   deadline instead of holding a slot for 30s; a garbage-token status line
   is dropped silently (no reply — the wire shape has none) with no link-up
   and no forward. Authenticated reporter connections additionally take a
   new **per-pane cap of 8** (`Limits.reporters` in sock.rs, mirroring
   `try_reserve_principal`'s accounting, released 1:1 with `link_panes`
   entries at `ConnGuard` drop) — a separate pool from `PRINCIPAL_MAX_CONN`,
   so a pane's own status links can't starve its control budget or vice
   versa. What this does *not* fix, by design: an **authenticated** hostile
   pane still gets its own capped share (8 reporter + 20 control
   connections) — the gate bounds squatting to authenticated, capped
   identities, it does not make a compromised pane harmless. Any same-user
   process that can read `<state>/control.token` remains fully trusted — the
   0600 file/dir is the boundary, unchanged.

   **Audit finding H3 correction (the arithmetic, not the cap numbers):**
   `app.rs`'s audit-log write is bounded by *bytes* attacker-controlled
   fields contribute (`sanitize`, app.rs, does not truncate — **NEED**: fix
   there, out of this file's lane), not by call count. With `MAX_LINE` (64
   KiB) against a 4 MiB log kept at one generation, as few as ~140 *allowed*
   requests roll it twice and erase everything — not the ~200k an earlier
   version of this section assumed. At the aggregate bucket's 20/s that's
   ~7s; at one principal's 5/s alone it's ~22s. The command buckets above
   slow this flood from "as fast as the wire allows" to that; they do not
   make the log-rotation attack impractical by themselves the way an
   earlier draft of this section claimed.
7. **Graceful at 0 instances; defined addressing at N.** Absent socket → clean
   no-op. (v1 scopes to one instance; multi-instance discovery is a non-goal.)
8. **Preserve single-owner + daemonless.** Commands marshal through the mpsc onto
   the one loop; replies via non-blocking one-shot (never a blocking send from
   main); no detach/reattach; server dies with the process.
9. **Unconditional server-side audit log** of every control action (principal +
   verb + target). Destructive verbs honor a consent/`force` semantic equivalent
   to the interactive `confirm_close`.

**Ownership-scoped control (the default that serves the real workflows).** The
two motivating workflows — "fork a pane from my current pi session" and "let pi
spin worker panes when it uses sub-agents" — are both *create a child in my own
subtree*, so both are allowed with **no grant**:

- `fork(pane)` → spawn a sibling running the same adapter+cwd, resuming a *fork*
  of that pane's session (pi/Claude can branch a session; roost launches the
  pane on the new id). The new pane's `spawned_by` = the forking pane.
- `spawn(...)` from an agent → a worker pane in the caller's subtree, which the
  caller may then `send`/`read`/`wait`/`close`.

Why this is safe *enough* for a coding agent: pi and Claude Code **already have a
bash/exec tool**, so "spawn a shell pane and type into it" grants no capability
they lack — they can already run commands in their own pane. The genuinely *new*
risk of pane control is therefore **cross-pane reach into panes the agent didn't
create** (reading your other session's screen, injecting into an unrelated
agent). Ownership scoping *narrows* this by default, but does **not** contain a
shell-capable injected agent: because the fleet token is a same-uid-readable file
(see the §5.2 correction), such an agent can read it and act as `Fleet` on any
pane. So cross-pane read/inject is a **residual risk you accept** when you run a
semi-trusted agent in roost — bounded by cross-uid isolation and recorded in full
by the audit log, not prevented. Ownership scoping remains the default
behavior/attribution and defense-in-depth against a *non-shell* principal.

**Fleet-wide grant** (`roost spawn --grant control`) is the opt-in escalation for
a genuine supervisor that must reach panes it didn't spawn. **Fork-bomb guard**
(orthogonal to ownership): a workspace pane budget + a recursion-depth counter
(`ROOST_FLEET_DEPTH`, refuse spawn past N) so an agent — malicious or just
looping — can't exponentially fan out. The control path rides the pi extension,
which already holds `ROOST_SOCK`+`ROOST_TOKEN` — it becomes **bidirectional**
(today it only reports status; it gains scoped spawn/fork/send verbs).

## 6. The killer capability (why this beats "just spawn subprocesses")

roost is the only orchestrator where the spawned agents are simultaneously:

1. **Programmable** — the LLM drives them via this interface.
2. **Watchable** — every one is a live pane in a stacked fleet dashboard with
   status badges; a subprocess pool is an invisible black box.
3. **Human-seizable** — because each pane is a real PTY, the human can take over
   *any* agent the LLM spawned (answer its prompt, correct it), then hand it
   back. The same worker is API-driven *and* human-operable. No subprocess
   orchestrator can offer this. **This is the moat.**
4. **Reboot-durable** — the fleet persists `(layout × session-id)` and resumes
   across the orchestrator dying and the machine rebooting.

Plus **exact status**, not spinner-scraping: `wait(until=waiting)` replaces the
sleep-and-grep-`capture-pane` loops every tmux-orchestrator reinvents.

## 7. MVP operation set (the irreducible seven)

| Op | Returns | Backs onto |
|---|---|---|
| `spawn(adapter, cwd?, initial_input?, tab?)` | `pane_id` | `new_pane_with` (+ return id, + type initial prompt) |
| `fork(pane_id?)` | `pane_id` | spawn a sibling resuming a *fork* of the pane's session (self-orchestration workflow #1) |
| `send_input(pane_id, text, submit?)` | ok | **new** `send_input_to` (the focus-relative gap) |
| `read(pane_id, mode=screen\|tail:N\|full)` | text | `grab_text` (+ scrollback); default `screen` |
| `status(pane_id?)` | enum | `AgentStatus`/`on_status` |
| `wait(pane_ids, until, timeout)` | `{id: status}` | **new** deferred-reply on status transitions |
| `list()` | pane records | `pane_order`+`find_spec`+`tab_summary` |
| `close(pane_id, force?)` | ok | `close_pane_by_id`; `force` replaces the human confirm |

**Hello world the interface must make trivial:**
```
p = spawn(adapter="pi", cwd="~/code/api", initial_input="run the tests, report pass/fail")
wait([p], until="waiting", timeout=300)
print(read(p, mode="tail:20"))
```
Spawn an agent on a task → block until done → read its answer. The fan-out
flagship is that in a loop + `wait(all)` + `read` each. And while `wait` blocks,
the human can `Alt+→` into the pane and drive it by hand — the whole pitch.

**Deliberately NOT exposed to the LLM:** resize, focus-move, copy-mode, scroll,
flip-split, URL-open — human ergonomics an orchestrator never needs. Don't
reflexively expose the whole `Action` enum.

## 8. Phased implementation plan

- **Phase 0 — core plumbing (no user-facing verbs yet).** Add the reply path
  (`AppEvent::Command{req, reply}`); pane-addressed ops (`send_input_to`, `spawn`
  returning id, `read_pane`, `close_pane_by_id`); CSPRNG control-token issuance to
  `<state>/control.token` (0600); pane-token scoping to the pane's own subtree
  (resolved per Open decisions §3). *This is the load-bearing refactor* (the focus-relative →
  pane-addressed conversion the earlier code reviews already flagged).
- **Phase 1 — MVP verbs + the CLI skin.** ✅ Done. The verbs over the socket
  (`list`/`status`/`spawn`/`fork`/`send`/`read`/`close`); `roost <verb>` one-shot
  CLI reading `control.token`; plus `wait` (deferred reply) pulled forward.
- **Phase 2 — MCP bridge.** ❌ **Descoped.** The CLI is the chosen interface —
  it's the safest, most auditable skin and LLMs drive it natively via shell, so
  a second (MCP) surface isn't worth the added attack surface + tokio/rmcp
  weight. Revisit only if a use-case genuinely needs native tool-calling.
- **Phase 3 — advanced (optional, not planned).** Consented event subscription;
  a real session-branching `fork` via a bidirectional pi extension; semantic
  `read(last_turn)`; HTTP transport; multi-instance discovery.

**Interface status: complete via the CLI.** Security constraints §5 mostly met,
with one downgrade: ownership scope (#1) turned out to be default behavior +
defense-in-depth, not a proven containment boundary, once a same-uid file read
is in scope (see the §5.2 correction) — the boundary actually enforced is
cross-UID. Otherwise met: per-verb capability (#3–4), owner-scoped reads (#5),
0600 socket + off-env control token (#2), CSPRNG control token that hard-fails
rather than falling back (#10), an unconditional audit log at
`<state>/control.log` — principal + verb + target + outcome, never the message
text (#9) — and, since then, a per-principal connection cap + command rate
limit (#6, beyond the global 64; see #6 above for the shipped shape). Still
open: a human-consent gate on reads (#5). Remaining overall is a
live-terminal smoke test and, if ever wanted, the Phase 3 niceties above.

## 9. The three paradigms, compared

| Axis | CLI (one-shot) | Socket control-mode | MCP server |
|---|---|---|---|
| LLM ergonomics | native (runs shell) | needs a held connection | native (tool-calling) |
| `wait`/push | weak (blocks a process each) | strong (streams) | strong (resources/notify) |
| Daemonless fit | **best** (stateless) | worst (persistent subscriber) | adds a helper process |
| Security (adversary rank) | **1st** | 3rd (passive keylogger) | 2nd (external client, but façade) |
| New surface | smallest | medium | largest (tokio/rmcp) |
| Discovery | `$ROOST_SOCK` in-pane; friction for N | same | elegant via env inheritance |

**Why layered wins:** the CLI is the safest, simplest default and the best
daemonless fit; MCP is the most LLM-native; both are thin clients of one core
credential+protocol. Build the core, ship the CLI first, add the MCP bridge on
top — don't build two security models, and don't ship the passive output stream
that makes control-mode the adversary's lowest-ranked option.

## 10. Principal risks

- **Privilege escalation via the inherited token** — §5.2/5.3 (separate,
  off-env, per-verb control credential) mitigate the *inherited*-token and
  cross-UID vectors, but not a same-uid `cat` of the token file by a
  shell-capable pane; that path is accepted, not fixed (see the §5.2
  correction). Cross-UID is the boundary that actually holds.
- **Fork-bomb / recursion** in self-referential orchestration — leaf tokens +
  pane budget + depth counter.
- **Command flood stalling the render loop** — ✅ closed, including
  status-socket traffic (an earlier version of this fix didn't — see #6
  above, finding M2): a per-line aggregate charge, a per-*admitted*-principal
  command bucket, and connection caps (§5 constraint #6, `sock.rs`); the
  bounded channel already prevented OOM regardless. **Open gap** (see #6's
  residual-gap paragraph): sock.rs can't validate a token before promoting a
  connection, so a caller minting a fresh *admitted connection* per garbage
  token (not merely a fresh per-request token — see #6's H2 note) can fill
  the entire global connection pool with them and hold it for the full 30s
  `READ_TIMEOUT` each, on repeat — a connection-count/duration problem the
  command-rate buckets don't reach. Closing it needs `Actor` resolution in
  `app.rs`, out of sock.rs's reach — see #6 above.
- **Secret exfiltration via reads** — owner-scoped, snapshot-only, consented.
- **Destructive verbs bypassing the human busy-guard** — `force` semantics +
  default-deny self/last-pane close over the API.
- **`rmcp`/tokio weight** vs roost's "10% of a muxer's surface" ethos — isolated
  in the bridge process, not roost's core.

---

## Open decisions (yours to make before Phase 0)

1. **Primary consumer?** In-pane self-orchestration (the moat, but the biggest
   security lift) / external orchestrator / human workspace-assistant. The ops
   are shared; this sets emphasis and how aggressively to gate in-pane control.
2. **Transport priority?** CLI-first (recommended: simplest, safest, most
   auditable) vs MCP-first (most LLM-native). The core is shared either way.
3. **In-pane trust?** *Resolved (post-audit):* ownership scoping is the default
   *behavior* — a pane freely spawns/forks and drives its own subtree, audited as
   that pane — but it is **not** a security boundary: a shell-capable pane can read
   the fleet token file and reach any pane, so in-pane control is effectively
   fleet-wide. Treat every in-pane agent as fully control-capable; the enforced
   boundary is cross-uid, and the audit log is the accountability mechanism. (The
   `--grant control` escalation is moot under this posture and is unimplemented.)
4. **Read policy?** Snapshot-on-demand only (recommended) vs allow the passive
   output stream (powerful, but the adversary's top risk).
5. **Scope?** Single-instance for v1 (recommended) vs multi-instance from the
   start.
