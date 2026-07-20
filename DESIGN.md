# Roost — a session-native multiplexer for AI agent CLIs

*Design doc v0.1 — July 2026. Working name "roost": your agents come home to roost after every reboot. Alternatives at the end.*

## 1. Thesis

Terminal multiplexers like tmux and zellij exist to keep *processes* alive: detach, reattach, survive SSH drops. That machinery is their hardest and heaviest part — and for AI agent CLIs it is dead weight. Claude Code, opencode, pi, and codex all persist their own conversation state on disk and can resume any session by ID. The process is disposable; the session is not.

Roost inverts the classic muxer contract:

> **tmux/zellij:** the layout is cheap, the processes are precious — keep them alive at all cost.
> **roost:** the processes are cheap, the *(layout × session-ID)* mapping is precious — persist that, and relaunch processes on demand.

This buys a capability no classic muxer has: **full workspace resurrection across terminal restarts and macOS reboots.** Quit roost, reboot the Mac, reopen the terminal, run `roost` — every tab, split, and stacked pane comes back, each running its agent CLI resumed into the exact session it was in, in the right working directory.

It also removes the hardest parts of building a muxer: no daemon, no client/server protocol, no detach, no scrollback serialization, no process migration. Roost is a single foreground process, like a text editor.

## 2. Decisions (from interview)

| Question | Decision |
|---|---|
| Core pain | Multi-tab/pane + stack management like zellij; resume correct sessions after terminal/macOS restart |
| Architecture | Own multiplexer (not layered on tmux/zellij) |
| Lifecycle | No daemon. Quit kills agents; relaunch restores layout and resumes sessions |
| Resume mechanism | Per-tool adapters that know launch/resume/session-detection for each CLI |
| Language | Rust |
| Layout primitives (v1) | Tabs, splits, stacked panes (floating panes deferred) |
| Status awareness | Core to v1: working / waiting-for-you / idle / exited per pane |
| Workspaces | One implicit workspace, auto-saved, auto-restored |
| First adapter | **pi** (Claude Code and others follow) |
| Status detection | Hybrid per adapter; prefer clean signals via an installable extension (pi), fall back to output heuristics |
| Audience | Personal tool; ship fast, optimize for one workflow |

## 3. Architecture

Single-process TUI, one thread per PTY reader plus a main event loop.

```
┌───────────────────────────────────────────────────────────┐
│ roost (single process, foreground)                        │
│                                                           │
│  main event loop (crossterm events + mpsc channel)        │
│    ├─ Input  → keymap → Action → mutate Workspace         │
│    ├─ PtyOutput(pane_id, bytes) → vt100 parser per pane   │
│    ├─ StatusEvent(pane_id, status) → update pane badge    │
│    └─ Tick → redraw (ratatui), debounce workspace save    │
│                                                           │
│  Workspace (the precious state)                           │
│    tabs: Vec<Tab>                                         │
│      layout: LayoutNode tree (Split / Stack / Pane)       │
│      Pane: { adapter, cwd, session_id, title, status }    │
│                                                           │
│  PaneRuntime (the disposable state)                       │
│    portable-pty child + reader thread + vt100::Parser     │
│                                                           │
│  Adapters: pi, claude, opencode, shell (generic)          │
│  Status listener: unix socket for extension events        │
└───────────────────────────────────────────────────────────┘
```

### Crates

- `portable-pty` (wezterm) — PTY spawn/resize, macOS + Linux
- `vt100` — terminal state machine per pane (grid, colors, cursor)
- `ratatui` + `crossterm` — rendering and input
- `serde` / `serde_json` — workspace persistence
- `notify` (later) — watch agent session dirs for ID detection fallback

### Why not reuse zellij's crates?

Zellij's server/client split, plugin host, and layout engine assume the daemon model. The pieces worth taking are ideas, not code: stacked-pane UX, the status-bar hint system, `Ctrl+<mode>` keybinding families. The PTY+vt100+ratatui stack above is ~90% of what we need at ~10% of the surface area.

## 4. Layout model

A `Tab` holds one `LayoutNode` tree:

```rust
enum LayoutNode {
    Split { dir: Horizontal | Vertical, ratios: Vec<f32>, children: Vec<LayoutNode> },
    Stack { children: Vec<PaneId>, expanded: usize },  // zellij-style stack
    Pane(PaneId),
}
```

- **Tabs**: one per project/repo typically. Tab bar on top, `Alt+1..9` to jump.
- **Splits**: n-ary with ratios (simpler resize math than strict binary trees, matches zellij behavior).
- **Stacked panes**: the star primitive for agents. Collapsed panes render as one-line title bars — *name + status badge* — so eight agents fit in the space of one. `Alt+↑/↓` moves through the stack; the expanded pane gets the room. A stack of collapsed agent title bars is effectively a live fleet dashboard for free.
- **Floating panes**: deferred (the quick-launch picker will eventually want one).

