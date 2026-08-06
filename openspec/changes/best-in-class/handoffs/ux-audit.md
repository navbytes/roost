# UX audit — full product (ux-expert, 2026-08-06)

Evidence: README, DESIGN.md, DESIGN-ui.md C1–C28 §8, SPEC-ux.md, ROADMAP.md,
src/{cli,main,core/app,core/status,ui/render,ui/input,infra/pty,ports}.rs.
Verified-fine (checked, not gaps): feed/roster empty states, flash microcopy,
◆-count/Alt+a shared predicate, per-mode hint truth, wait's self-teaching
--until error, C2 reserved count cell.

## P0

1. **No detach, and nothing states the cost.** Deliberate (no daemon), but
   README reads as "nothing is lost" — untrue for the target user: SSH drop /
   closed window / SIGHUP kills every pane; a Working agent loses its
   in-flight turn (layout + session ids survive; the turn doesn't). tmux
   user's day-one reflex is `prefix d`; roost's answer is silence. Fix: say
   it plainly on README first screen + sanctioned run-under-tmux recipe;
   extend busy-quit confirm (app.rs:3059) to the hangup path.
2. **`roost spawn` returns ok for a failed spawn.** ctl_spawn
   (app.rs:1593–1607) replies ok(pane id); PTY failure lands asynchronously
   in App::dead (app.rs:916–918), visible only on the TUI dead-pane bar
   (render.rs:1569). No control verb exposes it; `read` returns a blank
   grid; status says only `exited` → orchestrator retries forever. Fix:
   reflect failure in reply or make reason readable via status/read + feed.
3. **Spawn error drops the why.** pty.rs:411 wraps context; app.rs:917
   stores `e.to_string()` — anyhow Display keeps only the outer context.
   Bar reads "spawn failed: spawning pi"; ENOENT/EACCES computed then
   discarded one call before display. Fix: `{:#}` alternate format (chain).
4. **macOS Option-trap hint mistimed.** ALT_HINT_WINDOW=8s anchored to
   launch (app.rs:39, :4869). Read-first users get a stray `˜` and no hint
   ever; healthy Ghostty users typing within 8s get a false red bar that
   (per C11 precedence) hides the hint row — first keystroke kills their one
   discoverability surface. Fix the trigger, not the premise.

## P1

5. **Attention ring works fully only for extension panes.** Alt+a/◆-count
   ring NeedsInput only; without extension, NeedsInput is reachable only via
   BEL (status.rs:176–187); quiet turn-end → Waiting, outside ring+count.
   pi auto-installs its extension; Claude Code needs hand-copied hooks →
   the most-used agent degrades to "a ○ appeared, go look". Fix: auto-install
   claude hooks + Waiting-after-Working fallback when no ◆ exists.
6. **Heuristic ◆ is silent.** BEL consumed by vt100, never re-emitted;
   Notifier fires only on OSC 9 / socket status (ports.rs:59–62,
   main.rs:366,385). README's "rings the bell" is false exactly in the
   fallback case. Fix: relay BEL to host on ◆-transition.
7. **`wait` timeout exits 0.** app.rs:1519 ok({timed_out:true});
   cli.rs:60–62 prints, exit 0. `wait … && read` proceeds as if finished.
   Timeout must be branchable: nonzero exit.
8. **Unknown first arg launches the TUI.** cli.rs:30–32 → None → TUI seizes
   the terminal on `roost --version`, `roost ls`, typos. `--help` prints to
   stderr (cli.rs:27). Fix: unknown verb = hard error; --help/--version on
   stdout.
9. **Unknown flags silently dropped; no `--` separator.** positional()
   (cli.rs:186–204) discards `--*`: `send 5 hi --entr` sends without Enter,
   ok; `send 5 "--- plan ---"` sends empty string, ok. LLM-authored text
   truncated while replying ok = worst available failure. Fix: reject
   unknown flags; support `--`.

## P2

10. **· idle vs ○ waiting is no distinction.** Idle dies at first byte
    (status.rs:176–186); every shell at a prompt >2s reads ○ "your turn" —
    wrong for shells, dilutes the signal for agents.
11. **Roster: no urgency ordering/filter.** Rows in tab order
    (app.rs:3481–3487); past one screenful you scroll hunting ◆s.
    Fix: sort worst-first (◆→○→●→·→exited), optional status filter.
12. **●working vs ◆needs-you is glyph-shape-only** on monochrome/no-color;
    pulse is the only other cue and there's no motion opt-out. Decision
    fork: minimal config (see promote list).
13. **Picker offers uninstalled adapters** (app.rs:41 registry, not PATH) →
    first Alt+Enter → pi → dead pane + finding-3's useless error.
14. **N2/N4 silent no-ops remain** (Alt+o outside split; Alt+←/→ in stack)
    vs the every-no-op-flashes rule.
15. **TUI never teaches the control CLI.** HELP_GROUPS (render.rs:680–747)
    covers chords, never `roost send <id>` — U2 put ids on badges as the
    join key; the differentiator is invisible in-product.

## P3

16. <2-row terminals render blank (render.rs:24–26); no "too small" notice.
17. "copy failed" (app.rs:2164) names neither cause nor next step.
18. Room-exhaustion flashes never point at Alt+s/stacks as the exit.

## ROADMAP calls

Promote: claude-hooks auto-install (load-bearing for #5/#6); dead-pane retry
after #3 lands (real error string makes transient/permanent decidable);
minimal-config as a decision fork (#12 + readline collisions need an escape
hatch). Keep parked: fleet rail (urgency-sorted roster does the job at zero
columns; Alt pool is ~empty). Leave deferred: rate cap, read-consent (no UX
evidence either way).

## Top-5 make-them-switch bets

1. Attention signal works on default setup (hooks auto-install + Waiting
   fallback + BEL relay) — today the pitch holds only for pi.
2. Un-misparseable control CLI (exit codes, hard errors, `--`, stdout help,
   spawn failure visible over socket).
3. Answer detach honestly + tmux recipe.
4. Fix the Option-trap trigger timing.
5. Urgency-sorted, filterable roster = fleet dashboard at zero column cost.
