# DESIGN-ui.md — roost TUI chrome restyle

Canonical design reference: `docs/tui-design.html` (tokens at `:root` ~line 584,
terminal markup ~614–696, token legend ~698). This document translates that
mockup into testable contracts for a ratatui 0.29 cell-grid TUI. The
`design-supervisor` agent audits implementation against the numbered contracts
below (C1–C26) and issues a per-contract verdict: **ALIGNED** or **DEVIATED**
(with file:line evidence). Line anchors below were verified against the working
tree on 2026-07-21 and may drift a line or two; the code element named is the
anchor, not the number.

**Amendment 2026-07-22 (fleet features):** contracts C19–C26, the C9/C15
amendments, the §6 firehose gate, and the §8 key table were added for the
fleet-features engagement (BRIEF: `.claude/company/fleet-features/BRIEF.md`).
Their line anchors were verified against the working tree on 2026-07-22.
C1–C18 are unchanged and must **still** audit ALIGNED after the fleet build —
in particular the C1 grep gates and the C18 zero-diff rule.

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
| `ACCENT_DIM` | `#a83a28` | `Color::Rgb(168, 58, 40)` | ✕ exited glyph, expanded-stack edge `▌`, ● working pulse phase B, dead-pane action bar bg, alt-warning bg, `raw` badge token (C23) |
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
backdrop mechanism, C12), `REVERSED` (copy selection C17 + copy cursor C24),
`UNDERLINED` (stack header rule C6 + copy cursor-in-selection C24). Program
output keeps whatever attributes it sent.

### Glyph inventory (chrome)

`●` U+25CF · `◆` U+25C6 · `○` U+25CB · `·` U+00B7 · `✕` U+2715 · `▎` U+258E
(active-tab / focused-row marker) · `▌` U+258C (expanded-stack edge) · `│`
U+2502 (tab separator) · `▏` U+258F (rename cursor, existing) · `❯` U+276F
(picker selection) · `✓` U+2713 (saved) · `…` U+2026 (tab overflow). All are
single-width. The double-width `🪶` is removed with the brand block (C2),
eliminating the wide-glyph offset hazard in mouse math.