## 5. Lifecycle & persistence

**No daemon.** Roost quits → all child PTYs get SIGHUP and die. This is safe *by design*: every adapter targets a CLI whose ground-truth state is on disk, updated continuously by the agent itself. Whatever the agent had committed to its session file is what resumes.

**The workspace file** is the whole product, morally:

`~/.local/state/roost/workspace.json` (state, not config — config lives in `~/.config/roost/config.toml`)

```jsonc
{
  "version": 1,
  "active_tab": 0,
  "tabs": [{
    "name": "pi-mono",
    "layout": { "split": "vertical", "ratios": [0.6, 0.4], "children": [ ... ] },
    "panes": {
      "p1": { "adapter": "pi", "cwd": "~/code/pi-mono", "session": "01998e5f-...", "title": "refactor tui" },
      "p2": { "adapter": "shell", "cwd": "~/code/pi-mono" }
    }
  }]
}
```

Write policy: debounce-save 2s after any mutation (layout change, session ID learned, title change) **and** on clean quit. Crash-safety = atomic write (temp file + rename). Because saves happen continuously, even a hard kill or kernel panic loses at most the last 2 seconds of *layout* changes — never agent state, which the agents own.

**Restore policy** on launch: rebuild the tree, then for each pane ask its adapter for the resume command:

- session ID known → adapter's resume command (`pi --session <id>`, `claude --resume <id>`)
- no session ID (fresh pane, or a plain shell) → adapter's launch command in the saved cwd
- resume fails (session deleted, CLI updated) → pane shows the error + offers fresh launch; never blocks other panes

