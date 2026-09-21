# Product owner audit — 2026-09-21

## Confirmed findings

1. `ws mv` validates the destination name but can derive the source path from an invalid name.
2. Fixed-arity control verbs ignore surplus positional arguments.
3. `read` accepts contradictory `--tail` and `--full` modes, depending on their order.
4. A value-taking option can consume a following recognized option as its value.
5. Rename and pane-edit modals lose their save/cancel guidance when hints are hidden; pane edit does not label its name and note fields.
6. A picker filter with no adapter matches leaves a blank adapter column while cwd choices remain visible.

## Priority and acceptance

- P0 — contain workspace paths: validate both `ws mv` names before deriving either path; invalid input exits as usage without touching state.
- P1 — make control commands unambiguous: fixed-arity verbs reject extra positionals; `read` rejects both output modes in either order; recognized options cannot stand in for missing values; free-form `send` text and literal `--` behavior remain intact.
- P1 — keep modal decisions visible: rename and pane edit show compact `↵ save` and `Esc cancel` guidance in their frames, and pane edit labels `name` and `note` in the modal itself.
- P2 — explain empty picker results: show `no agent matches` in the adapter column without disturbing cwd selection or launch behavior.

Each item gets a focused regression test. The implementation is complete when those tests pass, formatting is clean, the pinned-toolchain clippy gate passes, and the full suite passes in a color-capable terminal environment.

## Deferred

- Raw `Alt+Shift+p` legacy-input ambiguity: historical report, not reproduced in this audit.
- Solo-rail direct selection: broader interaction design rather than a bounded defect.
- Screen-read consent policy: requires a product and trust-policy decision before implementation.

No new control capabilities, adapter behavior, workspace semantics, or release work are in scope.

## Outcome and evidence

All six bounded findings were fixed. Structured CLI values reject option-name collisions while free-form `--input` and `send` text retain literal hyphen-prefixed values; `ws mv` validates both names before deriving paths. Text modals now own their field and exit guidance, and the picker explains a zero-match adapter filter without taking cwd selection away.

- `cargo +1.96.1 fmt --check` — passed.
- `cargo +1.96.1 clippy --all-targets -- -D warnings` — passed.
- `env -u NO_COLOR TERM=xterm-256color COLORTERM=truecolor cargo test` — 1,286 passed, 0 failed.
- Independent code review — approved after pinning recognized option names as valid free-form `--input` text.
- TUI design-supervisor audit — aligned with C12/C13/C14/C32 after the contract amendments in `DESIGN-ui.md`.

The render regressions exercise the real ratatui draw path and assert visible labels, modal guidance, empty-result text, cwd selection, and marker placement. They do not replace a human visual pass in a native terminal across terminal emulators.

Owner recommendation: finish predictable everyday workflows before adding new rail, keymap, or automation capabilities. The deferred items above remain the right boundary for this audit.
