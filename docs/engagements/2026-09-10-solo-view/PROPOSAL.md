# Solo view — one pane at a time, the rest listed beside it (per tab)

*Design proposal, 2026-09-10. Research and recommendation only; nothing here
is built. Mockups (cell-accurate at 80 / 110 / 160 columns, with the rail
tier arithmetic computed live) are published as an artifact — link in the
ROADMAP entry that points here. If adopted, the target section becomes a
DESIGN-ui.md contract (next free number: C43) and this file stays as the
record of how the shape was chosen.*

## 0. The ask

> Support a view in which we show one pane at a time, with a sidebar listing
> all the panes so the user can switch between them. Scoped to each tab.

## 1. What roost already has, and what the ask is really asking for

Three existing surfaces each cover a third of this:

| Surface | What it does | What it lacks for the ask |
|---|---|---|
| **Zoom** (C21, `Alt+z`) | One pane fills the body; *zoom follows focus*; a pure view transform — the tree is untouched; `ZOOM · n hidden` names the count | App-wide, session-only, exits on every tab change and every structural chord; the hidden panes are counted, never *listed* |
| **Stack** (C6–C8, `Alt+s` held / `Alt+g` all-stack) | One expanded member + one C8 row per collapsed member: identity, live status glyph, state word — "a fleet dashboard for free" (DESIGN.md §4) | The list is spent in **rows**, the scarcer axis; the expanded pane's rect **moves** as you step (a collapsed row migrates above it); it is structural — the collapse ladder cannot return you to the tree you had (C42, by design) |
| **Roster** (C27, `Alt+Shift+a`) | Every pane in every tab in C8's row format, jump on `Enter` | A modal: it is a place you go, not a place you work |

Put together: **the ask is the all-stack arrangement with its collapsed rows
turned into a side rail, plus zoom's exact-return-ticket semantics, made
per-tab and persistent.** That framing decides most of the design, because
every piece already has a contract:

- The rail rows are C8's `collapsed_row_spans`, verbatim — the roster already
  reuses them "one column narrower".
- The rail header is C6's underlined `STACK · n PANES` idiom with one word
  swapped.
- The "one pane fills the body" mechanics are C21's `display_rects()` seam —
  renderer, PTY sizing and mouse hit-testing consume one display list, focus
  math keeps reading the real tree.
- Stepping through the list is what `Alt+↑/↓` already means inside a stack.
- Per-tab focus memory (SPEC-ux U11, `tab_focus`) already decides which pane
  a tab shows when you return to it.

No new glyph. No new colour. One new chord, one new mode word, one new
per-tab field.

### 1.1 The sidebar tribunal's rejection does not apply here

The 2026-07-28 tribunal rejected a herdr-style always-on rail on the
80-column arithmetic: a 20-column rail beside *tiled* panes leaves 30-column
panes, under `MIN_SPLIT_COLS = 36`. Its own later correction (C27 provenance)
narrowed that to "forbids side-by-side panes" and noted a ≤ 8-column rail
clears the floor exactly.

Solo view is the one configuration in which that arithmetic is moot: **there
is only one pane on screen**, so there is no split to fit. At 80 columns a
6-column rail leaves a 74-column pane (72 inner) — *wider* than either pane
of the 40/40 split the tribunal was protecting. The rail is not competing
with tiles for columns; it is replacing the frames of the panes you are not
looking at. The ROADMAP's parked *fleet rail* (projects → agents, always on,
beside tiles) is a different surface with a different job — §7 states the
split so the two never drift.

## 2. The shape

### 2.1 Model

- **`Tab.view: TabView { Tiled, Solo }`** — a per-tab, persisted field.
  `#[serde(default, skip_serializing_if = "TabView::is_tiled")]`, so a tiled
  tab writes byte-identical JSON to today, an older `workspace.json` loads
  unchanged, and an older roost ignores the key (the exact additive pattern
  `StackOrigin.from` and `PaneSpec.note` used — no migration, no version
  bump). `validate_and_repair` needs no case: a view flag has no invariant.
