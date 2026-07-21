# DESIGN-ui.md — roost TUI chrome restyle

Canonical design reference: `docs/tui-design.html` (tokens at `:root` ~line 584,
terminal markup ~614–696, token legend ~698). This document translates that
mockup into testable contracts for a ratatui 0.29 cell-grid TUI. The
`design-supervisor` agent audits implementation against the numbered contracts
below (C1–C18) and issues a per-contract verdict: **ALIGNED** or **DEVIATED**
(with file:line evidence). Line anchors below were verified against the working
tree on 2026-07-21 and may drift a line or two; the code element named is the
anchor, not the number.

---

## 1. Design thesis

> **Chrome is ink · paper · one red; program output keeps its own colors.**

Everything roost draws itself (tab bar, borders, badges, stack chrome, hint
bar, modals) uses exactly one accent hue plus a warm gray ramp. Everything a
program draws inside a pane passes through the vt100 blit byte-faithfully and
keeps its own palette. Red is never decoration: it means either *focus* or
*roost wants your attention* (live keys, needs-input, working pulse, failure).

---

## 2. Token table

| Token | Hex | ratatui expression | Where used in chrome |
|---|---|---|---|
| `BG` | `#15120f` | `Color::Rgb(21, 18, 15)` | "paper". Active tab cell bg is `Color::Reset` (see stance below); `BG` itself is referenced only if a surface ever needs an explicit paper fill. |
| `FG` | `#eae5df` | `Color::Rgb(234, 229, 223)` | primary ink: active tab label, waiting glyph ○, modal titles/body, dead-pane bar text, flash text, working/needs-input collapsed row names |
| `MUTED` | `#8f8983` | `Color::Rgb(143, 137, 131)` | secondary ink: inactive tab labels, corner-badge text, hint labels, picker unselected rows, help descriptions, waiting/idle collapsed row names |
| `DIM` | `#57524b` | `Color::Rgb(87, 82, 75)` | tertiary ink: idle glyph ·, tab-bar right status, stack header, collapsed-row right text, exited row names, hint-bar mode word |
| `RULE` | `#332f2a` | `Color::Rgb(51, 47, 42)` | structure: unfocused pane borders, tab separators `│`, focused collapsed-row bg, flash bg |
| `ACCENT` | `#ff563c` | `Color::Rgb(255, 86, 60)` | the one red: focused pane border, active-tab marker `▎`, hint keys, ◆ needs-input, ● working (pulse phase A), modal borders, "◆ N needs you", "save failed" |
| `ACCENT_DIM` | `#a83a28` | `Color::Rgb(168, 58, 40)` | ✕ exited glyph, expanded-stack edge `▌`, ● working pulse phase B, dead-pane action bar bg, alt-warning bg |
| `TAB_STRIP` | `#1b1713` | `Color::Rgb(27, 23, 19)` | tab bar row bg (inactive cells + fill + right status area) |
| `BAR` | `#211d19` | `Color::Rgb(33, 29, 25)` | hint bar row bg |
| `ok` | `#7fae7f` | — not defined in theme | **no chrome role.** Program-output palette in the mockup only. Must not appear in `src/ui/`. |
| `warn` | `#d8a657` | — not defined in theme | **no chrome role.** Same rule. (Alt-warning uses `ACCENT_DIM`, not warn.) |
| `info` | `#8fb2c9` | — not defined in theme | **no chrome role.** Same rule. |

All nine chrome tokens live as `pub const` in a new `src/ui/theme.rs` (C1).
The ok/warn/info trio is deliberately **not** defined there — defining unused
consts invites casual reuse and dilutes the one-red rule.

### Truecolor stance

**Truecolor required; modern-terminal target; no fallback palette.**

- All chrome colors are `Color::Rgb(..)`. No named-ANSI variants, no palette
  detection, no config (zero-config stands).
