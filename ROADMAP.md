# roost roadmap

Everything known to be outstanding, as of the current `main`. The core product
is complete, green, and released — v0.1.3, on Homebrew, mise, cargo, and GitHub
Releases; nothing here is a known-broken defect — it's deferred scope,
deliberate choices, and one thing only a human can do. The most recent work is
in **Shipped since best-in-class** just below; the deferred sections beneath it
predate that work and stay accurate for what they each cover.

Legend: **[you]** needs a real terminal / human judgment · **[gap]** promised
somewhere but not built · **[choice]** deliberately deferred · **[perf]**
optimization, not correctness · **[health]** internal quality, no behavior
change · **[descoped]** decided against unless a use-case demands it.

---

## Shipped since best-in-class (v0.1.1 → v0.1.3)

Everything the archived best-in-class engagement left open is now closed or
decided, across four patch releases. Newest work first.

- **[done] Both promotion doors gated by authentication (v0.1.2).** A
  connection is promoted out of the 2 s pre-auth timeout only once it
  authenticates — a control request under the fleet/pane token, or a status
  line whose pane+token pair matches — so an unauthenticated local process can
  no longer say one well-formed line and squat a slot in the 64-connection
  pool. Token storage is unified into one `TokenTable` the socket layer can
  only read, single-writer enforced by the compiler (`&mut self`); a per-pane
  reporter cap of 8; `roost.ts` reconnects with backoff instead of going
  silent for the session after one transient error. Design → architect pass →
  fable design review (which killed a proposed `arc-swap` dependency and caught
  a false-rejection bug in the spec) → adversarial review (which caught the
  mutators enforcing single-writer by convention rather than by type). Verified
  black-box against real binaries: on v0.1.0, 0 of 40 unauthenticated squatters
  are ever evicted; on v0.1.2, all 40 die at the deadline and authenticated
  clients are untouched. Spec: DESIGN-control.md §5.6.
- **[done] Unbounded control-reply write held a slot for the client's life
  (v0.1.2).** Found while black-box-testing the auth gate: a client that sends
  a request and never reads the reply parked a roost thread in `write_all` and
  held its connection slot until that client died, so ~64 stuck peers wedged
  the control plane — present in the shipped v0.1.0/v0.1.1. `write_reply` now
  writes against a 5 s deadline, the same split `read_line_deadlined` already
  used. (A separately-reported "42 requests wedge the control plane" turned out
  to be a **test-harness artifact** — an undrained pty backing roost's
  `draw()` up until the event loop blocked — not a product bug; recorded here
  so it isn't re-chased.)
- **[done] Status fidelity on pi (v0.1.1).** Byte noise — composer echo, the
  answer render landing just after a turn ends, a resize repaint — no longer
  fakes a Working status while the pi extension is live. A silent Working
  decays to **idle** when the extension link vouches for the pane (a quiet
  think), to **waiting** when the reporting link is down (hook presumed dead).
  pi's project-trust dialog now reports a real ◆ over the `project_trust`
  event — pi emits no audible bell (verified against installed 0.81.1; the old
  bell-heuristic comment was false). Link-liveness is refcounted per pane so
  overlapping reporters (a nested claude inside a pi pane) can't strand it.
- **[done] The Working glyph is pi's braille spinner (v0.1.1).** Replaces the
  pulsing ● — animation now always means *busy*, and a steady ◆ is the only
  mark that asks for you. Frames are pi-tui's own loader set, on roost's shared
  render clock. **This supersedes the "pulse phase" language in the chrome
  sections below** — those entries are accurate for the era they describe, not
  for current behavior.
- **[done] A drag holds the view still (v0.1.3).** On a pane that is still
  printing, a native-selection gesture presents a frozen snapshot for the
  length of the drag, so the highlight and the copied text agree instead of
  the text scrolling out from under the selection. It reuses the existing
  sync-view presentation path (no second snapshot mechanism); the freeze is
  released on every gesture end through a single chokepoint tied to the P20
  latch — five paths that swallow the mouse-up were found and closed in
  review — and dropped on a resize. `roost read`'s screen mode reads the live
  frame. Content-anchoring was assessed and **rejected**: the vendored vt100
  parser has no stable row identity and a resize rewraps the grid. Spec:
  DESIGN-ui.md C29 amendment + SPEC-parity P1.
- **[done] A minimal key-remap config file (v0.1.3).** `config.json`, next to
  `workspace.json` (`ROOST_STATE` redirects both), disables or remaps
  individual `Alt` bindings — the escape hatch for the `Alt+f`/`b`/`d` readline
  collisions this roadmap flagged. Absent file = prior behavior byte-for-byte;
  a malformed file never blocks startup (defaults + a toast naming the bad
  entry). Scope is keybindings only — no theming (the chrome inherits the
  terminal), no scripting. The zero-config stance now has its one escape hatch
  and nothing more.
- **[decided] Multi-page mouse selection — declined.** A single selection can't
  span more than one page; `roost read --full` covers "many pages of output".
  A fable reliability assessment found neither edge-autoscroll nor
  buffer-anchoring can be made as non-flaky as the selection that ships —
  timer nondeterminism, a direct conflict with the freeze above (which holds
  the view still while autoscroll wants it to move), and the same
  no-stable-row-identity wall. Declined on the reliability-first priority.
- **[descoped] `spawn --worktree` — by the client (2026-08-08).** git owns
  worktrees, roost owns panes; `git worktree add` + `roost spawn --cwd` already
  composes, and the lifecycle questions (naming, cleanup, resurrection into a
  deleted worktree) are baggage worth avoiding.

Still needing 30 seconds at a real iTerm2 — **[you] `Alt`+click a URL on
iTerm2.** Option is iTerm2's own bypass gesture, so the click may never reach
roost. Unverified there; works on Ghostty / kitty / WezTerm, which send Alt
by default.

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
  clean `Alt+q` exit within 2 s with no orphaned children). This gate is a
  perf smoke test, not a visual regression suite — see the next entry for
  the color scenarios once deferred here.

