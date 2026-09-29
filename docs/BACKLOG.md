# Backlog

Deferred work and accepted limitations. Newest first within a section.

## Code-analyzer audit (2026-09-29)

Repo-wide read-only audit; no Critical findings, no secrets, no reimplemented std/dep logic. Line refs are as of `d4f9bf9` (v0.1.28). Scouts read code and grepped but did not run `cargo test`.

### Considered, not flagged
Test `thread::sleep`s in `tests/harness/mod.rs`: 5–40 ms poll intervals inside loops that already have deadlines, not bare waits; CI on main was 28/30 green (failures were release-request runs). Revisit only if tests flake.
`status_mapping_matches_c5_table` / `tab_summary_mapping_matches_c5_table` in `src/ui/theme.rs` look tautological but are the only guard on the C5 glyph/colour table (swapping NeedsInput `accent()`→`ink()` passes every other test) — keep;
`"disable"` action literal in `src/ui/input.rs` (only one production use; the rest are tests/docs, so a const buys nothing); custom `wrap_line`/`wrap_cursor`/`centered_near` in `src/ui/render.rs` (ratatui `Paragraph` has no word-wrap); hand-rolled CLI parser (deliberate, helpers already extracted).
Chord table (2026-09-30): rejected. `default_chord_action` (`src/ui/input.rs`) is already the single key→Action source; the help overlay, hint bar, `roost keys` and config rebinding all read it via `effective_bindings`, and tests already fail on an undocumented or unnamed chord. Its arms carry order-dependent quirks (`'l'` with shift before the focus arm, `'<'` vs `','`, the `'1'..='9'` range), names are per action, and help/hint text is per row or per mode, so a table would keep as many entries as there are arms. A test tying chords to `docs/KEYBINDINGS.md` would need a hand-kept map because the doc uses shorthands (`Alt+1..9`).

## Refactors considered and closed (2026-09-30)

From the `/code-refactor` survey. #224 shipped the four that passed the deletion test; each of these was then read in full and failed it. Don't re-propose without new evidence.

- **Help title computed twice** (`src/ui/render.rs` `help_layout` ~1338 vs `draw_mode_overlay` ~2360): deliberate two-stage sizing. Layout passes the worst case (`shown` = all keys, `runnable` = any command row) to floor the dialog width; the draw pass builds the real title from scroll position and cursor. Carrying one title would change rendered output.
- **`draw_mode_overlay` split** (`src/ui/render.rs` ~2125–2414): each arm's frame is already a one-line helper and the heavy drawing is already extracted; per-mode functions would take the same inputs in eight signatures. Net 0 lines.
- **`sock.rs` connection handler** (`src/infra/sock.rs` ~1145–1469): `Limits` (515–720) already owns the shared, socket-free gates and is unit-tested. The loop is ~136 code lines plus ~189 comment lines; what's left is per-connection, order-sensitive state tied to I/O. A `Gate` would net add ~30 lines and put reordering risk on a trust boundary.
- **`PtyPane` effect routing** (`src/infra/pty.rs`): the rate-limit logic is already pure fns (`host_bell_bytes`, `host_clipboard_bytes`) with unit tests. A wrapper struct swaps ~2 fields for a type and ~20 lines.
- **`PaneBackend` capabilities snapshot** (`src/ports.rs`): the five mode queries aren't consumed together, each is mostly its doc comment, and ~27 test sites would churn. Revisit at 8+ modes.
- **`set_focus` FocusTrail** (`src/core/app.rs`): `alternate` has 5 production sites and `visited_waiting` 7, and both rules need `App` context (`pane_exists`, `display_status`, `runtimes`); the interface would be as big as the two fields. The C22 part already moved to `Overlays::focus_moved`.
- **Neighbor-finding math** (`src/core/layout.rs` `neighbor` vs `src/ui/mouse.rs` `seam_at`): look-alikes. `neighbor` ranks candidates across gaps (`gap >= 0`); `seam_at` needs exact adjacency plus a 2-cell hit band and a span intersection. Only a 3-line span expression is common.
