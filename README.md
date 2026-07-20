# roost

A session-native terminal multiplexer for AI agent CLIs (pi, Claude Code, …).

The inversion that makes it simple: **processes are disposable, sessions are
precious.** Agent CLIs persist their own conversation state and resume by id,
so roost never needs a daemon. It persists the layout tree plus each pane's
`(adapter, cwd, session-id)` and, on relaunch — even after a macOS reboot —
rebuilds every tab/split/stack and resumes each agent into its exact session.

See [DESIGN.md](DESIGN.md) for the full design; this repo is the scaffold
(M0–M1 functional core + adapter/status skeletons).

## Try it

```sh
cargo run            # one shell pane; workspace persists to
                     # ~/.local/state/roost/workspace.json
```

| Key | Action |
|---|---|
| `Alt+n` | new pane (auto split direction) |
| `Alt+arrow` / `Alt+hjkl` | move focus (expands stacked panes) |
| `Alt+t`, `Alt+1..9` | new tab / go to tab |
| `Alt+w` | close pane |
| `Alt+q` | quit (workspace saved; agents die, sessions live) |

To make a pane an agent pane today, edit `workspace.json` and set an entry's
`"adapter"` to `"pi"` or `"claude"` (with optional `"session"`); it will
launch/resume on next start.

## Layout

- `src/workspace.rs` — layout tree, persistence, geometry (the precious state)
- `src/pane.rs` — PTY + vt100 runtime per pane (the disposable state)
- `src/adapters/` — `AgentAdapter` trait; `pi`, `claude`, `shell` impls
- `src/status.rs` — Working/NeedsInput/Waiting/Idle/Exited + heuristics
- `src/app.rs`, `src/render.rs`, `src/input.rs`, `src/main.rs` — TUI core
- `extensions/roost.ts` — pi extension: exact status + session-id over a unix
  socket (the roost-side listener is milestone M3)

## Roadmap

M0 render core ✓ · M1 splits/tabs ✓ · M2 session detection + resume UX ·
M3 status socket + badges · M4 stacked-pane UX polish · M5 claude adapter,
quick-launch, notifications. Details in DESIGN.md §9.
