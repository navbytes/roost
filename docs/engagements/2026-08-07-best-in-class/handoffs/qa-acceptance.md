# Black-box QA acceptance pass (qa-verifier, 2026-08-06) — verdict: FAIL

Built release (1.4 MB, matches README). Driven through a 120×40 portable-pty
harness with isolated `ROOST_STATE` per run; all temp dirs removed, zero
stray processes, repo untouched. Two confirmed functional defects; the rest
of the surface is solid.

| # | Item | Verdict |
|---|---|---|
| 1 | Help output / exit codes | PARTIAL |
| 2 | Control round-trip | PASS |
| 3 | Error paths | PARTIAL |
| 4 | README accuracy | PASS |
| 5 | First-run experience | PASS |
| 6 | CLI with no running TUI | PASS |

## Confirmed defects

**QA-1 · `roost --version` / any unknown subcommand launches the full TUI.**
In a real terminal it silently opens the multiplexer and never prints a
version; **piped or non-interactive it panics with a raw Rust backtrace**
(`failed to initialize terminal`, exit 101). The most orchestrator-hostile
bug found: a script probing the binary either hangs or gets a stack trace.
Root cause cli.rs:30 — any unrecognized first arg falls through to the TUI.
(Extends ux P1-8 with the panic, which is worse than the audit assumed.)

**QA-2 · `roost spawn --cwd <nonexistent>` returns success and lies.**
Returns `{"pane":2}`, exit 0, and silently launches the shell in `$HOME`;
`roost list` then keeps reporting the *requested* (nonexistent) cwd while the
real process is elsewhere (`pwd` in the pane printed `/Users/naveen`). An LLM
orchestrator spawning a worker into a mistyped or just-deleted directory gets
no error and a pane working in the wrong place. **New finding.**

**QA-3 · No subcommand `--help`.** `roost spawn --help` prints the same
"spawn needs an ADAPTER" + generic usage as bare `spawn` (byte-identical by
`diff`). Worse, `roost list --help` / `status --help` / `fork --help`
**silently execute the real command** — `list --help` returned real pane JSON,
exit 0. **New finding.**

**QA-4 · Non-tty misuse panics** rather than failing cleanly — bad for CI and
any automation without a real terminal. (Same root cause as QA-1.)

**QA-5 · README nit:** "`roost --help` prints this same reference" is only
approximately true — same verb set, different order and format
(README.md:293–306 vs cli.rs USAGE).

## Verified good

Full control round-trip: spawn → `{"pane":2}`, send `--enter`, `read --tail`
echoed the exact typed text, status → working, `wait --until waiting` resolved
in 1.49 s with real status, fork → sibling, `send --all` broadcast count
matched pane count, `close` refused a busy pane with an actionable message,
`close --force` succeeded. `wait --timeout 3` on an unreachable condition
returned `{"timed_out":true}` at ~3.0 s — not early, not hung. A concurrent
`list` while a `wait` is parked returned in 0.04 s (no head-of-line blocking).

Error paths: unknown adapter, bogus/non-numeric pane id, bad/empty/absent
token all give clean actionable messages. Second-instance refusal is
byte-exact to the README and leaves the first instance unaffected.

Exit codes are a clean convention: 0 ok / 1 runtime error / 2 usage error.

**First run passes:** a fresh state dir paints immediately — tab bar,
bordered pane, corner badge, and a hint bar shown *by default* whose first
segment reads "Alt+? keys". There is an always-visible in-app pointer to the
keymap; discoverability is not broken.

README verified: quick-start, the exact macOS state path, single-instance
refusal text, all nine control-CLI example lines, binary size.

## Not covered

Whether `Alt+?` renders the complete keymap and `Alt+/` toggles the hint bar
— synthetic single-keystroke injection proved unreliable in this harness, so
results were inconclusive rather than pass/fail. Terminal-app-specific
Option-as-Meta behavior is outside a headless PTY harness.
