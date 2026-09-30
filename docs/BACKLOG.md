# Backlog

Deferred work and accepted limitations. Newest first within a section.

## Code-analyzer audit (2026-09-29)

Repo-wide read-only audit; no Critical findings, no secrets, no reimplemented std/dep logic. Line refs are as of `d4f9bf9` (v0.1.28). Scouts read code and grepped but did not run `cargo test`.

### Considered, not flagged
Test `thread::sleep`s in `tests/harness/mod.rs`: 5–40 ms poll intervals inside loops that already have deadlines, not bare waits; CI on main was 28/30 green (failures were release-request runs). Revisit only if tests flake.
`status_mapping_matches_c5_table` / `tab_summary_mapping_matches_c5_table` in `src/ui/theme.rs` look tautological but are the only guard on the C5 glyph/colour table (swapping NeedsInput `accent()`→`ink()` passes every other test) — keep;
`"disable"` action literal in `src/ui/input.rs` (only one production use; the rest are tests/docs, so a const buys nothing); custom `wrap_line`/`wrap_cursor`/`centered_near` in `src/ui/render.rs` (ratatui `Paragraph` has no word-wrap); hand-rolled CLI parser (deliberate, helpers already extracted).
Chord table (2026-09-30): rejected. `default_chord_action` (`src/ui/input.rs`) is already the single key→Action source; the help overlay, hint bar, `roost keys` and config rebinding all read it via `effective_bindings`, and tests already fail on an undocumented or unnamed chord. Its arms carry order-dependent quirks (`'l'` with shift before the focus arm, `'<'` vs `','`, the `'1'..='9'` range), names are per action, and help/hint text is per row or per mode, so a table would keep as many entries as there are arms. A test tying chords to `docs/KEYBINDINGS.md` would need a hand-kept map because the doc uses shorthands (`Alt+1..9`).

## Open small cleanups (2026-09-30)

Found while verifying the closures below; each is a few lines, no design needed. Line refs at `8ac7359`.

- [ ] **Help frame still runs `help_layout` twice** (`src/ui/render.rs`: `dialog_rect` ~2102 and `draw_mode_overlay` ~2334; the third, inside `help_scroll_extent`, is gone — the extent now derives from the layout in hand via `help_extent`). Removing the last needs the layout threaded out of `dialog_rect`, which is called from several places. Touches `src/ui/**`: design-supervisor audit.

## Refactors considered and closed (2026-09-30)

From the `/code-refactor` survey. #224 shipped the four that passed the deletion test; each of these was then read in full and did not. Verified independently afterwards. Don't re-propose without new evidence.

- **Help title computed twice** (`src/ui/render.rs` `help_layout` ~1363 vs `draw_mode_overlay` ~2362): the two calls pass different values by design (layout: worst case `shown`/`runnable` to floor the dialog width; draw: keys scrolled into view and cursor state), so one title cannot serve both and the string really changes when the table scrolls. The real waste is the triple `help_layout` call above.
- **`draw_mode_overlay` split** (`src/ui/render.rs` ~2125-2414): only Help, Feed and Roster delegate their drawing; PaneEdit (~57 lines), Picker (~52) and Broadcast (~42) are still inline. Each arm's locals stay in the arm and each frame is already a one-line `modal_frame`, so a split relocates lines without narrowing signatures. Taste, not a defect.
- **`sock.rs` connection handler extraction** (`src/infra/sock.rs` ~1145-1469): `Limits` (~515-720) already owns the shared socket-free gates and is unit-tested; the loop is ~133 code lines plus ~189 comment lines of per-connection, order-sensitive state. A `Gate` type is not worth it. (The one genuine duplication was the "promote this connection" step copied on the control and status paths; the copies had drifted into a real bug, and branch `fix/sock-status-promotion` is replacing them with one shared helper. Once that lands this note is history.)
- **`PtyPane` effect routing** (`src/infra/pty.rs`): the rate-limit logic is already pure fns (`host_bell_bytes` ~155, `host_clipboard_bytes` ~196) with unit tests; the desktop-notify gate (~700-705) is tested through a real `PtyPane` (~1815). The "last one too recent" check appears three times, a trivial fold at most.
- **`PaneBackend` capabilities snapshot** (`src/ports.rs`): the mode queries are not consumed together on the trait (except the pair above, which is a `main.rs` helper, not a trait change), each is mostly its doc comment, and ~23 test sites would churn. Revisit at 8+ modes.
- **`set_focus` FocusTrail** (`src/core/app.rs`): `alternate` has 5 sites and `visited_waiting` 7 (also cleared with `last_status`/`needy_msgs` on pane death, ~1568/~2831), and both rules need `App` lookups (`pane_exists`, `display_status`); the interface would be as big as the two fields. The C22 part already moved to `Overlays::focus_moved`.
- **Neighbor-finding math** (`src/core/layout.rs` ~547-602 vs `src/ui/mouse.rs` ~105-134): look-alikes. `neighbor` ranks `(!overlaps, gap, centre distance)` across gaps (`gap >= 0`, does not skip collapsed panes); `seam_at` needs exact adjacency, a 2-cell hit band, and skips collapsed panes. Only a 3-line span expression is common.
