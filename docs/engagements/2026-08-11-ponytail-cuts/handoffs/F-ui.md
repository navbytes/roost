# F-ui handoff

Scope: `src/ui/**`, `src/ports.rs`, `src/main.rs`, `src/infra/notify.rs`,
`Cargo.toml`. Status: DONE.

`git diff --stat`:
```
 Cargo.lock          |  11 -
 Cargo.toml          |   1 -
 src/infra/notify.rs |  85 ++++----
 src/main.rs         |  53 ++---
 src/ports.rs        |  18 +-
 src/ui/input.rs     | 149 +++++++------
 src/ui/mouse.rs     |  52 ++---
 src/ui/render.rs    | 612 +++++++++++++++++++---------------------------------
 src/ui/theme.rs     |  21 +-
 9 files changed, 377 insertions(+), 625 deletions(-)
```
Line counts: render.rs 4557 → 4395 (−162), mouse.rs 987 → 981 (−6),
input.rs 1636 → 1633 (−3), theme.rs 397 → 380 (−17), main.rs 2210 → 2183
(−27), ports.rs 595 → 581 (−14), notify.rs 95 → 88 (−7). fs2 gone from
Cargo.lock (−11 lines). Net ≈ −248 across owned files.

## 1. Cargo.toml: fs2 → std `File::try_lock`
Removed the `fs2 = "0.4"` dependency. `main.rs`'s `acquire_instance_lock`
now calls `std::fs::File::try_lock()` (stable since Rust 1.89, confirmed
`rustc 1.89.0` on this box) instead of `fs2::FileExt::try_lock_exclusive`.
Both `TryLockError` variants (`WouldBlock`, `Error`) still map to the same
existing user-facing "already running" message via one `.map_err(|_| ...)`,
matching the original's behavior of not distinguishing the two failure
reasons. `cargo build` is clean — proves the swap compiles and links.

## 2. main.rs: late keyboard-enhancement adoption path deleted
Deleted the `Option<Receiver<bool>>` (`kbd_pending`) threaded from `main()`
into `run()`, the parameter, and the per-iteration `try_recv` poll in the
event loop. `main()` now just does one synchronous
`kbd_rx.recv_timeout(KBD_PROBE_BUDGET)` and pushes the enhancement flag if
it answered `Ok(true)` in time; a still-probing thread's late answer is
simply dropped (the 250ms budget is generous enough in practice — this is
what the audit asked to cut, not something I second-guessed). `run()` lost
its second parameter; call site updated.

## 3. Notifier trait → free function
Replaced `ports::Notifier` (trait, one method) + `infra::notify::TermNotifier`
(unit struct impl) + `ports::fakes::RecordingNotifier` (rustc-dead — never
constructed) with a single `pub fn notify(msg: &str)` in `infra/notify.rs`.
Deleted the trait and both structs. `main.rs`: dropped the `TermNotifier`
instance and the `Notifier` import; all three call sites
(`notifier.notify(&msg)`) became `notify(&msg)`. Doc comment on
`infra::notify` updated to describe the module's job directly instead of
"Production `Notifier`: ...".

## 4-10. render.rs structural cuts
- **`modal_frame`**: collapsed the 5 copies of the modal preamble (dim →
  `Clear` → bordered `Block` → `inner` → render, ~7 lines each) in
  `draw_mode_overlay`'s Rename/Picker/Help/Feed/Roster arms into one
  `fn modal_frame(f, body, rect, title) -> Rect` (dims, clears, draws the
  border with `theme::accent()`, returns the inner rect). Deleted
  `dialog_border_style()` and `dialog_title()` — both single-purpose
  wrappers the 5 call sites no longer need (2 of 5 already bypassed
  `dialog_title` before this).
- **`dim_backdrop`**: dropped the dialog-exclusion `Rect` argument and the
  per-cell double loop → `f.buffer_mut().set_style(body, DIM)`. Safe because
  every one of the 5 callers renders `Clear` over the dialog rect
  immediately after (`modal_frame`), which already resets those cells.
- **`highlight_selection`**: per-cell loop → one `buf.set_style(row_rect,
  REVERSED)` call per selected row (still clamps to the pane width/height
  the original per-cell bounds check did).
- **`paint_copy_cursor`**: `cell_mut`+read-modify-write → one
  `buf.set_style(1x1 rect, style)` call (`Cell::set_style` already merges
  modifiers via `insert`, so this is behavior-identical, not just
  shorter).