**[Amended 2026-07-22, fleet features]: the fleet features add NO new
glyphs.** The feed (C20) reuses `◆` with its C5 meaning; zoom/raw are word
indications (`ZOOM`/`RAW`, C9) plus a plain-text `raw` badge token (C23); the
float (C22) is chrome-identical to a pane; the copy cursor (C24) is
modifier-based. Any new glyph appearing under `src/ui/` is a DEVIATED.

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
- [Amended 2026-07-22, fleet features] A raw pane's badge additionally carries
  the `raw` token per C23.

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
  - No-dup rule (mirrors C4): when the pane is untitled and its fallback name
    already carries the adapter, the right segment is the state word alone
    (`your turn`, not `shell · your turn`). [Amended 2026-07-22, ux finding #3.]
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
- [Amended 2026-07-22, fleet features] A raw pane's right segment carries the
  `raw · ` prefix per C23.

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
  (hints win on narrow widths): `"◆ {N} needs you · Alt+a"` fg `ACCENT` —
  N = count of panes whose runtime status is `NeedsInput` across **all**
  spawned panes (every entry in `app.runtimes`, not just the active tab);
  segment omitted when N = 0 — then two spaces, then the mode word fg `DIM`,
  uppercase (word list below), then one trailing space.
- Precedence unchanged: alt-warning (C11) takes the bar over flash (C10),
  which takes it over hints (`:45–64`).

**[Amended 2026-07-22, fleet features]:**
- **The Normal-mode seven-pair list gains NOTHING.** Justification: rendered
  per the `" {key} {label}  "` formula, the seven pairs measure exactly
  **100 columns** (12+15+14+17+15+14+13); at the 100-col floor any eighth pair
  pushes real hints off the bar. All six new chords (§8) are discoverable via
  `Alt+?` (C15); the jump chord additionally teaches itself at the moment of
  need via the amended right segment (previous bullet — `· Alt+a` costs zero
  columns when N = 0).
- **Yield order (re-amended 2026-07-22, live-QA finding):** the right segment
  WINS over the static pairs — trailing pairs drop (whole pairs, from the
  right) until the segment fits; the mode word is the last thing on the bar
  to ever disappear. Rationale: the mode word is a modal-safety affordance
  (RAW/COPY/ZOOM must be visible to be escapable) and `◆ N needs you` is the
  fleet's primary signal; both outrank a static cheat-row whose full content
  lives under `Alt+?`. Live QA at 120 cols showed the previous
  hints-win order blanked the mode word whenever N > 0. Predicate: at any
  width, `mode word visible` unless width < word length; pairs render
  left-to-right in list order, each dropped whole when it no longer fits
  alongside the right segment.
- **Mode-word list** (`mode_word`, `render.rs:99–108`) becomes:
  `NORMAL / RENAME / PICKER / SCROLL / COPY / HELP / FEED` — plus two
  **pseudo-state words** shown only in the Normal slot:
  `RAW` when the focused pane is raw (C23), `ZOOM` when zoomed (C21).
  Precedence: a real non-Normal mode word always wins; else `RAW` beats
  `ZOOM` beats `NORMAL` (input safety trumps view state).
- **New / amended pair lists** (all obey the same styling formula):
  - Feed mode (C20): `↑↓ scroll` · `Esc close`.
  - Focused-raw Normal (C23): exactly **one** pair — `Alt+Shift+p exit raw`.
    Every other hint would be a lie: nothing else is intercepted.
  - Copy mode (C24, replaces the two-pair list at `:70`):
    `hjkl move` · `v mark` · `y/↵ yank` · `drag select` · `Esc exit`
    (63 columns — fits beside the right segment at 100 cols).
  - Zoomed Normal keeps the standard seven pairs (they all still work).

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
- [Amended 2026-07-22, fleet features] The feed overlay (C20) is a fourth
  C12 modal. Modals are the **topmost** chrome layer — above the float pane
  (C22) and the zoomed view (C21); stacking order is contracted in C22.

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
Width fits the widest content line (key column + longest description),
clamped to screen bounds; anchoring via `centered_near` unchanged — no
mid-word clipping of its own content. [Amended 2026-07-22, ux finding #1;
the fixed 52-col width predates the restyle and clipped descriptions.]

**[Amended 2026-07-22, fleet features]:** "content unchanged" is superseded —
the help overlay's row list is now pinned to the **§8 key table** and grows by
exactly six rows (Alt+a, Alt+e, Alt+z, Alt+f, Alt+Shift+p, Alt+g — wording per
§8). **Hard cap: ≤ 20 content rows.** Arithmetic: at the 80×24 floor the body
is 22 rows; 20 content + 2 border = 22 — the overlay fits exactly with zero
slack. The current list is 14 rows (`render.rs:301–316`) + 6 = 20. Any future
chord must merge into an existing row (the `Alt+t / Alt+1..9` idiom), never
add a 21st.

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
exists to stop a well-meaning restyle from "theming" it. (The keyboard copy
cursor, C24, extends this rule — modifiers only.)

### C18 — vt100 blit guard

**Current:** `render.rs:450–499` (`blit_screen`, `conv_color`, `cell_style`).

**Target:** **zero diffs** in these functions for this engagement. Program
output keeps its own colors, attributes, and default-bg passthrough. Any
change here is an automatic DEVIATED. [Re-affirmed 2026-07-22 for the fleet
build: the feed, float, zoom, raw, and copy-cursor features all draw *around*
the blit, never inside it.]

---

### C19 — Jump-to-attention (Alt+a) — [Added 2026-07-22, fleet features]

**Current:** no binding — unmatched Alt chords are swallowed
(`input.rs:72–77`); the hint bar's `◆ N needs you` segment
(`render.rs:114–123`) is informational only; reaching a needy pane in another
tab takes Alt+{digit} then Alt+arrows.

**Target:**
- New `Action::JumpAttention`, bound to **Alt+a** in `translate()`
  (`input.rs:39–81`). Mnemonic: *attention*. (Collision note: zsh's emacs
  keymap binds ESC-a to accept-and-hold — already swallowed by roost today;
  raw mode, C23, is the remedy for panes that want it.)
- **Attention ring** R: every pane whose runtime status is `NeedsInput` —
  the *same* predicate as `needs_input_count` (`app.rs:526–528`), so the ring
  size always equals the hint bar's N (the affordance never lies). Ordered by
  `(tab index ascending, position in that tab's pane_order())`; the float
  pane (C22), if needy, is last. Nothing else is in the ring — not Exited,
  not Working: worst-first across roost's statuses means ◆ is the only
  actionable severity, and the ring must match the advertised count.
- Press semantics (deterministic, unit-tested):
  - R empty → flash `nothing needs you` (flash mechanism `app.rs:1169–1179`);
    no other state changes.
  - Else focus the first member of R whose ring position is strictly after
    the focused pane's position, **wrapping** past the end to R[0] (the ring
    wraps by contract); if the focused pane is the only member, flash
    `nothing else needs you` and stay.
  - Repeated presses visit every member in order and wrap.
- Jump mechanics reuse existing paths: a cross-tab jump switches the active
  tab with `go_to_tab` semantics (`app.rs:1485–1491`, lazy spawn included);
  any jump expands the target in its stack (`expand_in_stacks`, as focus
  moves do at `app.rs:1364–1370`); a jump to the float shows it (C22).
- Zoom interplay (C21): a same-tab jump keeps zoom (zoom follows focus); a
  cross-tab jump exits zoom (tab-switch rule).
- Hint-bar affordance: the amended C9 right segment
  (`◆ {N} needs you · Alt+a`) is this feature's discoverability surface; no
  Normal-mode pair is added (C9 amendment, justification there).
- Unit tests: ring order across two tabs; wrap; focused-not-in-ring; empty
  ring flash; count == ring size.

### C20 — Activity feed (Alt+e) — [Added 2026-07-22, fleet features]

**Current:** no surface. The data exists but scattered: control actions are
audit-logged (`app.rs:675–695` → `<state>/control.log`), status arrives via
extension events (`app.rs:1081`) or is polled from `StatusTracker`
(`status.rs:99–158`), exits via `on_pty_exit` (`app.rs:1058`).

