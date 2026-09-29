# Backlog

Deferred work and accepted limitations. Newest first within a section.

## Code-analyzer audit (2026-09-29)

Repo-wide read-only audit; no Critical findings, no secrets, no reimplemented std/dep logic. Line refs are as of `d4f9bf9` (v0.1.28). Scouts read code and grepped but did not run `cargo test`.

### Considered, not flagged
Test `thread::sleep`s in `tests/harness/mod.rs`: 5–40 ms poll intervals inside loops that already have deadlines, not bare waits; CI on main was 28/30 green (failures were release-request runs). Revisit only if tests flake.
`status_mapping_matches_c5_table` / `tab_summary_mapping_matches_c5_table` in `src/ui/theme.rs` look tautological but are the only guard on the C5 glyph/colour table (swapping NeedsInput `accent()`→`ink()` passes every other test) — keep;
`"disable"` action literal in `src/ui/input.rs` (only one production use; the rest are tests/docs, so a const buys nothing); custom `wrap_line`/`wrap_cursor`/`centered_near` in `src/ui/render.rs` (ratatui `Paragraph` has no word-wrap); hand-rolled CLI parser (deliberate, helpers already extracted).
