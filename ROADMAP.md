# roost roadmap

Everything known to be outstanding, as of the current `main`. The core product
is complete and green (553 unit tests); nothing here is a known-broken defect —
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
  (e.g. ENOENT vs PATH); the vt100 golden-frame harness itself is superseded
  below — its foundation shipped with the fleet-features firehose gate,
  though golden-frame *color* scenarios remain deferred on the same trigger.

- **[done, 2026-07-27] The chrome inherits your terminal's theme.** A user on
  a light-theme terminal reported the tab bar and hint bar as near-black bands
  and the **active tab's label invisible** — near-white ink drawn on
  `Color::Reset`, i.e. white on their white background. That was the shipped
  stance, not a slip (`DESIGN-ui.md` §2 "Truecolor stance", logged as
  SPEC-GAP-4), so the fix was to change the stance: every fixed `Rgb` hue is
  gone. Text is now the terminal's own fg on its own background (plus a `DIM`
  rung), ANSI 8 is spent on borders and separators only, attention surfaces
  reverse instead of filling, and the one red is ANSI 1 with ANSI 9 for the
  bright half of the pulse. No chrome fills at all: `BG`, `TAB_STRIP` and
  `BAR` are deleted, and the active tab and focused collapsed row are carried
  by ink weight plus their `▎` marker. Correct on light, dark and tinted
  themes alike, and it survives a live theme switch with no detection
  machinery. Spec of record: `DESIGN-ui.md` §2 + the dated per-contract
  amendments; SPEC-GAP-4 closed. Verified: 496 unit tests, three PTY
  scenarios asserting the emitted SGR (`tests/chrome_theme.rs`), four
  mechanical gates that fail if a fixed hue or a fill comes back, and a
  design-supervisor audit.

## Fleet features — shipped

- **[done] Navigate & arrange a bigger fleet.** Eight new surfaces landed on
  top of the restyled chrome: jump-to-attention (`Alt+a`, C19), an activity
  feed (`Alt+e`, C20), pane zoom (`Alt+z`, C21), a floating scratch pane
  (`Alt+f`, C22), per-pane raw pass-through (`Alt+Shift+p`, C23), keyboard
  copy mode (C24), canned layout cycling (`Alt+g`, C25), and broadcast send
  (`roost send --all` — CLI-only by design, no TUI key). Tab-undo (`Alt+u`)
  was already whole-tab capable; C26 is a scope statement + pinning test,
  not new behavior. Spec of record: `DESIGN-ui.md` (contracts C19–C26 +
  amended C9/C15/§8 key table). Verified: 306 unit tests
  (`cargo test --bin roost`), all green. Known follow-up candidate: a
  floating *quick-launch picker* (distinct from the scratch float that
  shipped) is still an open idea — out of scope this round, per the brief.

- **[done] Firehose input-latency gate + PTY harness foundation
  (DESIGN-ui.md §6).** The vt100 golden-frame harness assessed and deferred
  during the chrome restyle now has its foundation built:
  `tests/harness/mod.rs` spawns the real binary in a 120×40 portable-pty and
  parses its output with `vt100`; `tests/firehose.rs` uses it to assert
  input stays responsive under sustained output (every echo visible within
  250 ms, the firehose region still visibly moving every 500 ms sample, a
  clean `Alt+q` exit within 2 s with no orphaned children). **Golden-frame
  *color* scenarios are still deferred** — same trigger as before (first
  chrome regression, or the next engagement leaning on `src/ui/render.rs`);
  this gate is a perf smoke test, not a visual regression suite.

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
- ~~**[perf] Audit-log rotation.**~~ **DONE 2026-07-28.** `control.log`
  rotates to `control.log.1` past 4 MiB (~100k control calls), keeping one
  generation — so the trail is bounded at twice the cap and the part kept is
  always the most recent. Rename rather than truncate-in-place: it is atomic,
  so a reader tailing the log never sees a half-empty file. Best-effort and
  silent, the same stance the append itself already took — an audit line that
  cannot be written must not take a control call down with it.

## UX & robustness — deferred

- ~~**[choice] Tab-bar overflow past 9 tabs.**~~ **ALREADY DONE — entry was
  stale.** Both halves shipped without this being ticked off: U7 (2026-07-27)
  added `Alt+0` for the last tab and `Alt+i`/`Alt+m` to step the strip with
  wrap, so tabs past the ninth are keyboard-reachable; and C2's
  `mouse::tab_scroll_start` scrolls the strip so the **active tab is always
  visible**, with a `…` marking each end that hides tabs. C27's roster
  (`Alt+Shift+a`) is the tab-picker this entry offered as the alternative.
- **[choice] Dead-pane `Enter` retry.** Relaunching a dead pane re-runs the same
  resume command even if it just failed permanently; could distinguish transient
  vs permanent failure. Rare now that pi/claude ids are reliable.
- **[choice] Closing a tab's last pane deletes the tab.** Deliberate (mirrors
  "close last pane quits"); may become a configurable choice.
  *Amended 2026-07-28:* the **last** tab is the exception — there is no tab to
  delete, so `close_pane_id` left it with an emptied `Stack` root and saved
  that. Next launch loaded zero panes: chrome around a blank body, dead-pane
  hints (pane 0 has no runtime), and no key that makes a pane. Repaired at the
  load boundary rather than at the one write that produces it —
  `Workspace::validate_and_repair` now drops paneless tabs and starts over if
  none survive, so a hand-edited file or a crash mid-close lands in the same
  net. Pinned by `tests/empty_tab_recovery.rs`, which boots the real binary
  against the exact broken `workspace.json`.
- ~~**[perf] Orphan-child cleanup.**~~ **DONE 2026-07-28 — and it was not a
  pathological case.** The entry guessed the risk was "a child that won't die
  within the ~100ms poll"; the actual leak was structural and reproducible on
  the first try. An interactive shell with job control puts every job in a
  *new* process group, so `sleep 600 &` sat in neither the pane's group (the
  one roost SIGKILLs) nor the session's foreground group (the one the PTY's
  hangup reaches). It outlived every quit, reparented to init, still running.
  `PtyPane::kill` now sweeps the whole **session** after the group kill —
  `setsid` makes the pane its own session leader and a job-control fork
  changes only the group, so nothing a pane spawns can leave that net without
  asking to. Pinned by `tests/orphan_cleanup.rs`, which backgrounds a real
  job and fails without the sweep.
  *Separately:* `tests/firehose.rs`'s no-orphans gate had been failing on this
  host for an unrelated reason — `harness::is_alive` used `kill -0`, which
  succeeds for a **zombie**. A zombie is a process that has already died and
  holds nothing; counting one as a survivor made the gate fail on any host
  whose PID 1 reaps lazily (here it is a container shim). `is_alive` now
  checks the state too. The two fixes are independent, and each was verified
  against the other disabled.

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
