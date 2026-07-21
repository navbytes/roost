# roost — Advisor Review Synthesis

Three specialist advisors (UX, Security, Architecture) reviewed roost independently.
This consolidates their findings, reconciles overlaps, and proposes a fix order.

Scope reviewed: ~730 LOC of app code (plus a strong test suite) across
`core / ui / infra / agents / ports`.

---

## The headline: session-id handling is the crux

Two advisors reached the same code from opposite directions. That convergence
makes it the #1 priority.

- **Architecture H1** — `session_state` treats a *missing* session directory as
  definitively `Gone` (`agents/mod.rs:113-117`), and `spawn_pane` then **erases
  the session id from `workspace.json`** (`app.rs:199-205`). The session root is
  reverse-engineered from each tool's private on-disk path encoding
  (`pi.rs:54-62`, `claude.rs:13-18`). If a tool changes its layout — or `$HOME`
  resolves differently — every affected pane's resume pointer is silently wiped.
  That destroys the one thing DESIGN.md calls "the whole product."

- **Security #4** — the same `session` string flows *unvalidated* into
  `pi --session <x>` / `claude --resume <x>` (`app.rs:189-192`). `pi --session`
  accepts a filesystem path, so a tampered/synced `workspace.json` (or the socket,
  below) can steer a resume into attacker-planted conversation content.

One piece of code, two failure modes: it deletes good data on a false negative,
and trusts bad data on a false positive. **Fix:** downgrade `NotFound` to
`Unknown` (attempt resume, keep the id) unless the root is readable and the id is
provably absent; and validate the session id shape (reject `/`, `..`, NUL,
leading `-`) before building the command. The validation closes both the file and
socket vectors cheaply.

---

## Tier 1 — Data loss & the core promise (fix first)

| ID | Source | Issue | Fix |
|---|---|---|---|
| **T1.1** | Arch H1 + Sec #4 | Eager `Gone` wipes session ids; unvalidated session id trusted into resume | `NotFound`→`Unknown`; validate id shape before spawn |
| **T1.2** | Arch H2/M0 | No schema versioning; any parse error **hard-fails startup** (`store.rs:47`, `main.rs:94`) | On parse failure, move file to `.bak` and start fresh; branch on `version` to migrate |
| **T1.3** | Arch M4 | Quit sends **SIGKILL** (`pty.rs:125`), so agents can't flush their final turn — contradicts DESIGN §5 (SIGHUP) | SIGTERM/SIGHUP → grace window → SIGKILL backstop, on *quit* only |

## Tier 2 — Security hardening (local/supply-chain threats)

The realistic attacker is a compromised dependency or one of the agent CLIs roost
itself launches — i.e. same-UID code. Several defenses are missing.

| ID | Source | Issue | Fix |
|---|---|---|---|
| **T2.1** | Sec #1 | Control socket has **no peer auth** (`sock.rs:71-101`); any same-UID process can overwrite session ids, spoof status, hijack resume | Check `SO_PEERCRED`, drop uid ≠ self; validate `session` (T1.1) |
| **T2.2** | Sec #3 | `workspace.json` is **world-readable** (`store.rs:51`); session ids are resume tokens + full dir layout leak to other local users | Create state dir `0700`, write file `0600` (set perms *before* rename) |
| **T2.3** | Sec #2 | Socket path can fall back into world-writable `/tmp/roost`, dir perms unverified | Create runtime dir `0700`, verify ownership, refuse otherwise |
| **T2.4** | Sec #5 | Socket reader unbounded (`sock.rs:80-98`) — trivial local DoS (memory/threads) | `.take(N)` per line, cap line length + concurrent connections |

Confirmed **sound** by the security advisor (no action): no shell/argv injection
(execve-style spawn), `adapter` field is registry-gated (no arbitrary-program
exec), outer-terminal escape injection is closed by vt100 parsing + ratatui
control-char filtering, and the instance lock (`flock`) is TOCTOU-free. Good
foundations.

## Tier 3 — UX gaps (biggest hit to daily feel)

