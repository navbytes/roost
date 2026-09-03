# Roost — a session-native multiplexer for AI agent CLIs

*Design doc v0.1 — July 2026. The founding document: thesis, architecture, and the decisions roost was built from. It is kept as written except where a
claim would now mislead; the specs of record for shipped behavior are
[DESIGN-ui.md](DESIGN-ui.md) (chrome and keys, contracts C1–C28),
[DESIGN-control.md](DESIGN-control.md) (control interface), and
[README.md](README.md) (what the tool does today).*

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
- **Floating panes**: shipped as one app-wide floating scratch shell (`Alt+Shift+z`, DESIGN-ui C22; `Alt+f` before the 2026-09-03 re-key) — session-only, never persisted. The quick-launch picker stayed a modal rather than taking the float.

## 5. Lifecycle & persistence

**No daemon.** Roost quits → all child PTYs get SIGHUP and die. This is safe *by design*: every adapter targets a CLI whose ground-truth state is on disk, updated continuously by the agent itself. Whatever the agent had committed to its session file is what resumes.

**The workspace file** is the whole product, morally:

`~/.local/state/roost/workspace.json` on Linux; `~/Library/Application Support/roost/workspace.json` on macOS, which has no XDG state dir. State, not config: it is machine-written on every change, so it belongs in the state dir and not in a directory people keep in a dotfiles repo. The one escape hatch, `config.json`, is the opposite — hand-authored, so it reads from `~/.config/roost/` (and still from the state dir, where it used to live). It holds keybindings only; the generic TOML adapter is still unbuilt; roost is otherwise zero-config

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

Write policy: save synchronously on every mutation (layout change, session ID learned, title/name change, observed cwd/adapter change) **and** on clean quit. Crash-safety = atomic write (temp file + rename). Writes are small and cheap, so saving on each change rather than debouncing loses *nothing* on a hard kill or kernel panic — strictly safer than the originally-planned 2s debounce, and agent state is the agents' own anyway. (The periodic pane observation is itself throttled to ~2s, so status/observe churn doesn't cause pathological write amplification.)

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
| `agent_settled` (pi ≥ 0.80.4; `agent_end` kept as a latched fallback for older pi) | status = **Waiting** (turn truly over; ball is in your court). Bare `agent_end` also fires between a run and its automatic follow-ups — retry after a provider error, compaction, a queued continuation — and flapped ○/● on every recovery. The fallback is latched, not `isIdle`-guarded: once `agent_settled` fires at all, it owns turn-ends and `agent_end` goes silent; before that, `agent_end` reports unconditionally (`isIdle` exists back to pi 0.31.0, but nothing documents its value at the *final* `agent_end` on pre-0.80.4 versions — a guard there could suppress the only turn-end signal old pi has). |
| `tool_call` on user-facing asks / `ctx.ui.confirm` flows | status = **Needs input**, with the ask's question text riding along as `message` — surfaced in the feed line and the notification so you see *what* it's asking |
| turn ends with a prose question (heuristic) | a turn whose final assistant line ends with `?` reports **Needs input** with that line as `message`, instead of plain Waiting — pi has no event for "I asked you something in prose", so this is inferred from `agent_end`'s messages at settle time. Deliberately tolerant of borderline cases ("Done — want me to add tests?"): the shown question makes the ◆ self-explanatory, and roost's needs-input decay self-heals a wrong one |
| `session_shutdown` | close the socket; no status report |

`session_shutdown` deliberately reports nothing: **Exited** has exactly one ground truth — the pane's PTY hitting EOF when its child dies. The extension also runs in *nested* pi processes (a subagent, a one-shot `pi -p` tool call, a pi launched inside a `shell` pane — all inherit `ROOST_PANE`/`ROOST_TOKEN`), so a shutdown report would falsely mark the pane exited whenever a nested pi merely finished its work. Roost demotes any "exited" a stale extension still sends to **Waiting** for the same reason.

Transport: the extension writes newline-delimited JSON to a unix socket roost owns (`$XDG_RUNTIME_DIR/roost.sock`), identified by pane via a `ROOST_PANE` env var roost sets when spawning the PTY. If the socket is absent (pi run outside roost), the extension no-ops instantly — zero cost.

This is the "hybrid" model made concrete: where we control an extension API, status is *exact*. No parsing spinners.

### 6.2 claude adapter (v1.1)