- Justification: the design's identity is exact hue discipline (three grays
  seven RGB points apart carry the whole hierarchy). A named-ANSI fallback
  would re-import the user-theme variance this redesign exists to remove, and
  roost's audience (people multiplexing AI-agent CLIs) runs modern emulators
  (iTerm2, kitty, WezTerm, Alacritty, Ghostty, Windows Terminal — all
  truecolor).
- Degradation position: on non-truecolor terminals (notably stock macOS
  Terminal.app, which quantizes RGB SGR to the 256 palette) colors approximate;
  near-neighbors `BG`/`TAB_STRIP`/`BAR` may collapse to one shade. Acceptable:
  the fg ramp (FG/MUTED/DIM) and the accent survive quantization, and nothing
  breaks functionally. Document "best on a truecolor terminal with a
  `#15120f`-family background" in the README; ship no code path for it.

### Background policy (paper stance)

roost does **not** repaint the terminal background. The `#15120f` paper is
assumed to be (approximately) the user's terminal background; program cells
with default bg continue to show terminal-default through the blit. Chrome
sets an explicit bg **only** on: the tab bar row (`TAB_STRIP`), the hint bar
row (`BAR`), the flash (`RULE`), the alt-warning and dead-pane action bars
(`ACCENT_DIM`), and the focused collapsed row (`RULE`). The active tab cell bg
is `Color::Reset` so it visually fuses with the body, whatever the terminal bg
is — the mockup's "active tab shares the body background" effect without
assuming we own the background. (SPEC-GAP-4 below.)

### Bold policy

Chrome uses **no `Modifier::BOLD` anywhere** (the mockup's TUI region is
regular weight throughout; color carries hierarchy). Current BOLD uses (brand
block, active tab, glyphs, dialog borders, help keys, hint keys, flash,
focused border) are all removed. Modifiers still permitted: `DIM` (modal
backdrop mechanism, C12), `REVERSED` (copy selection, C17), `UNDERLINED`
(stack header rule, C6). Program output keeps whatever attributes it sent.

### Glyph inventory (chrome)

`●` U+25CF · `◆` U+25C6 · `○` U+25CB · `·` U+00B7 · `✕` U+2715 · `▎` U+258E
(active-tab / focused-row marker) · `▌` U+258C (expanded-stack edge) · `│`
U+2502 (tab separator) · `▏` U+258F (rename cursor, existing) · `❯` U+276F
(picker selection) · `✓` U+2713 (saved) · `…` U+2026 (tab overflow). All are
single-width. The double-width `🪶` is removed with the brand block (C2),
eliminating the wide-glyph offset hazard in mouse math.

---

## 3. Component contracts

Verdict rule for the auditor: every "Target" bullet is a predicate; a contract
is ALIGNED iff all its bullets hold in the rendered output / code.

### C1 — Theme module (tokens centralized)

**Current:** no theme module; every style is inline in `src/ui/render.rs`
(e.g. `:50, :60, :102–104, :146, :185, :229–231, :243–249, :274–289, :344–347,
:379, :392–397`).

**Target:**
- `src/ui/theme.rs` exists, exporting: the nine chrome color consts from the
  token table; the status-glyph mapping (C5); the pulse phase function (C5);
  and the chrome glyph consts listed above.
- `grep -n 'Color::' src/ui/render.rs` matches only inside the vt100 blit
  section (`conv_color` / `cell_style`, currently `:470–499`) — all chrome
  styles are built from `theme::` items.
- No `Color::` literal for ok/warn/info hues exists anywhere under `src/ui/`.

### C2 — Tab bar

**Current:** `render.rs:240–266` — `" 🪶 roost "` brand block (Black on
Yellow, BOLD) then per-tab `"  {glyph} {N} {name}"`; active = Yellow BOLD,
inactive = DarkGray; no separators; no right status; no bar bg. Hit math:
`mouse.rs:27–31` (`TABBAR_PREFIX`, `TABBAR_PREFIX_WIDTH = 10`), `tab_label`
`:59–61`, `tab_width = 4 + label` `:65–67`, `tab_at_x` `:70–80`; click routed
at `main.rs:306–309`; tests `mouse.rs:250–269`.