Panes restore lazily-but-eagerly: all spawn at startup (they're cheap), but a failed pane degrades to a placeholder rather than aborting restore.

## 6. Adapter interface

```rust
trait AgentAdapter {
    fn id(&self) -> &'static str;                       // "pi", "claude", "shell"
    fn launch(&self, cwd: &Path) -> CommandSpec;        // fresh session
    fn resume(&self, cwd: &Path, session: &SessionRef) -> CommandSpec;
    /// Learn the session ID of a freshly launched pane, so it can be
    /// persisted. Strategies: extension handshake, session-dir diffing,
    /// OSC title parsing.
    fn detect_session(&self, pane: &PaneObservation) -> Option<SessionRef>;
    /// Interpret raw signals into an AgentStatus. Receives both extension
    /// events (if any) and output heuristics; adapter picks what it trusts.
    fn interpret_status(&self, sig: &StatusSignal) -> Option<AgentStatus>;
}
```

`CommandSpec` = program + args + env. Adapters are compiled in (it's a personal tool; a TOML-defined generic adapter can come later for arbitrary CLIs — that also covers the "user-declared commands" path for tools without adapters).

### 6.1 pi adapter (v1 flagship)

Ground truth from pi's docs:

- Sessions auto-persist to `~/.pi/agent/sessions/`, organized by working directory.
- Resume: `pi --session <path|id>` (exact session by partial UUID or path); also `-c/--continue` (most recent) and `--fork`.
- Extensions: TypeScript modules auto-discovered from `~/.pi/agent/extensions/*.ts` or project-local `.pi/extensions/*.ts`; they receive an `ExtensionAPI` with lifecycle events.

**Launch**: `pi` in pane cwd. **Resume**: `pi --session <id>`.

**Session detection & status — the roost pi extension.** Roost ships a tiny `roost.ts` pi extension (installed on first run with user consent, into `~/.pi/agent/extensions/`). It uses exactly the events pi already exposes:

| pi event | roost meaning |
|---|---|
| `session_start` (reasons: startup/new/resume/fork) | report session ID → roost persists it |
| `agent_start` | status = **Working** |
| `agent_end` | status = **Waiting** (agent finished a turn; ball is in your court) |
| `tool_call` on user-facing asks / `ctx.ui.confirm` flows | status = **Needs input** |
| `session_shutdown` | status = **Exited** |

Transport: the extension writes newline-delimited JSON to a unix socket roost owns (`$XDG_RUNTIME_DIR/roost.sock`), identified by pane via a `ROOST_PANE` env var roost sets when spawning the PTY. If the socket is absent (pi run outside roost), the extension no-ops instantly — zero cost.

This is the "hybrid" model made concrete: where we control an extension API, status is *exact*. No parsing spinners.

### 6.2 claude adapter (v1.1)

- Sessions: `~/.claude/projects/<encoded-cwd>/*.jsonl`; resume with `claude --resume <session-id>` (or `claude --continue` for most recent in cwd).
- Clean signals: Claude Code **hooks** (`Notification`, `Stop`, `PreToolUse`) can run a shell command — point them at the same unix socket. Same design as the pi extension, different plug.
- Session detection fallback: diff the project's session dir before/after spawn; newest new `.jsonl` is ours.

### 6.3 Heuristic fallback (any adapter, incl. plain `shell`)

When no extension/hook channel exists:

- bytes flowing on the PTY in the last ~2s → **Working**
- output stopped and last non-empty row matches a prompt-ish pattern (`> `, `? `, `y/n`, cursor at col>0 after "?") → **Needs input**
- output stopped, no prompt pattern → **Waiting/Idle**
- child exited → **Exited**

Heuristics are per-adapter tunable (regexes in config). They'll be wrong sometimes; that's acceptable for fallback, and the extension path is the real answer for tools we care about.

### 6.4 Status model

```rust
enum AgentStatus { Working, NeedsInput, Waiting, Idle, Exited(ExitKind) }
```

Surfaced in three places: pane border color, stack title-bar badge (`● working  ◆ needs you  ○ idle`), and tab title aggregation (a tab shows ◆ if *any* pane inside needs input). `NeedsInput` panes also get a terminal bell / macOS notification (config-gated) — the "not knowing who needs me" pain, solved.

## 7. Keybindings (v1)

One flat modifier layer, zellij-flavored but simpler (no modal locking in v1). All on `Alt` to avoid fighting the agents' own `Ctrl` bindings:

| Key | Action |
|---|---|
| `Alt+n` | new pane (split auto: widest direction) |
| `Alt+s` | toggle: pull current pane into / out of a stack |
| `Alt+arrow` / `Alt+hjkl` | focus move (in stacks: expand next/prev) |
| `Alt+Shift+arrow` | resize |
| `Alt+t` / `Alt+1..9` | new tab / go to tab |
| `Alt+w` | close pane (confirm if status = Working) |
| `Alt+r` | rename pane |
| `Alt+q` | quit roost (saves workspace; agents die, sessions live) |
| `Alt+Enter` | quick-launch picker: choose adapter + recent cwd (v1.2, floating pane) |

Everything else passes through to the pane raw — agents see a normal terminal.

## 8. Rendering

Per pane: `vt100::Parser` fed by the PTY reader thread maintains the grid; on redraw, roost blits the visible grid region into the pane's ratatui rect, plus a 1-row title bar (name, adapter icon, status badge). Scrollback: vt100's built-in buffer, `Alt+PgUp` enters scroll mode. Mouse support and full OSC passthrough (hyperlinks, clipboard) deferred; get keys and colors right first.

Resize: on layout change, recompute rects → `pty.resize(rows, cols)` per pane → agents reflow themselves (they all handle SIGWINCH fine).

## 9. Build roadmap

Each milestone is independently usable; stop anywhere and still have a tool.

- **M0 ✓ — one pane** *(weekend)*: spawn `pi` in a PTY, render via vt100+ratatui full-screen, pass keys through, clean exit. Proves the render/input core.
- **M1 ✓ — splits + tabs**: layout tree, focus movement, resize, tab bar.
- **M2 ✓ — persistence + resume**: workspace.json, atomic debounced saves, restore-on-launch with the pi adapter (`--session`). Session detection via session-dir diffing (works before the extension exists). **← daily-driver threshold for the reboot story**
- **M3 ✓ — status**: heuristic detector + the roost pi extension over the unix socket; border colors, badges, bell on NeedsInput. **← the v1 bar from the interview**
- **M4 ✓ — stacked panes**: stack node, collapsed title bars, stack navigation. Fleet-at-a-glance.
- **M5 ✓ — polish (partial: claude adapter, picker, notifications; floating panes + config deferred)**: claude adapter, quick-launch picker, macOS notifications, config file, generic TOML adapter.

Risk notes: vt100 fidelity is the main unknown (agents use rich TUIs — pi and Claude Code both redraw aggressively). Mitigation: M0 exists precisely to stress this early; if `vt100` falls short, wezterm's `termwiz` is the upgrade path. Second risk: `NeedsInput` semantics differ per tool ("turn ended" vs "explicit question") — the adapter owns that interpretation, so wrongness stays local.

## 10. Name candidates

- **roost** — agents come home to roost after every reboot; short, brandable, `roost` free on crates.io-style naming vibes. *(used in this doc)*
- **coop** — a coop full of agents; doubles as "co-op".
- **perch** — where your agents sit; nice verb ("perch a new pane").
- **aviary** — a place that houses many birds; more literal, longer.
- **remux** — resume + mux; descriptive, less charming.

---

*Sources: pi session flags & storage from the pi coding-agent README; extension events & capabilities from pi's extensions.md (see chat for links).*
