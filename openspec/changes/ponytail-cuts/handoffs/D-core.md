# D-core handoff

Scope: `src/core/{app,status,control,layout,session_resolver,workspace}.rs`. All items behavior-preserving.

## Status: DONE

Note: mid-task an unexplained `git reset` briefly wiped uncommitted work across the
shared tree (other workers' doing, not mine); it was redone before this handoff and
verified against on-disk state, not memory.

## `git diff --stat` (src/core/**)

```
 src/core/app.rs              | 1037 ++++++++++++++++++------------------------
 src/core/control.rs          |   12 +-
 src/core/layout.rs           |   11 +-
 src/core/session_resolver.rs |  134 ++----
 src/core/status.rs           |  295 +++++-------
 5 files changed, 610 insertions(+), 879 deletions(-)
```
`app.rs`: 451 insertions / 586 deletions (net -135 lines).
`status.rs`: 117 insertions / 178 deletions (net -61 lines).
`workspace.rs`: untouched (no cuts scoped to it).

Two 1-line necessary edits outside my ownership, required for D-core's own
deletions to compile (another worker owns these files; flagged, not otherwise
touched): `src/ui/render.rs` (drop the deleted `roster_overlay_size` import,
call `feed_overlay_size` instead) and `src/main.rs` (`app::picker_items()` →
`agents::picker_ids()`, the deleted re-export inlined at its one external caller).

## What shipped (app.rs)

1. `split_fit(rect: Option<Rect>) -> Option<SplitDir>` extracted; used by
   `spawn_child` and `move_pane_to_tab` (was verbatim duplicated).
2. `ctl_spawn_child` extracted; shared tail of `ctl_spawn`/`ctl_fork` (owner
   resolution, focus save/restore, refusal message, relayout/save/spawn_reply).
3. `feed_page`/`roster_page` (byte-identical bodies) collapsed into one
   `overlay_page()`; `roster_view_rows` kept its own `-2` tail, now calling
   `feed_overlay_size` directly (its `roster_overlay_size` alias is gone).
4. `grab_selection_text` extracted; `finish_selection`/`finish_native_selection`
   now share it, differing only in the clear-afterward step.
5. Deleted alias wrappers, inlined at every caller: `display_name_of` (→
   `display_name_live(spec, None)`), `roster_overlay_size` (→
   `feed_overlay_size`), `picker_items` (→ `crate::agents::picker_ids()`),
   `new_pane_with` (→ `spawn_child(a, None, None)`).
6. `App::handle_control` untouched, as directed.
7. Prose trim: removed every `[Amended, …]` block (25), "used to" narration
   (34), "audit 2026-08-07" dated citations (26), and "PR #NN" essays (10) —
   confirmed zero remaining via grep. Kept each item's one-line summary and
   current invariant; several dense-but-current explanations (e.g.
   `vouched_live`, `attention_ring`'s fallback) were tightened, not gutted.
   Net -135 lines against a ~-500 estimate: most of this file's bulk is
   substantive per-branch rationale that survived the "when in doubt, KEEP"
   rule rather than pure amendment-history bloat — the four flagged patterns
   are now fully gone, which was the actual instruction.

## `session_resolver.rs`

Deleted `detect` (was pure delegation to `adapter.detect_session`); its one
call site (`app.rs` `tick`) now calls the adapter directly. `resolve` and
`claimed_sessions` are now free `pub fn`s (the unit struct held no state, so
it's gone along with them). Updated all callers, including tests. Deleted
`TakenAwareAdapter` + its `detect`-forwarding test (no logic left to test
once `detect` was inlined). Left `FixedAdapter` as-is per instructions.

## `control.rs`

`impl Default for ReadMode` → `#[derive(Default)]` + `#[default]` on
`Screen` (serde-compatible, `rename_all = "snake_case"` unaffected).
Per-byte `format!("{b:02x}")` hex loop → single `write!` into one `String`.
`TokenReader` untouched, as directed.

## `layout.rs`

`subtree_contains`: Vec-allocating membership test → recursive bool walk
(no allocation). `grid_layout`'s `cols = (1..=n).find(|c| c*c>=n).unwrap_or(1)`
→ `n.isqrt()`-based ceil (`if root*root>=n {root} else {root+1}.max(1)`).
Verified equivalent to the old formula for n=0..2000 with a standalone Rust
program before landing (n=0→1, n=1→1, n=2→2 match; `.max(1)` reproduces the
n=0 special case since `isqrt(0)=0`).

## `status.rs` prose trim

Struct-field docs, `set_extension_status`, `vouched_live`/`recently_reported`,
`bell_after_ext` (dropped a "verified against installed pi 0.81.1" version-audit
aside), and every branch comment in `current()` tightened to drop historical
narration while keeping the decision table each branch encodes. Net -61
lines against a ~-100 estimate, same reasoning as app.rs: no `[Amended]`/
`used to`/dated-audit markers remained after this pass (grep-confirmed empty).

## Verify

- `cargo check --all-targets`: clean.
- `cargo test --bin roost core::app::` / `core::status::` / `core::control::`
  / `core::layout::` / `core::session_resolver::`: 338 + 19 + 8 + 36 + 7 = 408
  passed, 0 failed.
- Full `cargo test --all-targets` (whole repo, not just D-core scope): 806
  unit tests + all integration test binaries pass. One flaky failure,
  `tests/firehose.rs::firehose_latency_starvation_and_clean_exit` (a 250ms PTY
  echo-latency assertion under load) — reproduces only when the full suite
  runs in parallel, passes standalone (`cargo test --test firehose`). Not in
  D-core scope (infra::pty) and not touched by this diff.

## NEEDS

None. No blockers, no scope questions outstanding.