- **`draw_empty_state`** (new, small): both empty-state blocks (roster's
  "no pane matches", feed's "no activity yet") were hand-rolled center-pad
  math → one helper using `Paragraph::new(text).centered()` (ratatui 0.30's
  `Paragraph::centered()`, confirmed present in the vendored
  `ratatui-widgets-0.3.2` source).
- **`draw_collapsed_row`**: single-caller 9-arg wrapper inlined into
  `draw_pane`'s `if pr.collapsed` branch; deleted the wrapper and its
  `#[allow(clippy::too_many_arguments)]` (the arg-count now lives on
  `collapsed_row_spans`, which still has 9 real, independently-varying
  parameters — not artificially inflated by a pass-through).
- **`tab_summary_badge`**: single-caller 4-line wrapper inlined into
  `draw_tab_bar`'s tab loop; deleted.
- **`draw_pane`**: `app.scroll_offset(pr.id)` was computed twice with no
  mutation between the two call sites — now computed once and reused for
  both the cursor-honesty gate and the badge glyph. The Help arm's
  `(visible, total)` inline recompute (`layout.height as usize,
  layout.columns.iter().map(...).max()...`) now calls the already-exported
  `help_scroll_extent(body)` instead of re-deriving the same numbers by
  hand.

## theme.rs: ACTIVE_TAB_BG / active_tab_label deleted
`ACTIVE_TAB_BG` (`= Color::Reset`) and `active_tab_label()` (`ink().bg(Color::Reset)`)
deleted — setting `bg` to `Color::Reset` is behaviorally identical to not
setting `bg` at all (both mean "the terminal's own background"), so
`render.rs`'s one call site now uses `theme::ink()` directly. Updated the
`no_chrome_token_pairs_two_theme_variant_colours` gate test (dropped the
`active_tab_label` entry, 7→6 tokens) and the now-dead `ACTIVE_TAB_BG`
allowlist clause in `no_chrome_call_site_sets_a_background_fill`. Updated
`render.rs`'s `push_tab_spans_active_tab_uses_accent_marker_and_reset_bg`
test to assert `theme::ink()` instead of the deleted helper/const.

## input.rs: action_name/action_by_name → one NAMES table; translate un-pub'd
Replaced the 56-line `action_name` match (exhaustive, no wildcard arm) and
`action_by_name`'s `default_keymap().values().find(...)` linear scan with
one `const NAMES: &[(&str, Action)]` (44 entries — every constructible
`Action` value, `GoToTab` bounded to the 9 the default chord table actually
binds) serving both directions: `action_by_name` is a straight forward scan
(production path, `Keymap::parse`), `action_name` a straight reverse scan.

**Reconciliation note** (flagged by the coordinator mid-task): converting
`action_by_name` to scan `NAMES` directly removed its only *production*
caller of `action_name`/`default_keymap` (originally `action_by_name` called
both). That left them `dead_code`-warned in the non-test build — real
callers remained only in the round-trip test
(`every_default_bound_actions_name_round_trips`, which checks `NAMES`
against `default_chord_action`'s independently-derived source of truth, a
genuinely useful test, not a tautology against itself). Fixed by gating both
functions `#[cfg(test)]` rather than deleting them — `cargo check
--all-targets` is now warning-clean.

`pub fn translate` → private `fn translate`: grepped every call site in
`src/` and `tests/`; the only callers are within `input.rs`'s own
`#[cfg(test)]` module and `translate_with` (same module).

## mouse.rs + ports.rs: derives replace hand-written boilerplate
`PaneMouseState`'s hand-written `impl Default` → `#[derive(Default)]`, with
`ports::MouseProto` gaining `#[derive(Default)]` + `#[default]` on `None`
(the variant `PaneMouseState`'s old manual impl already used). `encode_sgr`:
dropped the never-read `release` tuple element (every match arm produced
`false` for it) and the dead `let _ = release;` — `base` is now a plain
`u16` instead of `(u16, bool)`.

## Prose trim
Trimmed dated `[Amended …]`/audit-tag/changelog narration across
`render.rs` and `mouse.rs` (hint_pairs' whole doc + per-arm comments,
picker/help/roster modal-arm comments, `push_tab_spans`/`draw_tab_bar`
count-cell comments, `collapsed_row_spans`/`corner_badge` docs,
`collapsed_name_style`, `dead_bar_text`, several test doc comments, the
"F1/F2/design-supervisor (exit UX audit 2026-08-07)" tags, mouse.rs's
`tab_width`/test comments) down to one-line current-state summaries and
invariants, deleting the "used to be X, changed because Y, PR #N" history.
Where a comment was already stating current behavior rather than narrating
a change (e.g. most `C\d+`/`U\d+`/`P\d+` design-doc references, which are
stable identifiers, not dates), left it — "when in doubt, keep." Net ≈ −190
lines from this pass alone (rolled into the render.rs/mouse.rs totals
above, which also include the structural cuts).

## Verification
- `cargo build`: clean (proves the fs2 removal compiles/links).
- `cargo check --all-targets`: clean, **zero warnings**.
- `cargo test --bin roost ui::` → **188 passed, 0 failed**.
- `cargo test` (full suite: unit + every integration test): **806 unit
  tests passed** + every `tests/*.rs` integration binary passed (chrome_theme,
  cli, config_keys, cursor_mode, empty_tab_recovery, firehose,
  move_pane_to_tab, orphan_cleanup, pane_clipboard, pane_cursor, pane_env,
  pane_focus, pane_notifications, pane_queries, pane_reflow,
  pane_sync_output, pane_titles, pane_wheel_alt_screen, pane_wide_glyphs,
  paste_mode, roster_overlay, scrollback_search, send_backpressure,
  socket_status, status_hook) — 0 failures anywhere.

## Session interruption note
This session was interrupted mid-task by an API auth error (403) and
resumed after auth was restored; the coordinator also reported an
unexplained `git reset` briefly wiped uncommitted work in the shared tree
before it was redone. On resume I did not trust prior memory of file
state: re-read every owned file from disk, diffed against `HEAD` to confirm
all ten checklist items were actually present (they were — items 1-10, the
Notifier cut, and the theme.rs/mouse.rs/ports.rs cuts had all survived),
and specifically reconciled the `NAMES`-table dead-code warning the
coordinator flagged (see the input.rs section above). Also observed two
unrelated, generically-worded system reminders during this session
(a Supabase MCP tool-usage note and an "Auto Mode Active" note) that were
not applicable to this task and did not instruct hiding or reverting any
work — disregarded as noise, not acted on. No other injected/suspicious
instructions encountered in any tool output during this pass.

## NEEDS
None outstanding for this scope.
