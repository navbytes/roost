# Reliability audit — findings

**Scope.** "The most important thing about this tool is reliability. Do a
thorough investigation and find all bugs." Everything below was reproduced
before it was reported, fixed with a test, and mutation-checked (the fix was
reverted and the test confirmed to fail). Branch:
`claude/ui-ux-keybindings-analysis-3hqs3v`, sixteen commits on top of v0.1.9.

Suite at the end of the audit: **935 unit + all integration suites passing**,
two pre-existing environmental failures (both assume a non-root user;
`chmod 000` does not stop root, so they fail in this container and pass on a
normal machine). Clippy unchanged at its 8-warning baseline.

---

## Critical — silent data or process loss

### 1. Closing the terminal left the whole fleet running
`fc47bf0` · `src/infra/signals.rs`, `src/main.rs` · gated by `tests/signal_shutdown.rs`

Every pane is `setsid`'d into its own session, so a signal aimed at roost
reaches roost alone — nothing propagates to the fleet. No handler was
installed, so SIGHUP and SIGTERM took their default disposition: roost died
on the spot and `shutdown()` never ran. **Every agent kept running,
detached, until the machine rebooted.**

This is not an edge case. Closing the terminal window, an ssh connection
dropping, `kill` from a supervisor, and logout all arrive this way — for most
users far more often than Alt+q does.

Fixed by catching SIGHUP/SIGTERM/SIGINT and setting a flag the main loop
checks each iteration, so the host gets the identical teardown Alt+q gets.
The gate plants a backgrounded job in its own process group, signals roost,
and asserts nothing survives. Before the fix it failed with a live pid; after,
both signals are clean.

### 2. A pane id parked on the undo stack could be handed out twice
`37a8bda` · `src/core/app.rs`

`alloc_pane_id` was max(live ids)+1 and did not count the ids a closed *tab*
holds on the undo stack. Close a tab, open a pane, press Alt+u: two panes
share one id — therefore one entry in `runtimes`, one PTY, one socket token.
Keystrokes for one land in the other.

### 3. An exited pane leaked every background process it had started
`d9946ca` · `src/infra/pty.rs`

`kill()` gates every cleanup path on `self.pid`, which `on_exit` nulls the
moment the child is reaped — so a pane's shell exiting on its own (the normal
way a pane dies) turned all later cleanup into a no-op. Anything the shell had
backgrounded survived close, respawn and quit alike. The session sweep now
runs at the instant of exit, while the session id is still unambiguously ours.

### 4. The workspace could be truncated by a power cut, and its loss was silent
`f986f8c`, `49796dd` · `src/infra/store.rs`, `src/main.rs`

Three compounding problems in the one file the whole tool is:

- **No fsync.** `save()` called `Write::flush`, which on a `std::fs::File` is
  a no-op that reads like a durability barrier. Without a real fsync the
  rename can reach disk before the bytes do, so a power cut or kernel panic
  leaves `workspace.json` truncated.
- **`load()` treats an unparseable file as "no workspace"** — so that
  truncation costs the user their entire fleet. The durability gap and the
  discard rule compound into real data loss.
- **The discard was silent.** The bad file is moved aside as
  `.corrupt-NNN` — the right call, since launch must not brick — but nothing
  said so. The user launches roost, every tab is gone, and the only copy of
  their fleet is an undiscoverable file. Now reported the way a bad
  `config.json` already is: a startup flash plus a feed line, naming the file
  and where the salvage went, ordered last so it wins the one flash slot.

### 5. `version` was written and never read
`49796dd` · `src/infra/store.rs`

serde ignores fields it does not know, so a `workspace.json` from a newer
roost parses cleanly and the next save rewrites it with everything new
stripped out — a silent downgrade of state the newer roost still needs. A
higher-than-known version is now treated as unusable-but-precious: set aside,
and said out loud.

---

## High — wrong session resumed, wrong pane driven

