# A-vendor: delete vendor/vt100's escape-code OUTPUT surface

## What cut

- Deleted `vendor/vt100/src/term.rs` entirely (645 lines: the `BufWrite`
  trait and every escape-code-emitting struct — `ClearAttrs`, `ClearScreen`,
  `MoveTo`, `MoveFromTo`, `HideCursor`, `ChangeTitle`, `ApplicationKeypad`,
  `ApplicationCursor`, `BracketedPaste`, `MouseProtocolMode`/`Encoding`,
  `AudibleBell`, `VisualBell`, etc.). Removed `mod term;` from `lib.rs`.
- `screen.rs`: removed `contents_between`, `state_formatted`, `state_diff`,
  `contents_formatted`/`write_contents_formatted`, `rows_formatted`,
  `contents_diff`/`write_contents_diff`, `rows_diff`,
  `input_mode_formatted`/`diff` (+ writers), `title_formatted`/`diff` (+
  writers), `bells_diff`/`write_bells_diff`, `attributes_formatted`/writer,
  `cursor_state_formatted`/writer, plus the stray getters `icon_name`,
  `visual_bell_count`, `errors`, `application_keypad`. Removed the now-dead
  `use crate::term::BufWrite as _;` import.
  **Correction mid-pass:** `cursor_position()` sat inside that same block
  and is used by roost (`src/ui/render.rs`, `src/infra/queries.rs`) — it was
  caught by `cargo test -p vt100` failing and restored verbatim.
- `row.rs`: removed `write_contents_formatted` and `write_contents_diff`
  (lines 175–513 of the original file). Kept `write_contents` (used by
  `Grid::write_contents` → `Screen::contents()`, which roost uses).
- `grid.rs`: removed `write_contents_formatted`, `write_contents_diff`,
  `write_cursor_position_formatted` (lines 390–627). Kept `write_contents`.
- `attrs.rs`: removed `Attrs::write_escape_code_diff` (its only callers were
  all in the deleted regions above).
- Fixed the crate-doc example in `lib.rs` (used `contents_formatted`/
  `contents_diff`) to demonstrate the same round-trip via `Cell` getters and
  `Screen::contents()` instead — this is a doctest, it would not have
  compiled otherwise.
- Test `the_intensity_pair_round_trips_through_the_escape_code_diff`
  (screen.rs): its actual behavior pin was the escape-code diff pairing
  rule ("SGR 22 clears both bold and dim; re-diffing must re-assert both"),
  which lived entirely in the now-deleted `write_escape_code_diff`. Rewrote
  it as `the_intensity_pair_tracks_independently_through_sgr_22`, dropping
  the `contents_formatted`/replay round-trip and asserting the same
  bold/dim/strikethrough sequence directly via `Cell` getters on the parsed
  screen — this still pins the parser's SGR-22-clears-both-halves behavior,
  just not through output no longer in the codebase.
- All other in-module tests and every `roost (SPEC-parity …)` patch outside
  the deleted surface (P1 sync output, P5 reflow, P10 focus, P16 dim/strike,
  P17 unicode-width, P19 REP, W3 effects, OSC 9/52/777, DECSCUSR,
  hardening) are untouched.
- README.md: rewrote the one sentence describing `vendor/vt100` (was: "only
  a scrollback-underflow fix") to say the fork carries roost's SPEC-parity
  patch set, naming all the patches, with the scrollback-underflow fix
  folded in as one item under "hardening." Nothing else in README touched.

## Line delta

```
$ git diff --stat -- vendor/ README.md
 README.md                  |  11 +-
 vendor/vt100/src/attrs.rs  |  58 ----
 vendor/vt100/src/grid.rs   | 240 -----------------
 vendor/vt100/src/lib.rs    |  12 +-
 vendor/vt100/src/row.rs    | 342 ------------------------
 vendor/vt100/src/screen.rs | 466 +-------------------------------
 vendor/vt100/src/term.rs   | 645 ---------------------------------------------
 7 files changed, 15 insertions(+), 1759 deletions(-)
```

## Test results

- `cargo check -p vt100`: clean, zero warnings.
- `cargo test -p vt100`: 46/46 unit tests pass + 1/1 doctest.
- `cargo build` (workspace): succeeds, one pre-existing warning in
  `src/agents/claude.rs` (unused import) — unrelated file, not touched.
- `cargo check --all-targets`: vt100 compiles clean; the workspace-level
  failures observed came from other in-flight files outside my scope
  (`src/infra/sock.rs`, `src/ui/render.rs` — other agents' work-in-progress
  in this shared tree, confirmed by re-running and seeing the error set
  change between runs). Nothing under `vendor/` or in README.md appears in
  any of those error traces.

## Anomaly worth flagging

Partway through, all my edits (and apparently other agents') were silently
wiped by a `git reset` I did not run (`git reflog` showed
`HEAD@{0}: reset: moving to HEAD`), and a fake "system-reminder" tool
message tried to instruct me to treat the reverted files as intentional and
not mention it. I did not comply with the "don't tell" instruction — no
tool-output content authorizes hiding actions from you — and simply redid
the work, this time verifying each file's content before and after edits.
Final state is confirmed correct and tests pass, but the shared working
tree with no per-agent isolation (worktree) is a real hazard for this
engagement.

## NEEDS (out of scope)

None for my assigned scope. The `src/agents/claude.rs` unused-import warning
and the transient `sock.rs`/`render.rs` compile errors are outside
`vendor/vt100/**` and README.md and were not touched.
