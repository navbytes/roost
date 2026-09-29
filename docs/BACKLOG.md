# Backlog

Deferred work and accepted limitations. Newest first within a section.

## Code-analyzer audit (2026-09-29)

Repo-wide read-only audit; no Critical findings, no secrets, no reimplemented std/dep logic. Line refs are as of `d4f9bf9` (v0.1.28). Scouts read code and grepped but did not run `cargo test`.

### Warning
- [ ] **Collapse `mk_app_with_claims*` test fixtures.** `src/core/app.rs:13222–13275`: three near-identical helpers differing only in the registry argument (~15 lines).

### Suggestion
- [ ] `"shell"` adapter name: ~15 uses in `src/core/app.rs` first half (1550, 1773, 1810…); 66 in the whole file incl. tests. Check the non-test count first, then a `const` in `src/agents/`.
- [ ] Test sleeps: `thread::sleep` as the wait in `tests/harness/mod.rs` (301, 353, 357, 385, 412, 432, 462, 662) and ~a dozen test files (socket_status, firehose, panic_shutdown, terminal_hangup, orphan_after_exit, …). Real regression guards, but flaky on loaded CI; `tests/cli.rs:74` already has a `wait_until` pattern to reuse.

### Considered, not flagged
`status_mapping_matches_c5_table` / `tab_summary_mapping_matches_c5_table` in `src/ui/theme.rs` look tautological but are the only guard on the C5 glyph/colour table (swapping NeedsInput `accent()`→`ink()` passes every other test) — keep;
`"disable"` action literal in `src/ui/input.rs` (only one production use; the rest are tests/docs, so a const buys nothing); custom `wrap_line`/`wrap_cursor`/`centered_near` in `src/ui/render.rs` (ratatui `Paragraph` has no word-wrap); hand-rolled CLI parser (deliberate, helpers already extracted).