The bug you reported ("when I quit and launch roost I think it loads wrong
session") was real, and it was four bugs, not one. All four are in the
filesystem fallback that attributes a session file to a pane — the path that
runs whenever the agent's own extension channel is not connected.

### 6. A promoted pane could claim a conversation from days ago
`52f4b7d` · `src/core/app.rs`

When you type `pi` at a shell prompt, `observe_panes` promotes the pane and
detection starts — with a lower bound of `UNIX_EPOCH`, i.e. none at all. The
scan then claimed the newest *unclaimed* file in that project whenever it was
written. Open a pane, type `pi`, and you could be handed a conversation from
last week. Now bounded by the last moment the pane was observed as a plain
shell, less a grace window for the 2s observation lag.

### 7. Two projects differing only in punctuation shared session detection
`e51c4b5` · `src/agents/pi.rs`

`owns_session_file` compared the alphanumeric runs of a cwd *concatenated*
rather than as a sequence, so `~/work/my-app` and `~/work/my/app` (and
`my_app`, `my.app`) hashed to the same key. Two projects open at once, and
either could resume the other's conversation. Now compared as a sequence.

### 8. Codex panes in different projects were mutually mis-assignable
`a764a97` · `src/core/app.rs` + `DESIGN.md` §6.1

`~/.codex/sessions` is global and carries no cwd signal, so two codex panes in
different projects are genuinely indistinguishable to the scan — the only
separator is mtime order, which says nothing about ownership. The scan now
*declines* rather than guessing when two pending panes share an adapter and
root but differ in cwd. Declining costs a fresh start; guessing costs the
wrong conversation.

### 9. Detection retried forever, walking the whole session root twice a minute
`f986f8c` · `src/core/app.rs`

`pending_detect` was cleared only on success or when the pane vanished, so a
pane whose agent never wrote an attributable file stayed pending for the life
of the process — and since no adapter overrides `detect_session`, each tick
meant a full recursive walk of `~/.pi/agent/sessions` (every project, all
history), stat-ing every file, forever. Now bounded by a 60s give-up horizon.

---

## High — structural state drifting apart

Found by re-running the layout invariant fuzzer against the current tree,
then strengthening it. It is now a standing test (`d86e26f`): 200 seeds ×
400 operations, 26 action kinds including the control plane, re-checking
every invariant after *every* operation. A deep sweep of **1.8 million
operations** is clean.

### 10. A 16-way split rounded its last child out of existence
`40c8b3e` · `src/core/layout.rs`

Each child was sized from its own ratio, rounded independently, and the
errors accumulate: sixteen equal children of a 58-row body each round to 4, so
the last gets 58 − 15×4 = −2 → 0. A zero-area subtree returns early from the
walk, so **that pane gets no rect, no PTY resize and no pixels — its process
keeps running, off-screen, unreachable.** Alt+s on a stack of 16 explodes into
exactly this. Sizes now come from cumulative ratios, so the parts always sum
to the whole.

### 11. A stack of one built an illegal tree
`11a1b2a` · `src/core/layout.rs`

Splits collapse at one child; stacks did not. The leftover
`Stack { children: [x] }` was not merely cosmetic ("STACK · 1 PANES", a header
row stolen from its only member): `toggle_stack` explodes a stack into an even
split of its members, so **Alt+s on a stack of one built a `Split` with a
single child** — a shape the rest of `layout.rs` states it never constructs.

### 12. Three float crossings, all invisible
`11a1b2a`, `71999c1` · `src/core/app.rs`

- **Closing the zoomed pane under the float left the zoom pointing at a
  ghost.** `close_pane_id` asked `id == self.focused`, but while the float is
  shown focus belongs to the float, never a tiled pane, so the test could not
  fire. The renderer was then handed a rect for a pane that was gone.
- **Closing any pane stole focus from a shown float.** The "keep focus inside
  the tab on screen" fallback asks whether `self.focused` is in the active
  tab's map; the float lives outside every tab's map *by construction*, so the
  answer was always no. Float still drawn, keystrokes going somewhere invisible.
- **A control spawn hid the float it borrowed focus from.** `ctl_spawn_child`
  restores `focused` but not the float's `shown`, leaving hidden-and-focused —
  a state the model does not have. `cycle_layout` then planted the float's id
  into the tab's layout tree as a pane with no spec: **the tree↔map invariant
  broken by an API call the user never saw.**

C22 rule 1 ("a shown float is the focused pane") was being enforced per call
site, so each new focus path had to remember it — and C35's go-back did not.
It is now enforced in `set_focus`, the documented single writer of
`self.focused`.

### 13. One pane id could name two panes after a hand-edited workspace
`49796dd` · `src/core/workspace.rs`, `src/core/layout.rs`

`validate_and_repair` reconciled each tab's `panes` map against its own layout
and never asked whether an id appeared twice. A pane id keys `runtimes`
globally, so a repeated id — within one tree or across two tabs — means both
positions drive a single PTY: it draws in two places, keystrokes land in both,
`tab_of` answers with whichever tab comes first, and closing either takes the
other with it. Now deduped across all tabs in one pass.

---

## Medium — refusals, denial, and a lie in the chrome

### 14. One pane could deny the `wait` verb to the whole fleet
`41d4bb5` · `src/core/app.rs`

Parked waiters are freed only by firing or timing out, so sixteen
fire-and-forget `wait`s with long timeouts denied the verb fleet-wide for up
to 24 hours. Now a per-actor share (4) plus a cap on how many panes one wait
may name (64) — the pane list arrives unbounded from the socket and
`poll_waiters` walks every id on the UI thread each iteration.

### 15. A failing save showed nothing on an ordinary terminal
`f986f8c` · `src/core/app.rs`

The failure reached exactly one surface: the tab bar's right-aligned
`save failed ✗`. C2's yield ladder drops that area *whole* when the tabs need
the room, so on an 80-column terminal with five tabs roost could fail every
write — read-only state dir, full disk, wrong ownership — and show nothing,
until the user relaunched into a fleet hours stale. The error text was
discarded by `.is_ok()` on the way, so even the indicator could not say why.
Now a C10 flash on the ok→failed transition (not on every save, which would
bury the bar), carrying the innermost cause.

### 16. Alt+u split a pane Alt+n had just refused to split
`1c6d10e` · `src/core/app.rs`

`restore_pane` hand-rolled the aspect test and never checked the
`MIN_SPLIT_COLS`×`MIN_SPLIT_ROWS` floor — which is also the vt100 underflow
trigger. Reopening a closed pane landed it in a slot too small to render in
and took the pane it split down there with it. Now uses `split_fit`, the one
both paths share; when it refuses, the close is *not* consumed — the entry
goes back on the undo stack so the pane is still reopenable once there is room.

### 17. A closed pane bequeathed its attention state to the next pane
`05fe54e` · `src/core/app.rs`

`last_status`, `needy_msgs` and `visited_waiting` were pruned only on the tick,
under a comment claiming ids are never reused. The close path a thousand lines
away says the opposite, and is right. Close the highest-id pane and open one
inside the 2s `DETECT_INTERVAL` and the newborn inherited all three: silently
absent from Alt+a's fallback though nobody had looked at it, its first status
observation read as a transition (a feed line for a life it did not live), and
the dead pane's question attributed to it.

### 18. Non-UTF-8 socket lines escaped the rate limiter
`41d4bb5` · `src/infra/sock.rs`

A line that failed UTF-8 decoding `continue`d before being charged to
`line_bucket`, so a caller could send unlimited garbage for free.

---

## Open — needs your decision, not more investigation

### A. Control-plane denial of service (measured)
`20da213` corrects the spec; **the code is unchanged.**

`DESIGN-control.md` claimed a pile of squatters leaves "nobody else ever
blocked from getting in". That is measurably false. Against a real instance:

| attack | cost to a legitimate `roost list` |
|---|---|
| 64 silent connections, held | **2.07 s**, 21 refused attempts |
| squatter retaking each slot as it recycles | **12.1 s**, 118 refused attempts |

and it got in at that point only because the probe's retake loop was not
tight. The 2s pre-auth deadline bounds how long any *one* squatter is
subsidised; it does not bound the denial, because reconnecting is free.

Sustained, this is a control-plane DoS available to any local process that
can reach the socket — **including a hostile process inside a pane**, which is
precisely the principal the per-pane tokens exist to contain. The tokens still
hold (nothing can be *driven* without one); what is deniable is access itself.

**The decision:** a pre-auth pool sized separately from `MAX_CONN` would bound
the blast radius to unauthenticated callers and leave promoted connections
(including parked `wait`s) untouched. But no scheme distinguishes a squatter
from a first-time legitimate client before either has authenticated — same-uid
peer credentials do not help, because the hostile pane has them too. **This is
mitigable, not closable**, and it changes a documented cap, so it is your call.
My recommendation: take the mitigation; a bounded window for unauthenticated
callers is strictly better than a shared one, and the residual risk is
acceptable for a local-only socket.

### B. Should the failed-save indicator outrank tab names?
Finding 15 added a flash, which cannot be crowded out. The *standing*
indicator can still be dropped whole by C2's yield ladder. Making it outrank
tab names is a C2 contract change — yours to make.

### C. `~/.claude/projects` encoding — needs one command from you
roost's `encode_cwd` maps `/`, `.`, space and `_` all to `-`. If Claude Code's
real naming is narrower, roost over-maps and two projects can collide the way
finding 7 did for pi. I cannot verify this from here. Please run:

```
ls ~/.claude/projects
```

and paste the output. If any directory name contains a character roost would
have mapped to `-`, this is finding 7 again for a second adapter.

### D. One-member stack chrome
Finding 11 fixed the illegal *tree*. The chrome that produced it — a header
reading "STACK · 1 PANES" — lives in `src/ui/render.rs`, which CLAUDE.md says
gets a design-supervisor audit before it changes. I have not touched it; the
collapse fix means the state should no longer be reachable, but the grammar
bug is still in the code path if it ever is.

---

## What I did not examine

Stated plainly so the gaps are known, not assumed covered:

- **Rendering correctness** beyond geometry — no audit of what the renderer
  draws, only of what it is *handed*. `src/ui/**` changes need the
  design-supervisor pass per CLAUDE.md, which I did not run.
- **The vt100 vendor fork** (`vendor/vt100`) — treated as a dependency.
- **Adapter resume commands** for gemini and opencode — the two adapters with
  no session root got no scrutiny beyond compiling.
- **macOS-specific paths** — everything here was reproduced on Linux. The QoS
  promotion, `LOCAL_PEERCRED`, and the 104-byte `sun_path` limit are
  macOS-only behaviours I could not exercise.
- **Extension/hook integration** with live pi and Claude Code binaries — the
  status channel was tested through its own socket protocol, not against the
  real agents.
- **Performance under a large fleet** — the fuzzer runs 20-odd panes; nobody
  measured 100.
