# Backlog

Deferred work and accepted limitations. Newest first within a section.

## Release preflight depends on a PR index that can miss (2026-10-07)

- **Accepted limitation until it recurs.** `release.yml`'s `Require an authorized release PR` step (`.github/workflows/release.yml:62-71`) trusts `commits/$GITHUB_SHA/pulls`. For v0.1.30's merge commit 314e2c7 that returned `[]` (REST and GraphQL, still empty a day later) although #236's own record looked like #229's, so the release failed with nothing published. Workaround in AGENTS.md (`release/<version>-republish` PR, #237).
- **Idea:** fall back inside the gate to a merged PR found by `gh pr list --state merged --search <sha>`, or by the `(#N)` in the commit subject checked via `pulls/<n>` where `merge_commit_sha == $GITHUB_SHA`. Caveats: the fix itself must ship inside a `release/*` PR (a non-release commit cannot pass the gate), and it loosens an authorization gate, so the fallback must keep the merged/base/same-repo/`release/` checks.

## Configurable openers (2026-10-06)

Shipped: top-level `"open"` rules in `config.json` (`src/core/open.rs`, `src/infra/open.rs`). Left:

- **Paths with spaces are not detected** — the token is the whitespace run from `word_bounds_at` (`src/core/app.rs` ~7455), so `my file.rs` is two tokens.
- **Handler exit status is not reported** — `infra::open::spawn` reaps in a thread and drops the status, so a terminal editor (`nvim`, stdin is null) fails silently once it has started; only a failed spawn flashes.
- **`stat` blocks the main loop** on a hung network mount (`src/infra/open.rs` `stat`, called from the Alt+click path in `src/main.rs` `handle_mouse` and `open_pending`). Thread + timeout if it bites.
- **No `host` filter and no user-defined regex matchers** — rules match on kind/ext only; no regex crate was added for this.

## Library survey follow-ups (2026-10-03)

From the "which libraries could shrink roost or make it more reliable" pass. Shipped: `cargo-deny` in CI, `rustix`/`signal-hook` behind `infra::procs`/`infra::signals`, `infra::atomic`, `base64`, grapheme-aware `textfield`, `proptest` for `layout.rs`. What was looked at and left:

- **Time/date crate** — `cli.rs:1407` `rfc3339` + `civil_from_days` and `render.rs:2603` `local_hh_mm_ss` (one `localtime_r` `unsafe`) are ~45 lines, both pinned by tests. `jiff` is the only crate that fits (`time`'s local-offset lookup refuses in multithreaded programs on Unix unless you opt into unsound behaviour — not re-checked this pass). A new date-time dependency for three integers; revisit if either function bites.
- **macOS process inspection FFI** — `inspect.rs` (`proc_pidinfo`/`sysctl KERN_PROCARGS2`/`proc_listchildpids`) and `pty.rs:477` `all_pids` (`proc_listpids`) are hand-written `unsafe`. A macOS-only `libproc` wrapper might make them safe; unverified that it covers `KERN_PROCARGS2` (argv), and `sysinfo` is heavier than the ~µs syscalls this replaced (see `inspect.rs`'s macOS module doc for the measured subprocess cost). Look before investing.
- **Still `libc`** because rustix has no safe equivalent: `_exit` (`signals.rs:220`), the QoS calls (`qos.rs`), `getloadavg` (`perf.rs`), `localtime_r` (above).
- **`atomic-write-file`** — not adopted; `infra::atomic` is the one in-tree implementation and its tests pin the behaviours roost depends on (0600-from-creation, symlink-resolved guest config, mode kept, temp cleaned on a failed rename).
- **Three tests fail on a clean `HEAD` in the cloud sandbox** (the cloud sandbox runs as uid 0 with a shell init that prints extra lines): `core::app::tests::spawn_with_an_unreadable_session_root_also_attempts_resume_and_keeps_the_id` (`app.rs:10776`) and `infra::extension::tests::a_write_failure_is_reported_not_swallowed` (`extension.rs:709`) rely on a permission denial, which root bypasses — both pass as `nobody`. Rework them the way `atomic.rs`'s failed-rename test does (a non-empty directory where the file goes) so they hold as root. `tests/workspaces.rs:458` `pane_env_title_and_guarded_ws_verbs` fails as `nobody` too: the pane's shell echoes `$ROOST_WORKSPACE` after init noise the assertion does not expect. Environmental: it passes on CI (Linux and macOS, PR #232's run), so it is this sandbox's shell init, not roost.

## Code-analyzer audit (2026-09-29)

Repo-wide read-only audit; no Critical findings, no secrets, no reimplemented std/dep logic. Line refs are as of `d4f9bf9` (v0.1.28). Scouts read code and grepped but did not run `cargo test`.

### Considered, not flagged
Test `thread::sleep`s in `tests/harness/mod.rs`: 5–40 ms poll intervals inside loops that already have deadlines, not bare waits; CI on main was 28/30 green (failures were release-request runs). Revisit only if tests flake.
`status_mapping_matches_c5_table` / `tab_summary_mapping_matches_c5_table` in `src/ui/theme.rs` look tautological but are the only guard on the C5 glyph/colour table (swapping NeedsInput `accent()`→`ink()` passes every other test) — keep;
`"disable"` action literal in `src/ui/input.rs` (only one production use; the rest are tests/docs, so a const buys nothing); custom `wrap_line`/`wrap_cursor`/`centered_near` in `src/ui/render.rs` (ratatui `Paragraph` has no word-wrap); hand-rolled CLI parser (deliberate, helpers already extracted).
Chord table (2026-09-30): rejected. `default_chord_action` (`src/ui/input.rs`) is already the single key→Action source; the help overlay, hint bar, `roost keys` and config rebinding all read it via `effective_bindings`, and tests already fail on an undocumented or unnamed chord. Its arms carry order-dependent quirks (`'l'` with shift before the focus arm, `'<'` vs `','`, the `'1'..='9'` range), names are per action, and help/hint text is per row or per mode, so a table would keep as many entries as there are arms. A test tying chords to `docs/KEYBINDINGS.md` would need a hand-kept map because the doc uses shorthands (`Alt+1..9`).

## Refactors considered and closed (2026-09-30)

From the `/code-refactor` survey. #224 shipped the four that passed the deletion test; each of these was then read in full and did not. Verified independently afterwards. Don't re-propose without new evidence.

- **Help title computed twice** (`src/ui/render.rs` `help_layout` ~1363 vs `draw_mode_overlay` ~2362): the two calls pass different values by design (layout: worst case `shown`/`runnable` to floor the dialog width; draw: keys scrolled into view and cursor state), so one title cannot serve both and the string really changes when the table scrolls. The real waste is the triple `help_layout` call above.
- **Help frame builds `help_layout` three times** (`src/ui/render.rs`: hint bar ~589 via `help_scroll_extent`, `dialog_rect` ~2105, Help arm ~2337; #228 removed a fourth): one cheap pure computation each. Reaching one means threading the layout from `draw` into all three sites; even the narrow fix (Help arm owns its rect) adds ~5 lines, a `.expect` panic path and duplicates the `centered_near` geometry the mouse hitbox uses. Not worth it.
- **`draw_mode_overlay` split** (`src/ui/render.rs` ~2125-2414): only Help, Feed and Roster delegate their drawing; PaneEdit (~57 lines), Picker (~52) and Broadcast (~42) are still inline. Each arm's locals stay in the arm and each frame is already a one-line `modal_frame`, so a split relocates lines without narrowing signatures. Taste, not a defect.
- **`sock.rs` connection handler extraction** (`src/infra/sock.rs` ~1145-1469): `Limits` (~515-720) already owns the shared socket-free gates and is unit-tested; the loop is ~133 code lines plus ~189 comment lines of per-connection, order-sensitive state. A `Gate` type is not worth it. (The one genuine duplication was the "promote this connection" step copied on the control and status paths; the copies had drifted into a real bug, and branch `fix/sock-status-promotion` is replacing them with one shared helper. Once that lands this note is history.)
- **`PtyPane` effect routing** (`src/infra/pty.rs`): the rate-limit logic is already pure fns (`host_bell_bytes` ~155, `host_clipboard_bytes` ~196) with unit tests; the desktop-notify gate (~700-705) is tested through a real `PtyPane` (~1815). The "last one too recent" check appears three times, a trivial fold at most.
- **`PaneBackend` capabilities snapshot** (`src/ports.rs`): the mode queries are not consumed together on the trait (except the pair above, which is a `main.rs` helper, not a trait change), each is mostly its doc comment, and ~23 test sites would churn. Revisit at 8+ modes.
- **`set_focus` FocusTrail** (`src/core/app.rs`): `alternate` has 5 sites and `visited_waiting` 7 (also cleared with `last_status`/`needy_msgs` on pane death, ~1568/~2831), and both rules need `App` lookups (`pane_exists`, `display_status`); the interface would be as big as the two fields. The C22 part already moved to `Overlays::focus_moved`.
- **Neighbor-finding math** (`src/core/layout.rs` ~547-602 vs `src/ui/mouse.rs` ~105-134): look-alikes. `neighbor` ranks `(!overlaps, gap, centre distance)` across gaps (`gap >= 0`, does not skip collapsed panes); `seam_at` needs exact adjacency, a 2-cell hit band, and skips collapsed panes. Only a 3-line span expression is common.

## Wide-glyph follow-ups (2026-10-06)

From the ghost glyph and border gap fix (`skip_covered_cells`, `src/ui/render.rs:131`).

- **Accepted limitation: hosts that advance one column for VS16 emoji.** Terminals that move the cursor only one column for `⚙️` (VTE-based, possibly Alacritty; unverified) can show one stale cell after each changed VS16 emoji, because the covered cell is no longer sent. A fix that works on both kinds of host needs a crossterm-backend wrapper that writes the covered cell *before* the emoji. ratatui's diff order cannot do that.
- **vt100 splits ZWJ sequences.** `🧑‍💻` becomes `🧑`+ZWJ, then `💻` in a cell of its own (`vendor/vt100/src/screen.rs:713` appends only width-0 chars; `💻` takes the width>0 path at :801). That is 4 columns, where unicode-width, Claude Code and Ghostty count 2. This is the suspect for Claude Code rows drifting inside the grid. The reported "becausevery" is still unexplained.
- **`cell_to_char`/`char_to_cell` count zero-width codepoints as a column** (`src/core/app.rs:7426-7444`, `.max(1)`). Every ZWJ or combining mark shifts URL and word hit-testing one column right for the rest of the row. VS16 comes out right only by coincidence.
- **Styling on a wide glyph's second column is never sent.** A copy-mode cursor (`src/ui/render.rs:3692`), a selection edge or a search hit that starts on a covered column styles a cell the diff skips. This predates `skip_covered_cells`, which plain wide glyphs already got.

## Armed close vs mouse flashes (2026-10-07)

- **A mouse-set flash can replace an armed close/quit prompt while the second press still fires.** `CONFIRM_WINDOW` (`src/core/app.rs:323`) keeps Alt+w/Alt+q armed for 3 s and the prompt flash carries the same window (U22: prompt and armed close must match). The flash slot is single, so any flash set from a mouse action overwrites the prompt without cancelling the arm, and the Alt+click path (`src/main.rs` ~1118) does not cancel it either. Found by the design audit of the opener-hint PR; it predates that PR, which only added one more way to reach it (Alt+click on a path with no rule). Fix when it matters: have mouse-triggered flashes (or `handle_mouse` itself) disarm a pending confirm.