- **Solo is a pure view transform.** The layout tree is never touched by
  entering, leaving, or stepping. `display_rects()` in a solo tab yields
  `[float?, focused @ body-minus-rail]`; `rects()` (the real tree) is what
  focus math, `spawn_child`'s split direction and the tiled view still read.
  Leaving solo shows the tree exactly as it was — the round trip C42 says
  the ladder cannot offer, for free, because nothing was ever collapsed.
- **Shown ≡ focused.** There is no second cursor: the pane the rail marks
  with `▎` is `App::focused`, and every existing way of moving focus
  (`Alt+a`, `Alt+;`, roster/feed `Enter`, `roost focus`, a click) changes
  what is shown, through `set_focus` — the single writer. `set_focus`'s
  `expand_in_stacks` still runs, invisible in solo and correct on exit.
- **Zoom composes on top, unchanged.** `Alt+z` in a solo tab hides the rail
  and gives the pane the whole body — `ZOOM · n hidden` on the border,
  `ZOOM` in the word slot; `Alt+z` again brings the rail back. Zoom keeps
  every C21 rule (session-only, exits on tab change); solo is the persistent
  sibling. Precedence in C9's slot: real mode words > `RAW` > `ZOOM` >
  `SOLO` > `NORMAL`.

### 2.2 Geometry (the rail)

The rail is the leftmost columns of `body_area()`, full body height (tab bar
above it, hint bar below it — both stay full-width). No divider column: the
shown pane's own left border (C3) is the rule. No fill (§2 background
policy).

Two tiers, chosen by body width — the same collapse Chrome's vertical tabs
and the ROADMAP's rail sketch make:

| Body width | Rail | Contents per row |
|---|---|---|
| ≥ 100 | **labelled**, `clamp(width / 5, 20, 32)` columns | C8 row: `▎` marker · status glyph · id · name · fill · `adapter · word` — sheds right-to-left by C8's own rule (the state word goes first, the name clips last) |
| 40 – 99 | **glyph**, 6 columns | `▎` marker · glyph · space · id, padded — the ROADMAP's own "marker · glyph · id" tier |
| < 40 | none | sub-floor; the pane takes the body |

The pane rect keeps **≥ 80 columns** at every width from 100 up (rail ≤ 20
there) and 74 at the 80-column floor. Widening the terminal widens the rail
up to 32 columns, at which point C8 rows show name *and* state word —
progressive disclosure through a shedding rule that already exists.

- **Header row** (row 0 of the rail), C6 verbatim with one word swapped:
  labelled tier `" SOLO · {n} PANES"` left, `"ALT+↑↓ "` right-aligned,
  `quiet()`, `UNDERLINED` across every cell; glyph tier `" SOLO "`
  underlined. The header belongs to no pane: clicks there are dead.
