# roost control interface — design synthesis

*A programmatic interface through which LLMs (and humans, and scripts) can
manage/control roost. Synthesized from a five-member design tribunal
(CLI-champion, socket-RPC-champion, MCP-champion, security/architecture
adversary, product/use-case lens).*

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
   pane was launched with an explicit control grant. This is the boundary that
   makes self-orchestration ergonomic (an agent manages the workers it spawned,
   no ceremony) while still containing a prompt-injected pane to its own subtree.
   Track a `spawned_by: Option<PaneId>` parent pointer per pane; the capability
   check compares actor→target against subtree membership.
2. **Separate control credential from the status token.** A CSPRNG control token
   written to `<state>/control.token` at 0600, **never placed in any pane's
   environment.** External orchestrators / the human's CLI read the file (same
   trust boundary as the 0600 socket). The time-seeded `gen_token` fallback is
   *disqualifying* for a control secret — hard-fail instead.
3. **Capability is per-verb, not per-principal.** A "set my status" credential
   must be structurally incapable of spawn/read/write/kill. Control verbs are
   rejected from *any* principal presenting a pane token, even a valid one.
4. **Reports and commands do not share an authorization surface.**
5. **Reads are scoped + consented.** Owner-created panes only; no in-pane read
   verb; no all-panes passive stream without explicit consent.
6. **Rate-limited + per-principal connection cap** (not just the global
   `MAX_CONN=64`, or one pane opening 64 connections starves the real
   orchestrator).
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
agent). Ownership scoping removes exactly that: an injected worker can still spawn
and drive *its own* children, but cannot read the pane where you pasted a key or
hijack a sibling it doesn't own.

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
  `<state>/control.token` (0600); the capability check that rejects pane tokens
  from control verbs. *This is the load-bearing refactor* (the focus-relative →
  pane-addressed conversion the earlier code reviews already flagged).
- **Phase 1 — MVP verbs + the CLI skin.** The irreducible seven over the socket;
  `roost <verb>` one-shot CLI reading `control.token`; audit log; `wait` registry.
  Ships the hello-world and the fan-out flagship.
- **Phase 2 — MCP bridge.** `roost-mcp` stdio bridge exposing tools + resources;
  auto-written MCP config; in-pane agents get it only via explicit `--grant`.
- **Phase 3 — advanced (optional).** Consented event subscription; semantic
  `read(last_turn)` via the extension (token-efficient, not screen-scraping);
  HTTP transport for external orchestrators; multi-instance discovery.

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

- **Privilege escalation via the inherited token** — mitigated by §5.2/5.3
  (separate, off-env, per-verb control credential). This is the one that matters.
- **Fork-bomb / recursion** in self-referential orchestration — leaf tokens +
  pane budget + depth counter.
- **Command flood stalling the render loop** — per-connection token bucket +
  per-principal connection cap; the bounded channel already prevents OOM.
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
3. **In-pane trust?** *Resolved:* ownership-scoped control by default (a pane
   freely spawns/forks and drives its own subtree; fleet-wide reach needs
   `--grant control`). Enables the fork + sub-agent workflows with no ceremony
   while containing an injected pane to its own subtree.
4. **Read policy?** Snapshot-on-demand only (recommended) vs allow the passive
   output stream (powerful, but the adversary's top risk).
5. **Scope?** Single-instance for v1 (recommended) vs multi-instance from the
   start.