**Target:**
- Brand block removed entirely. `TABBAR_PREFIX` / `TABBAR_PREFIX_WIDTH` and
  the prefix-width test (`mouse.rs:262–269`) are deleted. Tabs start at x=0.
- Row 0 is filled edge-to-edge with bg `TAB_STRIP` (including empty middle).
- Each tab i renders as 7 parts, in order:
  `marker(1) + " " + label + " " + glyph(1) + " " + "│"(1)` where
  `label = tab_label(i, name) = "{i+1} {name}"` (function unchanged).
  - marker: `▎` fg `ACCENT` for the active tab; `" "` for inactive.
  - label + number: fg `FG` for active, fg `MUTED` for inactive; active cell
    bg = `Color::Reset`, inactive bg = `TAB_STRIP`. No BOLD.
  - glyph (aggregate `TabSummary`, semantics unchanged from `app.rs:448–476`):
    NeedsInput `◆` `ACCENT` · Working `●` `ACCENT` + pulse (C5) · Unknown `·`
    `DIM` · Waiting `○` `FG` · Quiet = single space.
  - separator `│` fg `RULE`, drawn after every tab (including the last).
- `mouse::tab_width(i, name)` returns `label.chars().count() + 6` and the
  renderer emits exactly that many columns per tab; `tab_at_x` starts at 0.
  Worked example (audit fixture): tabs `["main", "api"]` → tab 0 occupies
  cols 0..12, tab 1 cols 12..23; `tab_at_x(_, 11) == Some(0)`,
  `tab_at_x(_, 12) == Some(1)`, `tab_at_x(_, 23) == None`.
- Right-aligned status area, fg `DIM` on `TAB_STRIP`, content
  `"{cwd} · {save}"` + one trailing space:
  - `cwd` = focused pane's `PaneSpec.cwd` with a `$HOME` prefix abbreviated to
    `~`; segment omitted when the focused pane has no spec.
  - `save` = `"saved ✓"` fg `DIM` while the last workspace save succeeded
    (startup counts as saved — disk state is what we loaded), or
    `"save failed ✕"` fg `ACCENT` after a save error. Honest signal: `App::save`
    (`app.rs:210–212`) currently discards the `Result`; it must record success
    into a `last_save_ok: bool` consulted here. No other semantics change.
- Overflow (≥10 tabs or narrow terminal): tabs render left-to-right; the first
  tab that would collide with the status area (or the bar edge) is not drawn —
  a single `…` fg `MUTED` marks the clip point. Labels keep real numbers
  (`"10 name"`); Alt+1..9 still reaches only tabs 1–9 (input unchanged); any
  *visible* tab is clickable. Clicks on the `…` cell or the status area must
  not switch tabs (clamp at the `main.rs:309` call site or inside `tab_at_x`).
  If tabs + status collide, the status area is dropped first (tabs win).
- Mouse unit tests (`mouse.rs:250–260`) are rewritten to the new offsets **in
  the same change** as the renderer (lockstep rule, §4).

### C3 — Pane borders

**Current:** `render.rs:344–351` — focused = status-colored + BOLD, unfocused
= DarkGray; default `BorderType` (Plain); border title line `" {glyph} {name}
[scroll] "` built at `:294–331` and attached via `.title(...)` at `:349`.

**Target:**
- `BorderType::Plain` (single-line) for every pane border. No BOLD.
- Focused pane border fg = `ACCENT` — focus color no longer varies with
  status (status lives in the glyph system, C5).
- Unfocused pane border fg = `RULE`.
- The border-embedded title line is **removed** (no `.title(...)` on pane
  blocks). Pane identity moves entirely to the corner badge (C4); the
  `[scroll]` tag is dropped — the hint bar's mode word `SCROLL` (C9) covers it.

### C4 — Corner badge

