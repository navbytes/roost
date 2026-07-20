# roost

A session-native terminal multiplexer for AI agent CLIs (pi, Claude Code, …).

The inversion that makes it simple: **processes are disposable, sessions are
precious.** Agent CLIs persist their own conversation state and resume by id,
so roost never needs a daemon. It persists the layout tree plus each pane's
`(adapter, cwd, session-id)` and, on relaunch — even after a macOS reboot —
rebuilds every tab/split/stack and resumes each agent into its exact session.

See [DESIGN.md](DESIGN.md) for the full design rationale.

## Install & run

```sh
cargo install --path .   # or just: cargo run
roost
```

State lives in `~/.local/state/roost/workspace.json` (auto-saved on every
change, atomic writes). Delete it to start clean.

## Keys

| Key | Action |
|---|---|
| `Alt+n` | new shell pane (auto split direction) |
| `Alt+Enter` | quick-launch picker: pi / claude / shell |
| `Alt+arrow` / `Alt+hjkl` | move focus (expands stacked panes) |
| `Alt+Shift+arrow` | resize along that axis |
| `Alt+s` | toggle: collapse the surrounding split into a stack / explode it |
| `Alt+r` | rename pane |
| `Alt+PgUp` | scroll mode (`↑/↓/PgUp/PgDn` scroll, `Esc`/`q` exit) |
| `Alt+t`, `Alt+1..9` | new tab / go to tab |
| `Alt+w` | close pane |
| `Alt+q` | quit — workspace saved; agents die, sessions live |

Everything else passes through to the focused pane untouched.

In a **dead pane** (process exited or spawn failed): `Enter` relaunches /
resumes, `f` starts a fresh session (drops the stored session id).

## Status badges

Pane borders and stack title bars show each agent's state:
`●` working · `◆` needs input · `○` waiting for you · `·` idle · `✕` exited.
When a non-focused pane starts waiting for you, roost rings the terminal
bell (and posts a native notification on macOS).

Status arrives two ways:

1. **Exact** — agent-side integrations report over roost's unix socket
   (`$ROOST_SOCK`, pane identified by `$ROOST_PANE`):
   - pi: install [`extensions/roost.ts`](extensions/roost.ts) into
     `~/.pi/agent/extensions/` — uses pi's `agent_start`/`agent_end`/
     `session_start` events; also reports session ids instantly.
   - Claude Code: hook snippets in
     [`extensions/claude-code-hooks.md`](extensions/claude-code-hooks.md).
2. **Heuristic fallback** — recent PTY output ⇒ working; silence ⇒ waiting.

## Session resume

| Adapter | Launch | Resume | Session detection |
|---|---|---|---|
| `pi` | `pi` | `pi --session <id>` | socket handshake, or newest file under `~/.pi/agent/sessions/` |
| `claude` | `claude` | `claude --resume <id>` | newest `*.jsonl` under `~/.claude/projects/<encoded-cwd>/` |
| `shell` | `$SHELL` | relaunch in saved cwd | — |

New adapters implement the `AgentAdapter` trait in `src/adapters/` (five
small methods).

## Layout

- `src/workspace.rs` — layout tree (splits/stacks/tabs), persistence, geometry, layout ops
- `src/pane.rs` — PTY + vt100 runtime per pane (the disposable state)
- `src/adapters/` — `AgentAdapter` trait; `pi`, `claude`, `shell`
- `src/sock.rs` — status socket listener (ndjson over unix socket)
- `src/status.rs` — status model: extension signals + output heuristics
- `src/app.rs` — actions, modes (rename/picker/scroll), session detection
- `src/render.rs`, `src/input.rs`, `src/main.rs` — TUI core

## Roadmap status

M0 render core ✓ · M1 splits/tabs ✓ · M2 persistence + session detection ✓ ·
M3 status socket + badges ✓ · M4 stacks + resize ✓ · M5 picker, rename,
scroll, notifications ✓. Deferred: floating panes, mouse support, opencode
adapter, config file (roost is deliberately zero-config for now).
