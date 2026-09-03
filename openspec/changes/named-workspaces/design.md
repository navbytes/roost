## Context

See proposal.md for motivation. The facts that shape the approach, all verified on `main` at 60826d5:

- `FsStore::state_dir()` returns `$ROOST_STATE` or the XDG state directory plus `roost`, and every per-instance file hangs off it: `workspace.json` ([src/infra/store.rs:24](../../../src/infra/store.rs:24)), `workspace.lock` via `default_path().with_extension("lock")` ([src/main.rs:214](../../../src/main.rs:214)), `control.token` ([src/cli.rs:304](../../../src/cli.rs:304)), `control.log`, `perf.jsonl`, and `config.json` ([src/infra/config.rs:10](../../../src/infra/config.rs:10)). The socket is the exception: `socket_path()` uses the state directory only when `ROOST_STATE` is set, else `$XDG_RUNTIME_DIR/roost.sock` ([src/infra/sock.rs:239](../../../src/infra/sock.rs:239)).
- The instance lock is `std::fs::File::try_lock`, advisory, released by the OS on process death ([src/main.rs:220](../../../src/main.rs:220)).
- Control clients resolve `ROOST_SOCK` then `socket_path()` ([src/cli.rs:240](../../../src/cli.rs:240)); the verbs are `list status spawn fork send read close focus wait keys` and the argument parser is hand-written.
- Panes receive `ROOST_SOCK` and `ROOST_TOKEN` ([src/core/app.rs:1303](../../../src/core/app.rs:1303)) and `ROOST_PANE` ([src/infra/pty.rs:752](../../../src/infra/pty.rs:752)).
- `claimed_sessions()` is an in-memory set built from the instance's own panes ([src/core/session_resolver.rs:61](../../../src/core/session_resolver.rs:61)), consulted at spawn ([src/core/app.rs:1454](../../../src/core/app.rs:1454)). Nothing coordinates across processes.
- The terminal title is OSC 2 `roost · {focused pane}` ([src/main.rs:165](../../../src/main.rs:165), [src/core/app.rs:2849](../../../src/core/app.rs:2849)), fixed by DESIGN-ui.md contract C4.
- Extension and hook installs are global and env-driven, so multiple instances already share them safely ([src/infra/extension.rs](../../../src/infra/extension.rs)).
- The PTY harness gives every test a fresh `ROOST_STATE` ([tests/harness/mod.rs:116](../../../tests/harness/mod.rs:116)), so two-instance tests need no new fixture.
- A unix socket path is limited to about 104 bytes on macOS (nt lesson 3YXWPQ).
- Design audit 2026-09-03: the title needs a one-line C4 amendment; a tab-bar prefix violates C2 ("Tabs start at x=0"); the hint bar has no spare columns under C9; if an on-screen slot is ever wanted it is C2's right status area, quiet ink, never accent; the default workspace renders no name.

## Goals / Non-Goals

**Goals:**
- One mechanism, the state directory, gains a name. Every consumer keeps working because the directory it is handed is already a complete instance directory.
- Zero migration and zero change for users who never pass `-w`.
- Close the cross-instance session race, which exists today under `ROOST_STATE` too.
- Stay daemonless: the filesystem is the registry, advisory locks are the liveness signal.

**Non-Goals:**
- Switching workspaces inside the TUI. In roost a switch is quit plus relaunch, so the first cut documents one terminal window per workspace and offers no picker.
- Per-directory automatic workspaces (`-w .`, or the declarative `.roost.toml` from the August UX review). Later, opt-in only.
- Moving tabs or panes between workspaces.
- Live status in `ws ls` by querying running sockets. The listing reads files only.
- Any on-screen chrome change.

## Decisions