**Current:** `render.rs:376–383` — fg DarkGray, top-right, text = pane name
only, **suppressed when focused**; pure helper `corner_badge()` `:409–424`
with tests `:530–555`.

**Target:**
- Drawn on **every** non-collapsed pane, focused included (suppression branch
  at `:376` removed; occlusion of the inner app's top-right cells is accepted
  by design).
- Content: `"{name} · {adapter} {glyph}"` — where `name` is the existing
  display name (`title`, else the adapter/cwd fallback built at `:304–312`).
  When the pane has no custom title the fallback already contains the adapter,
  so the ` · {adapter}` segment is skipped (no `"pi · repo · pi"` dup):
  untitled badge = `"{name} {glyph}"`.
- Style: text fg `MUTED`; glyph fg per C5 status colors, pulsing when Working.
- Geometry: top row of the pane's inner area, right-aligned, one column of
  right breathing room — `corner_badge()` clipping behavior and its tests
  stay (helper may evolve to return spans for the two-tone styling).

### C5 — Status glyph system + pulse

**Current:** glyphs from `status.rs:34–42`; colors `render.rs:282–290`
(Working Green, NeedsInput Magenta, Waiting Yellow, Idle DarkGray, Exited
Red); tab variant `:271–280`; no animation.

**Target — one table, used by every chrome surface that shows status
(tab bar, corner badge, collapsed rows):**

| AgentStatus | Glyph | Color | Pulse |
|---|---|---|---|
| Working | `●` | `ACCENT` | **yes** |
| NeedsInput | `◆` | `ACCENT` | no (steady) |
| Waiting | `○` | `FG` | no |
| Idle | `·` | `DIM` | no |
| Exited | `✕` | `ACCENT_DIM` | no |

TabSummary variant: NeedsInput/Working/Waiting as above; Unknown `·` `DIM`;
Quiet renders a single space (both semantics unchanged).

**Pulse spec:** period **1100 ms**, 50% duty, two phases: elapsed-ms in
`[0, 550) → ACCENT`, `[550, 1100) → ACCENT_DIM`, repeating. Phase is computed
from one shared clock (elapsed since app start), so **all pulsing glyphs flip
in unison**; it is re-evaluated every frame and the ~33 ms draw tick
(`main.rs:164–173`) bounds phase-edge error to one frame. No new timers, no
extra redraw scheduling. Only Working `●` pulses — never `◆` (steady red =
"waiting on you", pulsing red = "alive"). Pure function
(`theme::pulse_phase(elapsed) -> Color` or equivalent) with unit tests at the
boundaries: 0 → ACCENT, 549 → ACCENT, 550 → ACCENT_DIM, 1100 → ACCENT.

### C6 — Stack header row

**Current:** none — stack members are laid out directly (`layout.rs:302–322`);
nothing announces "this region is a stack".

**Target:**
- Each `LayoutNode::Stack` reserves **one** header row at the top of its area
  when the area is tall enough: shown iff `stack_area.height >= n + 3`
  (n members: header 1 + collapsed n−1 + expanded ≥ 3). Below that threshold,
  geometry is exactly today's (header omitted).
- With the header shown, the expanded member's height is
  `area.height − (n−1) − 1`; collapsed bars keep 1 row each. Geometry is
  produced by `compute_rects` (`layout.rs:296–346`) or a parallel walk emitting
  header rects; the header is **not** a `PaneRect` — clicks on it hit no pane
  (`hit_test` returns None) and it forwards nothing. PTY resize follows the
  shrunken expanded rect automatically.
- Content: left `" STACK · {n} PANES"`, right `"ALT+↑↓ "` (right-aligned) —
  uppercase, fg `DIM`, no bg.
- Every cell of the header row (both texts and the fill between) carries
  `Modifier::UNDERLINED` — the cell-level translation of the mockup's 1px
  bottom rule.
- `layout.rs` unit tests updated for the new stack geometry in the same change.

