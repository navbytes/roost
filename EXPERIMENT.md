# Agent usage-testing experiment

roost was hardened by putting it in front of a fleet of AI agents and having
them *use* it as a daily driver for real agent CLIs (Claude Code, pi), then
feeding their findings back into fix cycles. This documents the method and
what it caught.

## Method

A pyte-based harness (`exp/harness.py`, not shipped) drives roost inside a
real pty and reconstructs the exact rendered screen a human would see, so an
agent can *observe* the TUI, not just send bytes. Each tester gets an
isolated state dir via the `ROOST_STATE` env var, so many run in parallel
without colliding.

Testers received a brief (`exp/TESTER-GUIDE.md`) and a focus area, and were
told to behave as users — report bugs and missing features with evidence,
never to read or touch the source. Real `claude` (2.1.211) and `pi` (0.73.1)
ran inside panes; a deterministic `pi-fake` provided a stable fixture.

## Cycle 1 — five testers in parallel (2 Sonnet, 3 Haiku)

Focus areas: Claude Code workflow, pi session-resume, layout stress, input &
modes, mouse & status. Eleven findings; the load-bearing ones:

| # | Sev | Finding | Resolution |
|---|-----|---------|------------|
| pi-1 | critical | roost stored the whole pi filename stem `<ts>_<uuid>`; `pi --session` only accepts the bare UUID → every real pi pane came back **dead** | pi adapter extracts the UUID after the last `_`; fixture's simple name still works via fallback |
| pi-2 | critical | two pi panes in one cwd cross-wired onto the **same** session id | `detect_session` takes a `taken` set, walks candidates newest-first, skips ids another pane owns |
| crash | critical | rapid pane creation / tiny panes panicked vt100 (`subtract with overflow`) | vendored vt100 clamps grid `Size` to a 1×1 floor + saturating subs; roost refuses splits that would make sliver panes |
| focus | high | `Alt+h/l` felt inverted — focus was cyclic (DFS), not spatial | directional focus: `Alt`+arrows/hjkl move to the nearest pane in that direction |
| — | — | a panic left the terminal raw + alt-screen + mouse-captured | panic hook restores the terminal on any crash |

The pi-fake fixture had *hidden* pi-1 by using a UUID-only filename — a
lesson that fixtures must mirror real artifact formats. Only the real `pi`
CLI exposed it.

## Cycle 2 — verification + deep investigation (2 Haiku, 1 Sonnet)

Two verifiers re-ran every cycle-1 repro against the rebuilt binary: **all
fixes PASS, no regressions.** Two "medium" cycle-1 reports (Esc doesn't close
the picker; dead-pane retry) turned out to already work — harness-timing
artifacts, not bugs.

The investigator found the last critical had *shifted*: the original
full-screen-corruption repro no longer reproduced (the vt100 clamp fixed it),
but a **resize storm** — several terminal resizes faster than the 33 ms event
loop, i.e. dragging a window edge — still corrupted the screen. A width-only
storm left the right half unpainted, the tell. Root cause: the loop consumed
one event per iteration and never hard-cleared, so roost's geometry lagged
the true size and an intermediate frame's cells survived.

Fix (event loop): drain all pending events per tick coalescing resizes to
one, reconcile against `terminal.size()` (the true size), and `clear()` on
resize. Verified: height-, width-, and multi-step storms all render cleanly.

## Deferred (tracked, not yet done)

- Offer to auto-install `roost.ts` into `~/.pi/agent/extensions/` on first
  run (DESIGN §6.1) — today the exact-status/session path requires a manual
  copy; the fs fallback works but is ~2 s slower.
- Click a tab in the tab bar to switch tabs (README already lists tab-bar
  click as deferred).
- Closing a tab's last pane deletes the tab — currently deliberate (mirrors
  "close last pane quits"); may become a configurable choice.
- Dead-pane `Enter` re-runs the same resume command even when it just failed
  — now rare (pi ids are correct), but could distinguish permanent vs
  transient failure.
