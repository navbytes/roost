# Final report — the best-in-class engagement

**Brief:** find every issue, gap and UI/UX gap; keep a living phased plan;
work autonomously, merging as you go, until roost is the best tool in its
category. A UX expert advises throughout.

**Delivered:** 39 PRs merged, 63 files, +14,198 / −514 lines. Tests 553 → 742
unit plus 26 integration binaries. CI green on Linux and macOS. No open PRs,
no stale branches.

---

## What shipped

**Two crash-class defects, both reproduced before fixing.**
A malformed control request got *no reply at all* — the client hung forever
and 64 of them killed the control plane until restart. And a ratatui patch
bump had made `terminal.clear()` block on a cursor query, so on a host that
doesn't answer, **resizing a window SIGKILLed every pane.**

**The whole security audit, closed.** A pane could smuggle escape sequences
into the operator's real terminal through its own working directory (OSC 52
clipboard poisoning); the "private" scratch pane was readable over the
control plane through four verbs, not two; the control token leaked into
panes through a third environment variable nobody had listed; one pane could
occupy every control connection and lock out both the human and a legitimate
orchestrator; and the audit log — the accountability mechanism the whole
threat model rests on — could be rolled clean in about 140 requests, not the
200,000 the design assumed.

**A control CLI an orchestrator can trust.** `wait` returned success on
timeout, so `wait && read` proceeded as if the agent had finished. Any typo
launched the full-screen TUI, or panicked with a backtrace off-tty. `roost
list --help` *executed the command*. Unknown flags were silently dropped, so
`send 5 hi --entr` sent without Enter and reported ok — and text starting
with dashes sent an empty string and reported ok. A known flag missing its
value silently took the default, so `read 5 --tail` quietly returned the
screen. `spawn --cwd /typo` succeeded, launched in `$HOME`, and then lied
about where the pane was.

**Native macOS interaction (client requirement).** Drag selects and copies on
release; double-click takes the word, triple-click the line, shift-click
extends; panes running vim or Claude Code's own interface keep the mouse.
There is no ⌘C because a terminal application cannot receive ⌘C on macOS —
roost puts the text on the pasteboard itself.

**Category reach.** Distribution went from zero install channels to release
binaries, a Homebrew formula, `cargo binstall` metadata and mise. Agent
coverage went from three CLIs to six. Attention routing works without
per-agent setup — the Claude Code hooks install themselves, and the
documented ones never worked on macOS at all (`nc -q` is not in BSD netcat).

**The one that would have hurt most.** A single 60 KB `send` — under roost's
own limit — froze the entire process: control plane, and the TUI's keyboard
with it. Root cause was a blocking write on the event-loop thread, forming a
closed cycle: the loop blocks writing, so it stops draining the pane's output
channel, so the channel fills, so the pane's output backs up, so its line
discipline stops accepting input, so the write can never finish.

## Client requests

| Request | Status |
|---|---|
| Native macOS selection/copy, no new shortcuts | shipped (contract C29) |
| `mise` install without a registry entry | shipped |
| `Alt+?` opening the keymap on macOS | shipped |
| `Alt+←/→` continuing into the adjacent tab | shipped (contract C31) |
| Leave the dotfiles change alone | honoured, untouched |

## Decisions made on your behalf

Full reasoning in DECISIONS.md. The ones worth ratifying:

- **Fleet rail stays parked**; roster urgency ordering does the same job at
  zero column cost.
- **amp and aider adapters descoped** — both break the
  `(adapter, cwd, session-id)` model that makes resurrection work.
- **Dependency inversion deferred** with the cost measured, not guessed: the
  mechanical part is cheap, but two input methods take raw terminal event
  types, so inverting means the core inventing its own key vocabulary.
- **`ROOST_STATE` does not imply "don't touch my global config"** — argued
  down by a builder: two real fleets on one machine both legitimately want
  the one real `~/.claude`.
- **Merged 4 PRs on local verification during a GitHub Actions outage**,
  with reasoning recorded in each; CI confirmed the whole stack green after.

## What is still open

- **`spawn --worktree`** — the one real feature gap the UX advisor named
  against claude-squad. Nothing architectural blocks it.
- **Both promotion doors need `Actor` validation** — closing only the
  control-request one would close half the gap while reading as closing all
  of it. Documented as an open gap in DESIGN-control.md rather than a
  checked box.
- **Selection anchors to screen coordinates, not content** — contracted as
  deliberate; the advisor argues the core case ("copy the path the agent just
  printed, while it's still streaming") makes it wrong. Needs a decision.
- **A minimal config file** — the standing zero-config stance has no escape
  hatch for the `Alt+f`/`b`/`d` readline collisions or a motion opt-out.
- **`Alt`+click on iTerm2** — needs 30 seconds at a real iTerm2. Option is
  iTerm2's own bypass gesture, so the click may never reach roost.

## What this engagement is actually evidence for

Every defect that survived to the closing wave survived because something
*looked* verified. The pattern, seven times:

- Four §2 theme gates passed **vacuously** over surfaces the fixture never
  drew. A fifth and sixth were found in the exit audit; a seventh was
  introduced by the fixes and caught before merge.
- The test named `squatting_connections_cannot_lock_out_a_fresh_caller`
  opened 32 connections against a 64-connection cap — it could not fail for
  the thing it was named for.
- A test asserted a caller racing a full pool "is correctly rejected too",
  **pinning the bug as the specification.**
- A test for the status socket surviving an idle gap slept 2.5 s against the
  30 s timeout that actually killed it.
- The chord-documentation gate is a hardcoded array that reads neither the
  key table nor the translator it claims to reconcile.
- A contract test used a shell as a "generic" fixture, encoding an
  assumption the contract never made — and blocked the correct fix.
- This ledger recorded 21 shipped items as open.

Five of six code fixes to the rate limiter made something *cheaper* to
attack than the bug being fixed. Round 4 would have silently disabled agent
status reporting. None of that was visible in any builder's report.

The counter-practice that worked: reproduce before fixing (the freeze root
cause came from a thread sample, not inspection); prove a test can fail
(mutation-test the pin, revert the fix and watch it go red); verify claims
against source rather than against the ledger; and brief reviewers to attack
rather than confirm — "has this round introduced anything cheaper than what
it fixed?" found three regressions that "does this look right?" would not.

Builders corrected the Principal's own instructions four times, each
correctly: the edge predicate that would have been a no-op, the two-line fix
that needed two halves, the eviction policy that introduced an amplification
vector, and the elapsed-time gate that needed re-anchoring rather than
removing.
