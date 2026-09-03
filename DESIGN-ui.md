# DESIGN-ui.md — roost TUI chrome restyle

Canonical design reference: `docs/tui-design.html` (tokens at `:root` ~line 584,
terminal markup ~614–696, token legend ~698) — **for layout, glyphs and
relationships, not for its hex values**, which §2's theme-inheritance amendment
demoted when roost stopped shipping fixed hues. This document translates that
mockup into testable contracts for a ratatui 0.30 cell-grid TUI. The
`design-supervisor` agent audits implementation against the numbered contracts
below (C1–C28) and issues a per-contract verdict: **ALIGNED** or **DEVIATED**
(with file:line evidence). Line anchors below were verified against the working
tree on 2026-07-21 and may drift a line or two; the code element named is the
anchor, not the number.

**Amendment 2026-07-22 (fleet features):** contracts C19–C26, the C9/C15
amendments, the §6 firehose gate, and the §8 key table were added for the
fleet-features engagement (BRIEF: `.claude/company/fleet-features/BRIEF.md`).
Their line anchors were verified against the working tree on 2026-07-22.
C1–C18 are unchanged and must **still** audit ALIGNED after the fleet build —
in particular the C1 grep gates and the C18 zero-diff rule.

**Amendment 2026-07-27 (SPEC-parity W6):** **C18 is the one exception to the
line above** — its zero-diff rule now carries a dated amendment covering
`blit_screen`'s wide-char continuation handling (SPEC-ux U24) and `cell_style`'s
dim/strikethrough mapping (SPEC-parity P16). Read C18's own amendment before
auditing it; the restated predicate is *"does the blit add anything the program
did not emit?"*. `conv_color` and the C1 grep gates are untouched.

**Amendment 2026-07-27 (theme inheritance) — read §2 before auditing any
contract below.** A user on a light-theme terminal reported the tab bar and
hint bar rendering as near-black bands and the **active tab's label being
invisible** (near-white ink on `Color::Reset` = their white background).
roost's chrome now inherits the terminal's palette instead of shipping fixed
hues. Consequences that touch every contract with a colour in it:
- The token names changed and two of them merged — see §2's rename map. Any
  `FG`/`MUTED`/`DIM`/`RULE`/`ACCENT`/`ACCENT_DIM` still spelled below is the
  pre-amendment wording; read it through the map.
- `BG`, `TAB_STRIP` and `BAR` are **deleted**. **No chrome surface has a
  background fill any more** — where a contract below says "bg `X`", the
  current rule is either `attention()` / `attention_problem()` (`REVERSED`,
  plain or red-tinted, for the three surfaces that must shout) or no fill at
  all. The per-contract amendments say which.
- Exact-hue auditing is retired: there are no hexes left to match. The
  auditable predicates are the §2 legibility principle and the four mechanical
  gates it names — they replace the old "hex values match exactly or they
  don't" rule for chrome colour, which now has nothing to compare against.

**Amendment 2026-07-28 (vertical-tabs tribunal):** **C27** (the fleet roster,
`Alt+Shift+a`) is new, and **C2** carries a dated amendment adding a count
cell beside the tab summary glyph. Both come from a tribunal that *rejected* a
herdr.dev-style vertical sidebar on the 80-column arithmetic and closed the one
gap it did find; the reasoning lives in C27's provenance paragraph. They add no
glyph (§2's inventory is unchanged — the count cell is ASCII text, see C2) and
no colour.

**Amendment 2026-07-28 (move-pane + keymap):** **C28** (move a pane between
tabs, `Alt+Shift+i`/`Alt+Shift+m`) is new, and **C15 retires the help
overlay's ≤20-row cap** — the keymap is now grouped, takes a second column
when the terminal is wide enough, and scrolls only when it must. Read C15's
last amendment before auditing it: six merges in eight days had turned "the
merge idiom" into a budget that was shaping the artifact, and the cap's
replacement is a stronger check, not a weaker one — *every chord roost binds
appears in the overlay*, an assertion the cap made unsatisfiable. Neither
adds a glyph or a colour.

**Amendment 2026-08-06 (native selection):** **C29** (drag-select,
double/triple-click, shift-click-extend over a pane whose app never asked
for the mouse) is new — the client's standing macOS-parity requirement
(`docs/engagements/2026-08-07-best-in-class/PLAN.md`, Phase 2N, client requirement #1).
It reuses C17's `Selection`/`highlight_selection` and
C24's `finish_selection`/`grab_text` verbatim rather than forking them, adds
no glyph and no colour, and needed no amendment to C23 or C24 (both verified
unchanged, see C29's own "must-not-break" list) or to §8's key table (the
gesture is mouse-only — §8 gets a same-dated cross-reference note instead,
since it names no chord). `main.rs` also now requests a deliberate subset of
crossterm's mouse-capture modes (C29's own bullet); that is a startup-cost
change, not a chrome one, and touches no contract's rendered output.

**Amendment 2026-08-20 (reliability audit, second pass):** **C10** now paints
a flash over the body's last row when the hint bar is not drawn. The flash
had been a child of the hint bar, so hiding hints (`Alt+/`) or a terminal
under 3 rows silenced every C10 message — refusals, copy results, the
workspace-set-aside notice, and U22's confirm-arm prompts, which armed a
destructive second press while saying nothing. Same text, same `attention()`
styling, same single row, same function; no new glyph and no new colour. C6
gains a clarification that a stack's `n` is always ≥ 2 (a stack of one is
normalized to a `Pane`), so `"STACK · 1 PANES"` is unreachable rather than
merely ungrammatical. Both found by the design-supervisor pass on the C2
amendment below.

**Amendment 2026-08-20 (reliability audit):** **C2**'s yield ladder is
amended so that `save failed ✕` outranks tab *names* — the failure was
previously dropped whole by "tabs win", which on an ordinary terminal with a
few tabs made a permanently-failing save permanently invisible. The strip
scrolls under U7 instead, always keeping the active tab, and the indicator
yields after all rather than empty the bar. No new glyph and no new colour:
the indicator, its `accent()` styling and its columns are unchanged — only
which of two existing things yields to the other. Details, including the
floor case, live in C2's own dated amendment.

**Amendment 2026-09-01 (boxed collapsed rows):** a collapsed stack member
was easy to miss — a bare 1-row strip of text between two fully bordered
panes, the one pane-shaped thing on screen with no frame around it (client
report, with screenshot: "in the stacked view, the collapsed pane is easy to
miss"). **C6** and **C8** are amended together: where the stack's area
affords it, each collapsed member now occupies a **3-row `accent_quiet()`
bordered box** with the unchanged C8 row on its single inner line; where it
doesn't, the 1-row bar renders exactly as before. The §4 "every bar stays
exactly 1 row" translation row carries the matching exception. No new glyph;
no new colour — the box border reuses the quiet red that already means
"stack chrome" (C7's expanded-member edge).

**Amendment 2026-09-01 (typing filters the keymap):** **C39**'s `/` gate
opens — a bare printable in the un-filtered overlay now opens the filter
seeded with itself, the trade C27's roster made first ("a letter is filter
text"); the owner reached for the palette by typing and the overlay closed
on the first letter, reading as broken. `/` still opens an empty query;
`j`/`k` become query text (the arrows and paging keys keep the un-filtered
scroll); Esc, Enter, Space, chords, clicks and dead-end scroll keys keep
closing. **C15**'s "any key closes it" narrows accordingly, and the C39
announce wordings change to `type to filter · Esc closes` /
`type filter` · `Esc close`. Details in C39's dated amendment; no new
glyph, no new colour.

---

## 1. Design thesis

> **Chrome is the user's own ink on the user's own paper, plus their one red;
> program output keeps its own colors.**

Everything roost draws itself (tab bar, borders, badges, stack chrome, hint
bar, modals) is built from the terminal's own palette — one accent hue plus
the ink the user's shell prompt already uses. Everything a program draws
inside a pane passes through the vt100 blit byte-faithfully and keeps its own
palette. Red is never decoration: it means *focus*, *roost wants your
attention* (live keys, needs-input, failure), or *the agent is alive and busy*
(Working — animated; steady red everywhere else means attention, C5 amended
2026-08-07).

**[Amended 2026-07-27 — the thesis line used to read "ink · paper · one red"
with fixed hues.]** See §2.

---

## 2. Token table

**[Amended 2026-07-27 — chrome inherits the terminal's theme.]** Every token
below used to be a fixed `Rgb(..)` triple from the mockup. A user on a light
terminal reported the tab bar and hint bar rendering as near-black bands and
the **active tab's label being invisible**: it was drawn in the near-white
`#eae5df` on `Color::Reset`, i.e. white text on the user's white background.
That was not an oversight — it was this section's own stance, logged as
SPEC-GAP-4. The stance is now reversed, and the tokens are styles rather than
colours. Names below are the accessors in `src/ui/theme.rs`; the old
`SCREAMING_CASE` names are gone. Rename map for reading older text in this
document: `FG`→`ink`, `MUTED`/`DIM`→`quiet` (they collapse), `RULE`→`rule`,
`ACCENT`→`accent`, `ACCENT_DIM`→`accent_quiet`; `BG`, `TAB_STRIP` and `BAR`
are **deleted**. **[Amended 2026-08-07, C5]** `pulse phase A`/`pulse phase B`
are retired concepts, not renamed ones — `pulse_bright()` no longer exists,
and the two-phase idea it named has no successor (animation is a ten-frame
glyph cycle now, not a two-value colour flip). Older text using either term
should be read against C5's spinner amendment, not looked up as an accessor.

| Token | ratatui expression | Where used in chrome |
|---|---|---|
| `ink()` | `Color::Reset` | primary ink: active tab label, waiting glyph ○, modal titles/body/input, the live search query, picker selections, working/needs-input collapsed-row names, **every** focused collapsed row, the tab bar's mode word, the feed's needs-input text, the `¶` note marker and the focused badge's note headline (C32) |
| `quiet()` | `Color::Reset` + `Modifier::DIM` | the one secondary rung: inactive tab labels, corner-badge text, hint labels, picker unselected rows, help descriptions, idle glyph ·, tab-bar cwd + saved word, stack header, collapsed-row right segment and unfocused waiting/idle/exited names, feed timestamps and text, hint-bar mode word, overflow `…`, the note age tag (C32) |
| `rule()` | `Color::DarkGray` (ANSI 8) | **structure only**: unfocused pane borders (except a boxed collapsed member's, which is stack chrome — C8, amended 2026-09-01), tab separators `│`. Never text (see the legibility principle). |
| `accent()` | `Color::Red` (ANSI 1) | the one red: focused pane border, active-tab marker `▎`, hint keys, ◆ needs-input, the Working spinner (C5, amended 2026-08-07 — one steady red, no second phase), modal borders, "◆ N needs you", "save failed", spawn-error line, `❯` picker/feed markers |
| `accent_quiet()` | `Color::Red` + `Modifier::DIM` | ✕ exited glyph, expanded-stack edge `▌`, collapsed-member box border (C8, amended 2026-09-01), `raw` badge token (C23), `↑N` badge token (U3) |
| `attention()` | `Modifier::REVERSED`, no colour | the **neutral** attention surface: the transient flash (C10) |
| `attention_problem()` | `Color::Red` + `Modifier::REVERSED` | the **problem** bars: alt-warning (C11), dead-pane action bar (C16) |
| `ok` / `warn` / `info` | — not defined in theme | **no chrome role.** Program-output palette in the mockup only. Must not appear in `src/ui/`. |

All chrome styling lives in `src/ui/theme.rs` (C1) — accessors now rather than
consts, because a token is a whole `Style` (colour *and* modifier) and
`Style`'s builders are not `const fn`. The ok/warn/info trio is deliberately
**not** defined there — defining unused tokens invites casual reuse and
dilutes the one-red rule.

### The legibility principle

Theme variance is concentrated in the ANSI 8 slot. Variance is therefore
allowed **only where degradation is graceful**:

- **Text always derives from `Color::Reset`** — the terminal's own foreground
  on its own background: the one contrast pair the user has already validated,
  because it is what every line of their shell output uses. Legible by
  construction, in light, dark and tinted themes alike.
- **The quieter text rung is `Reset` + `Modifier::DIM`.** Worst case a
  terminal ignores DIM and it renders as primary ink; it can never become
  invisible. This is why there are **two** text rungs and not three: a third
  spelled out of ANSI 8 would put words in the one colour a theme is free to
  swallow.
- **ANSI 8 (`rule()`) is structure only** — borders, separators, rules. If a
  theme makes it faint you lose a hairline, not a word.
- **Attention surfaces use `Modifier::REVERSED`, never a colour fill.**
  Reversing the terminal's own fg/bg is guaranteed contrasty in any theme.
- **The one red is the user's red**: ANSI 1, unmodulated. **[Amended
  2026-08-07, C5]** Animation used to live in the colour (a second red,
  ANSI 9, so a terminal ignoring `DIM` couldn't flatten the pulse into a
  steady dot) — it now lives in the *glyph*: the Working status swaps
  braille spinner frames on a shared clock instead, so `accent()` is spent
  once, never modulated, and needs no second red to stay visible.

### Theme-inherited stance

**Inherit the terminal's palette; no curated palettes, no light/dark
detection, no config (zero-config stands).**

- Chrome names only `Color::Reset`, `Color::DarkGray`, `Color::Red`,
  `Color::LightRed` and modifiers. **No `Color::Rgb` and no `Color::Indexed`
  anywhere under `src/`** — the sole exemption is the vt100 blit's
  `conv_color`/`cell_style`, which carry *program* output and must keep the
  full palette (C18). Mechanically gated by
  `theme::tests::no_truecolor_or_indexed_colour_is_constructed_in_src`, a
  source scan honouring a `chrome-gate-exempt` marker.
- Justification: roost is chrome *around* other programs' output, so it should
  recede into the theme the user already chose. Inheriting is correct for
  **all** themes rather than a light/dark binary, survives a live theme switch
  with no restart, and needs no detection machinery at all.
- What is preserved from the old stance is the *discipline*, not the hues: one
  accent, quiet structure, glyphs that carry meaning by shape (§ glyph
  inventory), no decoration. What is dropped is the exact-hue requirement —
  and with it the mockup's authority over specific values. Where
  `docs/tui-design.html` shows a hex, it now documents the *relationship*
  (ink ≫ quiet ≫ structure, one red apart from all of them), not the colour;
  that is not a spec/mockup conflict to flag.

### Background policy

**Chrome paints no background fill at all.** roost does not repaint the
terminal background, and it no longer paints bands on top of it either: the
tab bar row, the hint bar row, the flash, the alt-warning bar, the dead-pane
action bar and the focused collapsed row have all lost their fills. The three
that needed to *shout* reverse the terminal's own pair instead — plain
(`attention()`) for the neutral flash, red-tinted (`attention_problem()`) for
the two problem bars; the two chrome bars and the focused collapsed row carry
no surface of their own and are distinguished by ink weight and markers
instead. The active tab cell's bg is `Color::Reset` —
the single `bg` chrome sets, and it sets it to nothing, so the label fuses
with whatever the terminal's own background is.

Mechanically gated twice: `theme::tests::no_chrome_call_site_sets_a_background_fill`
(source scan under `src/ui/`) and `render::tests::chrome_paints_no_background_fill`
(every cell of every drawn chrome state). A companion gate,
`render::tests::structure_colour_never_carries_text`, enforces that `rule()`
appears on no text-bearing cell. (SPEC-GAP-4, closed by this amendment.)

### Bold policy

Chrome uses **no `Modifier::BOLD` anywhere** (the mockup's TUI region is
regular weight throughout; hierarchy is carried by ink weight and the one
red). Modifiers permitted, and what each is load-bearing for:
- `DIM` — the quiet text rung and the quiet red (§2 table), plus the modal
  backdrop mechanism (C12). **[Amended 2026-07-27]** it is a token component
  now, not only a mechanism.
- `REVERSED` — the three attention surfaces (C10/C11/C16), copy selection
  (C17), copy cursor (C24), search hits (C17 amendment).
- `UNDERLINED` — the stack header rule (C6), the copy cursor inside a
  selection and the current search hit (C24/C17), and the pane editor's
  name row (C32, amended 2026-08-15 — the field's own rule, padded edge
  to edge so it doubles as the name/note separator).

Program output keeps whatever attributes it sent.

### Glyph inventory (chrome)

**[Amended 2026-08-07, C5 — the Working dot retires]** the Working spinner
(`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`, the Braille Patterns block U+2800, `theme::SPINNER_FRAMES` —
pi-tui's own default loader frames, verbatim) · `◆` U+25C6 · `○` U+25CB · `·`
U+00B7 · `✕` U+2715 · `▎` U+258E (active-tab / focused-row marker) · `▌`
U+258C (expanded-stack edge) · `│` U+2502 (tab separator) · `▏` U+258F
(rename cursor, existing) · `❯` U+276F (picker selection) · `✓` U+2713
(saved) · `…` U+2026 (tab overflow). All are single-width — including every
spinner frame, so the swap costs no badge-column or tab-strip width anywhere.
The double-width `🪶` is removed with the brand block (C2), eliminating the
wide-glyph offset hazard in mouse math.

Terminals without braille glyph support render the spinner frames as tofu;
accepted deliberately (see C5) rather than adding per-terminal fallback
detection, which is not knowable from inside a PTY.

**[Amended 2026-07-22, fleet features]: the fleet features add NO new
glyphs.** The feed (C20) reuses `◆` with its C5 meaning; zoom/raw are word
indications (`ZOOM`/`RAW`, C9) plus a plain-text `raw` badge token (C23); the
float (C22) is chrome-identical to a pane; the copy cursor (C24) is
modifier-based. Any new glyph appearing under `src/ui/` is a DEVIATED.

**[Amended 2026-07-27, SPEC-ux U3]:** one glyph added since: `↑` U+2191
(`theme::SCROLLED`, single-width) — the scrollback marker leading the badge's
`↑N` token (C4) and the scroll hint's `↑N/M` position (C9). It appears only
while a pane's view is frozen in history; no other new glyph is sanctioned.

**[Amended 2026-07-28, vertical-tabs tribunal]: the roster and the tab count
add NO new glyph.** The roster (C27) draws the `❯` selection marker, the `▎`
focused-row marker and the C5 status glyphs, all of them already here and all
with their existing meanings. The tab bar's count cell (C2) holds a **digit or
`+`** — ASCII text in the glyph's own style, the same class of thing as the
`raw` badge token, not a symbol. This inventory governs symbols; text tokens
are governed by their own contracts. Any new *glyph* under `src/ui/` is still
a DEVIATED.

**[Amended 2026-08-15, C32]:** two glyphs added: `¶` U+00B6 — the note
presence marker (C4 badge, C8 collapsed rows, and the roster through the
shared row) — and `⋮` U+22EE, appended to it as `¶⋮` when a note has body
lines under its headline. Both single-width (no badge-column or mouse-math
hazard), both drawn in `ink()`, and both governed by C32's reveal-on-visit
rule: they appear only on a pane carrying a parking note. No other new
glyph is sanctioned.

---

## 3. Component contracts

Verdict rule for the auditor: every "Target" bullet is a predicate; a contract
is ALIGNED iff all its bullets hold in the rendered output / code.

### C1 — Theme module (tokens centralized)

**Current:** no theme module; every style is inline in `src/ui/render.rs`
(e.g. `:50, :60, :102–104, :146, :185, :229–231, :243–249, :274–289, :344–347,
:379, :392–397`).

**Target:**
- `src/ui/theme.rs` exists, exporting: the chrome style tokens from the §2
  table; the status-glyph mapping (C5); the spinner frame function (C5); and
  the chrome glyph consts listed above.
- `grep -n 'Color::' src/ui/render.rs` matches only inside the vt100 blit
  section (`conv_color` / `cell_style`) — all chrome styles are built from
  `theme::` items.
- No `Color::` literal for ok/warn/info hues exists anywhere under `src/ui/`.

**[Amended 2026-07-27, theme inheritance]** The token table is now styles, not
colours, so the exports are **accessor fns** (`ink`, `quiet`, `rule`,
`accent`, `accent_quiet`, `attention`)
— a token carries a modifier as well as a colour and `Style`'s builders are
not `const fn`. **[Amended 2026-08-07, C5]** `pulse_bright` is retired along
with the colour pulse it existed for — the accessor list above already
reflects its removal — and `theme::spinner_frame(elapsed) -> char` joins the
exports beside `status_style`/`tab_summary_style` as the Working glyph's frame
selector. theme.rs remains the single source of chrome styling, which is what
this contract has always been about; the "tokens only from theme.rs" gate is
unchanged in force. Three new mechanical gates ride with the 2026-07-27
amendment, and their failure is a C1 DEVIATED:
- `theme::tests::no_truecolor_or_indexed_colour_is_constructed_in_src` —
  chrome inherits or it doesn't ship (§2 stance), with the marked
  program-output exemption in `conv_color`/`cell_style`.
- `theme::tests::no_chrome_call_site_sets_a_background_fill` and
  `render::tests::chrome_paints_no_background_fill` — §2 background policy,
  scanned in source and measured on drawn frames.
- `render::tests::structure_colour_never_carries_text` — `rule()` (ANSI 8) is
  structure only; a `DarkGray` cell must hold a box-drawing glyph or a blank.
Two further tests pin the principle itself:
`theme::tests::both_text_rungs_derive_from_reset` and
`render::tests::every_chrome_word_is_drawn_in_ink_the_user_already_reads`.

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
- Each tab i renders as 8 parts, in order:
  `marker(1) + " " + label + " " + glyph(1) + " " + "│"(1) + " "(1)` where
  `label = tab_label(i, name) = "{i+1} {name}"` (function unchanged).
  - marker: `▎` fg `ACCENT` for the active tab; `" "` for inactive.
  - label + number: fg `FG` for active, fg `MUTED` for inactive; active cell
    bg = `Color::Reset`, inactive bg = `TAB_STRIP`. No BOLD.
  - glyph (aggregate `TabSummary`, semantics unchanged from `app.rs:448–476`):
    NeedsInput `◆` `ACCENT` · Working the C5 spinner frame `ACCENT` (amended
    2026-08-07 — animates, no longer a pulsed colour) · Unknown `·` `DIM` ·
    Waiting `○` `FG` · Quiet = single space.
  - separator `│` fg `RULE`, drawn after every tab (including the last), with
    a trailing gutter space on `TAB_STRIP` after it.
  - **[Amended 2026-07-23, herdr-inspired tab breathing room]** The trailing
    gutter is the 8th part: it gives every separator symmetric 1-cell padding
    (one space before `│` from the glyph slot, one after), so adjacent tabs no
    longer touch (`│▎` → `│ ▎`) and the strip reads closer to the mockup's
    roomy `padding:9px 18px` tabs. A cell TUI expresses "horizontal padding"
    as space cells; one symmetric gutter cell is the tasteful analog — more
    would waste columns and cut tab capacity. Vertical padding is NOT added:
    the bar stays one row (see the height note in §4).
- `mouse::tab_width(i, name)` returns `display_width(label) + 7` and the
  renderer emits exactly that many columns per tab; the trailing gutter counts
  as that tab's own columns; `tab_at_x` starts at 0.
  Worked example (audit fixture): tabs `["main", "api"]` → tab 0 occupies
  cols 0..13, tab 1 cols 13..25; `tab_at_x(_, 12) == Some(0)`,
  `tab_at_x(_, 13) == Some(1)`, `tab_at_x(_, 25) == None`.
  **[Superseded 2026-07-28 — the formula is `+ 8` and the worked example moves
  with it; see this contract's count-cell amendment below.]**
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
  **[Amended 2026-08-20 — a failed save outranks tab names.]** "Tabs win" is
  right for *context* — a cwd, a mode word, `saved ✓` — all of which you can
  get another way. `save failed ✕` is not context: it is the only standing
  sign that the workspace on disk is going stale, and this rule dropped it
  *whole*, so on an ordinary 80-column terminal with five tabs roost could
  fail every single write and show nothing at all. (The same audit gave
  `App::save` a C10 flash on the ok→failed transition; that fires once, and a
  standing signal that vanishes exactly when the terminal is busy is not a
  standing signal.) On a failed save the ladder therefore inverts: the cwd
  yields, then the mode word yields (the reverse of U15's usual order —
  ZOOM/RAW/COPY can be rediscovered by pressing a key, "not reaching disk"
  cannot), and finally the strip itself yields, scrolling under U7 with its
  `…` markers rather than the indicator being dropped. The active tab is
  always among the ones kept, so what is spent is other tabs' names,
  temporarily and visibly. One floor: if even the active tab could not be
  drawn beside the indicator, the indicator is dropped after all — a tab bar
  with no tabs is not a trade worth making, and C10's flash has already
  fired. **[Corrected 2026-08-20, same audit: that last clause was not yet
  true when it was written.]** The flash was drawn only inside the hint bar,
  so with hints hidden — `Alt+/`, or a terminal under 3 rows, and the narrow
  terminal is exactly where this floor fires — neither signal reached the
  user. C10 is amended in the same change to paint a flash over the body's
  last row when the hint bar is not drawn, which is what makes the fallback
  real rather than nominal.
  `mouse::effective_status_width` takes `save_ok` and `active` for exactly
  this; `status_fit` grows one failed-save-only rung.
- Mouse unit tests (`mouse.rs:250–260`) are rewritten to the new offsets **in
  the same change** as the renderer (lockstep rule, §4).

**[Amended 2026-07-27, SPEC-ux U7 — the strip scrolls to the active tab]**
Overflow above ("tabs render left-to-right; the first tab that would collide
is not drawn") described a strip anchored at tab 1, which meant the active
tab could be off-screen entirely — selected by chord or by `Alt+a`'s
cross-tab jump, with nothing on the bar saying where you were, and (per U7)
unclickable to get back. The strip now scrolls: `mouse::tab_strip` picks the
**leftmost** window that still fits the active tab whole, so it scrolls the
least it can and keeps as much history on screen as possible (the active tab
rides the right edge until later tabs pull it back).
- Marker semantics are unchanged, just no longer right-only: a single `…`
  fg `MUTED` marks *each* end that hides tabs. The leading one occupies
  column 0 and is paid for out of the tab budget (`TabStrip::x0` = 1 when
  scrolled); the trailing one stays opportunistic — drawn only if a spare
  column remains, never displacing a tab. Neither is ever a tab: clicking
  either switches nothing.
- Hitboxes ride the same layout: `tab_at_x` reads `tab_strip` and returns
  **real tab indexes**, not window offsets, so a click on the first drawn
  tab of a scrolled strip selects that tab and not tab 1 (§4/§5 lockstep;
  the worked example above still describes the unscrolled case).
- The window is derived from the active tab every frame, never stored: there
  is no scroll state to drift, and no way to be looking at a strip that
  doesn't contain where you are.

**[Amended 2026-07-27, SPEC-ux U15 — the status area carries the mode word
when the hint bar can't]** The right status area becomes
`"{MODE} · {cwd} · {save}"`: the C9 mode word leads it fg `MUTED` (state
reads a step brighter than the context beside it; still inside the ink ramp,
never the accent — it is not an alarm), followed by the existing cwd fg `DIM`
and save word. The word is present **iff the hint bar is not drawn** — hidden
via Alt+/ or squeezed out by a terminal under 3 rows — **and** it is not
`NORMAL`; real modes and the `ZOOM`/`RAW` pseudo-states all qualify. With
hints hidden, a zoomed pane was previously indistinguishable from a one-pane
tab and SCROLL/COPY/RAW vanished entirely (live QA), which made the mode word
— a modal-safety affordance — conditional on an unrelated toggle.
Consequences for this contract's width math, all shared by
`mouse::status_fit` so the renderer and `tab_at_x` cannot disagree:
- Overflow gains one rung *inside* the status area, **ahead** of the existing
  drop: full → `{MODE} · {save}` (the cwd yields first: the word is safety,
  the cwd is context you can also read off the pane) → no status area at all.
  Tabs still win outright over whatever is left. **[Superseded 2026-08-20
  for a failed save only — see this contract's dated amendment below: the
  save indicator gains a further rung beneath the mode word and then the
  *strip* yields to it, so "tabs win outright" now holds for a healthy save
  and nothing else.]**
- The wider area takes its columns from the tab hitboxes exactly as it takes
  them from the drawn tabs — `tab_at_x` is fed the fitted width, so clicks
  and pixels stay in lockstep (§4/§5).

**[Amended 2026-07-27, SPEC-ux U11 — what a tab switch means]** The bar shows
tabs; these two rules say what selecting one does, and they hold for every
selector (digit, click, `Alt+a`'s cross-tab jump, being moved onto a tab when
another closes):
- **Selecting the active tab is a no-op.** Not "a switch that happens to end
  where it started": `go_to_tab` returns before touching anything. Live QA
  pressed Alt+1 while on tab 1 and lost zoom; the same path also hid the
  float and reset focus to the tab's first pane. Out-of-range digits stay the
  silent no-op they were.
- **Each tab returns focus to the pane it was left on** (`App::tab_focus`,
  session-only, never persisted — a fresh launch starts on each tab's first
  pane). Fallback to the tab's first pane when a tab has no memory yet or its
  remembered pane has since closed. The memory is held as a set of pane ids,
  not a map keyed by tab index: `ws.tabs` has no stable identity and indexes
  shift on close/reorder/undo, so a tab's entry is the one remembered id that
  tab still owns — a vanished tab can never hand its memory to another.

**[Amended 2026-07-27, theme inheritance — the bar loses its strip and the
active tab loses its highlight]** This is the contract the reported bug lived
in. `TAB_STRIP` is deleted:
- **Row 0 has no fill.** The bar is ink on the terminal's own paper; the gap
  before the status area and the row past the last tab show the user's
  background, not a band roost painted over it. (A near-black strip on a light
  terminal was half of what the user saw.)
- **Active vs inactive is ink weight plus the `▎` marker**, never a
  background: active label = `ink()` with no background set (chrome paints no
  fill anywhere), inactive label = `quiet()`. The old rule — near-white `FG` on
  `Color::Reset` — spelled *white text on the user's white background*, which
  is precisely why the active tab was invisible. There is no "inactive bg =
  `TAB_STRIP`" any more; inactive cells set no bg at all.
- Glyph table per C5 as amended; separator `│` stays `rule()` — structure,
  where a faint theme costs a hairline rather than a word.
- The **status area** (U15) keeps its relative rule, respelled for a two-rung
  ramp: the mode word reads a step brighter than the cwd beside it, which is
  now `ink()` over `quiet()`. The saved word is `quiet()`, "save failed"
  `accent()`. There is no `TAB_STRIP` beneath any of it. Width math, overflow
  markers and every hitbox are untouched.

**[Amended 2026-07-28, vertical-tabs tribunal (SPEC-ux U27) — the glyph gains
a count]** The summary reduces a whole tab to one glyph, so `◆` has always
meant "at least one pane here needs you" and nothing more: **a tab with three
blocked agents was pixel-identical to a tab with one**, and the only way to
learn the difference was to go and look. This is the sibling half of the gap
C27 closes from the other side (the tribunal's provenance is recorded there).

- **Content.** Each tab's parts become **nine**, the count cell riding
  immediately after the glyph with no space between them:
  `marker(1) + " " + label + " " + glyph(1) + count(1) + " " + "│"(1) + " "(1)`.
  The count is how many of that tab's panes are in the **summarized** state.
  `App::tab_summary` returns it alongside the summary — one call, so the
  ranking (U13's, verbatim) and the number can never disagree about which
  state won. It is never a pane total: the glyph says what the tab is
  reporting and the count says how much of it there is. `◆3`, `⠹2`, `✕2`.
- **Style.** The count carries the **glyph's own style**. **[Amended
  2026-08-07, C5]** the style itself no longer flips — the *glyph* does (the
  Working spinner) — so the count still rides in lockstep with it: `⠋3`→`⠙3`→…
  reads as one animating token rather than a static digit stuck to a spinning
  dot, the same "count is part of the signal" guarantee the pulse used to
  carry.
- **Geometry rule — stability over saving a column.** The cell is **always
  reserved**: a space below 2, the digit for `2`–`9`, `+` for 10 or more —
  always exactly one column (`render::tab_count_cell`). Tab widths therefore
  do **not** jitter as agent statuses flip. That is worth the column: a strip
  that reflowed whenever a background pane changed state would move every
  hitbox on the bar (and every tab's drawn position) for a signal the user did
  not act on, and §4/§5's lockstep rule exists precisely because those two
  must never disagree. `+` past nine because two digits would break the
  one-column invariant the hit math rests on, and because past nine the exact
  number stops being actionable — "more than you can eyeball" is the honest
  reading.
- **Lockstep.** `mouse::tab_width` becomes `display_width(label) + 8`, and the
  renderer's cell layout, `mouse::tab_width`/`tab_at_x` and their tests all
  move in the **same commit** (§4/§5, hard rule).
  Worked example, superseding the one above: tabs `["main", "api"]` → tab 0
  occupies cols 0..14, tab 1 cols 14..27; `tab_at_x(_, 13) == Some(0)`,
  `tab_at_x(_, 14) == Some(1)`, `tab_at_x(_, 27) == None`.
- **Measured cost (+1 column per tab), as promised rather than assumed.** How
  many tabs the strip shows before the `…`, at the default names
  (`main`, `tab2`, `tab3`, …), status area dropped as C2's ladder already
  drops it once tabs overflow:

  | bar width | before | after |
  |---|---|---|
  | 80 | 6 tabs | **5** |
  | 100 | 7 tabs | 7 |
  | 120 | 9 tabs | **8** |
  | 160 | 11 tabs | 11 |

  With one-character tab names (the cheapest labels there are, so the fixed
  columns dominate) it is 8→7, 9→9, 11→10, 15→14 at the same four widths.
  So: **one fewer visible tab at two of the four widths, none at the other
  two** — the +1 only costs a tab when it crosses a multiple of the tab width.
  Per U7 the strip *scrolls*, so the effect is fewer tabs visible before the
  `…` marker, never a tab you cannot reach: every tab stays reachable by
  `Alt+1..9`/`Alt+0`/`Alt+m`/`Alt+Shift+m`, and the active one is always drawn. That
  is the cost this amendment is worth paying — an unreadable count is worse
  than a shorter strip, and C27's roster now answers "which panes, exactly"
  for anything the bar cannot say.
- One consequence worth recording, visible in the tests: at a width where the
  scrolled window now fills the bar exactly, the **trailing** `…` yields —
  it is opportunistic by contract and may never displace a tab (U7's
  amendment). The leading marker, which is paid for out of the tab budget, is
  unaffected.
- No new glyph: the cell holds ASCII text (a digit or `+`) in the glyph's own
  style, so §2's inventory — which governs *symbols* — is unchanged.

**[Amended 2026-08-07, client request — cross-tab arrow-key focus, C31]**
U11 rule 2 above says every selector lands a tab switch on `tab_focus`
(`App::tab_focus`, else the tab's first pane) and calls that true "for every
selector" — **withdrawn as an absolute**: C31's `Alt+Right`/`Alt+Left` off a
tab's geometric edge is a new selector, and it deliberately does not follow
rule 2. It lands on the destination's geometric leftmost/rightmost pane
instead — "keep going right" means arriving nearest the edge just crossed,
not wherever that tab was last left. Rule 1 (selecting the active tab is a
no-op) is untouched: a lone tab never reaches C31 at all — fewer than two
tabs is its own gate, checked before any switch. See C31 for the mechanism.

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

**[Amended 2026-07-27, theme inheritance]** Focused border fg = `accent()`
(the user's ANSI 1); unfocused border fg = `rule()` (ANSI 8). Borders are the
sanctioned home of the theme-variant slot: a theme that renders ANSI 8 faint
costs a hairline here and nothing else. Everything else in this contract —
`BorderType::Plain`, no BOLD, no border title — is unchanged.

**[Amended 2026-08-21 — the border carries the pane's label again.]** This
contract's target line "the border-embedded title line is **removed** … pane
identity moves entirely to the corner badge (C4)" is reversed for identity
and extended for the note. A pane's top border carries its **identity
title** (C4) left-aligned; its bottom border carries its **note title**
(C32) left-aligned. `ZOOM · {n} hidden` keeps the top-right (C21), and is
served first — identity yields the columns zoom takes plus one, so the two
never touch.

**[Cross-reference 2026-09-01, boxed collapsed rows]** "Unfocused border =
`rule()`" governs panes drawn *as panes* (the border around live content).
A **boxed collapsed member** is the one unfocused border not drawn in
`rule()`: its box is C8's stack chrome — `accent_quiet()`, `accent()` in
the transient focused case — audited under C8, not here.

The 2026-07-27 reading was that a border is structure and text on it is
noise. Measured against real frames, the alternative was worse: the badge
was painted over the pane's own **first content row**, which is its most
valuable one — a prompt, the first line of a diff, the answer just asked
for. It took 29–55 columns of a 58-column pane, and the *entire* row of a
28-column one, on all four panes of a four-way split, while the border
above sat empty. `tests/firehose.rs` had carried a workaround for this since
it was written (a deliberately short cwd and an in-band `PS1`, both to keep
the badge from clobbering the echo it measures); both are deleted by this
amendment, and with the pre-amendment renderer they are demonstrably load-
bearing — that gate fails at keystroke 6 without them.

Nothing else in C3 changes: `BorderType::Plain`, no BOLD, `accent()` focused
and `rule()` unfocused, and U21's two-cell drag handle all stand. A title
occupies cells that were already border and already drag; the content cells
it vacates go back to plain click-to-focus, so no click behaviour changes.

**[Amended 2026-07-28, SPEC-ux U21 — a shared border is a handle.]** The
border between two panes can be dragged to move that split. Nothing about
how a border is *drawn* changes; this contracts what it *is*.

- **The seam is two cells.** Every pane draws its own border, so neighbours
  are separated by `a`'s far edge and `b`'s near edge — adjacent
  columns/rows. Both are the handle. A user aiming at "the line between the
  panes" cannot be expected to distinguish them, and a one-cell mouse target
  is unkind. `mouse::seam_at` is the single hit-test, and it is only ever a
  *shared* border: a pane's outer edge, against the body, focuses like any
  other cell of that pane.
- **The drag is a closed loop on the drawn geometry.** `layout::resize_pane`
  works in ratios of a split whose extent the mouse layer cannot see — the
  pair may be nested two splits deep — so an open-loop cell→ratio conversion
  drifts and never recovers. Every event instead measures where the border
  is *now* and asks for the change that would put it under the cursor, so an
  imperfect estimate is corrected a cell later. Pinned by a test requiring
  the border to land within one column of the pointer, which the open-loop
  form fails.
- **A seam drag does not move focus.** Grabbing a border is an act on the
  layout, not a choice of which pane to type into.
- **No seam while zoomed** (C21: one pane fills the body), and none on a
  collapsed stack member — those rows are C6/C8's own chrome, not a split
  ratio anything could move. The gesture latches at button-down like P20's
  pane latch, and for the same reason: the pointer leaves the two-cell
  border almost immediately.
- **Middle-click-closes-a-tab is rejected**, not deferred — see U21's own
  entry. It contradicts C26 (tabs die by last-pane close only) and the
  "deliberately left out" list's *close-whole-tab gesture*; a tab can hold
  several agents, and middle-click is the button that pastes on X11.

### C4 — Corner badge

**Current:** `render.rs:376–383` — fg DarkGray, top-right, text = pane name
only, **suppressed when focused**; pure helper `corner_badge()` `:409–424`
with tests `:530–555`.

**Target:**
- Drawn on **every** non-collapsed pane, focused included (suppression branch
  at `:376` removed; occlusion of the inner app's top-right cells is accepted
  by design).
- Content: `"{id} {name} · {adapter} {glyph}"` — where `name` is the display
  name (`title`, else the adapter/cwd fallback). When the pane has no custom
  title the fallback already contains the adapter, so the ` · {adapter}`
  segment is skipped (no `"pi · repo · pi"` dup): untitled badge =
  `"{id} {name} {glyph}"`.
  **[Amended 2026-07-27, SPEC-ux U2]:** the badge leads with the pane id —
  the join key for `roost send <id>`, which previously appeared nowhere in
  the TUI — and the display-name fallback is no longer render-local: it is
  the shared `core::app::display_name_live` (title, else
  `{adapter} · {cwd-tag}`), the one helper every fleet surface (badge,
  collapsed rows, feed, notifications, flashes) derives pane identity from.
  **[Amended 2026-07-27, SPEC-parity P6]:** the naming chain gains a middle
  rung — explicit Alt+r title → the pane's **live OSC 0/2 title** (agent
  panes only; a plain `shell` pane skips this rung, since its title comes
  from `PS1` and by default restates `user@host: /path`, duplicating the cwd
  tag and crowding the badge — a hand-launched agent still qualifies the
  moment `observe_panes` promotes the pane's adapter, and demotes back with
  it) → `{adapter} · {cwd-tag}` — resolved by `App::display_name` (the free
  `display_name_live(spec, None)` remains the chain with no live title, for the paths that
  have a spec but no runtime, e.g. a closed pane's feed line). An agent CLI
  that publishes `spinner + task` through its title therefore says what it is
  doing, on the badge, in collapsed rows, in the feed, and in notifications.
  The live title is untrusted text: control characters are stripped and it is
  bounded to 48 characters *before* the badge's own width clipping, so a
  paragraph-length task line degrades by clipping rather than by blowing up a
  row. An explicit rename still wins outright — a name the user typed is a
  decision, and an app repainting its title every frame must not overwrite
  it. The same resolved name is published to the **host terminal** as
  `OSC 2 ; roost · {focused pane's display name}` on focus and title changes
  (throttled to at most one update per 200 ms), and reset to a plain `roost`
  on exit and in the panic hook — roost's chrome and the outer tab now agree
  on what a pane is called.
- Style: text fg `MUTED`; glyph fg per C5 status colors. **[Amended
  2026-08-07]** Working no longer pulses the colour — the glyph itself
  animates (the C5 spinner) while its colour stays the steady one red.
- Geometry: top row of the pane's inner area, right-aligned, one column of
  right breathing room — `corner_badge()` clipping behavior and its tests
  stay (helper may evolve to return spans for the two-tone styling).
- [Amended 2026-07-22, fleet features] A raw pane's badge additionally carries
  the `raw` token per C23.
- **[Amended 2026-07-27, SPEC-ux U3/N1]** Whenever the pane's grid-clamped
  scrollback offset is > 0 the badge carries a dim `↑N` token — fg
  `ACCENT_DIM`, same family as the `raw` token, placed glyph-adjacent (after
  `raw` when both): `"{id} {name} · ↑N {glyph}"`, raw
  `"{id} {name} · raw · ↑N {glyph}"`. N is the *view's* offset read back from
  the grid (`PaneBackend::scroll_offset`), never a caller's unclamped
  counter. While the token shows, the Working glyph's C5 animation is
  suppressed (frozen at its steady frame, amended 2026-08-07 — was "steady
  base color" under the retired colour pulse): an animating glyph asserts
  "alive right now", which a frozen view must not do (N1) — the glyph itself
  keeps reporting the true status, and any path that resets the offset
  removes the token and resumes the animation. Collapsed rows and the tab bar
  keep animating — they show no grid, so there is no frozen view to lie
  about.

**[Amended 2026-07-27, theme inheritance]** Badge text `quiet()`; glyph per
C5 as amended (animating when Working and the view is live, 2026-08-07); the
`raw` and `↑N` tokens both `accent_quiet()`, still their own spans and never
folded into the text. No bg — the badge is a watermark over the pane's own
last output, and `quiet()` is exactly the right shape for that: the user's
ink, one rung back.

**[Superseded 2026-08-21 — the note segment moved to its own border; see
C32's amendment of that date. The composition and styling below still
describe what is drawn, just on the bottom edge rather than in this badge.]**
**[Amended 2026-08-15, C32 — the note segment.]** A pane carrying a parking
note (C32) grows a note segment between the identity text and the
`raw`/`↑N`/glyph tail; identity keeps leading, so U2's id-first rule holds
untouched and a narrow pane clips the note before the join key:
- **Focused:** `"{id} {name} · ¶ {headline} ({age}) {glyph}"` — `¶` (or
  `¶⋮` when the note has body lines under its headline) plus the note's
  first line in `ink()`, one span; the `({age})` tag in `quiet()`. The
  headline is the badge's one full-strength element — reveal-on-visit is
  C32's display contract, and the focused pane is the visit. This is the
  only chrome text that ever renders note *content*.
- **Unfocused:** the bare marker, `"{id} {name} ¶ {glyph}"` — `¶`/`¶⋮` in
  `ink()`, no headline, no age. Presence, never content.
- The age tag is `render::age_word`: floored coarsest-sensible unit —
  `now` under a minute, then `{n}m` / `{n}h` / `{n}d` — read from one
  wall-clock sample per frame (C5's shared-read idiom, `now_unix_secs`).
  A backwards clock clamps to `now`. Staleness confessing its age is the
  design's whole defense against a Tuesday note lying on a Friday. A note
  **missing its timestamp** (C32 always writes the pair together, so only
  hand-edited state or a future writer can produce one) shows **no age tag
  at all** rather than a fabricated `now` — an absent fact renders as
  absent, never as fresh.
- Width behavior unchanged: same parts pipeline (`clip_spans`), tail
  trimmed first; no bg, no new colour, no BOLD.

**[Amended 2026-08-21 — the badge becomes the identity title.]** The corner
badge moves off the pane's first content row and onto its **top border**,
left-aligned, one column of breathing room after the corner. See C3's
amendment of the same date for why. Everything C4 contracts about *what* it
says is unchanged and pinned as an equality in
`identity_title_is_two_toned_and_reads_exactly_as_the_badge_did`: the same
`"{id} {name} · {adapter}"` composition, the same U2 id-first rule, the same
P6 naming chain, the same `raw`/`↑N` tokens in the same order, the same
two-tone styling (`quiet()` text, C5 glyph, `accent_quiet()` tokens). Only
the placement moved, and with it:

- **Geometry.** Left-aligned on the top border, budget `width − 2` (the
  border's title area, corners excluded — the same span `inner.width`
  measures), less whatever C21's zoom title has taken.
- **The glyph is never shed.** The badge clipped its own tail, so a long
  name deleted the status glyph — the one element reporting whether the
  agent is alive. The tail is now fixed and the *name* absorbs the
  shortfall, down to nothing; a title that cannot fit even ` {glyph} `
  disappears whole rather than clipping the glyph in half.
- **Styling does *not* follow C21's "match the border you sit on".** That
  rule is right for `ZOOM · {n} hidden`, which is one undifferentiated
  statement about the pane. This title has structure C5 requires be
  coloured: the glyph carries status and `raw`/`↑N` carry view state, and
  flattening them to the border colour would delete the signal.

### C5 — Status glyph system + spinner

**Current:** glyphs from `status.rs:34–42`; colors `render.rs:282–290`
(Working Green, NeedsInput Magenta, Waiting Yellow, Idle DarkGray, Exited
Red); tab variant `:271–280`; no animation.

**Target — one table, used by every chrome surface that shows status
(tab bar, corner badge, collapsed rows). [Amended 2026-08-07, C5 — see the
spinner amendment below for the full rule]:**

| AgentStatus | Glyph | Color | Animated |
|---|---|---|---|
| Working | `SPINNER_FRAMES` — `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`, 80ms/frame, shared elapsed clock | `ACCENT` | **yes** — steady `⠋` only in the three sanctioned cases below (N1 frozen view, C15 legend, C27 filter tag) |
| NeedsInput | `◆` | `ACCENT` | no (steady) |
| Waiting | `○` | `FG` | no |
| Idle | `·` | `DIM` | no |
| Exited | `✕` | `ACCENT_DIM` | no |

TabSummary variant: NeedsInput/Working/Waiting as above; Unknown `·` `DIM`;
Quiet renders a single space (both semantics unchanged).

**[Amended 2026-07-27, SPEC-ux U13 — closes SPEC-GAP-2]** The TabSummary
table gains **Exited `✕` `ACCENT_DIM`** — the same glyph and colour as the
`AgentStatus` row above it, because C5 is one table and a tab of corpses may
not read differently from a corpse. It ranks **between Waiting and Quiet**:
a live agent with anything to say outranks a dead one, and a dead one
outranks silence. A pane counts as exited when its runtime reports `Exited`
*or* when its spawn failed outright (a recorded `App::dead` entry — no
runtime, but not unspawned either, so it must not read as `Unknown`). Before
this, a background tab whose agents had all died rendered the Quiet blank:
one transient bell, then a tab that looked idle forever (§7 SPEC-GAP-2).
Quiet keeps the blank cell — it is now the only summary that draws nothing,
and it means what it says.

**Pulse spec [retired 2026-08-07 — kept for the historical record; the live
rule is the spinner amendment at the end of this contract]:** period
**1100 ms**, 50% duty, two phases: elapsed-ms in `[0, 550) → ACCENT`,
`[550, 1100) → ACCENT_DIM`, repeating. Phase is computed from one shared
clock (elapsed since app start), so **all pulsing glyphs flip in unison**;
it is re-evaluated every frame and the ~33 ms draw tick (`main.rs:164–173`)
bounds phase-edge error to one frame. No new timers, no extra redraw
scheduling. Only Working `●` pulses — never `◆` (steady red = "waiting on
you", pulsing red = "alive"). Pure function
(`theme::pulse_phase(elapsed) -> Color` or equivalent) with unit tests at the
boundaries: 0 → ACCENT, 549 → ACCENT, 550 → ACCENT_DIM, 1100 → ACCENT.

[Amended 2026-07-27: one sanctioned exception to "pulse: yes" — a scrolled
pane's corner badge shows Working steady (no pulse) while its view is frozen
in history, per C4's N1 carve-out. The table above describes live views; C4
owns the frozen-view composition. The exception itself survives 2026-08-07
unchanged in shape — see the spinner amendment below, which respells it as
"no animation" rather than "no pulse".]

**[Amended 2026-07-27, SPEC-parity P7]** The same frozen-view rule governs
the *host cursor*, which is the loudest liveness assertion roost makes.
roost places it inside a pane only when all four hold: the pane is focused,
it has not exited, its app has not hidden its own cursor (DECTCEM `?25l`),
and its view is at the live tail (`scroll_offset == 0`) — the pure
`render::should_place_cursor`. A scrolled pane therefore shows the `↑N`
token, a steady glyph, and no cursor: three surfaces telling one consistent
story. Additionally, the focused pane's DECSCUSR shape (`CSI Ps SP q`, 1..=6)
is mirrored to the host terminal on change, so an editor's insert bar looks
like a bar; a pane that asks for no shape — including the pane focus moves
*to* — restores the terminal's default, as do exit and the panic hook.

**[Amended 2026-07-27, theme inheritance — the table and the pulse]** Read
the table above through §2's rename map: Working/NeedsInput `accent()`,
Waiting `ink()`, Idle `quiet()`, Exited `accent_quiet()`; TabSummary Unknown
`quiet()`, Exited `accent_quiet()`, Quiet a blank. (The rename map still
holds — Working still reads `accent()`. The paragraph that used to follow
this one, describing a two-phase colour flip between `pulse_bright()` and
`accent()`, is superseded whole by the amendment immediately below: it
described a mechanism that no longer exists rather than a renaming of one
that does.)

**[Amended 2026-08-07, C5 — the pi spinner replaces the colour pulse]** The
client reported that the pulsing dot read as "waiting for your attention" —
backwards, since Working is precisely the status that should *not* pull the
eye the way NeedsInput's steady ◆ does. The fix swaps which axis carries
"busy": **animation now means busy; steady red means attention**, so ◆
becomes the only glyph that ever asks the user to look. Concretely:

- **Glyph.** Working's `●` retires. In its place, `theme::SPINNER_FRAMES` —
  `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` — pi-tui's own `loader.js` `DEFAULT_FRAMES` (pi 0.81.1),
  copied verbatim, so a pi pane's own badge agrees with what pi draws inside
  the pane. Colour is unchanged: `accent()`, the one red, never modulated —
  every frame is the same red the old steady phase was.
- **Clock.** `theme::spinner_frame(elapsed) -> char` replaces
  `theme::pulse_phase`, reading the same shared clock (`App::elapsed`, one
  read per frame, C5/D3) so every Working glyph on screen — badge, collapsed
  row, tab strip, tab count cell — shows the *same* frame in the same draw,
  exactly as every pulsing glyph used to flip in unison. Frame length is
  80ms (pi's own nominal rate), a full ten-frame rotation every 800ms —
  finer than roost's own render tick (`main.rs`'s ~33ms crossterm poll
  timeout, which redraws every iteration even with nothing to read, so a
  silent Working pane still animates), so no quantizing against a coarser
  tick is needed. Unit boundaries: 0ms →
  frame 0, 79ms → frame 0, 80ms → frame 1, 795ms → frame 9, 800ms →
  frame 0 (wraps).
- **Three sanctioned steady-frame cases — everywhere else, Working animates.**
  All three show `theme::GLYPH_WORKING` (`SPINNER_FRAMES[0]`, `⠋`) instead of
  the live spinner, and all three exist for the same reason: an animating
  glyph asserts "alive right now", which none of the three may claim.
  1. **N1 frozen view (C4).** A scrolled pane's corner badge — the frozen-view
     exception carried over unchanged in shape from the colour-pulse era. The
     `↑N` token already says the view is history; an animating glyph beside
     it would contradict that.
  2. **C15 help legend.** The status legend row in the help overlay is static
     text and cannot itself animate, so it prints Working's representative
     frame rather than a live one.
  3. **C27 status-filter tag.** The roster's title tag names *which tier* is
     filtered — it is a label for the tier, not a live report on any one
     pane's animation, so it stays steady the same way the legend does.

  Collapsed rows and the tab bar carry none of these three carve-outs (no
  grid to freeze, no static label to be) — they always animate, exactly as
  they always pulsed.
- **Terminals without braille glyph support** render the spinner frames as
  tofu. Accepted deliberately: pi itself (the flagship adapter) already
  requires braille support to draw its own spinner, and per-terminal
  fallback detection is not knowable from inside a PTY.
- **`pulse_bright()` and `theme::pulse_phase` are removed outright** — no
  remaining callers, and the property they existed for (a second
  guaranteed-visible red so a terminal ignoring `DIM` couldn't flatten the
  pulse into a steady dot) has no equivalent need once animation is carried
  by glyph shape rather than colour.
- Every other rule here — one shared clock, only Working animates, the
  frozen-view carve-out, `should_place_cursor` — stands unamended.

### C6 — Stack header row

> **[2026-09-01: the `Alt+s` toggle became the `s`/`Shift+s` pair — see §8's
> second amendment. This contract's geometry is unchanged.]**
>
> **[2026-09-02, C42: `Alt+s` now repeats, absorbing the next enclosing split
> on each press. This contract's geometry is still unchanged — a climbing
> rung produces a flat `Stack` of more members, which is a shape this
> header already sizes for (`height >= n + 3`, any n) and which
> `all_stack_layout` has always been able to build. What the ladder does add
> is a *reason to read the header*: `STACK · 2 PANES` becoming
> `STACK · 3 PANES` is the only feedback a rung gives, so the header is now
> load-bearing for teaching the gesture, not merely for labelling the
> region. C7 and C8 are likewise unchanged, for the same reason.]**

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
  **[Clarified 2026-08-20, reliability audit.]** `n` is always ≥ 2: a stack
  is a way of sharing one region between several panes, so a stack of one is
  not a stack, and `layout.rs` normalizes it into a plain `Pane` wherever one
  can arise — `remove_pane` for a stack that shrinks to one, `dedupe_pane_ids`
  (reached from `Workspace::validate_and_repair`) for a `workspace.json` that
  holds one outright or is left with one after duplicate ids are stripped.
  So no singular label is contracted, and none is needed: `"STACK · 1 PANES"`
  is unreachable rather than merely ungrammatical. This was not always true —
  before the normalization, `toggle_stack` on a one-member stack also built a
  `Split` with a single child, a shape the module states it never constructs.
  The header a one-member stack drew was the visible half of that bug: a row
  borrowed from the only pane it described.
- Every cell of the header row (both texts and the fill between) carries
  `Modifier::UNDERLINED` — the cell-level translation of the mockup's 1px
  bottom rule.
- `layout.rs` unit tests updated for the new stack geometry in the same change.

**[Amended 2026-07-27, theme inheritance]** Header text is `quiet()`; the
`Modifier::UNDERLINED` rule that stands in for the mockup's 1px bottom border
is unchanged, and is the right shape for the new stance — a rule expressed as
an attribute rather than a colour cannot be swallowed by a theme. Still no bg.

**[Amended 2026-09-01, boxed collapsed rows]** Collapsed members are no
longer always 1-row bars. When the stack's area affords it, each collapsed
member gets **3 rows** (C8's bordered box: border + row + border):
- Boxed geometry applies iff `area.height >= 1 + 3·(n−1) + MIN_SPLIT_ROWS`
  (header + boxes + the expanded member's own split floor, `layout.rs`'s
  `MIN_SPLIT_ROWS = 10`) — i.e. `area.height >= 3n + 8`. The expanded member
  never pays for the boxes out of its floor: the boxes are a legibility
  upgrade where rows are plentiful, not a new fixed cost.
- In boxed geometry the expanded member's height is
  `area.height − 1 − 3·(n−1)`; the header is always shown there (the boxed
  threshold clears `n + 3` for every `n`).
- **Below the boxed threshold, geometry is exactly the pre-amendment rules**
  — 1-row bars, header iff `area.height >= n + 3` — so the scarcity
  arithmetic that set the old thresholds still holds where it bites, and
  nothing changes at the sizes where rows were already contended.
- Click-to-expand and focus math need no cases: a collapsed member's
  `PaneRect` simply grew, and every consumer (hit test, `layout::neighbor`,
  C33 swaps) is rect-driven. The C25 fit predicate keeps exempting collapsed
  rects whatever their height.

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

**[Amended 2026-07-27, theme inheritance]** The `▌` edge is `accent_quiet()`
and the focused member's full border is `accent()`; the reasoning (don't stack
a quiet red inside a red frame) is unchanged.

**[Clarified 2026-09-01, boxed collapsed rows]** The smear this contract
guards against is two reds fused **within one frame** — the `▌` edge
overpaints a column of the member's own border, so frame and edge read as
one object in two reds. A boxed collapsed member (C8) sitting above or
below a focused member's `accent()` border is different in kind: two
*separate* frames meeting at a row boundary, the same adjacency two
`rule()`-bordered split panes have always had. The box therefore stands
next to a focused member, and this contract's own rule — the `▌` edge is
withheld from the focused member — is unchanged.

### C8 — Collapsed stack rows

**Current:** `render.rs:333–342` — 1-row Paragraph, text `" {glyph} {name} "`;
focused = Black on status-color bg, unfocused = status-color fg.

**Target:**
- Row format: `marker(1) + glyph(1) + " " + id + " " + name + fill + "{adapter} · {word}" + " "`
  with the right segment right-aligned.
  **[Amended 2026-07-27, SPEC-ux U2]:** the pane id rides ahead of the name
  (same placement as the C4 badge), styled with the name.
  - marker: `▎` fg `ACCENT` when the row is the focused pane, else `" "`.
  - glyph: per C5 (Working animates, the spinner frame; every other status
    is a steady glyph in its own colour).
  - id + name fg by state: Working/NeedsInput → `FG`; Waiting/Idle → `MUTED`;
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

**[Amended 2026-07-27, theme inheritance — the focused row loses its fill]**
- **No row fill in either state.** "Focused row additionally paints bg `RULE`
  across the full row width" is withdrawn: chrome paints no fills (§2). Focus
  is carried the way the tab bar carries it — the `▎` `accent()` marker plus
  **full-strength ink on the row's id+name, whatever its status**
  (`render::collapsed_name_style(status, focused)`).
- The unfocused name ramp collapses with the token table: Working/NeedsInput
  `ink()`; Waiting/Idle/**Exited** `quiet()`. Exited used to have its own
  third rung; §2 sanctions only two text rungs, and a dead pane already says
  so twice on the same row — the `✕` `accent_quiet()` glyph and the `exited`
  state word.
- Right segment `quiet()`. Width-shedding order, the no-dup rule, the state
  words and the C23 `raw · ` prefix are all untouched.

**[Amended 2026-08-15, C32]** A noted pane's right segment leads with a
`¶ ` marker in its own `ink()` span (ahead of the C23 `raw · ` prefix's
position in the string, i.e. `"¶ raw · {word} "` when both) — the C4
marker's exact meaning here: presence of a parked note, never its text;
collapsed rows and the roster stay reveal-on-visit surfaces. It rides the
right segment, so a narrow row sheds it with the segment — the C32 marker
is a courtesy, identity and status stay the row's priority. The roster's
rows (C27) inherit it through the shared `collapsed_row_spans`.

**[Amended 2026-09-01, the collapsed row becomes a box]** A 1-row bar
between two fully bordered panes read as a rule, not a pane — the one
pane-shaped surface in the body with no frame around it, and the easiest
thing on screen to miss (client report with screenshot). When the stack's
area affords the rows (C6's boxed-geometry amendment: the member's rect is
3 rows tall), the collapsed member draws as a **bordered box**:
- `Block::bordered()`, plain line glyphs (the C3/C12 border shape), border
  fg **`accent_quiet()`** — the quiet red that already means "stack chrome"
  (C7's expanded-member edge), so the box reads "collapsed pane of this
  stack" without spending a new colour. When the row is the focused pane
  (transient — focus on a collapsed member auto-expands it), the border is
  **`accent()`**, C3's focus signal verbatim.
- The row itself is unchanged: the same `collapsed_row_spans` output —
  marker, glyph, id + name, right segment, same styles, same width-shedding
  order — rendered on the box's single inner row at `width − 2`.
- A collapsed rect below 3 rows (the C6 fallback regime, or a clamped last
  member) renders the 1-row bar exactly as before. The roster (C27) and
  feed reuse of the row format are untouched — the box is stack chrome,
  not part of the row.
- Click-to-expand covers the whole box, borders included (the rect grew;
  the hit test is rect-driven); `route_mouse` still forwards nothing for a
  collapsed member.

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
  **[Amended 2026-08-09, copy resume]** The dead-focused Normal list gains a
  fifth pair, `y copy resume`, between `f fresh` and `Alt+w close` — present
  only when the pane has a session pointer (same
  `App::resume_command_line(..).is_some()` predicate as C16's bar). Ahead of
  close/quit in the yield order because those two are discoverable
  everywhere; `y` exists only on this bar.
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
- **[Amended 2026-07-27, SPEC-ux U6 — pair order is the yield order]:** the
  seven Normal-mode pairs are unchanged in content but re-ordered to
  `Alt+? keys` · `Alt+n new` · `Alt+↵ launch` · `Alt+s stack` ·
  `Alt+←↓↑→ focus` · `Alt+w close` · `Alt+r rename`. Since pairs drop whole
  from the right, the list order *is* the yield order, and the old one
  dropped `Alt+? keys` first — at 120 cols, the moment `◆ N needs you`
  appeared (live QA), i.e. exactly when a new user under pressure most needs
  the pointer to every binding that just vanished. It now leads, so it is
  the **last** pair to yield: at any width that fits one pair, that pair is
  the help pair. `Alt+r rename` trails and drops first — the one pair whose
  absence costs nothing that isn't a keystroke away under `Alt+?`. Total
  width is unchanged (100 columns), so the "gains NOTHING" bullet above
  still holds.
- **[Amended 2026-07-27, SPEC-ux U3]:** while the mode is Scroll, the right
  segment additionally carries the position `↑N/M` fg `DIM` immediately
  ahead of the mode word (`… needs-you segment  ↑N/M SCROLL `) — N = the
  focused pane's grid-clamped view offset, M = its banked history rows,
  both read from the backend (never `Mode::Scroll`'s own counter, which can
  hold a phantom the grid refused — U9). The position rides *inside* the
  right segment, so the existing fit/yield machinery covers it: pairs drop
  whole before any of the segment clips.
- **New / amended pair lists** (all obey the same styling formula):
  - Feed mode (C20): `↑↓ scroll` · `Esc close`.
  - Focused-raw Normal (C23): exactly **one** pair — `Alt+Shift+p exit raw`.
    Every other hint would be a lie: nothing else is intercepted.
  - Copy mode (C24, replaces the two-pair list at `:70`):
    `hjkl move` · `v mark` · `y/↵ yank` · `drag select` · `Esc exit`
    (63 columns — fits beside the right segment at 100 cols).
  - Zoomed Normal keeps the standard seven pairs (they all still work).
- **[Amended 2026-07-27, SPEC-ux U17 — the copy list carries the whole
  vocabulary]:** the copy-mode list becomes, in this (= yield) order:
  `hjkl move` · `w/b/e word` · `0/$ ends` · `v/V mark` · `y/↵ yank` ·
  `drag select` · `Esc exit` — **83 columns**, so it still fits beside the
  right segment at the 100-col floor (pinned by
  `hint_pairs_copy_mode_is_the_c24_list_amended_by_u17`). `w/b/e` and `V`
  are new keys (C24 amendment below); `0`/`$` are not — they had existed
  since C24 while appearing in no hint and no help row, which is the same
  as not existing. Labels are deliberately terse (`ends`, `mark`) to buy
  the columns: the full wording lives one keystroke away under `Alt+?`.
  **[Re-amended 2026-07-27, SPEC-ux U19]** `o open` joins the list after
  `y/↵ yank` (92 columns with it — still inside the floor).
- **[Amended 2026-07-27, SPEC-ux U20 — picker]:** the picker's list becomes
  `↑↓ choose` · `↵ open` · `1..9 launch` · `Esc cancel` (48 columns).
- **[Amended 2026-07-27, SPEC-ux U25 — feed]:** the feed's two-pair list
  becomes `↑↓ select` · `PgUp/Dn page` · `↵ go to pane` · `q/Esc close`
  (56 columns). `PgUp`/`PgDn` and `q` were **already implemented** and
  advertised nowhere; `↵` is new (C20 amendment). `scroll` becomes `select`
  because with an actionable entry the arrows move a cursor, not a view —
  the hint has to name what the key now does.
- **[Amended 2026-07-27, SPEC-ux U16/U20 — two mode lists grow]:** rename
  becomes `type {pane,tab} name` · `←→ move` · `↵ save` · `Esc cancel`
  (45 columns) now that there is a cursor to move (C13); the picker becomes
  `↑↓ choose` · `↵ open` · `1..9 launch` · `type filter` · `←→ dir` ·
  `Esc cancel` (71 columns, C14). Both stay inside the 100-col floor beside
  the right segment, and neither touches the Normal-mode seven.
- **[Amended 2026-07-27, SPEC-parity P21 — scrollback search]:** the bar
  gains a mode and grows one token.
  - **Mode word list** gains `SEARCH`, on the same terms as the rest: a real
    mode always beats the Normal-slot pseudo-words.
  - **Scroll's pair list** becomes `↑↓ scroll` · `PgUp/Dn page` ·
    `/ search` · `n/N next` · `Esc exit` (**59 columns**). Scroll mode is
    where a search starts and where its hits are walked once the prompt
    closes; an unadvertised key is an absent one (the lesson U20 paid for
    with the picker's unhinted `j/k`).
  - **Search's own list** is `type filter` · `↵ keep` · `Esc cancel` ·
    `n/N next` (**52 columns**). The two exits lead — they are the yield
    order's last survivors, and a prompt you cannot leave is the worst
    modal there is — while `n/N` trails, because it is the pair that keeps
    working *after* the prompt closes and is advertised again on the Scroll
    list when it does.
  - **Copy's list is unchanged at 92 columns**, deliberately: `/` works from
    copy mode too, but an eighth pair would push the list past the 100-col
    floor and drop `Esc exit` — the one hint that must never yield. The key
    is taught by the C15 overlay's copy/scroll row instead. A hint bar that
    drops the escape hatch to advertise a convenience has its priorities
    backwards.
  - **Right segment**, while the prompt is up: `{needs-you}  /{query}▏
    {i}/{n} SEARCH`. The query is the **one `FG` token on this bar** — it is
    text the user is typing right now, and DIM live input is input you
    cannot proofread; the `▏` caret is the same glyph the rename dialog uses
    (C13), so a typed prompt looks like a text field everywhere in roost.
    The `i/n` hit counter takes `↑N/M`'s slot and its `DIM` styling: while
    searching, "which hit am I on" *is* the position question, and showing
    both would be two answers to one question. `0/0` when nothing matches —
    an empty result is the answer, not the absence of one.
  - **No dialog.** Rejected: a centered prompt box (C12). The search's whole
    output is *the pane behind it* — the jumped view and the highlighted
    hits — so a modal over that pane would cover the answer while asking the
    question. This is the same reasoning that keeps copy mode overlay-free
    (C17/C24), and it is why the prompt had to fit in the bar's existing
    fit/yield machinery rather than get a surface of its own.

**[Amended 2026-07-27, theme inheritance — the bar loses its fill]** `BAR` is
deleted: **row bg is dropped entirely**, so the hint bar is ink on the user's
own paper (a near-black band on a light terminal was the other half of the
reported bug). Keys `accent()`, labels `quiet()`, the right segment's
aggregate `accent()`, mode word and position `quiet()`, and the P21 search
query `ink()` — it is text being typed right now, and the quiet rung is not
for input you must proofread. Precedence, pair lists, fit/yield order and
every width number are untouched.

**[Amended 2026-07-28, C27 — the roster's mode and list]** The mode-word list
gains `ROSTER`, on the same terms as `FEED` and `SEARCH` (a real mode always
beats the Normal-slot pseudo-words), and the roster's own pair list is
`↑↓ select` · `PgUp/Dn page` · `↵ go to pane` · `type filter` · `Esc close`
(**68 columns** — inside the 100-col floor beside the right segment). It is
the feed's list with `type filter` in place of `q/Esc close`'s `q`: the
roster filters as you type, so a letter cannot also be a command (U20), and
the hint has to advertise the key set that actually exists. The **Normal-mode
seven gain nothing** — the 100-column argument in this contract's 2026-07-22
amendment is unchanged, and `Alt+Shift+a` is discoverable through `Alt+?`
(C15) beside the `Alt+a` it pairs with.

**[Amended 2026-08-07, exit UX audit F1 — precedence flips]** "Precedence
unchanged: alt-warning (C11) takes the bar over flash (C10), which takes it
over hints" is reversed: **flash now wins over the alt-warning**; hints stay
last. The old order let the alt-warning pre-empt `draw_hint_bar`'s flash
branch outright, so a copy performed while the warning wanted the bar showed
no confirmation at all — and since C11's elapsed gate is also gone this same
date, an unresolved alt-trap would otherwise have swallowed every flash for
the rest of the session, not just a startup window. With flash checked
first, a transient confirmation always gets its `FLASH_WINDOW`; the
alt-warning (which now persists until resolved, not for a fixed window)
reclaims the bar the moment the flash expires. Pinned by
`flash_wins_the_hint_bar_over_the_alt_warning`.

**[Amended 2026-08-07, exit UX audit F2 — right segment gains the ○
fallback]** `"◆ {N} needs you · Alt+a"` is no longer the segment's only
shape. It now renders `App::attention_segment()`: unchanged text/style when
a real ◆ exists (`Some((n, true))`), `"○ {n} your turn · Alt+a"` fg `ink()`
when the ◆ pass is empty but `attention_ring`'s Waiting fallback isn't
(`Some((n, false))`), omitted only when `Alt+a` truly has nothing to do
(`None`). `ink()`, not `accent()`, is deliberate: it's the same style
`theme::status_style` already gives the Waiting glyph everywhere else, one
visual step back from the accent-red ◆ case so a real ◆ still reads as more
urgent. This closes C19's 2026-08-07 "known gap" amendment — see there for
the full rationale — and means the segment now matches what `Alt+a` will
actually do in every case, not only N > 0. Pinned by
`attention_segment_matches_the_ring_in_every_case` (app.rs) and
`hint_bar_right_segment_renders_the_waiting_fallback_one_step_back_from_needs_input`
(render.rs).

**[Amended 2026-08-15, C32]** The mode-word list gains `NOTE`, and the note
editor gets its own pair list (the C13 shape, plus the vertical keys):
`type note` · `↵ save` · `Shift+↵ new line` · `↑↓←→ move` · `Esc cancel` —
65 columns rendered (`hint_pair_cols`), inside the 100-col floor beside the
right segment.
`Ctrl+↵`/`Alt+↵` are unhinted synonyms of `Shift+↵`, the same alias rule as
everywhere else on this bar. The Normal-mode seven-pair list still gains
nothing: `Alt+Shift+n` is discoverable via `Alt+?` (C15), and the badge's
`¶` marker (C4) is its own advertisement.

**[Amended 2026-08-19, C34 — the chord spellings on this bar are derived]**
Every `Alt` key this contract quotes as bar text (the Normal seven, the
raw-mode pair, the dead-focused list's `Alt+w`/`Alt+q`, and the right
segment's `· Alt+a`) is now **resolved from the live keymap**, not printed
from these strings. On a default keymap the bar is byte-identical to what
this contract has always specified — the quotations below stay accurate as
the *default* rendering — but under a `config.json` remap the bar correctly
disagrees with them, which is the whole point (C34).

Three consequences worth stating rather than inferring:
- **The pair lists and the 100-column arithmetic are untouched.** The seven
  Normal pairs, their U6 yield order, and every column count in this
  contract describe the default keymap and still hold.
- **A remap degrades through the machinery already here.** A longer resolved
  label makes trailing pairs drop from the right, exactly as a narrow
  terminal does — `fit_hint_pairs` is a prefix loop, so nothing clips. No
  new rule was needed.
- **A disabled chord drops its pair** rather than advertising a dead key.
- **Mode-local keys stay literal** (`hjkl`, `v/V`, `Esc`, `n/N`, `↵`, `f`,
  `y`): config.json cannot reach them, so they cannot go stale.

**[Amended 2026-08-15 (later), C32 combined editor]** `NOTE` retires with
its mode; the word for the combined pane editor is `EDIT`. Its pair list:
`type name / note` · `↵ save` · `Shift+↵ new line` · `↑↓←→ move` ·
`Esc cancel` — 72 columns rendered, still inside the floor. The Rename
list's leading pair reads `type tab name` unconditionally (Tab is Rename's
one target now). And one Normal-mode relabel, content otherwise untouched:
`Alt+r rename` becomes **`Alt+r edit`** — the chord names and notes now,
and "rename" would under-describe it; two columns narrower, so the
seven-pair fit math only loosens.

### C10 — Flash message

**Current:** `render.rs:56–64` — Black on Green BOLD.

**Target:** `" {msg} "` fg `FG` on bg `RULE`, no modifiers. (Flash carries
generic notices — "copied", extension updates — so it gets the neutral
elevated treatment, not a reserved color; ok-green is banned from chrome.)
Timing/precedence unchanged.

**[Amended 2026-07-27, SPEC-ux U14]:** Styling and timing are untouched, but
the *copy* flash's wording is now contracted — it was the one flash that
could lie, firing at extraction time while `clipboard::copy` discarded both
channels' results. It reads `copied N chars` only when a native helper
exited successfully, `copied N chars (OSC 52)` when the escape went out
unacknowledged (the SSH/tmux channel — it may equally have vanished into a
terminal that ignores it), and `copy failed` when neither landed, with no
count to quote. The text comes from the pure `App::copy_flash_text`, and
all copy paths (mouse release, keyboard `y`/`↵` in copy mode, and the
dead-pane bar's `y` copy-resume — amended 2026-08-09) flash only *after*
the clipboard has answered.

**[Amended 2026-08-09, flash-text reconciliation]** The failure variant's
full text is `copy failed: no clipboard channel worked; check your
terminal's clipboard support` — the code's wording (`App::copy_flash_text`'s
`Failed` arm, pinned by its tests) supersedes the bare `copy failed` this
section previously quoted: the trailing hint names the one thing actually
worth checking (a missing native helper is the common half of "both channels
lost"), and shortening the code to match the spec would have deleted
information. Short-form `copy failed` mentions elsewhere in this doc refer
to this full string.

**[Amended 2026-08-20, reliability audit — a flash is not a hint.]** The
flash was drawn only by `draw_hint_bar`, so it appeared only when the hint
bar did. `hints_shown()` is false both when the user pressed `Alt+/` and when
the terminal is under 3 rows — and in that state *every* C10 message reached
nobody: all 38 of them, including C38's refusals ("no room to split"), U14's
copy result, the startup notice that a `workspace.json` was set aside, and
U22's confirm-arm prompts, so `Alt+w` armed a destructive second press —
"last pane — Alt+w again to quit roost" — while showing nothing at all. That
is precisely the hazard C2's 2026-07-27 amendment names for the mode word:
a safety affordance made conditional on an unrelated toggle.

With the hint bar not drawn and at least two rows of terminal, the flash now
paints over the **body's last row** instead — clearing that row first. The
clear is load-bearing, not tidiness: `attention()` is `REVERSED` and nothing
else (§2 — a flash inverts the user's own pair rather than assuming a
background roost does not own), and ratatui *patches* styles, so with no fg
of its own the message takes the fg of whatever cells it lands on. On the
hint bar those are empty. Over the body they carry the pane border drawn
moments earlier in the same frame, and the message came out reversed in the
border's colour: `RULE` under an unfocused pane (structure colour carrying
text, banned by §2) and, under a focused one, `ACCENT` reversed —
bit-identical to `attention_problem()`, so "copied 12 chars" rendered as
C11/C16's reserved problem treatment. The cells past the message kept their
border glyphs, reversed, leaving a ragged band. Clearing the row first makes
both paths land on empty cells, which is what actually makes "same function,
so they cannot drift" true of *styling* and not only of text and timing.
The §2 chrome fixtures gained a hints-hidden flash in the same change: they
had never exercised this surface, which is why every gate passed while it
was wrong. Painted over, not by shrinking
the body: the geometry the panes were laid out and PTY-resized to must not
change for two seconds and back, and the row repaints itself from the pane
beneath the moment the flash expires. Text, styling (`attention()`), timing
and precedence are all unchanged — both paths call the same `draw_flash`, so
they cannot drift. Modals still paint after it (C22 stacking order).

**[Amended 2026-07-27, SPEC-ux U22]:** "timing unchanged" is superseded for
confirm-arm prompts only: a flash that arms a destructive second-press
confirm lives exactly `CONFIRM_WINDOW` (3 s) instead of `FLASH_WINDOW`
(2 s), and dies early with its arm — prompt and armed window may never
disagree in either direction. Ordinary flashes keep `FLASH_WINDOW`; styling
and precedence are untouched.

**[Amended 2026-07-27, theme inheritance; revised same day after the design
supervisor's SG-1]** `" {msg} "` is now `attention()`
— `Modifier::REVERSED`, no colours — instead of `FG` on a `RULE` fill. Same
intent (neutral elevated treatment, no reserved hue), expressed as a reversal
of the terminal's own pair, which is guaranteed contrasty in any theme. Timing
and precedence remain untouched.

### C11 — Alt-key warning

**Current:** `render.rs:45–54` — Black on Yellow.

**Target:** same text, fg `FG` on bg `ACCENT_DIM` — "roost-level problem"
bars (this and the dead-pane bar, C16) share the dim-accent treatment. The
mockup's `warn` yellow is program-output-only and must not be used.

**[Amended 2026-07-27, SPEC-ux U4 — trigger and wording]** Styling is
unchanged; what fires the bar, and what it says, are now contracted:
- **Trigger: evidence, not an allowlist.** Show it while keys are arriving
  and *not one of them has carried Alt*, inside the existing startup window
  (`ALT_HINT_WINDOW`, 8 s). Any terminal qualifies — gating on
  `TERM_PROGRAM == "Apple_Terminal"` left iTerm2, the README's own
  recommendation, silent while it swallowed Option identically. The trigger
  is also self-timing: with Option-as-Meta off, the chord the user just
  tried arrives as an unmodified key (Option+b → `∫`), so the failed chord
  is itself the evidence that raises the bar. One Alt key ever — or the
  window running out — ends it for the session, as before.
- **Wording: per terminal, from roost's own `TERM_PROGRAM`** (the host's
  value; panes are handed `TERM_PROGRAM=roost`, but that is the child's
  environment, not this process's). `Apple_Terminal` → Terminal > Settings >
  Profiles > Keyboard, "Use Option as Meta Key"; `iTerm.app` → iTerm2 >
  Settings > Profiles > Keys, Left Option = `Esc+`. Anything else gets a
  terminal-agnostic line — never a menu path that terminal doesn't have.

**[Amended 2026-08-20 — the bar is no longer hint-bar-only]** Styling,
trigger and wording are untouched; where it may be *drawn* is not. The bar
reached the screen only through `draw_hint_bar`, so `hints_shown()` gated it
— false both under `Alt+/` and under 3 rows of terminal. The failure mode is
circular and was the reason to fix this ahead of the rest of C11: if Alt
really is being swallowed, `Alt+/` cannot bring the hint bar back, so the one
sentence that explains why no chord works is unreachable by the only key that
would reveal it. It now falls back to the body's last row on exactly the
terms C10's flash does (same amendment, same `Clear`, same shared function
per path, flash still winning over it), with one difference worth stating
because it is a real cost rather than an oversight: this bar is
**persistent**, so it holds that row for as long as the trap is detected
rather than for two seconds. That is accepted because the trigger is already
evidence-gated (U4/F1 above — the signature of a swallowed Option chord, not
a mere absence of Alt), a user in that state has no working chords at all,
and one Alt key ever — or the 8 s window closing — ends it.

**[Amended 2026-07-27, theme inheritance; revised same day after the design
supervisor's SG-1]** The bar is `attention_problem()`
(`REVERSED`), not `FG` on an `ACCENT_DIM` fill. The "roost-level problem bars
share one treatment" rule survives verbatim — this and the dead-pane bar
(C16) are both `attention_problem()` — and the mockup's `warn` yellow stays
banned. The first amendment made all three bars `attention()`, which silently
dropped a distinction the pre-theme design carried in colour (`RULE` fill for
the neutral flash, `ACCENT_DIM` for problems): `copied 12 chars` and "Alt keys
aren't reaching roost" rendered identically. The distinction is restored from
primitives that survive any palette — reversing `accent()` paints the row in
the user's red with their background as the ink, so a problem still reads as a
problem, while the flash keeps the plain reversal.
Trigger and wording are untouched.

**[Amended 2026-08-07, exit UX audit F1 — trigger re-anchored to the accent
signature, elapsed gate dropped]** The 2026-07-27 amendment's "self-timing"
reasoning does not hold up: "keys are arriving and not one of them has
carried Alt" is true of nearly all typing, not only a swallowed chord — a
healthy Ghostty/iTerm2 user's first action (typing a shell prompt) satisfies
it inside the 8s window, so the bar fired on a correctly-configured
terminal, and it fired *before* the user had pressed anything roost cares
about. Worse, it pre-empted `draw_hint_bar`'s flash branch outright (see the
companion C9 amendment), so a copy performed in that window showed no
confirmation either.
- **Evidence, corrected.** SPEC-ux U4's originally proposed "Option-accent
  signature", dropped in the 2026-07-27 amendment as "unnecessary", turns out
  to be exactly what was missing: the character the **standard US** macOS
  keyboard layout emits for `Option+<letter>` with Option-as-Meta off,
  arriving with **no Alt modifier at all**, is what actually distinguishes a
  swallowed chord from ordinary typing — "some key arrived" does not. The
  full set is 26 characters, one per `a..=z` (`Alt+n` → `˜`, `Alt+w` → `∑`,
  and 24 more — `is_alt_swallow_char`'s `matches!` arm list in `app.rs` is
  itself the complete definition, nothing abbreviated). Scoped to the US
  layout only: a non-US layout's own Option+letter table is a different 26
  characters, and this does not attempt to cover them (see the false
  positive this creates, next amendment). `App::note_key_seen` now takes the
  `KeyEvent` itself and only records evidence when the unmodified key
  matches that table. `note_alt_seen` (a real Alt key getting through,
  ending the warning for the session) is unchanged.
- **The elapsed-time gate is re-keyed to the evidence, not dropped** —
  corrected the same day by the design-audit amendment immediately below
  (SG1): dropping it outright was wrong. See there for why and for what
  still holds from the reasoning here (the read-first user's requirement).
- Trigger function: `wants_alt_hint(alt_seen, since_evidence)` — see SG1
  below for the current signature; wording (previous amendment) is
  untouched.
- Both hard requirements from the original finding are pinned by `App`-level
  tests: a healthy terminal typing an ordinary shell prompt never sets
  `alt_swallow_at` (`healthy_terminal_typing_a_shell_prompt_never_fires_the_alt_hint`);
  a read-first user whose first Alt press lands at t=40s still gets the bar
  (`read_first_user_still_gets_the_hint_however_late_the_first_alt_press_is`).

**[Amended 2026-08-07, design audit SG1 — the evidence is real but not
unambiguous, so it must not latch]** The amendment above dropped
`ALT_HINT_WINDOW` on the reasoning that evidence this specific "needs no
clock to stay accurate". That reasoning was wrong: every character in
`is_alt_swallow_char`'s table is *also* a directly-typed letter on some
non-US macOS layout — `ç` (Portuguese, French, Turkish), `å`/`ø` (Nordic),
`ß` (German), `µ`, `´`, `¨` among them. A user typing their own language can
produce the identical evidence with Alt never involved, and with no window
at all, `alt_seen == false` latched the red `attention_problem` bar over the
hint row **for the rest of the session** — worse than the false positive
this contract set out to fix, which at least expired after 8s. This is the
tradeoff being deliberately acknowledged, not a gap that went unnoticed: the
evidence narrows the *rate* of false positives a great deal (ordinary
English-language shell use essentially never produces these 26 characters)
but cannot eliminate them, so the design has to bound their *cost* instead.
- **`ALT_HINT_WINDOW` returns, keyed to the evidence's own timestamp
  (`App::alt_swallow_at: Option<Instant>`), not to launch.**
  `wants_alt_hint(alt_seen, since_evidence: Option<Duration>)` shows the bar
  only while `since_evidence < ALT_HINT_WINDOW` — a false positive clears
  itself in a few seconds instead of owning the row for the session. The
  read-first-user requirement the original elapsed-gate removal was for
  survives intact: the window starts at the *evidence's* timestamp, still
  never at launch, so a first Alt attempt at t=40s (or any t) still raises
  the bar the moment it lands.
- **Fresh evidence re-arms the window.** Each matching keystroke overwrites
  `alt_swallow_at` with the current time, so a genuinely broken Alt layer —
  the user keeps trying Alt chords, keeps producing the evidence — keeps
  being told, not just once.
- **A real Alt chord dismisses the warning permanently, for the rest of the
  session, regardless of any evidence that arrives afterward** — `alt_seen`
  is checked first in `wants_alt_hint` and never reset. This is what
  protects a multilingual user who *does* use Alt chords: the first real one
  ends it for good, the same guarantee the original fix always made.
- Pinned by
  `a_non_us_layouts_own_letter_self_clears_instead_of_latching_for_the_session`
  (the false positive named above, and its bound),
  `fresh_evidence_re_arms_the_window`, and the unchanged
  `the_alt_warning_appears_on_swallowed_alt_evidence_and_dies_on_the_first_real_alt`
  (evidence after `alt_seen` stays suppressed).
- The "no dead-key composition" assumption `is_alt_swallow_char` relies on
  (a terminal emits `Option+n` as `˜` immediately, not composed with a
  following vowel) is believed true of every terminal this project targets
  but is **not verified by anything in this suite** — it would need a real
  terminal and a real OS keyboard driver. Said plainly here rather than
  implied as tested (design audit SG3).

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
- **[Amended 2026-07-27, SPEC-ux U8]** A modal owns the **non-keyboard**
  input surface too, not just the keys — being topmost visually and
  transparent to the mouse was the contradiction U8 recorded (a click during
  Rename moved focus and redirected the commit; a paste landed in the pane
  hidden underneath; the wheel scrolled the pane under the feed). While any
  C12 modal is up:
  - **Mouse.** Nothing beneath it is ever mutated: no focus change, no tab
    switch, no pane scroll, nothing forwarded to a pane's app. The
    composition root gates on `App::modal_active` before any pane/tab
    routing (`main.rs::handle_mouse`, the shape copy mode already used) and
    calls `App::handle_modal_mouse` with the dialog's **drawn** rect, which
    comes from `render::modal_rect` — the same geometry `draw_mode_overlay`
    paints, so hit-testing can't drift from the screen (§4/§5 lockstep).
  - **Wheel.** The feed pages by its own PgUp/PgDn step (half the overlay
    height, `App::overlay_page` — one source for key and wheel) wherever the
    pointer sits, since nothing beneath is reachable anyway. Every other
    modal swallows the wheel.
  - **Click.** Help closes on any click (its "any key closes it", in mouse
    form); the feed closes on a click outside its rect; the picker launches
    the row clicked (`mouse::picker_row_at`) and cancels on a click outside.
    **Rename is the carve-out: an outside click is swallowed, not a cancel**
    — its buffer is unsaved work, and discarding it on a stray click is
    U8(a)'s own harm inverted. Esc/Enter stay its ways out.
  - **Paste.** `Event::Paste` routes through `App::handle_paste`: into the
    Rename buffer (printables only — a pasted newline must not commit the
    rename and no control byte may reach a title), swallowed by the other
    three, forwarded to the focused pane in every non-modal mode (Scroll and
    Copy included: they draw no dialog and hide nothing).

**[Amended 2026-07-27, theme inheritance]** Border `accent()`, title `ink()`.
The interior's "`Clear` then default bg, no explicit paper fill" line is now
the rule everywhere rather than a modal-only carve-out (§2 background policy).
Backdrop, anchoring, stacking order and the U8 input-ownership rules are
untouched.

**[Amended 2026-07-28, C27]** The fleet roster is a **fifth** C12 modal
("four" above, and "the other three" in the paste rule, predate it). It takes
the frame, the backdrop, the anchoring and every U8 rule unchanged; its two
mode-specific answers are contracted in C27 — the wheel moves its cursor (it
has one; the feed's `offset` *is* its cursor, so both are "the wheel drives
the selection"), and a click on a row jumps while a click outside dismisses,
the picker's shape. Paste is swallowed, like every non-Rename modal.

**[Amended 2026-08-15, C32]** The note editor is a **sixth** C12 modal. It
takes the frame, backdrop, anchoring and stacking unchanged, and extends
exactly two of the U8 answers to a second member: an outside **click** is
swallowed, not a cancel (Rename's carve-out, same unsaved-work rationale
with more at stake), and a **paste** belongs to its buffer — with the one
filter difference contracted in C32: newlines survive as line breaks
(CR/CRLF normalized) where Rename strips them, because in a multi-line
field a pasted newline is content, not a commit. Wheel swallowed, like
every non-feed/roster modal.

**[Amended 2026-08-20]** U8 above contracts what a modal owns of the
**mouse**. The keys are not C12's to answer: an Alt chord leaves every mode,
modal or not, and that rule is **C24b**'s — amended the same day with the
two chords a mode may keep and the gates that pin them. C12 adds nothing to
it; a modal is a mode, and holds keys exactly as weakly as one.

### C13 — Rename dialog

**Current:** `render.rs:153–168` — 44×3, Double/Cyan, plain input text.

**Target:** C12 frame; input text fg `FG`; cursor stays the `▏` suffix
(`:167`), fg `FG`. Size and behavior unchanged.

**[Amended 2026-07-27, SPEC-ux U16 — the `▏` becomes a real caret]:** "cursor
stays the `▏` suffix" is superseded. The dialog now carries an insertion
point, so the glyph renders **at** it (`rename_field`), not always at the
end — same token, same `FG`, no new styling. It was already spelled like a
caret; it just never moved, which is the tell that made a text field nobody
tried to edit in place. Everything else is unchanged: 44×3, C12 frame, and
the U8 rule that an outside click does not dismiss unsaved work.

- **Char-indexed, not byte-indexed.** The point counts what the user sees.
  A byte index would render the caret in the wrong column for any accented
  or emoji name, and slicing one mid-character panics — so the field
  clamps and converts (`byte_at`) at every edit.
- **Motion is clamped, never wrapping.** `←` at the start and `→` at the
  end do nothing. A one-line field that teleported the caret between its
  ends on an overshoot would make every held arrow key a hazard.
- **Hint bar** gains `←→ move` (48 columns since the 2026-08-15
  amendment — the leading pair reads `type tab name` now, C9).

**[Amended 2026-08-15, C32 combined editor — Tab is the one target
left.]** Pane renames moved into the combined pane editor (C32's dialog,
where the name is the underlined first row); `Alt+r` opens that editor and
`Alt+Shift+r` still opens this dialog for the tab. Everything above — the
frame, the moving caret, char-indexing, the clamped single-line motion,
U8's outside-click rule — is unchanged and now describes the tab dialog
alone (heading ` rename tab `, hint list leading `type tab name`).
`RenameTarget` keeps its enum shape with `Tab` as the sole variant, so the
mode doesn't churn twice if a third single-line target ever appears.

### C14 — Picker (quick-launch)

**Current:** `render.rs:169–193` — Double/Cyan; selected row Black on Yellow.

**Target:** C12 frame. Rows render as:
- selected: `"❯ {item}"` — `❯` fg `ACCENT`, item fg `FG`, **no bg highlight**;
- unselected: `"  {item}"` fg `MUTED`.
(The `❯`-prefix selection idiom is lifted from the mockup's approval-prompt
markup, lines ~669–671.) Size and behavior unchanged.

**[Amended 2026-07-27, SPEC-ux U8]** Clicking a row selects **and** launches
it in one press — the picker is a launcher, so "select, then confirm" would
be a second click for nothing. Rows are hit-tested by `mouse::picker_row_at`
against the dialog rect C12's amendment routes in: item `i` is the single
row `rect.y + 1 + i`, inside the border columns; the border, the title and
any row past the last item hit nothing. The keyboard Enter and this click
share one launch path (`App::picker_launch`), so they can't diverge.

**[Amended 2026-07-27, SPEC-ux U20 — number accelerators]** The picker was
arrows-and-Enter only: two or three keystrokes to pick from a list you can
already see in full. Rows now lead with their accelerator —
selected `"❯ {n} {item}"`, unselected `"  {n} {item}"` (`picker_row_body`,
shared by both arms so they can differ only in style) — and `1`..`9`
launches that row through the same `picker_launch` the click and Enter use.
Items past the ninth get no digit (there is no tenth accelerator) but keep
the three columns, so the ids stay aligned. A digit past the end of the
list is ignored and the picker stays up: the rows carry their own numbers,
so an out-of-range press is self-evidently one and needs no flash. The
accelerator is also on the hint bar (`1..9 launch`) — an accelerator
nothing advertises is one nobody presses, which is what "unhinted `j/k`"
already cost this dialog.

**[Amended 2026-07-27, SPEC-ux U20 — type-ahead and the §7 cwd column]** The
picker becomes two columns and grows a filter. "Size and behavior unchanged"
is fully superseded.

- **Layout.** A fixed 16-column adapter column (the numbered rows, exactly
  as above) then, separated by a gap, a **recent working directory** column
  — DESIGN.md §7's long-promised "choose adapter + recent cwd". Rows are
  `max(adapters, cwds)` tall; the dialog's width is
  `16 + 2 + widest-cwd-label + 2`, floored at the pre-U20 **32** so a
  column of short labels can never make the picker *shrink* relative to the
  one it replaced (`picker_dialog_width`, shared by the drawn dialog and
  the mouse hitbox so §4/§5's lockstep holds).
- **Directory labels** are the last two path components (`src/roost`), not
  full paths: a fleet's directories are usually siblings, so the tail is
  what distinguishes them and the head is what would eat the dialog.
- **Marking, three states** (`row_marks`): the column holding the keyboard
  marks its selection with `❯` `ACCENT` + `FG` text — C14's existing idiom,
  untouched; the *other* column's selection keeps `FG` **without** the
  marker; everything else is `MUTED`. Two markers would claim two
  selections; dropping the inactive one entirely would hide which
  directory a launch is about to use. No bg highlight anywhere, as before.
- **Keys.** `↑`/`↓` steer the focused column, `←`/`→` (and Tab) hand the
  keyboard between them. Every other printable filters the adapter list by
  ASCII-case-insensitive **substring**, `Backspace` widens it back, and the
  live query replaces the title's tail (`new pane — clau▏`) so a narrowed
  list always says why it is narrow — a one-row picker with a normal title
  reads as a picker that lost its adapters.
- **Digits stay accelerators, never filter text.** No adapter id or path
  label needs a digit typed to reach it, and `1..9` is the fastest thing in
  the dialog; giving it up to type-ahead would trade the best key for the
  rarest. `1..9`, the click and Enter all index the **filtered** rows, so
  the mouse and the keyboard can never disagree about what row 2 is.
- **`j`/`k` stop being motions.** A list you filter by typing cannot
  reserve letters. Nothing advertised is lost — their being unhinted is
  the complaint U20 opened with — and the arrows the hint bar *does*
  advertise are untouched.
- **Hint bar** becomes `↑↓ choose` · `↵ open` · `1..9 launch` ·
  `type filter` · `←→ dir` · `Esc cancel` (71 columns, C9).
- **State ownership:** the recent-cwd list is session-only, on `App`, not in
  `workspace.json` — see SPEC-ux U20 for why (it reconstructs itself from
  the persisted pane cwds at startup, so persisting it would buy a schema
  migration almost nothing).

**[Amended 2026-07-27, theme inheritance]** `❯` `accent()`, selected item
`ink()`, unselected `quiet()`, title `ink()`, border `accent()`. C14's "**no
bg highlight**" was already the right instinct and is now the house rule (§2);
the two-column selection idiom is untouched.

**[Amended 2026-08-07, exit UX audit F8 — not-installed wording and the
adapter column's width]** Two things this contract never wrote down (both
lived only as `app.rs`/`render.rs` doc comments, P2-13) drifted stale
together:
- **`App::picker_filtered` annotates a not-installed adapter's row with a
  `"not found"` suffix, not `"gone"`.** "Gone" implies the adapter *was*
  reachable and disappeared; it never was, on this machine, so every fresh
  install would show the identical "gone" the moment after setup. "Not
  found" — the familiar shell idiom for "no such program on `$PATH`" —
  claims nothing about history.
- **The fixed adapter column (this contract's "16-column adapter column")
  is now 23, not 16.** It was sized for the registry's then-longest id
  (`claude`/`gemini`, 6 chars) and the old 5-char `"gone"` suffix. The
  registry has grown since (six adapters now, not five —
  `agents::adapter_specs`) and its longest id is now `opencode` (8 chars,
  longer than `claude`/`gemini`); the new suffix is 5 columns longer
  besides. `picker_dialog_width`'s formula (adapter column, plus a gap,
  plus the widest cwd label, plus another gap) is otherwise untouched —
  same 2-column gaps, same pre-U20 32-column floor.

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

**[Amended 2026-07-27, SPEC-ux U23 — the overlay stops being keys-only]:**
"pinned to the §8 key table" is superseded: the overlay is roost's one
explain-itself surface, and it explained only the chords. It now ends with
**three reference rows**, after `Alt+q` and in this order:

| key column | description |
|---|---|
| `status` | `⠋ working ◆ needs you ○ waiting · idle ✕ exited` |
| `mouse` | `wheel scrolls · click focuses · drag selects` |
| `Alt+click` | `open the URL under the pointer` |

- **Why a legend at all:** the glyph set (the Working spinner plus `◆○·✕`) is
  the product's core language (C5) — every badge, tab summary and feed line
  speaks it — and nothing anywhere said what the symbols mean.
  **[Amended 2026-08-07, C5]** the `status` row shows `SPINNER_FRAMES[0]`
  (`⠋`) as Working's representative frame — the legend is static text and
  cannot itself animate, so it prints the same steady frame the badge falls
  back to whenever animation is suppressed. The row's text is the C5 table
  itself, pinned against the `theme::GLYPH_*` constants by
  `help_legend_row_matches_the_theme_glyph_table`, so retheming a glyph
  breaks a test instead of quietly leaving the overlay teaching a symbol
  roost no longer draws.
- **Why mouse rows:** wheel/click/drag/Alt+click were implemented, live, and
  documented nowhere a user could reach. Alt+click gets its own row because
  it is a *chord* — it belongs in a key column, not buried in prose.
- **The ≤ 20 cap is unchanged and still binding.** The three rows are paid
  for by merging three natural chord pairs into one row each — the same
  `Alt+t / Alt+1..9` idiom the cap has always demanded: `Alt+s / Alt+o`
  (split ops), `Alt+z / Alt+f` (the two view toggles), `Alt+w / Alt+u`
  (close and its undo). 20 rows before, 20 rows after.
  **[2026-09-03: the float half of this pair re-keyed from `Alt+f` to
  `Alt+Shift+z` — see the dated amendment below. "The two view toggles"
  is exactly why that destination was chosen: this line is what made the
  pairing structural instead of coincidental.]**
- **Rejected: scrolling the overlay** (the U23 proposal's other option). It
  would have bought unlimited rows at the cost of C15's "any key closes it"
  — the dismiss rule would have to carve out arrow/PgUp/PgDn keys, so the
  one modal you open when you are lost becomes the one modal with a
  non-obvious way out. Merging keeps the overlay a single glance.
- **New width rule (the merge's cost, made explicit):** the reference rows
  are long, so `help_dialog_width(HELP_KEYS)` must stay **≤ 80** — the
  80-col floor — or `centered_near` clamps and clips a description mid-word,
  the exact failure the 2026-07-22 width amendment exists to prevent.
  Pinned by `help_dialog_fits_the_eighty_column_floor`.

**[Amended 2026-07-27, SPEC-ux U16/U20 — two descriptions retuned, no rows
added]:** `Alt+Enter`'s row becomes
`picker: 1..9 launch · type filters · ←→ recent cwd` (71 columns with its
key prefix) — the picker grew two ways of driving it and the overlay is the
only surface that says so. The rename row is unchanged: `←→`/`Home`/`End`
inside a text field is what a text field *is*, and spending overlay width to
say "the arrow keys move the cursor" would teach nothing. Row count stays
**20**.

**[Amended 2026-07-27, SPEC-parity P21 — search adds no row]:** the
`Alt+c / Alt+PgUp` row's description tightens from
`copy mode (hjkl w b e 0 $ v V y o) / scroll mode` to
`copy (hjkl wbe 0$ vV y o) / scroll — / search, n/N` (71 columns with its
key prefix, inside the ≤80 floor). The row count stays **20**, which is what
the cap has always demanded of a new mode-local key: absorb it into the row
that already owns its mode. `/` and `n`/`N` are not Alt chords and so are
not §8 table rows, but the overlay is the one surface that has to know they
exist — a search nothing advertises is a search nobody finds, which is
precisely the state P21 catalogued.

**[Amended 2026-07-27, theme inheritance]** Key column `accent()`,
description column `quiet()` — the same key/label system as the hint bar, as
before. Content, width rule and the ≤20-row cap are untouched.

**[Amended 2026-07-28, C27 — the roster costs no row]:** `Alt+Shift+a` joins
C19's own row rather than taking a 21st: `Alt+a / Alt+Shift+a` →
`jump to next pane that needs you / list every pane` (70 columns with its key
prefix, inside the ≤80 floor). This is the merge idiom the cap has always
demanded of a new chord — and here it is also the honest presentation, since
the two chords are one deliberate pair (C27's binding rationale). Row count
stays **20**; `help_keys_match_the_c8_key_table_verbatim_and_in_order` and
`help_dialog_fits_the_eighty_column_floor` pin both halves.

**[Amended 2026-07-28, C28 — the ≤20-row cap is retired; the keymap is
grouped, columned and scrollable.]**

Every amendment above this one paid for new content the same way: **merge two
chords onto one row.** Read the trail — U7 merged `Alt+c`/`Alt+PgUp` and
`Alt+/`/`Alt+?`; U23 merged `Alt+s`/`Alt+o`, `Alt+z`/`Alt+f` and
`Alt+w`/`Alt+u`; C27 merged `Alt+a`/`Alt+Shift+a`. Six merges in eight days,
each recorded as "the idiom the cap has always demanded". C28's two chords
would have made it seven, on a row already holding two. That is not an idiom
any more; it is a budget, and the budget was distorting the artifact — the
one surface whose entire job is explaining roost had started explaining it
in the fewest rows rather than the clearest.

So the cap goes, and with it the merges:

- **Grouped, not one flat list.** `HELP_GROUPS` — `PANES`, `LAYOUT`, `TABS`,
  `FLEET`, `READING`, `SESSION`, `READING THE SCREEN` — in "how often you
  reach for it" order, each heading in C6's idiom (uppercase, `quiet()`,
  underlined across its column), exactly as C27's roster stacks its groups.
  No blank row between groups: the underline *is* the separator, and six
  blanks would be six rows of a table that already scrolls at the floor.
  Grouping is also what makes an unmerged list readable — 26 undifferentiated
  chord rows is a worse artifact than 20 merged ones; 26 in seven named
  blocks is a better one than either.
- **The fewest columns that fit** (`help_layout`). One column while the list
  fits the body's height — the calm form, and the only form at the 80-col
  floor. A second column only when one would overflow *and* the terminal is
  wide enough for two, split at the group boundary nearest the halfway mark
  so a heading is never sawn off from its chords. Never columns for their
  own sake.
- **Scrolling, when even that is not enough** — the option U23 explicitly
  rejected, taken now because its stated cost has been paid down. U23's
  objection was that carving arrows out of "any key closes it" makes the
  modal you open when you are lost the one with a non-obvious way out. Two
  things answer that. First, the carve-out is **conditional on there being
  something to scroll to**: `help_scroll` returns whether it moved, and a key
  that did not move the window falls straight through to the dismissal — so
  on every terminal that shows the whole table (which two columns now make
  the common case) `↓` closes the overlay exactly as it always did, and the
  amendment is invisible. Second, when it *is* scrolled the overlay says so
  in its own title (`keys — 26/36 · ↑↓ more · any key closes`) and the C9
  hint bar switches to `↑↓ PgUp/Dn read on` · `any other key close`. The way
  out is more visible while scrolled than it was before, not less.
  `↑`/`↓`/`j`/`k` step, `PgUp`/`PgDn` page by half the visible height
  (C20's `overlay_page` rule), `Home`/`End` jump; the wheel pages, and any
  **click** still dismisses (U8's mouse form of the same rule).
  **[Amended 2026-09-01, C39 typing amendment]** `j`/`k` leave the step
  set — every bare printable is filter text now (it opens the query,
  seeded), so the arrows and paging keys carry the un-filtered scroll
  alone, and the title wordings quoted above read
  `type to filter · Esc closes` in place of `/ filters · any key closes`.
  **`Space` is deliberately not a page key** — in a modal whose contract is
  "any key closes it", `Space` is what a reader hits to make it go away, and
  answering with another screenful is the opposite of the ask.
- **Width rule, unchanged in substance:** one *column* must stay ≤ 80 or
  `centered_near` clamps and clips a description mid-word. Pinned by
  `one_help_column_fits_the_eighty_column_floor`.
  **[Clarified 2026-08-19, C37 audit]** That test measures the **dialog rect**
  (`help_layout(..).size.0` = column + 3 for border and padding), not the
  column, so it is stricter than this bullet's wording: a column of 80 is
  already a dialog of 83 and clamps at an 80-column terminal. The test is the
  operative check; read this bullet as "the dialog must fit the floor", which
  is what it was always defending.
- **What replaces the cap as the binding check:**
  `every_bound_chord_is_documented_in_the_keymap` — every chord roost binds
  appears in the overlay. That assertion was *impossible* to write under the
  cap, where a new chord's options were "merge a row" or "go undocumented".
  It is the check that actually matters, and it only became available by
  removing the constraint that made it unsatisfiable.

**[Amended 2026-08-20, F9 — the keymap filters]** `/` opens a type-ahead
filter over the table, and while a query is open "any key closes it" no
longer holds: printables type. That is a **larger** carve-out than the
scroll amendment above, which is conditional and hands its keys back the
moment they have nothing to do — the rules, the reason C27's roster rather
than C15's own scrolling licenses it, and the four title/hint-bar wordings
that announce it are contracted in **C39**. Everything on this page holds
unchanged while the filter is closed, which is the default and, for a reader
who never presses `/`, the only state.

**[Amended 2026-08-06, PR #46 design audit (D2) — the mouse row grows]**
C29's three new gestures had no discoverability surface at all:
`every_bound_chord_is_documented_in_the_keymap` only ever walks Alt chords,
so it passed **vacuously** over double-click, triple-click and
shift-click — none of them binds a chord, so the check that exists to
catch an undocumented one structurally cannot see them. This is the same
gap U23 closed for the wheel/click/drag/Alt+click verbs, applied to the
verbs C29 added on top of them, and it resolves the §8 key table's own
loose end from that amendment (a note there, now withdrawn in favour of
this one — mouse verbs belong in this contract's `READING THE SCREEN`
group per U23's own precedent, not in a table whose subject is chords).

- **Content.** The `mouse` row (`READING THE SCREEN`) widens from
  `wheel scrolls · click focuses · drag selects` to
  `wheel scrolls · click focuses · drag/2x/3x/shift selects` — 75 columns
  with its key prefix, still inside the 80-col floor (`Alt+c`'s row, at
  71, was the previous widest; this becomes the new widest, and the
  dialog is 3 columns of border/padding narrower than that, `74` → `78`).
  Terse by the row's own convention: the original never said "copies"
  either, and the full account is C29, not a legend line.
  **Why Shift+click earns a place and a miss doesn't get one of its
  own:** this contract's own rule for the sibling row — "`Alt+click` gets
  its own row because it is a *chord*" — applies verbatim to Shift+click,
  which is why it is spelled out rather than folded wordlessly into
  "drag selects" the way a plain click already is.
- **No new glyph, no new colour** (§2's inventory governs symbols; `2x`/
  `3x` are ASCII text in the row's own style, the same class of thing as
  the C2 tab-count cell's digit).
- **Test:** `help_rows_document_every_mouse_verb` (amended) now asserts
  `2x`/`3x`/`shift` are present, alongside the pre-existing `mouse`/
  `wheel`/`click`/`drag`/`Alt+click`/`URL` — the check that actually has
  to know the three new gestures exist, since the chord-table check
  cannot.

**[Amended 2026-08-06, PR #42 spec audit — the CONTROL CLI group]:**
`HELP_GROUPS` contains eight groups in the code; DESIGN-ui.md listed only
seven. The eighth is `CONTROL CLI` (`render.rs:700–713`), which closes the
table with the scriptable fleet verbs (`roost
send`/`read`/`status`/`spawn`/`wait`/`fork`/`close`). It was added per ux
P2-15 — the pane id appeared on badges specifically as the join key between
what's on screen and what a caller types, but that key went unnamed in the
UI until now. Justification: it sorts last for the same reason `READING THE
SCREEN` does (it teaches the product rather than a binding), and follows
rather than leading (a user opens `Alt+?` to remember a chord first, learns
the fleet is scriptable second). Covered by the existing width rule and
`every_bound_chord_is_documented_in_the_keymap` gate (as a non-Alt surface
it poses no new bindings).

**[Amended 2026-08-20, floor stress — the key column must end in a space]**
`draw_help_columns` draws a row as **two adjacent spans**, the key prefix and
the description, with nothing between them: all the separation there is comes
from the prefix's own padding. That padding was `format!(" {key:<18}")` — a
*minimum* width, not a column. A key 18 cells or wider gets padded by nothing,
and the description fuses straight onto it.

It was already visible on the **default keymap**, in the group this contract's
own PR #42 amendment added — and on two rows, not one: `roost send <id>
"text"` (22) and `roost spawn ADAPTER` (19), rendering `…"text"type into that
pane` and `…ADAPTERlaunch a new pane`. C34 then made the key column keymap-derived,
so an enumerated family reaches 23 and `Alt+← / Alt+↓ / Alt+j …move focus (…)`
joined them. None of these is a *clipping* failure — the row fits — which is
why every width test passed over all three.

**Rule:** a key column always ends in at least one space. Pad to the
18-column grid when the key fits it, otherwise exactly one space. Rows under
18 render byte-identically to before; only the ones that were unreadable move.
Pinned by `a_key_column_never_fuses_to_its_description`, which asserts the
*separator*, not the width — width was never what broke.

**And the column it costs had to come from somewhere.** `HELP_COL_FLOOR` — the
ceiling `elide_key` cuts against — read `80` from the start and was three
columns too loose: `help_layout` asks for `content + 3` (two borders plus the
column of air before the right one), so a column of 78 is a dialog of 81,
clamped to the terminal with the air going first. It never bit only because
the widest content the table could produce was exactly 77, the ceiling the
constant should have named. Adding a column took it to 78 and, at 80×24 with
`alt+h` disabled, `… move focus (…at an edge)` sat flush against the border.
The constant is now **derived** — `HELP_FLOOR_COLS − HELP_DIALOG_CHROME` = 77
— because the derivation is the part that was wrong, and `help_layout` uses
the same `HELP_DIALOG_CHROME` rather than a bare `+ 3`.

Both floor tests were rewritten in the same pass, because neither could have
caught this: they asserted `layout.size.0 <= 80`, and `size.0` is
`.min(body.width)`, so at an 80-column body it reports 80 whether the layout
fit or was clamped down to it. **A tautology that reads like a gate is worse
than no gate** — it makes the check look done. They now assert the *ask*
(`content + HELP_DIALOG_CHROME`) against the floor.

Found by the design audit of the padding fix, which measured the rendered row
rather than trusting the arithmetic; the padding bug itself was found by a
simulation agent stressing the design floor. Neither found the other's half.

### C30 — Sub-two-row floor notice — [Added 2026-08-06]

**Current:** `render.rs:24–29` (`draw_too_small`) — when `area.height < 2`,
draw the message "too small — resize" and return, pre-empting all other
chrome.

**Target:**
- **Trigger:** iff the top-level `area.height < 2` — no room for even tab bar
  + one-row body minimum (§4's sizing contracts all refer to the body area
  after the tab bar consumes one row; the floor is therefore `area.height ≥
  2`). The sub-two case is not a resize-target; it is a "you're too tight"
  notice: terminals at 80×1 or a split down to one row mid-transaction
  briefly, or a constraint roost inherited from its launcher.
- **Rendering:** one line, left-aligned (plain `Paragraph`, no alignment
  set), `too small — resize` in `ink()`, no
  background, no glyph. `Paragraph` clips long content at the area boundary
  rather than wrapping — the message is terse enough to fit the 80-column
  floor by construction (`"too small — resize"` is 18 chars; an 80×1 buffer
  has 80 cells, plenty). Zero-sized area draws nothing (the `return` at
  `render.rs:93–94`).
- **Precedence:** this is the **first and only thing `draw()` checks**
  (`render.rs:24–30`). It pre-empts tab bar, panes, modals, floating scratch,
  hints — everything. The user sees the notice immediately; nothing else
  paints.
- **Unit test:** the §2 mechanical gates include the sub-two-row case. The
  fixture (`chrome_buffers`) receives an 80×1 snapshot; `chrome_paints_no_background_fill`
  and `every_chrome_word_is_drawn_in_ink_the_user_already_reads` audit it.
  A 0-height area test is implicitly covered by the `if area.height == 0`
  guard at `:92`.

### C16 — Dead-pane overlay

**Current:** `render.rs:387–403` — error line Red fg; action bar Black on Red.

**Target:**
- spawn-error line: `" spawn failed: {err} "` fg `ACCENT`, no bg.
- action bar (bottom row, full inner width): text unchanged
  (`" ✕ exited — Enter: relaunch/resume · f: fresh (drops resume) · Alt+w: close "`),
  fg `FG` on bg `ACCENT_DIM`.
- Placement (bottom rows over preserved last screen) unchanged.

**[Amended 2026-08-09, copy resume]** A pane with a session pointer
(`App::resume_command_line` is Some) inserts `· y: copy resume` before
`· Alt+w: close`; `y` copies the pasteable `cd <cwd> && <resume command>`
line and flashes the U14 copy outcome. Session-less panes (shells, agents
dead before detection) keep the bar unchanged — `y` would copy nothing
there, and both this bar and the C9 hint bar gate on the same predicate so
they can't disagree.

**[Amended 2026-08-19, C34 — the close chord is resolved]** The bar's
`· Alt+w: close` is **derived from the live keymap**, not printed from the
text quoted above; an unbound close drops the clause entirely rather than
naming a key that does nothing. The quotation stays accurate as the default
rendering.

Why this bar specifically, beyond C34's general rule: the C9 hint bar sits
**one screen row below it** and already derived the same chord, so before
this a remap made two surfaces a single glance apart disagree about which key
closes the pane. `Enter`, `f` and `y` stay literal — mode-local keys
config.json cannot reach (C34's stated exemption), and the same predicate
still gates `y` on both bars so they cannot disagree about *that* either.

**[Amended 2026-07-27, theme inheritance]** The spawn-error line stays
`accent()` on no bg. The action bar becomes `attention_problem()` instead
of `FG` on an `ACCENT_DIM` fill — same pairing with C11 as before, same
placement over the preserved last screen. The style rides on the **widget**,
so the reversal covers the bar's whole inner width: styling the span alone
reversed only the columns the message occupied and left the dead program's
last output at normal video across the rest of the row (the supervisor's D1).
C17's neighbouring rule is worth naming here: this bar is chrome painted *over* program output, and `REVERSED`
composes with whatever the dead pane last drew rather than assuming a
background.

### C17 — Copy-mode selection

**Current:** `render.rs:365–368` + `highlight_selection()` `:428–446` —
`Modifier::REVERSED` per cell.

**Target:** unchanged, and **must stay modifier-based**: selection sits on top
of arbitrary program colors, so it may not assume any palette token. Contract
exists to stop a well-meaning restyle from "theming" it. (The keyboard copy
cursor, C24, extends this rule — modifiers only.)

**[Amended 2026-07-27, SPEC-parity P21 — search hits are a third tenant of
this rule]:** a running scrollback search paints every visible hit
`REVERSED`, and the hit the view is parked on additionally `UNDERLINED` —
the same two-modifier vocabulary C24 already uses to keep the copy cursor
distinguishable inside a reversed selection, for the same reason (hits land
on arbitrary program colors; a palette token here is a DEVIATED). Hits are
painted **before** the selection pass, so selecting over a hit still reads
as selected — the selection is the thing you are about to act on.
Positioning is `line − (banked − offset)`, both numbers read from the grid,
so a hit scrolled off the top of the view paints nothing rather than
smearing onto row 0.

### C18 — vt100 blit guard

**Current:** `render.rs:450–499` (`blit_screen`, `conv_color`, `cell_style`).

**Target:** **zero diffs** in these functions for this engagement. Program
output keeps its own colors, attributes, and default-bg passthrough. Any
change here is an automatic DEVIATED. [Re-affirmed 2026-07-22 for the fleet
build: the feed, float, zoom, raw, and copy-cursor features all draw *around*
the blit, never inside it.]

**Amended 2026-07-27 (SPEC-ux U24, SPEC-parity P16) — the rule is restated,
not relaxed.** The zero-diff rule existed to stop a *restyle* from reaching
into the blit: the chrome may not theme program output. That intent is
unchanged and still binding. What the rule cannot forbid is fixing the blit's
own fidelity to the program, which is the opposite failure — output the
program emitted and roost did not reproduce. Two such changes land here:

- `blit_screen` leaves a wide glyph's continuation cell at the buffer's reset
  default rather than stamping `" "` plus a style over it (U24). Nothing in
  roost may write a symbol into a cell another glyph already spans; a wide
  glyph clipped by the drawn area's edge degrades to a space rather than
  emitting a two-column symbol that would suppress the border's own cell.
- `cell_style` maps SGR 2 and 9 to `Modifier::DIM` and `CROSSED_OUT` (P16).
  These were dropped end to end, so dimmed secondary text rendered at full
  weight — the pane asserting emphasis the program never asked for.

`conv_color` is **unchanged**, and the colour rule is untouched: no palette
token, no theme lookup, no default-bg substitution may appear in any of the
three. The revised predicate for an audit is *"does the blit add anything the
program did not emit?"* — the answer must stay no. Adding a modifier the
program **did** emit is fidelity; C1's theme gate and the no-BOLD rule
continue to apply to chrome only and are unaffected by either change.

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

**[Amended 2026-08-07, ux P1-5 — the Waiting fallback]** "Nothing else is in
the ring — not Exited, not Working: worst-first across roost's statuses means
◆ is the only actionable severity, and the ring must match the advertised
count" is withdrawn as an absolute: a ◆-only ring keeps `Alt+a`'s "one key
jumps there" promise only for extension-instrumented panes, since without a
hook `NeedsInput` is reachable solely via the BEL heuristic and a quiet
turn-end lands on `Waiting` instead — which the ring used to ignore outright,
leaving an uninstrumented agent's finished turn unreachable by the chord built
to reach it.

- **The ring now has two passes.** Pass one is unchanged (◆, the predicate
  above, verbatim). **Only when pass one is empty** does a second pass run,
  over every pane whose status is `Waiting` — same shape (tab order, that
  tab's `pane_order()`, the float last). An instrumented fleet is untouched
  by construction: the fallback is reachable only once the ◆ pass comes back
  empty, so a real ◆ is never skipped for a ○, and the ring/count invariant
  above still holds exactly whenever N > 0. It no longer holds when N = 0 and
  the fallback ring is non-empty — deliberately: the hint bar's `◆ N needs
  you` segment is already omitted at N = 0 (C9), so nothing visible asserts
  the old equality in that case.
- **The predicate is `display_status`, not the raw runtime status** (P2-10):
  a plain shell reading heuristic `Waiting` is presented as `Idle` (it has no
  turn to hand back), so it can never pull either ring pass. Only a pane
  whose `Waiting` means something — a real agent — does. The roster (C27)
  reads the identical two-pass ring for its opening cursor, so the two
  surfaces cannot disagree about what a fleet with zero ◆s should jump to.
- Unit tests added: fallback fires only with an empty ◆ pass; a real ◆
  anywhere suppresses it outright; the fallback excludes a quiet shell (and
  the scratch float, which is always a shell); the roster/ring agreement
  test below covers the three-way interaction.
- **Known gap, accepted rather than closed:** at N = 0 the hint bar's
  `◆ N needs you · Alt+a` segment is omitted (C9), so the fallback this
  amendment adds has *no* advertised surface — the one case it exists for
  is the one where nothing on screen says `Alt+a` still does anything. This
  is the same trade-off the original C9 amendment already made for `Alt+a`
  generally (no Normal-mode hint pair at any N), just visible at a new edge:
  a user who has never pressed `Alt+a` while N > 0 has no way to discover
  the fallback exists. Left alone rather than inventing a new hint-bar
  surface in an audit-response pass; a Normal-mode pair (dropping the
  N > 0-only rule) is the fix if this needs to be discoverable cold.

**[Amended 2026-08-07, exit UX audit F2 — the gap above is closed]** The
right segment itself now carries the fallback, rather than growing a
Normal-mode pair: `hint_bar_right_spans` takes `App::attention_segment()` —
`Some((n, true))` for the real ◆ count (unchanged text/style), `Some((n,
false))` rendering `"○ {n} your turn · Alt+a"` in `ink()` when the ◆ pass is
empty but the Waiting pass isn't, `None` when `Alt+a` has nothing to do at
all (C9 amended alongside). This was chosen over the Normal-mode pair the
prior amendment named as the alternative: a static pair would cost columns
at every N (colliding with C9's "the Normal-mode seven gain nothing"
100-column budget) and can't say *how many* panes are waiting the way the
existing aggregate slot already does for free. The segment now matches
`attention_ring` in every case, not only N > 0 — closing this gap and
making the ring/count invariant this contract opened with fully true again,
not just true when N > 0.

**[Amended 2026-08-07, exit UX audit F3 — the Waiting fallback now empties]**
The two-pass ring above never removed a pane from its fallback pass once
that pane had been visited: a real ◆ clears itself the moment you act on it
(its own status moves on), but a quiet `Waiting` pane's status doesn't
change just because focus landed on it, so `Alt+a` over an all-Waiting fleet
round-robinned forever and "nothing else needs you" (the empty-ring flash)
only ever fired with exactly one pane in the fallback. Bit hardest at the
fleet size this feature exists for.
- **Fix: a visited-set, cleared per-pane the moment that pane leaves
  `Waiting`.** `App::visited_waiting: HashSet<PaneId>` — `set_focus` (P10's
  single funnel for every focus move: `Alt+a`, a click, arrow-focus, the
  roster) adds a pane the instant focus lands on it while its
  `display_status` is `Waiting`; `attention_ring`'s fallback pass filters
  the id out. Chosen over "clear the whole set whenever the fleet changes"
  (the mechanism this amendment's originating finding suggested as one
  option): a spawn/close elsewhere in the fleet has nothing to do with
  whether *this* pane's finished turn was already seen, and clearing
  per-pane on close would still leave a revisit-without-a-new-turn
  underspecified — the real invariant needed is per-pane, not fleet-wide.
- **Cleared on the transition that matters, not a timer.** `diff_statuses`
  (already the one place that diffs every pane's status once per tick, C20)
  removes a pane from `visited_waiting` the instant its status is observed
  leaving `Waiting` — so a pane that starts a new turn and finishes it again
  is never silently swallowed by a mark the *previous* turn left behind.
  Pruned on pane close alongside `last_status`, same cadence, same reason.
  The ◆ pass is entirely untouched — a real ◆ can never be hidden by a
  stale visit.
- Unit tests added: the fallback ring shrinks by one with each visit and
  the fourth press over a three-pane all-Waiting fleet flashes `nothing
  needs you` rather than wrapping (`jump_attention_waiting_fallback_shrinks_as_each_pane_is_visited`);
  a plain spatial focus move retires a pane exactly as a jump does
  (`any_focus_move_not_just_alt_a_retires_a_visited_waiting_pane`); a pane
  that leaves and re-enters `Waiting` is eligible again, not stuck excluded
  (`a_revisited_pane_re_enters_the_fallback_after_a_fresh_finished_turn`).

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
  | `status` | the 2 s housekeeping tick (`app.rs:367–401`): App keeps a last-known-status map for spawned panes and diffs it *before* the `pending_detect` early-return. One source for all transitions — extension-pushed and heuristic alike — at ≤ 2 s granularity (documented; a sub-2 s flicker may be missed, accepted). Transitions **to Exited are suppressed** here (the `exit` hook owns that). First observation of a pane logs nothing (`spawn` owns birth). | `{id} {name}: {old} → {new}` using the C8 state words. **[Amended 2026-08-08, needs-input question]:** a transition landing on NeedsInput whose report carried the ask's question appends it: `… → needs you — {question}` (sanitized socket text; the notification says the same via `{name} asks: {question}`) |
  | `spawn` | `spawn_pane` success (`app.rs:349–358`) — covers Alt+n, picker, control spawn/fork, undo restore, respawn | `spawned {id} {name} ({adapter})`, the suffix for titled panes only |
  | `close` | `close_pane_id` (`app.rs:980–1028`) and `undo_close` (`app.rs:1309–1333`) | `closed {id} {name}` / `closed tab {name}` / `reopened {id} {name}` / `reopened tab {name}` |
  | `exit` | `on_pty_exit` (`app.rs:1058–1072`), including the focused pane (unlike the notifier) | `{id} {name} exited` |
  | `ctl` | `audit()` (`app.rs:675–695`, becomes `&mut self`; feed push happens even when there is no socket dir) — the same sanitized `method_summary` as `control.log`, so broadcast and every other control verb land here with zero extra code | `ctl {principal}: {summary} → {ok\|err}` |

  **[Amended 2026-07-27, SPEC-ux U2]:** pane-referencing lines lead with the
  pane id (`{id} {name}` via `App::feed_label`; `name` is the shared
  `display_name_live`) — live QA showed four identical `shell: working → your
  turn` lines with no way to tell the panes apart. Tab lines keep the tab
  name; a pane whose spec is already gone degrades to `pane {id}`. The
  spawn line's `({adapter})` suffix is titled-only (C4's no-dup rule — an
  untitled display name already ends in the adapter/cwd tag).

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

**[Amended 2026-07-27, SPEC-ux U25 — entries are actionable]:** the feed
told you what happened and then left you to go find it by hand, which for a
fleet surface is half a feature.
- **`FeedEntry` carries `pane: Option<PaneId>`** — the pane the line is
  *about*. `Some` for `spawn`, `status` and `exit` (a dead pane is exactly
  where you want to land); **`None`** for `close`/`reopened tab`/`ctl`: a
  closed pane is gone and `Alt+u` is that line's recovery path, and a `ctl`
  line is about a request, not a pane. The id is a jump target, not a
  guarantee — ids are recycled (`next_pane_id` is a max+1), so Enter
  re-checks the pane exists before moving.
- **`Enter` focuses that pane** through `focus_attention_target` — the same
  helper C19's ring uses — so a jump out of the feed switches tabs, expands
  collapsed stacks and shows the float exactly like `Alt+a` does, and the
  feed closes behind it. A line with no pane flashes `no pane on that
  line`; a stale id flashes `that pane is gone`. Neither moves focus and
  neither closes the overlay: a no-op you can see beats a silent one.
- **The selected entry is the window's last row** — `offset` already means
  "how many entries back from the newest", and `feed_window` ends at
  `len − 1 − offset`, so the cursor the arrows move *is* the bottom visible
  line. It is now marked: the row's leading column carries `❯` fg `ACCENT`
  instead of a space (the C14 picker idiom, reused). Zero columns spent —
  the leading space was already in the row rule — so `" HH:MM:SS  {text}"`
  is otherwise untouched, `◆ ` prefix included.
- Hint pairs per the C9 amendment; the arrows' label becomes `select`,
  since with an actionable entry they move a cursor rather than a view.

**[Amended 2026-07-27, theme inheritance]** Feed styling in current tokens:
frame `accent()` border with an `ink()` title over a `DIM` backdrop;
timestamps and ordinary text `quiet()`; a NeedsInput line's text `ink()` with
an `accent()` `◆ ` prefix; the U25 selection marker `❯` `accent()`; the empty
state `quiet()`. No fills anywhere in it.


**[Amended 2026-08-20, floor stress — the overlay always has a frame]**
`feed_overlay_size` subtracts four from the body in each axis, and nothing
floored the result: a body four rows tall gave a **zero-height dialog**,
which draws nothing — no border, no title, no rows. The body is two rows
short of the terminal (tab bar and hint bar), so any terminal six rows or
shorter was affected. `Alt+e` and C27's `Alt+Shift+a` still flipped the mode
word and still offered `↑↓ select` in the hint bar, so the whole visible
effect of the chord was one word changing: indistinguishable from a binding
that does not work, at exactly the sizes where a user is least sure what
they are looking at.

C14's picker had answered this already — "an empty result still needs a
frame to say so" — and these two never adopted it. Both axes are now floored
at one cell. `centered_near` clamps the ask to the body, so the floor can
never overflow a body smaller than the frame it asks for; a one-row dialog
is its top border and title, which is enough to say *something opened*.

Knowingly residual: at that size the hint bar still offers `↑↓ select` and
`PgUp/Dn page` over an overlay with zero selectable rows. The floor's job is
to prove the chord worked, not to make a two-row terminal a usable roster,
and pruning the pairs would be C9's arithmetic rather than this one's — but
it is the next thing to look at if the sub-eight-row case ever matters.
Pinned by `the_fleet_overlays_always_have_a_frame_to_say_so_with` (geometry,
every body size from 1×1 to 11×11) and `tests/small_terminal_overlays.rs` (a
real roost at 40×8, 40×6, 40×5 and 40×4, looking for the overlay's title
rather than a border glyph — the panes draw borders too, and a first draft
that counted corners passed with the bug present because the *panes'*
corners arrived between its two samples).

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
  Alt+n, picker launch, Alt+s, Alt+o, Alt+Shift+arrows, Alt+g,
  Alt+Shift+g, Alt+Shift+hjkl — which exit zoom *first, then apply*, so the
  layout never changes invisibly · the
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

**[Amended 2026-08-19, C33 and C37]** The structural-action list above gains
`Alt+Shift+hjkl`, and `Alt+Shift+g` on the same date for the same reason —
C37's reverse cycle is as structural as C25's forward one, and this list is
*enumerated by name*, so an omission makes it false rather than vague. Both
were found by a design audit, which is the check that exists for exactly this. Recorded rather than left implied: the list is labelled
*exhaustive*, so a new structural action that exits zoom without appearing
here does not leave the contract vague — it makes it false. (Found by the
design-supervisor audit of C33, which is the check that exists to catch
exactly this.) C22 rule 3's sibling list takes the same addition on the same
date.

**[Amended 2026-09-01, the map re-key]** The list is re-spelled to the map
that now exists, on this section's own "enumerated by name ⇒ false when
stale" rule: **`Alt+Shift+s`** joins it (explode is as structural as the
collapse half it split off from), and the resize entry is the punctuation
family **`Alt+- = < >`** rather than `Alt+Shift+arrows` — the shifted arrows
still exit zoom, but now as the *move-the-pane* family alongside
`Alt+Shift+hjkl`, and they do it from inside `move_pane_dir` (C38) rather
than from `apply`'s guard. The guard's own arms are the ground truth for
all but one entry: `NewPane · StackPane · ExplodeStack · FlipSplit · Resize ·
CycleLayout`. **Picker launch is the exception** and always was — it exits
zoom and hides the float from its own call site (`App::launch`, "picker
launch is a structural action"), because *opening* the picker must not, and
only the launch step is structural. It belongs on this list; it is simply not
reachable by reading the guard.

**[Amended 2026-07-27, SPEC-parity P5 — the round trip is lossless]** Zoom
resizing the pane's PTY both ways is only safe because the resize itself now
preserves the grid: the vendored parser **reflows the live grid** on a size
change, rebuilding the logical lines from the rows' wrap flags and laying
them out again at the new width (narrowing wraps, widening rejoins,
attributes and wide glyphs carried whole). Before, unzooming hard-truncated
every column past the tiled width — a 110-column line printed at 118 lost its
tail permanently, and pre-zoom wrapped lines never rejoined. Two limits are
deliberate and contracted:
- **Scrollback keeps its historical width.** Only the live grid rewraps;
  rows already banked stay as they were banked, and rows a narrowing pushes
  off the top are banked at the new width (so history can be mixed-width
  after a round-trip). Same veneer-vs-second-state split C4's P1 snapshot
  draws — it buys the lossless round-trip without a full history rewrap. A
  narrowing spends the screen's own blank rows first, so the common case
  (a shell with room below the prompt) banks nothing at all.
- **The alternate screen is never reflowed**, nor is a grid with a scroll
  region set: those applications own their canvas and repaint on SIGWINCH, so
  a rewrap would only fight the redraw already on its way.
"No reflow churn while reading" above is unchanged and means what it always
did — hidden panes are not resized at all while zoomed, so nothing rewraps
behind the zoomed view. C4's `↑N` token and C9's `↑N/M` stay honest across a
resize: the offset is re-read from the grid's own clamp afterwards, never
carried over.

**[Amended 2026-08-07, client request — cross-tab arrow-key focus, C31]**
C31 adds a new way to change tabs: `Alt+Right`/`Alt+Left` off a tab's
geometric edge. It follows the rule above exactly rather than becoming an
exception to it — **quoted**, "any tab change (Alt+t, Alt+1..9, tab-bar
click, cross-tab Alt+a)" now also reads *cross-tab Alt+arrows*: every one of
them exits zoom, no new case. The one line worth narrowing is **"Keeps
zoom: focus moves"** above, written before a focus move could also be a tab
change — read it as *same-tab* focus moves only; a cross-tab edge jump is
the tab-change branch, not this one. This is the same split C19 already
draws for `Alt+a` ("a same-tab jump keeps zoom...; a cross-tab jump exits
zoom (tab-switch rule)") — C31 follows that precedent rather than inventing
a new one. Nothing else in this contract changes; see C31 for the feature.

**[Amended 2026-08-11, zoom indicator]** "No border/badge change" above is
narrowed: the zoomed pane's top border now carries a right-aligned title,
`ZOOM · {n} hidden` where `n` = the real tree's pane count (`rects()`) minus
one, or bare `ZOOM` when `n == 0`. Styled to match the border it sits on
(accent() focused, rule() unfocused — under a focused float), never a fixed
color of its own. Width sheds in two steps, same
unicode-width measuring as C4's badge: the `· {n} hidden` clause drops
first, then the whole title — the identity badge (a separate row, C4)
always keeps its space. No new hitboxes: a border title is cosmetic only.

### C22 — Floating scratch pane (Alt+Shift+z) — [Added 2026-07-22, fleet features; re-keyed 2026-09-03]

**Current:** no floating anything. All panes live in a tab's layout tree;
`hit_test` scans tiled rects in order (`mouse.rs:37–47`); pane ids are
allocated by scanning the tabs (`workspace.rs:57–65`).

**Target:**
- New `Action::ToggleFloat`, bound to **Alt+f**. Mnemonic: *float* — zellij's
  own Alt+f. (Collision note, flagged per brief: Alt+f is readline
  forward-word; roost already swallows it today (`input.rs:72–77`), and raw
  mode (C23) is the remedy for panes that need it back.)
  **[Re-keyed 2026-09-03]** — the collision note above turned out to
  understate the cost: on a terminal without the kitty keyboard protocol,
  Alt+Right is delivered as the *same bytes* as Alt+f (`ESC f`, the
  meta-ESC pair), so pressing Alt+Right opened the float. Raw mode is a
  per-pane opt-in and cannot fix a global chord, and the collision is
  unfixable at the byte level — the only fix is to stop binding `Alt+f`.
  `Action::ToggleFloat` now binds **Alt+Shift+z**, paired onto `Alt+z`
  (`Action::ToggleZoom`) as the two view toggles this section's own U23
  amendment already named them (§8); `Alt+f` is unbound by default and
  forwards to the pane (U5), completing the `b`/`d`/`f` readline
  word-motion trio §8 leaves free. See the dated amendment on
  `default_chord_action` in `src/ui/input.rs` and the §8 amendment below
  for the full reasoning.
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
  (C21) → **float** → C10's body-row flash, when it is drawn there → C12
  modal overlays (rename/picker/help/feed). The float never dims the
  workspace — it is a pane, not a modal.
  **[Amended 2026-08-20 — the flash's place named.]** C10's fallback (a
  flash painted over the body's last row when the hint bar is not drawn)
  had no place in this list, and two of its consequences were therefore
  uncontracted rather than chosen. Both are now: it paints **above the
  float**, so a float whose bottom border reaches the body's last row has
  that one row overpainted for the flash's two seconds — right, because a
  refusal or a confirm-arm prompt is about the whole session and the float
  is one pane; and it paints **below modals** and inside `body`, so it is
  dimmed by C12's backdrop along with everything else behind an open modal
  — also right, since a modal owns the frame while it is up. Neither costs
  the float or the modal any input: the flash is chrome with no hitbox
  (`hit_test` reads `display_rects`, which the flash does not touch).
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
     Alt+Shift+arrows, Alt+g, Alt+Shift+g, Alt+Shift+hjkl, Alt+z — first hide
     the float and restore
     `prev_focus`, *then* apply. (The float is outside the layout tree;
     without this, `spawn_child`'s empty-tab fallback at `app.rs:1425–1427`
     would wipe the tab's layout when asked to split a pane the tree doesn't
     contain.)
  4. Alt+w closes it for real (above); Alt+Shift+z hides it (`Alt+f`
     before the 2026-09-03 re-key).

  **[Amended 2026-08-19, C33 and C37]** Rule 3's list gains `Alt+Shift+hjkl`
  and `Alt+Shift+g` — a swap and a reverse layout cycle are both structural
  pane actions and hide the float like the rest. Same reasoning as C21's
  amendment of the same date: a list this one enumerates by name is wrong,
  not merely incomplete, when an action is missing from it.

  **[Amended 2026-09-01, the map re-key — and one older correction it
  surfaced]** Rule 3's list is now **Alt+n · picker launch · Alt+s ·
  Alt+Shift+s · Alt+o · Alt+- = < > · Alt+g · Alt+Shift+g** — `apply`'s
  structural guard, plus picker launch, which hides the float from its own
  call site for the reason C21's amendment records (opening the picker is
  not structural; launching from it is). Three changes, by the same enumerated-by-name
  rule: `Alt+Shift+s` (explode) is new and belongs; the resize entry
  re-spells to the punctuation family; and **`Alt+Shift+hjkl` /
  `Alt+Shift+arrows` and `Alt+z` come off the list** — that half is a
  correction of an older drift, not of the re-key. `MovePane` left the
  guard with C38 (see that contract: while it was there, C38's float row
  was dead code and the swap silently landed on the pane the float was
  covering), and `Alt+z` was never in the guard at all — it is excluded
  deliberately, taking the "can't zoom the float" no-op instead of a
  retarget onto `prev_focus`. Both now refuse the float themselves rather
  than hiding it first, which is a different rule with a different visible
  result, so listing them here made rule 3 say something the code does not
  do.
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

**[Amended 2026-09-03 — the Alt+f re-key.]** `Action::ToggleFloat` moves off
`Alt+f` onto `Alt+Shift+z`; `Alt+f` is now deliberately unbound. **Why now:**
Alt+Right, on any terminal that has not negotiated the kitty keyboard
protocol, is delivered as the meta-ESC pair `ESC f` — the *identical bytes*
roost already reads as Alt+f, because `encode_raw`/`translate` never see the
wire form, only the parsed `KeyEvent`, and crossterm cannot tell "the user
pressed Alt+Right on a terminal using the old encoding" apart from "the user
pressed Alt+f" — they are the same input. Every other chord this table
documents that collides with something a user wants (readline's `M-<`/`M->`
in the resize family, §8's collision-flags list below) is a *shared* key the
user opted into losing by choice of keymap; this one silently ate a
navigation key nobody bound on purpose, on any terminal old enough to lack
kitty's protocol — which is most of them. Raw mode (C23) cannot fix a global
chord, and there is no byte-level fix: the only way to stop losing Alt+Right
is to stop binding Alt+f.

**Why `Alt+Shift+z`, not a fresh unshifted letter.** `Alt+z` is already
`Action::ToggleZoom`, and this section's own 2026-07-27 U23 amendment (§8's
copy: "`Alt+z / Alt+f` (the two view toggles)") had already named the two
actions as a natural pair before this amendment existed — it just hadn't yet
made that pairing cost a key. Landing `ToggleFloat` on `Alt+Shift+z` spends
no new letter (`z` was already spent) and matches the same-letter
shift-pair idiom the 2026-09-01 modifier-consistency re-key established for
`g`/`Shift+g` (C37), `m`/`Shift+m` and `i`/`Shift+i` (§8), and `s`/`Shift+s`
(C6–C8): the unshifted chord is the *view* toggle, the shifted one the
*pane* toggle they are next to in every other sense. `Alt+Z` (uppercase, no
SHIFT bit) is accepted too, the same delivery tolerance every other shifted
letter in this table carries. `Alt+v`/`Alt+x` were considered and rejected —
C40 reserves them deliberately (mark/pull), and they are the only two
unshifted letters still free in both roost and the shells this table's
collision-flags paragraph tracks; spending one there would trade a
still-open readline collision for a permanently closed one.

**What `Alt+f` becomes.** Unbound, falling through to U5's forward-the-key
default: `ESC f` now reaches the pane, restoring `M-f` (readline
forward-word) to shells that bind it — completing the `b`/`d`/`f` trio §8's
free-key discussion already reserved two-thirds of. Anyone who preferred the
old chord can still have it: `{"keys": {"alt+f": "toggle_float"}}` in
config.json (`docs/KEYBINDINGS.md`) — now an informed choice to spend `M-f`,
not roost's default.

**No shim, same precedent as 2026-09-01.** No dual-binding period, no
deprecation warning. The overlay (C15) teaches the new row the next time it
is opened, and `roost keys` prints the effective map — the same answer the
2026-09-01 re-key gave to "why did my chord move."

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
    titled: `"{id} {name} · {adapter} · raw {glyph}"`, untitled:
    `"{id} {name} · raw {glyph}"` — the `raw` token fg `ACCENT_DIM` (the
    "roost stepped back" color family, C11/C16). [Id prefix per C4's U2
    amendment, 2026-07-27.]
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

**[Amended 2026-07-27, theme inheritance]** The badge's `raw` token is
`accent_quiet()` and the collapsed row's `raw · ` prefix rides the row's
`quiet()` right segment, as before. Raw mode adds no surface of its own, so
this contract is a rename only.

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
  model, three input methods as of C29's native selection — [Amended
  2026-08-06, PR #46 audit, D4]): inclusive anchor→cursor, reading order,
  same `Selection` struct, same `highlight_selection`, same `grab_text`. Honest
  limit, shared with the mouse path and documented: the **visible grid
  only** — no scrollback paging inside copy mode (deliberately left out;
  Scroll mode remains a separate concern).
  **[Amended 2026-07-27, SPEC-ux U9]:** "no scrollback paging" is
  superseded — `PgUp`/`PgDn` in copy mode page the **view** by the pane's
  inner height (grid-clamped), and Alt+c from Scroll mode hands the frozen
  view over intact (the Alt-chord snap exempts exactly the copy
  transition), so history can be yanked by keyboard. The visible-grid
  extraction limit itself **stands**: cursor and selection live in visible
  cell space, and yanking grabs what is on screen at yank time — paging
  moves the paper under them, what you see is what yanks.
- **Mouse drag still works in copy mode** and simply replaces the keyboard
  selection (both write `app.selection`); a drag also moves the cursor to
  the drag point, so the two methods interleave without surprises.
- Hint pairs per C9 amendment: `hjkl move · v mark · y/↵ yank · drag select
  · Esc exit`.
- Unit tests: motion clamping; `0`/`$`; anchor toggle and extension;
  yank-with/without-selection; Esc clears; drag-replaces-keyboard-selection.

**[Amended 2026-07-27, SPEC-ux U17 — the key set grows by four]:** "the
brief's minimum, nothing more" is superseded. Copy mode was the one place a
user goes to *do* something with an agent's output, and it could only be
walked one cell at a time. Added:
- **`V` — line select.** Selects the cursor's whole row: anchor
  `(row, 0)`, cursor `(row, inner_width − 1)`, which the mode cursor also
  moves to. Deliberately **absolute, not a toggle** like `v`: it always
  (re)takes the current line, so pressing it again is idempotent and
  `j`/`k` then extend by whole rows. `v` and `Esc` remain the ways to
  clear. Rationale: "grab what the agent just said" is the gesture people
  reach for first, and it should never need a clear-then-mark dance.
- **`w` / `b` / `e` — word motions**, whitespace-delimited (vim's WORD, the
  same tokenizer `find_url_at` uses, so `w` and Alt+click agree on where a
  token starts). They walk **within the cursor's row only** and clamp at
  both ends rather than wrapping onto a neighbour: the cursor lives in
  visible-grid cell space, and a motion that silently changed rows would
  break the selection model. Pure `word_forward`/`word_back`/`word_end`,
  unit-tested at both clamps.
- Motions extend an active selection under the existing uniform rule — no
  new selection semantics.
- The hint list and the `Alt+c` help row now name every one of these keys,
  `0`/`$` included (C9 amendment above; C15/§8's row reads
  `copy mode (hjkl w b e 0 $ v V y) / scroll mode`).

**[Amended 2026-07-27, SPEC-ux U19 — `o` opens the URL under the cursor]:**
copy mode gains one more key. Alt+click has always opened a URL, but no
keyboard path opened one at all, so the single most common thing an agent
prints — a link — was reachable only by leaving the keyboard. `o` looks up
`App::url_at` at the cursor cell and, on a hit, stashes it in
`pending_open` for the composition root to hand to the browser (core does
no I/O — the same split `pending_yank` uses); on a miss it flashes
`no URL under the cursor` rather than no-opping silently. Either way the
mode stays open: opening a link is not a reason to lose your selection.
Hint pair `o open`; help row `Alt+click / o`.

### C24b — Mode entry chords toggle — [Added 2026-07-27, SPEC-ux U18]

**Current:** `handle_mode_key` special-cases exactly one chord — C20's
Alt+e closes the feed. Every other Alt chord resets `mode` to Normal and
falls through to its global binding, so a mode's *own* entry chord
re-enters it: Alt+c in copy mode discards the cursor and selection for a
fresh one, Alt+PageUp in scroll mode snaps the view to the live tail and
calls that scroll mode, Alt+? in Help closes and reopens the overlay.

**Target:**
- **A mode's own entry chord exits it.** The rule is universal, not a
  per-mode carve-out: Alt+r / Alt+Shift+r (Rename), Alt+Enter (Picker),
  Alt+PageUp (Scroll), Alt+c (Copy), Alt+? (Help), Alt+e (Feed).
- **Derived from the binding table, not a copy of it.** The check runs the
  incoming key through `input::translate` and compares the resulting
  `Action` against `mode_entry_action(mode)`, so rebinding a chord moves
  its toggle with it and the two can never disagree.
- **Toggling off *is* exiting.** Both routes go through one `exit_mode`:
  Scroll snaps `set_scrollback(0)`, Copy drops its selection, the rest just
  return to Normal — identical to what that mode's `Esc` does, pinned by
  `esc_and_the_entry_chord_leave_identical_state`.
- **Nothing else changes.** A chord that is *not* the current mode's entry
  chord still breaks out to its global binding, mode reset and U9's
  scroll-snap (with its Alt+c exemption) included. The consumed toggle
  returns `true`, so it is never re-dispatched.
- Rename's toggle cancels (Esc semantics), discarding the buffer: an
  explicit second Alt+r is a deliberate act, unlike U8's stray click, which
  still may not throw unsaved text away.

**[Amended 2026-08-20, the escape hatch is now a gate]** "Nothing else
changes" above is the load-bearing half of this contract, and until now the
only half with no test: it was one `if` at the top of `handle_mode_key` and
an assertion nowhere. State it as the invariant it is — **an Alt chord
always leaves the mode** — and name the complete list of chords a mode may
keep, which is two:

1. Its own **entry chord**, the bullets above. Consumed, and *exits*.
2. **`Alt+Enter`** in the two multi-line editors — the note editor (C32) and
   the composer (C36). Consumed, and **does not exit**: it breaks a line and
   the dialog stays open. This is the tree's only *retained* Alt chord, and
   the only admissible kind. Terminal.app spells Shift+Enter as exactly
   those bytes (README's CSI-u collapse), so there it is the only spelling
   of "break a line" the dialog can receive — the chord is the mode's **own
   text**. That is the whole test a carve-out must pass; it is never a
   shortcut for "this mode wants that key". Note the asymmetry with (1),
   which is the point: an entry chord is a *way out*, a retained chord is
   *content*.

Everything else drops to `Mode::Normal` and dispatches globally, so `Alt+q`
quits, `Alt+n` opens a pane and `Alt+1` switches tabs from inside any dialog
— **out of any mode**, which is narrower than "from anywhere" and
deliberately so: C23's raw pass-through (a raw pane in Normal mode) claims
every Alt chord but its own toggle, and a C11 terminal that never sends ALT
at all has no chord to route.

Pinned three ways, all added 2026-08-20:
- `the_only_alt_chords_a_mode_keeps_are_the_two_c24b_names` sweeps every
  bound chord against every mode — 1,111 probes — and fails on any third
  consumption. Sweeping the space rather than listing chords is what makes
  it a gate: a mode that claims some future `Alt+x` is caught without anyone
  remembering to test `Alt+x`.
- `every_mode()` carries an exhaustive `match` over `Mode`, so a variant
  cannot be added without that `match` failing to compile — which lands the
  author in the one function that has to know about it.
- `tests/modal_quit.rs` drives five real modals in a real PTY, asserting
  each is on screen **before** pressing anything at it. That assertion is
  the one worth keeping: a simulation agent reported `Alt+q` hanging in
  every modal, and its harness had pressed the opening chord 100 ms after
  spawn, before roost drew a frame. The modal never opened, so what it timed
  was Normal mode against a deadline shorter than a real quit. A modal test
  that never opened a modal reports on a screen it never reached.

### C25 — Canned layout cycle (Alt+g / Alt+Shift+g) — [Added 2026-07-22, fleet features]

**Current:** layout shape is built up manually (splits Alt+n/Alt+o, stacks
Alt+s, ratios Alt+Shift+arrows); no way to snap the tab to a known-good
arrangement. **[2026-09-01: this paragraph is the world as C25 found it, and
two of its spellings have since moved — ratios are `Alt+- = < >` now, and
`toggle_stack` is the `stack_pane`/`explode_stack` pair. Left as written
because it is a dated snapshot, annotated because a reader looking for the
current map should not have to know that.]** Tree ops live in `layout.rs`
(`toggle_stack :129–166`, `split_pane :54–78`); `MIN_SPLIT_COLS/ROWS = 36/10` gate splits
(`app.rs:31–32`).

**Target:**
- New `Action::CycleLayout`, bound to **Alt+g**. Mnemonic: *arranGe / grid*.
  **[Amended 2026-08-19, C37]** It carries a `forward: bool` now, and
  `Alt+Shift+g` walks the same cycle backwards — see C37 for the arithmetic
  and for why stepping back from the first arrangement wraps to the last
  rather than restoring a pre-cycle layout.
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
  **fitting** arrangement, skipping unfit ones.

  **[Corrected 2026-08-19, C37 audit]** This used to end "the counter lands on
  what was applied", which is not what the code does and never was:
  `layout_cycle` is set to `idx + 1`, i.e. **the arrangement to try next**, so
  what is showing is `layout_cycle - 1`. The distinction is invisible while the
  cycle only runs forward and is exactly what C37's reverse direction has to
  reason about, which is how the error surfaced.
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

### C27 — Fleet roster (Alt+Shift+a) — [Added 2026-07-28, vertical-tabs tribunal]

**Provenance.** A three-member tribunal evaluated adopting a herdr.dev-style
left vertical sidebar and **rejected** it: at roost's 80×24 floor a 20-column
rail leaves 30-column panes, below `MIN_SPLIT_COLS = 36`, so side-by-side
agents become illegal by roost's own predicate — and herdr's two sections are
Workspaces + Agents, a tier roost's singleton `Workspace` does not have. The
tribunal did find one gap, unanimously, and this contract closes it: **roost
has no surface showing named, per-pane identity for panes in *other* tabs, at
rest.** `Alt+a` (C19) reaches them without listing them; `Alt+e` (C20) is a
time-ordered log, not current state; a tab with three needy agents renders as
one `◆`, identical to a tab with one (the C2 amendment of the same date
addresses that last one from the tab bar's side).

**[Correction 2026-07-28 — one of the tribunal's two reasons was wrong.]**
The arithmetic above stands, with one clarification and one gap:

- *Clarification:* a 20-column rail at the 80-column floor does not make
  splitting impossible, it makes **side-by-side** panes impossible.
  `spawn_child` picks its direction from the target's rect shape, so it
  would stack instead — which is a real loss, but a narrower one than
  "illegal by roost's own predicate" reads as.
- *The gap:* the tribunal ran the numbers on **one** rail width — herdr's
  20-column labelled one. A rail of **≤ 8 columns clears the floor**
  (72 body ÷ 2 = 36, exactly `MIN_SPLIT_COLS`), which is what a *collapsed*
  rail costs. That tier was never evaluated.

**The second reason is simply false.** "A tier roost's singleton
`Workspace` does not have" identified the missing tier as the workspace. It
is the **directory**: every `PaneSpec` carries a `cwd`, C14's picker
already keeps a recent-directory column and floats the ones you launch
into, and nothing in roost has ever grouped by it. herdr's own sections are
repositories and the agents inside them — which roost can express exactly.

Recorded because it changes what a future sidebar would *be*, not just
whether to build one: grouped by tab it rotates the axis C2 already draws,
which is why it reads as duplication; grouped by **project → agent** it
adds an axis roost has never shown. That variant is parked, not rejected —
see ROADMAP's entry for the model, the tiers and the honest costs.

**Current:** none of the above surfaces answers "what is my fleet, right
now". `App::attention_ring` (`app.rs`) already enumerates the workspace in
the right order but only over `NeedsInput` panes and only to move focus.

**Target — a read-only navigator that makes the fleet legible without moving
you.**

- **Binding: `Alt+Shift+a`** (`Action::ToggleRoster`), also accepted as
  `Alt+'A'` — the uppercase-delivery tolerance `Alt+Shift+r` and
  `Alt+Shift+p` already carry, and the same in-repo precedent for choosing a
  *shifted sibling* at all. Deliberate pairing with C19: **`Alt+a` takes you
  to the next one; `Alt+Shift+a` shows you all of them and lets you choose.**
  The unshifted Alt pool cannot supply this chord — `b`/`d` are live readline
  word-ops since U5, `i`/`m` went to U7's tab navigation, and `p` is reserved
  by C23 so the raw toggle has no near-miss.
  (Delivery note: unlike C23's `ESC`+`P`, `ESC`+`A` is not an escape
  introducer, so it carries none of N3's DCS ambiguity.)
- **Surface: a C12 modal** (`Mode::Roster`), drawn through the existing frame
  + `draw_mode_overlay` path — the same machinery the feed, picker and help
  use, so the backdrop, border, anchoring and U8 mouse rules all apply
  unchanged. It is the **fifth** C12 modal (C12's U8 amendment lists four).
- **Geometry:** the roster calls `feed_overlay_size` directly —
  `w = min(72, body.width − 4)`, `h = min(16, body.height − 4)`, anchored by
  `centered_near` like every other modal. Deliberately one function, not a
  copy: the two overlays answer the fleet's two questions and must not
  resize under a user toggling between them, and the 80×24 floor is then
  proven once for both.
- **Content:** every pane in the workspace, **grouped by tab**, the float
  (C22) last under its own group.
  - **[Amended 2026-08-07, ux P2-11 — worst-first, not tab order]** "Tabs in
    order, panes in that tab's `pane_order()`" is withdrawn as the sort key:
    past one screenful — the fleet case the roster exists for — tab order
    means scrolling past every quiet pane in every earlier tab to find the
    one ◆ parked in a later one, which is the exact hunt `Alt+Shift+a` was
    built to end. Both tiers now sort **worst-first**
    (`roster_rank`: ◆→○→●→·(incl. "not started")→exited, C5's own severity
    order): panes within a tab's group, and the groups themselves by their
    own worst pane — so the tab holding the fleet's one ◆ is the *first*
    group, and that pane its *first* row. Both sorts are Rust's stable sort,
    so panes/groups tied on rank keep C19's ring order (tab index, that tab's
    `pane_order()`) — the roster and `Alt+a` still agree on *tie* order,
    just no longer on row order once severities mix; an all-quiet or
    all-needy fleet (every existing fixture) reads exactly as before. The
    rank read is `App::display_status`, not the raw runtime status — C27's
    own "not started" rung (`None`) ranks with `Idle`, and P2-10's shell
    downgrade (below) applies here too, so a quiet shell never out-ranks a
    resting agent's ○. The float's own group is **not** reordered by
    severity, matching C19's ring, which also pins it last regardless of
    urgency — one placement rule for both surfaces.
  - **[Amended 2026-08-07, ux P2-11 — status filter]** `Tab` cycles a status
    filter forward through six stops (every tier, then ◆, ○, ⠋ (steady frame),
    ·, exited — `roster_rank`'s own order, so the filter and the sort agree on
    what "worse" means); `Shift+Tab` steps back; both wrap. The Working stop
    is one of C5's three sanctioned steady-frame cases (with N1 and the C15
    legend): the tag names *which tier* is filtered, not a live per-pane
    report, so it never substitutes the live spinner. It composes with the
    type-ahead **by AND** — a row must satisfy both, the ordinary multi-facet-
    search reading and the simplest rule that doesn't special-case either
    filter. A group whose panes all fail *either* filter drops its header
    too, same as type-ahead-only always did. The active tier tags the frame
    title with its own C5 glyph **and color** (`fleet — ◆ only`, or
    `fleet — ◆ clau▏` with a query too — the `◆` alone carries `accent()`,
    the rest of the title stays `ink()` [design-supervisor D4: a bare `ink()`
    glyph reads as no tier at all, which defeats the tag]) — the type-ahead's
    own "a narrowed list always says why" idiom, extended to the second
    filter rather than given a competing one.
    `Tab`/`Shift+Tab` are not `Char`, so neither collides with type-ahead
    text (U20's rule: a list you filter by typing cannot reserve letters) —
    they are the two keys that were left over once every printable was
    spoken for.
  - **The float is listed whether or not it is shown.** [Amended
    2026-07-28, supervisor SG-1] Hidden is a display state, not an absence,
    and C19's ring carries a hidden float that needs you — so a roster that
    listed it only while shown could *open with its cursor on a row that
    isn't there*: no `❯` drawn anywhere and `Enter` acting on something
    invisible. The invariant is the general one: **every pane the opening
    cursor can land on is a row.** `Enter` on the float's row shows it, the
    same way a ring jump does.
  - **Group header rows** use C6's idiom: an uppercase label
    (`" 1 MAIN · 4 PANES"` — the tab's own bar label, then how many of its
    panes are being shown), `quiet()`, `Modifier::UNDERLINED` across every
    cell of the row (the label is padded to the row width so the rule runs
    edge to edge, exactly as `stack_header_text` does it). A header is a
    label, never a destination.
  - **Pane rows reuse C8's collapsed-row format verbatim** — marker + glyph +
    id + name + `{adapter} · {state word}` — by calling the very same
    `collapsed_row_spans`, one column narrower. `display_name_live` /
    `App::display_name`, `state_word`, `collapsed_name_style` and the C5
    glyph table are all shared, so a pane reads identically here, in a
    collapsed stack row, and on its badge. **No new glyphs**: §2's inventory
    holds.
  - **A pane that has never been spawned reads "not started", never
    "exited".** [Amended 2026-07-28, supervisor D1] The roster is the only
    surface that lists panes outside the active tab, and `spawn_active_tab`
    starts *only* the active tab's panes — so **a workspace restored from
    disk has no runtime for any other tab**, which is precisely C27's
    headline case. Rendering a runtime-less pane as dead turned the first
    `Alt+Shift+a` of a session into a morgue, contradicting the `·` the tab
    bar drew on those same tabs in the same frame. The single source is
    `App::row_status(id) -> Option<AgentStatus>` (`None` = no runtime and
    not recorded dead), which is also the control plane's `status_str`
    `"unknown"` rung, so the CLI and the chrome answer this from one place.
    The `None` row's glyph and style come from
    `theme::tab_summary_style(TabSummary::Unknown)` — **the tab bar's own
    call** — so the two surfaces cannot disagree by construction, and the
    state word is `not started`. Still no new glyph: `Unknown` already
    resolves to the idle dot.
  - **Empty state.** [Amended 2026-07-28, supervisor SG-2] The type-ahead
    is the only way to empty the list (a workspace always has a pane), and
    an empty overlay would read as a broken one. It draws a single centered
    `no pane matches` line in `quiet()` at the inner area's middle row —
    C20's empty-feed shape verbatim, for the same reason: a modal that
    explains its own emptiness beats one that just goes blank.
    **[Amended 2026-08-07, ux P2-11]** No longer the *only* way: the status
    filter can empty it too (a tier nothing is in). Same row, same reason —
    the emptiness has one shape regardless of which filter caused it.
  - **Two marks, two meanings.** The row's leading column is the *cursor*
    (`❯`, `accent()` — the C14/C20 idiom), and C8's own `▎` still marks the
    *focused* pane inside the row. "What will Enter act on" and "where am I
    right now" are different questions and get different marks rather than
    one overloaded one.
- **Opening cursor lands on the pane `Alt+a` would jump to** — the same
  worst-first ring order, through the shared `App::attention_next` that
  `jump_attention` itself now calls. This is load-bearing: it makes
  `Alt+Shift+a` `Enter` land exactly where `Alt+a` lands, so the roster is a
  superset of the chord users already know rather than a competing command
  (pinned by `roster_enter_lands_exactly_where_alt_a_would_have`). With
  nothing needing attention it opens on the focused pane.
  - **One case is deliberately not equivalent** [Amended 2026-07-28,
    supervisor SG-1]: with the float *focused*, `Alt+a` dismisses the float
    instead of jumping (C22 rule 2), while the roster still opens on
    `attention_next`. The equivalence claim is about where a jump *lands*,
    not about C22's dismissal shortcut — the roster's job is to show you the
    fleet, and it has no row that means "hide this".
- **Keys.** `↑`/`↓` move by pane (headers are skipped by construction —
  the cursor is a `PaneId`, and a header has none), clamped at both ends
  rather than wrapping; `PgUp`/`PgDn` page by half the overlay's height (C20's
  `overlay_page` rule, one source for both keys); `Enter` jumps; `Esc` dismisses;
  `Alt+Shift+a` toggles closed (U18: a mode's entry chord exits it, derived
  through `translate` like every other mode's). **[Amended 2026-08-07, ux
  P2-11]** `Tab`/`Shift+Tab` cycle the status filter — see below.
- **Type-ahead filters the list**, U20's picker idiom: every printable
  character narrows it by ASCII-case-insensitive substring over **id, display
  name and adapter** (the three things a row shows — the id because it is the
  `roost send <id>` join key), `Backspace` widens, and the live query replaces
  the frame title's tail (`fleet — clau▏`) so a narrowed list always says why
  it is narrow. A group whose panes all filter out loses its header too — an
  empty group is a row that says nothing. `Enter` acts on the filtered
  selection, and the cursor follows the filter when the pane under it is
  narrowed away. **[Amended 2026-08-07, ux P2-11]** Composes by AND with the
  status filter — full rule and the title-tag behavior are with the sort
  amendment above, so the two P2-11 changes are described in one place.
  - **Deviation from the brief, recorded:** the brief asked for `j`/`k` as
    motions *and* type-ahead, and asked for `q` to dismiss. Those cannot both
    hold — U20 settled this exact conflict for the picker: *"a list you filter
    by typing cannot reserve letters"*, and it is why `j`/`k` stopped being
    picker motions. The arrows and the paging keys are the motions here; `j`,
    `k` and `q` are filter text like every other letter, and `Esc` (plus the
    entry chord) is the way out. The hint bar advertises exactly that set, so
    nothing advertised is lost.
    **[Amended 2026-08-07, ux P2-11, design-supervisor D3]** The set gains
    `Tab`/`Shift+Tab`, the status filter's cycle (above) — not `Char`, so
    they cost the type-ahead nothing (same reasoning U16/U20/U25/P21 each
    recorded when their own mode grew a key: an unadvertised key is an
    absent one). The hint bar's `Tab status` pair and the C15 overlay's
    `FLEET` row both say so; nothing here is silent.
- **Jump goes through `focus_attention_target`** — the helper C19's ring and
  C20's `Enter` already share — so tab-switching (`go_to_tab` semantics, lazy
  spawn, C21's zoom rule), collapsed-stack expansion and float-reveal all come
  free and behave identically to every other jump in roost. The roster closes
  behind the jump. Ids are recycled (`next_pane_id` is a max+1), so a pane
  that closed while the roster was open flashes `that pane is gone` without
  moving focus and without closing the overlay: a no-op you can see beats a
  silent one (C20's own rule).
- **Live.** The rows are recomputed every frame from the workspace, so
  statuses change and exited/closed panes leave the list while it is open. It
  is a monitoring surface; stale state is worse than none. The cursor is a
  **`PaneId`, not a row index**, precisely so the list can churn underneath it
  without the cursor silently re-pointing at a different pane. The scrolled
  window (`top`) is the one piece of view state, clamped by the pure
  `roster_top_clamped`, which the renderer and the mouse hit-test both reach
  through the single `App::roster_view` accessor — one rows-and-offset answer
  per frame, so a click can never land on a different row than the one under
  the pointer (§4/§5 lockstep); it scrolls the least it can to keep the cursor visible,
  and takes the cursor's group header with it when revealing upward (a row
  whose tab you cannot see is a pane you cannot place).
- **Mouse (U8's modal rules, no exceptions):** a click on a pane row jumps in
  **one** press (C14's "the picker is a launcher" reasoning — "select, then
  confirm" would be a second click for nothing), hit-tested by the existing
  `mouse::picker_row_at` against the dialog's *drawn* rect; a click on a
  header does nothing; a click outside dismisses; the wheel moves the roster's
  **cursor** (one row per notch, the arrows' own step) and never reaches the
  pane beneath. The wheel deliberately moves the cursor rather than a detached
  view: `Enter` acts on the cursor, so a wheel that scrolled the list out from
  under it would leave the overlay pointing at a row nobody can see. Paste is
  swallowed like every other non-Rename modal — a pasted blob is not a filter
  anybody meant to type.
- **Chrome:** mode word `ROSTER` (C9's list); hint pairs
  `↑↓ select` · `PgUp/Dn page` · `↵ go to pane` · `type filter` · `Esc close`
  (**68 columns**, inside the 100-col floor beside the right segment).
  **[Amended 2026-08-07, ux P2-11]** `Tab status` joins the list (**81
  columns**, still inside the floor). The C15 overlay teaches the chord on C19's own row, merged:
  `Alt+a / Alt+Shift+a` → `jump to next pane that needs you / list every
  pane` — the merge idiom the ≤20-row cap has always demanded, and here also
  the honest presentation of a deliberate pair.
  **[Superseded 2026-07-28, C15's cap retirement]** the merge is undone:
  `Alt+a` and `Alt+Shift+a` are two rows of the overlay's `FLEET` group,
  which is where the pairing is now legible without cramming. The chords,
  their meanings and this contract are unchanged — only the row packing was,
  and it was only ever a consequence of the cap.

**The C20 distinction (contracted, so the two never drift):**
> **`Alt+e` answers "what happened"** — a time-ordered history of transitions,
> spawns, exits and control calls, newest last, with entries that outlive the
> panes they name.
> **`Alt+Shift+a` answers "what is"** — the current state of every pane that
> exists right now, grouped by where it lives, with no history at all.
>
> A change that gives the roster a timeline, or the feed a grouped
> current-state view, is a merge of two contracts and must be argued as one.

**Scope: jump is the only action in v1.** No close, no send, no broadcast,
no rename. Rationale, recorded so a later change is a deliberate decision
rather than a drift: those would turn a navigator into a control panel,
duplicate keys the panes already answer to, and `roost send <id>` (with the
id the roster puts in front of you) already covers scripted dispatch —
the CLI is also where the fat-finger-unsafe verbs deliberately live (§7's
"no TUI broadcast key"). Adding an action here means re-arguing that split.

**Unit tests (the executable form of this contract):** ring-order opening
cursor and the `Alt+a` equivalence · row grouping incl. the float's own group
· a **hidden** float still listed, so the opening cursor is always a real row
· a **restored, never-spawned** tab's panes reading `not started` under the
tab bar's own `Unknown` glyph rather than `exited`
· header skipping and end clamping · type-ahead over id/name/adapter with
Backspace widening and header collapse · `Enter` through the shared helper
across tabs · stale-cursor flash · entry-chord and `Esc` toggling closed ·
window follow/clamp · mouse row-click jump, header-click no-op, outside-click
dismiss, wheel-moves-cursor · the drawn overlay (headers underlined, C8 rows,
both marks, live query). Plus the PTY e2e `tests/roster_overlay.rs`: the
roster lists a **non-active** tab's panes and jumps across to one, with
`roost list` as ground truth — that cross-tab case is the whole point of the
feature, so it gets a real terminal.

**[Amended 2026-08-07, ux P2-11]** Added: worst-first reorders both panes
within a group and groups by their own worst pane, an all-tied fleet keeping
the old tab-order reading · `Tab`/`Shift+Tab` cycle the status filter through
all six stops and wrap · the status filter composes with type-ahead by AND,
including the group-drops-whole case with either filter as the cause · the
title tags the active tier with its own glyph. Plus the three-way interaction
with C19's P1-5 fallback and P2-10's shell downgrade
(`roster_and_ring_agree_on_a_mixed_instrumented_and_shell_fleet`): a fleet
mixing an instrumented ◆, an uninstrumented agent resting on a heuristic ○,
and plain shells — the roster sorts the ○ ahead of every shell, and the ring
opens the cursor on the real ◆, never the resting agent or a shell.

### C28 — Move a pane between tabs (Alt+i / Alt+Shift+i) — [Added 2026-07-28, re-keyed 2026-09-01]

**Current:** a pane is born in a tab and dies in it. Every arrangement verb
roost has (`Alt+s`, `Alt+o`, `Alt+g`, `Alt+Shift+arrows` — the last of which
resized when this was written and moves the pane since the 2026-09-01
re-key) rearranges panes
*within* one tab; the only way to get a running agent into a different tab is
to close it and start another one over there, which throws away its PTY, its
scrollback and its session. Tabs are how roost separates concerns, and
concerns get reassigned — a pane started in `main` while exploring belongs in
`api` once you know what it is.

**Target — the pane changes tabs and you go with it.**

- **Binding: `Alt+i` / `Alt+Shift+i`** (`Action::MovePaneToTab
  { forward }`), also accepted as `Alt+I` (the uppercase-delivery
  tolerance `Alt+Shift+r`/`a`/`p` already carry). **[Re-keyed
  2026-09-01]** — the verb moved off `Alt+Shift+i`/`Alt+Shift+m` so the
  two tab families are same-letter, shift-reverse pairs like C37's
  `g`/`Shift+g` (§8 amendment): `m`/`Shift+m` step the strip, `i`/`Shift+i`
  carry the pane — **the unshifted chord of either family moves you or
  your pane forward, the shifted one backward.** `Alt+Shift+i` keeps the
  direction it has always had (carry-previous); `Alt+M` follows
  `Shift+m` to previous-tab. Costs the unshifted Alt pool nothing (§8's
  amendment).
  `Alt+[`/`Alt+]` — the brief's suggestion — are rejected: `ESC [` is the
  CSI introducer, so `Alt+[` cannot be told apart from the start of an
  escape sequence (§8's standing rejection, same hazard as C23's `ESC`+`P`).
- **Wraps at both ends**, exactly like the tab-step pair it shares the
  idiom with.
- **The process is never touched.** `runtimes` is keyed by `PaneId` across
  the whole workspace, so a running agent keeps its PTY, its scrollback and
  its resume session through the move; only the layout trees and the spec
  change hands. This is the entire reason the chord exists rather than
  "close it and start another one over there", and it is what
  `move_pane_to_tab_carries_the_pane_and_the_focus_without_respawning`
  proves by stamping the live runtime before the move and reading it after.
- **Where it lands:** split off the destination tab's remembered focus pane
  (U11's `tab_focus`, else that tab's first), cut the widest way — the same
  rule `spawn_child` uses for `Alt+n`, so a moved pane arrives exactly where
  a new one would.
- **Refusals are checked *before* anything moves,** and all three are
  flashed rather than silent (C11: a no-op you can see beats one you can't):
  the C22 float belongs to no tab (`the scratch pane belongs to no tab`); a
  lone tab has nowhere to send (`only one tab`); and a destination whose
  split would break C25's `MIN_SPLIT_*` floor (`{tab} has no room`). The
  destination's geometry is real, not guessed — `compute_rects` is pure and
  the body area does not depend on which tab is active — so the check is the
  same arithmetic the split itself will do. Ordering is the contract here: a
  pane must never be stranded between two tabs by a split the destination
  turns out not to be able to take.
- **Moving a tab's last pane away removes the tab**, `close_pane_id`'s own
  rule (a tab with no panes is not a thing roost can draw), with the same
  index fix-up — and the destination index shifts down with it when the
  source sat before it. **No undo record**: the pane is alive in the
  destination, and resurrecting the tab around it would duplicate it. The
  inverse chord is the way back.
- **C21/C22:** a move is a tab change, so it exits zoom and hides the float,
  exactly as `go_to_tab` does. U11: the departing pane is cleared from
  `tab_focus` — a tab cannot remember a pane that left it, and coming back
  falls through to its first pane.
- **C20:** one feed line, `moved {id} {name} to {tab}` (plus `closed tab
  {name}` when the source emptied — the same line a last-pane close emits,
  because it is the same event).
- **Chrome:** no new surface. The tab bar's C2 count cells and C27's roster
  both re-read the workspace every frame, so they show the move the instant
  it happens; C15's keymap teaches the pair directly under the unshifted
  chords it is the shifted form of, because that adjacency *is* the
  explanation.

**Unit tests (the executable form of this contract):** the carry (pane,
focus, and the same runtime object) · wrap at both ends · the emptied source
tab removed with the destination index still landing right, and no undo
record · both flat refusals, visible · the pre-flight geometry refusal
leaving *both* tabs untouched · zoom exited · the chord table, with the
unshifted pair proven unchanged.

---

### C29 — Native text selection over a mouse-unaware pane — [Added 2026-08-06]

**Current:** dragging the mouse across a roost pane does nothing at all. Over
a pane whose app never asked for mouse reporting (`MouseProto::None`),
`route_mouse` (`mouse.rs:395–417`) answers every `Down`/`Drag`/`Up` with
`MouseAction::None` — only the wheel and the press-to-focus click
(`App::on_click`) do anything. Selection exists only inside the modal
`Alt+c` copy mode (C17/C24), reachable by nobody who doesn't already know
the chord. The client's standing requirement
(`docs/engagements/2026-08-07-best-in-class/PLAN.md`, Phase 2N, client requirement #1)
is that drag-select, double-click-word, triple-click-line and
shift-click-extend work with **no new shortcuts to learn** — indistinguishable
from a native macOS app — and that ⌘C always lands the emulator's own copy of
whatever roost just highlighted, because roost puts the text on the system
clipboard itself (`infra::clipboard::copy`, unchanged, U14).

**Why a new contract instead of amending C17:** C17 is the copy-mode
selection, and its "must stay modifier-based" rule is unchanged and
unrelated to what *fires* it. This contract is a **second way to reach the
same `Selection`** — a mouse-only gesture that lives entirely in
`Mode::Normal`, never enters `Mode::Copy`, and has clearing rules C17/C24
don't need (Copy mode's own Esc/toggle-off already clears it, C24b). Where
the two share machinery it is called out below rather than duplicated —
`Selection`'s own doc comment (`app.rs:177–189`) now says so too.

**Target:**

- **Scope.** Applies only when `mouse_proto() == MouseProto::None` for the
  pane a gesture is over, in `Mode::Normal`, and only to the pane the P20
  latch resolved (`App::mouse_latch`) — `main.rs::handle_native_selection`
  (`:671–717`), called from the same `handle_mouse` gate (`:653–662`) that
  already computes `state` for P9's wheel routing
  (`state.proto == MouseProto::None`), so the two can never disagree about
  which panes get which treatment. **A `MouseProto::Sgr` pane is untouched**
  — `route_mouse` still owns it exactly as before this contract, and
  `handle_native_selection` is never called for one.
- **Left press** (`Down`) focuses the pane (`App::on_click`, unchanged) and
  starts a selection. `App::click_count(pane, row, col)` (`app.rs:2176–2200`)
  classifies the press as the 1st/2nd/3rd of a run — crossterm reports no
  click count at all, so roost derives one from timing and position.
  **Interval: 500 ms** (macOS's standard double-click window), **tolerance:
  1 cell** (a terminal cell is already coarser than a mouse pixel, so even
  one is generous — but zero would fail a double-click the instant a real
  hand drifts a hair between presses): `DOUBLE_CLICK_INTERVAL`/
  `CLICK_TOLERANCE` (`app.rs:229–234`). A 4th click within the window wraps
  back to the 1st rather than climbing past triple, since nothing above
  triple-click is bound.
  - **1st click** — `App::begin_selection` (`:2155–2164`, shared verbatim
    with C24's mouse path): anchor = cursor = the click point.
  - **2nd click (double)** — `App::select_word_at` (`:2202–2213`): the
    whitespace-delimited run under the pointer, via the new `word_bounds_at`
    (`:4633–4646`) — the tokenizer `find_url_at` (`:4652–4662`) also walks,
    refactored to share it, so `w`/`b`/`e` and Alt+click (already agreeing
    with each other, C24's own U19 amendment) now agree with double-click
    too. No word under the pointer (whitespace, or past the row's trimmed
    content) degrades to a 1st-click-shaped point selection.
  - **3rd click (triple)** — `App::select_line_at` (`:2215–2221`): the whole
    row, anchor `(row, 0)` to `(row, inner_width−1)` — the same shape C24's
    `V` produces.
- **Drag** extends the selection: `App::extend_selection` (`:2165–2174`,
  shared verbatim with C24's mouse path). Clamped to the originating pane by
  construction — the P20 latch already resolves *which* pane every
  `Drag`/`Up` in a gesture belongs to before `handle_native_selection` is
  ever called, so this contract adds no clamping of its own (reuse, not
  reinvention).
- **Shift+left-click** extends the live selection to the pointer, keeping
  the anchor: `App::extend_selection_to` (`:2223–2238`). With nothing of its
  own pane to extend — no selection yet, or one that belongs to a different
  pane — it starts a fresh one at the click point instead, same as a 1st
  click; it also arms `dragging`, so a held drag after the shift-click keeps
  extending through the ordinary drag path.
- **Release** (`Up`): a gesture that never moved (`anchor == cursor` — a
  plain click, or a double/triple-click that found nothing) clears the
  selection outright; anything else calls `App::finish_native_selection`
  (`:2281–2296`) — extracts via the identical `grab_text` path C17/C24 use —
  and, on non-empty text, `infra::clipboard::copy` + `App::flash_copy` (U14,
  unmodified: `copied N chars` / `copied N chars (OSC 52)` / `copy failed`,
  never an optimistic claim before the clipboard has answered). **Unlike
  `finish_selection`, this does not clear `self.selection` or touch
  `self.mode`** — the highlight stays lit.
- **Clearing — "until the next click or keypress":**
  - A left click **on a different pane** than the one holding the selection
    drops it (`App::on_click`, `:2369–2379`) — a click back on the *same*
    pane is left alone there, since the fresh gesture that click also starts
    (1st/2nd/3rd click, above) already supersedes it.
  - Any keypress while `Mode::Normal` drops it (`App::handle_mode_key`, top
    of `:3938–3950`) — gated on `Mode::Normal` specifically, so it can never
    fire once Copy mode is managing `self.selection` for its own cursor (by
    the time that's true, `self.mode` is already `Mode::Copy`). This runs
    *before* the Alt-chord dispatch below it, so pressing `Alt+c` with a
    lingering native highlight clears it first — Copy mode always opens
    clean rather than inheriting a stale selection.
- **Painting: no `render.rs` change.** `if let Some(sel) =
  app.selection.filter(|s| s.pane == pr.id) { highlight_selection(...) }`
  already runs unconditionally on `app.selection`, mode included, so a
  native selection draws with the identical `REVERSED` C17 treatment for
  free.
- **Shared state, not a fork.** Everything above reuses the one `Selection`
  struct and the one `app.selection` field C24 already had — the brief's
  instruction to reuse the existing REVERSED treatment and the U14 outcome
  path is implemented as *the same code path*, not a parallel one.
- **Contracted: tracks grid coordinates, not content** (raised in the PR #46
  review — settled here rather than left undefined). `Selection` is
  `(pane, anchor, cursor)` in inner-cell space; it does not snapshot what
  is under those cells at press time. If the pane's program prints new
  output while a drag is in progress (or between release and the next
  interaction, since the highlight stays lit), the reversed region and
  whatever `grab_text` later extracts follow the *screen positions*, not
  the characters that were there when the gesture started — a fast-moving
  pane can highlight, and copy, text that scrolled into those cells after
  the drag began. **This is deliberate, not a gap**: it is the same
  behavior every terminal emulator's own native selection has (none of
  them diff the grid either — a selection is coordinates over a mutable
  surface in every one of them), C17's copy-mode selection already works
  this way and always has, and coordinate-tracking is what makes
  `highlight_selection`/`grab_text` shareable between the two gestures at
  all. A content-tracking alternative would need the selection to pin a
  *snapshot* of the grid rather than read the live one, which nothing else
  in roost's selection model does and no part of this brief asked for.
  **[Superseded 2026-08-08 — see the selection-freeze amendment below: "If
  the pane's program prints new output while a drag is in progress" and "no
  part of this brief asked for [a snapshot]" no longer hold for the gesture
  window itself; everything else in this bullet (coordinates, not content;
  the between-release-and-next-interaction case) still does.]**
- **Mouse-capture cost.** The pre-existing blanket `EnableMouseCapture`
  requested five DEC private modes; roost now asks for exactly the three it
  uses — `mouse::MOUSE_CAPTURE_ENABLE`/`_DISABLE` (`mouse.rs:49–55`) drop
  `?1003` (any-motion tracking — `route_mouse` has no arm for `Moved` and
  never did) and `?1015` (RXVT extended coordinates, superseded by the
  `?1006` SGR form every gesture in this file already speaks). Verified
  byte-for-byte against crossterm 0.29's own `EnableMouseCapture::write_ansi`
  source; disable is the exact reverse-order inverse, mirroring crossterm's
  own idiom. Pinned by
  `mouse::tests::mouse_capture_sequences_are_the_1000_1002_1006_subset_symmetric_in_reverse`.
- **Safety fix alongside this contract (not itself a gesture):**
  `input::encode_key`'s `KeyCode::Char` arm (`input.rs:309–323`) now
  swallows any SUPER-modified char before it can be forwarded. Most
  terminals keep ⌘ for their own bindings and it never reaches roost at all
  — but a ⌘-chord that *did* arrive (possible under the kitty keyboard
  protocol roost negotiates) must not leak its bare letter into a pane's
  prompt (a ⌘C the emulator's own copy binding missed typing a stray `c`).
  Shared by `encode_raw` (C23's raw pass-through), since that delegates to
  the same function — a raw-focused pane is equally protected.

**Must-not-break, verified:**
- A `MouseProto::Sgr` pane is provably untouched — gated on
  `state.proto == MouseProto::None` before `handle_native_selection` is ever
  called (`a_mouse_aware_pane_is_untouched_by_native_selection_gestures`).
- Wheel routing (P9) is untouched — this contract only ever matches
  `Down`/`Drag`/`Up` of the left button; every existing wheel test in
  `mouse.rs`/`main.rs` still passes unmodified.
- Seam-drag (U21) is checked in `handle_mouse` *before* the pane/latch block
  this contract hooks into, so a border drag can never be reinterpreted as a
  selection (`dragging_a_shared_border_resizes_the_split` still passes
  unmodified).
- Copy mode (C17/C24) is untouched: `handle_copy_mouse` still ignores
  `MouseProto` (the universal escape hatch over a mouse-aware pane), and
  `handle_mouse`'s `if app.in_copy_mode() { ...; return; }` gate runs before
  any of this contract's code.
- Raw mode (C23) needed no amendment: its own text already says "mouse is
  unaffected (raw is a key-path property)" — a raw pane's `MouseProto` is
  whatever its underlying app reports, so native selection applies to it on
  exactly the same terms as any other pane.
- Clipboard honesty (U14): the flash only ever fires from `flash_copy`, fed
  by the real `ClipboardOutcome` — no path claims "copied" ahead of the
  clipboard answering.
- C24's own text is unchanged by this contract: its keyboard cursor, key
  set and hint pairs are untouched, and "mouse drag still works in copy mode
  and simply replaces the keyboard selection" still describes
  `handle_copy_mouse` verbatim — this contract adds a second entry point to
  `Selection`, not a second selection model.

**Unit tests (the executable form of this contract), all in `main.rs`'s test
module, driven through the real `handle_mouse` entry point:**
drag-selects-and-copies, leaving the highlight lit
(`dragging_over_a_native_pane_selects_and_copies_on_release`) · double-click
word (`double_click_selects_the_word_and_stages_the_copy_on_release`) ·
triple-click line (`triple_click_selects_the_line_and_copies_on_release`) ·
shift-click extend (`shift_click_extends_the_selection_and_copies_on_release`)
· a plain click elsewhere clears it
(`a_plain_click_clears_a_lingering_native_selection`) · a mouse-aware pane is
untouched (`a_mouse_aware_pane_is_untouched_by_native_selection_gestures`).
Plus the underlying `App` methods in isolation, in `core::app`'s test module:
`click_count_cycles_through_1_2_3_and_wraps_on_a_4th`,
`select_word_at_grabs_the_whitespace_delimited_word`,
`select_line_at_grabs_the_whole_row`,
`extend_selection_to_extends_the_live_selection_or_starts_fresh`,
`finish_native_selection_extracts_text_but_leaves_the_highlight_lit`,
`on_click_clears_a_selection_that_belongs_to_a_different_pane_only`,
`any_keypress_clears_a_normal_mode_selection_but_copy_mode_is_exempt`,
`entering_copy_mode_clears_a_lingering_native_selection_first`. The SUPER
guard: `ui::input::tests::super_modified_chars_are_swallowed_not_forwarded`.
**[Superseded/extended 2026-08-06 — see the PR #46 amendment below for the
post-audit test list.]**

**[Amended 2026-08-06, PR #46 design + code audit — two behavioural bugs and
two correctness gaps, fixed; the bullets above describe the intent
correctly but several undersold what enforced it. Restated where it
matters, not rewritten wholesale.]**

- **D1 — the "in `Mode::Normal`" scope clause is now actually enforced.**
  `handle_mouse` had exactly two mode-aware early returns (Copy mode,
  a C12 modal) and `Mode::Scroll` was neither, so a drag during Scroll mode
  fell all the way through to `handle_native_selection` and quietly
  selected and copied — while the keypress-clear stayed `Mode::Normal`-gated
  and so couldn't clean up the result until Scroll mode was left. Fixed at
  the one call site (`main.rs:764`, inside `handle_mouse`):
  `matches!(app.mode, Mode::Normal)` joins the existing `!pane.collapsed &&
  state.proto == MouseProto::None` gate. **Chosen over the alternative**
  (permit it in Scroll, since a frozen view is exactly what Copy mode
  already lets you select from via its own U9 Alt+c handoff) because the
  contract always said Normal-only, and permitting Scroll would have needed
  the clear path widened too, for a mode nothing in the brief asked native
  selection to cover. Pinned by
  `main.rs::tests::a_drag_in_scroll_mode_does_not_select`.
- **D3 — "found nothing" is tracked, not inferred from geometry.**
  `select_word_at` (`app.rs:2227`) used to degrade "no word under the
  pointer" to a same-point (anchor == cursor) selection — indistinguishable
  from a genuine **one-character** word, which also has anchor == cursor by
  construction. Release logic that read "anchor == cursor" as "nothing
  selected" therefore silently dropped double-clicks on `a`, `$`, or any
  other 1-char token. Fixed two ways that compose: `select_word_at` now
  clears `self.selection` outright (`None`, not a degenerate point) when
  `word_bounds_at` finds nothing, so `Some` afterward already means "there
  is something here" regardless of its width; and the release decision
  (`App::release_native_selection`, `app.rs:2352`) keys "nothing selected"
  off `sel.dragging` — true only for an un-upgraded click/shift-click that
  never moved — rather than off the resulting range. Pinned by
  `select_word_at_a_one_char_word_is_a_real_selection_not_nothing_found`
  and, end to end, `main.rs::tests::double_click_on_a_one_char_word_still_selects_it`.
- **B1 — word lookup is grid-CELL-indexed, not char-indexed.**
  `row_text`/`grab_text` (P15) drop wide glyphs' continuation cells, so the
  extracted string's char positions and the row's cell positions diverge
  the moment a row holds one; `word_bounds_at` used to index that string
  directly with the caller's cell column, so a row like `日本語 hello`
  walked off by however many extra cells the glyphs ate and could select
  (and, on release, copy) the wrong word entirely. The same flaw was
  already latent in `find_url_at`/`url_at`'s single-row case (documented
  and accepted there — SPEC-ux U19 — because a wrong-URL click just fails
  silently) but this contract turns it into silently copying the *wrong
  text*, which is worse. Fixed at the shared level: `word_bounds_at`
  (`app.rs:4756`) now converts the incoming cell column to a char index via
  the new `cell_to_char` (`:4727`) before walking, and `select_word_at`
  converts the resulting char bounds back to cell columns via `char_to_cell`
  (`:4742`) — including a trailing wide glyph's full cell span, not just its
  first column. `find_url_at`'s own char-based slicing needed no change
  (the bounds it receives were always meant to be char-space; only what fed
  them in was wrong). `find_url_at`'s single-row callers are a pure
  correctness improvement as a result; `url_in_wrapped_rows`'s own
  cross-row char-count accumulation (a *different*, already-documented gap,
  same U19 citation) is untouched — out of this contract's scope.
  `ports::fakes::FakePane::grab_text` also grew a `slice_cells` mirror of
  the real backend's cell-indexed extraction (it used to ignore columns
  entirely and return the whole row, which is exactly why no test could
  have caught the bug it was hiding). Pinned by
  `select_word_at_accounts_for_a_wide_glyph_earlier_in_the_row`.
- **S3 — a selection could be extended/copied by a pane it didn't belong
  to.** The Alt+click-URL branch (`main.rs`, inside `handle_mouse`) latches
  the clicked pane and, on a hit, `return`s *before* `App::on_click` runs —
  so the cross-pane clear this contract's own "Clearing" bullet relies on
  never fired for that gesture. A drag-select left lit in pane A, then an
  Alt+click that opens a URL in pane B, then a further drag/release on B's
  now-latched gesture, could extend or re-copy pane A's stale selection
  under a pane B event that had nothing to do with it. Fixed two ways: the
  URL-hit branch now clears `app.selection` itself before opening the
  browser (a URL open is its own complete gesture, same as any other
  click); and `handle_native_selection`'s `Drag`/`Up` arms are now guarded
  on `app.selection.is_some_and(|s| s.pane == pane.id)`, so neither can
  touch a selection that isn't the current gesture's own regardless of how
  it was left stale. Pinned by
  `alt_click_opening_a_url_clears_a_different_panes_selection`.
  **Decided (the "uncontracted path" question): an Alt+click that *misses*
  a URL falls through to the ordinary click path unchanged** — Alt is
  roost's own layer and a miss most plausibly means the user simply
  clicked there (Alt incidental or misjudged), so it starts a selection
  like any other left press. No special-casing needed or added.
- **S4 — a double-click's own release no longer fires a premature copy.**
  A real triple-click arrives as three full press/release pairs, and the
  2nd click's own `Up` used to commit its word selection immediately —
  before the 3rd click had a chance to arrive and supersede it with the
  whole line — so every triple-click copied the word, then the line, a
  heartbeat later: two clipboard writes and two flashes for one gesture.
  Fixed with a short, explicit defer: `App::release_native_selection`
  (`app.rs:2352`) stages a double-click's release instead of committing it
  (`self.pending_copy`, keyed off `App::click_count`'s own last-recorded
  count — nothing else needs to know a click happened twice) for exactly
  `DOUBLE_CLICK_INTERVAL`; `App::due_copy` (`:2378`), polled once per
  event-loop iteration by the composition root (not gated on any
  particular event, since it fires on a deadline), reports the staged text
  once that window passes **and** `self.selection` still matches what was
  staged — a 3rd click overwriting it with the line, or any other clear,
  silently cancels the stage. A plain click/drag and a triple-click's own
  release are never staged (nothing above triple-click exists to wait for;
  shift-click never touches `click_count` at all), so they still commit on
  the spot. Pinned by
  `core::app::tests::release_native_selection_stages_a_double_click_and_due_copy_fires_it_later`
  and `a_third_click_cancels_the_staged_double_click_copy`; end to end,
  `main.rs::tests::double_click_selects_the_word_and_stages_the_copy_on_release`
  asserts the release does *not* flash immediately.

**[Amended 2026-08-07, exit UX audit F5 + code review — what "still
matches" meant was wrong twice over]** "`due_copy` reports the staged text
once that window passes **and** `self.selection` still matches what was
staged … any other clear silently cancels the stage" is withdrawn. That one
check conflated two unrelated things, both bugs:
- **Grid staleness (code review).** `self.pending_copy` staged coordinates
  (`pane, anchor, cursor`), and `due_copy` re-extracted the text from the
  **live** grid when the deadline passed. A streaming pane can scroll or
  overwrite those cells inside the 500ms window, so the clipboard could
  silently get whatever now occupies them — not the word actually
  double-clicked, and no error, no wrong-looking flash, nothing.
- **Silent drop on an unrelated keypress (F5).** "Any other clear" included
  a keypress clearing `self.selection` (C29's own "any click or keypress
  clears" rule, above) — so double-click a word, then type anything at all
  within the window, and the clipboard silently didn't update. A keypress
  has nothing to do with whether the word that was actually double-clicked
  should still land; the check couldn't tell a keypress from a real
  supersession (a 3rd click) because it never looked at *why*
  `self.selection` had changed, only *that* it had.

**Fixed by moving both jobs to where they actually belong.** `PendingCopy`
is now `(text, fire-at)` — `App::release_native_selection`'s double-click
branch grabs the text immediately via `finish_native_selection` and stages
*that*, so there is nothing left to re-derive from the grid later.
Supersession is detected at the only place a real one can originate — a
new mouse release — not at fire time: every call to
`release_native_selection` clears `self.pending_copy` unconditionally
before doing anything else, so a 3rd click's own release (which commits
its wider selection immediately, per the bullet above) cancels the
narrower stage as a direct side effect of *being* a new release, the same
way a plain click or drag elsewhere would. `due_copy` no longer reads
`self.selection` at all — once the deadline passes, the staged text fires,
full stop. Pinned by
`an_unrelated_keypress_does_not_cancel_the_staged_double_click_copy` (F5)
and `the_staged_copy_is_fixed_at_release_time_not_re_read_from_a_later_grid`
(review); `a_third_click_cancels_the_staged_double_click_copy` now drives
the 3rd click's own release for real rather than only mutating
`self.selection` by hand, so it continues to pin the supersession case
under the new mechanism.

- **B2 — the test suite no longer touches the operator's real clipboard or
  browser.** `infra::clipboard::copy` and `infra::open::open_url` are both
  reached by this contract's own `handle_mouse`-driven tests (that's the
  point — they exercise the real composition-root path), and doing so for
  real left a `pbcopy` write sitting in the *operator's own* system
  clipboard after every `cargo test` run (confirmed via `pbpaste`) and OSC
  52 bytes leaking into captured stdout that CI's `--show-output` prints
  straight into the log; `open_url` would have spawned the operator's
  actual browser the moment a test exercised the Alt+click-URL path (S3's
  own regression test above is the first to). Both now have a `#[cfg(test)]`
  twin that touches neither: `clipboard::copy` deterministically reports
  `Native` so a test can pin the *exact* flash text (`copied N chars`)
  instead of whichever real channel wins on the machine running it —
  before this, `assert!(flash().is_some())` passed identically for
  `copy failed`, pinning nothing about U14. Every `...copies_on_release`
  test above now asserts the literal flash string.
- **Test list, current as of this amendment (`main.rs`'s test module unless
  noted):** `dragging_over_a_native_pane_selects_and_copies_on_release` ·
  `a_drag_in_scroll_mode_does_not_select` (D1) ·
  `double_click_selects_the_word_and_stages_the_copy_on_release` (S4) ·
  `double_click_on_a_one_char_word_still_selects_it` (D3) ·
  `triple_click_selects_the_line_and_copies_on_release` ·
  `shift_click_extends_the_selection_and_copies_on_release` ·
  `alt_click_opening_a_url_clears_a_different_panes_selection` (S3) ·
  `a_plain_click_clears_a_lingering_native_selection` ·
  `a_mouse_aware_pane_is_untouched_by_native_selection_gestures`; in
  `core::app`'s own test module, `click_count_cycles_through_1_2_3_and_wraps_on_a_4th`,
  `select_word_at_grabs_the_whitespace_delimited_word`,
  `select_word_at_a_one_char_word_is_a_real_selection_not_nothing_found` (D3),
  `select_word_at_accounts_for_a_wide_glyph_earlier_in_the_row` (B1),
  `select_line_at_grabs_the_whole_row`,
  `extend_selection_to_extends_the_live_selection_or_starts_fresh`,
  `finish_native_selection_extracts_text_but_leaves_the_highlight_lit`,
  `release_native_selection_stages_a_double_click_and_due_copy_fires_it_later` (S4),
  `a_third_click_cancels_the_staged_double_click_copy` (S4),
  `on_click_clears_a_selection_that_belongs_to_a_different_pane_only`,
  `any_keypress_clears_a_normal_mode_selection_but_copy_mode_is_exempt`,
  `entering_copy_mode_clears_a_lingering_native_selection_first`. The C17
  gates' own fixture (`ui::render::tests::chrome_buffers`) gained a
  `"lit text selection"` case, since none of its entries had ever set
  `app.selection` and every §2 mechanical gate was therefore vacuous over
  `highlight_selection`'s output for *both* tenants of C17's modifier-only
  rule (Copy mode and this contract) — not a regression this PR
  introduced, but one it made newly worth noticing. The SUPER guard:
  `ui::input::tests::super_modified_chars_are_swallowed_not_forwarded`.
  The mouse-capture subset:
  `ui::mouse::tests::mouse_capture_sequences_are_the_1000_1002_1006_subset_symmetric_in_reverse`.

**[Amended 2026-08-08, selection-freeze — presentation pins for the gesture,
content still doesn't]** The "Contracted: tracks grid coordinates, not
content" bullet's own closing claims are withdrawn for exactly the gesture
window; everything else about coordinate-based selection stands unchanged.
Restated: **`Selection` still tracks grid coordinates, not content** —
`(pane, anchor, cursor)`, untouched — and this is still not content
anchoring, which was assessed for this amendment too and rejected on the
same grounds as before: the vendored parser's scrollback
(`vendor/vt100/src/grid.rs:13-15`) carries no row identity, and a resize
mid-drag would leave a content anchor nothing to reattach to. What changes
is *what `presented()` those coordinates are read against* while a
native-selection gesture (mouse-Down to mouse-Up) is in flight: the pane
holds the frame it was presenting at the gesture's `Down`, the same way P1
already holds one for an open synchronized-output bracket —
`PtyPane::presented` (`infra/pty.rs`) now checks a per-gesture snapshot
ahead of the sync-view veneer, so `screen()` (the blit) and `grab_text`
(the copy) read the *identical still frame* for the whole gesture. The
highlight the user aimed at is provably the text that lands on the
clipboard — closing the same class of defect `PendingCopy` (app.rs:195-204)
already closed for a ≤500ms grid re-read; a drag runs longer than that.

"A content-tracking alternative would need the selection to pin a snapshot
of the grid rather than read the live one, which nothing else in roost's
selection model does" no longer holds — something now does, but it is not
content tracking: the snapshot is keyed to the *gesture*, carries no row
identity, is dropped the moment the gesture ends, and `Selection` itself
never gained a content pin. The withdrawn sentence was the argument for
*why not build this*; it is gone because the brief changed, not because the
argument was wrong at the time.

**Between release and the next interaction** (the highlight-stays-lit
window this contract already documents) is untouched by this amendment —
no freeze applies there, so a lingering post-release highlight can still
visually drift if the pane prints before the next click or keypress,
exactly as before.

**[Amended 2026-08-08, design audit D1/D2 — the paragraph below originally
described release as living entirely inside `handle_native_selection`
(`Up` unfreezes unconditionally, a wheel tick drops it, the stale cap
covers the rest). That was wrong: `handle_mouse` only ever calls
`handle_native_selection` under several gates — `!collapsed`,
`MouseProto::None`, `Mode::Normal`, the latched pane still resolving in
`rects` — and copy mode or a modal owns the mouse outright before any of
those gates are even reached. Five real paths (entering copy mode or a
modal mid-drag; entering Scroll/Search mode; a tab switch or C31 cross-tab
focus move; the pane collapsing; the pane's app enabling SGR mouse) each
let the `Up` arrive without `handle_native_selection` ever running again,
leaving the pane frozen with nothing to release it short of the 30s cap —
a worse failure than the bug this contract fixes. Restated below.]**

**Held and released, precisely:** `App::mouse_latch` (P20) is the
gesture's lifetime — no second "gesture in progress" flag is introduced,
and release is tied to *that* lifetime everywhere it ends, not to
`handle_native_selection` running again. Every `Down` inside
`handle_native_selection` (`main.rs:882`) still freezes the latched pane
(`PaneBackend::freeze_view`) before touching `Selection`. Release has
exactly three sites, each covering a distinct way the latch's own life
ends, funneling through `App::release_mouse_gesture` (clears the latch and
unfreezes together) or the narrower `unfreeze_view` alone where the latch
is already clear or unfreezing any earlier would race an in-flight
extraction:
- **The ordinary `Up`, resolved to a pane** — `handle_mouse` unfreezes it
  unconditionally right after `handle_native_selection` returns, whether or
  not that call actually ran. *After*, not before: a completing gesture's
  own `grab_text` still has to read the frame it started on. This one call
  is what covers Scroll/Search mode, an SGR flip and the pane collapsing —
  none of those stop the pane from being *found*, only from being
  *selected from*, so the release still runs even when the selection logic
  doesn't.
- **An orphan `Up`**, whose latched pane no longer resolves in
  `rects`/`display_rects` (a tab switch, or C31 cross-tab focus, mid-drag —
  both are active-tab-only): `release_mouse_gesture` fires the moment
  `pane` comes back `None`, before anything downstream ever gets a chance
  to try.
- **Copy mode or a modal taking the mouse mid-drag**: both are early
  returns at the very top of `handle_mouse`, ahead of the P20 latch code
  entirely, so neither of the other two sites can ever run for that
  gesture again. `release_mouse_gesture` fires there instead, on the first
  mouse event `handle_mouse` processes once the mode changed — bounded by
  the gesture's own eventual `Up` (the button is still physically held),
  not by some later, unrelated event.

`GESTURE_FREEZE_STALE_CAP` (checked lazily inside `presented()`, exactly
the way `SYNC_STALE_CAP` already is) is now what it was always meant to be:
a backstop for whatever none of the three sites above catches — a
genuinely stuck `Up` — not the mechanism that makes release happen in the
ordinary case. A resize mid-gesture (design audit D2) doesn't release
cleanly, it invalidates: the frame is wrong at the OLD size, not merely
stale, so `PtyPane::resize` clears `gesture_freeze` itself rather than
waiting for any of the three sites above. And a pane already scrolled into
its own history when the gesture starts is left live, not frozen, for the
identical reason `sync_presented` already defers to live there:
`Screen::snapshot` always resets to the live tail
(`vendor/vt100/src/grid.rs:49-53`), so freezing it would silently yank a
history-reading user to the tail rather than protect anything — and banked
rows are already immutable, so there is nothing moving under a
scrolled-back drag to protect against in the first place.

**`roost read` sees live content, deliberately** (SPEC-parity P1 gap,
closed in the same design audit): a control client polling a pane is not
the human the freeze protects. `ReadMode::Screen` (`core::app::ctl_read`)
now calls the new `PaneBackend::read_screen_text` instead of `grab_text` —
identical coordinates, and it still honors the P1 sync-output veneer (a
torn mid-redraw frame is wrong for both consumers), but it reads
`PtyPane::presented_live` — `presented()`'s `sync_presented` half, minus
the gesture-freeze check — rather than `presented`. `grab_text` itself is
untouched: copy-mode and native-selection extraction still go through the
frozen `presented()`, exactly as the rest of this contract requires.

**Must-not-break, reconfirmed:** an SGR pane never reaches
`handle_native_selection`, so it is never frozen. Copy mode's
`handle_copy_mouse`, the seam-drag path, and wheel routing for every
*other* pane are untouched — `release_mouse_gesture`'s two extra call
sites sit in `handle_mouse` itself, immediately ahead of
`handle_copy_mouse`/`handle_modal_mouse`, and touch neither function's own
body. Double-click word, triple-click line and shift-click-extend all read
through the same frozen `grab_text`/`row_text` path a plain drag does,
since the freeze arms before any of them run — one freeze, not a special
case per gesture shape.

**Tests, current as of the design audit:** the mutation pin,
`main.rs::tests::output_banked_mid_drag_does_not_change_what_release_copies`
(fails without the freeze, by construction); the wheel-drop,
`a_wheel_tick_mid_drag_drops_the_freeze`; the staleness cap,
`infra::pty::tests::a_lost_mouse_up_does_not_freeze_the_pane_forever`; the
five D1 abandonment paths, each mutation-checked against the specific
release site it pins — `main.rs::tests::entering_copy_mode_mid_drag_releases_the_freeze`,
`entering_scroll_mode_mid_drag_releases_the_freeze`,
`a_tab_switch_mid_drag_releases_the_freeze`,
`a_pane_collapsing_mid_drag_releases_the_freeze`,
`an_sgr_flip_mid_drag_releases_the_freeze`; D2's
`infra::pty::tests::a_resize_mid_gesture_drops_the_freeze`; and the read
bypass, `main.rs::tests::roost_read_screen_mode_bypasses_the_gesture_freeze`.
Every pre-existing C29 and `PendingCopy` test passes unmodified — none of
them mutate a pane's content mid-gesture, so a frozen read and a live one
were already indistinguishable to them.

---

### C31 — Cross-tab directional focus at a tab's edge (`Alt+←/→`) — [Added 2026-08-07, client request]

**Current:** `Alt+←↓↑→`/`hjkl` (`Action::Focus`, §8 row 3) move focus
spatially within the active tab via `layout::neighbor` and stop dead at an
edge — `neighbor` returns `None` when nothing lies that way, and
`App::focus_dir` leaves focus exactly where it was. Reaching a pane in
another tab has always taken a separate chord first — a digit or
`Alt+m`, then an arrow to reach a specific pane once there, or
`Alt+a`'s direct ring jump (C19) — never a plain arrow at an edge.
`neighbor`'s own edge/overlap/gap/id tie-break predates this document's
contract numbering and stays uncontracted here — C31 governs only what
happens at the edge it already finds, not the in-tab pick itself.

**Client's ask, verbatim:** "Alt+ arrow keys are used to navigate b/w panes.
If the user is on the last or first pane can we move to previous or next
tab panes?" — conditional on reliability: a navigation key that occasionally
lands somewhere surprising is worse than one that predictably does nothing.

**Target:**
- **`Left`/`Right` only.** `Up`/`Down` keep today's dead end,
  unconditionally. Tabs are roost's own horizontal axis — the strip draws
  left to right, and `Alt+m`/`Alt+Shift+m` (U7, C2) already step it horizontally
  — so only the two keys sharing that axis pick up tab-switching semantics.
  All four would hand a vertical split's `Up`/`Down` a surprise nothing
  about them advertises. `h`/`l` inherit the behavior for free (§8 row 3:
  they dispatch the identical `Action::Focus(Dir::Left/Right)`); `j`/`k` do
  not, for the same reason `Up`/`Down` don't.
- **Trigger: `layout::neighbor` returns `None`, *and* the focused pane is
  genuinely at that edge.** `App::focus_dir` tries the in-tab move first
  and falls through to C31 (`focus_dir_cross_tab`) only when that comes
  back empty — within-tab behavior, stack tie-breaks included, is
  untouched byte-for-byte. `neighbor == None` alone is **not** sufficient:
  a pane spanning the tab's full width (a row sitting above or below a
  split row) has no Left *or* Right neighbour either — `neighbor`'s gap
  check excludes every other pane on both sides at once, since nothing can
  sit beside a full-width rect, regardless of row — but such a pane is not
  meaningfully "the last or first pane" the client asked about; it's
  reachable from a single pane as `Alt+n`, `Alt+o`, `Alt+n`. C31
  additionally requires the focused pane's rect to **not** span the tab
  body's full width, unless it is the tab's only pane (`rects.len() == 1`)
  — which necessarily spans the full width *and* height, and is
  unambiguously both ends at once: the ordinary single-pane-tab case every
  other test here relies on.
  **[Amended 2026-09-02 — the exemption was too narrow, and it trapped
  people.]** `rects.len() == 1` is the right *idea* stated as the wrong
  *test*. What earns the refusal is not "this pane is alone" but "this tab
  has somewhere better to cross from" — and where it does not, the refusal
  has nowhere to send the user. Every pane in the tab spans the full width,
  so every one of them is refused, and `Alt+←`/`→` is dead in that tab with
  no pane to move to first. Reported from a fully stacked tab; the shape
  that shows it was never about stacks is `Alt+n` `Alt+o` — **two panes,
  top and bottom**, as plain a layout as roost has, and equally trapped.
  C42's ladder endpoint and `Alt+g`'s all-stack reach it too.
  So the condition is now: refuse when the focused pane spans the full width
  **and some other pane in the tab does not**. The single-pane tab is that
  same condition with one pane in it (nothing narrower exists), so the
  original exemption is subsumed rather than special-cased, and the layout
  this bullet was written for — a full-width row above a split row —
  still refuses, because the split row's panes are narrower. Pinned by
  `a_tab_with_no_horizontal_structure_still_crosses_from_any_of_its_panes`
  (all three shapes, every pane, both directions),
  `a_full_width_pane_still_refuses_while_the_tab_has_a_narrower_one` (the
  other half, so the fix cannot become "everything crosses"), and
  `alt_arrow_leaves_a_fully_stacked_tab_through_a_real_terminal` at a real
  PTY.
- **Motion continues in the same direction.** `Right` at the right edge
  switches to the **next** tab and focuses its **leftmost** pane; `Left` at
  the left edge switches to the **previous** tab and focuses its
  **rightmost** pane. The destination pane is nearest the edge just
  crossed, not wherever that tab was last left — deliberately **not** U11
  rule 2 (`tab_focus`, C2, amended same date): "keep going right" means
  arriving next to where you came from.
- **Leftmost/rightmost is geometric, read from the destination tab's own
  freshly-computed rects** (`layout::compute_rects` over that tab's layout
  and the current `body_area()` — pure, and correct for a tab that has
  never been active, the same fact C28's own destination-geometry check
  already relies on) — never pane-id order, never tree/DFS order.
  **Tie-break: smallest x, then `collapsed == false`, then smallest y, then
  smallest pane id** for leftmost; **largest x**, same remaining keys, for
  rightmost (`edge_pane`, `app.rs`). Every member of a stack sits at the
  same x, so the collapsed-vs-expanded key is what decides there: the jump
  lands on the member the stack already had **expanded**, never a
  collapsed 1-row title bar — a navigation key must not rearrange a tab
  you haven't looked at yet, so `expand_in_stacks` (below) comes back a
  no-op instead of silently collapsing whatever was open. The x/y pair is
  `layout::neighbor`'s own tie-break vocabulary, and id-as-last-resort
  borrows that same function's own rule, for whatever residual tie
  position still can't break — a real tab never produces one (two panes at
  the same x, collapsed-state, *and* y), but the guard costs one field and
  keeps the pick total rather than order-dependent.
- **Wraps at both ends**, exactly like `Alt+m`/`Alt+Shift+m` (`step_tab`):
  `next = (active_tab + delta).rem_euclid(tab_count)`. Two ways to move
  between tabs disagreeing about hitting an end would be its own bug.
- **Only when more than one tab exists.** Below two tabs this is exactly
  today's dead end — C31 declines to run at all rather than, say,
  refocusing the same tab's own leftmost pane, which would be a different,
  unrequested behavior change.
- **Zoom (C21), decided:** a cross-tab jump exits zoom unconditionally —
  the same "any (real) tab change exits zoom" rule `go_to_tab`/C28/C19's
  cross-tab `Alt+a` already follow (C21's own amendment, same date), applied
  here rather than invented fresh. A same-tab arrow move is unaffected — it
  still keeps zoom (zoom follows focus), exactly as before.
- **The float (C22) is untouched.** `focus_dir` already returns before
  reaching C31 whenever the float is focused (rule 2: leaving it via a
  directional key returns to `prev_focus`; `dir` doesn't apply to it) — and
  by rule 1 ("shown ⇒ focused") the float cannot be shown while unfocused,
  so C31 can never observe it shown. No float-handling code was added for
  this contract; the existing guard is the whole story.
- **`expand_in_stacks` still runs at the destination**, exactly as the
  within-tab path already does. Typically a no-op now that the tie-break
  above lands on the expanded member itself; it remains the fallback that
  guarantees the landing pane is visible — never a collapsed 1-row title
  bar, the "surprising" outcome the brief's caveat rules out — in whatever
  case isn't a clean x-tie.
- **Chrome:** §8's key table row 3 and C15's matching overlay row both gain
  the same parenthetical (`←`/`→` continue at an edge) so the in-app keymap
  doesn't fall out of step with the canonical table —
  `every_bound_chord_is_documented_in_the_keymap` is chord-level and would
  have passed either way, the same class of gap the 2026-08-06 C15
  amendment closed for mouse verbs.
- **Persistence and PTY sizing are unremarkable.** The switch runs inside
  `App::apply`, whose trailing `relayout()`/`save()` (unconditional, after
  every action) is exactly what `Alt+m`/`Alt+Shift+m`/`Alt+1..9` already rely on;
  C31 adds no save/resize call of its own, so it cannot disagree with them
  about when a switch persists.
- **`go_to_tab` is deliberately not reused for the switch itself.** It would
  set focus once, to `tab_focus_target`, and C31 a second time right after
  to the geometric pick — and `App::set_focus` (P10) reports a real, if
  momentary, focus transition on every call that changes the id, so that
  shape would send a spurious `CSI O`/`CSI I` pair to a pane that was never
  really focused. C31 instead inlines `go_to_tab`'s own skeleton — C21 zoom
  exit, C22 float hide (a no-op here, kept for parity with every other tab
  switch), U11's `remember_tab_focus` bookkeeping, `spawn_active_tab` for a
  never-visited destination — and calls `set_focus` exactly once, with the
  geometric target.

**[Amended 2026-08-07, exit UX audit F7 — both refusals now flash]** The two
`focus_dir_cross_tab` early returns above — a full-width pane, and fewer
than two tabs — were silent: `Alt+→`/`Alt+←` visibly does something at every
other edge in the same tab (a spatial move, or now a tab switch), so a
no-op indistinguishable from an unbound key read as broken rather than
deliberate. roost's own rule, stated plainly elsewhere in this document, is
that every no-op flashes; these two were the last cross-tab-focus refusals
that didn't. Fixed with one line each, no behavior change: the full-width
case flashes `full-width pane — nothing to cross into`; the fewer-than-two-
tabs case flashes `only one tab`, reusing `move_pane_to_tab`'s identical
wording for its own `n < 2` refusal (C28) so the two read as one rule
rather than two independently-worded ones. `Up`/`Down`'s dead end is
unchanged and stays silent — it predates C31 and is a different,
unaudited surface. Pinned by amending
`cross_tab_focus_is_a_no_op_with_only_one_tab` and
`cross_tab_focus_ignores_a_full_width_pane_thats_not_the_tabs_only_pane` to
assert the flash text, and `cross_tab_focus_never_fires_for_up_or_down` to
assert the *absence* of one — pinning the fix's scope, not just its effect.

**Unit tests (`core::app::tests`):**
`cross_tab_right_edge_lands_on_next_tabs_leftmost_pane` ·
`cross_tab_left_edge_lands_on_previous_tabs_rightmost_pane` ·
`cross_tab_focus_wraps_at_both_ends` ·
`cross_tab_focus_is_a_no_op_with_only_one_tab` ·
`cross_tab_focus_never_fires_for_up_or_down` ·
`cross_tab_focus_ignores_a_full_width_pane_thats_not_the_tabs_only_pane`
(the `Alt+n`, `Alt+o`, `Alt+n` repro, both directions, plus a same-layout
sanity check that a genuine edge still crosses) ·
`cross_tab_focus_lands_on_the_already_expanded_stack_member_at_the_destination` ·
`cross_tab_focus_exits_zoom_like_any_other_tab_change` ·
`float_rule2_focus_dir_does_not_cross_tabs_even_with_more_than_one`.

### C32 — Pane parking notes (`Alt+Shift+n`) — [Added 2026-08-15, client request]

**Problem being solved:** the client runs many tabs and panes, sleeps the
laptop overnight (no quit, so no resurrection moment), and returns having
forgotten each pane's *intent* — the transcript survives in the agent's own
session, but "waiting on CI", "this approach is a dead end", "next: rebase
then merge" do not. A name (`Alt+r`) says what a pane *is*; a note says
where it *stands*.

**[Amended 2026-08-21 — the note gets an edge of its own.]** The note segment
leaves the corner badge (which itself became C4's identity title on the top
border, same date) and becomes the **note title** on the pane's **bottom
border**, left-aligned. Reveal-on-visit is untouched and is the whole reason
the two forms still exist: `" ¶ {headline} ({age}) "` on the focused pane,
a bare `" ¶ "` (or `" ¶⋮ "`) everywhere else — presence everywhere, content
only where you are looking. Text, styling (`ink()` headline, `quiet()` age),
the `¶⋮` body marker, and the missing-timestamp rule all stand.

Its own edge rather than a segment of the identity title, for two reasons.
It is the widest thing roost draws about a pane, so sharing the top border
would make it the first casualty of a narrow one — which is exactly what it
was as the badge's tail, clipped before it could be read. And it answers a
different question from a name: *what did I park here*, not *what is this*.
On its own edge a 28-column pane shows `└ ¶ waiting on schema review ┘`
whole, which no width of pane ever did before. The marker is fixed and the
headline absorbs a narrow border, so a note never becomes invisible — it
degrades to a `¶`.

Collapsed stack rows (C8) and the roster (C27) are unchanged: they are their
own single-row chrome with no border to hang a title on, and they keep the
`¶` presence marker exactly where it was.

**Data:** `PaneSpec.note: Option<String>` + `PaneSpec.noted_at: Option<u64>`
(unix seconds), both `#[serde(default)]` — a pre-C32 `workspace.json` loads
untouched, and notes ride the existing auto-save/`Alt+u` clauses with no
machinery of their own (pinned: `note_and_timestamp_roundtrip_through_json`,
`pre_note_workspace_json_loads_with_no_note`,
`undo_reopens_a_closed_pane_with_its_note`). Newlines separate lines; the
**first line is the headline** every chrome surface shows; `noted_at` is
set/cleared with `note`, never separately. The float (C22) takes a note
in memory like any pane and loses it at quit — its own documented scope.

**Editor (the C12 modal, C13's multi-line sibling):** `Alt+Shift+n` opens
`Mode::Note` on the focused pane — prefilled, point at the very end,
because the editor is also the note's *reader* (peek, edit and clear are
one surface; there is no separate viewer). `pane` is pinned at open time.
The dialog is Rename's 44-col width, one content row per line, growing to
`NOTE_MAX_LINES` (8) instead of scrolling; heading `" pane note "`; all
text `ink()`; the `▏` caret rides the cursor row (C13's `rename_field`).
Keys: C13's editing vocabulary on the current row (insert at point,
Backspace/Delete, Home/End per row, Ctrl+U/W), plus the vertical half —
`↑↓` move rows keeping the column where they can; **`←→` flow across line
ends** (the one C13 clamp deliberately relaxed: in a multi-line field a
line edge is a seam, not a wall — C13's "never wrapping" stays true of
single-line Rename); `Shift+↵` splits the line at the point (`Ctrl+↵`
synonym; **`Alt+↵` is claimed inside this dialog only** — Terminal.app
delivers Shift+↵ as exactly Alt+↵'s bytes, so on that terminal it is the
only spelling of "newline" the dialog will ever receive; every other Alt
chord still cancels out to the global bindings, U18's toggle-off
included); and Backspace/Delete at a line edge join across it. At the cap
a split is swallowed whole. A line wider than the field clips at the
field's right edge, caret included — C13's own field behavior, accepted:
the 8-line dialog interior is the honest bound of a parking note, not a
document editor. `↵` commits; `Esc` and the U18 toggle-off cancel; an
outside click is **swallowed, not a cancel** (C12's Rename carve-out,
extended — see C12's 2026-08-15 amendment).

**Commit:** whole-text trim (stray blank edge lines die — a headline of
`""` would make the badge segment a lie; interior blanks are the author's
call); empty result clears `note` + `noted_at` both — **the clear gesture
is the acknowledgment**, C32's staleness hygiene alongside the age tag;
non-empty sets both and re-stamps. Either way the workspace saves
immediately (the overnight story is a disk write, not a mode).

**Display is reveal-on-visit, contracted in the surface contracts:** the
focused pane's badge reads its headline + age out; every other surface —
unfocused badges, collapsed rows, roster rows — shows only a `¶` presence
marker. No startup digest, no toast, no timer, nothing shouts: C4's
amendment holds the badge forms and the age-tag rule, C8's the row marker,
C9's the `NOTE` mode word and pair list, C12's the modal-family rules
(outside click swallowed; paste keeps newlines). Deliberately **not**
built: tab-level notes (entering a tab focuses its remembered pane, whose
note is the tab's de facto status line), roster note *text* (against
reveal-on-visit), and a `roost note <id>` control verb (natural follow-up;
the data model already supports it).

**Unit tests (`core::app::tests`):**
`note_types_splits_saves_and_stamps` ·
`note_opens_prefilled_and_an_empty_save_clears` ·
`note_esc_and_toggle_off_both_cancel_without_touching_the_note` ·
`note_alt_enter_splits_instead_of_opening_the_picker` ·
`note_line_cap_swallows_the_split` ·
`note_paste_weaves_lines_and_folds_overflow_at_the_cap` ·
`note_backspace_at_line_start_joins_lines` ·
`undo_reopens_a_closed_pane_with_its_note`; input:
`alt_shift_n_notes_pane_alt_n_still_news`. **Render:**
`focused_badge_reads_the_headline_in_ink_with_a_quiet_age` ·
`unfocused_badge_shows_only_the_note_marker` ·
`age_word_floors_to_the_coarsest_sensible_unit` ·
`noted_collapsed_row_marks_the_right_segment` ·
`note_dialog_height_tracks_its_line_count`; the §2 gates audit the new
spans through the `noted badges` / `noted collapsed rows` / `note dialog`
`chrome_buffers()` fixtures (C12's every-Mode-variant rule enforces the
fixture's existence at compile time).

**[Amended 2026-08-15 (later, client request) — the COMBINED pane editor:
one dialog, one chord, two fields.]** Alt+Shift+n retires after one
release (v0.1.7) and **`Alt+r` opens the pane's whole text surface**:
the name as the dialog's first row, the note lines under it —
`Mode::PaneEdit`, replacing `Mode::Note`. Motivation is chord economy (the
client's: "I don't want the user to remember so many shortcut keys"): the
feature's net new-chord count returns to zero, and `n` goes back to §8's
free pool. Everything the original C32 contracts — storage, badge/row
display, reveal-on-visit, U8 modal rules, the cap, paste, cancel semantics
— carries over unchanged except as follows:

- **The dialog** (` edit pane `, 44 cols, C12 frame): row 0 is the name,
  **`ink()` + `UNDERLINED`, padded edge to edge** so the underline reads
  as the field's rule and the name/note separator even when the name is
  empty (stack_header_text's fill trick; §2 modifier inventory amended);
  rows below are the note in plain `ink()`. Height = name + note lines +
  frame (`lines + 3`), growing to `NOTE_MAX_LINES`. The point opens at
  the **end of the name row** — Alt+r's rename muscle memory is preserved
  keystroke for keystroke.
- **The seam**: `↑↓` and `←→` cross between name and note freely (`←→`
  flow across row ends); **edits never cross** — Backspace at the note's
  top-left and Delete at the name's end are walls, so a typo can never
  merge status text into identity. Inside the note, edge joins still work.
- **Break key**: `Shift+↵` (`Ctrl+↵`/`Alt+↵` synonyms, the Alt-branch
  carve-out unchanged) *descends* from the name row into the note — the
  name is single-line by contract, so "line break" there means "start the
  note", making name → headline → body one continuous typing flow. The
  descent lands at the **headline's end** (the writing point of an
  existing line), never at the name's column carried over. On note rows
  it splits at the point, at the cap swallowed whole.
- **Commit** (`↵`, from either field): name trims, empty clears the title
  back to the adapter fallback (C13's rule, unchanged); note trims/joins
  as before — and **`noted_at` re-stamps only when the note text actually
  changed**. A pure rename can't refresh a stale note's age; an untouched
  note keeps its honest timestamp; an emptied note still clears both.
  Tightened from the original "every save re-stamps", which the combined
  dialog would have turned into a lie.
- **Paste** targets the row it lands on: name row = Rename's
  printables-only filter (a name paste never descends); note rows = the
  newline-preserving weave, unchanged.
- **Chrome renames**: mode word `EDIT` (C9), Normal-bar pair `Alt+r edit`,
  §8 row 12 covers both chords, the C15 keymap row reads "name + parking
  note (first line shows on the badge)". Config names: `edit_pane` is
  canonical; `rename_pane` and `note_pane` parse as aliases so v0.1.7
  config.json remaps keep working.

**Unit tests (combined):** `edit_pane_types_name_and_note_in_one_flow` ·
`edit_pane_opens_prefilled_and_an_emptied_note_clears_only_the_note` ·
`edit_pane_restamps_the_note_age_only_when_the_note_changed` ·
`edit_pane_esc_and_toggle_off_both_cancel_without_touching_either_field` ·
`edit_pane_alt_enter_breaks_instead_of_opening_the_picker` ·
`edit_pane_line_cap_swallows_the_split` ·
`edit_pane_note_paste_weaves_lines_and_folds_overflow_at_the_cap` ·
`edit_pane_edits_never_cross_the_name_note_wall` ·
`alt_shift_n_is_retired_back_to_the_free_pool` (input) ·
`hint_pairs_rename_word_differs_tab_vs_pane_editor` (render); the
`pane editor` chrome fixture replaces `note dialog`.

---

## 4. Pixel-idea translations (explicit)

Every px-only construct in the mockup, and its cell-level fate:

| Mockup construct | Translation |
|---|---|
| 2px `--color-accent` top edge on active tab (`:628`) | `▎` U+258E fg `accent()` as the active tab's first column (C2). A 1-row bar has no vertical edge to give; a left quarter-block preserves "one red edge marks the active tab". [Amended 2026-07-27] it now carries *more* weight than the mockup gave it: with the active tab's highlight fill gone, the marker plus full-strength ink is the whole signal. |
| 2px `--tui-red-dim` left edge on expanded stack member (`:662`) | left border column overpainted `▌` U+258C fg `accent_quiet()` (C7); half-block ≈ "thicker than 1px". |
| 1px borders throughout | `BorderType::Plain` single-line glyphs (C3, C12). |
| 6px pane gap + 12–14px pane padding (`:636, :639`) | **dropped** — border cells already separate panes; spending whole cell columns on gaps wastes terminal real estate. |
| ~9px vertical padding on every bar (tab/hint/stack-header/collapsed-row) → each renders ~2 text-lines tall in the browser | **height dropped — every bar stays exactly 1 row.** A terminal row is indivisible and scarce; reproducing the padding means adding blank rows, burning ~3 of ~44 rows for pure air. No serious TUI (tmux/zellij/lazygit) uses multi-row bars. The mockup's tall bars are a CSS-padding rendering artifact, not a directive. Only *horizontal* padding translates (space cells, C2 gutter); vertical does not. [Amended 2026-09-01] The **collapsed row** is now the one exception, and only where rows are plentiful: when its stack clears C6's boxed threshold the row grows into C8's 3-row `accent_quiet()` box; the 1-row bar remains its floor form and the other bars stay exactly 1 row. |
| ~18px horizontal tab padding (`:628`) | ~1 gutter cell per separator (C2, amended 2026-07-23) — the translatable half of the mockup's tab padding. |
| letterspacing (0.02–0.11em) | **dropped** — no letterspacing in a cell grid; spacing out characters by hand is a gimmick that breaks widths. |
| tab strip `border-bottom` / hint bar `border-top` (1px rules) | **dropped** — no spare rows. [Amended 2026-07-27] the `TAB_STRIP`/`BAR` bg steps that used to carry the separation are gone too (§2 background policy): the bars are set off by the ink weight of what is on them, and the panes' own top/bottom borders are the rules that survive. |
| stack header `border-bottom` (`:659`) | `Modifier::UNDERLINED` across the header row (C6) — the one place a rule translates to an attribute instead of a row. |
| `tui-pulse` opacity animation (1 → 0.28) | [Amended 2026-08-07, C5] a ten-frame braille spinner cycling in the glyph's own steady colour (`accent()`), not an opacity or colour flip — an opacity dip has no theme-safe cell equivalent, and a two-colour flip (the 2026-07-27 ANSI 9 ↔ ANSI 1 answer this row used to give) reads as "wants attention" rather than "busy"; a shape change carries "busy" without touching colour at all. |
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
  **[Amended 2026-07-28]** The formula is now `label + 8` (the C2 count-cell
  amendment); the rule itself is unchanged and was exercised by that change —
  renderer, width formula, `tab_at_x` and every offset test moved together,
  and the count cell's "always exactly one column" invariant is asserted from
  the mouse side against the renderer's own `tab_count_cell` rather than a
  hard-coded 1, so the two cannot drift apart later either.
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
- **[Amended 2026-07-27, SPEC-parity P9 — where a wheel tick goes]** Wheel
  routing has three answers, not two, and `mouse::route_mouse` decides all of
  them from one `PaneMouseState` (mouse protocol · alternate screen · DECCKM)
  so the decision stays in one pure, tested place:
  1. the pane's app speaks SGR mouse reporting → forward the encoded event,
     unchanged (asking for the wheel means wanting the wheel);
  2. no protocol, **alternate screen** (`?1049h`/`?47h` — `man`, `less`,
     vim) → forward `ALT_SCROLL_KEYS` = 3 Up/Down presses, encoded by the
     keyboard path's own `encode_raw` + `app_cursor_upgrade` (so a pager
     driven through `smkx` gets `ESC O A/B`). That grid has scrollback
     capacity 0, so roost-side scrolling there could only ever be a no-op —
     measured as zero bytes reaching the pane from six wheel events. This is
     the DECSET 1007 / `alternate-scroll` convention every peer settled on;
  3. otherwise → scroll roost's own scrollback by `WHEEL_LINES` = 3, exactly
     as before. The two 3s are deliberately the same number: one notch moves
     a pager by what it moves history by.
  The C15 help row (`wheel scrolls · click focuses · drag selects`) is
  unchanged and now true in more places. Clicks over a protocol-less pane
  remain roost's, alternate screen or not.

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
  predicate, C24 motion/anchor, C25 builders/fit; C27 row model, ring-order
  opening cursor, filter and window clamp): verify against the unit
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
without the socket — keep it out of golden frames. (2) **[Amended
2026-08-07, C5]** the Working glyph is wall-clock — assert its cell's symbol
∈ `theme::SPINNER_FRAMES` and its colour is exactly `accent()` (never a
second hue, since the colour pulse this bullet used to describe —
`pulsing cells ∈ {pulse_bright, accent}`, ANSI 9/1 — is retired), never an
exact frame. (3) Needs a PTY-capable CI runner (fine on macOS/Linux). (4)
Frame settling — poll-parse until stable rather than sleep, though a live
Working glyph never truly "settles" (`vt100::Screen::contents()` changes
every animation frame); scenarios that deliberately drive Working sample the
glyph directly rather than waiting on `settle()`.

**Effort:** ~1–2 days for the harness plus 3–4 golden scenarios.

**Verdict: feasible, but a follow-up — not a package in this build.**
Rationale: after C1 centralizes tokens, chrome correctness is dominated by
pure span/geometry construction, which the required inline unit tests (mouse
offsets, layout rects, the spinner frame function, corner-badge clipping) pin
far more cheaply and less flakily. The harness's real payoff is regression
armor for *future* chrome churn; building it now would serialize behind the
restyle it is meant to verify. It is recorded as its own decision item in the
plan (PLAN.md P6) with a revisit trigger: first post-restyle chrome
regression, or the next engagement that touches `src/ui/render.rs`.
**[Note, 2026-08-07]** a lighter-weight version of this harness has since
landed as `tests/chrome_theme.rs`, including the Working-spinner scenario
described in (2) above.

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
  3. **Clean exit under load:** quit mid-spew; the roost process exits
     within **2 s** of the first keypress and no child of it survives
     (the historical quit-freeze regression, ROADMAP "Alt+q freeze fix").
     Mid-spew the fleet is busy, so per U1 (SPEC-ux) the first Alt+q arms
     the second-press confirm — the harness's `quit_and_wait` answers it
     like a user would (second Alt+q after a short grace window); a quiet
     fleet still exits on the single press. [Amended 2026-07-27: U1's
     busy-quit guard made a lone mid-spew Alt+q deliberately insufficient;
     the 2 s budget is unchanged and now covers both presses.]
- Skipped on runners without a functional PTY (compile-time cfg or runtime
  skip with a printed reason) — same stance as the golden-frame assessment.

---

## 7. Spec gaps & deliberate exclusions

- **SPEC-GAP-1 — exit codes.** Mockup shows `pi · exited 130`; roost has no
  exit-code plumbing (`status.rs` `exited: bool`; `on_pty_exit` carries no
  code). Contracted as bare `exited` (C8). Optional follow-up: thread the
  child's exit status through `PaneBackend`/`on_pty_exit` into `StatusTracker`.
- **SPEC-GAP-2 — no tab-level Exited.** ~~`TabSummary` has no Exited variant,
  so a tab whose panes all exited shows a blank (Quiet) glyph.~~ **CLOSED
  2026-07-27** by SPEC-ux U13: `TabSummary::Exited` → `✕` `ACCENT_DIM`,
  ranked between Waiting and Quiet (C5 amended, same date).
- **SPEC-GAP-3 — collapsed-row task detail.** Mockup's `running build` is
  task-level detail roost doesn't have; the state-word table (C8) is the
  honest substitute.
- **SPEC-GAP-4 — paper assumption.** ~~roost does not repaint the terminal
  bg; the exact mockup look requires a terminal background near `#15120f`.
  Documented stance, no code (see §2).~~ **CLOSED 2026-07-27** by the
  theme-inheritance amendment. It stopped being a documented stance and became
  a bug the day a user on a light terminal found the active tab's label
  invisible — near-white ink on `Color::Reset` is white-on-white when
  `Color::Reset` is white. Replaced by: chrome derives all of its text from
  `Color::Reset` (the terminal's own validated fg/bg pair), spends ANSI 8 on
  structure only, reverses for attention rather than filling, and takes its
  one red from ANSI 1/9. roost no longer assumes *any* background, rather than
  assuming a dark one — and the assumption cannot creep back in, because the
  four mechanical gates named in C1 fail if it does.
- **Deliberately left out:** config/theme file (zero-config stands — and
  since 2026-07-27 there is nothing to configure: the theme *is* the
  terminal's) · curated light/dark palettes and any background-colour
  detection (`OSC 11` probing, `COLORFGBG` sniffing — inheriting is right for
  every theme, not just a light/dark binary, and survives a live theme switch
  with no machinery at all) · tab-overflow scrolling (ROADMAP item;
  C2 clips honestly) · any restyle of program output · golden-frame *color
  scenarios* in this build (§6 — the harness foundation itself now ships with
  the firehose gate) · letterspacing/gap/padding emulation (§4).
- **[Added 2026-07-22, fleet features — deliberately left out:]**
  float persistence across restarts (scratch is ephemeral, C22) · feed
  persistence / feed filtering / feed search (200-entry ring is the whole
  product, C20) · sub-2 s status-transition granularity in the feed (single
  tick-diff source beats double-reporting, C20) · per-tab zoom flags (one
  app-level bool, C21) · ~~copy-mode scrollback paging~~ [superseded
  2026-07-27 by SPEC-ux U9 — PgUp/PgDn view paging shipped, C24 amendment;
  the visible-grid *extraction* limit stands] · layout-cycle undo (C25) · a close-whole-tab
  gesture (C26 — tabs die by last-pane close only) · a TUI broadcast key
  (fat-finger safety — CLI only, per brief; grammar lives in PLAN F2) ·
  configurable keys for any of the above (zero-config stands).

---

### Open: a click on the picker's cwd column launches the wrong thing
**[Added 2026-08-20, ADAPTER_COL audit]**

`App::handle_modal_mouse`'s picker arm hit-tests **rows only**
(`mouse::picker_row_at`) and calls `picker_launch(i)`. A click anywhere on
row `i` — including on the *recent-directory* label in the second column —
launches adapter `i` with whatever cwd was already selected. So clicking a
directory does not choose that directory; it launches an adapter you may not
have meant, into a directory you did not click.

Pre-existing, and older than the constant that surfaced it. It is recorded
now because C14's column geometry became *derivable* when `ADAPTER_COL`
stopped being two numbers: a click's column can now be compared against it,
which is what any fix would need.

The fix is not obvious enough to guess at. C12's U8 click rules say the
picker "launches the row clicked and cancels on a click outside" — written
before there were two columns, and silent about which column. Three
readings: a click on the cwd column *selects* that directory without
launching (matching `←→`'s keyboard behaviour, and the picker's own
two-column focus model); or it selects **and** launches, which is the
fewest clicks but makes a mis-aimed click destructive; or the columns stay
one hitbox and U8's sentence is amended to say so deliberately. The first
matches the keyboard, which is usually roost's tie-breaker.

Found by the design audit of the `ADAPTER_COL` fix — not by the fix, which
touches no hit-testing.

### ~~Open~~: the picker's adapter column has two widths and neither widens
**[Added 2026-08-20, floor-stress audit · RESOLVED 2026-08-20]**

`render.rs` defines `ADAPTER_COL` **twice, with different values**: 23 in
`picker_dialog_width` (documented as 3-char row prefix + longest id + longest
status suffix + slack) and 16 in the draw path, where it is the pad the
adapter cell is filled to. Neither derives from the other, and the drawn row
can exceed the smaller one: `" 1 opencode not found"` is 21 cells, so `pad`
saturates to 0 and the cwd column shifts right, out of alignment with every
other row.

Not a fusion — the cwd column opens with its own selection marker, so a cell
of separation survives — and not reachable unless an adapter is missing from
`PATH`, which is why it has never been seen. But it is the §4/§5 lockstep
smell in miniature: two constants with one name, one sizing the dialog and
one drawing inside it, free to disagree. The fix is to derive both from one
number; the reason it is filed rather than done is that it belongs to C14's
sizing contract, not to the help overlay's, and the two arrived in the same
audit only by proximity.

Found by the design-supervisor sweep for the `{:<N}` minimum-width mistake
that produced the C15 amendment above — the class, not the instance, which is
what that sweep was for.

**Resolved:** one module-scope `ADAPTER_COL = 23`, used by both the sizing
and the draw loop, so the dialog is sized for exactly the column it draws.
The reason for filing rather than fixing — that it belongs to C14's sizing
contract rather than the help overlay's — turned out to be a reason to give
it its own commit, not to leave it. `the_adapter_column_is_wide_enough_for_
the_row_it_pads` walks the worst case (`picker_row_body(0, "opencode not
found")`) instead of asserting the number, and re-checks that
`picker_row_body` still produces it, so the worst case cannot quietly stop
being the worst case. Reverting the constant to 16 reproduces the original
misalignment by name.

### ~~Open~~: the composer's `Exited` filter tier can never match
**[Added 2026-08-20, simulation pass · RESOLVED 2026-08-20]**

C36's `Tab` filter shares `ROSTER_STATUS_CYCLE` with C27's roster, which is
deliberate — the composer shows a *target count* for the tier it names, and
tiers that meant different things in the two places would make that count
unreadable. But `broadcast_targets` excludes exited panes **unconditionally**
(`rt.status() != AgentStatus::Exited`), so the cycle's `Exited` stop was
structurally always "no panes" in the composer: a stop you could Tab onto
that could never do anything, on any fleet.

**Resolved: the composer skips it; the roster keeps it.** The two invariants
turned out not to be in tension once the question was put precisely. What
the sharing protects is that the tiers *name the same statuses in the same
order* on both surfaces — not that both surfaces stop at all of them. A stop
that can only ever mean "nobody" is not a filter, it is a hole in the cycle;
and the roster is a *monitoring* surface, where "which of my panes died" is
the whole point.

**Precisely, though:** the tiers do not *mean* identically the same thing on
both surfaces, and an earlier draft of this entry said they did. `None` is
"every pane" in the roster and "every pane that can receive" in the composer,
because `broadcast_targets` excludes exited panes whatever the filter says.
That difference is inherent to the two surfaces' jobs rather than introduced
here — the composer could never have targeted an exited pane — and the count
stays honest either way, which is what C36 needs of it. Named by the design
audit, which was right that the looser wording papered over it.

So there is still exactly one definition of the tiers and their order
(`ROSTER_STATUS_CYCLE`), stepped by one function, with the composer passing
`deliverable_only` — rather than a second const free to drift, which is the
shape the original C36 rationale was written to avoid.

The skip is a **second step in the same direction**, never a fallback to
`None`: `Tab` and `Shift+Tab` must stay exact inverses across the gap, and
the naive fallback makes them agree at one point in the lap and diverge for
the rest of it. Pinned by `the_composer_cycle_is_reversible_across_the_gap`,
which is what the mutation check breaks.

Found by a simulation agent reviewing C36 adversarially — it could not
defeat the guard, and found this instead.

### Open: an Alt chord discards an editor's unsaved buffer
**[Added 2026-08-20, C24b amendment audit]**

Found while contracting C24b's escape hatch, and deliberately **not** fixed
there — the amendment documented and gated existing behaviour; this is a
behaviour question that deserves its own answer.

C24b's Rename bullet permits discarding an unsaved buffer on the entry
chord, and justifies it narrowly: "an explicit second `Alt+r` is a
deliberate act, unlike U8's stray click, which still may not throw unsaved
text away." But the escape hatch discards on **every** Alt chord, not just
the entry one. `Alt+n` while composing a broadcast (C36), or `Alt+1` in the
note editor (C32), drops the text with no prompt and no undo — the same harm
U8 named, arriving through the keyboard instead of the mouse, and without
the deliberateness the Rename bullet leans on.

The tension is real but the fix is not obvious, which is why it is recorded
rather than guessed at. Three shapes, none free:
- **Confirm on a dirty buffer.** Honest, but puts a dialog in front of
  `Alt+q`, which U1 already guards — two confirms deep to quit is worse.
- **Keep the buffer and restore it on re-entry.** No interruption, but adds
  state that must be invalidated (which pane? which tab? how stale?), and a
  composer that silently remembers last week's half-message is its own bug.
- **Leave it.** Defensible for `Alt+q` (you asked to quit) and much less so
  for `Alt+1`.

Blocked on nothing but a decision. Whichever way it goes, C24b's amendment
and its gates stand — they pin *which* chords a mode keeps, not what a mode
throws away on the way out.

### C33 — Move a pane within its tab (`Alt+Shift+hjkl`, and its arrow spellings) — [Added 2026-08-19, comparative UX review; re-keyed 2026-09-01]

> **[2026-09-01 amendment, read first for the live map.]** Resize moved off
> `Alt+Shift+arrows` to vim's Ctrl-w punctuation (`Alt+-`/`=` height,
> `Alt+<`/`>` width — §8), and the arrows **joined** `Alt+Shift+hjkl` as
> spellings of the same move-the-pane verb: one verb, two spellings, the
> unification this contract's own adjacency rationale anticipated. The
> paragraphs below describe the contract as added, when only the letters
> carried the verb; the operation, guarantees and edge rules are unchanged.

**Origin.** The comparative review against zellij / lazygit / gh dash
(`docs/engagements/2026-08-19-comparative-ux-review/`) opened two findings
that turn out to be one change. F8: roost could move a pane *between* tabs
(C28) but not *within* one, so promoting an agent into main+stack's main slot
meant closing and respawning it — zellij has a whole move mode for this. F2:
`Alt+Shift+hjkl` was bound to nothing coherent. The shifted vim letters fell
into the focus arms, which carry no `shift` guard, so a terminal delivering
`('h', SHIFT)` moved focus left — a duplicate of plain `Alt+h` — while one
delivering `'H'` matched nothing and forwarded meta-H into the agent. One
physical chord, two behaviours, split by terminal.

So four chords were being spent on a redundant second spelling of `Alt+hjkl`,
in a table whose free unshifted pool (§8) is empty, while the verb that wanted
them had no chord at all. C33 spends them on the verb.

**The binding, and why it is not a new idiom.** C28 already established that
the shifted sibling *carries the pane* the way the unshifted one *carries
you*: `Alt+i`/`Alt+m` move you between tabs, `Alt+Shift+i`/`Alt+Shift+m` move
the focused pane there (the spellings C28 carried until the 2026-09-01 re-key
put the carry on `i`/`Shift+i` — see that contract's amendment).
`Alt+hjkl` moves you within a tab; `Alt+Shift+hjkl`
moves the pane within a tab is the same sentence on the other axis. Shifted-
letter-carries-the-pane stops being a tab special case and becomes a rule that
holds on both axes. **[2026-09-01: the rule is now spelled the same way on
both halves of the keyboard — the arrows took the verb too, and resize
re-keyed to `-`/`=`/`<`/`>` (§8); the tab families were re-homed to
same-letter shift-reverses.]**

Both delivery forms are bound (`Char('h')` + SHIFT and bare `Char('H')`, for
all four letters), the same tolerance C23's `Alt+P`, C27's `Alt+A` and C28's
`Alt+I`/`Alt+M` carry. That also repairs the escape hatch: `twins` pairs two
`Char` chords exactly when they share a case-folded letter **and both already
default to the same action**, so before C33 a `config.json` remap of
`alt+shift+h` bound only one encoding and silently no-opped on terminals
sending the other — the precise failure `twins`' own doc comment forbids. With
both forms defaulting to `MovePane`, `twins` pairs them with no change to
`twins` itself.

**Swap, not re-parent.** The operation is `layout::swap_panes` — two `PaneId`s
exchange slots and *nothing else changes*: every `Split`'s `dir` and `ratios`,
every `Stack`'s arity, the whole shape is bit-identical afterwards. A true
"move" (detach and reinsert) would change tree shape, could collapse a split
to one child — the hazard `remove_pane` carries its own collapse rule for —
and could reach nestings `Alt+g`'s canned cycle would never generate. There is
no layout a swap can produce that a pair of splits couldn't already have
built, which is why it needs no new invariant and no ratio arithmetic.

**Direction comes from `layout::neighbor` — the same call `focus_dir` makes.**
Deliberately not a second adjacency rule: "which pane is that way?" has one
answer in roost, so the move can never disagree with the focus key that taught
the user the direction.

**Stacks need no special case.** Every stack member already owns a `PaneRect`
(collapsed ones a 1-row bar or 3-row box, C8 amended 2026-09-01), so `Down`
inside a stack finds the next
member exactly as it finds the next split, and the gesture reorders the stack.
One thing does need handling: `Stack`'s `expanded` is an **index**, not an id,
so a bare id exchange would leave the moved pane collapsed and expand whichever
pane it displaced — press "move down" and the pane you are reading shrinks to a
title bar while a different one opens. `swap_panes` therefore carries the
expanded slot with its pane, the same way `remove_pane` already repairs
`expanded` itself rather than making callers remember to.

**Focus follows the pane, for free.** The tree swaps two ids, so the focused id
never changes — it simply occupies a different slot. No `set_focus` call, and
therefore none of the spurious CSI O/I focus-report pairs `focus_dir_cross_tab`
documents.

**No cross-tab handoff at the edge — the one deliberate divergence from
`focus_dir`.** C31 lets `Alt+←/→` continue into the next tab at a tab's edge;
`Alt+Shift+hjkl` at an edge is a **no-op**. Three reasons. Moving a pane out of
its tab is a structural edit rather than a look, so the recoverable failure is
doing nothing. `Alt+i`/`Alt+Shift+i` (C28) already do exactly that job and
name it. And C31's own rule — "tabs are roost's own horizontal axis, so only
the two keys that already share that axis pick up tab-switching semantics" —
would give `h`/`l` a cross-tab meaning that `j`/`k` could never have, splitting
one chord family across two scopes. The handoff remains available as a later
amendment if use argues for it; it is not the default because an accidental
cross-tab move is the more expensive mistake.

**Zoom and the float.** A swap joins `NewPane`/`ToggleStack`/`FlipSplit`/
`Resize`/`CycleLayout` in `apply`'s structural-action guard: it exits zoom
(C21 — the tab must never change invisibly behind a full-screen pane) and hides
the float (C22 — the float has no position in the tiled tree, so no direction
applies to it). The float is additionally excluded inside `move_pane_dir` on
C22 rule 2's grounds.

**Chrome.** §8 gains row 4b. C15's `LAYOUT` group gains one row, placed
directly under `Alt+Shift+←↓↑→ resize` — the two shifted-direction chords sit
adjacent so the one thing a reader must learn (**arrows resize, letters carry
the pane**) is visible at a glance rather than inferred.
**[2026-09-01: the re-key inverted the lesson this paragraph taught — arrows
and letters now carry the pane together (row 4b spells both), and the resize
rows above them are the punctuation `-`/`=`/`<`/`>`. The adjacency survives:
the family still sits under the resize rows, which is now the distinction a
reader is most likely to blur — shifted direction moves, punctuation
resizes.]**

**This is a different adjacency rule from C28's, deliberately** (caught by the
design-supervisor audit of this contract, which found the first draft claiming
C28's argument as its own). C28 seats a row under *its own unshifted form* —
`Alt+Shift+i / +m` directly below `Alt+i / Alt+m`, both in `TABS` — so the
pairing itself explains the chord. **[2026-09-01: C28's row spellings moved
to `Alt+i / Alt+Shift+i` under `Alt+m / Alt+Shift+m` — the adjacency rule
itself is unchanged.]**
C33 cannot do that: `Alt+hjkl` is a focus
verb and lives in `PANES`, while this is a layout verb and belongs in
`LAYOUT`. What C33 seats itself under is therefore the chord it is most likely
to be *confused with* rather than the one it derives from — the other
shifted-direction chord — because the reader's live question at that row is
"shift plus a direction did something different a moment ago, which is which?"
The two rules share the tactic (teach by adjacency) and not the criterion
(derivation vs. confusability); the older one should not be cited for the
newer.

The C9 hint bar's Normal-mode seven
**gain nothing**: the 100-column arithmetic is unchanged and the chord is
discoverable via `Alt+?`, exactly as C27 and C28 were.

**Config.** Four new `NAMES` entries — `move_pane_left` / `_right` / `_up` /
`_down` — so the chords can be remapped or disabled like any other.

**Known gap, stated rather than left implicit.** C15's
`every_bound_chord_is_documented_in_the_keymap` is a hand-kept list of chord
literals checked against the hand-kept `HELP_GROUPS` text — both `const`,
neither derived from `default_chord_action`. It did not and could not catch
four newly bound chords; the list was extended by hand here. Deriving that gate
from the effective keymap is F1 of the review that produced this contract, and
is where the real fix belongs.

### C34 — The chrome reads the live keymap (`config.json` awareness) — [Added 2026-08-19, comparative UX review F1]

**The defect.** roost has shipped a keybinding escape hatch since v0.1.3:
`config.json` can disable or remap any `Alt` chord, and `translate_with`
applies it. Both surfaces that *teach* those chords ignored it. `HELP_GROUPS`
(C15) was a `const` of `(&'static str, &'static str)` tuples and `hint_pairs`
(C9) returned `&'static str` keys, so the README's own worked example —
remapping away from the `Alt+f`/`Alt+b`/`Alt+d` readline collisions — produced
a roost whose `Alt+?` still taught `Alt+f` and never mentioned the chord that
replaced it. The one modal you open when you are lost was the one that could
lie to you.

**The check could not catch it, and was weaker than it looked.**
`every_bound_chord_is_documented_in_the_keymap` was a hand-kept list of 27
chord literals tested for containment against the hand-kept `HELP_GROUPS`
text. Both sides `const`; neither derived from `default_chord_action`. It
proved that one constant mentioned strings another constant was written to
contain — nothing about the running binary under any configuration — and a
newly bound chord passed it silently unless someone remembered to extend the
list. C33 bound four chords and had to do exactly that, by hand, four days
before this contract.

**The rule.** *Any chrome that names a chord derives that name from the live
keymap.* A surface may not spell a binding it did not ask the keymap for.

**Scope, enumerated** (the C34 audit found the first draft asserting the rule
universally while implementing it for two surfaces — so here is the list, and
it is the list the rule is accountable to):
- **C15** help-overlay rows — resolved.
- **C9** hint-bar `Alt` pairs — resolved.
- **C9** attention segment (`◆ N needs you · Alt+a`) — resolved. C9's own
  amendment calls this "this feature's discoverability surface"; a surface
  teaching a dead chord is worse than one teaching nothing, so an unbound
  jump drops the `· chord` tail.
- **C16** dead-pane bar (`· Alt+w: close`) — resolved. It and the C9 hint bar
  one screen row below name the same chord, and before this only one derived
  it.
- **C10** confirm and refusal flashes (`Alt+w again to close`, `stack a pane
  with Alt+s first`) — resolved, via `App::chord_label`. The confirms matter
  most: "press X again" naming a dead X is a guard that cannot be satisfied
  by the key it names, which is U1's hazard wearing a helpful face. Unbound
  degrades to a bare "again", still true because the confirm is armed on the
  *action* and a repeat by any route satisfies it.

- **`roost keys`** (F11, the control CLI's one *local* verb) — the same
  resolution, printed. It is not a socket verb: it reads `config.json` off
  disk and answers with no running roost, because the moment you want to ask
  "what did my dotfile do" is before launching, or because launching is what
  is going wrong. Deriving it from `effective_bindings` is what made it a
  printer rather than a feature; it also gives config.json's diagnostics a
  home outside the TUI, which is the gap the review named — they used to
  surface only as a startup toast, so a mistyped chord was found by catching
  a transient message.

**Not in scope, and why:** mode-local keys (`hjkl`, `v/V`, `Esc`, `n/N`,
`↵`, `f`, `y`) and mouse verbs. config.json's grammar is `Alt` chords only
(`Chord::parse` requires the prefix), so none of them can move and none can
go stale. Only what can move is derived.

**The width rule, and what C34 nearly cost it.** C15 requires one *column* to
stay ≤ 80 or `centered_near` clamps and clips a description mid-word. Before
C34 a row's key was authored text of known width, so that was a thing you
checked once. Deriving it made it **unbounded**: an enumerated family grows
with its membership, and the audit measured `{"keys":{"alt+h":"disable"}}`
producing a 107-column column — reintroducing the precise failure C15's rule
exists to prevent, by the mechanism meant to make the row truthful. (The
pinning test had only ever exercised the default keymap: a default-only
assertion about a quantity that had just stopped being constant.)

So a resolved key column is **elided** to fit: cut at a `" / "` boundary with
a trailing `…`, never mid-chord. **The key yields and the description never
does** — a reader who sees two of eight chords still learns what the row is
for and can widen the terminal for the rest, while a clipped description
teaches nothing at any width. Pinned by
`one_help_column_fits_the_floor_under_a_remap_too`, which sweeps the configs
the audit measured rather than the default alone.

**Mechanism.** `input::effective_bindings(&Keymap)` returns every
`(label, action)` roost actually binds — the default table merged with
config.json's overrides, which is the same merge `translate_with` dispatches
on, asked as a question instead of answered one key at a time. `default_keymap`
stops being `#[cfg(test)]` to serve it (memoized; the render path asks every
frame).

**One physical chord, one entry.** The sweep collapses three redundant
spellings, or the overlay would print one key twice — or a key nobody can
press:
1. **Uppercase-delivery twins of a shifted letter.** Terminals disagree
   (`('p', SHIFT)` vs bare `'P'`) and the table binds both (C23/C27/C28/C33).
   Letters keep the `Alt+Shift+p` spelling.
2. **Shifted-punctuation twins** — the same duality on the other half of the
   keyboard (`('/', SHIFT)` vs `('?', no shift)`). Here the *glyph* spelling
   wins (`Alt+?`, not `Alt+Shift+/`): mirror-image choices, each keeping the
   half that reads naturally. The two rules are mutually exclusive by
   construction — letters are excluded from rule 2 — because as symmetric
   rules they would otherwise delete both halves of every letter pair.
3. **Shift states no arm ever tested.** Most arms don't guard `shift`, so
   `('1', SHIFT)` resolves to `GoToTab(0)`. Nothing is *bound* to
   `Alt+Shift+1`; the arm ignores shift, and the unshifted spelling is the
   chord.
Each rule fires only when both spellings carry the same action — where they
differ, both are real (`Alt+←` focuses, `Alt+Shift+←` moves the pane — the
2026-09-01 re-key, which is also why the example no longer reads "resizes").

**Row model (C15).** A help row declares the **actions** it documents, not a
chord string:
- `Chords(actions)` — print whatever is bound to them, joined by `" / "`.
  A second chord for a documented action therefore documents *itself*.
- `Family(spelling, actions)` — a compact hand-written spelling for a family
  too wide to enumerate (`Alt+←↓↑→ / hjkl`, `Alt+1..9 / Alt+0`). The actions
  are still declared, so the coverage sweep sees each one; the shorthand is
  printed **only while every member is still on its default chord**. Move one
  and the row enumerates the real chords — otherwise the shorthand would be
  precisely the stale spelling this contract abolishes, merely a wider one.
- `Text(key)` — authored text of known width: the C5 legend, the mouse-verb
  rows, the control-CLI block. Named "names no chord" in this contract's
  first draft, which the C34 audit found to be false of one row —
  `Alt+click / o` is a `Text` row that does name a chord. It is correct
  *behaviourally* (a mouse chord is not in config.json's grammar, so it
  cannot move) but the criterion is authorship, not chord-freeness: `Text`
  is what roost writes by hand, `Chords`/`Family` is what it resolves.
A row whose every chord is disabled is **dropped**, and a group left with no
rows drops its heading too: an empty titled block advertises a section that
isn't there.

**Hint bar (C9).** Same treatment for its `Alt` pairs, and a disabled chord
drops its pair. **Mode-local keys stay literal** — `hjkl`, `v/V`, `Esc`,
`n/N`, `↑↓` — and that is not an oversight: config.json's grammar is Alt
chords only (`Chord::parse` requires the prefix), so those keys cannot move
and cannot go stale. Only what can move is derived. C9's fit/yield machinery
is untouched and absorbs the rest: a remap that lengthens a label simply
makes pairs drop from the right, in the order U6 fixed.

**Two spellings, deliberately.** `Chord::label` is *not* `Chord::parse`'s
inverse. `parse` accepts config.json's grammar — lowercase, spelled out
(`alt+pageup`) — because it is typed into a JSON file; `label` produces the
chrome's vocabulary (`Alt+PgUp`, `Alt+←`), which is what the key table has
always shown. Each register serves its own reader; the README documents the
config one.

**The rule is mechanical.** `no_surface_spells_a_chord_it_did_not_resolve`
(`input.rs`) scans every production line under `src/` and fails on a string
literal containing `Alt+` that is not on a short allowlist of **resolver
defaults** — `Chord::label`'s two format strings, C9's `alt(..)` fallbacks,
C15's `Family` shorthands, and the one authored mouse row.

The allowlist matches the **whole literal**, which is what gives the gate its
teeth: a bare `"Alt+w"` is what a resolver argument looks like, while a chord
inside a sentence (`"last pane — Alt+w again to quit roost"`) is what a
hard-coded surface looks like, and only the first form can be listed. Every
deviation this contract's own audit found was of the second form — and every
one of them was caught by a person reading code, which is the reviewing
capacity this gate exists to stop spending. A new spelling now fails until it
is either resolved or added to the list, and adding it is the moment its
author has to ask whether the surface should be deriving instead.

Not a proof — `format!("press {} now", "Alt+w")` would pass — and the
contract claims no more than it does: a guardrail against the accident, not a
defence against circumvention.

Scanning source is the honest way to pin a rule about *what may be written*,
the same argument §2's fixed-hue gate makes; the two now share one walker
(`ui::srcscan`). Two things about that scanner were found by its own guard
tests rather than reasoned out, and are worth knowing before touching it: the
production/test cut must land on a test **module** (`#[cfg(test)]` also
attaches to test-support items scattered through production code, and cutting
at the first of those hid most of two files), and it must accept **any**
test-gated `cfg` (`infra/qos.rs` gates its module on
`all(test, target_os = "macos")`). A source scan that silently stops reading
is worse than none.

**What replaces the old check.** Three gates, none keeping a list of chords:
- `every_bound_chord_is_documented_in_the_keymap` sweeps
  `effective_bindings` and asserts every bound **action** is declared by some
  row. Actions rather than spellings, because a `Family` legitimately draws a
  string containing no single chord as a substring — and only ever draws it
  while it is accurate.
- `every_documented_row_names_an_action_roost_actually_binds` is the
  converse, covering a failure mode that did not exist before: a row naming
  an unbound action now renders nothing and **vanishes silently**, where the
  old literal row would have stayed visible and merely lied.

**Cost, recorded.** `HelpLine::Row` carries an owned `String`;
`help_content_width` takes the resolved lines rather than reading the const;
`help_layout`, `help_scroll_extent`, `dialog_rect` and `hint_pairs` take a
`&Keymap`. `HelpLayout` gained a `content` field so the drawer stops
recomputing a width that now costs a resolution pass.

`effective_bindings` clones and sorts the default map on each call — one to
two calls per frame in Normal mode, up to four while the help overlay is
open. Left uncached deliberately: the memoization that matters
(`default_keymap`'s `OnceLock`) covers the expensive half, the remainder is a
~51-element sort whose cost does not register against the §6 firehose gate,
and a keyed cache would need a lock in the render path to stay correct across
the several keymaps a single test process uses. Recorded so a later pass
measures rather than rediscovers.

### C35 — Go back (`Alt+;`) — [Added 2026-08-19, comparative UX review F6]

**The gap.** Every navigation chord roost had was absolute (`Alt+1..9`,
`Alt+0`) or forward-directional (`Alt+a`, `Alt+m`, `Alt+hjkl`).
Nothing returned you to the pane you were just on. At fleet scale the
dominant motion is "check on B, come back to A", and `Alt+a` only ever walks
*on* through the attention ring — on a two-pane hop it is the wrong tool, and
across a tab switch there was nothing at all. tmux has had `prefix ;` for
last-pane since forever; vim has `Ctrl-^`.

**The chord, and why it costs nothing.** `Alt+;` — and the reason it was
available is worth recording, because this table's own §8 accounting had
declared the pool empty. That accounting enumerated **letters**. Outside
`Alt+/` and `Alt+?` every punctuation chord was, and mostly still is, free
(`;` `'` `,` `.` `]` `-` `=` `` ` ``). **[2026-09-01: `-` `=` and the
`<`/`>` glyph are now bound (resize, §8's re-key), and `,`/`.` are taken in
their SHIFT delivery only — the unshifted keys still forward as M-, / M-.;
the list records the pool as this contract found it.]** `Alt+[` remains
rejected — `ESC [` is
the CSI introducer (§8) — but the rest were never counted. So F7's leader key
is **not** a prerequisite for this contract, and the "concrete feature needs
a chord and finds none free" trigger has not fired here.

`;` is also the right key rather than merely a free one: it is tmux's own
last-pane binding, so the muscle memory already exists, and `M-;` is not a
readline binding, so nothing in a shell pane loses a key.

**One chokepoint.** `App::alternate` is maintained in `set_focus`, which
`set_focus`'s own comment already establishes is where *every* focus move
funnels — a click, an arrow, `Alt+a`, a tab switch, a roster jump. Recording
there rather than per call site is what makes the trail complete without the
next focus-moving feature having to remember to update it.

**It toggles.** The pointer records every real transition, including the ones
this chord causes, so A→B leaves A, going back to A leaves B, and pressing
again returns to B. A stack would be the other design and is deliberately not
it: "back" that sometimes goes somewhere new is worse than one that always
goes to the same place.

**Existence is checked at both ends, by one predicate.** `pane_exists`
(tiled or float) guards both the record and the use:
- **Recording** — `focused` initializes to a `0` sentinel that names no pane,
  so without this the very first focus move would file a phantom and the
  first press would report a pane that never existed. (Found by the
  "nothing to go back to" test, which failed with the wrong message.)
- **Using** — the pane can close while you are away. Landing focus on a dead
  id would be a real bug.
Sharing the predicate is deliberate: two ends that disagree about what
"exists" means is how the phantom gets back in.

**It flashes rather than no-oping.** No alternate yet, or an alternate that
has closed, each say so. A navigation key that silently does nothing reads as
broken, and the whole value of this one is confidence about where it lands.

**Reuses `focus_attention_target`.** A cross-tab return switches tabs, expands
a collapsed stack member and handles the float exactly as `Alt+a`'s jump does
— "which pane" and "how do I get there" stay one answer each rather than two
that can drift.

**Chrome.** §8 gains row 10c. C15's `FLEET` group gains one row, directly
under `Alt+a`: the two are the pair a fleet navigates with — one goes on to
whoever needs you, the other comes back. Both the row and the chord spelling
are resolved from the live keymap (C34), so this contract adds no literal and
needed no allowlist entry — the C34 machinery absorbed a new chord with no
manual bookkeeping. The C9 hint bar's Normal seven **gain nothing** (the
100-column arithmetic is unchanged; `Alt+?` teaches it).

**Config.** One new `NAMES` entry, `focus_alternate`.

### C36 — Broadcast composer (`Alt+'`) — [Added 2026-08-19, comparative UX review F3]

**The gap.** `roost send --all` has existed since the fleet-features round and
was **CLI-only by design, no TUI key**. So the one verb roost is uniquely
positioned for — say the same thing to every agent at once — was the one thing
you had to leave roost to do. zellij's sync-input-to-tab is among its
most-cited features and it isn't even aimed at agents; roost had the better
version of it, built, and unreachable from the keyboard.

**Not a sticky sync mode, and that is the design.** The obvious shape — a
mode where every keystroke goes to every pane — is rejected. It is the
unguarded-destructive shape U1 exists to prevent: you would have to *notice a
mode indicator* to avoid typing a stray `Alt+q` into five agents, and U1's
whole lesson is that a fleet-wide action must not depend on the user
remembering what mode they are in. C36 is one composed message and one
deliberate send.

**The guard is the visible blast radius.** The dialog's title is
`broadcast → N panes`, live: it moves as `Tab` cycles the status filter, and
at zero it says **`broadcast → no panes`** outright. That count is the safety
affordance — *not* a confirm-twice.

*The zero case is carried by words, not by a colour.* The first draft styled
the title `accent()` at zero; the design audit flagged it as unsanctioned —
C12 specifies a modal title as `ink()`, no other dialog varies its title token
by state, and §2's attention idiom is reversal rather than recolouring.
Amending C12 for one dialog would have been the wrong trade when the words can
simply say it, which is also unmissable in a way a colour is not. The tier
glyph, by contrast, *does* carry its own C5 colour — C27's rule verbatim,
since "a bare `ink()` glyph would read as no tier at all" — and the first
draft discarded that style, which the same audit caught. A count you read at the moment of commit tells you something a
second keypress cannot: **who**, and how many. The confirm-twice idiom (U1,
U22) is right for closing one busy pane, where the question is "are you sure";
here the question is "sure about whom", and only a count answers it.

For that to be a guard rather than a decoration, the number and the send must
come from **one predicate**: `App::broadcast_targets(actor, status)` — alive,
not the float, within the actor's authority, matching the filter. The title
counts it; the send writes to it. A guard that lies would be worse than none,
so the two cannot be separate code paths. Pinned by
`the_target_count_is_the_set_that_actually_receives`.

**`Tab` is C27's filter, shared — minus one stop.** The composer and the
roster step the same `ROSTER_STATUS_CYCLE` through the same function —
extracted rather than copied, because the composer is showing a *count* for
the filter it names, and a filter that meant something subtly different in
each surface would make that count unreadable. "Send to the three panes that
are `◆`" is now one gesture, and it composes two surfaces that already
existed.

**[Amended 2026-08-20]** The composer passes `deliverable_only` and so skips
`Exited`. `broadcast_targets` excludes exited panes unconditionally, which
made that stop structurally always "no panes" here — a stop you could Tab
onto that could never do anything, on any fleet. What the sharing protects
is that the tiers *mean the same thing and come in the same order* on both
surfaces, not that both stop at all of them; a stop that can only ever mean
"nobody" is a hole in the cycle rather than a filter. The roster keeps it,
because "which of my panes died" is what a monitoring surface is for. Still
one definition of the tiers, one stepping function, no second const free to
drift. See §7 for the full reasoning, including why the skip must be a
second step in the same direction rather than a fallback to `None`.

**Keys are C32's.** `Enter` sends · `Shift+Enter` (or `Ctrl+Enter`, **or
`Alt+Enter`**) breaks a line · `Tab`/`Shift+Tab` cycle the targets · `Esc`
walks away having sent nothing · `Ctrl+U`/`Ctrl+W` and the motion keys are
Rename's vocabulary. Multi-line matters: agent prompts are, and a composer
that could only carry one line would push people back to the CLI for exactly
the messages worth broadcasting.

*This contract's first draft said "exactly C32's" and was wrong by one:*
C32's `Alt+Enter` carve-out was `PaneEdit`-only, so on Terminal.app — where
Shift+Enter arrives as `Alt+Enter` (README's CSI-u note) — the one chord that
should break a line opened the quick-launch picker and **discarded the
composed message**. Found by this contract's own design audit; the carve-out
now covers both modes through one shared `broadcast_break`.

**Capped at `BROADCAST_MAX_LINES`** (= C32's `NOTE_MAX_LINES`). The dialog is
`lines + 2` rows tall and never scrolls, so an uncapped message pushes its own
rows and the caret off the body with nothing to bring them back. At the cap
the break is *refused* rather than partially applied: losing the split is
recoverable, losing the text under it is not.

**It submits.** Unlike `roost send --all`, where `--enter` is opt-in because a
script may want to stage text, the composer always appends `\r`. A human who
typed a message and pressed Enter has expressed the intent; typing into five
agents and leaving five prompts un-submitted does the tedious half of the job
and skips the half you opened it for.

**Audited as `Actor::Local`** — a third actor variant beside `Fleet` and
`Pane(id)`, added by this contract. It reaches every pane like `Fleet` and
authenticates nothing (no token resolves to it; pressing a key inside roost
*is* the credential). Separate purely for the audit trail: a keyboard
broadcast logged as `fleet` would be indistinguishable from a token holder's,
and attribution is that log's whole value. The C20 feed comes free, since it
renders the same `ctl` lines — exactly one per broadcast, the property the
control path already pins.

**Chord.** `Alt+'`, which neighbours C35's `Alt+;`: the fleet's two
punctuation verbs — go back, and speak to everyone. Punctuation costs the §8
letter pool nothing (C35's amendment), so the leader key's trigger **still**
has not fired.

**Chrome.** §8 gains row 22. C15's `FLEET` group gains one row after the feed.
C9's mode word gains `BROADCAST` and the composer gets its own pair list —
**74 columns**, ordered `↵ send` · `Esc cancel` · `Tab who gets it` ·
`Shift+↵ new line` · `type message`.

That order is a safety property, not a style choice. 74 plus the 33-column
needs-you segment overflows the 100-column floor, and U6's rule is that list
order *is* yield order, so trailing pairs drop exactly when the fleet is busy
— which is exactly when someone is composing a broadcast. The two that must
survive therefore lead: what the dialog does, and how to leave without doing
it. `type message` trails because a text field you are already typing into is
the pair whose absence costs least. The motion keys are deliberately **not**
on this bar, on C24's precedent: an eighth pair would push `Esc` off, and a
bar that drops its escape hatch to advertise a convenience has its priorities
backwards. They are C13/C32's shared vocabulary and the C15 overlay teaches
them. Pinned by
`hint_pairs_broadcast_leads_with_the_two_that_must_not_yield`. The dialog is C12's frame at C13's 44-column width — deliberately
not wider, because the eye should be on the title's count, not sweeping a wide
field. The Normal-mode seven gain nothing.

**C12/U8 — the composer is a modal, and had to be told so.** Its first draft
omitted `Mode::Broadcast` from `App::modal_active`, which is U8(b) verbatim:
a click moved focus *under* the open dialog, and a paste landed in the **pane
behind it**. Someone composing a fleet-wide message and pasting a snippet
would have silently typed it into whichever agent happened to be focused. The
highest-severity finding of this contract's design audit, and the reason
`modal_active` is the single list every modal must join rather than a
behaviour each one implements.

**Config.** One new `NAMES` entry, `toggle_broadcast`.

### C37 — Reverse the layout cycle (`Alt+Shift+g`) — [Added 2026-08-19, comparative UX review F10]

**The gap.** C25's cycle is forward-only, so the arrangement you want can be
two presses away and there is no way to step back to the one you just left.
Every other cycle in roost goes both ways — `Alt+m`/`Alt+Shift+m` for tabs,
`Tab`/`Shift+Tab` for the roster's status filter — and C28 established that a
shifted chord is its unshifted sibling's inverse. zellij's swap-layout keys
(`Alt+[` / `Alt+]`) are bidirectional for the same reason.

**The arithmetic, since it is not obvious.** `layout_cycle` is the arrangement
to try *next going forward*, so what is showing is `layout_cycle - 1` and
stepping back starts two behind it. Forward tries `lc, lc+1, lc+2`; backward
tries `lc+1, lc, lc-1` — the same sequence reversed, expressed as
`(lc + if forward { step } else { 1 - step }).rem_euclid(3)`. Both directions
share C25's skip-what-doesn't-fit rule, so a shape the terminal is too small
for is passed over identically each way and the two can never disagree about
which arrangements exist.

**A ring, not an undo stack.** Stepping back from the first arrangement wraps
to the last; it does **not** restore whatever custom layout the tab had before
the first `Alt+g`. That layout is not one of the three canned arrangements, and
remembering arbitrary trees is what `Alt+u` and the split chords are for. The
inverse property therefore holds *from inside the ring*, which is the honest
statement of it and what
`reversing_the_layout_cycle_undoes_a_forward_step` asserts.

**Arrangements compare by shape, not by tree.** `arrangement_for` re-derives
from the live `pane_order`, which a previous step can have reordered, so two
visits to "grid" are the same arrangement with the panes in different slots.
Pre-existing C25 behaviour — the forward-only tests already assert shapes
rather than trees — inherited here rather than introduced.

**Chrome, and a merge that did not fit.** C15's `LAYOUT` group gains a
**second row** rather than merging both directions into one. The merged form
(`Alt+g / Alt+Shift+g` + "…(shift reverses)") put the **content column at
exactly 80 and the dialog rect at 83**, failing
`one_help_column_fits_the_eighty_column_floor`.

*Which number matters is worth pinning down, because this contract's first
draft said "83 columns" and blew past the distinction.* C15's rule is written
about one **column**; its pinning test measures `help_layout(..).size.0`, the
**dialog** — the column plus three cells of border and padding. The test is
therefore the stricter of the two and is the operative check: a column of 80
is already a dialog of 83, which `centered_near` clamps at an 80-column
terminal. The merged row did not exceed the rule as literally worded; it
exceeded what the rule exists to prevent. Two rows is also the more correct presentation: C28's
adjacency rule seats a row under *its own unshifted form*, which is precisely
what this is. The other kind of adjacency — seating a row under the chord it
is most likely to be *confused with* — is **C33's** (`Alt+Shift+hjkl` under
`Alt+Shift+←↓↑→`; since the 2026-09-01 re-key the family spells both
`Alt+Shift+←↓↑→ / hjkl` and seats under the `-`/`=`/`<`/`>` resize rows),
and C33's own design audit is where the two rules were
first told apart. An earlier draft of this paragraph credited that to C36;
corrected by this contract's audit. C15's row cap was retired in 2026-07-28
specifically so rows need not be merged to fit; this is the first change to
take that at its word.

**Config.** One new `NAMES` entry, `cycle_layout_back`. `cycle_layout` keeps
its name and now means the forward direction explicitly.

### C38 — A refusal says so — [Added 2026-08-20, simulation pass]

**The gap.** Eight gestures refuse correctly and wordlessly. Pressing them
changes nothing on screen, so "bound, but nothing to do here" is
indistinguishable from "not bound at all" — and roost's own contracts
already disagreed about whether that is acceptable. C35, shipped the same
week as C33, states the rule outright: *"a navigation key that silently does
nothing reads as broken, and the whole value of this one is confidence about
where it lands."* C33's edge no-op is the same gesture family and says
nothing. Found by a simulation agent comparing the two — the comparison is
what makes the inconsistency visible, since each contract reads fine alone.

**C33's silence was the outlier, not a decision**, and the tree already said
so: `move_pane_to_tab` flashes its own refusals with the reason written into
the code — *"a no-op you can see beats one you can't"* — and `focus_dir`
flashes at its cross-tab boundary. C38 is less a new rule than the rule the
rest of roost was already following, applied to the sites that missed it.
(That evidence came from this contract's design audit, which went looking
for whether the reading of C33 was fair. It is.)

C33's defence of the no-op is **not** a defence of the silence. It argues
that a swap must not cross tabs ("moving a pane out of its tab is a
structural edit rather than a look, so the recoverable failure is doing
nothing"), which is a claim about what the *layout* does. A flash moves
nothing.

**The rule.** When a gesture declines to act, it says why in a C10 flash —
and where another chord *would* do what the user is reaching for, it names
that chord, **but only when that chord would work**. A dead end that teaches
the way out is worth more than one that merely reports itself; a dead end
that hands you a second dead end is worth less than silence.

| site | flash |
|---|---|
| `Alt+Shift+h/l` at a tab edge, with another tab | `at the tab's edge — {chord} moves it to the {next\|previous} tab` |
| `Alt+Shift+h/l` at a tab edge, single tab | `at the tab's edge` |
| `Alt+Shift+j/k` at a layout edge | `nothing {above\|below} to swap with` |
| `Alt+Shift+hjkl` on the float | `the scratch pane sits outside the layout` |
| `Alt+s` / `Alt+o` / `Alt+- = < >`, tab has one pane | `nothing to {stack\|explode\|flip\|resize}: this tab has one pane — {chord} splits it` |
| `Alt+s` / `Alt+o` / `Alt+- = < >`, shape has no such move | `nothing to {stack\|explode\|flip\|resize} here` |
| `Alt+s`, the stack already holds the whole tab (C42) | `the whole tab is one stack — {chord} explodes it back into a split` |
| `Alt+s`, the rung would leave a pane with no pixels (C42) | `no room to stack: this tab has more panes than rows` |
| `Alt+1..9` past the last tab | `no tab {n}: this workspace has {n} {tab\|tabs}` |
| `Alt+n` / picker launch, split refused | `no room to split — {side by side needs 36 columns, has 30 \| stacked needs 10 rows, has 7}` |

Decisions inside that table, each load-bearing:

- **The horizontal edge names `Alt+i`/`Alt+Shift+i`, resolved from the
  live keymap, and only with somewhere to carry the pane to.** C33 declined
  the cross-tab handoff *because* those chords "already do exactly that job
  and name it" — and the moment the user needs to know that is the moment
  they press the wrong one. Through `chord_clause`, never a literal: the C34
  gate bans spelling one in `src/`, and a flash teaching a remapped-away
  chord would be the exact drift C34 exists to prevent. Guarded on
  `tabs.len() > 1` as well as on bound, because on the default single-tab
  workspace the named chord immediately refuses with "only one tab".
- **The vertical edge names nothing**, because there is nothing to name:
  C31 makes tabs roost's horizontal axis, so `j`/`k` have no cross-tab
  counterpart. Inventing one for symmetry would teach a chord that does not
  exist.
- **The three structural chords detect their no-op by comparing the tree,
  not by the mutator's `bool`.** `layout::resize_pane` returns `true` at the
  0.1/0.9 ratio clamp — "handled, no change" — so the bool does not mean
  "changed", and trusting it would drop exactly the case a user leans on the
  key for. One mechanism, no per-site special cases, and it cannot drift as
  those mutators grow.
- **A one-pane tab and an inapplicable shape get different sentences**,
  because they need different things: the first needs a split (`Alt+n`,
  named), the second needs a different key.
- **The refused split names the axis that actually failed, and the number it
  missed by.** `split_fit` picks the axis from the rect's aspect and tests
  *that axis only*, so an earlier draft's "a pane needs 36×10" asserted a
  rule the guard does not have — a 30×30 pane splits happily into two
  30-wide halves. A helpful message that is false is the worst kind.
- **A refusal flashes at the keypress, not inside `spawn_child`.** The
  control CLI reaches the same refusal and reports it in its own `Reply`;
  raising a TUI flash from an API call would put a message on screen that
  nobody at the keyboard caused. The two surfaces **word it differently on
  purpose** — an API client cannot widen a terminal, so its advice is "stack
  a pane with `Alt+s` first" rather than the pane's shortfall. What they
  share is that the refusal is *said*.

**A success stays quiet.** Only the refusal speaks — the new pane, or the
pane arriving in its new slot, is its own feedback, and a flash on every
`Alt+n` would be noise that trains the eye to skip the bar. Pinned by
`a_split_that_succeeds_stays_quiet` and
`the_structural_chords_stay_quiet_when_they_do_something`.

**`MovePane` left `apply`'s structural guard to make this work.** The guard
hid the float before `move_pane_dir` ran, so `float_focused()` was always
false there and this contract's float row was **dead code** — and what
happened instead was worse than the silence C38 set out to fix: the float
vanished and the swap landed on whichever pane it had been covering, moving
a pane the user was not looking at. `MovePane` now joins `Alt+z` in refusing
the float itself, by the same mechanism and for the same reason; C21's
zoom exit moved into `move_pane_dir`, where it must run before `rects()`
(under zoom `rects()` yields only the zoomed pane, so `neighbor` would find
nothing). Found by this contract's own design audit, which probed the row
rather than reading it.

**Deliberately still silent.** `layout::swap_panes` returning `false` is
unreachable — both ids came from `neighbor` — so it is a defensive branch,
not a refusal a user can reach; giving it a voice would put an impossible
sentence in the vocabulary. `go_to_tab`'s *same-tab* return is U11's
deliberate no-op, not a refusal: `Alt+1` on tab 1 has done exactly what it
says. The picker's Enter over an empty filtered list is left for now — the
empty frame is already partial feedback, and the right answer is probably a
line inside the dialog rather than a flash behind it.

**Chrome.** Nothing new: C10's flash, C9's hint bar, no glyph, no colour.
The longest flash measures 62 columns, inside both the 80-column floor and
C9's 100-column budget; a flash takes the whole bar and returns, so C9's
right-segment arithmetic is untouched. This contract adds sentences, not a
surface.

**Not in scope, and since resolved elsewhere.** The `Exited` tier of the
composer's `Tab` filter (C36) was structurally always empty — a dead stop in
the cycle rather than a silent refusal, so not C38's kind of problem. It was
C36/C27's question and got C36/C27's answer: the composer now skips the
tier, the roster keeps it, and §7 records why the two are not in tension.

### C39 — The keymap filters (`/`) — [Added 2026-08-20, comparative UX review F9]

**The gap.** The overlay is roost's longest list and its only unfilterable
one. `Alt+?` draws thirty-odd rows in eight groups; finding "the one that
moves a pane between tabs" means reading. Meanwhile roost already has
type-ahead in C14's picker and C27's roster, and `/` search inside scroll
mode (P21) — three surfaces where narrowing a list is a reflex, and the one
surface whose entire job is *explaining roost* is the one that cannot.

**The rule.** `/` opens a type-ahead filter. Typing narrows the table;
`Backspace` widens it; `Esc` clears a query and then, pressed on an empty
one, closes. The scroll keys keep working throughout. Matching is
case-insensitive over **both** columns — key and description — because a
reader is as likely to remember "the one with `g` in it" as "the layout
one".

**`Option<String>`, not `String`.** `None` is the un-filtered overlay and
`Some("")` is a live but empty query (`/` then `Backspace`). They behave
differently and must: under `None` every key still closes, `q` included;
under `Some` every printable types. An empty string standing in for "not
filtering" would make `/`-then-`Backspace` close the overlay on the next
letter. Pinned by `the_help_overlay_is_unchanged_until_slash_opens_the_filter`.

#### On the size of this carve-out — stated, not smuggled

C15's scroll amendment earned its carve-out by being **conditional**:
`help_scroll` reports whether it moved, a key that moved nothing falls
through and dismisses, so on a terminal showing the whole table the
amendment is invisible. It is tempting to present this one as the same
shape. **It is not, and the difference should be read plainly:**

- The scroll carve-out claims eight keys (`↑ ↓ j k PgUp PgDn Home End`),
  conditionally, and hands them back the moment they have nothing to do.
- This one claims **every printable key, unconditionally**, for as long as
  the query is open. There is no "did nothing, fall through" available:
  `Backspace` has to work when the query matches nothing, or a typo becomes
  a trap.

What licenses it is not the scroll precedent but **C27's roster**, which
made exactly this trade first and wrote the reason into its hint bar: *"`q`
is deliberately absent (the roster filters as you type, so a letter is
filter text, U20's rule) — `Esc` is the way out."* C14's picker is the same
rule again. So this is roost's own established idiom applied to the surface
that lacked it — not a new liberty, and not an extension of a smaller one.

Two things keep it honest:

- **`/` is the only unconditional new key**, and only from the un-filtered
  state. Everything else lives inside a state the user opened deliberately.
- **A second `/` is text, not a second open.** A key meaning "open" in one
  state and "type" in another is the ambiguity U20 already resolved for the
  picker; a query can contain a slash.

#### Where it announces itself

Both C15 surfaces, because while a query is open "any key closes it" is
false and a reader who cannot see why is stuck — on the one modal you open
*because* you are lost.

*[Amended 2026-09-01 — the two un-filtered rows below are superseded; the
dated amendment at the end of this section carries the current wordings
(`type to filter · Esc closes` / `type filter` · `Esc close`). The two
filtering rows are still current.]*

| state | title | hint bar |
|---|---|---|
| plain | `keys — / filters · any key closes` | `Alt+? all keys` · `/ filter` · `any key close` |
| scrolled | `keys — 26/36 · ↑↓ more · / filters · any key closes` | `↑↓ PgUp/Dn read on` · `/ filter` · `any other key close` |
| filtering | `keys — /mov · 4 shown · ↵ runs · Esc clears` | `type filter` · `↑↓ PgUp/Dn move` · `↵ run` · `Esc clear · close` |
| filtering, scrolled | `keys — /a · 26/31 · ↑↓ move · ↵ runs · Esc clears` | as above |

[C41] The filtering rows changed when the palette landed. `read on` became
`move` and `↑↓ more` became `↑↓ move` because the arrows now drive a cursor,
not a view — a hint that says "read on" over a cursor is teaching the wrong
model. `↵ runs`/`↵ run` sits between the motion that chooses a row and the
way out, so the bar reads in the order the hands move, and it appears only
when a row under the query is runnable (`help_title`'s `runnable`, for F1's
reason: a title teaching a key that does nothing here is worse than none).
The two un-filtered rows are untouched — that is still C15's poster.

The filtered hint row is C27's roster pairs, for C27's reason. The
unfiltered rows gain only `/ filter` — the affordance has to be visible or
the feature does not exist, which is precisely the state P21 catalogued for
scroll-mode search. All four rows stay inside C9's 100-column budget, and
`the_help_hint_row_narrows_only_once_the_keymap_actually_scrolls` measures
every one of them rather than the two it used to.

**All four wordings come from one function** (`help_title`), because the
dialog's **width is floored by the title's** — see below — and a second
spelling would let the floor guard a string the frame does not draw. §4/§5
lockstep, applied to a modal's own heading.

#### Consequences the first draft had to be told about

- **The dialog is sized for the filtered table**, C14's picker rule applied
  to the surface that borrowed its type-ahead: a query cutting 36 rows to 3
  must not leave a 36-row frame around them.
- **Editing the query resets `top` to 0.** A filtered list is a *new* list
  and an old `top` can point past its end. C14 and C27 both put the cursor
  at the start for the same reason.
- **A dead-end scroll key must not close while filtering.** Un-filtered it
  does — C15's conditional rule, untouched — but losing a live query to an
  over-pressed `↓` is exactly the "the modal you open when you are lost is
  the one with a surprising way out" failure that made U23 reject scrolling
  in the first place.
- **`/` is taught in the overlay's own `Alt+?` row** (`this keymap — /
  filters it`), not only in §8 and the live title. C15's P21 precedent:
  `/ search, n/N` rides the `Alt+c` row rather than taking one of its own,
  because a key the overlay does not print is a key the overlay does not
  teach. The C39 audit found `/` documented everywhere except the surface
  whose job is documenting.
- **Enter and Tab close a live query; Delete does not.** Delete is an
  *edit* key and a reader reaching for it is erasing a typo — losing the
  overlay would be the sharpest possible answer. It has nothing to delete
  (the caret is always at the end of an append-only query), so it does
  nothing, which is what Delete does at the end of any text field. Enter
  and Tab have no meaning on a list with no cursor, so they read as "done"
  — contracted deliberately rather than left as a fall-through.
- **A group whose every row is filtered away contributes no heading**, the
  rule `config.json`'s `disable` already put there. A heading that matched
  while its rows did not would title an empty block.
- **The 80-column floor holds under every query**, not just the empty one.
  Filtering only removes rows, so the widest survivor is never wider than
  the widest row overall — but that is an argument, and two prior audits
  found this floor sitting at exactly its limit with zero slack, so
  `the_help_dialog_fits_the_floor_under_every_query` checks instead.
- **The dialog is never narrower than its own title**, which the filter is
  what made possible. Before it, the table always contained its widest row,
  so the frame was always wider than any heading. A query isolating one
  *short* row breaks that: `/this keymap` left a 33-column dialog under a
  44-column title, and `modal_frame` clipped it — hiding the one sentence
  telling a filtering reader how to get out, on the surface they opened
  because they were lost. `help_layout` floors its width on
  `help_title`'s.

**[Amended 2026-09-01, typing filters — the `/` gate opens]** The rule
above stopped at `/` and said so plainly ("`/` is the only unconditional
new key"). Field use said otherwise: the owner reached for the palette by
*typing* — the reflex C14's picker and C27's roster teach — and the
un-filtered overlay answered by closing on the first letter, which read as
the feature being broken. The roster rule now starts at the first key:

- **A bare printable opens the filter seeded with itself** — any
  `KeyCode::Char` except space, carrying no modifier beyond SHIFT (an
  uppercase letter is still typing). `top` and `cursor` reset to 0 exactly
  as a query edit does. `/` still opens an *empty* query — muscle memory,
  and the route to a query that genuinely starts with a slash (`/` then
  `/`); once filtering, `/` is text as before.
- **`j`/`k` are query text now**, so their un-filtered scroll carve-out is
  retired — the arrows and paging keys keep both jobs (scroll the poster
  un-filtered, drive the palette cursor while filtering). A chorded
  printable (`Ctrl+j`) is not typing and closes like any other chord.
- **What still closes the un-filtered poster:** `Esc`, `Enter`, `Space` —
  Space deliberately: C15's own "the key a reader hits to make it go
  away", and no query starts with one — every non-printable, any click
  (U8's mouse form), and a scroll key with nothing to scroll (C15's
  conditional rule, untouched).
- **The announce table's two un-filtered rows are superseded:** title
  `keys — type to filter · Esc closes` (scrolled:
  `keys — 26/36 · ↑↓ more · type to filter · Esc closes`); hint bar
  `Alt+? all keys` · `type filter` · `Esc close` (scrolled:
  `↑↓ PgUp/Dn read on` · `type filter` · `Esc close`). The two filtering
  rows are unchanged. The overlay's own `Alt+?` row becomes
  `this keymap — type filters it`.
- **"`/` is the only unconditional new key" is withdrawn.** The whole
  printable set is claimed, un-conditionally — which is exactly the trade
  this section documented C27's roster making first, now made on the
  surface that had declined it. What licensed the roster licenses this: a
  list you filter by typing cannot reserve letters (U20), and the way out
  is taught in the title, the hint bar, and by `Esc` itself.
- **`/` and the seeded printables are one entry path now**, which fixed
  two things the audit of this amendment caught: the old `/` branch reset
  only the cursor, so `/` pressed on a *scrolled* poster opened the
  palette with its `❯` off-screen (C41 wants the view holding the row `↵`
  would run — both entries reset `top` with the cursor); and it took `/`
  under any modifier, so `Ctrl+/` opened the filter while `Ctrl+j` closed
  — the SHIFT-only gate now covers `/` like every other printable.

  **Found by driving the overlay in a PTY, not by any unit test**, and the
  reason is worth keeping: every test here looked at the frame *or* the
  title, never at the two together. The same shape as the C15 padding bug —
  each half correct, the pair wrong — and the second time on this branch
  that the check which caught it was "render it and look".

  **That floor is only half the answer, and the audit had to say so.** It
  fixes *content narrower than the title*; a **body** narrower than the
  title is the other half, and `.min(body.width)` re-admitted it — at the
  80-column floor a 46-character query clamped and the frame truncated the
  tail, losing `Esc clears` all over again. Widening cannot help when the
  title already exceeds the terminal, so **the query elides** (`…`) and
  everything after it survives: the count, and the way out. The query is
  the right thing to cut because it is the one part the user can already
  see themselves typing.
- **The floor test asserts on the un-clamped `asked`, never on `size.0`.**
  `size.0` is `.min(body.width)`, so at an 80-column body it reports 80
  whether the dialog fitted or was cut down to it. C39's first floor gate
  read `size.0` and therefore passed by construction — the third tautology
  on this branch, written 160 lines below a corrected test in the same file
  carrying the comment "assert on the ask instead", and five days after
  C15's own amendment named the shape. `HelpLayout::asked` exists so the
  honest assertion is the easy one.
- **The filtering state has its own chrome fixture and its own C24b
  probe.** `chrome_buffers` rendered only the un-filtered overlay, so §2's
  gates never saw the filtering title or hint row; and `every_mode()` lists
  one state per variant, so the sweeps never probed the one surface that
  holds every printable key — precisely where C24b's rule is under real
  pressure. A second list (`extra_mode_states`) covers surfaces-within-a-
  variant without weakening `every_mode()`'s duplicate check, which is what
  makes its exhaustive `match` mean anything.

**[Amended 2026-08-20] The dead-pane keys join the table.** `↵` relaunch,
`f` fresh and `y` copy-resume are **bare keys, not Alt chords** —
`main.rs` claims them out of `InputResult::Forward` while the focused pane
is dead — so §8 has no row for them and C34's chord sweep cannot reach
them. The C9 bar advertised them and nothing else did: P21's case verbatim
("a search nothing advertises is a search nobody finds"), on the one
surface whose job is to prevent it. They now ride one row in `PANES`, next
to the other recovery verb, which is how C15 has always absorbed a
mode-local non-Alt key.

`y` is listed unconditionally although the bar shows it only when there is
a session to resume. The overlay is the whole keymap, not the keymap for
this instant, and every other row here documents a key whose effect depends
on context.

`the_overlay_teaches_every_key_the_dead_pane_bar_advertises` is keyed off
the **bar**, not a written list, because the bar is where a new dead-pane
key would appear first. It walks both bars — `resumable` gates `y`, so the
un-resumable one alone leaves out the key this amendment spends a paragraph
on — and it matches each key against the overlay's **key columns**, not its
text.

That last part is the load-bearing one. The first version searched the
joined overlay text for each key as a substring, and every letter a–z
already appears in `HELP_GROUPS`'s prose — "close pane (confirm if busy)"
alone supplies both `f` and `y`. A future dead-pane key added with no row
would have passed silently; only `↵` was doing any work, by the accident of
occurring once. Worse, the mutation check that "proved" the gate used an
uppercase `Q`, which happens not to appear anywhere: **picking an
unrepresentative mutant is how a vacuous check survives its own
verification.** Both directions are now checked with a lowercase letter
that does appear. Named by the C39 audit; a seventh instance of the shape
in §7's list, with a rule of its own.

**Not in scope.** The report's other half — ordering the groups by context
— is deliberately not built. Two separate reasons, and both are worth
recording because the report proposed it as the cheap alternative:

- Its headline case ("copy mode leads with READING") is **unreachable**.
  C24b's escape hatch sets `mode = Normal` before `Action::Help` runs, so
  `Mode::Help` never learns what it was entered from, and `selection` is
  cleared on the way too (verified against the running code, not reasoned).
  Making it reachable means threading a predecessor mode through the escape
  hatch contracted the same day.
- Its *reachable* case — a dead focused pane — turns out to be a **no-op**.
  `PANES` already leads `HELP_GROUPS`, and it already holds every overlay
  row a dead pane cares about. Reordering would change nothing on screen.
  What was actually missing there was not an order but a *row*, which is
  the amendment above. Found by checking whether the reordering would do
  anything before building it.

### C40 — Mark and pull (`Alt+Shift+x` / `Alt+Shift+v`) — [Added 2026-08-27]

C28 moves the focused pane one tab at a time. That is the whole feature on a
three-tab workspace and a walk on a ten-tab one, and the walk is not free:
every step re-parents the pane into a tab it is not staying in, splitting and
re-splitting layouts on the way past. C40 is the same verb with an arbitrary
destination — **mark here, pull there.**

- `Alt+Shift+x` marks the focused pane. Marking the marked pane unmarks it.
- `Alt+Shift+v` pulls the marked pane into the tab that is active *then*,
  landing it exactly as C28 lands one: same host (the tab's focus target),
  same widest-way split, same refusal below C25's floor, focus following the
  pane, and the source tab removed if the pane was its last.

**Nothing moves on the mark, and that is not a compromise.** A pane is a live
process; it cannot sit in a clipboard between two tabs. This is tmux's
`select-pane -m`, not a cut — the mark is a *name* roost is holding, and the
pull is the whole move, atomic, with C28's refusals intact.

**The mark is visible or it does not exist.** The marked pane is usually in
another tab by the time it matters, so a mark with no chrome would be a mode
the user cannot see. While one is pending, C9's hint bar leads with
`Alt+Shift+v pull marked pane` — a *leading* pair, because pairs drop whole
from the right and this is the one that must survive a narrow terminal. The
flash on the mark names the pull chord (resolved, C34), and the feed records
the arrival as an ordinary move.

**The mark is cleared by every outcome that resolves it** — the pull,
"already in this tab", a marked pane that has since closed — and *kept* when
the destination refuses for want of room, which is the one refusal the user
can fix and retry. It is also cleared when the marked pane closes: pane ids
are recycled (`next_pane_id` is max+1), so a stale mark would eventually name
a stranger rather than merely nothing.

**Only the shifted forms are taken.** Bare `Alt+x` and `Alt+v` stay unbound,
so emacs' `M-x` and `M-v` keep reaching the pane through U5 — the same reason
`b`/`d` were struck out of §8's free pool. And the shifted forms are where
this verb belongs anyway: C28 and C33's rule is that a shifted chord carries
the *pane* where its unshifted sibling carries *you*.

### C41 — The keymap overlay runs what it teaches (`Alt+?` `/` … `↵`) — [Added 2026-08-28]

**No new chord, no new overlay, no second list.** C15's keymap already draws
every binding grouped and labelled; F9 already gave it a `/` type-ahead over
both columns; F1 already made each row declare the *actions* it documents
rather than a compiled-in spelling. That is a command palette in every
respect but one — it could not press the button. C41 is that one keystroke:
while the filter is open, `↵` runs the row under the cursor.

The alternative was a separate palette mode on its own chord. It was rejected
for the reason F1 exists: a second list of commands is a second thing to keep
true, and this table is already the one roost audits (`HELP_GROUPS` is swept
against `Action` for coverage). The palette's command list *is* the keymap.

**Only the filtering state is a palette.** Un-filtered, C15's overlay is
unchanged down to the byte — a poster you read, where any key dismisses it
and no row is marked. `/` is what turns it into a picker, and that is the
state whose title and C9 bar advertise `↵`. This keeps C15's shipped contract
whole rather than widening it, and it is the shape F9 already chose for the
same surface ("the two carve-outs are deliberately different shapes").

**A row is runnable only when it documents exactly one action** — and the
rows that resist are precisely the ones nobody would drive this way:

- `Chords([a])` is one verb with one outcome. Unambiguous.
- A multi-action row — `Family` (`Alt+←↓↑→ / hjkl`, `Alt+Shift+←↓↑→ / hjkl`) or a
  multi-action `Chords` (`previous / next tab`) — is a *direction set*.
  There is no answer to which one `↵` runs, and picking one would be the
  wrong feature anyway: those are the chords you press five times running,
  and a palette that performs one step and then closes is strictly worse
  than the chord it would be standing in for. **The ambiguity and the
  uselessness coincide**, which is why one rule covers both.
- `Text` rows bind nothing — the `CONTROL CLI` block, the glyph legend, the
  dead-pane keys `main.rs` claims. Nothing to run.

What survives is what a palette is *for*: the rare, one-shot, hard-to-
remember verbs — flip split, cycle layout, mark/pull, toggle
raw/float/zoom/feed/roster, undo, rename, quit.

**The cursor indexes commands, not drawn lines.** `↑`/`↓`/`PgUp`/`PgDn`/
`Home`/`End` step it over runnable rows only, so it can never park somewhere
`↵` has no answer for; it is clamped at both ends rather than wrapping (C27's
rule, and the stakes are a rung higher here — the row it lands on is one
keystroke from running); and it is read from `help_lines`, which is the flat
table *before* `help_layout` pours it into one column or two, so a resize
under an open palette cannot move what `↵` is pointing at.

**Editing the query resets the cursor to the first command**, alongside the
`top` reset F9 already did — and for a sharper reason than tidiness. It is
what makes the core gesture work: type until the row is there, press `↵`, no
arrow key. Clamping instead would leave `↵` aimed at a row the reader chose
under a *different* list.

**The view follows the cursor; the cursor does not follow the view.** C27's
rule verbatim — a view that scrolled independently would leave the overlay
pointing at a row nobody can see. Both columns share one `top` because they
are one table. Scrolling up keeps one line of context above the cursor,
because in this table that line is usually the group heading that says what
the command is for; the tighter arithmetic pushed `PANES` off the top the
moment the cursor walked back to the first row.

**The wheel is a motion key too.** [Amended 2026-08-28] While filtering it
moves the cursor one command per notch — the arrow keys' own step, C27's
roster wheel verbatim — because the rule above does not care which device
the motion came from: a wheel that scrolled the view out from under the
cursor leaves the overlay one `↵` from firing a row nobody can see. C41
shipped with the keyboard path converted and the wheel still calling
`help_scroll`, which is exactly that failure;
`the_wheel_moves_the_palette_cursor_rather_than_scrolling_past_it` now holds
it. Un-filtered the wheel still reads C15's poster on by the page, unchanged.

**The mark spends no width.** The cursor is `❯` — C14's picker marker and
C27's roster marker (and C20's feed), the same "`↵` acts on this row"
idiom in its fourth surface — and it is drawn *into the leading space
`help_key_prefix` already opens every key column with*. A marker column of its own would widen every
row by one, which re-trips `elide_key` and moves `HELP_COL_FLOOR`: the exact
accounting C38 and C39 each had to correct once already. Gated by drawing
the row marked and unmarked and asserting that the marked one, with the
glyph put back to the space it spent, is byte-identical to the other. Not by
restating the glyph's width — and not by comparing two calls to a
`help_layout` that never sees the cursor, which is how the original gate
passed by construction.

**`↵ runs` is earned, not assumed.** The title offers it only when the query
actually has a command under the cursor. A query isolating the `CONTROL CLI`
block matches rows but no *commands*, and there `↵` does exactly what it did
before C41 — close. F1's rule applied to the heading: a title teaching a key
that does nothing here is worse than no title.

**The overlay closes before the action lands.** Several of the verbs it can
run open a mode of their own (`Alt+r`'s editor, `Alt+Enter`'s picker,
`Alt+Shift+r`'s rename). Dispatching under a live `Mode::Help` would stack
two modals, and the one the renderer drew would not be the one the keys
reached.

**Not taken: click-to-run.** C15's "any click dismisses it" (the mouse form
of "any key closes it") is unchanged. A click that ran a command would make
the dismissal gesture and the fire gesture the same shape, distinguished only
by where the pointer happened to be.

### On gates that pass by construction
**[Added 2026-08-20, after the eighth instance]**

Eight assertions on this branch turned out to prove nothing, six of them
the same shape — **a test asserting a bound on a value already clamped to
that bound** — and two their generalisation: a comparison whose right side
is satisfied by construction. Recorded here because the count is the argument — each one was
found by a design audit or a deliberate sweep, never by CI, and each made a
check look done when it was not.

The instances, so the shape is recognisable:

1. `help_dialog_fits_the_eighty_column_floor` — `size.0 <= 80` at an
   80-column body, where `size.0` ends in `.min(body.width)`.
2. `one_help_column_fits_the_eighty_column_floor` — the same, one test over.
3. C39's `the_help_dialog_fits_the_floor_under_every_query` — the same
   again, written *160 lines below* the corrected version of (1), which
   carries the comment "assert on the ask instead".
4. `help_fits_the_eighty_column_floor_and_reaches_every_row` — the width
   half had been fixed; the **height** half (`size.1 <= body.height`, where
   `size.1` is `min(tallest, body.height - 2) + 2`) was still vacuous.
5. `help_dialog_clamps_to_the_screen_via_centered_near` — every assertion
   vacuous, and the test's own comment stated the clamps before asserting
   them. Two clamps covered for each other, so deleting either left it
   green.
7. `the_overlay_teaches_every_key_the_dead_pane_bar_advertises` — matched
   single-character keys with `contains` over the whole overlay text, where
   every letter a–z already appears. Only `↵` was load-bearing, by accident.
   Not a clamp this time but the same family: **a comparison whose right
   side is satisfied by construction.** Now matched against the key columns.
8. `tests/help_filter.rs`'s Esc sequence — and the one that bit rather than
   merely lying. Between the two Esc presses it waited for the screen to
   contain `"/ "`, which `Alt+/` and the `Alt+?` row's own "`/` filters it"
   already put there: the predicate was true before the first Esc was
   parsed, so `wait_for` returned instantly and the second ESC byte went
   out on the heels of the first. Two ESCs arriving together fuse into one
   event, so only one Esc landed — the query cleared, the overlay stayed
   open, **and CI went red on macOS while passing on Linux**. Now waits for
   the *transition* (the query leaving the title), which the preceding
   assertion has already proved starts false.
6. `roster_window_follows_the_cursor_and_clamps` — `top + height <=
   rows.len()` asserted after `roster_view`, which had just applied
   `roster_top_clamped` (`top.min(len - height)`). The assertion restated
   the clamp that produced the answer. This one is the clearest case of the
   real cost: **`roster_top_clamped` had no test at all**, its
   `saturating_sub` edge included, because the tautology had been standing
   in for one. Breaking the clamp fails the direct test now and left the
   caller's test green.

Three rules follow, and they are cheap:

- **Never assert on a value the production code clamped.** Assert on the
  *ask* — the pre-clamp want. `HelpLayout::asked` exists for exactly this,
  and a computed quantity that gets clamped on the way out should carry its
  unclamped form when a test needs to bound it.
- **When two mechanisms both enforce a property, a test of the property
  tests neither.** (5) survived deleting `centered_near`'s clamp because
  `help_layout` clamped too. Pick the input that makes exactly one of them
  responsible — in that case, an anchor hugging an edge.
- **Mutation-check the replacement, not just the original.** The first
  rewrite of (5) asserted placement and *still* passed with the clamp
  deleted, because a dialog centred on the whole body is inside it either
  way. Only the mutation showed that; the assertion looked meaningful.
- **A `wait_for` predicate is an assertion too, and a vacuous one races.**
  (8) is the only entry here that failed rather than lied, and it is the
  cheapest to prevent: a predicate that is already true when the wait
  begins does not synchronise anything, so whatever the test does next
  runs against unsettled state. Wait for a **transition** the preceding
  step has proved starts false, never for a condition that merely holds
  afterwards.
- **Pick a *representative* mutant.** (7)'s gate matched single-character
  keys with `contains` over the whole overlay text, where every letter a–z
  already appears; the mutation that "proved" it used an uppercase `Q`,
  which happens not to occur. The check was vacuous and its own
  verification said otherwise. A mutant chosen for convenience tests the
  mutant, not the gate.
- **Test a clamp where it lives, not through a caller that applies it.**
  (6)'s property was real; asserting it downstream of the clamp made it
  unfalsifiable *and* hid that the clamp had no test. A pure helper with an
  edge case (`saturating_sub`, `rem_euclid`, `.max(1)`) deserves its own.

A test that cannot fail is worse than a missing one: the missing test is
visible in coverage, and this one reads as a guarantee.

### C42 — The collapse ladder (`Alt+s`, held) — [Added 2026-09-02, reported flow]

**The gap, as reported.** Four panes built with `Alt+n` — one left, one
top-right, two bottom-right — then `Alt+s` from the bottom-right pane. It
collapsed the innermost split and then **refused every press after**:
*"already stacked."* The reporter's words: "I can't continuously keep on
collapsing panes."

They are right, for two reasons this contract acts on.

**One: the verb has an obvious iteration and nothing said otherwise.** There
is always a next enclosing split, up to the tab root. A user who has just
watched one press collapse a split has no way to learn from the interface
that this was the only rung available — collapsing is a zoom-out gesture, and
zoom-out gestures repeat. `Alt+g`'s layout cycle repeats, `Alt+m` repeats,
`Alt+- = < >` repeat. This one stopping after one press is the odd one out.

**Two: the refusal taught a dead end, and could not have done otherwise.**
"already stacked — {chord} explodes it back into a split" answers "what now?"
with only the *reverse* of what the user just asked for. Worse, it fired
identically whether or not a split remained above — the old `stack_pane` did
not look, so the message could not distinguish "you are at the top" from
"there is more, but this chord will not do it." A refusal that cannot tell
those apart is C38's rule broken from the inside: it says *that* it refused
without being able to say *why*.

**The contract.** `Alt+s` collapses the split that contains the focused pane
— and each press after it absorbs the **next enclosing split** into the same
stack, until the stack holds the whole tab. `stack_pane` collapses a split
that directly holds either the target pane (the first rung) or the stack the
target is already in (every rung after).

- **It terminates, structurally.** Each rung replaces a `Split` subtree with
  a `Stack` leaf, so the tree gets strictly shallower. The ceiling is the
  root.
- **The endpoint is a shape roost already draws.** Every pane of the tab in
  one stack is exactly what `Alt+g`'s `all-stack` preset builds, so C6's
  header, C7's edge marker, C8's fit degradation and `rects` all meet a
  stack they have seen before. This is why the ladder needs no new
  rendering and no new invariant.
- **Flat, not nested.** `pane_order` flattens a nested stack into its member
  ids, so absorbing one yields a flat stack of every leaf below that split —
  the same shape the first rung produces, which is what keeps `Stack`'s
  "children are panes, never nodes" invariant true at every rung.
- **The focused pane stays expanded on every rung**, which is what makes the
  ladder legible: the thing you are looking at does not move while the
  things around it fold away.
- **The ceiling names itself.** The refusal is now reachable only when the
  stack already holds the tab, so it can finally say so: **"the whole tab is
  one stack — {explode chord} explodes it back into a split."** It still
  names the way out (C38), but it no longer claims a rung is a ceiling.

**The round trip had to be fixed with it, and this is the honest part.**
Auditing the ladder surfaced a pre-existing bug in its inverse:
`explode_stack` hardcoded `SplitDir::Horizontal` and even ratios, so
`Alt+s` → `Alt+Shift+s` **rotated** a side-by-side pair into a stacked one
and discarded any widths `Alt+< >` had set. Verified against the reporter's
own tree: panes 3|4 went in `Vertical` and came back `Horizontal`. With
`Alt+u` scoped to closes (C26), nothing walked that back. A one-rung ladder
made this rare; a three-rung one would have made it routine.

So `Stack` gains `from: Option<StackOrigin>` — the direction and ratios of
the split it replaced:

- **`dir` and `ratios` are trusted separately.** A direction means the same
  thing at any member count; a ratio list does not, so it is used only while
  it still has one entry per member. A C42 rung that absorbs another split
  records ratios for fewer slots than the stack ends up holding, so it keeps
  the direction and falls back to even ratios. The length check is not
  defensive politeness — handing two ratios to a four-member split is an
  index straight off the end of `rects`.
- **A length check alone is not enough, and this contract's audit is why
  that is written down.** A member count can change and change *back*:
  collapse a `0.3/0.7` pair, `Alt+n` a pane into the stack, close a
  different one, and there are two members and two remembered ratios again —
  the check passes, describing a split those two panes were never in. So
  `split_pane` and `remove_pane` **clear `from`** when they add or drop a
  member, making the real rule *valid iff the membership is unchanged since
  the collapse*; the length check stays as the second line of defence for
  the rung case above. `swap_in` deliberately does not clear: a swap
  reorders without changing the count, and split ratios are positional, so
  slot 0 keeps its share whichever pane now sits in it. Pinned by
  `a_stack_forgets_its_origin_once_its_membership_changes` and
  `a_swap_inside_a_stack_keeps_the_remembered_shape`.
- **`None` is a real answer, not a missing one**: `all_stack_layout`'s
  stacks, and every stack in a `workspace.json` written before this field,
  explode to the pre-C42 even-horizontal split. That is the fallback, still.
- **Additive in both directions, deliberately.** `#[serde(default)]` means an
  older `workspace.json` loads unchanged; `skip_serializing_if` means a stack
  with nothing remembered writes byte-identical JSON to what it always wrote,
  so a newer roost does not churn files it merely touches. Nothing in the
  crate sets `deny_unknown_fields`, so an older roost reading a newer file
  ignores the key and takes the fallback. No migration, no version bump —
  pinned by `a_stack_origin_round_trips_and_older_workspaces_still_load`,
  which asserts the exact pre-C42 JSON byte for byte.

**One guard the ladder needed and the one-rung version did not.** A stack
costs about a row per member, so a tab collapsed whole can ask for more rows
than the terminal has — past roughly 12 members at 36x10, or 32 at 80x24,
`compute_rects` starts returning **empty rects**. A pane with an empty rect
still has its process and its PTY; it receives no pixels and no resize. It
is gone from the screen while alive, which no chord may cause. So a rung
whose result is not fully drawable is **rolled back** and refused: *"no room
to stack: this tab has more panes than rows."*

The predicate is `every_pane_is_drawable`, deliberately **not**
`arrangement_fits`. `arrangement_fits` is a comfort floor —
`MIN_SPLIT_COLS/ROWS` on the expanded member — and a legitimately tight
terminal fails it while rendering perfectly: a 4-member stack at 36x10 is
"unfitting" by that measure and looks fine. Guarding with it would refuse
the common case to protect the rare one. Drawability is the question with
no matter of taste in it. (`cycle_layout` needs no equivalent guard: its own
`arrangement_fits` check happens to exclude these shapes as a side effect,
which is why this hazard had never surfaced.) Pinned by
`every_pane_is_drawable_separates_tight_from_actually_broken` and
`a_rung_that_would_leave_a_pane_undrawable_is_rolled_back_and_says_so`,
which also pins that a refused rung leaves the tree byte-identical.

**What this deliberately does not do.** `Alt+Shift+s` stays **one press to a
flat split** — it does not descend the ladder rung by rung. Climbing three
levels and exploding once lands you in an even split of all the panes, not
back in your original tree. This is a chosen asymmetry, not an oversight:
the symmetric version needs the stack to remember every shape it swallowed,
and the one-sentence rule ("`Alt+s` collapses more, `Alt+Shift+s` blows the
stack apart") is worth more than an exact inverse a user would have to count
presses to predict. The one-level case — collapse, explode, get your split
back — *is* exact, and that is the case people actually round-trip.

**Chrome.** Nothing new. No chord added, no row added to §8 or C15 (row 5
already spells the pair), no glyph, no colour. C6's header text does the
teaching for free: `STACK · 2 PANES` becoming `STACK · 3 PANES` on the next
press *is* the ladder, visible without a legend. One row's **wording**
changes: C15's stack line read "stack this pane (collapses its split; it
expands)", which describes a single press, and now reads "stack this pane
(repeat to collapse further out)" — same length class, still inside C15's
`HELP_FLOOR_COLS` budget, which `one_help_column_fits_the_floor_under_a_remap_too`
measures rather than assumes.

**The config alias climbs too, and that is the intended reading.**
`toggle_stack` — the parse-only alias §8's 2026-09-01 amendment kept for
`config.json` files written against the old toggle — maps to `StackPane`, so
a user's existing remap now climbs like the default chord. That is what an
alias means: it names the action, not a frozen copy of the behaviour the
action had. Nothing to migrate.

**Tests.** `stack_pane_climbs_one_enclosing_split_per_press_until_the_tab_is_one_stack`
walks the reported tree rung by rung;
`climbing_keeps_the_focused_pane_expanded_at_every_rung` and
`climbing_leaves_untouched_siblings_alone_until_it_reaches_them` pin the two
properties that make it legible; `the_ceiling_is_a_property_of_the_tree_not_of_how_the_stack_was_built`
pins that the climb reads the tree rather than a history;
`holding_the_stack_chord_climbs_quietly_and_names_the_ceiling_once` pins the
flash discipline (silent on every rung, one message at the top); and
`holding_alt_s_climbs_the_tab_pane_by_pane_through_a_real_terminal` drives
it at a real PTY and watches the header count up. The round trip is
`collapse_then_explode_restores_the_splits_direction_and_ratios` plus
`a_stack_with_nothing_to_remember_explodes_to_the_even_horizontal_fallback`
for all three ways to reach the fallback.

## 8. Key table — [Added 2026-07-22, fleet features]

The one canonical list. The help overlay (C15) renders every chord here —
**grouped**, unmerged, and with no row cap since 2026-07-28 (see C15's
amendment; the ≤20-row cap and the merges it forced are gone). The hint bar
shows only the C9-curated subsets.

| # | Chord | Action | Contract |
|---|---|---|---|
| 1 | `Alt+n` | new shell pane (auto split) | — |
| 2 | `Alt+Enter` | quick-launch picker (pi / claude / shell) | C14 |
| 3 | `Alt+←↓↑→ / hjkl` | move focus (`←`/`→` continue into the next/prev tab at an edge) | C31 |
| 4 | `Alt+- / Alt+=` | **resize height: shrink / grow** (vim's Ctrl-w − / +) | — |
| 4a | `Alt+< / Alt+>` | **resize width: shrink / grow** (vim's Ctrl-w < / >) | — |
| 4b | `Alt+Shift+←↓↑→ / hjkl` | **move this pane that way inside the tab (swaps with its neighbour)** | C33 |
| 5 | `Alt+s / Alt+Shift+s` | **stack this pane (collapse its split; it expands) / explode the stack around it into a split** | C6–C8 |
| 6 | `Alt+o` | flip split orientation | — |
| 7 | `Alt+g / Alt+Shift+g` | **cycle layout: grid / main+stack / all-stack, forward / back** | C25/C37 |
| 8 | `Alt+z` | **zoom focused pane (view only; Alt+z again to exit)** | C21 |
| 9 | `Alt+Shift+z` | **floating scratch shell (toggle)** | C22 |
| 10 | `Alt+a` | **jump to next pane that needs you** | C19 |
| 10c | `Alt+;` | **go back to the pane you came from (toggles)** | C35 |
| 10b | `Alt+Shift+a` | **fleet roster: every pane, grouped by tab — jump to one. `Tab`/`Shift+Tab` inside it cycle a status filter** | C27 |
| 11 | `Alt+e` | **activity feed (status / spawns / exits / control)** | C20 |
| 12 | `Alt+r / Alt+Shift+r` | **edit pane (name + parking note, one dialog)** / rename tab | C32/C13 |
| 13 | `Alt+t / Alt+1..9 / Alt+0` | new tab / go to tab / **last tab** | C2 |
| 13b | `Alt+m / Alt+Shift+m` | **next / previous tab (wraps)** | C2 |
| 13c | `Alt+i / Alt+Shift+i` | **move this pane to the next / previous tab (wraps)** | C28 |
| 13d | `Alt+Shift+x / Alt+Shift+v` | **mark this pane / pull the marked pane into this tab** | C40 |
| 14 | `Alt+w` | close pane (confirm if busy / last) | — |
| 15 | `Alt+u` | undo — reopen last closed pane/tab | C26 |
| 16 | `Alt+c` | copy mode (hjkl+v+y, or drag) | C17/C24 |
| 17 | `Alt+PgUp` | scroll mode | — |
| 18 | `Alt+Shift+p` | **raw pass-through for this pane (same chord exits)** | C23 |
| 19 | `Alt+/` | toggle hint bar | C9 |
| 20 | `Alt+?` | full keymap overlay (this table); typing filters it (2026-09-01; `/` still works), **and `↵` runs the row it lands on** | C15, C39, C41 |

[Amended 2026-09-01 (2), the `s` pair. Row 5's toggle is split into two
one-way chords on the shift-reverse idiom the same day's map re-key
established: `Alt+s` stacks (collapse half), `Alt+Shift+s` explodes. A
stacked pane's `Alt+s` now **refuses** — "already stacked — {explode chord}
explodes it back into a split" (C38's name-the-way-out rule) — where the old
toggle exploded silently, so toggle muscle memory in a stack now gets a
message, not a surprise. `toggle_stack` survives as a parse-only config
alias mapped to the collapse half. The layout operations, fit degradation
and expanded-slot repair are unchanged (C6–C8).]

[Amended 2026-09-02, C42 — the collapse ladder. Row 5's `Alt+s` **repeats**:
each press absorbs the next enclosing split into the same stack, until the
stack holds the whole tab. The refusal above is re-aimed with it — it now
fires only at that ceiling, and says so — because a rung is no longer a
refusal. `Alt+Shift+s` is unchanged (one press, one flat split) and now
restores the collapsed split's direction and ratios instead of always
rebuilding an even horizontal one. No chord moves and no row is added; see
C42 for why the asymmetry is chosen rather than missed.]

*Amended 2026-08-07 (row 20, delivery tolerance).* `?` is Shift+`/`, and
terminals disagree about which half of that they report: some deliver
`Char('?')` with the shift folded in, others deliver `Char('/')` and leave
SHIFT in the modifiers. Row 20 is accepted **either way** — `Char('/')`
dispatches on shift, so `Alt+Shift+/` reaches the overlay and unshifted
`Alt+/` still reaches row 19. Same class of tolerance as C23's
Alt+`'P'`, C27's and C28's uppercase forms, and stated here for the same
reason: one physical chord, two spellings on the wire. Reported from real
use on macOS, where the unshifted arm was claiming the event first and row
20 silently did row 19's job.
| 21 | `Alt+q` | quit (workspace saved; sessions live) | — |
| 22 | `Alt+'` | **broadcast: type once, send to every pane (`Tab` picks who)** | C36 |

[Amended 2026-09-03, the Alt+f re-key. Row 9 moves from `Alt+f` to
`Alt+Shift+z`, paired with row 8's `Alt+z` as the same-letter shift-pair
idiom the 2026-09-01 re-key below established for rows 7, 13b and 13c —
`Alt+z` zooms (the view toggle), `Alt+Shift+z` floats (the pane toggle).
`Alt+f` is now unbound and falls through to U5's forward default, closing
the collision-flags paragraph's oldest open item (below): on a terminal
without the kitty keyboard protocol, Alt+Right and Alt+f arrive as
byte-identical meta-ESC pairs (`ESC f`), so binding `Alt+f` at all meant
Alt+Right silently opened the float. See C22's own dated amendment for the
full reasoning and the config.json line back to the old chord.]

[Amended 2026-09-01, the modifier-consistency re-key. Three families moved;
the verb set is unchanged. **(1) Rows 4/4a/4b:** resize leaves
`Alt+Shift+arrows` for vim's Ctrl-w punctuation — `-`/`=` height,
`<`/`>` width — and the arrows join `Alt+Shift+hjkl` as spellings of the
same move-the-pane verb, so Shift+direction now means *move the pane* on
both halves of the keyboard. The four resize actions keep their config
names (`resize_{horizontal,vertical}_{grow,shrink}`). **(2) Rows
13b/13c:** the two tab families are same-letter, shift-reverse pairs,
matching C37's `g`/`Shift+g` — `Alt+m`/`Alt+Shift+m` step next/previous
tab, `Alt+i`/`Alt+Shift+i` carry the pane to the next/previous tab.
`Alt+Shift+i` keeps the direction it always had (carry-previous); bare
`i` and both `m` spellings moved, so Shift+m muscle memory for
carry-next now lands on previous-tab — the overlay teaches the new rows
and `roost keys` prints the effective map. **(3) Delivery twins:** bare
`+` and `,`/`.` with SHIFT are accepted as the `=`/`<`/`>` spellings (the
row-20 rule), the unshifted `,`/`.`/`_` stay free so readline's M-, M-.
M-_ keep reaching the pane (U5), and the `twins` closure now covers
glyph/base pairs — which also closes the pre-existing gap where
`{"alt+?": "disable"}` left the `('/')+SHIFT` delivery of Alt+? live.
**What this family costs, stated rather than implied:** the `<`/`>` glyphs
*are* readline's M-< / M-> (beginning- and end-of-history), so the shifted
half of the width pair shadows two live bindings. U5's guarantee here is
about the **unshifted** keys, not the whole punctuation vocabulary — this
table has always been allowed to spend a key the pane wanted, and C23's raw
mode is the escape hatch, the same one every other chord in it relies on.
The trade was taken because vim's `Ctrl-w <`/`>` is the resize idiom a
multiplexer user already has in their fingers, and history-start/end have
alternatives in every shell that M-,/M-. word-ops do not.]

[Amended 2026-08-19, C34: this table is still the canonical *list* of what
roost binds by default, but it is no longer what the chrome *prints*. The
C15 overlay and the C9 hint bar resolve every chord they name from the live
keymap (defaults merged with config.json), so on a machine with a remap they
correctly disagree with the rows below. The check that they cover this table
is `every_bound_chord_is_documented_in_the_keymap`, rewritten as a sweep over
`input::effective_bindings` — it keeps no list of chord literals, so a chord
added here can no longer go undocumented by anyone forgetting to.]

[Amended 2026-08-15, C32 combined editor: row 12's pane half is the
combined editor now, and **row 12b (`Alt+Shift+n`, added earlier the same
day) is withdrawn** — the chord lived exactly one release (v0.1.7) before
`Alt+r` absorbed the note editor. `n`'s shifted form returns to the free
pool: a terminal reporting it as `n`+SHIFT falls through to row 1's
unshifted arm exactly as before v0.1.7, one reporting uppercase `N` takes
U5's unbound-printable forward. Chord economy was the client's explicit
ask; nothing else in this table moved.]

[Amended 2026-07-22, supervisor SPEC-GAP: row 20 `Alt+?` was bound in
translate() and advertised by C9's hint bar but missing from this canonical
table. The C15 help overlay's ≤20-content-row cap counts key ROWS, some of
which pair two chords — the overlay stays within cap.]

[Amended 2026-07-27, SPEC-ux U7 — tab reachability. `Alt+1..9` stops at
nine, so tabs 10+ had **no keyboard route at all**, and Alt+0/Alt+9 both
no-opped silently in live QA. Three chords close it: `Alt+0` takes the digit
row's own "and the rest" slot (last tab, whatever its number), and
`Alt+i`/`Alt+m` step previous/next with wrap-around, so any tab is a few
presses away and neither end is a dead end.
**Why i and m**, from the free pool `b d i m p v x y 0 PgDn`: `b`/`d` are
struck out by this table's own last line (readline word ops, the
most-missed bindings — and since U5 they really do reach the shell, so
taking them now would be a live regression, not a theoretical one); `p` is
`Alt+Shift+p`'s lowercase twin, left free by C23 so the raw toggle has no
near-miss; `v`/`x`/`y` are the clipboard letters, reserved for copy-mode
vocabulary (`y` is already the yank key inside C24); `PgDn` would pair
asymmetrically with `Alt+PgUp` = scroll mode. That leaves `i` and `m`,
assigned alphabetically — i before m, previous before next.
The overlay (C15) absorbs both rows within its ≤20 cap by merging
`Alt+c`/`Alt+PgUp` (the two look-back modes) and `Alt+/`/`Alt+?` (the two
help toggles) into one row each.
[2026-09-01: i and m were later re-assigned by the modifier-consistency
re-key — `m`/`Shift+m` step the strip, `i`/`Shift+i` carry the pane; see
that amendment above.]

[Amended 2026-08-27, C40 — the `v`/`x` reservation, spent.] `Alt+Shift+x`
and `Alt+Shift+v` are mark-pane and pull-pane (row 13d). That *is* the
clipboard vocabulary the note above reserved them for, one level up: on
panes rather than on text. Only the **shifted** forms are taken — bare
`Alt+x`/`Alt+v` stay unbound, so emacs' `M-x` and `M-v` keep reaching the
pane through U5, the same live-regression argument that struck `b`/`d` out.
`y` is untouched and remains C24's yank key inside copy mode.]

[Amended 2026-07-27, SPEC-ux U23 — this table is no longer the *whole* help
overlay. The chord rows below are still rendered verbatim and in order, but
C15's row list now continues past row 21 with three reference rows (status
legend, mouse verbs, `Alt+click`). Rows 5+6, 8+9 and 14+15 render merged
(`Alt+s / Alt+o`, `Alt+z / Alt+f`, `Alt+w / Alt+u`) to pay for them within
the ≤20 cap — the chords, their meanings and their contracts are unchanged;
only the row packing is.]

[Amended 2026-07-28, C15's cap retirement — **every merge recorded in the
two amendments above is undone.** `Alt+c`/`Alt+PgUp`, `Alt+/`/`Alt+?`,
`Alt+s`/`Alt+o`, `Alt+z`/`Alt+f`, `Alt+w`/`Alt+u` and `Alt+a`/`Alt+Shift+a`
each get their own overlay row again, and the reference rows above become
the overlay's own `READING THE SCREEN` group. The rows are also no longer
rendered "verbatim and in order" from this table: they are grouped by what
the chord acts on (C15's amendment names the groups). This table stays the
canonical *list* — what is bound, and to what contract — and the check that
the two agree is `every_bound_chord_is_documented_in_the_keymap`, which
supersedes the retired `help_keys_fit_the_cap` /
`help_keys_match_the_c8_key_table_verbatim_and_in_order` /
`help_dialog_fits_the_eighty_column_floor` trio named in the older text
(`one_help_column_fits_the_eighty_column_floor` carries the width rule
forward).]

[Amended 2026-07-27, SPEC-ux U16 — rename-dialog editing keys. The dialog
is a text field, and inside it `Ctrl+U` (clear the line) and `Ctrl+W` (rub
out the last word) are edits, matching readline's `unix-line-discard` /
`unix-word-rubout` — the two chords every line editor on the platform
binds. Every *other* modified char is discarded rather than inserted: an
unimplemented chord may never leave its letter in a name (live QA committed
the title `abcwu`). These are mode-local, not entries in this table, and
they do not consume the chords anywhere else. Cursor motion inside the
buffer remains unimplemented.]

[Amended 2026-07-27, SPEC-ux U16/U20 — the two dialogs' mode-local keys,
listed here for the same reason as the blocks below: none is an Alt chord.
Rename (C13): `←`/`→`/`Home`/`End` move an insertion point, `Delete` joins
`Backspace` as its forward twin, and `Ctrl+U`/`Ctrl+W` now cut relative to
the point (kill-to-start and word-behind-point, readline's real
definitions) — the "cursor motion inside the buffer remains unimplemented"
note above is superseded. Picker (C14): any printable filters the adapter
list and `Backspace` widens it, `←`/`→` (or Tab) move between the adapter
and recent-cwd columns, `↑`/`↓` steer whichever has the keyboard, and
`1`..`9` still launch — addressing the filtered rows. `j`/`k` are no longer
picker motions; they are filter text like every other letter.]

[Amended 2026-07-27, SPEC-parity P21 — scrollback search. Three more
mode-local keys, and for the same reason as the block below they join no row
of the table above: none is an Alt chord. In Scroll **and** Copy mode, `/`
opens an incremental search over the focused pane's scrollback + screen, and
`n`/`N` step forward/back through its hits with wrap-around at both ends.
Inside the prompt every printable key is query text — `n`, `N` and `/`
included: a prompt that stole letters from what you are typing would be
unusable. `↵` keeps the result (the hits and their highlights outlive the
prompt), `Esc` cancels back to the view `/` was pressed on. The Scroll→Copy
handoff (`Alt+c`) carries the search across along with the frozen view — the
U9 exemption exists so history can be *found* and then selected; every other
Alt chord ends it. Reachable from the C15 overlay's `Alt+c / Alt+PgUp` row.]

[Amended 2026-07-27, SPEC-ux U17/U19/U20/U25 — mode-local keys. None of
these are Alt chords, so none join the table above; they are listed here so
the canonical page still knows they exist. Copy mode (C24): `w`/`b`/`e`
word motions, `V` line select, `o` open the URL under the cursor, alongside
the pre-existing `0`/`$`. Picker (C14): `1`..`9` launch that row. Feed
(C20): `Enter` focuses the selected entry's pane, and the long-implemented
`PgUp`/`PgDn`/`q` are now advertised. Every one of them is reachable from
the C15 overlay's rows.]

[Amended 2026-07-28, C27 — the roster's own keys, and why it takes a shifted
chord. `Alt+Shift+a` is row 10b above; it is deliberately the shifted sibling
of row 10, because the unshifted pool cannot supply a mnemonic one: `b`/`d`
are live readline word-ops since U5 (taking them now would be a regression,
not a theory), `i`/`m` went to U7's tab navigation, and `p` stays free by
C23's own rule so the raw toggle has no near-miss. `Alt+Shift+r` and
`Alt+Shift+p` are the in-repo precedent for a shifted sibling, and the same
uppercase-delivery tolerance (`Alt+'A'`) rides along.
Mode-local, not Alt chords, so they join no row: `↑`/`↓` move the cursor by
pane, `PgUp`/`PgDn` page, `Enter` goes to the cursor's pane, `Esc` closes, and
**every printable character is filter text** — `j`, `k` and `q` included,
because a list you filter by typing cannot reserve letters (U20's rule, paid
for by the picker). All of them are advertised on the roster's own hint list
(C9) and the chord itself on C15's merged `Alt+a / Alt+Shift+a` row.]

[Amended 2026-08-06, C29, **withdrawn the same day (PR #46 design audit,
D2)**: native selection's gestures are mouse verbs, not keys, and this
table's subject is chords — the withdrawn text placed them here anyway,
telling the reader in the same breath that none of them belonged. C15's
own `READING THE SCREEN` group is where U23 already put the sibling mouse
verbs (wheel/click/drag/Alt+click), and that contract's amendment is where
double-click/triple-click/shift-click now live too. Nothing here changes:
no new Alt chord, no reassigned key, and the free-Alt-keys tally below is
untouched.]

Contextual, non-Alt: dead pane — `Enter` relaunch/resume, `f` fresh, `y`
copy resume command (resumable panes only — C16); raw pane — **every** key
passes through except `Alt+Shift+p` (C23); modes capture their own keys
(C9 lists them).

Control-plane only, no key by design: `roost send --all TEXT [--enter]`
(broadcast — PLAN F2; surfaces in chrome only as a C20 `ctl` feed line).

Free Alt keys remaining after this engagement: `b d p v x y PgDn`
(`i m 0` were taken by U7, 2026-07-27).
[Amended 2026-07-28, C28] Row 13c costs the **unshifted** pool nothing: it
spends `Alt+Shift+i`/`Alt+Shift+m`, the shifted siblings of row 13b, so the
free list above is unchanged. [2026-09-01: row 13c now spends
`Alt+i`/`Alt+Shift+i` (§8's re-key); the pool is unchanged either way.] That
is the fourth use of the shifted-sibling
idiom (`Alt+Shift+r` renames the tab its unshifted form's pane belongs to,
`Alt+Shift+a` lists the fleet its unshifted form jumps through,
`Alt+Shift+p` has no unshifted twin by C23's design) and the tightest
reading of it yet: **shift makes the tab chord take the pane with you.**
Each carries the same uppercase-delivery tolerance (`Alt+I`/`Alt+M`), since
some terminals send a shifted Alt+letter as an uppercase char with no SHIFT
bit.
`Alt+[` / `Alt+]` were the brief's suggestion and are rejected: `ESC [` is
the CSI introducer, so `Alt+[` is indistinguishable from the start of an
escape sequence (the same N3 hazard that kept C23 off `ESC`+`P`; see the
rejection recorded above).
Collision flags (all already swallowed by roost today, `input.rs:72–77`;
raw mode C23 is the remedy): `Alt+f` readline forward-word · `Alt+a` zsh
accept-and-hold · `Alt+b/d` left deliberately free (readline word ops — the
most-missed bindings; do not assign them to chrome without strong cause).
[Amended 2026-09-03: `Alt+f` is no longer one of these — see the Alt+f
re-key amendment after row 22 and C22's own dated amendment. It is not a
"raw mode is the remedy" collision any more; it is unbound outright, and
joins `b`/`d` in the free-Alt-keys list above as the third of readline's
high-frequency word-motion trio. `Alt+a` remains a live, deliberate
collision — zsh's accept-and-hold is a far less common binding than
readline's word ops, and jump-to-attention (C19) earns the trade.]

[Amended 2026-08-07, client request — cross-tab arrow-key focus, C31] Row 3's
`←`/`→` now continue past a tab's geometric edge into the next/previous tab
(wrapping, like rows 13b/13c) instead of stopping dead; `↑`/`↓` are
untouched, for the reason C31 gives (tabs are roost's own horizontal axis,
and all four directions taking on tab-switching semantics would surprise a
vertical split). No new chord and no reassigned key — the free-Alt-keys
tally above is unchanged, and `h`/`j`/`k`/`l`'s existing meanings don't move
either (`h`/`l` already alias `←`/`→` at the `Action::Focus` dispatch, so
they inherit the edge behavior for free; `j`/`k` don't, matching `↑`/`↓`).
See C31 for the tie-break rule, the wrap, and the zoom/float interplay.
