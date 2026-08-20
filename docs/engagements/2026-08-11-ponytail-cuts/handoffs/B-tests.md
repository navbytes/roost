# B-tests handoff

Scope: `tests/**` only. Three tasks from the audit, all done.

## 1. Deleted dead QA scripts

`git rm tests/live_qa.rs tests/ux_nav_qa.rs` (1125 lines, both `#[ignore]`d
evidence drives, zero CI references). Fixed one stale comment in
`tests/harness/mod.rs` that named `live_qa.rs` as the one scenario overriding
`ROOST_TEST_NO_HOST_IO` back to `"0"` — now speaks generically since that
scenario is gone.

## 2. `spawn_or_skip` promoted to the harness

`tests/chrome_theme.rs`'s local `fn spawn(what) -> Option<Harness>` shape is
now four fns in `tests/harness/mod.rs`, one per `Harness::try_spawn*`
variant actually used by a call site:

- `spawn_or_skip(what, workspace_json)`
- `spawn_or_skip_with_env(what, workspace_json, envs)`
- `spawn_or_skip_sized(what, workspace_json, envs, rows, cols)`
- `spawn_or_skip_with_config(what, workspace_json, config_json)`

Each prints `SKIP {what}: {reason}` and returns `None` on a PTY-less
sandbox — same message shape as before, still load-bearing for CI logs.
Every call site (38, all of them — the 3 inline ones in `cli.rs` included,
not just the ones with a named `fixture_workspace` fn) converted to
`let Some(mut h) = harness::spawn_or_skip(...) else { return };`.
`chrome_theme.rs`'s own `spawn()` now just calls `harness::spawn_or_skip`
and keeps its first-frame-wait logic (that part is chrome_theme-specific,
not duplicated elsewhere).

## 3. Fixture builders promoted to the harness

Added `one_pane`, `two_panes`, `two_tabs` to `tests/harness/mod.rs`.

- **`one_pane`** — 16 call sites converted: the 14 byte-identical files the
  audit named, plus `send_backpressure.rs`'s `single_pane_workspace` and
  `cli.rs`'s three inline (not-even-fn'd) literals, both semantically
  identical to the rest, just differently whitespaced.
- **`two_panes`** — 4 call sites: `pane_focus.rs` and `pane_reflow.rs` (the
  named pair), plus `firehose.rs` and `pane_cursor.rs`. **Deviation from the
  brief**, flagging it: the audit named only 2, but `firehose.rs`'s fixture
  was byte-identical modulo brace spacing, and `pane_cursor.rs`'s only
  differed by an inline comment repeating what `two_panes`'s own doc comment
  already says (pane 1 focused by default, vertical ⇒ side by side). Folded
  both in since dedup was the point; shout if you wanted them left alone.
- **`two_tabs`** — `move_pane_to_tab.rs` and `roster_overlay.rs`, exactly the
  named pair, byte-identical.

**Kept local (genuinely differ):**
- `chrome_theme.rs` — its main `fixture_workspace` is 2 tabs where one tab
  itself is a 2-pane split (3 panes total across 2 tabs, not the 2-tabs/
  1-pane-each shape `two_tabs` is), plus a second `roster_fixture_workspace`
  (2 tabs, 1 pane each — a third distinct shape, used once).
- `empty_tab_recovery.rs` — `EMPTY_TAB` is a workspace with zero panes in
  its one tab; not a pane-count variant, a different bug fixture entirely.

## Verify

- `cargo check --all-targets`: clean (only 2 pre-existing dead-code warnings
  in `src/ui/input.rs`, unrelated to this change — src/ is other tracks'
  scope).
- `cargo test --test pane_titles --test pane_focus --test roster_overlay`:
  4/4 pass.
- Full `cargo test` (unit + all 27 integration binaries): 806 unit tests +
  every integration test green, 0 failed, 0 skipped (this sandbox has a
  working `/dev/ptmx`, so the SKIP path itself wasn't exercised here — it's
  a straight code-shape mirror of the original match arms, reviewed by eye
  file by file).

`git diff --stat -- tests/`: 28 files changed, 249 insertions(+), 1818
deletions(-) (net −1569 lines).

## Note on session hiccup

Mid-task the coordinator flagged a 403 auth interruption and a possible
stray `git reset` earlier in the session. Re-verified from scratch before
finishing: `git status`/`git diff --cached` on `tests/**`, presence of the
new harness fns, absence of `live_qa.rs`/`ux_nav_qa.rs`, absence of leftover
`Harness::try_spawn` call sites and local `fixture_workspace` fns outside
`chrome_theme.rs`, then reran `cargo check --all-targets` and the 3
representative tests. All consistent with what's described above — nothing
was lost. Confirmed nothing outside `tests/**` is staged by me.