**Target:**
- **Surface: a C12 modal overlay, not a persistent pane.** Rationale: at the
  80×24 floor there is no spare row for a persistent strip (the C6 height
  rules show how expensive reserved rows are), a pane would churn PTY resizes
  on every toggle, and the ~33 ms draw tick already makes an open overlay
  *stream live* — entries appended while it is open appear on the next frame.
  Trade-off accepted: keys are captured while it is open (it is a monitoring
  glance, not a workspace).
- New `Mode::Feed { offset: usize }` (`Mode` enum, `app.rs:49–58`), entered
  by **Alt+e** (`Action::ToggleFeed`) from Normal. Mnemonic: *events*. Mode
  word `FEED` (C9). Keys while open: `Esc`/`q`/`Alt+e` close; `Up`/`k`,
  `Down`/`j` scroll one entry; `PgUp`/`PgDn` scroll half the overlay height;
  `offset` counts entries back from the newest, clamped to the buffer;
  offset 0 follows the live tail. All other keys are consumed
  (`handle_mode_key` pattern, `app.rs:1496`).
- **Ring buffer:** `VecDeque<FeedEntry>` on `App`, capacity **200**, oldest
  evicted first. Session-only — never persisted. An entry is
  `(SystemTime, kind, text)`; text is preformatted at push time.
- **Event taxonomy — exactly five kinds, hooked at these seams:**

  | kind | hook (single source each — no double reporting) | line text |
  |---|---|---|
  | `status` | the 2 s housekeeping tick (`app.rs:367–401`): App keeps a last-known-status map for spawned panes and diffs it *before* the `pending_detect` early-return. One source for all transitions — extension-pushed and heuristic alike — at ≤ 2 s granularity (documented; a sub-2 s flicker may be missed, accepted). Transitions **to Exited are suppressed** here (the `exit` hook owns that). First observation of a pane logs nothing (`spawn` owns birth). | `{name}: {old} → {new}` using the C8 state words |
  | `spawn` | `spawn_pane` success (`app.rs:349–358`) — covers Alt+n, picker, control spawn/fork, undo restore, respawn | `spawned {name} ({adapter})` |
  | `close` | `close_pane_id` (`app.rs:980–1028`) and `undo_close` (`app.rs:1309–1333`) | `closed {name}` / `closed tab {name}` / `reopened {name}` / `reopened tab {name}` |
  | `exit` | `on_pty_exit` (`app.rs:1058–1072`), including the focused pane (unlike the notifier) | `{name} exited` |
  | `ctl` | `audit()` (`app.rs:675–695`, becomes `&mut self`; feed push happens even when there is no socket dir) — the same sanitized `method_summary` as `control.log`, so broadcast and every other control verb land here with zero extra code | `ctl {principal}: {summary} → {ok\|err}` |

  Session-detection events are deliberately excluded (noise, not action).
- **Geometry:** centered on `body_area()`;
  `w = min(72, body.width − 4)`, `h = min(16, body.height − 4)`; C12 frame
  (Plain `ACCENT` border, title `" activity "`, `Clear` interior, DIM
  backdrop). At the 80×24 floor: 72×16, fits.
- **Entry rows**, newest at the bottom, one row per entry (no wrap; the
  paragraph clips long lines at the overlay width):
  `" HH:MM:SS  {text}"` — timestamp (local wall clock) fg `DIM`, text fg
  `MUTED`. Exception: a `status` line whose new state is NeedsInput renders
  its text fg `FG` prefixed with `◆ ` fg `ACCENT` — the one red in the feed,
  same meaning as everywhere (C5).
- **Empty state:** a single centered line `no activity yet` fg `DIM`.
- Unit tests: ring cap eviction at 200; taxonomy hooks fire (status diff on
  tick, ctl entry on an audited control call, exit-suppression rule); the
  NeedsInput-line styling rule; offset clamping.

### C21 — Pane zoom (Alt+z) — [Added 2026-07-22, fleet features]

**Current:** no zoom. Rendering walks every `PaneRect` of the active tab
(`render.rs:48–51` via `app.rects()`, `app.rs:283–287`); PTY sizing and mouse
hit-testing walk the same list (`app.rs:1118–1128`, `main.rs:328–329`).

**Target:**
- New `Action::ToggleZoom`, bound to **Alt+z** (`translate()`). Mnemonic:
  *zoom* — tmux's `prefix+z` heritage. App-level `zoomed: bool`,
  session-only, never persisted.
- **Semantics: zoom is a pure view transform.** The layout tree is untouched.
  While zoomed, the renderer, PTY-resize, and mouse paths consume a display
  list containing exactly one entry: the focused pane at the full
  `body_area()` (a `display_rects()` accessor beside `rects()`); the focus
  math (`layout::neighbor`, `app.rs:1364–1370`) keeps using the real tree.
  Consequences, all contracted:
  - The zoomed pane draws with its normal chrome — `ACCENT` focused border
    (C3) and corner badge (C4). Stack headers (C6) are not drawn while
    zoomed. Tab bar and hint bar are unaffected (outside the body).
  - Focus moves still work: Alt+arrows/hjkl (and same-tab Alt+a) move focus
    through the real layout and the zoomed view then shows the newly focused
    pane — **zoom follows focus** (zellij-style; deliberate deviation from
    tmux's unzoom-on-switch: zoom stays a stable one-pane-at-a-time reading
    mode instead of silently ending).
  - The zoomed pane's PTY is resized to the full body inner dims; hidden
    panes keep their last size until unzoom relayouts them (no reflow churn
    while reading).
  - Mouse: body clicks/wheel can only hit the zoomed pane (it is the whole
    display list); tab-bar clicks behave normally (and exit zoom, below).