- Sessions: `~/.claude/projects/<encoded-cwd>/*.jsonl`; resume with `claude --resume <session-id>` (or `claude --continue` for most recent in cwd).
- Clean signals: Claude Code **hooks** (`Notification`, `Stop`, `PreToolUse`) can run a shell command — point them at the same unix socket. Same design as the pi extension, different plug. The `Notification` hook's own stdin JSON carries the human-readable reason ("Claude needs your permission to use Bash"); roost's hook shim forwards it as the status line's `message`, so the ◆ says why.
- Session detection fallback: diff the project's session dir before/after spawn; newest new `.jsonl` is ours.
  - **Every scan needs a lower time bound, and "before spawn" is only half
    the cases.** A pane launched as an agent gets `now()` — tight and
    obviously right. A pane *promoted* (you opened a shell and typed `pi`)
    has no spawn moment to anchor on, because the agent started between two
    observation ticks. That window was `UNIX_EPOCH` — no bound — on the
    reasoning that the claimed-id set would prevent cross-wiring. It does
    not: `claimed_sessions` only knows ids stored on **live** panes, so a
    conversation from a closed pane or an earlier run is unclaimed and
    therefore eligible. The scan took the newest such file whenever it was
    written, committed it, and dropped the pane from `pending_detect` — so
    the mistake was permanent and the next relaunch resumed a conversation
    from days ago.
  - The bound is **the last tick the pane was observed running no agent**
    (`last_shell_seen`), minus a few seconds for observation lag: a promoted
    pane cannot own a session file older than the moment it was still a
    shell. Reported from real use as "it loads the wrong session". The bound
    only ever tightens — a later promotion never restores an earlier one's
    window.
  - **When the root cannot attribute a file to a pane, decline.** Some
    adapters' session roots are global (codex: `~/.codex/sessions`, bucketed
    by date, the same directory for every project), so two panes in
    different projects mid-detection cannot be told apart — the only thing
    separating the candidates is mtime order, which says nothing about whose
    they are. roost skips the scan and leaves the pane pending rather than
    guessing. **A wrong session is far worse than no session:** losing a
    resume pointer costs a `--continue`; attaching a pane to another
    project's conversation corrupts work in it. Same-cwd concurrency is not
    ambiguous in this sense and is still resolved, newest-spawn-first.
  - **The scan gives up after a minute** (`DETECT_GIVE_UP`). No adapter
    overrides `detect_session`, so every retry is a full recursive walk of
    the session root stat-ing every file; a pane that never resolves used to
    keep that running every 2s for the life of the process. Giving up costs
    nothing real — the file appears within seconds if it appears at all —
    and the failure is the safe one: start fresh rather than resume wrong.