### C7 — Expanded-member edge marker

**Current:** the expanded stack member renders as an ordinary bordered pane
(`render.rs:344–351`); nothing distinguishes "expanded member of a stack".

**Target:**
- **Unfocused** expanded stack member: normal `RULE` border (C3), then its
  left border column (`x = rect.x`, all `rect.height` rows) is overpainted
  with `▌` fg `ACCENT_DIM` — the cell translation of the mockup's 2px
  `--tui-red-dim` left edge (a half-block reads "thicker than a 1px line").
  The left edge consequently shows no corner joints; accepted.
- **Focused** expanded stack member: full `ACCENT` border, **no** marker —
  focus is the stronger signal and adjacent red-on-red (ACCENT frame +
  ACCENT_DIM edge) would smear the one-red discipline.
- Applies only to expanded members of a `Stack` node; ordinary split panes
  never get the marker.

### C8 — Collapsed stack rows

**Current:** `render.rs:333–342` — 1-row Paragraph, text `" {glyph} {name} "`;
focused = Black on status-color bg, unfocused = status-color fg.

**Target:**
- Row format: `marker(1) + glyph(1) + " " + name + fill + "{adapter} · {word}" + " "`
  with the right segment right-aligned.
  - marker: `▎` fg `ACCENT` when the row is the focused pane, else `" "`.
  - glyph: per C5 (color + Working pulse).
  - name fg by state: Working/NeedsInput → `FG`; Waiting/Idle → `MUTED`;
    Exited → `DIM`.
  - right segment fg `DIM`; adapter = `PaneSpec.adapter`.
- State-word mapping from `AgentStatus`:
  | Status | word |
  |---|---|
  | Working | `working` |
  | NeedsInput | `needs you` |
  | Waiting | `your turn` |
  | Idle | `idle` |
  | Exited | `exited` |
  The mockup's `exited 130` (exit code) is **not** implementable honestly
  today — no exit-code plumbing exists (`status.rs` tracks only a bool;
  `on_pty_exit`, `app.rs:921`, carries no code). Spec is `exited` bare;
  see SPEC-GAP-1.
- Focused row additionally paints bg `RULE` across the full row width;
  unfocused rows have no bg.
- When the row is too narrow, the right segment drops first (name clips last).
- Click-to-expand behavior unchanged.

### C9 — Hint bar

**Current:** `render.rs:44–108` — key chips Black-on-DarkGray BOLD, labels
Gray, no bar bg, no right segment. Normal-mode list has 10 pairs (`:84–95`).

**Target:**
- Row bg `BAR` edge-to-edge (the mockup's `border-top` rule is dropped — no
  spare row; the bg step provides the separation).
- Pairs render as `" {key} {label}  "`: key fg `ACCENT` (no chip bg, no BOLD),
  label fg `MUTED`.
- Normal-mode pairs, exactly these seven (mockup-curated; the dropped
  bindings remain discoverable via Alt+?):
  `Alt+n new` · `Alt+↵ launch` · `Alt+s stack` · `Alt+←↓↑→ focus` ·
  `Alt+r rename` · `Alt+w close` · `Alt+? keys`.
  (Deviation from mockup literal: `Alt+←↓↑→` instead of `Alt+↑↓` — focus
  moves four ways; mode-specific lists below are unchanged in content.)
- Other modes keep their current pair lists (`:68–83`), restyled identically
  (dead-focused Normal list `:81–83` included).
- Right-aligned segment, drawn only when it fits after the hint spans
  (hints win on narrow widths): `"◆ {N} needs you"` fg `ACCENT` — N = count
  of panes whose runtime status is `NeedsInput` across **all** spawned panes
  (every entry in `app.runtimes`, not just the active tab); segment omitted
  when N = 0 — then two spaces, then the mode word fg `DIM`, uppercase:
  `NORMAL` / `RENAME` / `PICKER` / `SCROLL` / `COPY` / `HELP`, then one
  trailing space.
