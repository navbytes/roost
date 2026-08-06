# DECISIONS.md — calls made on the client's behalf

Every decision the Principal took autonomously during the best-in-class
engagement, for ratification in the final report. Format: **choice — why —
revisit-when**.

---

## D1 · Engagement state lives in `openspec/changes/best-in-class/`

**Choice** — `openspec init` in roost; PLAN.md is the living ledger, one
handoff per audit lens, this file for decisions. **Why** — company standard,
and the client asked for a phased document that keeps accumulating findings;
a repo-tracked ledger survives compaction and session loss. **Revisit** —
never, unless roost's docs move wholesale.

## D2 · Every writing builder runs in an isolated git worktree

**Choice** — all Agent-tool builders get `isolation: worktree`; the
Principal's doc work lives in a separate `git worktree` at
`~/repos/roost-plan`. **Why** — a subagent told to "create branch X" runs
`git checkout` in the *shared* working directory; one did, switching the tree
under the Principal mid-edit. Isolation makes parallel builders safe.
**Revisit** — if the platform ever gives subagents their own checkout by
default. (Recorded as a durable lesson in nt.)

## D3 · Merge policy: squash + delete branch, auto on green

**Choice** — every engagement PR is squash-merged by a polling watcher as
soon as CI is green on both runners; branches deleted. Riskier merges
(schema, release, anything destructive) still get flagged to the client.
**Why** — the client's standing rule authorizes auto-merging low-risk work on
green, and the brief says keep merging autonomously. GitHub's native
auto-merge is disabled on this repo, hence the watcher. **Revisit** — if a
merged PR ever needs reverting, tighten to a review gate.

## D4 · Fleet rail stays parked; roster urgency ordering ships instead

**Choice** — the persistent projects→agents rail (fully designed in ROADMAP)
stays parked; Phase 3 ships worst-first ordering + a status filter inside the
existing `Alt+Shift+a` roster. **Why** — ux-expert's verdict: the real gap is
urgency ordering, which costs zero columns, where the rail is the largest
permanent chrome expenditure roost would ever make and argues against
DESIGN §1 (every column is one not showing agent output). The free Alt-chord
pool is also effectively empty. **Revisit** — if users with 20+ panes still
report hunting after the roster change ships.

## D5 · Three adapters now (codex, gemini, opencode), two never (amp, aider)

**Choice** — build codex → gemini → opencode; descope amp and aider.
**Why** — codex/gemini/opencode all resume by explicit session id, which is
exactly roost's `(adapter, cwd, session-id)` model. amp is thread-based and
would break that model; aider has no session-id resume at all and would be
heuristic-only, i.e. no better than `shell`. **Revisit** — on concrete user
demand, or if amp/aider ship session ids.

## D6 · Distribution is Phase 4's first item, not its last

**Choice** — release binaries + Homebrew tap + binstall metadata built
before the fancier differentiators. **Why** — roost currently has zero
install channels while every competitor has three or more; a tool nobody can
install cannot be category-best regardless of its features. Machinery only —
no tag is pushed and no release is cut without the client. **Revisit** — n/a.

## D7 · Two reviewer findings promoted above their reported severity

**Choice** — the ratatui `terminal.clear()` regression (reported P1) and the
socket no-reply wedge (P0) are both treated as release-blocking P0.
**Why** — the clear() path SIGKILLs every pane on a resize when the host
doesn't answer a cursor query; destroying a user's running fleet is a
data-loss-class outcome regardless of how it was ranked. **Revisit** — n/a.

## D8 · Native macOS parity outranks the inferred backlog

**Choice** — Phase 2N runs alongside Phase 1, ahead of the rest of the UX
backlog. **Why** — stated directly by the client; client requirements
outrank findings the team inferred. **Revisit** — n/a.

## D9 · Phase 2N interaction model: roost mirrors native selection, scoped by the pane's own mouse appetite

**Choice** — option (c) of four. In Normal mode over a pane that has *not*
requested mouse reporting: left-press anchors, drag extends (painted with the
existing C17 REVERSED), release copies to the system clipboard and flashes
"copied N chars"; double-click selects the word, triple-click the row;
Shift+click extends; any click or keypress clears. **No mode, no chord.**
Over a pane that *has* requested mouse reporting (vim, Claude Code's TUI) the
app keeps the mouse, unchanged — the same grabbed/ungrabbed line every
emulator draws. `Alt+c` copy mode survives untouched as the universal
fallback, and the emulator's own bypass modifier is documented per terminal.

**Why** — three findings decide it:
1. **Drag over a plain pane is a dead no-op today** (`mouse.rs:390` returns
   `MouseAction::None`). Making it select costs nothing that currently works,
   and converts "learn Alt+c" into "do what you already do".
2. **Cmd cannot reach a TUI on macOS.** Terminal.app routes every ⌘ combo to
   the menu bar with no escape-sequence path; kitty/Ghostty bind ⌘C/⌘V to
   their own copy/paste; iTerm2 delivers ⌘ only if the user remaps it and
   loses system-wide ⌘C/⌘V in that profile. So ⌘C always means "the emulator
   copies *its own* selection" — which roost's capture prevents from ever
   existing. The only honest way to make ⌘C-shaped intent work is for roost
   to put the text on the pasteboard itself. `clipboard.rs` already does
   (pbcopy → OSC 52, with honest per-channel reporting).
3. Not capturing at all (option a) would forfeit click-to-focus, tab clicks,
   seam-drag resize, wheel routing and P9 alternate-scroll — and would take
   the mouse away from vim and Claude Code entirely. Relying on the bypass
   modifier alone (option b) selects across roost's pane borders, because the
   emulator sees one flat window.

**Revisit** — if a future macOS terminal delivers ⌘ to TUIs by default, or if
users report the auto-copy-on-release surprising them (iTerm2 ships the same
behavior as an option, so precedent is on our side).

## D10 · README's advertised escape hatch is wrong on the client's own terminal

**Choice** — treat it as a defect, fix per-terminal. **Why** — README:144–145
says "your terminal's Shift+drag native selection still works too." Shift is
the bypass in Ghostty and kitty; **iTerm2's is Option**, and Terminal.app has
no hold-to-bypass at all (only the global View ▸ Allow Mouse Reporting
toggle). The single fallback roost advertises names the wrong key on macOS's
two most common terminals. **Revisit** — n/a.

## D11 · Alt+click-to-open-URL is suspected dead on iTerm2

**Choice** — verify before trusting; if confirmed, re-home the gesture off
Option on macOS. **Why** — Option held *is* iTerm2's suspend-mouse-reporting
gesture, so an Alt+click likely never reaches roost — meaning a documented
feature (README:134, C24, the help overlay's mouse row) silently does nothing
on the recommended terminal. Needs a human at a real iTerm2 for 30 seconds.
**Revisit** — after that test.