- **Rows**: one per pane of the tab in `pane_order()` (the tree's leaf order —
  the same order `Alt+↑/↓` will step and the roster's tie order), top-aligned
  under the header. The shown pane's row carries the `▎` `accent()` marker
  and full-strength ink (C8's focused style). Rows recompute every frame
  from the workspace, like the roster: renames, status flips and closes
  show up live.
- **Overflow**: when rows exceed the rail's height, the window scrolls the
  least it can to keep the shown row visible (C2's strip rule, vertical),
  with a `…` row marking each hidden end. Derived from focus every frame,
  never stored.
- **The float (C22)** is not in the tree and is not a row. It draws above the
  solo view, as it draws above zoom.
- **PTY sizing**: the shown pane is resized to its display rect on entry and
  on every step; hidden panes keep their last size until shown or until the
  tab is tiled again (C21's "no reflow churn" rule; P5's lossless reflow
  makes the round trip safe).

### 2.3 Chord

**`Alt+Shift+t`** (`Action::ToggleSolo`), with the `Alt+'T'` uppercase-
delivery tolerance every shifted chord carries.

The reading: **`Alt+t` adds a tab to the strip; `Alt+Shift+t` gives this tab
a strip of its own.** It is the same-letter shift-pair idiom the map was
re-keyed to on 2026-09-01 (`g`/`Shift+g`, `s`/`Shift+s`, `m`/`Shift+m`,
`i`/`Shift+i`, and `z`/`Shift+z` five days ago). `ESC T` is not an escape
introducer, so it carries none of N3's ambiguity.

Why not the obvious ones — recorded so the next pass does not re-derive it:

| Chord | Verdict |
|---|---|
| `Alt+Shift+z` | The natural "persistent zoom" sibling — **taken** by the float on 2026-09-03 |
| `Alt+Shift+o` (vim's `Ctrl-w o`, "only") | **Rejected**: on a terminal without the kitty protocol it arrives as `ESC O`, the **SS3 introducer** — application-mode arrows and F1–F4 begin with it. Same hazard class as `Alt+[` (CSI) and `ESC P` (DCS), and a more common collision than the `ESC f` = Alt+Right one that just cost the float its chord |
| `Alt+p` ("panes") | Runner-up. Spends C23's reservation (bare `p` kept free so the raw toggle has no near-miss) and shadows readline's `M-p` (history search) — the same trade `Alt+n` already makes on `M-n`. Take it if the shift-pair reading above doesn't land |
| `Alt+g` fourth stop | **Rejected** as the *only* entry: C25's rule is "the arrangement dictates structure"; a view flag among structural stops has no fit predicate and breaks the one-sentence rule. See §5 |
| `Alt+b` / `Alt+d` / `Alt+f` | Protected readline word ops (U5, and the 2026-09-03 re-key) |
| `Alt+\|` | Visual mnemonic (the rail is a bar), but no shift-pair, and emacs' `M-\|` |
| `Alt+y` | Free, no mnemonic |

### 2.4 Keys inside a solo tab

Every chord keeps its `Action`; only the dispatch is solo-aware. Nothing is
silent (C38).

| Chord | In solo |
|---|---|
| `Alt+↑/↓`, `Alt+j/k` | Step to the previous / next row (`pane_order`). Clamped at the ends, silently — the same dead end `↑/↓` keep in every other in-tab move (C31) |
| `Alt+←/→`, `Alt+h/l` | **No new rule.** The shown pane spans the full body width and no other pane is narrower, so C31's cross-tab condition holds on the first press: previous / next tab. `←→` = tabs, `↑↓` = panes — a two-axis model that matches the strip's own axis |
| `Alt+Shift+↑/↓` | **Reorder**: swap the shown pane with its order-neighbour (`layout::swap_panes` with the adjacent leaf). Persisted; in the tiled view it is the same swap |
| `Alt+Shift+←/→` | Refused: `solo view — Alt+i / Alt+Shift+i moves a pane between tabs` |
| `Alt+n`, picker launch, `roost spawn` | Adds the pane to the tree by the normal split rule (direction from the *tiled* rect), then it is focused and shown; a row appears after the current one. **The comfort floor is not applied** — the user cannot see the tiled geometry, so "no room to split" would be a refusal about a picture they are not looking at. The guard is C42's `every_pane_is_drawable` against the tiled body: drawability, not taste |
| `Alt+w`, control close | Closes; focus lands on the **next row below, else the one above** — the tab-strip idiom (closing the active tab activates its neighbour), not `first_visible()`. Closing the last pane closes the tab, as today |
| `Alt+s`, `Alt+Shift+s`, `Alt+o`, `Alt+g`, `Alt+Shift+g`, `Alt+- = < >` | **Refused**: `solo view — Alt+Shift+t tiles this tab`. These change *shape*, and shape is the one thing solo does not show; applying them invisibly is the surprise C21 exits zoom to avoid, and exiting a persisted mode on a stray chord would be the bigger surprise. Membership (`n`, `w`, `u`, `i`, mark/pull) and order (`Shift+↑↓`) are real in the rail and stay live |
| `Alt+z` | Zoom over solo: rail hidden, full body, `ZOOM · n hidden`; again restores the rail (§2.1) |
| `Alt+Shift+z` | Float toggles above the view, unchanged |
| `Alt+a`, `Alt+;`, roster/feed `Enter`, `roost focus` | Move focus → the shown pane changes. A cross-tab jump lands in the other tab's own view (solo or tiled) |
| `Alt+i`/`Alt+Shift+i`, `Alt+Shift+x`/`v`, `Alt+u` | Membership moves; rows appear and leave |
| `Alt+r` | Rename → the row updates that frame |
| `Alt+1..9`, `Alt+0`, `Alt+m`, `Alt+t` | Tabs, unchanged; arriving in a solo tab shows its remembered focus (U11) |
| `Alt+Shift+t` | Back to tiles: tree byte-identical, focus unchanged. **Refused** when the tiled tree is not drawable at the current size: `can't tile {n} panes at {w}×{h} — close some, or widen the terminal` |
| `Esc`, everything else | Solo is not a `Mode`; keys pass to the pane as in Normal |

### 2.5 Mouse

- Click a rail row → `on_click(id)`: focus, therefore shown. One press, the
  tab strip's and roster's rule.
- Click the header or the `…` rows → nothing.
- Wheel over the rail → scrolls the rail's window only when it overflows;
  **never moves focus**. (The roster's wheel moves a *cursor* that acts on
  `Enter`; there is no such buffer here, and a stray notch must not swap the
  pane you are reading.)
- Clicks, wheel, drags over the pane → the shown pane, exactly as today.
- No seams exist in the display list, so seam drags are impossible rather
  than refused.
- Drag-to-reorder: **deferred** (`Alt+Shift+↑/↓` covers it; the P20 latch
  and the C29 selection freeze make a drag gesture the expensive path).

### 2.6 Chrome

- **Mode word** `SOLO` in the C9 slot and, via U15, in the tab bar's status
  area when the hint bar can't carry it.
- **Hint bar, Normal in a solo tab** (six pairs, 86 columns — inside the
  100-column floor beside the right segment, with headroom for a remapped
  chord under C34): `Alt+? keys` · `Alt+↑↓ pane` · `Alt+←→ tab` ·
  `Alt+n new` · `Alt+w close` · `Alt+Shift+t tile`.
- **Help overlay** (C15): one row in the layout group — `Alt+Shift+t` →
  *solo view: one pane at a time, the rest listed beside it (same chord
  tiles them again)*. `every_bound_chord_is_documented_in_the_keymap`
  enforces it.
- **Pane border**: the shown pane draws with normal C3/C4 chrome (focused
  `accent()` border, identity badge). No `SOLO` on the border — the rail
  header and the word slot already say it, and the border title slot is
  zoom's.
- **Tab strip**: unchanged. A tab's view is not shown on the strip; the
  rail is the indication, and only the active tab's view can matter.

### 2.7 Persistence and control plane

- Saved on toggle and on reorder like any other mutation (`App::save` at the
  tail of `apply`); restored on launch, so a solo tab comes back solo with
  its remembered focus.
- `roost list` / `roost status` are unchanged. No CLI verb toggles the view
  (zoom has none either); `roost focus` lands in whatever view the tab is in.

## 3. Why this shape — the decisions, each with its alternative

1. **A view mode, not a layout node.** The alternative — render a
   *root-level* `Stack`'s collapsed rows as a side rail when the body is
   wide — needs no new mode, chord, or field, and is worth naming because
   it is close. It loses on three counts: it destroys the tiled tree the
   user built (C42 concedes the ladder can't restore it), it has to define
   what a rail-stack *nested inside a split* looks like, and it changes the
   look of every existing all-stack tab. The view mode keeps the tree, is
   flat by construction, and leaves stacks alone.
2. **Per-tab, persisted — not zoom extended.** Zoom's contract is
   transience (tmux `prefix+z` muscle memory: maximize, look, come back).
   Making it per-tab and persistent would break that for everyone who uses
   it that way. Two toggles with one sentence between them: *zoom is a
   glance, solo is a way of working*; zoom over solo hides the rail.
3. **A vertical rail, not a second horizontal strip.** A row of pane chips
   under the tab bar costs a row where rows are scarcest (80×24), truncates
   names, overflows by the eighth agent, duplicates C2's scroll-and-`…`
   machinery, and puts two look-alike strips one above the other. A rail
   holds ~22 named rows at 24 lines and gets *more* legible as the terminal
   widens.
4. **Left, not right.** Convention (iTerm2's left tab bar, Chrome/Arc
   vertical tabs, herdr, every file tree) and the tree reading: the rail sits
   under the active tab's label, so *tab → its panes* reads top-left to
   bottom-left. VS Code puts its terminal list on the right only because the
   explorer already owns its left edge. The one argument for the right —
   the pane's left margin doesn't move on toggle — loses to a pane that
   moves 6–32 columns once, versus a list that reads against convention
   forever.
5. **Rows are C8 rows, not boxes or two-line cards.** The roster made this
   call first. C8's boxes exist because a bare bar *between two framed
   panes* was easy to miss; a rail is nothing but rows, so the framing has
   no gap to fix, and boxes would cost 3 rows each. Wider rails reveal the
   state word through C8's own shedding order rather than a second row.
6. **Shape verbs refuse; they do not exit-then-apply.** C21 exits zoom
   before a structural chord so "the layout never changes invisibly". Solo
   is a choice the user persisted; exiting it because they pressed `Alt+o`
   would be the surprise. Refusing and naming the way out (C38) keeps both
   principles.
7. **The rail is always drawn in solo, even for one pane.** VS Code hides
   its list for a single terminal by default. Here the rail (and its header)
   is the only on-screen evidence of the view besides the word slot — U15's
   lesson — and a toggle that visibly does nothing on a fresh tab would read
   as broken.
8. **`Alt+↑/↓` clamp rather than wrap.** Matches the stack and every other
   in-tab move; a wrap would make a held key spin. `Alt+m` wraps because the
   strip is a ring; the rail is a list with a top.
9. **Lists the tab's panes in `pane_order`, not worst-first.** The roster
   sorts worst-first because it is a search surface over a whole fleet;
   the rail is a stable place you *return* to, and rows that reorder
   themselves under `Alt+↓` would be the roster's own "cursor re-points at a
   different pane" bug in a surface with no cursor to protect. Attention
   still shows: the ◆ is on its row, and `Alt+a` goes to it.

## 4. Peer precedent (what was checked)

- **herdr** — the sidebar the tribunal evaluated: agent state (blocked /
  working / done / idle) rolled up workspace → tab → pane, always on,
  beside tiles. roost's rail borrows the *rows* idea, scopes it to one tab,
  and shows it only when there are no tiles to squeeze.
- **VS Code integrated terminal** — a tabs list beside the terminal (right
  by default, `terminal.integrated.tabs.location`), one terminal or split
  group shown at a time, each row icon + title + description + status
  decoration, hidden with a single terminal by default
  (`terminal.integrated.tabs.hideCondition`). The closest mainstream
  precedent for "list beside one terminal"; §3.7 says why the hide default
  is not copied.
- **claude-squad / Conductor / Crystal / the Claude Code desktop redesign**
  — the "list of sessions + the selected one" shape is now the default in
  every agent runner. That is the user population roost serves.
- **iTerm2** — tab bar on the left is a shipped option (vertical tabs in a
  terminal are not exotic).
- **zellij** — stacked panes (roost's ancestor) and no sidebar; **tmux** —
  `choose-tree`, which is roost's roster.

## 5. What this deliberately does not do

- No fourth `Alt+g` stop, no `Alt+g` inside solo (refused). If demand
  appears, `Alt+g` could *also* enter solo as a stop that leaves the tree
  alone — argue it as a C25 amendment then.
- No per-row actions in the rail (close, rename, send): the roster's own
  "jump is the only action" scope statement applies for the same reasons.
- No drag-to-reorder (§2.5). No rail on the right, no configurable side —
  zero-config stance.
- No workspace-wide rail. The parked fleet rail (ROADMAP) is still parked and
  still distinct; §7.
- Not a `Mode`: solo owns no keys of its own, so the `Mode` enum, the
  `every_mode_variant_has_a_chrome_buffers_fixture` gate and `modal_active()`
  are untouched. The chrome fixture for a solo tab therefore has to be added
  by hand — it joins C30's "human-must-remember" list.

**[2026-09-10, superseded at the client's request]** The "No fourth `Alt+g`
stop" line above no longer holds. Solo is now the layout cycle's fourth
stop — `grid → main+stack → all-stack → solo → grid` — landed the same day,
argued as the C25 amendment this section itself said such a change would
need. See C25's and C43's dated amendments in DESIGN-ui.md for the shape,
the fit rule (solo always fits, so an unfit tiled tab lands there instead of
refusing), and the `layout_cycle` counter sync between the two entry paths.

## 6. Tests — the executable form

Unit (`app.rs` / `layout.rs` / `render.rs` / `mouse.rs`):

- `display_rects` in a solo tab is `[focused @ body-minus-rail]` (float first
  when shown); tier arithmetic pinned at 79/80/99/100/160/200 columns and the
  `clamp(width/5, 20, 32)` corners.
- Toggling solo twice leaves `tab.layout` byte-identical (serde round trip),
  focus unchanged, and a collapsed-member focus expanded on exit.
- Two tabs, one solo and one tiled, switch back and forth without either
  view leaking.
- Serde: a tiled tab writes pre-change JSON byte for byte; a solo tab round
  trips; a file without the key loads tiled (the `StackOrigin` test shape).
- `Alt+↑/↓` step `pane_order` and clamp; `Alt+←/→` cross tabs on the first
  press; `Alt+Shift+↑/↓` swap and persist and are the same swap in tiled.
- Each refused chord leaves the tree byte-identical and flashes the
  contracted message (C38's own test shape); `Alt+n` in solo uses the
  drawability guard and refuses the comfort floor nowhere.
- Close-in-solo lands on the row below, else above.
- Zoom over solo hides the rail; `Alt+z` again restores it; a tab change
  exits zoom and returns to solo-with-rail.
- Rail overflow scrolls to the shown row with `…` at each hidden end.
- Mouse: row click focuses (one press), header click no-op, wheel never
  focuses, wheel scrolls only an overflowing rail.
- Hint pairs and the word-slot precedence (`ZOOM` > `SOLO` > `NORMAL`);
  `every_bound_chord_is_documented_in_the_keymap` picks up the new row.
- The layout invariant fuzzer gains `ToggleSolo` and the solo step/reorder
  actions in its random sequence, so the "focused member is always
  expanded" and tree/panes-map invariants are checked under it.

PTY (`tests/`): enter solo on a four-pane tab, step with `Alt+↓`, assert the
rail marker moves and the pane rect does not; close a pane and assert the
row count; quit, relaunch against the saved `workspace.json`, assert the
tab comes back solo on the same pane — the resurrection case is the reason
the field is on `Tab`.

Design-supervisor audit after the `src/ui/**` change, per CLAUDE.md.

## 7. Contracted splits (so surfaces don't drift)

> **The tab strip (C2) answers *which tab*.**
> **The rail answers *which pane in this tab* — and only while the tab is
> solo.**
> **The roster (C27) answers *which pane anywhere*, and only when asked.**
> **The fleet rail (ROADMAP, parked) would answer *which project, which
> agents in it* — always on, beside tiles.**
>
> A change that gives the rail other tabs' panes, or the roster a resident
> column, is a merge of two of these and must be argued as one.

## 8. Open decisions for the maintainer

1. **Chord**: `Alt+Shift+t` (recommended) or `Alt+p` (re-argues C23's
   reservation).
2. **Labelled-tier width**: `clamp(width / 5, 20, 32)` (recommended —
   progressive, one line) or a fixed 24.
3. **Exit guard floor**: `every_pane_is_drawable` alone, or that plus the
   vt100 underflow minimum the blit guard (C18) needs — an implementation
   check, flagged so it is not discovered by a crash.
4. **`Alt+g` in solo**: refuse (recommended) or allow it to rearrange the
   hidden tree.
