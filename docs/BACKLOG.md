# Backlog

Deferred work and accepted limitations. Newest first within a section.

## Code-analyzer audit (2026-09-29)

Repo-wide read-only audit; no Critical findings, no secrets, no reimplemented std/dep logic. Line refs are as of `d4f9bf9` (v0.1.28). Scouts read code and grepped but did not run `cargo test`.

### Warning
- [ ] **Centralize `ROOST_*` env var names.** Literal counts: `ROOST_SOCK` 9, `ROOST_TOKEN` 8, `ROOST_PANE`/`ROOST_WORKSPACE`/`ROOST_STATE` 4 each, `ROOST_CONTROL_TOKEN` 2, across `src/cli.rs`, `src/main.rs`, `src/core/app.rs`, `src/infra/`. `src/infra/pty.rs:354` `CONTROL_ENV_VARS` covers only three. A typo silently makes a different variable. Fix: one `pub const` per name in `src/infra/mod.rs`.
- [ ] **`insert_pane` helper in `build_request`.** `src/cli.rs:562,593,613,623,641,648` repeat `m.insert("pane".into(), parse_pane(..)?.into())`.
- [ ] **Collapse `mk_app_with_claims*` test fixtures.** `src/core/app.rs:13222–13275`: three near-identical helpers differing only in the registry argument (~15 lines).

### Suggestion
- [ ] File-name constants: `"roost.sock"` ×5 (`src/infra/sock.rs:249,252,262`, `src/core/app.rs:10624,10721`); `"workspace.json"` / `"workspace.lock"` ×7 across `src/infra/store.rs` and `src/cli.rs`.
- [ ] Extract `strip_control(s, keep_newline)`: `src/core/app.rs:3506,3544` identical control-char filter, `3514` newline-keeping variant. Text-sanitization path, likely to drift.
- [ ] `"disable"` action literal ×16 in `src/ui/input.rs` (e.g. 825, 2060, 2082, 2521, 2584, 2761, 2815) → one `const`.
- [ ] `"shell"` adapter name: ~15 uses in `src/core/app.rs` first half (1550, 1773, 1810…); 66 in the whole file incl. tests. Check the non-test count first, then a `const` in `src/agents/`.
- [ ] Delete tautological tests in `src/ui/theme.rs:315–321` (`status_mapping_matches_c5_table`) and `:334–340` (`tab_summary_mapping_matches_c5_table`); they assert the functions return their own literal tuples. `only_working_spins` and the spinner tests cover the contract.
- [ ] Test sleeps: `thread::sleep` as the wait in `tests/harness/mod.rs` (301, 353, 357, 385, 412, 432, 462, 662) and ~a dozen test files (socket_status, firehose, panic_shutdown, terminal_hangup, orphan_after_exit, …). Real regression guards, but flaky on loaded CI; `tests/cli.rs:74` already has a `wait_until` pattern to reuse.

### Considered, not flagged
Custom `wrap_line`/`wrap_cursor`/`centered_near` in `src/ui/render.rs` (ratatui `Paragraph` has no word-wrap); hand-rolled CLI parser (deliberate, helpers already extracted).