- If the focused pane is a collapsed stack member when Alt+z fires, it is
  expanded first (`expand_in_stacks`), then zoomed.
- **Exits zoom** (exhaustive list): Alt+z again · any tab change (Alt+t,
  Alt+1..9, tab-bar click, cross-tab Alt+a) · any structural layout action —
  Alt+n, picker launch, Alt+s, Alt+o, Alt+Shift+arrows, Alt+g — which exit
  zoom *first, then apply*, so the layout never changes invisibly · the
  zoomed pane closing (Alt+w or control close). **Keeps zoom:** focus moves,
  same-tab Alt+a, entering/leaving scroll·copy·rename·help·feed modes, the
  float toggle (the float draws above the zoomed view, C22), control-plane
  activity in other panes.
- Alt+z while the float (C22) is focused: no-op + flash `can't zoom the
  float`.
- **Chrome indication:** the C9 mode-word slot shows `ZOOM` whenever zoomed
  and the mode is Normal (precedence per C9 amendment: real mode words and
  `RAW` win over `ZOOM`). No border/badge change — the full-body accent
  border is itself the signal. Normal hint pairs unchanged.
- Unit tests: display list is `[focused @ body]` iff zoomed; each exit
  trigger clears the flag; focus-move-under-zoom retargets the display list;
  PTY resize targets.

### C22 — Floating scratch pane (Alt+f) — [Added 2026-07-22, fleet features]

**Current:** no floating anything. All panes live in a tab's layout tree;
`hit_test` scans tiled rects in order (`mouse.rs:37–47`); pane ids are
allocated by scanning the tabs (`workspace.rs:57–65`).

**Target:**
- New `Action::ToggleFloat`, bound to **Alt+f**. Mnemonic: *float* — zellij's
  own Alt+f. (Collision note, flagged per brief: Alt+f is readline
  forward-word; roost already swallows it today (`input.rs:72–77`), and raw
  mode (C23) is the remedy for panes that need it back.)
- **One float slot, app-wide** (not per tab): `Option<Float>` holding
  `{ id, spec, shown, prev_focus }`. The scratch's roadmap cousin (a floating
  *picker*) is explicitly out of scope.
- **Lifecycle:** first toggle spawns a `shell` adapter pane in the focused
  pane's cwd (else the process cwd), spec title preset `"scratch"`, shown and
  focused. Later toggles hide/show; the process stays alive while hidden.
  **Session-only by design:** never written to `workspace.json`; at quit it
  dies like every pane and is not restored — a scratch pane is ephemeral
  (documented honest scope). Closing it (Alt+w while focused) kills it and
  clears the slot **without** an undo entry (scratch is not precious); flash
  `scratch closed`. If its shell exits, the C16 dead-pane overlay renders
  inside the float rect and Enter/f relaunch as normal.
- **Id safety (hard predicate):** pane-id allocation must account for the
  float — `workspace.rs::next_pane_id` scans only the tabs, so without a
  guard the next split would reuse the float's id. Allocation goes through an
  App-level wrapper: `max(ws.next_pane_id(), float.id + 1)`.
- **Geometry:** centered on `body_area()`;
  `w = clamp(3·body.width/5, 36, body.width − 4)`,
  `h = clamp(3·body.height/5, 8, body.height − 2)`. If `body.width < 40` or
  `body.height < 10`, the toggle refuses with flash `no room for float`.
  Worked example (audit fixture): 80×24 terminal → body 80×22 → float 48×13,
  centered. Recomputed on resize.
- **Stacking order (topmost last), contracted:** tiled panes → zoomed view
  (C21) → **float** → C12 modal overlays (rename/picker/help/feed). The
  float never dims the workspace — it is a pane, not a modal.
- **Border/badge:** rendered exactly as a pane: `ACCENT` focused border
  whenever shown (it is focused whenever shown — next bullet), corner badge
  through the normal titled path → `scratch · shell {glyph}`. No new glyphs,
  no special border.
- **Focus & input rules (the whole contract in four lines):**
  1. Shown ⇒ focused. All keys route to it normally; scroll, copy, and
     rename modes target it like any pane.
  2. Any action that moves focus off it **hides** it (process alive):
     Alt+arrows/hjkl, Alt+a, any tab change, a mouse click outside its rect
     (that click then lands normally on what it hit). Focus returns to
     `prev_focus`.
  3. Structural pane actions — Alt+n, picker launch, Alt+s, Alt+o,
     Alt+Shift+arrows, Alt+g, Alt+z — first hide the float and restore
     `prev_focus`, *then* apply. (The float is outside the layout tree;
     without this, `spawn_child`'s empty-tab fallback at `app.rs:1425–1427`
     would wipe the tab's layout when asked to split a pane the tree doesn't
     contain.)
  4. Alt+w closes it for real (above); Alt+f hides it.
- **Mouse:** when shown, the float's rect is **first** in the hit-test list
  (`hit_test` takes the first match — the caller orders the slice; topmost
  wins). Wheel, clicks, drags, and copy-mode selection inside it behave as
  for any pane.