- **[done] Golden-frame *color* scenarios.** The deferred trigger fired
  (this engagement leaned on `src/ui/render.rs` heavily — see
  `openspec/changes/best-in-class/PLAN.md` Phase 5). Audit first: of
  C1-C30, the in-process `chrome_buffers()` fixture (render.rs) already
  covered nearly every contract — four prior reactive patches (the
  too-small notice, a lit selection, the filtered/reordered roster, the
  dead-pane bar) had each closed a gap the hard way. This pass found and
  closed one more before it became a fifth reactive patch (scrollback
  search hits, P21/C17: unit-tested in isolation but never exercised
  through the real `draw()` path), and named one it left open —
  C7's *unfocused* expanded-stack-member edge marker, reachable only by
  moving keyboard focus onto a collapsed row while a stack's expanded
  member stays put elsewhere — still no fixture exercises it. `tests/chrome_theme.rs`
  gained five PTY-level scenarios asserting on the real emitted SGR (not
  just the in-process buffer): the Working pulse (both guaranteed reds,
  never a third hue, never DIM), the dead-pane bar (red-reversed across
  its whole row, not just its text — the C16/D1 regression shape),
  a lit copy-mode selection (reversed, no forced color, no fill), the
  roster's status filter (title tag in the tier's own C5 color — new as
  of this same week, P2-11), and the C30 sub-two-row notice at its real
  80×1 geometry (`Harness::try_spawn_sized` is new). Each was confirmed to
  fail — reintroducing a fixed RGB in `accent()`/`pulse_bright()`, then a
  background fill in `attention_problem()` and separately in
  `highlight_selection()`, every time reverted after. Self-extension:
  `render::tests::every_mode_variant_has_a_chrome_buffers_fixture`
  exhaustively matches `Mode` with no wildcard arm, so a new modal surface
  is a compile error until it earns a fixture — genuinely self-enforcing
  for that one axis (C12-C16/C20/C22/C24/C27). Screen-size, status-
  combination and focus-permutation axes (C30, the roster filter, C7)
  aren't enum-shaped and stay a human-must-remember list.

## Control interface — shipped

The interface is complete via the CLI (`list/status/spawn/fork/send/read/close/
wait`, ownership-scoped, audit-logged, CSPRNG control token).

