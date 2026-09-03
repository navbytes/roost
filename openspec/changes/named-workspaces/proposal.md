## Why

Running two roost windows at once is possible only through the `ROOST_STATE` escape hatch, which has no name, no listing, splits keybindings per directory, cannot be targeted by out-of-pane control verbs, and lets two instances adopt the same agent session. Multi-window use is the one part of zellij's session model that fits roost's no-daemon thesis, and the September 2026 brainstorm (nt note ERB1VK) settled on promoting the hatch into first-class named workspaces rather than adding any server.

## What Changes

- **Named workspaces.** `roost -w <name>` (also `--workspace <name>` or `ROOST_WORKSPACE=<name>`) opens a workspace that is a directory under the shared state root holding exactly what a state directory holds today. The default workspace keeps today's files in place; nothing migrates. `ROOST_STATE` keeps its meaning as the root override, and the two compose.
- **Workspace verbs.** `roost ws ls` lists the default and every named workspace with running state, tab and pane counts, adapters and last-saved time, offline. `roost ws rm` and `roost ws mv` refuse while the workspace is running and refuse for `default`.
- **Identity.** Panes receive `ROOST_WORKSPACE`. A named workspace sets the terminal title to `roost · {workspace} · {pane}`; `roost status` reports the workspace; desktop notifications from a named workspace carry its name. No on-screen chrome changes (design audit 2026-09-03: a tab-bar prefix would reinstate the removed brand block and the hint bar has no spare columns; the title needs only a one-line C4 amendment).
- **Shared keybindings.** Named workspaces read the root `config.json`; a `config.json` inside the workspace directory replaces it wholesale when present. Plain `ROOST_STATE` isolation keeps today's behaviour.
- **Control targeting.** Out-of-pane verbs accept `-w` and `ROOST_WORKSPACE`; the "cannot reach a running roost" error names the workspace it tried and lists the ones that are running.
- **Clearer lock refusal.** The second-instance error names the workspace and points at `-w` and `ws ls` instead of the raw `ROOST_STATE` recipe.
- **Cross-instance session claims.** Each instance holds an advisory lock per agent session it drives, so two windows can never resume or adopt the same conversation. This closes a race that exists today and protects single-workspace users too.
- **Docs.** README gains a Workspaces section replacing the "only one roost runs per workspace" paragraph; DESIGN-ui.md contract C4 gains the workspace title segment; DESIGN.md's decision table notes named workspaces beside the implicit one.

Nothing is breaking: `roost` with no flag behaves exactly as today, and existing `ROOST_STATE` users see the same files in the same places.

## Capabilities

### New Capabilities
- `workspaces`: selecting, naming, creating, storing, listing, renaming and deleting workspaces; keybinding config sharing; the identity signals a named workspace emits.
- `control-targeting`: how a control client run outside a pane chooses the instance to talk to when several run at once, and what it reports when none is reachable.
- `session-claims`: exclusivity of one agent session across concurrently running instances, at restore and at detection.

### Modified Capabilities
- none. `openspec/specs/` holds no capabilities yet; every requirement here is new.

## Impact

- **Code.** `src/cli.rs` (global `-w` parsing, `ws` verbs, target resolution, error text), `src/main.rs` (root vs workspace directory resolution, lock message, title), `src/infra/store.rs` and `src/infra/config.rs` (root and workspace directories, config layering), `src/infra/sock.rs` (socket path per workspace, path-length check), `src/infra/notify.rs` (name in notifications), `src/core/app.rs` (pane env, title, status JSON, claim handles), `src/core/session_resolver.rs` (claims as a second exclusion source), a new `src/infra/claims.rs`. No new dependencies: the advisory locking already used for the instance lock covers claims.
- **Specs of record.** DESIGN-ui.md C4 (title string), DESIGN.md decision table, README.
- **Tests.** Pure unit tests for name validation and resolution precedence; PTY harness tests running two instances under one root, including the lock refusal, out-of-pane targeting and a claim conflict. The harness already isolates each test through `ROOST_STATE`.
- **Decisions taken with the client on 2026-09-03.** The word is workspace with `-w`, not session with `-s`. Keybinding config is shared with a whole-file override. Creation is implicit on first use. Identity is title, env and status only, with no on-screen slot. Cross-instance claims are in scope for the first cut.
