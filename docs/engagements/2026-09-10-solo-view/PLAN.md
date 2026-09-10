# Solo view — implementation plan (v1)

Companion to PROPOSAL.md (the design of record; read its §2 and §3 once).
This is the build order. Anchors are `file:line` as of commit 0c09eed and may
drift a few lines; the named symbol is the anchor.

## Scope for v1 (what ships; what is deferred)

Ships: `TabView` on `Tab` · `Alt+Shift+t` toggle · rail (both tiers) ·
display-list transform · step / cross-tab / reorder / refusals / close
landing · zoom-over-solo · exit guard · rail mouse click · `SOLO` word +
solo hint list + help row · persistence · unit tests + one `chrome_buffers`
fixture + one PTY test · C43 contract text + §8 row + docs.

Deferred (say so in C43): rail overflow scrolling (v1 draws `…` on the last
rail row when rows do not fit, and never scrolls); wheel over the rail
(ignored); skipping the split comfort floor for `Alt+n` in solo (v1 keeps
today's `split_fit` refusal — it reads the tiled rect and may refuse where
the screen looks empty; contract says so); drag-to-reorder.

## 1. Model — `src/core/workspace.rs`

- Add `pub enum TabView { Tiled, Solo }` with `Default = Tiled`,
  `#[serde(rename_all = "snake_case")]`, and `fn is_tiled(&self) -> bool`.
- `Tab` (`workspace.rs:41-46`) gains
  `#[serde(default, skip_serializing_if = "TabView::is_tiled")] pub view: TabView`.
  Fix every `Tab { .. }` literal (grep `Tab {` across `src/` and `tests/`).
- Test (beside the `StackOrigin` round-trip test's shape,
  `layout.rs` ~"older_workspaces_still_load"): a tiled tab serializes with no
  `view` key (byte-identical to before); a solo tab round-trips; JSON without
  the key loads `Tiled`.

## 2. Geometry — `src/core/layout.rs` (pure)

- `pub const RAIL_GLYPH_COLS: u16 = 6;`
- `pub fn rail_width(body_width: u16) -> u16`: `>= 100` →
  `clamp(body_width / 5, 20, 32)`; `>= 40` → 6; else 0.
- `pub fn solo_rects(body: Rect, focused: PaneId) -> (u16, PaneRect)` returning
  the rail width and the shown pane's rect
  `Rect { x: body.x + rw, y: body.y, width: body.width - rw, height: body.height }`,
  `collapsed: false`.
- Tests: rail width at 39/40/99/100/160/200 (0, 6, 6, 20, 32, 32); the pane
  rect keeps `≥ 80` columns at every width from 100 up.

## 3. App state and display list — `src/core/app.rs`

- `App::solo(&self) -> bool` = `ws.active_tab().view == TabView::Solo`.
- `display_rects()` (`app.rs:1062-1083`): after the float, if `zoomed` keep
  today's branch; else if `solo()` → `vec![solo_rects(body, focused).1]`
  (the focused pane; if `focused` is the float, use `tab_focus_target` /
  `first_visible` like the zoom branch does). `rects()` stays the real tree.
- `App::rail_area(&self) -> Option<Rect>`: `Some` iff `solo() && !zoomed &&
  rail_width(body.width) > 0` — the leftmost `rw` columns of `body_area()`.
- `App::rail_rows(&self) -> Vec<PaneId>` = `pane_order()` of the active tab.
- `relayout` (`app.rs:3271-3282`) already sizes from `display_rects()`; verify
  that entering/leaving solo and stepping call it (the zoom toggle is the
  precedent — copy where `toggle_zoom` triggers relayout/save).
- Stack headers: `render.rs:52-56` skips them while zoomed; also skip while
  `solo()`.

## 4. Actions — `src/ui/input.rs`, `src/core/app.rs`

