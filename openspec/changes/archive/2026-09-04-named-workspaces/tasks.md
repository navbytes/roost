## 1. Workspace resolution

- [x] 1.1 Split `FsStore::state_dir()` into `root_dir()` (today's `$ROOST_STATE` or XDG logic) and a workspace-aware `state_dir()` backed by a process-wide `OnceLock`; `default_path()` follows it (src/infra/store.rs)
- [x] 1.2 Add name validation as a pure function: `^[a-z0-9][a-z0-9._-]{0,31}$`, `default` reserved, uppercase rejected with a lowercase hint; unit tests for each rejection
- [x] 1.3 Add selection precedence as a pure function: flag, then `ROOST_WORKSPACE`, then default; unit tests
- [x] 1.4 Route `config_path()` to the root `config.json` for named workspaces with whole-file override when the workspace directory has its own; keep the `ROOST_STATE`-only path unchanged; reword the store comment (src/infra/config.rs, src/infra/store.rs:18)
- [x] 1.5 Route `socket_path()` to the workspace directory for named workspaces, default branch untouched; add a bind-time path-length check with a message suggesting a shorter name or root (src/infra/sock.rs)
- [x] 1.6 Keep the lock at `default_path().with_extension("lock")` and replace the refusal text with the workspace name, the `-w <name>` hint and `roost ws ls` (src/main.rs:213)

## 2. CLI flag and control targeting

- [x] 2.1 Add an argv pre-pass that strips `-w <name>`, `--workspace <name>` and `--workspace=<name>` before or after the verb; unit tests across every verb (src/cli.rs)
- [x] 2.2 Resolve the control target as flag, then `ROOST_SOCK`, then `ROOST_WORKSPACE`, then default (src/cli.rs:240)
- [x] 2.3 Replace the unreachable error with the tried workspace, the running workspaces found by probing locks, and the `-w` hint; cover the nothing-running case (src/cli.rs:287)
- [x] 2.4 Add `workspace` to the `status` reply

## 3. Workspace verbs

- [x] 3.1 Implement `ws ls` and bare `ws`: enumerate default plus `workspaces/*`, probe each lock without holding it, parse `workspace.json` read-only for tabs, panes and adapters, use mtime for last saved; exit 0 when empty
- [x] 3.2 Implement `ws ls --json` with the same fields
- [x] 3.3 Implement `ws rm <name>`: refuse while running and for `default`, remove the workspace directory only
- [x] 3.4 Implement `ws mv <old> <new>`: validate the new name, refuse while running and for `default`, rename the directory
- [x] 3.5 Update the usage text; confirm `roost keys` and `ws` verbs never open a socket

## 4. Identity signals

- [x] 4.1 Export `ROOST_WORKSPACE` into every pane beside `ROOST_SOCK` (src/core/app.rs:1303)
- [x] 4.2 Set the title to `roost · {workspace} · {pane}` for named workspaces and keep today's title for default; reset to `roost` on exit (src/main.rs:165, src/core/app.rs:2849)
- [x] 4.3 Amend DESIGN-ui.md contract C4 with the workspace title segment and the rule that default renders nothing
- [x] 4.4 Prefix desktop notifications with the workspace name for named workspaces (src/infra/notify.rs)

## 5. Cross-instance session claims

- [x] 5.1 Add a claims port to src/ports.rs and an in-memory fake, so core stays free of filesystem calls
- [x] 5.2 Add src/infra/claims.rs: `root/claims/` created 0700, one file per `<adapter>.<session-id>`, exclusive `try_lock`, owner workspace and pid written for messages, handle type that releases on drop
- [x] 5.3 Restore path: take the claim before launching the resume command; on failure degrade to the existing placeholder naming the owning workspace and keep the saved session id (src/core/app.rs)
- [x] 5.4 Detection path: try to claim a candidate id before adopting it and keep scanning on failure (src/core/session_resolver.rs, src/core/app.rs:1454)
- [x] 5.5 Release claims on pane close and on shutdown by dropping handles
- [x] 5.6 Unit tests against the fake: restore conflict, detection conflict, conflict cleared, single instance never blocked

## 6. Harness tests

- [x] 6.1 Two instances `-w a` and `-w b` under one `ROOST_STATE` root both start; `ws ls` shows both running with counts
- [x] 6.2 Second instance on a running workspace prints the refusal naming it and exits 1
- [x] 6.3 Out-of-pane `roost -w b list` reaches `b`; `roost list` with only `a` running prints the unreachable error listing `a`
- [x] 6.4 Claim conflict: seed two workspaces with the same session id for the test adapter, the second shows the placeholder, and after the first quits a relaunch of the second resumes
- [x] 6.5 Default workspace regression: with no flag the same files are read and written in the same places as before
- [x] 6.6 Pane environment carries `ROOST_WORKSPACE`; the named-workspace title sequence is emitted; `ws rm` and `ws mv` refuse while running and for default

## 7. Docs

- [x] 7.1 README: replace the single-instance paragraph with a Workspaces section covering `-w`, `ROOST_WORKSPACE`, `ws ls|rm|mv`, one window per workspace, and `ROOST_STATE` as the root override (README.md:95)
- [x] 7.2 README: reword the `config.json` note for shared config and the whole-file override (README.md:177)
- [x] 7.3 DESIGN.md decisions table: add named workspaces beside the implicit one; DESIGN-control.md: note `-w` targeting

## 8. Verify and ship

- [x] 8.1 Run `cargo fmt`, `cargo +$LINT_TOOLCHAIN clippy --all-targets -- -D warnings` and `cargo test`
- [x] 8.2 Design-supervisor audit: C4 amendment aligned, no other chrome contract touched
- [x] 8.3 Reviewer pass on the diff, then a PR against `main` with CI green