- Title channel (D5): Claude Code also publishes its state in the terminal title — a braille spinner frame (`⠧ …`) while a turn runs, `✳ …` at rest. Roost parses title changes into a screen-derived Working/Waiting signal that ranks between hook reports and byte heuristics: never consulted while a status-socket link is live, ranked below the bell (a blocked agent's one remaining "needs you" signal must win), never able to touch NeedsInput/Exited, and a Working title decays like a report once stale *and* silent — an *animating* spinner refreshes the clock every frame, so only a frozen (hung) spinner ever decays, and conversely a live spinner sustains a quiet hook-reported Working past its usual decay (a long silent tool call between one-shot hooks). The whole channel sits behind an app-pushed gate — enabled only while an agent actually runs in the pane (spawn adapter + `observe_panes` promote/demote, which also clears the stored signal on demote): a vt100 title outlives the process that set it, so without the gate an exited agent's leftover `✳` would veto Working for every later shell command in that pane. The gate is what makes the channel safe; the ranking is what keeps claude panes honest **between** their one-shot hook connections — a keystroke echo on a resting pane no longer paints a phantom ●, and a lost `Stop` hook settles the moment the title flips to rest.

### 6.3 Heuristic fallback (any adapter, incl. plain `shell`)

When no extension/hook channel exists:

- bytes flowing on the PTY in the last ~2s → **Working**
- output stopped and last non-empty row matches a prompt-ish pattern (`> `, `? `, `y/n`, cursor at col>0 after "?") → **Needs input**
- output stopped, no prompt pattern → **Waiting/Idle**
- child exited → **Exited**

Heuristics are per-adapter tunable (regexes in config). They'll be wrong sometimes; that's acceptable for fallback, and the extension path is the real answer for tools we care about.

**Link-liveness gates how much the fallback may override the extension path.**
An extension/hook channel isn't just present-or-absent — its socket
connection is itself either up or down right now, and roost tracks that per
pane (`StatusTracker::ext_link`, fed by the socket listener's own
connection→pane accounting). While it's up: a resting report (Waiting/Idle)
is trusted over PTY byte noise almost unconditionally — an extension's
`agent_start` is exact and arrives within ms of a real turn, so bytes alone
(composer echo, a resize repaint) must not repaint a phantom `●`. Once the
link is down — the hook died, or was never persistent (Claude Code's hooks
are one-shot connections that report and disconnect) — the heuristic is
exactly this section's fallback again.

### 6.4 Status model

```rust
enum AgentStatus { Working, NeedsInput, Waiting, Idle, Exited(ExitKind) }
```

Surfaced in three places: pane border color, stack title-bar badge (`● working  ◆ needs you  ○ idle`), and tab title aggregation (a tab shows ◆ if *any* pane inside needs input). `NeedsInput` panes also get a terminal bell and an `OSC 9` their host terminal raises as a real desktop notification — the "not knowing who needs me" pain, solved. (Through the terminal, not through `osascript`: a CLI cannot post a macOS notification under its own name, so that route always said "Script Editor". SPEC-parity P2 carries the reasoning.)

A reported `Working` that goes silent past a generous timeout (45s) decays
rather than sticking forever — but not always to the same place. With the
extension/hook link still up, the silence reads as the hook being alive and
merely quiet (a slow local model's prefill routinely runs longer than that),
so it settles to **Idle** — calmer than an eternal `●`, and honest that
roost genuinely doesn't know if it's still your turn. With the link down,
there's no evidence the hook is still around at all, so it settles to
**Waiting** as before. `NeedsInput`'s own decay stays purely time-based
either way: self-healing beats a `◆` that can pull the user to the pane
forever.

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
| `Alt+Enter` | quick-launch picker: choose adapter + recent cwd |

Everything else passes through to the pane raw — agents see a normal terminal.

The layer grew well past this v1 table — zoom, float, roster, feed, copy mode,
canned layouts, raw pass-through, tab stepping. The current keymap is
[DESIGN-ui.md §8](DESIGN-ui.md) (the in-app `Alt+?` overlay is generated from
the same table), and README lists it for users.

## 8. Rendering

Per pane: `vt100::Parser` fed by the PTY reader thread maintains the grid; on redraw, roost blits the visible grid region into the pane's ratatui rect, plus a 1-row title bar (name, adapter icon, status badge). Scrollback: vt100's built-in buffer, `Alt+PgUp` enters scroll mode. Mouse support and OSC passthrough both shipped after this was written: the wheel routes to the inner app or roost's own scrollback, clicks and drags forward to mouse-aware apps, `Alt`+click opens links, and OSC 9/52 are re-emitted to the host (SPEC-parity W3).

Resize: on layout change, recompute rects → `pty.resize(rows, cols)` per pane → agents reflow themselves (they all handle SIGWINCH fine).

## 9. Build roadmap

Each milestone is independently usable; stop anywhere and still have a tool.

- **M0 ✓ — one pane** *(weekend)*: spawn `pi` in a PTY, render via vt100+ratatui full-screen, pass keys through, clean exit. Proves the render/input core.
- **M1 ✓ — splits + tabs**: layout tree, focus movement, resize, tab bar.
- **M2 ✓ — persistence + resume**: workspace.json, atomic debounced saves, restore-on-launch with the pi adapter (`--session`). Session detection via session-dir diffing (works before the extension exists). **← daily-driver threshold for the reboot story**
- **M3 ✓ — status**: heuristic detector + the roost pi extension over the unix socket; border colors, badges, bell on NeedsInput. **← the v1 bar from the interview**
- **M4 ✓ — stacked panes**: stack node, collapsed title bars, stack navigation. Fleet-at-a-glance.
- **M5 ✓ — polish**: claude adapter, quick-launch picker, macOS notifications. Floating panes landed later as the `Alt+f` scratch shell (re-keyed to `Alt+Shift+z` 2026-09-03, DESIGN-ui C22); the config file shipped as `config.json` — keybindings only, read once at startup; the generic TOML adapter is still unbuilt — roost is otherwise deliberately zero-config (see [ROADMAP.md](ROADMAP.md)).

Risk notes: vt100 fidelity is the main unknown (agents use rich TUIs — pi and Claude Code both redraw aggressively). Mitigation: M0 exists precisely to stress this early; if `vt100` falls short, wezterm's `termwiz` is the upgrade path. Second risk: `NeedsInput` semantics differ per tool ("turn ended" vs "explicit question") — the adapter owns that interpretation, so wrongness stays local.

---

*Sources: pi session flags & storage from the pi coding-agent README; extension events & capabilities from pi's extensions.md (see chat for links).*
