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

- ✅ Sync main, delete 5 stale local + 4 stale remote branches. [principal]
- 🔨 Baseline `cargo test` on this host — confirm green before any change.
- ☐ Commit engagement scaffolding (openspec/), PR, merge.

## Phase 1 — Correctness & security (P0/P1 from audits)

*Placeholder — populated when reviewer + security-auditor reports land.*

## Phase 2 — UX quick wins

- ☐ Spawn-failure error says nothing about *why* (ENOENT vs PATH vs perm).
  Surface the cause in the dead-pane hint. [roadmap]
- *More on ux-expert + qa reports.*

## Phase 3 — Fleet & robustness UX (promoted deferred items)

- ☐ P21's dump-to-editor half (copy-mode scrollback → $EDITOR) — the one
  SPEC-parity sub-item deliberately left undone; assess promotion.
  [principal, SPEC-parity P21]

- ☐ Dead-pane `Enter` retry re-runs the same resume command even after a
  permanent failure; distinguish transient vs permanent. [roadmap: choice]
- ☐ Per-principal connection/rate cap on the control socket (global 64-conn
  cap today; one pane can starve a legit orchestrator). [roadmap: choice]
- *Promotion/demotion calls pending ux-expert + security verdicts.*

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
- ☐ Persistent fleet rail (projects → agents) — parked with full design
  notes in ROADMAP; decision on promotion pends ux-expert verdict. [roadmap]
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
