# E-agents handoff

Scope: `src/agents/**`, `extensions/roost.ts`. Status: DONE.

`git diff --stat src/agents extensions`:
```
 extensions/roost.ts    |  82 +++------------
 src/agents/claude.rs   |  10 +-
 src/agents/codex.rs    |  58 +++--------
 src/agents/gemini.rs   |  67 +++---------
 src/agents/mod.rs      | 160 +++++++++++++++++++---------
 src/agents/opencode.rs |  12 +--
 src/agents/pi.rs       | 275 ++-----------------------------------------------
 7 files changed, 176 insertions(+), 488 deletions(-)
```
Net -312 lines across the two owned trees.

## 1. pi.rs: deleted the cwd-narrowing fast path
Removed `encode_cwd`, the `detect_session`/`session_state` overrides,
`PiAdapter::detect_session_in`/`session_state_in`, and the 6 scoping tests
(guess-hit, fallback-when-guess-misses, taken-id-through-narrow, state
candidate/fallback/gone/missing-root). `PiAdapter` now inherits the trait
default (`session_root` returns the whole sessions dir; `owns_session_file`'s
fuzzy compare, kept as-is, does the cwd scoping). Left a
`// ponytail: O(all sessions) read_dir walk per detect (trait default);
re-add cwd-narrowing if a huge ~/.pi measurably hurts.` comment on
`session_root`. 4 tests remain (uuid extraction ×2, resume flag,
owns_session_file scoping).

## 2. Consolidated test doubles
Added `agents::test_support` (`#[cfg(test)] pub(crate) mod`, in `mod.rs`):
`scratch_dir(tag)` (pid+counter temp dir) and `RootAdapter` (session-root
override; `new()` for the file-stem default, `with_id_from_path(root, fn
ptr)` for adapters with real parsing — codex's rollout-UUID extraction,
gemini's first-line JSON read, both passed as non-capturing closures
coercing to `fn` pointers). Replaced `mod.rs`'s inline `RootAdapter` tuple
struct, `codex.rs`'s `FixtureCodex`+`fixture_dir`, and `gemini.rs`'s
`FixtureGemini`+`fixture_dir` with this one double. `session_resolver.rs`'s
`FixedAdapter` was left untouched per scope.

## 3. Collapsed launch/resume to data
`AgentAdapter` trait gained default `launch` (`CommandSpec::new(self.id(),
cwd)`), default `resume` (`self.launch(cwd).arg(self.resume_flag()).arg(session)`),
and `resume_flag()` (defaulted to `"--resume"`, not required — see below).
pi/claude/codex/gemini/opencode now each implement only `resume_flag()`
(`"--session"`, `"--resume"`, `"resume"`, `"--resume"`, `"--session"`) and
dropped their custom `launch`/`resume`. `shell.rs` untouched — its `launch`
spawns `$SHELL` (not `id()`) with conditional `-l`, and `resume` ignores the
session id entirely; both stay structurally custom, matching the "only if
net reduction and obvious" clause.

`resume_flag` got a **default**, not a required method: `session_resolver.rs`'s
`FixedAdapter` (out of scope, "do not touch") overrides `resume` directly
without ever defining `resume_flag`; making it required would have broken
that file. This is a small deviation from the literal "no default" framing
in the brief, done to honor the higher-priority "don't touch
session_resolver.rs" constraint.

## 4. extensions/roost.ts: flat reconnect
Removed `MAX_RETRY_ATTEMPTS`/`BASE_DELAY_MS`/`MAX_DELAY_MS`/`JITTER_FRACTION`/
`HEALTHY_AFTER_MS`, `attempt` counter, `healthyTimer`/`clearHealthyTimer`,
and `kickReconnect` (its cold-start-kick rationale no longer applies once
retry is unconditional and un-budgeted). `scheduleReconnect` now just does
`setTimeout(connect, 500)` on close/error, deduped by the existing
`reconnectTimer` idempotency check. `send()` on a dropped socket now just
returns (relying on `last` + the flat retry) instead of calling
`kickReconnect()`. Replay-on-connect (`last.session`/`last.status` resend in
the `"connect"` handler) is untouched. Trimmed the module doc and inline
comments that narrated the deleted budget/jitter/healthy-timer design.

## Verification
- `cargo check --bin roost`: clean, no warnings.
- `cargo check --all-targets` in the live tree currently fails, but only in
  files outside this scope (`tests/*` mid-refactor by worker B, `src/ui/render.rs`
  mid-refactor by worker F — both other parallel workers' in-progress work,
  confirmed via `git status`/`tasks.md`). To get a real signal I ran the full
  suite in an isolated `git worktree` at HEAD with only this scope's diff
  applied (`git diff -- src/agents | git apply` there): `cargo check
  --all-targets` clean, `cargo test --bin roost agents` → **36 passed, 0
  failed**. Worktree removed after.
- `node extensions/roost.test.mjs`: **all 7 scenarios pass** (no npm
  script/tsconfig in the repo — ran directly per the file's own header
  instructions).

## NEEDS
None. Re-run `cargo test agents` once workers B/F land, to confirm on the
real tree (isolated-worktree result should hold unchanged since this
scope's files aren't touched by either).
