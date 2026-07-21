# roost roadmap

Everything known to be outstanding, as of the current `main`. The core product
is complete and green (110 unit tests); nothing here is a known-broken defect —
it's deferred scope, deliberate choices, and one thing only a human can do.

Legend: **[you]** needs a real terminal / human judgment · **[gap]** promised
somewhere but not built · **[choice]** deliberately deferred · **[perf]**
optimization, not correctness · **[health]** internal quality, no behavior
change · **[descoped]** decided against unless a use-case demands it.

---

## Verification

- **[done] Live smoke test.** Exercised against a real terminal via a PTY
  harness: socket round-trip, the full control CLI
  (`list/spawn/send/read/status/wait/close`), the deferred-reply `wait`,
  `read --full` scrollback, bad-token rejection, the audit log, `workspace.json`
  persistence, and the **Alt+q freeze fix** (quit in 0.26 s with live child
  panes) — 15/15 green, clean shutdown, no orphans. The startup handshake
  (alt-screen, mouse, `CSI ? u` kitty query + correct fallback) was verified
  from the wire. The one path a headless PTY can't drive — **Shift+Enter
  inserting a newline in a real pi/Claude pane** — was confirmed by hand in
  iTerm2. (It needs a CSI-u terminal — iTerm2/Ghostty/kitty/WezTerm; Terminal.app
  sends Shift+Enter and Option+Enter identically, so there it hits the Alt+Enter
  picker. See README.)

## Chrome restyle — shipped

- **[done] Ink · paper · one red.** roost's chrome (tabs, borders, badges,
  stack, hint bar, modals) restyled to the `docs/tui-design.html` mockup.
  Spec of record: `DESIGN-ui.md` (contracts C1–C18 + amendments). Verified:
  161 unit tests, design-supervisor audit 18/18 ALIGNED, code review, ux
  review, and a live iTerm2 session hosting pi (pulse phases confirmed on
  screen). Alignment stays auditable via the `design-supervisor` agent
  (`.claude/agents/design-supervisor.md`) — invoke after any `src/ui/**`
  change. Known follow-up candidates: spawn-failure error doesn't say *why*
  (e.g. ENOENT vs PATH); vt100 golden-frame harness deferred until the first
  chrome regression (DESIGN-ui.md §6).

## Control interface — remaining

The interface is complete via the CLI (`list/status/spawn/fork/send/read/close/
wait`, ownership-scoped, audit-logged, CSPRNG control token). Left:

- **[choice] Per-principal connection/rate cap.** Today there's a global 64-conn
  cap; the design (§5.6) wanted a per-principal cap + command rate limit so one
  pane can't open many connections and starve a legitimate orchestrator.
- **[choice] Human-consent gate on reads.** Reads are ownership-scoped but not
  consented; the design (§5.5) noted "the model can see any screen it owns" is a
  different consent posture than managing layout.
- **[gap] Real session-branching `fork`.** `fork` currently opens a fresh
  sibling in the same adapter+cwd. A true fork (branch the agent's conversation)
  needs the pi extension to become bidirectional — pi branches its session and
  reports the new id, roost opens the pane on it.
- **[perf] Audit-log rotation.** `<state>/control.log` is append-only and
  unbounded; add size-based rotation.

## UX & robustness — deferred

- **[choice] Tab-bar overflow past 9 tabs.** Beyond 9, tabs clip and only 1–9
  are keyboard-reachable (`Alt+1..9`). Add horizontal scroll keeping the active
  tab visible, or a tab-picker. Low priority (target is a few tabs).
- **[choice] Dead-pane `Enter` retry.** Relaunching a dead pane re-runs the same
  resume command even if it just failed permanently; could distinguish transient
  vs permanent failure. Rare now that pi/claude ids are reliable.
- **[choice] Closing a tab's last pane deletes the tab.** Deliberate (mirrors
  "close last pane quits"); may become a configurable choice.
- **[perf] Orphan-child cleanup.** The freeze fix reaps non-blocking and SIGKILLs
  the process group; in the pathological case where a child won't die within the
  ~100ms poll it's left to the OS. Fine, but worth revisiting if leaks appear.

## Internal quality — refactors

Pure restructures, no behavior change; do only if roost keeps growing, and each
as its own isolated, well-reviewed change (they touch roost's trickiest code).

- **[health] Dependency inversion.** `core` imports `ui` (`Action`) and raw
  `crossterm` key types; the arrow should point `ui → core`.
- **[health] Extract `SessionResolver`.** The filesystem session-detection logic
  is the real coordination leak of the daemonless model and is spread through
  `app.rs` as private methods; extract it so it's testable in isolation.

## Performance — deferred

- **[perf] Scope pi `session_state` to the cwd.** It walks the *entire* pi
  sessions root per pane at spawn because pi's per-cwd subdir is fuzzy-matched,
  not deterministic. Correct but O(all sessions); narrowing it risks breaking
  detection, so it needs care. (Claude's root is already cwd-scoped.)
- **[perf] PTY read coalescing.** Reads are already memory-bounded by the 1024
  channel; alacritty-style per-read byte coalescing (64 KiB) would cut
  per-message overhead under a firehose. A nicety, not a fix.

## Descoped — not planned

Decided against unless a concrete use-case demands it:

- **MCP bridge** (`roost-mcp`). The CLI is the interface — safest, most
  auditable, LLMs drive it natively via shell. A second surface isn't worth the
  attack surface + async-runtime weight.
- **Event subscription / live output stream.** The design's adversary ranked a
  persistent output subscription worst (a silent cross-pane keylogger). Reads
  stay snapshot-on-demand.
- **HTTP transport**, **multi-instance discovery**, **semantic `read(last_turn)`
  via the extension.** Phase-3 niceties; revisit if needed.
