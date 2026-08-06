# PLAN.md — the living phased plan

The single ledger for the best-in-class engagement. Every issue, gap, and
UI/UX gap found lands here with a phase assignment; nothing leaves except by
shipping (✅ + PR#) or descoping (❌ + rationale). Newest findings get
appended to their phase with a `[src]` tag naming which lens found them.

Sources: `[roadmap]` ROADMAP.md · `[ux]` ux-expert audit · `[rev]` reviewer
sweep · `[sec]` security audit · `[qa]` black-box QA · `[res]` competitive
research · `[principal]` found during the work.

Status legend: ☐ open · 🔨 in progress · ✅ shipped (PR#) · ❌ descoped.

---

## Phase 0 — Baseline & housekeeping

- ✅ Sync main, delete 5 stale local + 5 stale remote branches. [principal]
- ✅ Baseline `cargo test` green on host: 553 unit + 24 integration.
- ✅ Engagement scaffolding merged (PR #31).

## Phase 1 — Correctness & security (P0/P1 from audits)

**P0 — the control plane wedges permanently** [rev, handoffs/code-review.md]:

- ☐ **Malformed control request gets NO reply; client hangs forever.**
  `parse_control` (control.rs:110–114) returns None on any deser failure,
  falls through `parse_line` into `log_dropped`, writes nothing back;
  `cli.rs:222` read_line has no timeout. Reproduced: missing token, unknown
  method, missing text, bad pane type — all hang. 64 of them (MAX_CONN) park
  64 threads forever and the next legit client is shed with BrokenPipe →
  control plane dead until restart. An agent retrying a typo'd verb reaches
  64 in seconds. Fix: reply `{"err":…}` whenever a line has a `method` key
  but fails to deserialize; add `set_read_timeout(30s)`; log the shed
  (sock.rs:148–149, 176–202). [rev P0]
- ☐ **A terminal resize can kill the entire fleet.** ratatui-core 0.1.2
  changed `Terminal::clear()` to query the cursor (`ESC[6n`, blocks up to
  2 s); arrived via patch bump under the `ratatui = "0.30"` pin. On a host
  that doesn't answer DSR (nested muxes, some CI/serial terminals, dropped
  ssh mid-resize) `terminal.clear()?` at main.rs:346 returns Err → `run()`
  Err → `shutdown()` SIGKILLs every pane. Reproduced. Fix: don't propagate a
  cosmetic clear; count consecutive draw/size failures and bail only after
  N. [rev P1-1, treated as P0 — it destroys user work]
- ☐ **Socket listener failure silently swallowed** (main.rs:238 `.ok()`) —
  roost looks normal with no control plane at all: no $ROOST_SOCK in panes,
  extension/hooks never report status, no audit log, every `roost <verb>`
  fails. Hit accidentally with a 112-char state dir (macOS sun_path limit
  104). Fix: flash the error; keep the `dir_is_private_and_ours` security
  refusal fatal. [rev P1-2]
- ☐ OSC 52 clipboard relay is size-capped but not **rate**-capped
  (pty.rs:342–348) — one shell loop relayed 5000 sequences / 129 KB to the
  operator's real clipboard. Give it the per-pane interval gate OSC 9
  already has (pty.rs:365). [rev P2-3]
- ☐ P3 hardening trio: `workspace.rs:68,72` `len()-1` underflow class (pub
  `active_tab()`, ~40 callers) → `.get().or(first())`; `main.rs:188` panic
  hook restores the terminal from *any* thread while main keeps drawing;
  `pty.rs:595` `kill(-pid)` needs `if pid > 1`. [rev P3-4/5/6]

Control-CLI contract fixes — an orchestrator must be able to trust what
roost tells it [ux, handoffs/ux-audit.md]:

- ☐ **`roost spawn` returns ok for a failed spawn** — PTY failure lands
  async in `App::dead`, invisible to the socket; status says only
  `exited`, orchestrators retry forever (app.rs:1593–1607, 916–918).
  Make the failure readable: reason in `status`/reply + feed. [ux P0-2]
- ☐ **`wait` timeout exits 0** — `{timed_out:true}` printed, exit 0;
  `wait … && read` proceeds as if done. Nonzero exit on timeout
  (app.rs:1519, cli.rs:60–62). [ux P1-7]
- ☐ **Unknown first arg launches the full-screen TUI** — `roost --version`,
  `roost ls`, typos seize the terminal; `--help` prints to stderr. Hard
  error on unknown verbs; help/version on stdout (cli.rs:27–32). [ux P1-8]
- ☐ **Unknown flags silently dropped; no `--` separator** — `send 5 hi
  --entr` sends without Enter and replies ok; `send 5 "--- x ---"` sends
  empty. Reject unknown flags, add `--` (cli.rs:186–204). [ux P1-9]

Security fixes — verdict is **fix-first**, minimum-to-ship is H1+M1+M3
[sec, handoffs/security-audit.md]:

- ☐ **H1 (High): pane cwd/title escapes into the operator's real terminal.**
  `sync_host_title` (app.rs:1934–1936) writes `ESC]2;` + label + BEL straight
  to stdout, bypassing ratatui's control-char filter; the label is
  `spec.title` or `cwd.file_name()` **verbatim**. An in-pane agent mkdirs a
  directory whose name carries OSC 52 and spawns there → clipboard poisoning
  / title spoof on the human's terminal. Fix: `sanitize_title` at that
  boundary. [sec H1]
- ☐ **M1: the "private" scratch float is readable/writable over the control
  plane.** `ctl_close`/`ctl_broadcast` exclude the float; `ctl_read`/
  `ctl_send` don't (app.rs:1643, 1716, 1114). Any token-reading agent can
  `roost read` the human's private scratch shell. Fix: same `is_float`
  refusal. Also README:311 wrongly claims broadcast reaches the float. [sec M1]
- ☐ **M3: no per-principal connection/rate cap** — one pane opens all 64
  conns (or 16 waits) and locks out the human + the real orchestrator; it is
  also the enabler for M2's audit-log rolling. Per-token conn cap (~8) +
  token bucket on resolved Actor (sock.rs:148, app.rs:224). [sec M3,
  supersedes the roadmap "choice" entry]
- ☐ **L2: control-token file mode not reset on reuse** — `.mode(0o600)`
  applies at creation only; a crash-left token keeps its old mode
  (main.rs:448). [sec L2]
- ☐ **L3: pane token/socket env leak when the listener fails to bind** —
  panes then inherit roost's own env, so a nested roost hands children the
  *outer* live token (main.rs:238, app.rs:4498). `env_remove`
  unconditionally. [sec L3]
- ☐ **M2: audit log is not tamper-evident** against a same-uid pane
  (truncate, or flood to force double rotation). Fix M3 first, then either
  an out-of-process sink (`ROOST_AUDIT_FD`/syslog) or — minimum — document
  the log as advisory against same-uid. [sec M2]
- ☐ **Info-c: DESIGN-control.md §5.3 + §8 checklist are stale** — they claim
  control verbs are rejected from pane tokens; code accepts them
  (app.rs:1243), superseded by §Open-decisions-3. Doc fix. [sec Info]
- ☐ notify.rs:17 AppleScript interpolation is quoting-by-luck (no break-out
  found); pass the body as an `on run {b}` argument. [sec Info-b]
- ☐ L1: control spawn splits the *human's* active tab rather than the
  caller's context (app.rs:2884) — layout churn under the human's hands.
  [sec L1, phase 3 candidate]

## Phase 2 — UX quick wins

- ☐ **Spawn-failure error drops the why** — anyhow Display keeps the outer
  context only; ENOENT/EACCES discarded one call before display. `{:#}`
  chain format (pty.rs:411, app.rs:917). [roadmap + ux P0-3]
- ☐ **macOS Option-trap hint mistimed** — 8s launch-anchored window
  false-fires on healthy terminals (and C11-hides the hint row on first
  keystroke) while missing read-first users entirely. Re-anchor the
  trigger to evidence, not launch time (app.rs:39, :4869). [ux P0-4]
- ☐ **README states the detach cost honestly** + sanctioned run-under-tmux
  recipe for SSH; extend busy-quit confirm to the hangup path
  (app.rs:3059). [ux P0-1]
- ☐ Picker offers adapters not on PATH → guaranteed dead pane; annotate or
  filter by `which` (app.rs:41). [ux P2-13]
- ☐ Help overlay never mentions the control CLI (`roost send <id>`) — the
  category differentiator is invisible in-product (render.rs:680–747).
  [ux P2-15]
- ☐ "copy failed" flash names neither cause nor next step (app.rs:2164).
  [ux P3-17]
- ☐ Room-exhaustion flashes should point at Alt+s/stacks as the exit.
  [ux P3-18]
- ☐ <2-row terminals render blank; show a "too small" notice
  (render.rs:24–26). [ux P3-16]

## Phase 3 — Fleet & robustness UX (promoted deferred items)

- ☐ **Attention works on default setup** (the #1 switch bet): auto-install
  Claude Code hooks the way pi's extension is auto-installed + let the
  Alt+a ring fall through to Waiting-after-Working when no ◆ exists
  (status.rs:176–187). [ux P1-5, promoted]
- ☐ **Relay the BEL** — heuristic ◆ transitions never reach the host bell;
  README's "rings the bell" is false in the exact fallback case it serves
  (ports.rs:59–62, main.rs:366,385). [ux P1-6]
- ☐ **Roster urgency ordering** — sort worst-first, optional status filter;
  turns Alt+Shift+a into the fleet dashboard at zero column cost
  (app.rs:3481–3487). [ux P2-11, top-5 bet]
- ☐ · idle vs ○ waiting collapses at first byte — every shell at a prompt
  reads "your turn"; rethink Idle survival (status.rs:176–186). [ux P2-10]
- ☐ N2/N4 silent no-ops (Alt+o outside split; Alt+←/→ in stack) vs the
  every-no-op-flashes rule. [ux P2-14, SPEC-ux]
- ☐ Dead-pane `Enter` retry: distinguish transient vs permanent — do AFTER
  the Phase-2 error-cause fix makes that decidable. [roadmap + ux]
- ☐ Per-principal connection/rate cap on the control socket. [roadmap:
  choice; security verdict pending]
- ☐ P21's dump-to-editor half (copy-mode scrollback → $EDITOR) — assess
  promotion. [principal, SPEC-parity P21]
- ☐ **DECISION FORK: minimal config file.** Zero-config is stated
  philosophy, but monochrome ●/◆ ambiguity (no motion opt-out) and the
  Alt+f/b/d readline collisions have no escape hatch without one. Consult
  cos + advisor before Phase 3 build; log in DECISIONS.md. [ux P2-12]

## Phase 4 — Category-best differentiators

Research verdict [res]: roost's send/wait race-safety and no-daemon
resurrection are already category-leading; what's missing is reach
(install, adapters) not architecture. Full report: handoffs/research.md.

- ☐ **Distribution — the #1 gap.** Zero install channels today (no releases,
  no tap, no crates.io, no binstall). Every peer ships 3+. Ship: GitHub
  Release binaries (mac arm64/x64 + linux x64/arm64) via release workflow,
  Homebrew tap, cargo-binstall metadata; consider crates.io. [res: HIGH]
- ☐ **codex CLI adapter** — `codex resume <id>`, JSONL sessions under
  `~/.codex/sessions/YYYY/MM/DD/`. Cleanest, highest-payoff new adapter. [res: HIGH]
- ☐ **gemini CLI adapter** — `gemini --resume <uuid>`, project-scoped
  history. [res: HIGH]
- ☐ **opencode adapter** — `--session <id>`; defend against sst/opencode#2086
  (`--continue` can grab a subagent thread — always resume by explicit id). [res: MED]
- ☐ **`roost spawn --worktree` (opt-in)** — create/enter a git worktree per
  pane; neutralizes claude-squad's headline differentiator without lifecycle
  management. Claude adapter already cwd-scopes its session files, so
  worktree = clean session namespace for free. [res: MED-HIGH]
- ☐ **README: state the send/wait no-race guarantee as a named edge** —
  claude-squad's most-commented open bug (prompt sent before CLI ready,
  lost) is the exact failure roost's status-socket + `wait` prevent; say so
  with the receipts. [res: MED, docs-only]
- ☐ Real session-branching `fork` (needs pi extension to go bidirectional;
  scope may be pi-side — assess). [roadmap: gap]
- ❌ Persistent fleet rail (projects → agents) — STAYS PARKED per ux-expert
  verdict: the missing thing is urgency ordering *inside the roster* (zero
  columns, Phase 3), not a second spatial axis; free-Alt pool is ~empty.
  Full design notes remain in ROADMAP for a future round. [roadmap + ux]
- ☐ **DECIDE (not auto-promoted): thin opt-in read-only remote check-in**
  (`roost serve`-shape, "did my agent finish" from a phone). Category
  normalized (Zellij web client, VibeTunnel, Cowork handoff) but collides
  with the standing HTTP-transport descope and §5.5 consent posture —
  needs advisor + security assessment before any build. [res: MED]
- ❌ amp adapter — thread-based, breaks the (adapter,cwd,session-id) model;
  do only after the easy three prove the pattern. [res]
- ❌ aider adapter — no session-id resume mechanism; would be heuristic-only
  like shell. Revisit if demand shows. [res]
- ❌ Synchronized panes / session groups / plugin-WASM extensibility —
  classic-multiplexer table stakes not demanded by fleet users. [res: LOW]

## Phase 5 — Health & performance

- ☐ Dependency inversion: `core` imports `ui::Action` + raw crossterm key
  types; arrow should point ui → core. [roadmap: health]
- ☐ Extract `SessionResolver` from app.rs private methods. [roadmap: health]
- ☐ Scope pi `session_state` walk to the cwd (O(all sessions) per spawn
  today). [roadmap: perf]
- ☐ PTY read coalescing (64 KiB per-read batching). [roadmap: perf]
- ☐ Golden-frame *color* scenarios for the vt100 harness — trigger was
  "next engagement leaning on src/ui/render.rs", which this engagement
  likely is. [roadmap]

## Descoped (standing, from ROADMAP)

- ❌ MCP bridge — CLI is the interface; second surface not worth the attack
  surface. Revisit only on concrete demand.
- ❌ Event subscription / live output stream — adversary-ranked worst
  (cross-pane keylogger shape). Reads stay snapshot-on-demand.
- ❌ HTTP transport, multi-instance discovery, semantic `read(last_turn)`.

## Exit criteria

- All P0/P1 shipped or descoped with rationale; P2 substantially shipped.
- Verify wave clean: cargo test, clippy, qa-verifier acceptance pass,
  design-supervisor 100% ALIGNED, reviewer + security sign-off on touched
  surfaces.
- ROADMAP.md updated to remain the public truth; final report delivered
  with COS-advised calls (if any) listed for ratification.
