# C-infra handoff

Scope: `src/infra/sock.rs`, `src/infra/pty.rs`. Status: DONE (post-review).

`git diff --stat -- src/infra/sock.rs`:
```
 src/infra/sock.rs | 610 ++++++++++++++++++++++--------------------------------
 1 file changed, 244 insertions(+), 366 deletions(-)
```
Line counts: sock.rs 3005 → 2883 (net −122, after review restored two
mechanisms cut in the first pass — see "Review fixes" below). pty.rs
untouched by me in this pass (another worker restored `ROOST_SYNC_CAP_MS`
there, minimal form; 1777 → 1733).

## 1. sock.rs: rate limiter — two layers (post-review)
First pass collapsed to `line_bucket` alone; review found that let a
same-token attacker bypass rate limiting entirely by reconnecting per
request (`while :; do roost list; done` — a fresh connection means a fresh
`line_bucket`), turning the audit-log rotation-erasure attack (4 MiB log,
~56k requests at the field-capped size) from ~impractical back into
~1 minute. Restored `Limits.buckets` (`Mutex<HashMap<String, Bucket>>`),
`Limits::take_principal`, `evict_one`, `PRINCIPAL_BUCKET_CAPACITY`/
`PRINCIPAL_REFILL_PER_SEC`, `MAX_TRACKED_TOKENS`, the dispatch charge in the
control-request path, and the four tests that pin it
(`a_flood_is_throttled_not_dropped_or_hung`,
`a_legitimate_bursty_sequence_is_not_throttled`,
`rotating_the_bodys_token_does_not_mint_a_fresh_bucket_on_one_connection`,
`bucket_map_stays_bounded_and_never_evicts_an_actively_throttled_principal`).
`throttle_and_cap_replies_are_distinguishable_from_unauthorized_and_
malformed` reverted to trigger via `PRINCIPAL_BUCKET_CAPACITY` again.
**Left deleted**: the shared *global* aggregate bucket
(`Limits.global_bucket`/`take_global`/`GLOBAL_BUCKET_*`) — that's the layer
the original C1/C2 finding actually indicts (a shared pool sized to bound
an attacker bounds the victim just as much). Rewrote the `Limits` doc for
the resulting two-layer shape: `line_bucket` is the lock-free per-connection
fast path (doesn't survive a reconnect), `Limits.buckets`'s per-principal
bucket is the reconnect-surviving bound (keyed on the admitted token,
persists for the listener's life), global pool deliberately absent.
`per_principal`/`PRINCIPAL_MAX_CONN` (connection *count* cap, a separate
mechanism) was never touched either pass.

## 2. sock.rs: reporters pool — restored in full (post-review)
First pass deleted `Limits.reporters`/`REPORTER_MAX_CONN_PER_PANE`; review
found status-only connections never set `guard.principal` and are never
idle-reaped, so post-cut one pane's token could pin all 64 `MAX_CONN` slots
and starve every control connection. Restored `Limits.reporters`,
`try_reserve_reporter`/`release_reporter`, `REPORTER_MAX_CONN_PER_PANE`
(dropped its self-deprecating `ponytail:` misfire note per the reviewer's
call — kept the rest), the reservation call at the status/session door
(adjacent to the `link_panes` insert, "slot ⟺ entry"), the release call in
`ConnGuard::drop`, and the two tests that pin it
(`mc_pane_cap_admits_exactly_eight_reporters_then_recycles_on_close`,
`mc_overflow_no_strand_the_conn_cap_overflow_path_never_reserves_a_reporter_
slot`). Restored the invariant statement in the cap's doc ("a pane's own
status links must not be able to starve its control budget, or vice
versa") and `nested_claude_hook_alongside_a_live_pi_connection_are_both_
admitted`'s "well under the cap of 8" wording.

## Review fixes: app.rs claim re-checked (read-only)
Checked `src/core/app.rs:5326-5330`'s claim ("Capping the field is what
makes the rotation attack take a length of time the rate limiter can then
make impractical") against the restored code: true again. It depends on a
bucket that persists across reconnects — `Limits.buckets`'s per-principal
bucket, keyed on the admitted token — which is exactly what §1 restored;
`AUDIT_FIELD_CAP` (app.rs, untouched) bounds bytes/request, the
per-principal bucket bounds requests/sec regardless of reconnects, and the
two together are what the comment describes. Did not edit app.rs.

## 3. sock.rs: stale "no reconnect" comments rewritten
Fixed the three comments (pre-auth promotion gate, the P1 `READ_TIMEOUT`
retry clause, and its test's doc) that asserted `extensions/roost.ts` "dials
once, no reconnect anywhere in it" — no longer true. Reality changed twice
during this pass: first to backoff+budget reconnect, then (another worker,
concurrently, same file) to an unconditional flat-500ms retry — I re-read
`extensions/roost.ts` before the final pass and matched the comments to the
current on-disk version (unconditional redial + replay-on-connect). The
retained reasoning: the idle-kill gate (`READ_TIMEOUT` retry for a
status-only connection) exists because without it, a normal >30s idle gap
would force the extension to redial needlessly on every ordinary quiet
stretch instead of only on a genuine drop. Did not touch `roost.ts` itself.

## 4. pty.rs: ROOST_SYNC_CAP_MS knob deleted
Deleted `sync_stale_cap()` and its `OnceLock`/env-var override. `presented_
live()` now passes `SYNC_STALE_CAP_DEFAULT` directly to `sync_presented`
(which already took the cap as a parameter — the in-file unit tests already
called it with `SYNC_STALE_CAP_DEFAULT` directly, unaffected). Also dropped
`ROOST_SYNC_CAP_MS` from the `CONTROL_ENV_VARS` doc's "genuinely inert env
vars" list.

**NEEDS**: `tests/pane_sync_output.rs` (owned by another worker, "B-tests")
still sets `ROOST_SYNC_CAP_MS` when spawning the harness — after this change
it's a silent no-op, so that E2E test lost its escape hatch against the
documented CI flakiness (a loaded runner occasionally stretching a 60ms
sleep past the 150ms default cap). Whoever owns that file should either
drop the now-dead env var and accept the small flake risk, or loosen the
test's timing so it doesn't need a raised cap. Left `tests/pane_sync_
output.rs` untouched — outside my owned scope.

## 5. Prose trim
Trimmed the audit-history/"first cut"/"[Amended]" narration on both files'
big constant comments while keeping each item's current invariant:
`PRE_AUTH_READ_TIMEOUT` (29→16 lines), `WRITE_TIMEOUT` (33→18),
`MAX_LINE`/`OVERSIZE_LINE_MSG`/`PRINCIPAL_MAX_CONN`/`UNSAFE_SOCKET_DIR_MSG`
(sock.rs), `WRITE_QUEUE_BYTES`, the `sync_view` field's `[Amended, C29 ...]`
block, and the bell-relay-gate comment in `process_output` (pty.rs). Where a
comment was already stating a current invariant rather than narrating
history (e.g. pty.rs's `kill()` session-sweep reasoning, `host_clipboard_
bytes`' "not gated by ROOST_TEST_NO_HOST_IO" rationale), left it — "when in
doubt, keep."

## Verification
- `cargo check --all-targets`: clean, zero warnings in this scope (also
  zero overall at time of the post-review check).
- `cargo test --bin roost sock::` → **43 passed, 0 failed** (37 in the
  first-pass collapse, +6 restored by the review fixes:
  `a_flood_is_throttled_not_dropped_or_hung`,
  `a_legitimate_bursty_sequence_is_not_throttled`,
  `rotating_the_bodys_token_does_not_mint_a_fresh_bucket_on_one_connection`,
  `bucket_map_stays_bounded_and_never_evicts_an_actively_throttled_principal`,
  `mc_pane_cap_admits_exactly_eight_reporters_then_recycles_on_close`,
  `mc_overflow_no_strand_the_conn_cap_overflow_path_never_reserves_a_reporter_
  slot`).
- `cargo test --bin roost pty::` → **29 passed, 0 failed** (untouched by
  this pass; pty.rs was already in a passing state, restored knob and all).
- Re-verified from scratch after a mid-task session interruption (API auth
  error + a reported stray `git reset`, prior to the review round): re-read
  both files on disk, confirmed via grep that everything from the first pass
  was intact, nothing lost. No injected/suspicious tool-output instructions
  encountered at any point in this engagement.

## NEEDS
- `tests/pane_sync_output.rs`'s `ROOST_SYNC_CAP_MS` usage (see §4 of the
  first pass, now moot — another worker restored the knob in pty.rs, so
  this NEEDS is resolved).