- Precedence unchanged: alt-warning (C11) takes the bar over flash (C10),
  which takes it over hints (`:45–64`).

### C10 — Flash message

**Current:** `render.rs:56–64` — Black on Green BOLD.

**Target:** `" {msg} "` fg `FG` on bg `RULE`, no modifiers. (Flash carries
generic notices — "copied", extension updates — so it gets the neutral
elevated treatment, not a reserved color; ok-green is banned from chrome.)
Timing/precedence unchanged.

### C11 — Alt-key warning

**Current:** `render.rs:45–54` — Black on Yellow.

**Target:** same text, fg `FG` on bg `ACCENT_DIM` — "roost-level problem"
bars (this and the dead-pane bar, C16) share the dim-accent treatment. The
mockup's `warn` yellow is program-output-only and must not be used.

### C12 — Modal system (shared)

**Current:** `dialog_border_style()` `render.rs:145–147` = Cyan BOLD;
`BorderType::Double` at `:163, :176, :219`; dim backdrop `:126–141`; anchor
via `centered_near()` `:114–122`.

**Target — the derived system all three modals follow (mockup shows no
modals; derived from its rules):**
- `BorderType::Plain` single-line border, fg `ACCENT`, no BOLD (Cyan and
  Double are gone). Rationale: a modal is the focused interaction surface, so
  it takes the focus color; the dimmed backdrop prevents confusion with the
  focused pane's accent border.
- Title text (e.g. `" rename pane "`) fg `FG`, regular weight.
- Interior: `Clear` then default bg (no explicit paper fill — background
  policy §2).
- Backdrop: `Modifier::DIM` on every body cell outside the dialog —
  mechanism unchanged (`:126–141`).
- Anchoring: `centered_near(anchor, body, w, h)` unchanged, including its
  tests (`:507–527`).

### C13 — Rename dialog

**Current:** `render.rs:153–168` — 44×3, Double/Cyan, plain input text.

**Target:** C12 frame; input text fg `FG`; cursor stays the `▏` suffix
(`:167`), fg `FG`. Size and behavior unchanged.

### C14 — Picker (quick-launch)

**Current:** `render.rs:169–193` — Double/Cyan; selected row Black on Yellow.

**Target:** C12 frame. Rows render as:
- selected: `"❯ {item}"` — `❯` fg `ACCENT`, item fg `FG`, **no bg highlight**;
- unselected: `"  {item}"` fg `MUTED`.
(The `❯`-prefix selection idiom is lifted from the mockup's approval-prompt
markup, lines ~669–671.) Size and behavior unchanged.

### C15 — Help overlay

**Current:** `render.rs:194–237` — Double/Cyan; keys Yellow BOLD, desc Gray.

**Target:** C12 frame; key column fg `ACCENT` (no BOLD), description column
fg `MUTED` — same key/label system as the hint bar. Content unchanged.

### C16 — Dead-pane overlay

**Current:** `render.rs:387–403` — error line Red fg; action bar Black on Red.

**Target:**
- spawn-error line: `" spawn failed: {err} "` fg `ACCENT`, no bg.
- action bar (bottom row, full inner width): text unchanged
  (`" ✕ exited — Enter: relaunch/resume · f: fresh (drops resume) · Alt+w: close "`),
  fg `FG` on bg `ACCENT_DIM`.
- Placement (bottom rows over preserved last screen) unchanged.

### C17 — Copy-mode selection

**Current:** `render.rs:365–368` + `highlight_selection()` `:428–446` —
`Modifier::REVERSED` per cell.

**Target:** unchanged, and **must stay modifier-based**: selection sits on top
of arbitrary program colors, so it may not assume any palette token. Contract
exists to stop a well-meaning restyle from "theming" it.

### C18 — vt100 blit guard

**Current:** `render.rs:450–499` (`blit_screen`, `conv_color`, `cell_style`).

