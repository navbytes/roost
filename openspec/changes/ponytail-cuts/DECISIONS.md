# Decisions (Principal, on client's behalf)

- **REVERSED audit finding: ROOST_SYNC_CAP_MS restored** (in minimal form,
  Principal-applied after worker C's cut). tests/pane_sync_output.rs documents
  why the knob is load-bearing: on a loaded CI runner the 60 ms in-bracket
  sleep stretches past the 150 ms production cap, roost correctly presents the
  torn frame, and the gate false-fails (2 of 3 macOS CI runs, measured). An
  integration test that drives the spawned binary has no parameter channel
  except env. The audit's "pass it as a parameter" only works for unit tests,
  which already do.

- **SKIP perf.rs deletion.** Audit said "nothing reads perf.jsonl"; nt decision
  NGZBGF records it as the client-ratified QoS keep-or-delete measurement rig
  (ROOST_NO_QOS A/B). Deleting it would destroy a decided protocol. Flagged to
  client in final report: run the A/B or ratify deleting perf.rs+qos.rs together.
- **SKIP clap migration.** −150 lines but +1 dependency; audit itself called
  keeping the zero-dep parser defensible. Ponytail: no new dep for what works.
- **SKIP App::handle_control removal.** ~10 lines gained, ~30 tests rewritten.
  ROI negative. Left as-is with its #[allow(dead_code)].
- **SKIP TokenReader collapse.** The wrapper is 14 lines but encodes
  "sock cannot write tokens" at compile time — a security boundary, not bloat.
- **SKIP session_resolver.rs FixedAdapter consolidation** (~20 lines): would
  couple two workers' file sets across core/ and agents/. Not worth it.
- **Review fixes (post-verify-wave):** reviewer found two sock.rs over-cuts,
  both restored by the owning builder: (a) per-principal rate bucket — the
  per-connection bucket alone resets on reconnect, making flood-by-reconnect
  (e.g. audit-log erasure in ~1 min) unthrottled; the shared GLOBAL bucket
  stays deleted (it's the layer the C1/C2 victim-starvation argument indicts);
  (b) reporters pool — status-only connections never set a principal and never
  idle-reap, so one pane token could pin all 64 MAX_CONN slots.
- **ACCEPTED: kbd late-adoption removal is a behavior change.** A terminal
  answering the kitty probe after 250 ms (very slow SSH hop) loses keyboard
  enhancement for the session instead of adopting it on a later tick. The
  250 ms budget is itself sized for a slow hop; re-add the channel if a real
  terminal shows up in that window.
- **resume_flag() made required (no default)** per review: a defaulted flag
  turns "new adapter forgot to declare its resume shape" into a silently
  wrong command line instead of a compile error. Overriders declare an unused
  stub.
- **rust-version = "1.89" pinned** (File::try_lock, isqrt) so source builds on
  older stable fail clearly.
- **Prose trim rule given to builders:** keep the one-line summary and any
  current invariant/constraint; delete [Amended …] blocks, "used to" history,
  audit-date narration, rejected alternatives. When in doubt, keep.
- No commit: working tree only, client reviews via git diff (on main; client
  hasn't asked for a branch or commit).