**D1. Root directory versus workspace directory.** Split `FsStore::state_dir()` into `root_dir()` (today's function, unchanged semantics: `$ROOST_STATE` or XDG) and `state_dir()` (the root for `default`, else `root/workspaces/<name>`). The chosen workspace is resolved once at startup into a process-wide `OnceLock`, not by mutating `ROOST_STATE` in the environment. *Alternative rejected:* setting `ROOST_STATE` for the process would make every consumer work with no edits, but `set_var` is unsafe under Rust 2024 once threads exist, and panes would inherit a workspace-scoped `ROOST_STATE`, so a nested roost or a hand-typed `ROOST_STATE=... roost -w x` would nest workspaces inside workspaces.

**D2. Flag parsing.** A pre-pass strips `-w <name>`, `--workspace <name>` and `--workspace=<name>` from argv wherever they appear, before verb dispatch, and resolves the name. Precedence: flag, then `ROOST_SOCK` for control verbs only, then `ROOST_WORKSPACE`, then default. *Alternative rejected:* a positional `roost <name>` collides with the verb namespace.

**D3. Names are lowercase.** Grammar `^[a-z0-9][a-z0-9._-]{0,31}$`, `default` reserved. Rejecting uppercase is the simplest way to be correct on APFS, which folds case, and on ext4, which does not. The name is also the directory name and part of the socket path, hence the length cap.

**D4. Socket location.** A named workspace's socket lives in its own directory, mirroring today's `ROOST_STATE` branch; the default workspace keeps the `$XDG_RUNTIME_DIR` branch untouched. At bind time roost checks the path against the platform limit and fails with a message suggesting a shorter name or a shorter `ROOST_STATE` root. *Alternative rejected:* `$XDG_RUNTIME_DIR/roost/<name>.sock` is shorter but macOS has no runtime dir, so it would only move the problem.

**D5. Config layering is whole-file.** Named workspaces read `root/config.json`; if `root/workspaces/<name>/config.json` exists it is used instead, never merged. The keymap loader already takes one file, and merging keymap tables is speculative. The `ROOST_STATE`-only path keeps its documented "redirects both" behaviour, so the store comment and the README line change wording, not semantics.

**D6. Liveness by try-lock.** `ws ls` opens each `workspace.lock`, attempts a non-blocking exclusive lock, reports running on failure and idle on success, then drops the handle. No pid files, no registry, no daemon. The momentary lock is invisible to a starting instance in practice; the instance's own lock is taken once and held. Counts and adapters come from parsing `workspace.json` read-only; last-saved is the file's mtime.

**D7. Claims are lock files.** `root/claims/<adapter>.<session-id>` is created on demand, exclusively `try_lock`ed, and the open handle is kept in the app keyed by pane. The file's contents are the owner's workspace name and pid, informational only; the lock is the truth. Restore takes the claim before launching the resume command and degrades the pane to the existing placeholder path when it fails, naming the owner from the file contents. Detection tries to claim a candidate id before adopting it and keeps scanning on failure. Quit and pane close drop the handles. Stale files are harmless because an unlocked file is a free claim. *Alternative rejected:* asking running instances over their sockets requires every instance to be reachable and adds a protocol; a shared JSON registry needs its own locking and can go stale.

**D8. Identity is title, env and status.** `ROOST_WORKSPACE` joins `ROOST_SOCK` in the pane environment. The title becomes `roost · {ws} · {pane}` for named workspaces only, with C4 amended in the same PR and a design-supervisor audit in the verify wave. The `status` reply gains a `workspace` field. Notifications from a named workspace prefix the name. Nothing is painted on the tab bar or hint bar.

**D9. Lock and unreachable messages.** The lock refusal becomes "roost is already running in workspace 'x'. Open another with roost -w <name>; roost ws ls lists them." The control client's unreachable error names the tried workspace, lists running ones by probing locks, and suggests `-w`.

## Risks / Trade-offs

- [Fragmentation: users forget which workspace holds what] → title carries the name, `ws ls` shows counts and adapters, and tabs remain the per-project primitive inside one workspace.
- [Switching feels heavy compared with zellij] → documented as one window per workspace; the existing quit guard still double-confirms while an agent is working.
- [Socket path length on macOS] → name cap plus an explicit bind-time check with a clear message.
- [Hand-written argument parser grows a global flag] → the pre-pass is isolated and unit-tested against every verb, including `-w` after the verb.
- [Claims add a new file family under the root] → one directory, tiny files, self-cleaning by lock semantics; `ws rm` removes nothing there because claims belong to sessions, not workspaces.
- [Two instances start their pi or Claude hook install at the same moment] → both write identical content with compare-and-overwrite; benign today, unchanged by this design.
- [A title change touches a UI contract] → C4 amendment and design-supervisor audit are explicit tasks.

## Migration Plan

- No data migration. The default workspace's files stay in place; the `workspaces/` and `claims/` directories are created on first use.
- Rollback is deleting `workspaces/` and `claims/`; the default workspace is untouched by either.
- `ROOST_STATE`-only users keep their exact behaviour, including per-directory config.

## Open Questions

- Whether `ws ls` should one day ask running instances for needs-you counts over their sockets. Deferrable; it changes nothing in the specs above.
- Whether a `-w` on a not-yet-existing name should show a one-line "new workspace" notice in the TUI. Deferrable; creation is implicit either way.