**Target:** **zero diffs** in these functions for this engagement. Program
output keeps its own colors, attributes, and default-bg passthrough. Any
change here is an automatic DEVIATED.

---

## 4. Pixel-idea translations (explicit)

Every px-only construct in the mockup, and its cell-level fate:

| Mockup construct | Translation |
|---|---|
| 2px `--color-accent` top edge on active tab (`:628`) | `▎` U+258E fg `ACCENT` as the active tab's first column (C2). A 1-row bar has no vertical edge to give; a left quarter-block preserves "one red edge marks the active tab" and survives 256-color quantization. |
| 2px `--tui-red-dim` left edge on expanded stack member (`:662`) | left border column overpainted `▌` U+258C fg `ACCENT_DIM` (C7); half-block ≈ "thicker than 1px". |
| 1px borders throughout | `BorderType::Plain` single-line glyphs (C3, C12). |
| 6px pane gap + 12–14px pane padding (`:636, :639`) | **dropped** — border cells already separate panes; spending whole cell columns on gaps wastes terminal real estate. |
| letterspacing (0.02–0.11em) | **dropped** — no letterspacing in a cell grid; spacing out characters by hand is a gimmick that breaks widths. |
| tab strip `border-bottom` / hint bar `border-top` (1px rules) | **dropped** — no spare rows; the `TAB_STRIP`/`BAR` bg steps carry the separation. |
| stack header `border-bottom` (`:659`) | `Modifier::UNDERLINED` across the header row (C6) — the one place a rule translates to an attribute instead of a row. |
| `tui-pulse` opacity animation (1 → 0.28) | two-phase color flip `ACCENT ↔ ACCENT_DIM`, 1100 ms period (C5) — nearest cell-model equivalent of an opacity dip against `BG`. |
| `tui-blink` block cursor (`:673`) | out of scope — the real cursor belongs to the inner program; roost already positions the hardware cursor (`render.rs:355–362`). |
| emulator chrome row (traffic lights, `:617–624`) | out of scope — OS terminal window chrome. |
| JetBrains Mono font | out of scope — user's terminal font. |

---

## 5. Interaction-preserving notes

- **Tab hitboxes move in lockstep (hard rule).** Renderer tab-cell layout,
  `mouse::tab_width`/`tab_at_x`, and the tests at `mouse.rs:250–269` change in
  the same commit. The prefix-width test dies with the prefix; a new test pins
  the `label + 6` formula and the worked example in C2. Click routing stays at
  `main.rs:306–309`; it gains the status-area/overflow clamp (C2).
- **Dialog anchoring stays.** `centered_near()` (`render.rs:114–122`) and its
  tests are untouched; dialogs keep anchoring to the focused pane, not screen
  center.
- **vt100 blit untouched** (C18). The restyle is chrome-only by construction.
- **Layout contract.** `app.body_area()` (`app.rs:217–220`) is unchanged; the
  stack header consumes a row *inside* the stack's area only (C6), so PTY
  resizing, `hit_test`, and focus math flow through existing paths. The header
  row belongs to no pane: clicks there are dead, wheel events fall through to
  nothing.
- **Collapsed-row click-to-expand and tab click-to-switch semantics are
  unchanged** — only their pixels change.
- **Status semantics unchanged.** `StatusTracker` (`status.rs`), `TabSummary`
  aggregation (`app.rs:448–476`), and decay windows are not touched; this is a
  re-skin of their presentation plus one honest addition (save-result
  tracking, C2).

---

## 6. Supervision

### Per-contract audit

The `design-supervisor` agent runs after any change under `src/ui/**`,
`src/core/layout.rs`, or `src/core/app.rs` (UI-adjacent helpers), and issues
one verdict per contract C1–C18: **ALIGNED** or **DEVIATED** (+ file:line and
the violated bullet). Mechanics per contract class:

- **Greppable predicates** (C1 theme gate, C18 zero-diff, no-BOLD rule,
  banned hues): verify by reading `src/ui/render.rs` / `theme.rs` — e.g.
  `Color::` outside the blit section, `Modifier::BOLD` anywhere in `ui/`,
  any of `7fae7f|d8a657|8fb2c9` under `src/ui/`.