- **Control plane (documented, deliberate):** the float is absent from
  `roost list` (`ctl_list` walks `ws.tabs`); `send`/`read` by id work
  (`find_spec` learns the float so badges/rename/respawn work); control
  `close` of the float is refused with `cannot close the scratch pane`.
- Unit tests: spawn-once/hide/show lifecycle; id-allocation guard; geometry
  formula incl. refusal floor; focus rules 1–3; hit-test ordering.

### C23 — Per-pane raw mode (Alt+Shift+p) — [Added 2026-07-22, fleet features]

**Current:** roost owns the whole Alt layer: matched chords become actions,
**unmatched Alt chords are swallowed**, never forwarded (`input.rs:72–77`) —
an agent CLI with its own Alt bindings (readline word ops, custom editors)
can never see them.

**Target:**
- New `Action::ToggleRaw`, bound to **Alt+Shift+p** — also accepted as
  Alt+`'P'` (uppercase-delivery tolerance, same as the rename-tab chord,
  `input.rs:59–60`). Toggles the **focused** pane's membership in an
  App-level `raw: HashSet<PaneId>`; per-pane, session-only, never persisted.
- **Exit-chord rationale (safety-critical, recorded):** while raw this is the
  only chord roost intercepts, so it must be (a) nearly impossible to hit by
  accident — a three-key shifted chord is; (b) collision-free — no default
  readline/zsh/agent-CLI binding uses shifted meta letters; (c) memorable —
  it is the *same* chord that enters raw ("the key that got you in gets you
  out"), P = **P**ass-through, and the hint bar displays it the entire time
  the pane is raw (below), so nobody can get trapped. Lowercase Alt+p stays
  unbound in Normal and passes through in raw.
- **Routing predicate (the core of the contract):** when
  `mode == Normal && raw.contains(focused) && !focused_dead()`, every key
  event except Alt+Shift+p bypasses `translate()` and is forwarded as bytes
  (`main.rs` key path, `:281–298`):
  - non-Alt keys: exactly today's `encode_key` bytes (`input.rs:103–142`),
    kitty upgrade included;
  - **Alt-modified printable keys: the meta-ESC convention** — `0x1b` + the
    unmodified key's encoding (this is what readline/agent CLIs bind);
    Alt+Enter → `0x1b 0x0d`. Alt+special keys forward as **meta-ESC
    uniformly**: `0x1b` + the key's unmodified sequence (Alt+Right →
    `0x1b 0x1b 0x5b 0x43`) — this is xterm's `altSendsEscape` behavior, and
    unlike a bare passthrough it preserves the Alt distinction for the inner
    app. [Amended 2026-07-22, supervisor D1: the earlier "bare unmodified
    encoding" wording lost information and matched no real terminal; the
    upgrade path (xterm `CSI 1;3` modifier encodings) stands if an agent
    ever needs it.]
  - **Nothing else is intercepted.** Not Alt+q, not Alt+arrows, not
    Alt+1..9 — that is the feature. The hint bar shows the way out.
- **Interplay with modes:** raw routing applies only in Normal mode.
  Non-Normal modes are unreachable from a raw-focused pane by keyboard (their
  entry chords pass through) — by design. **Mouse is unaffected** (raw is a
  key-path property): click another pane to move focus away; the flag stays
  on its pane and routing resumes when it is refocused. Paste events forward
  unchanged. A **dead** raw pane falls back to dead-pane key handling
  (`main.rs:284–287` — Enter/f/Alt+w work; forwarding keys to a corpse would
  trap the user).
- **Indication (must be visible on the pane even when unfocused):**
  - Corner badge (C4): the badge text gains a `raw` token —
    titled: `"{name} · {adapter} · raw {glyph}"`, untitled:
    `"{name} · raw {glyph}"` — the `raw` token fg `ACCENT_DIM` (the
    "roost stepped back" color family, C11/C16).
  - Collapsed stack row (C8): right segment gains the prefix →
    `"raw · {word}"`.
  - Hint bar while a raw pane is focused and mode is Normal: mode word
    `RAW`, pair list exactly `Alt+Shift+p exit raw` (C9 amendment). `RAW`
    beats `ZOOM` in the word slot.
- Orthogonal to zoom/float/stacks: the flag follows the pane wherever it
  renders; the float can be marked raw too.
- Unit tests: routing predicate (raw focused: `Alt+q` forwards as
  `0x1b 'q'`, `Alt+b` as `0x1b 'b'`, Alt+Shift+p toggles off; cooked pane
  unchanged); dead-pane override; badge/row tokens; only-intercepted-chord
  property (table-driven over the whole current action list).

### C24 — Keyboard copy mode — [Added 2026-07-22, fleet features]

**Current:** `Mode::Copy` is mouse-only — drag selects, release copies; keys
just exit (`app.rs:1595–1602`); selection painted `REVERSED` per C17
(`render.rs:763–781`); extraction via `grab_text` (visible grid, inclusive
reading order).

**Target:**
- `Mode::Copy` gains a cursor: `Mode::Copy { cursor: (u16, u16) }` in the
  focused pane's inner cell space. Initial position: `(inner_height − 1, 0)`
  — bottom-left; deterministic, and adjacent to the prompt in practice.