- ~~**[choice] Per-principal connection/rate cap.**~~ **DONE 2026-08-07.** A
  global 64-connection cap plus a per-principal cap of 20 connections (enforced
  in `src/infra/sock.rs`, DESIGN-control.md §5.6, audit M3): so one pane can't
  open many connections and starve a legitimate orchestrator. The per-principal
  floor is pinned to support `app.rs`'s `MAX_WAITS` (16 parked wait commands
  hold one connection each) with headroom for other commands alongside.

## Control interface — remaining

Left:
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

- **[choice] Leader-key / chord-sequence keybinds — assessed reliable,
  deferred until the keyspace is actually pressured.** A prefix layer to
  expand beyond the flat `Alt+<key>` layer. A fable reliability assessment
  (2026-08-08) found a **single modal leader** clears the "as reliable as the
  flat Alt layer" bar: `Alt+<leader>` enters a `Mode::Leader` pending state,
  the next chord resolves from a flat config table (`"leader": {"c":
  "new_pane"}`), `Esc` aborts — reusing the copy/scroll/search modal machinery
  with **no wall-clock timer**, no state added to the pure `ui/input.rs`, and
  no change to `main.rs` routing or the raw-passthrough path. The leader must
  stay in the `Alt` namespace (a bare `Ctrl` prefix would steal the agent's
  readline bytes — the P13 collision Alt-only exists to avoid). The **timed /
  ambiguous-prefix** variant (vim-style `g` that might become `g t`) is
  **rejected permanently**: wall-clock disambiguation is the nondeterminism
  this project killed edge-autoscroll for. Deferred on worth-it, not on risk —
  demand is other projects' upvotes (crush #737 ~23👍, zellij #2890 ~41👍),
  per-chord remap already covers "move a binding", and ~6 unshifted + ~20
  shifted `Alt` chords are still free. **Trigger to build:** a concrete feature
  needs a chord and finds none free. Build it then, with this design — not
  before. (General principle it confirmed: in roost, *modal* features — enter a
  `Mode`, next keys resolve, `Esc` exits — are reliable and timer-free; any
  feature needing a wall-clock timer to disambiguate input is the flaky path.)
- ~~**[choice] Tab-bar overflow past 9 tabs.**~~ **ALREADY DONE — entry was
  stale.** Both halves shipped without this being ticked off: U7 (2026-07-27)
  added `Alt+0` for the last tab and `Alt+i`/`Alt+m` to step the strip with
  wrap, so tabs past the ninth are keyboard-reachable; and C2's
  `mouse::tab_scroll_start` scrolls the strip so the **active tab is always
  visible**, with a `…` marking each end that hides tabs. C27's roster
  (`Alt+Shift+a`) is the tab-picker this entry offered as the alternative.