| ID | Source | Issue | Fix |
|---|---|---|---|
| **T3.1** | UX H1 | The **entire Alt layer is swallowed** — unmapped Alt chords are dropped, not forwarded (`input.rs:37-66`). Kills readline Meta bindings (`Alt+f/b/d/.`) and agent `Alt+Enter` multiline | Forward unmapped Alt chords as ESC-prefixed bytes; add a literal-next passthrough prefix |
| **T3.2** | UX H2 | No confirmation on destructive `Alt+w` / `Alt+q`, despite DESIGN §7 promising a Working-pane guard; `Alt+w` on last pane silently quits everything | Implement "press again to confirm" for Working panes and last-pane close |
| **T3.3** | UX M1/M2/M5 | No transient message channel — refused splits, save/spawn/resume errors, and scrollback position have **nowhere to surface** | Add a short-lived toast/message region; add a scrollback indicator |
| **T3.4** | UX M3/M6 | Discoverability holes — hint bar omits scroll/resize/tab-jump/rename-tab; iTerm2 users get no Option-key nudge (`app.rs:740`) | Add a `?` full-keymap overlay; extend the Alt nudge beyond Terminal.app |

Lower-priority UX (L-tier): tab bar overflow past 9 tabs, tab-switch resets focus,
dead-pane hint omits `Alt+w`, duplicated name in title + corner badge, no copy-mode.

## Tier 4 — Architectural cleanups (not urgent, protect future growth)

- **Dependency inversion (M1):** `core` imports `ui::Action` and raw `crossterm`
  key events (`app.rs:19`, `:631-723`). The arrow should be ui→core; move the
  `Mode` text-editing out of the core.
- **Adapter wiring not single-sourced (M2):** adding an agent CLI means editing
  the registry + `PICKER_ITEMS` + status wiring in three places; the DESIGN'd
  generic/TOML adapter is unimplemented. Derive the picker from `registry.keys()`.
- **Extract a `SessionDetector` (M3):** the filesystem-diffing detector is the
  real coordination leak of the daemonless model; it's spread across `app.rs` as
  private methods and can't be tested in isolation.
- **Unbounded event drain (M6):** `while let Ok(ev) = rx.try_recv()` with no cap
  (`main.rs:148`) lets a firehose pane starve draw/input. Cap events per tick.
- **Centralized status (M5):** `NeedsInput` prompt-detection is a TODO
  (`status.rs:6`); shell/unhooked tools can never show "needs you." Restore a
  per-adapter status seam before a 4th agent lands.

---

## What's genuinely good (don't break it)

All three advisors volunteered strengths worth protecting:

- **`ports.rs` seam + fakes** — the whole app core runs in unit tests with no
  PTY/fs/terminal. Best decision in the repo.
- **`core/layout.rs` is pure and exhaustively tested** — the crown jewel; layout
  features can be added safely. Keep it pure.
- **Single-owner concurrency** — reader/socket threads only *send* events over one
  mpsc; all mutable state on the main thread. No locks, no races on the precious
  workspace. This is what makes daemonless tractable.
- **Atomic writes + `flock` instance lock** — correct, TOCTOU-free.
- **Tri-state `SessionState`** — deliberately refuses to discard a resume pointer
  on transient errors (the *intent* is right; H1 is the one hole in it).

---

## Recommended fix order

1. **T1.1** — session id: `NotFound`→`Unknown` + validate shape *(stops data loss AND closes the biggest security vector at once)*
2. **T1.2** — don't hard-fail startup on a bad `workspace.json`; add migration
3. **T2.2 / T2.1 / T2.3** — file perms `0600`, socket peer-auth, private runtime dir
4. **T1.3** — graceful shutdown (SIGHUP, not SIGKILL)
5. **T3.1 / T3.2** — stop swallowing the Alt layer; add destructive-action confirms
6. **T3.3 / T3.4** — message channel + discoverability
7. **Tier 4** structural cleanups as capacity allows

The theme across every tier: roost's *happy path* and *test discipline* are
strong; the risks cluster where the daemonless model re-introduces coordination
(session detection/validation) and where trust boundaries were assumed rather than
enforced (socket, file perms, restored state).