- **Structural predicates** (C2 cell layout + width formula, C6 geometry
  threshold, C5 phase boundaries): verify against the unit tests this plan
  requires (mouse offsets, layout stack rects, pulse phase) — tests are the
  executable form of the contract.
- **Visual predicates** (colors in place, marker glyphs, right-alignment):
  verify by reading the span-construction code; ambiguity → run roost and
  eyeball, or wait for the harness below.

### vt100 golden-frame harness — assessment

**Mechanics (all pieces already in-tree):** roost vendors `vendor/vt100`
(path dep) and already depends on `portable-pty` 0.8. An integration test can
spawn the built binary via the std `CARGO_BIN_EXE_roost` env (no new deps),
inside a portable-pty PTY at a fixed size (e.g. 120×40), with `HOME`/state
dirs pointed at a temp fixture `workspace.json` and `shell`-adapter panes
running scripted `/bin/sh` (deterministic output). Read PTY bytes into a
`vt100::Parser`, then assert cell `fgcolor()/bgcolor()/attrs` at fixed
coordinates: tab-bar row 0 (strip bg, active marker `▎` accent), focused
border cells (accent), hint-bar row (BAR bg, accent keys), corner badge
position.

**Complications:** (1) statuses are time/heuristic-driven — freshly spawned
quiet shells are deterministically `Idle`→`Waiting`; `Working` needs scripted
output (`while sleep …; do echo; done`); `NeedsInput` is hard to script
without the socket — keep it out of golden frames. (2) Pulse phase is
wall-clock — assert pulsing cells ∈ {ACCENT, ACCENT_DIM}, never an exact
phase. (3) Needs a PTY-capable CI runner (fine on macOS/Linux). (4) Frame
settling — poll-parse until stable rather than sleep.

**Effort:** ~1–2 days for the harness plus 3–4 golden scenarios.

**Verdict: feasible, but a follow-up — not a package in this build.**
Rationale: after C1 centralizes tokens, chrome correctness is dominated by
pure span/geometry construction, which the required inline unit tests (mouse
offsets, layout rects, pulse phase, corner-badge clipping) pin far more
cheaply and less flakily. The harness's real payoff is regression armor for
*future* chrome churn; building it now would serialize behind the restyle it
is meant to verify. It is recorded as its own decision item in the plan
(PLAN.md P6) with a revisit trigger: first post-restyle chrome regression, or
the next engagement that touches `src/ui/render.rs`.

---

## 7. Spec gaps & deliberate exclusions

- **SPEC-GAP-1 — exit codes.** Mockup shows `pi · exited 130`; roost has no
  exit-code plumbing (`status.rs` `exited: bool`; `on_pty_exit` carries no
  code). Contracted as bare `exited` (C8). Optional follow-up: thread the
  child's exit status through `PaneBackend`/`on_pty_exit` into `StatusTracker`.
- **SPEC-GAP-2 — no tab-level Exited.** `TabSummary` has no Exited variant, so
  a tab whose panes all exited shows a blank (Quiet) glyph. Semantics
  unchanged by this restyle; flagged for a later product call.
- **SPEC-GAP-3 — collapsed-row task detail.** Mockup's `running build` is
  task-level detail roost doesn't have; the state-word table (C8) is the
  honest substitute.
- **SPEC-GAP-4 — paper assumption.** roost does not repaint the terminal bg;
  the exact mockup look requires a terminal background near `#15120f`.
  Documented stance, no code (see §2).
- **Deliberately left out:** config/theme file (zero-config stands, tokens are
  consts) · named-ANSI fallback palette · tab-overflow scrolling (ROADMAP item;
  C2 clips honestly) · any restyle of program output · golden-frame harness in
  this build (see §6) · letterspacing/gap/padding emulation (§4).
