# roost keys

Every shortcut in roost, the `config.json` escape hatch that lets you move or
switch off any of them, and how roost handles the mouse.

The README carries the dozen chords worth knowing on day one; this is the
whole surface. Nothing here has to be memorised — `Alt+?` draws the same
table live, filtered as you type, and reflects your own remaps rather than
these defaults.

> **On macOS, Option must be sending Alt** or none of this fires. Two
> settings, one per terminal — see
> [Quick start](../README.md#macos-make-option-send-alt) in the README.

**Contents**

- [Every chord](#every-chord)
- [Remap or disable a key](#remap-or-disable-a-key)
  - [What `"disable"` sends](#what-disable-sends)
  - [Asking roost what it bound: `roost keys`](#asking-roost-what-it-bound-roost-keys)
- [Shift+Enter, and where it does not work](#shiftenter-and-where-it-does-not-work)
- [Dead panes](#dead-panes)
- [Mouse, links & text selection](#mouse-links--text-selection)

## Every chord

| Key | Action |
|---|---|
| `Alt+n` | new shell pane (auto split direction) |
| `Alt+Enter` | quick-launch picker: pi, claude, codex, gemini, opencode, shell |
| `Alt+arrow` / `Alt+hjkl` | move focus (expands stacked panes) |
| `Alt+-` / `Alt+=` | resize height: shrink / grow (vim's Ctrl-w `−`/`+`) |
| `Alt+<` / `Alt+>` | resize width: shrink / grow (vim's Ctrl-w `<`/`>`) |
| `Alt+Shift+arrow` / `Alt+Shift+hjkl` | move the focused pane that way within the tab — swaps it with its neighbour, reorders inside a stack |
| `Alt+s` / `Alt+Shift+s` | stack: collapse the surrounding split into a stack, focused pane expanded — **press again to absorb the next split out**, up to the whole tab / explode the stack back into the split it came from |
| `Alt+o` | flip the focused split's orientation (vertical ⇄ horizontal) |
| `Alt+g` / `Alt+Shift+g` | cycle layout forward / back: even grid → main pane + stack → all-stack (skips shapes that don't fit) |
| `Alt+z` / `Alt+Shift+z` | zoom the focused pane to fill the screen — view only, layout stays put (`Alt+z` again, a tab switch, or any layout edit exits) / toggle the floating scratch shell — the two view toggles, on one physical key |
| `Alt+a` | jump to the next pane that needs input, across tabs, wrapping (zsh accept-and-hold collision — same remedy) |
| `Alt+;` | go back to the pane you came from — toggles, and follows across tabs (tmux's `prefix ;`) |
| `Alt+Shift+a` | fleet roster — every pane, grouped by tab, opening on the one `Alt+a` would jump to |
| `Alt+'` | broadcast — compose one message, send it to every pane (`Tab` picks who: all, or one status tier); the title shows how many will get it |
| `Alt+e` | activity feed — status changes, spawns, closes/reopens, exits, control calls |
| `Alt+r` | edit pane — name and parking note in one dialog; the note's first line shows on the badge |
| `Alt+Shift+r` | rename tab (e.g. one tab per project) |
| `Alt+t`, `Alt+1..9`, `Alt+0` | new tab / go to tab / go to the last tab |
| `Alt+m` / `Alt+Shift+m` | next / previous tab (wraps — the route to tabs past the ninth) |
| `Alt+i` / `Alt+Shift+i` | carry the focused pane to the next / previous tab |
| `Alt+Shift+x` / `Alt+Shift+v` | mark a pane / pull the marked pane into the tab you're on — a move whose destination is any tab, not the next one |
| `Alt+w` | close pane (press twice to confirm when the agent is busy or it's the last pane) |
| `Alt+u` | undo — reopen the last closed pane or tab, sessions resumed (exact scope below) |
| `Alt+c` | copy mode — `hjkl`/arrows + `v` mark + `y`/`Enter` yank, or drag with the mouse (`Esc`/`q` exits) |
| `Alt+PgUp` | scroll mode (`↑/↓/PgUp/PgDn` scroll, `Esc`/`q` exit) |
| `Alt+Shift+p` | raw pass-through for the focused pane — same chord exits it |
| `Alt+/` | toggle the shortcut hint bar |
| `Alt+?` | show the full keymap (type to filter it, `↵` runs the row it lands on, `Esc` closes) |
| `Alt+q` | quit — workspace saved; agents die, sessions live |

A shortcut hint bar runs along the bottom by default (zellij-style), showing
the keys you can press right now — it changes with context, so rename /
picker / scroll / copy / feed / dead-pane modes each show their own keys, and
a raw-focused pane collapses it to one pair (`Alt+Shift+p exit raw`).
`Alt+/` hides it to reclaim the row.

## Remap or disable a key

Every shortcut above lives on `Alt`, which can collide with your shell's own
readline bindings (`Alt+b`/`Alt+d` are the usual culprits — `Alt+f` used to
be a third, but roost stopped binding it by default on 2026-09-03: see
below). Fix one in `config.json`. No file — the default — and roost behaves
exactly as documented above.

```json
{ "keys": { "alt+shift+z": "disable", "alt+v": "toggle_float" } }
```

**Want the old `Alt+f` chord back?** Roost bound `Action::ToggleFloat` to
`Alt+f` through v0.1.x, and unbinding it by default was a deliberate,
non-negotiable fix — on any terminal without the kitty keyboard protocol,
Alt+Right is delivered as the *identical bytes* as Alt+f (`ESC f`), so
binding `Alt+f` at all meant Alt+Right silently opened the float. If you
still want the shell's readline `M-f` forward-word given up for the float
instead, that is now an informed choice you make yourself:

```json
{ "keys": { "alt+f": "toggle_float" } }
```

### Where the file goes

`~/.config/roost/config.json`. On macOS, `~/Library/Application
Support/roost/config.json`.

roost also still reads it from the state dir next to `workspace.json`
(`~/.local/state/roost/config.json` on Linux), which is where it lived
before — an existing file there keeps working and keeps winning if you
somehow have both, and `roost keys` names the one actually in force. New
files belong in the config dir above.

Not sure? Ask:

```console
$ roost keys >/dev/null
roost keys: no config.json — create /home/you/.config/roost/config.json to change these bindings
```

Under [`ROOST_STATE`](../README.md#a-separate-workspace-is-one-environment-variable)
the search stops there and `$ROOST_STATE/config.json` is the only file read —
that is the point of the variable, and it is how you try a remap without
touching your real one. (`roost keys` skips the location line in that case:
you named the directory, and a clean config saying nothing on stderr is a
property scripts rely on.)

A chord is `alt+<key>` or `alt+shift+<key>` — nothing else parses (`ctrl+f`
is rejected, deliberately), and `<key>` is one character (`alt+f`, `alt+3`,
`alt+/`) or a named key: `enter`, `pageup`, `up`, `down`, `left`, `right`
(`alt+enter`, `alt+pageup`, …).

### What `"disable"` sends

roost reads *parsed* key events, not the byte
stream, so `Alt+←` arrives identically whether your terminal spells it
`ESC [1;3D` or `ESC ESC [ D` — roost cannot replay what it never saw, and has
to pick one spelling to forward. It picks **meta-ESC**, the same convention
every forwarded Alt chord already uses: `ESC f` for `Alt+f`, `ESC ESC [ D`
for `Alt+←`, `ESC CR` for `Alt+↵`. So this gives word motion back to a shell
that binds the meta-ESC form:

```json
{ "keys": { "alt+left": "disable", "alt+right": "disable" } }
```

and `Alt+h` / `Alt+l` still move focus between panes — the arrows and the vim
letters are separate bindings, so disabling one spelling keeps the other.
If your shell only binds the CSI form, add the meta-ESC one next to it —
`bindkey "^[^[[D" backward-word` (zsh) or `"\e\e[D": backward-word`
(`~/.inputrc`).

The one exception is a pane that negotiated the **kitty keyboard protocol**
(most modern TUIs do). Asking for disambiguation is asking to be told which
modifiers were held, and meta-ESC cannot say — `ESC ESC [ D` is the same
bytes with or without Alt. Those panes get the CSI form (`ESC [1;3D`)
instead, so no shell-side binding is needed. It is the only case where roost
picks the encoding rather than defaulting to meta-ESC, and the only one
where it can do so without guessing, because the pane declared what it
wants.

### Asking roost what it bound: `roost keys`

A value is `"disable"` (the chord passes straight through to the pane) or a
snake_case `Action` name — **`roost keys` prints every one of them**,
alongside the chord it is currently on:

```console
$ roost keys | head -3
Alt+/	toggle_hints
Alt+0	last_tab
Alt+1	go_to_tab_1
```

It reads `config.json` directly and needs no running roost, so it answers
before you launch. Stderr names the file it read, or the one to create —
except under `ROOST_STATE`, where you have already named the directory
yourself and a clean config stays silent. Remapped and disabled chords are
marked `config.json`, and
an entry roost had to skip is named on stderr with a non-zero exit — so a
dotfile test can gate on it instead of you catching a startup toast. Only a
*skipped* entry sets that exit code. Rebinding a chord that already had a
default is ordinary use, not a failure: it exits 0, and stderr says so only
when the displaced action is left with no chord at all — a swap like
`{"alt+w": "new_pane", "alt+n": "close_pane"}` is silent, because neither
action lost its way in.
**`Alt+?` is also the command palette.** Press `/` inside it and the keymap
becomes a picker: type to narrow, `↑`/`↓` to choose, `↵` to run the row —
no need to dismiss the overlay and remember the chord. The rows it can run
are the single-verb ones (flip split, cycle layout, zoom, undo, rename,
mark/pull a pane, the raw/float/feed/roster toggles); direction families
like `Alt+←↓↑→` and the `roost send`/`read` reference rows stay read-only,
because "resize one notch, then close" is worse than the chord it would
replace. The title says `↵ runs` exactly when there is something to run.

**`Alt+?` and the hint bar follow your remaps** — both read the live keymap,
so they show the chord you bound and stop showing the one you disabled,
rather than teaching the defaults at you. A
chord entry covers however your terminal happens to report that key (e.g.
`alt+shift+p` and `alt+P` are the same chord), so one entry is enough
regardless of which encoding your terminal uses. A chord listed twice keeps
only its last value. Bad JSON, an unknown action, or an unparseable chord
never blocks startup — roost starts with its defaults and names the bad
entry (toast + activity feed). Read once at startup, not watched for
changes.

## Shift+Enter, and where it does not work

Everything else passes straight through to the focused pane. **Shift+Enter**
and **Ctrl+Enter** are sent as "insert newline" rather than "submit", so you
can compose multi-line prompts in agent TUIs that support it — this needs a
terminal that reports modified keys via the CSI-u ("kitty") keyboard
protocol (**iTerm2, Ghostty, kitty, WezTerm**), which roost negotiates on
start.

> ⚠️ **Not macOS Terminal.app.** It sends Shift+Enter and Option+Enter as the
> same bytes (`ESC CR`), which roost can only read as Alt+Enter — so on
> Terminal.app, Shift+Enter opens the quick-launch picker instead of
> inserting a newline. Use one of the CSI-u terminals above to compose
> multi-line prompts.

## Dead panes

In a pane whose process exited or failed to spawn: `Enter` relaunches or
resumes, `f` starts fresh (drops the stored session id).

## Mouse, links & text selection

**Mouse**: the wheel scrolls the pane under the cursor — forwarded to the
inner app when it has mouse reporting enabled (pi/claude TUIs, vim, less),
otherwise it scrolls roost's own scrollback for that pane; typing snaps back
to the live tail. A left click focuses a pane (and expands collapsed stack
members). Over a mouse-aware app, clicks and drags are forwarded too, so you
can interact with an agent's TUI directly (menus, buttons, selection). Click
a tab in the tab bar to switch to it.

**Opening links**: `Alt`+click a URL in any pane to open it in your browser
(`open` on macOS, `xdg-open` on Linux). roost uses `Alt`+click rather than a
plain click so it doesn't fight click-to-focus, and because a terminal can't
report Cmd-clicks to it.

**Text selection**: in a normal pane, **drag to select — the highlight stays
lit until the next click or keypress**. Double-click selects a word,
triple-click selects the whole line, Shift+click extends the selection. On
release, roost copies the text to your system clipboard (via a native helper
— pbcopy / wl-copy / xclip — and OSC 52, so it works locally and over SSH).
No mode, no chord: exactly like a native macOS or Linux terminal.

If a pane is running an app that asked to handle the mouse itself (vim, Claude
Code's TUI, or any interactive command), roost stays out of the way — the app
sees clicks and drags directly, and you can use **`Alt+c` copy mode** instead
(press `Alt+c`, move the cursor with hjkl/arrows, press `v` to mark, press `y`
or `Enter` to copy and exit). That's also the way to copy from scrollback: scroll
with `Alt+PgUp`, then `Alt+c` to select.

**Why there's no ⌘C to press:** a terminal application cannot receive ⌘C on
macOS at all — Terminal.app routes it to its own Edit menu, kitty and Ghostty
bind it to their own copy, and iTerm2 only delivers it if you remap Command
and give up system-wide copy/paste in that profile. So roost doesn't try:
it puts the text on the system pasteboard itself the moment you release the
mouse (or press `y` in copy mode), which is why ⌘V works everywhere
afterwards and there is nothing to press in between. On Linux, your terminal's own
clipboard shortcuts (Shift+Ctrl+C/V, middle-click, etc.) work for the system
clipboard; roost's own selection uses the same pasteboard they do.

If your terminal has a modifier to suspend mouse reporting (Shift+drag in
Ghostty/kitty, Option+drag in iTerm2), you can use it to fall back to your
terminal's native selection in any pane — but a mouse-aware app keeps the
mouse regardless, and copy mode is there if you need it.


---

*Part of [roost](../README.md). The chord table above is the canonical list
of what roost binds by default; `DESIGN-ui.md` §8 records why each key was
chosen and what it displaced.*
