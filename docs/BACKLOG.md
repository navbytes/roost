# Backlog

Deferred work and accepted limitations. Newest first within a section.

## Code-analyzer audit (2026-09-29)

Repo-wide read-only audit; no Critical findings, no secrets, no reimplemented std/dep logic. Line refs are as of `d4f9bf9` (v0.1.28). Scouts read code and grepped but did not run `cargo test`.

### Considered, not flagged
Test `thread::sleep`s in `tests/harness/mod.rs`: 5–40 ms poll intervals inside loops that already have deadlines, not bare waits; CI on main was 28/30 green (failures were release-request runs). Revisit only if tests flake.
`status_mapping_matches_c5_table` / `tab_summary_mapping_matches_c5_table` in `src/ui/theme.rs` look tautological but are the only guard on the C5 glyph/colour table (swapping NeedsInput `accent()`→`ink()` passes every other test) — keep;
`"disable"` action literal in `src/ui/input.rs` (only one production use; the rest are tests/docs, so a const buys nothing); custom `wrap_line`/`wrap_cursor`/`centered_near` in `src/ui/render.rs` (ratatui `Paragraph` has no word-wrap); hand-rolled CLI parser (deliberate, helpers already extracted).
Chord table (2026-09-30): rejected. `default_chord_action` (`src/ui/input.rs`) is already the single key→Action source; the help overlay, hint bar, `roost keys` and config rebinding all read it via `effective_bindings`, and tests already fail on an undocumented or unnamed chord. Its arms carry order-dependent quirks (`'l'` with shift before the focus arm, `'<'` vs `','`, the `'1'..='9'` range), names are per action, and help/hint text is per row or per mode, so a table would keep as many entries as there are arms. A test tying chords to `docs/KEYBINDINGS.md` would need a hand-kept map because the doc uses shorthands (`Alt+1..9`).

## Deferred refactor candidates (2026-09-30)

From the `/code-refactor` survey; the strong ones shipped in #224. Line refs are from scouts' reads at `20d35de` and were only spot-checked. Pick one up only when a change makes its friction real.

- [ ] **Help title computed twice** (small, no design needed): `help_layout` (`src/ui/render.rs` ~1338) computes the title to floor the dialog width and `draw_mode_overlay` (~2346) recomputes it. Carry it on `HelpLayout`. Check first the recompute isn't deliberate (title state may change in between). Touches `src/ui/**`: design-supervisor audit.
- [ ] **`sock.rs` connection handler** (`src/infra/sock.rs` ~1031–1476): auth, three rate-limit buckets, timeouts and dispatch interleaved in one ~450-line loop. Downgraded from the scout's "Strong": `parse_control` (327) and `parse_line` (378) are already separate pure fns, so only the limits/auth gating is left to untangle.
- [ ] **`PtyPane` effect routing** (`src/infra/pty.rs` ~670–777): bell/clipboard rate-limit gates and effect dispatch, with 25+ fields on the struct. A small router owning the three `last_*` timestamps would drop ~4 fields. Low urgency.
- [ ] **`PaneBackend` query snapshot** (`src/ports.rs`): `app_cursor_keys`/`bracketed_paste`/`alternate_screen`/`focus_events`/kitty flags mirror vt100 getters 1:1 and `FakePane` grows a field per mode. A cached capabilities struct would shrink the trait. Revisit when the next mode is added.
- [ ] **`set_focus` bundles four concerns** (`src/core/app.rs`; the C22 hide is now `Overlays::focus_moved`, still alternate, visited-waiting and host focus reporting). Taste, not friction; a `FocusTrail` owning `alternate` + `visited_waiting` is the shape.
- [ ] **Neighbor-finding math** (`src/core/layout.rs` `neighbor`/`perp_span` ~547–602 vs `src/ui/mouse.rs` `seam_at` ~105–134): similar perpendicular-span logic. Ranking rules for focus vs seam may differ; audit before merging.
- [ ] **`draw_mode_overlay` per-mode split** (`src/ui/render.rs` ~2102–2400): ~300 lines, 8 modes, reads fine today.