- `Action::ToggleSolo`; bind `Alt+Shift+t` and `Alt+'T'` in
  `default_chord_action` (`input.rs:169-334`, copy the `Alt+Shift+z`/`Alt+Z`
  pair at 264-265); config name `toggle_solo` in the keymap parser (grep
  `"toggle_zoom"` for the table); `twins` picks up the pair automatically
  when both default to the same action. Help overlay: add the row in the
  same group as `Alt+z` (grep `help_rows`/`ToggleZoom` in `render.rs`);
  text: `solo view: one pane at a time, the rest listed beside it (again to
  tile)`. `every_bound_chord_is_documented_in_the_keymap` must pass.
- `App::toggle_solo()`:
  - entering: set `view = Solo`, exit zoom is NOT required (zoom may stay;
    simplest: call `exit_zoom()` on entry so the first frame shows the rail),
    hide the float if focused (C22 rule 3 pattern), `relayout`, save.
  - leaving: guard — compute the real tree's rects for `body_area()` and
    refuse with `set_flash(format!("can't tile {n} panes at {w}×{h} — close
    some, or widen the terminal"))` if `!every_pane_is_drawable(..)`
    (`layout.rs:907`); else set `Tiled`, `relayout`, save. Focus unchanged
    (`set_focus`'s `expand_in_stacks` already ran on every focus change).
- `apply()` dispatch (`app.rs:4041-4267`), **before** the structural guard at
  `4083-4094` (so a refusal never first exits zoom):
  - `StackPane | ExplodeStack | FlipSplit | Resize{..} | CycleLayout{..}` and
    `MovePane(Left|Right)` while `solo()` → refuse. Message for the shape
    verbs: `solo view — {chord} tiles this tab` where `{chord}` is
    `chord_for(Action::ToggleSolo)` (C34: read the live keymap, see how
    C42's ceiling message names the explode chord). `MovePane(Left|Right)`:
    `solo view — {chord_i} / {chord_shift_i} moves a pane between tabs`.
  - `MovePane(Up|Down)` while `solo()` → swap the focused pane with its
    previous/next id in `rail_rows()` via `layout::swap_panes`; clamp
    silently at the ends; relayout+save (same as C33's swap path).
  - `Focus(Up|Down)` while `solo()` → focus previous/next in `rail_rows()`,
    clamped silently. `Focus(Left|Right)` while `solo()` → go straight to
    `focus_dir_cross_tab` (`app.rs:4663`) — the tiled `neighbor` must not
    be consulted (it would walk the hidden tree).
- `close_pane_id` (`app.rs:2700-2856`): when the closing pane is in a solo
  active tab, before removal record `next = rail_rows()[i+1]` else
  `rail_rows()[i-1]`, and after removal focus `next` if it still exists,
  instead of the `tab_focus_target`/`first_visible` fallback.
- Zoom: no change to `toggle_zoom`; `display_rects` zoom branch already wins.
  Tab change exits zoom (today) and the tab returns to solo-with-rail — no
  code, just a test.

## 5. Render — `src/ui/render.rs`

- In `draw` (`render.rs:23-111`), after the tab bar and before panes: if
  `app.rail_area()` is `Some(rail)`, call `draw_rail(f, app, rail)`.
- `draw_rail`: row 0 = header via a new `rail_header_text(width, n)`
  (labelled: `" SOLO · {n} PANES"` left + `"ALT+↑↓ "` right, space-filled
  exactly like `stack_header_text` at `render.rs:2755`; glyph tier:
  `" SOLO"` padded), painted `theme::quiet().add_modifier(UNDERLINED)`.
  Rows 1.. = one per `rail_rows()` id: labelled tier → `collapsed_row_spans`
  (`render.rs:2700`, focused = `id == app.focused`) at `rail.width`; glyph
  tier → marker + glyph + `" {id}"` padded to 6 (marker `▎` `accent()` when
  focused; glyph via the same status glyph helper the collapsed row uses).
  If rows exceed `rail.height - 1`, draw `…` (`quiet()`) on the last rail
  row instead of that row.
- Mode word (`mode_word`, `render.rs:348`): add `solo: bool` param; order
  `RAW` > `ZOOM` > `SOLO` > `NORMAL`. `tab_status_word` (`render.rs:2165`)
  mirrors it.
- Hint pairs (`hint_pairs`, `render.rs:156`): a `solo` flag; Normal+solo
  (not dead, not raw) = `Alt+? keys` · `Alt+↑↓ pane` · `Alt+←→ tab` ·
  `Alt+n new` · `Alt+w close` · `{toggle} tile` — chords resolved from the
  live keymap like the existing pairs (C34). Test that it measures ≤ 100
  columns.
- Fixture: add a solo-tab case to `chrome_buffers()` (grep it in render.rs)
  and assert the header text, the `▎` on the focused row, and that the pane
  border starts at `x = rail width`.
- Run the `design-supervisor` agent after this file changes (CLAUDE.md).

## 6. Mouse — `src/ui/mouse.rs`, `src/main.rs`

- `pub fn rail_row_at(rail: Rect, rows: &[PaneId], col: u16, row: u16) -> Option<PaneId>`:
  `None` on the header row or outside; row index `row - rail.y - 1`.
- `main.rs` `handle_mouse` (`main.rs:918`): after the tab-strip branch
  (954-983) and before seam/pane hit-testing, if `app.rail_area()` is `Some`
  and the event is a left press inside it → `rail_row_at` → `app.on_click(id)`
  (`app.rs:3644`); wheel inside the rail → consumed, no-op. Unit test the
  pure helper.

## 7. Tests

- `app.rs` unit tests (use the existing test `App` builders — grep
  `fn app_with` / `two_pane_app` in the tests module): toggle twice leaves
  `active_tab().layout` equal; `display_rects` is one rect at
  `body-minus-rail` in solo; per-tab independence (tab 0 solo, tab 1 tiled);
  step up/down clamps; `Focus(Left)` in solo changes tab; `MovePane(Up)`
  swaps and persists; each refused action leaves the tree equal and sets the
  contracted flash; close lands below then above; zoom hides the rail
  (`rail_area() == None`) and `toggle_zoom` restores it; exit guard refuses
  on a tiny body.
- Add `ToggleSolo` (and a solo-aware `Focus(Up/Down)`) to the invariant
  fuzzer's action pool (`app.rs:13396`) — cheap and catches the expand
  invariant.
- `render.rs`: `mode_word` precedence; solo hint list width; fixture above.
- PTY: `tests/solo_view.rs` using `tests/harness/mod.rs` (`two_panes`
  fixture; `Harness::try_spawn`, `wait_for`, `screen`): send `Alt+Shift+t`
  (see how `tests/rekeyed_chords.rs` sends `Alt+Shift+s`), wait for
  `"SOLO · 2 PANES"` on screen; send `Alt+↓` and assert the `▎` moved to the
  second rail row; send `Alt+Shift+t` again and assert `STACK`/two borders
  are back. Keep it to one test function.

## 8. Docs

- `DESIGN-ui.md`: append **C43 — Solo view (`Alt+Shift+t`) — [Added
  2026-09-10]** before §8: a condensed contract (model, tiers, keys table,
  mouse, chrome, persistence, deferred list), pointing at PROPOSAL.md for
  the rationale. Add the §8 key-table row (`23 | Alt+Shift+t | solo view:
  one pane at a time, the rest listed beside it | C43`) and a dated
  amendment noting C9's solo list, C21's zoom-over-solo interplay and the
  free-pool note (spends a shifted sibling, unshifted pool unchanged).
- `docs/KEYBINDINGS.md` and README key list: one row each.
- ROADMAP: flip the `[proposed]` entry to `[done]` in one sentence, keep the
  links.

## 9. Gates before pushing

`cargo fmt` · `cargo clippy --all-targets -- -D warnings` (try
`rustup toolchain install 1.96.1 --profile minimal --component rustfmt,clippy`
first and use `cargo +1.96.1` if it installs; otherwise the local stable
and say so) · `cargo test` (unit + PTY). Commit in two commits: code+tests,
then docs. Do not open a PR.