- **Key set (the brief's minimum, nothing more):**
  - `h j k l` and arrows — move one cell, clamped to the inner grid;
  - `0` — column 0; `$` — last column (`inner_width − 1`);
  - `v` — set the anchor at the cursor / clear an existing anchor (toggle);
    with an anchor set, movement extends the selection (`Selection.cursor`);
  - `y` or `Enter` — with a selection: yank via the existing
    `finish_selection` path (`app.rs:1153–1166` — clipboard, `copied N
    chars` flash, exit to Normal); without: flash `nothing selected`, stay;
  - `Esc` / `q` — exit, clearing any selection.
  - Alt chords still break out to global bindings (existing rule,
    `app.rs:1503–1511`).
- **Cursor visualization (modifier-only, extends C17 — no palette tokens):**
  the cursor cell always carries `Modifier::REVERSED`; when it lies inside an
  active selection it additionally carries `UNDERLINED`, so it stays
  distinguishable within the reversed region. Painted after the selection
  pass. Any color-token styling of the cursor is a DEVIATED.
- **Selection semantics — identical to the mouse path** (one selection
  model, two input methods): inclusive anchor→cursor, reading order, same
  `Selection` struct, same `highlight_selection`, same `grab_text`. Honest
  limit, shared with the mouse path and documented: the **visible grid
  only** — no scrollback paging inside copy mode (deliberately left out;
  Scroll mode remains a separate concern).
- **Mouse drag still works in copy mode** and simply replaces the keyboard
  selection (both write `app.selection`); a drag also moves the cursor to
  the drag point, so the two methods interleave without surprises.
- Hint pairs per C9 amendment: `hjkl move · v mark · y/↵ yank · drag select
  · Esc exit`.
- Unit tests: motion clamping; `0`/`$`; anchor toggle and extension;
  yank-with/without-selection; Esc clears; drag-replaces-keyboard-selection.

### C25 — Canned layout cycle (Alt+g) — [Added 2026-07-22, fleet features]

**Current:** layout shape is built up manually (splits Alt+n/Alt+o, stacks
Alt+s, ratios Alt+Shift+arrows); no way to snap the tab to a known-good
arrangement. Tree ops live in `layout.rs` (`toggle_stack :129–166`,
`split_pane :54–78`); `MIN_SPLIT_COLS/ROWS = 36/10` gate splits
(`app.rs:31–32`).

**Target:**
- New `Action::CycleLayout`, bound to **Alt+g**. Mnemonic: *arranGe / grid*.
  (Rejected alternatives, recorded: `Alt+Space` — tmux's next-layout key, but
  OS-captured as the window/system menu on GNOME and Windows Terminal;
  `Alt+[`/`Alt+]` — zellij's, but ESC-`[` *is* the CSI introducer byte pair,
  an encoding hazard.)
- **Zero-config, hardcoded, exactly three arrangements**, applied to the
  active tab's pane set. Let `P` = `pane_order()` of the current tree at
  press time, `f` = the focused pane, `n = |P|`. Only `tab.layout` is
  replaced — specs, sessions, titles, runtimes untouched.
  1. **even-grid:** `c = ceil(sqrt(n))`, `r = ceil(n/c)`;
     `Split{Horizontal}` of `r` rows (even ratios), each row a
     `Split{Vertical}` of the next ≤ c panes of P (even ratios); a single
     row/column collapses to one Split; `n = 1` → `Pane`. Worked shapes
     (audit fixtures): n=2 → side-by-side; n=3 → 2 over 1; n=4 → 2×2;
     n=5 → 3 over 2; n=7 → 3/3/1.
  2. **main+stack:** `Split{Vertical, ratios [0.6, 0.4]}` — left `Pane(f)`,
     right `Stack(P minus f, expanded 0)`. `n = 2` → a plain 0.6/0.4
     vertical split (no one-member stacks); `n = 1` → `Pane`.
  3. **all-stack:** `Stack(P, expanded = position of f in P)`.
- **Preservation rules (each a predicate):** focus stays on `f` in all
  three · pane order is preserved — `pane_order()` of the produced tree
  equals `P`, except main+stack where `f` moves to the front (deterministic,
  pinned by test) · prior stack membership is **not** preserved (the
  arrangement dictates structure — that is the feature) · prior ratios are
  lost (canned means canned) · not undoable via Alt+u (undo is for closes;
  documented) · the result is persisted like any layout edit; PTYs resize
  via the normal relayout.
- **Cycle & fit:** one App-level cycle counter (session-only) advancing
  grid → main+stack → all-stack → grid. An arrangement **fits** iff every
  non-collapsed rect it would produce in the current body area is
  ≥ `MIN_SPLIT_COLS × MIN_SPLIT_ROWS` (36×10 — the existing split floors;
  collapsed 1-row stack bars are exempt by design). Alt+g applies the next
  **fitting** arrangement, skipping unfit ones; the counter lands on what was
  applied.
- **Cycling is disabled** (press is a no-op, counter does not advance) when:
  `n < 2` → flash `one pane — nothing to arrange`; or no arrangement fits →
  flash `no room to rearrange`.
- Interplay: exits zoom first (C21 structural rule); hides the float first
  if focused (C22 rule 3); the float itself is untouched (not in the tree).
- Builders and the fit predicate are pure `layout.rs` functions with unit
  tests: the worked shapes above, order preservation, fit refusal, n=1/n=2
  degenerate forms.

### C26 — Tab undo: scope statement — [Added 2026-07-22, fleet features]

**Current = already implemented.** Verified in the working tree: the undo
stack has a whole-tab variant (`Closed::Tab`, `app.rs:89–95`), captured with
the tab's full state when closing its last pane empties it
(`close_pane_id`, `app.rs:999–1014` — snapshot cloned *before* removal, so
the last pane's spec and session ride along), restored at its original index
with name + layout + specs + sessions by `undo_close` (`app.rs:1315–1322`),
respawned via `spawn_active_tab`; pinned by the existing test
`undo_reopens_a_closed_tab` (`app.rs:1859–1867`). The brief's "extend to
whole tabs" is therefore a **scope statement + pinning**, not a build.

**The honest scope (contracted wording for README/help):**
- What restores on Alt+u after a tab disappears: the tab, by name, at its
  original position, with the layout and pane specs it had at the moment it
  emptied — **session ids included, so agents resume**.
- Honest limits (deliberate, documented):
  - A multi-pane tab is dismantled close-by-close, so its earlier panes come
    back as individual pane-undos — sessions intact, but re-split off the
    focused pane (`restore_pane`, `app.rs:1337–1361`) rather than at their
    original geometry/ratios. Only the state at last-pane close restores
    atomically. (There is no close-whole-tab gesture to snapshot sooner —
    tabs only die by their last pane closing.)
  - The stack holds 20 entries (`UNDO_DEPTH`, `app.rs:78`) and is
    session-only — quitting roost clears it.
  - Closing the last pane of the *last* tab quits roost; nothing to undo
    (existing confirm guard covers it, `app.rs:1445–1456`).
  - The float pane (C22) never enters the undo stack.
- **Build work is exactly:** one added unit test (a 3-pane tab closed
  pane-by-pane, then 3×Alt+u restores all three panes with their sessions
  and the tab name) + the README wording above. Zero behavior change; any
  behavior diff in this area is a DEVIATED.

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
- **[Added 2026-07-22, fleet features] One display list, ordered.** The
  renderer, PTY sizing, and mouse hit-testing consume the same
  `display_rects()` sequence: float first when shown (topmost wins in
  `hit_test`), else the zoom singleton when zoomed, else `rects()`. Focus
  math alone keeps reading the real tree (`rects()`), which is what makes
  zoom-follows-focus work. Raw mode (C23) alters the **key path only** —
  mouse routing, mouse capture, paste, and the vt100 blit are untouched by
  it.

---

## 6. Supervision

### Per-contract audit

The `design-supervisor` agent runs after any change under `src/ui/**`,
`src/core/layout.rs`, or `src/core/app.rs` (UI-adjacent helpers), and issues
one verdict per contract C1–C26: **ALIGNED** or **DEVIATED** (+ file:line and
the violated bullet). Mechanics per contract class:

- **Greppable predicates** (C1 theme gate, C18 zero-diff, no-BOLD rule,
  banned hues): verify by reading `src/ui/render.rs` / `theme.rs` — e.g.
  `Color::` outside the blit section, `Modifier::BOLD` anywhere in `ui/`,
  any of `7fae7f|d8a657|8fb2c9` under `src/ui/`.
- **Structural predicates** (C2 cell layout + width formula, C6 geometry
  threshold, C5 phase boundaries; fleet additions: C19 ring order, C20 ring
  cap/taxonomy, C21 display list, C22 geometry/id guard, C23 routing
  predicate, C24 motion/anchor, C25 builders/fit): verify against the unit
  tests this plan requires — tests are the executable form of the contract.
- **Visual predicates** (colors in place, marker glyphs, right-alignment,
  feed styling, raw badge token): verify by reading the span-construction
  code; ambiguity → run roost and eyeball, or use the harness below.

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

### Firehose input-latency gate — the harness's first concrete test
**[Added 2026-07-22, fleet features — item 10]**

The trigger above has fired in spirit: the fleet engagement touches
`src/ui/render.rs` heavily, and item 10 needs a real PTY anyway. The firehose
test therefore **instantiates the harness foundation** — the spawn/settle
helper is ~80 % of what the golden frames would need, so the marginal cost of
folding the foundation in is near zero. **Golden-frame color scenarios remain
deferred** on the same trigger as before; this is a smoke-level perf gate,
not a benchmark suite.

- **Files:** `tests/harness/mod.rs` (shared helper: open a portable-pty at
  120×40 · exec `CARGO_BIN_EXE_roost` with `ROOST_STATE=<tempdir>` and a
  fixture `workspace.json` · feed PTY bytes to a `vt100::Parser` ·
  `settle()` = poll-parse until two consecutive parses agree) and
  `tests/firehose.rs`. roost is a **binary** crate, so integration tests
  cannot import its modules — the harness drives the built binary and needs
  its own parser deps: `Cargo.toml` gains
  `[dev-dependencies] portable-pty = "0.8"` and
  `vt100 = { path = "vendor/vt100" }` (same versions as the main deps; no
  new third-party code).
- **Scenario:** fixture workspace of two `shell` panes side by side. Pane A
  runs a flat-out spew loop (`sh -c 'while :; do printf "%0.sX" $(seq 200);
  echo; done'` — deterministic filler, ~200-char lines). Pane B is a quiet
  interactive shell holding focus. During ≥ 5 s of sustained spew, write 20
  single printable characters to the outer PTY at 100 ms intervals; after
  each, poll the parsed outer screen for the character echoed in pane B's
  region.
- **Pass thresholds (each an assertion):**
  1. **Input latency:** every echo visible within **250 ms** of the write
     (≈ 7–8 of the ~33 ms draw ticks — an order-of-magnitude guard that
     survives CI jitter, chosen against the loop's own budget:
     33 ms poll + 512-events/tick cap, `main.rs:173/:210`).
  2. **No draw starvation:** pane A's on-screen region differs between
     consecutive 500 ms samples for the whole run (the firehose visibly
     keeps flowing — bounded, not frozen).
  3. **Clean exit under load:** send Alt+q (`0x1b q` — meta-ESC) mid-spew;
     the roost process exits within **2 s** and no child of it survives
     (the historical quit-freeze regression, ROADMAP "Alt+q freeze fix").
- Skipped on runners without a functional PTY (compile-time cfg or runtime
  skip with a printed reason) — same stance as the golden-frame assessment.

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
  C2 clips honestly) · any restyle of program output · golden-frame *color
  scenarios* in this build (§6 — the harness foundation itself now ships with
  the firehose gate) · letterspacing/gap/padding emulation (§4).
- **[Added 2026-07-22, fleet features — deliberately left out:]**
  float persistence across restarts (scratch is ephemeral, C22) · feed
  persistence / feed filtering / feed search (200-entry ring is the whole
  product, C20) · sub-2 s status-transition granularity in the feed (single
  tick-diff source beats double-reporting, C20) · per-tab zoom flags (one
  app-level bool, C21) · copy-mode scrollback paging (visible grid only,
  same as the mouse path, C24) · layout-cycle undo (C25) · a close-whole-tab
  gesture (C26 — tabs die by last-pane close only) · a TUI broadcast key
  (fat-finger safety — CLI only, per brief; grammar lives in PLAN F2) ·
  configurable keys for any of the above (zero-config stands).

---

## 8. Key table — [Added 2026-07-22, fleet features]

The one canonical list. The help overlay (C15) renders exactly these rows in
this order (merged rows stay merged; ≤ 20 rows, hard cap). The hint bar shows
only the C9-curated subsets.

| # | Chord | Action | Contract |
|---|---|---|---|
| 1 | `Alt+n` | new shell pane (auto split) | — |
| 2 | `Alt+Enter` | quick-launch picker (pi / claude / shell) | C14 |
| 3 | `Alt+←↓↑→ / hjkl` | move focus | — |
| 4 | `Alt+Shift+←↓↑→` | resize along that axis | — |
| 5 | `Alt+s` | toggle split ⇄ stack | C6–C8 |
| 6 | `Alt+o` | flip split orientation | — |
| 7 | `Alt+g` | **cycle layout: grid / main+stack / all-stack** | C25 |
| 8 | `Alt+z` | **zoom focused pane (view only; Alt+z again to exit)** | C21 |
| 9 | `Alt+f` | **floating scratch shell (toggle)** | C22 |
| 10 | `Alt+a` | **jump to next pane that needs you** | C19 |
| 11 | `Alt+e` | **activity feed (status / spawns / exits / control)** | C20 |
| 12 | `Alt+r / Alt+Shift+r` | rename pane / tab | C13 |
| 13 | `Alt+t / Alt+1..9` | new tab / go to tab | C2 |
| 14 | `Alt+w` | close pane (confirm if busy / last) | — |
| 15 | `Alt+u` | undo — reopen last closed pane/tab | C26 |
| 16 | `Alt+c` | copy mode (hjkl+v+y, or drag) | C17/C24 |
| 17 | `Alt+PgUp` | scroll mode | — |
| 18 | `Alt+Shift+p` | **raw pass-through for this pane (same chord exits)** | C23 |
| 19 | `Alt+/` | toggle hint bar | C9 |
| 20 | `Alt+?` | full keymap overlay (this table) | C15 |
| 21 | `Alt+q` | quit (workspace saved; sessions live) | — |

[Amended 2026-07-22, supervisor SPEC-GAP: row 20 `Alt+?` was bound in
translate() and advertised by C9's hint bar but missing from this canonical
table. The C15 help overlay's ≤20-content-row cap counts key ROWS, some of
which pair two chords — the overlay stays within cap.]

Contextual, non-Alt: dead pane — `Enter` relaunch/resume, `f` fresh (C16);
raw pane — **every** key passes through except `Alt+Shift+p` (C23); modes
capture their own keys (C9 lists them).

Control-plane only, no key by design: `roost send --all TEXT [--enter]`
(broadcast — PLAN F2; surfaces in chrome only as a C20 `ctl` feed line).

Free Alt keys remaining after this engagement: `b d i m p v x y 0 PgDn`.
Collision flags (all already swallowed by roost today, `input.rs:72–77`;
raw mode C23 is the remedy): `Alt+f` readline forward-word · `Alt+a` zsh
accept-and-hold · `Alt+b/d` left deliberately free (readline word ops — the
most-missed bindings; do not assign them to chrome without strong cause).