- **[choice] A persistent fleet rail — projects → agents.** Parked 2026-07-28
  after a design pass; **not** the sidebar the vertical-tabs tribunal rejected,
  and worth keeping distinct from it.

  The tribunal gave two reasons. The arithmetic one holds (with a correction
  recorded at C27's provenance: it forbids *side-by-side* panes rather than
  splitting outright, and it was only ever run at herdr's 20-column width —
  a rail of ≤ 8 columns clears the 80-column floor). The other reason,
  "a tier roost's singleton `Workspace` does not have", **is wrong**: the
  missing tier is the **directory**, which every `PaneSpec` already carries
  and C14's picker already surfaces as a recent-cwd column.

  That changes what the surface would be. Grouped by *tab* it rotates the
  axis C2's strip already draws — the duplication that made the first pass
  feel redundant. Grouped by **project → agent** it adds an axis roost has
  never shown: which repository, which agents inside it, what each is doing.
  Two orthogonal reads of one fleet, the way `Alt+e` (what happened) and
  `Alt+Shift+a` (what is) are already contracted apart.

  The shape, if it is ever picked up:
  - **Two levels.** A project row (directory name, worst-first aggregate
    glyph + count exactly as C2 computes it, git branch from `.git/HEAD` on
    a second line), then one row per agent (status glyph, pane id, adapter).
    Agents indent to the branch column; a blank row separates projects but
    never splits one. The rail reserves a trailing gutter column.
  - **Responsive tiers.** ~20 columns labelled on wide terminals, ~6 columns
    (marker · glyph · id) between 80 and 120, nothing below — the collapse
    is what keeps the floor legal, and it is what Chrome's vertical tabs
    collapse to.
  - **Grouped by `PaneSpec.cwd`**, not the live observed cwd: stable beats
    truthful here, or rows jump between folders as you `cd` in a shell pane.

  Honest costs, so a later pass doesn't have to rediscover them: it argues
  against §1 (every column is one not showing agent output) and would be the
  largest permanent chrome expenditure roost has made; it needs a contracted
  split against C27's roster or the two drift; and it is **not** the free
  reuse of `roster_rows()` an earlier sketch claimed — cwd grouping is a new
  row model with its own aggregate. The free Alt pool is also down to
  `b d p v x y PgDn`, and `b`/`d` are protected readline ops, so the toggle
  chord is an open problem rather than a detail.

  Mockup (cell-accurate at 80/100/120/160 columns, with the split-floor
  arithmetic computed live):
  https://claude.ai/code/artifact/b048130b-8f0a-4b47-a3c6-afca00888491

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
  `crossterm` key types; the arrow should point `ui → core`. **Assessed
  2026-08-07, deferred — not attempted.** Measured rather than guessed: in
  `app.rs`'s *production* code (excluding its own test module, which accounts
  for most of the 502 `Action::` references there) the coupling is the
  ~30-arm `Action` dispatch in `apply()`, one call into `ui::input::
  {InputResult, translate}` (the Alt-toggle-off check in `handle_mode_key`),
  a `ui::mouse::Seam` struct field, three `ui::mouse`/`ui::render` hit-test
  helper calls, and — the real weight — two `pub fn`s that take raw
  `crossterm::event::{KeyEvent, MouseEvent}` directly as their parameter
  type (`handle_mode_key`, ~580 lines; `handle_modal_mouse`, ~100 lines),
  with `KeyCode`/`KeyModifiers` matched throughout for text editing, cursor
  movement, and mode toggling. About a third of that — the `Seam` field, two
  pure hit-test helpers, and `state_word` (already a pure `AgentStatus ->
  &str` map with no rendering concern) — could move to `core` cheaply, and
  `Action` itself could follow with a re-export left behind in `ui::input`
  so its ~200 other references (main.rs, ui/render.rs, ui/mouse.rs,
  ui/input.rs) never need touching. The rest has no mechanical fix: `core`
  accepting crossterm's own event types as its public API means a real
  inversion needs `core` to define its own key/mouse vocabulary and have
  `ui`/`main.rs` translate at the boundary — a design decision, not a move —
  threaded through the two largest, highest-traffic input-handling methods
  in the app, exactly where "watch for a moved method quietly changing when
  it's called" bites hardest. That's the "large mechanical churn late in an
  engagement" this entry already warned about, for a payoff that's purely
  structural. Left for its own engagement; if picked up, split the cheap
  slice (types/field/helpers) from the crossterm-vocabulary redesign — they
  are different risk classes and don't need to land together.
- ~~**[health] Extract `SessionResolver`.**~~ **DONE 2026-08-07.** The
  filesystem session-detection logic previously spread through `app.rs` as
  private methods — `spawn_pane`'s stored-session resume/stale decision and
  `tick`'s claimed-session exclusion — is now `core::session_resolver::
  SessionResolver`: three methods (`resolve`, `detect`, `claimed_sessions`)
  over an `&dyn AgentAdapter` and a `Workspace`, none of it needing a
  running `App` to construct or exercise (proved by 8 new unit tests against
  the resolver alone). `App` still owns the bookkeeping this leans on —
  `pending_detect`, `last_detect`, persistence — since that's tied to its
  own runtime lifecycle, not the decision logic. Landed as two commits:
  characterisation tests against the *pre-extraction* code first (pinning
  Exists/Gone/Unknown, no session root, a root that exists but can't be
  read, and a session id already claimed by another pane — that last one
  wasn't previously pinned in isolation, and a mutation check confirmed all
  the new tests, old and new, actually fail without the fix they pin), then
  the extraction, which changes none of `App`'s public surface. Full suite
  green twice (722 unit tests, up from 714), `cargo clippy --all-targets`
  unchanged (the same 4 pre-existing warnings, none in the new code).
  `agents/pi.rs`'s narrowed-then-full-scan-fallback detection was left
  untouched, as was every other adapter.

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
