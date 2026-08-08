# SPEC-parity — lessons from peer terminals: verified defects & contracts

**Status: CLOSED — P1–P21 are all fixed or shipped.** One sub-item is
deliberately left undone and says so in place: P21's dump-to-editor half. A
spec of defects and gaps that peer tools (tmux,
zellij, wezterm, kitty, alacritty, ghostty, Claude Code's own issue tracker)
spent years discovering, verified present in roost. Companion to SPEC-ux.md
(which catalogues gaps found by direct UX review); items here came from mining
peer issue trackers and were **individually verified against the real binary**
before being written down.

**Method.** Stage 1: research over peer projects' open+closed issues (public
web), calibrated against SPEC-ux.md and shipped fixes so only new failure
modes surface; each candidate carried a runnable verification recipe. Stage 2:
every recipe executed against roost — unit probes against `translate`/vt100/
`extract_selection`, PTY-harness sessions with raw host-byte capture, and the
control CLI as ground truth. Stage 3 (this doc): only verified items, with
corrected mechanisms where the research guess was wrong. A separate deep-dive
diagnosed the startup-stall cluster (P4) end to end with measurements.

**Verdicts:** `CONFIRMED` (reproduced, evidence in Appendix A) ·
`PARTIAL` (symptom real, mechanism corrected) · `GAP` (missing feature, not a
bug). Severity is impact for a user supervising AI-agent panes.

---

## Workstreams (fix-surface grouping — the order they were implemented in)

- **W1 · Pane env hygiene** — P11. Small; shrinks the blast radius of W2/W3
  (apps stop sniffing iTerm/kitty/tmux identity and negotiating protocols
  roost can't back). Sequence first.
- **W2 · Query responder** — P4 (+ the DECRQM half of P1). Implementation-
  ready; full byte-precise design in Appendix B. Kills the yazi/atuin startup
  stalls and the crossterm infinite hang.
- **W3 · Pane escape-traffic channel** — P1, P2, P3, P6, P7. One missing piece
  of plumbing unlocks all five: vt100's `Perform` impl has no way to surface
  events (or re-emit sequences to the host). Includes the vendored-parser arms
  (2026, 1004, OSC 9/52/777, DECSCUSR, SGR 2/9, REP) shared with W6.
- **W4 · Input gating** — P12, P13. Two guards in `translate`; P13's fix is
  the same change as SPEC-ux U5 (meta-ESC fallthrough via `encode_raw`).
- **W5 · Scroll/zoom state truth** — P5, P9, P14 (folds into/amends SPEC-ux
  U9), P7's scrolled-cursor facet (belongs with U3/N1). **P5 + P9 shipped** —
  the live grid reflows on resize (scrollback keeps its historical width, the
  alternate screen never reflows), and the wheel over a protocol-less
  alternate-screen app becomes arrow keys; amends DESIGN-ui C21 and §5
  (2026-07-27). P14 closed later, in two passes — the second deleted the two
  shadow scroll-offset caches outright, so the defect became unrepresentable.
- **W6 · Width & styling fidelity** — P15, P16, P17, P19 (+ SPEC-ux U24 as one
  bundle: unify unicode-width, fix continuation cells in blit *and*
  extraction, add missing SGR/REP arms). **Shipped** — all five FIXED; amends
  DESIGN-ui C18 (2026-07-27).
- **W7 · Mouse & shell polish** — P18, P20, P21. P18/P20 shipped; P21's
  search half shipped (dump-to-editor deferred, see the item).

---

## P0 — breaks the agent-supervision loop

### P1 · FIXED (this branch) · High — synchronized output (mode 2026) neither honored nor forwarded
*Fixed: the vendored parser tracks mode 2026 and captures the last complete
frame at the exact stream position of `?2026h` (`Screen::snapshot` — visible
grids only, no history clone); `PtyPane` presents that frame from `screen()`
and `grab_text` until the bracket closes, with a 150 ms staleness cap so a
stuck bracket can never freeze a pane. DECRQM 2026 now answers 1/2 instead of
0. e2e `tests/pane_sync_output.rs`: 29/60 `roost read` samples caught the pane
mid-bracket before, 0/60 after.*
Apps wrap redraws in `CSI ?2026h … ?2026l` expecting atomic presentation —
the mechanism behind a year of "Claude Code flickers/tears in tmux" reports
(claude-code #37283; tmux PR #4744; zellij #4693). roost's vt100 has no 2026
arm and the render loop blits whatever the parser holds every ~33 ms tick.
**Measured:** 31 of 50 server-side `roost read` samples caught the pane's
grid mid-bracket (cleared/partial); 42 of 80 host frames torn; `?2026` never
reaches the host.
**Contract:** track mode 2026 in the vendored parser; while a pane's bracket
is open, the renderer presents that pane's *last pre-bracket* grid (with a
staleness cap so a stuck bracket can't freeze the pane); answer DECRQM 2026
honestly per what ships (see W2). *(The DECRQM half shipped with W2: 2026 —
like every untracked mode — answered `0`, "not recognized"; W3 flipped it to
1/2.)*
**Split, deliberate:** the presentation view serves the surfaces that answer
"what does this pane look like right now" — the blit, the host cursor,
copy-mode extraction, and `roost read`'s screen mode. Scroll state, the input
modes, and `read --full`/`--tail` stay on the live grid: the snapshot is a
≤150 ms veneer over the visible frame, never a second terminal state (it
carries no scrollback at all, which is also why a scrolled-back pane ignores
it — U3's frozen view wins).

*Amended 2026-08-08 (C29 selection freeze):* the presentation view now has a
second tenant. A native-selection drag holds its own snapshot for the length
of the gesture — far longer than 150 ms, and it wins ahead of this veneer —
so that the highlight and the copied text agree on a pane whose output is
still moving. It carries no scrollback either, and the same U3 rule applies.
One consumer is deliberately exempted: `roost read`'s screen mode reads
through the gesture freeze to the live frame, because a control client
polling a pane must not receive a stale one because a human is dragging a
mouse. The full rule, its three release sites, and the resize invalidation
live in DESIGN-ui.md C29.

### P2 · FIXED (this branch) · High — OSC 9 / 9;4 / 777 notifications vanish (and don't count as attention)
*Fixed: the vendored parser turns OSC 9 (`9;body`) and OSC 777
(`777;notify;title;body`) into `Effect::Notify`; `PtyPane` puts each one on
the same attention path as a bell (`StatusTracker::on_bell` → ◆ once the pane
rests) and queues a bounded re-emission to roost's own stdout — max 1 per pane
per second, 200 chars, C0-stripped so an untrusted body can't break out of the
sequence — written between draws by the main loop. The nudge names the pane
via U2's `display_name` and, like every other roost notification, is skipped
for the focused pane. e2e `tests/pane_notifications.rs`: `"status": "waiting"`
and no `ESC ]9;` in the host capture before; `needs_input` + a verbatim
`ESC ]9;NEEDS-YOU BEL` after.*
***Deferred:** OSC 9;4 (progress) is recognized so it can never be mistaken
for a notification body, then dropped — surfacing it as a badge percentage
stays out of scope, as this item's contract already said.*
Claude Code emits desktop notifications via OSC 9 and progress via OSC 9;4
(claude-code #19976, #57366). roost drops them at `osc_dispatch` (only OSC
0/1/2 handled) — and because the OSC-terminating BEL is deliberately not
counted as a bell, the NeedsInput heuristic never fires either.
**Measured:** after a pane emitted `OSC 9 ; NEEDS-YOU BEL` + 3.3 s quiet,
status stayed `waiting`; the control probe (a real BEL) flipped it to
`needs_input`; no `ESC ]9;` ever reached the host stream.
**Contract:** an OSC 9/777 notification from a pane (a) maps to the same
attention path as a bell (badge ◆ + notifier), and (b) is optionally
re-emitted to the host terminal so native desktop notifications fire.
OSC 9;4 progress may be surfaced later (badge percentage) — out of scope here.

**Extended 2026-08-07 (ux P1-6):** (b) held only for OSC 9/777 — a pane's
*own* bell (the heuristic ◆ path this same item put on "the same attention
path as a bell") was consumed by the vt100 parser and never re-emitted, so
the contract's own re-emission half was false in exactly the fallback case
it exists to serve. `PtyPane::queue_host_bell` closes it: a raw BEL relay,
gated on `!StatusTracker::reported()` (no live extension already owns the
notify) and rate-limited on the same per-pane gate as (b)'s OSC 9 relay.
e2e `tests/pane_notifications.rs::a_pane_bare_bell_relays_to_the_host_and_is_rate_limited`.
Not a DESIGN-ui.md concern — this is host I/O (`PaneEffects.host_writes`),
never a drawn `Frame`/`Buffer`; this file is its contract.

### P3 · FIXED (this branch) · High — inner-app OSC 52 clipboard writes are discarded
*Fixed: the vendored parser surfaces `52;<sel>;<base64>` as
`Effect::Osc52Write` and refuses to surface reads (`52;<sel>;?`, or a
truncated sequence with no payload) at all — the effect that would carry one
does not exist, so no future call site can accidentally answer a paste-theft
probe. `PtyPane` relays writes to the host through the same queued
between-draws path as P2, capped at 100 KB of base64 and validated to be
actual base64 with a real xterm selection target — a payload carrying ESC/BEL
would otherwise close roost's own sequence and repaint the user's terminal.
roost's own copy-mode OSC 52 path is untouched. e2e
`tests/pane_clipboard.rs`: `ESC ]52` absent from the whole host capture
before, `ESC ]52;c;cm9vc3Q= BEL` verbatim after, and a `?` read still absent.*
***Deferred:** an over-cap or malformed write is dropped silently — roost has
no channel to tell the pane its "copied" was a lie (SPEC-ux U14's other
half), and a user-facing flash for it is not in this wave.*
An app inside a pane (Claude Code copy action — claude-code #63054/#63061,
nvim+osc52, anything over SSH) sets the clipboard via `OSC 52`; roost eats it;
the app reports "copied" over an unchanged clipboard — the same lie SPEC-ux
U14 documents for roost's own copy path, now for inner apps.
**Measured:** pane emitted `OSC 52;c;aGk=`; the bytes `ESC ]52` never
appeared anywhere in the captured host stream.
**Contract:** forward pane OSC 52 writes to the host terminal (roost's own
copy mode already proves the host path works); optionally cap size and gate
reads (`52;c;?`) which are a paste-theft vector — forward writes, refuse reads.
*Cross-reference (2026-07-27): both halves of this lie are now closed on this
branch. SPEC-ux U14 made roost's own copy path report which channel actually
took the text (`copied N chars` / `copied N chars (OSC 52)` / `copy failed`)
instead of flashing success unconditionally; P3 (above) stopped roost eating
a pane's own OSC 52. The two paths stay independent — U14's outcome flash
runs at the copy call site, while a pane's write is relayed through the
queued host-write channel W3 introduced.*

### P4 · FIXED (this branch) · High — the terminal-query black hole
*Fixed per Appendix B: `src/infra/queries.rs` (kitty.rs generalized) answers
DA1/DA2/DSR 5·6/DECRQM/XTVERSION/XTWINOPS 18t (14/16t only with real pixel
geometry, now plumbed host→pane through spawn/resize into PTY winsize);
replies post-parse, stream-order. roost's own startup no longer blocks on
the enhancement probe (259 ms vs 2011 ms first frame in the harness gate;
e2e: `tests/pane_queries.rs`).*
roost answers exactly one query (kitty `CSI ?u`) and swallows DA1, DSR-CPR
`6n`, DECRQM, XTVERSION, XTWINOPS 14/16/18t, XTGETTCAP — worse than answering
nothing: crossterm-0.28's `supports_keyboard_enhancement()` gets its kitty
half answered and then **blocks forever** waiting for DA1 (measured: hung at
10 s; on a bare PTY it times out at 2.002 s). atuin's default inline viewport
**aborts at ~2.045 s** waiting for a cursor-position report (50 ms once
answered). yazi burns two 500 ms DA1-terminated probe rounds and its 400 ms
watchdog prints the red *"Terminal response timeout"* flash users see. roost
itself pays the same 2 s as a client at startup (first frame 2014 ms vs 23 ms
under an answering terminal), and roost-inside-roost hard-hangs. PTYs are also
opened with `pixel_width/height: 0` (degrades image-capable apps silently).
**Contract:** the full responder design — queries, byte-precise replies,
stream-order requirement, what to deliberately NOT answer, pixel-geometry
plumbing, and the client-side startup fix — is in **Appendix B**. This is
implementation-ready.

### P5 · FIXED (this branch) · High (destructive) — no reflow; a zoom round-trip truncates the grid
*Fixed by option (a), scoped to the live grid: `Grid::set_size_reflowing`
rebuilds the logical lines from the rows' wrap flags and lays them out again
at the new width — narrowing wraps, widening rejoins, cells copied whole so
colors/bold survive, a 2-column glyph that doesn't fit moving down whole, and
the cursor mapped to the same logical character (clamped into the grid if the
rewrap pushed it out). Rows a narrowing pushes off the top bank into the
scrollback like ordinary scrolling, but only after the screen's own blank
rows below the cursor are spent — so the common case banks nothing and the
round-trip is exactly lossless. **Scrollback rows keep their historical
width** (no history rewrap): the same veneer-vs-second-state split P1's
snapshot draws, and the documented price of not opening the full-history
problem. The **alternate grid never reflows** (nor does a grid with a scroll
region set) — those apps repaint on SIGWINCH. `PtyPane::resize` re-reads the
grid's scroll clamp so U3's `↑N` stays truthful. U19's `row_wrapped` now
survives resizes too. DESIGN-ui C21 amended 2026-07-27. e2e
`tests/pane_reflow.rs`: before, `roost read --tail` after unzoom showed
`Q7HEAD` + 52 columns and no `Q7TAIL` at all; after, the line wraps intact and
re-zooming rejoins it onto one row.*
`Grid::set_size` hard-truncates rows in place and clears wrap flags; zoom
deliberately resizes the pane's PTY to full-body and back.
**Measured:** a 110-char line printed *while zoomed* (118 cols) lost its tail
(`Q7TAIL`, cols 95–100) permanently on unzoom to 58 cols — still gone after
re-zooming; pre-zoom wrapped lines never rejoin. Nuance: scrollback rows keep
their old width, so history becomes mixed-width after a round-trip.
**Contract:** a zoom round-trip must be lossless. Either (a) implement reflow
(rewrap on width change, live grid + defined scrollback semantics), or (b)
stop resizing the zoomed pane's PTY (letterbox at tiled size). Pick
deliberately; (b) is honest and cheap, (a) is what users of alacritty/wezterm
expect. Interim: document the loss. Also fixes SPEC-ux U19's dependency on
`row_wrapped` surviving resizes.

## P1 — status, identity, input correctness

### P6 · FIXED (this branch) · Med-High — pane OSC 0/2 titles invisible
*Fixed: `App::display_name`'s chain is now explicit Alt+r title → the pane's
live `screen().title()` (agent panes only) → `adapter · cwd-tag`
(`display_name_live`), so the badge, collapsed rows, feed lines and
notifications all adopt a pane's published title; it is sanitized and bounded
to 48 chars before the badge's own width clipping. A plain `shell` pane skips
the live rung: a shell's title is `PS1` chrome (`user@host: /path`) that
restates the cwd tag and crowds the badge, whereas P6's value is agent CLIs
publishing task status. A hand-launched agent loses nothing — `observe_panes`
promotes a shell pane's adapter to `pi`/`claude` when it sees the agent
running, and demotes when it exits. roost also publishes `OSC 2 ; roost · {id} {focused pane}` to
the host terminal on focus/title changes (throttled to 200 ms) and resets it
to a plain `roost` on exit and in the panic hook. DESIGN-ui C4 and SPEC-ux U2
amended 2026-07-27. e2e `tests/pane_titles.rs`: a pane's `OSC 2 ; TASK-X`
never reached its badge before; after, the badge reads `1 TASK-X` and the
host stream carries `ESC ]2;roost · TASK-X BEL` (plus the exit reset).*
Claude Code continuously publishes `spinner + task` via OSC 0/2 (claude-code
#17887/#52258) — the cheapest live fleet-status text there is. vt100 already
parses and stores it; **zero** call sites read `screen().title()`; nothing
re-emits it to the host (outer tab title goes stale the moment roost starts).
**Contract:** untitled panes' `display_name` (SPEC-ux U2) prefers the live
OSC title over `adapter · cwd-tag`; an explicit Alt+r title still wins; roost
sets the *host* terminal title to `roost · <active pane's display_name>`.

### P7 · FIXED (this branch) · Med — cursor fidelity: hidden/shape/scroll all wrong
*Fixed, all three facets: `should_place_cursor` (pure, in `render.rs`) gates
host-cursor placement on focused ∧ alive ∧ `!hide_cursor()` ∧ `scroll_offset
== 0`, so (a) a TUI that hid its cursor no longer gets a ghost one and (c) a
scrolled-back view no longer floats a cursor over history (the same
frozen-view surface as SPEC-ux U3/N1 — while `↑N` shows, roost stops
asserting liveness, and a blinking cursor is the loudest such assertion). (b)
The vendored parser gained the SP-intermediate arm, so DECSCUSR
(`CSI Ps SP q`) becomes `Effect::CursorShape`; each pane remembers what it
asked for, and the main loop mirrors the FOCUSED pane's shape to the host via
crossterm `SetCursorStyle` on change only. Focusing a pane that asked for
nothing restores the default with no special case (it simply reports `None`);
exit and the panic hook restore it too. DESIGN-ui C4/U3 amended 2026-07-27.
e2e `tests/pane_cursor.rs`: before, roost kept emitting `?25h` + placement
while the pane held `?25l`, and `ESC [5 SP q` never reached the host; after,
the hidden window contains no placement, `?25h` restores it, the shape is
mirrored, and a focus switch emits `ESC [0 SP q`.*
Three facets, all verified: (a) the renderer places the host cursor whenever
the pane is focused, never consulting `screen.hide_cursor()` — a ghost cursor
blinks over TUIs that hid theirs; (b) DECSCUSR (`CSI 5 q` etc.) dies in the
vendored parser's unhandled arm and is never mirrored to the host — insert-bar
cursors render as blocks; (c) the cursor position comes from the live grid
even while the view is scrolled back — it floats over history.
**Contract:** honor `hide_cursor()`; park the cursor while scrollback offset
> 0 (same surface as U3/N1); parse DECSCUSR per pane and mirror the focused
pane's shape to the host (restoring roost's own on focus change/exit).

### P8 · FIXED (this branch) · Med — kitty keyboard protocol: roost affirms flags it doesn't implement
*Fixed, both halves — the promise and the delivery.*
***The promise:** a push is now masked to `queries::SUPPORTED` **before it is
stored**, so the flag stack itself can only ever describe behaviour roost
delivers. A push of 7 reports `?1u`; a push of only unimplemented bits
reports `?0u` and leaves `disambiguate()` false, so the app keeps the legacy
encodings roost does send correctly instead of waiting for CSI-u that never
comes. Masking on the way *in* rather than on the reply is what keeps the
three readers of that state — the ACK, `disambiguate()`, and
`input::kitty_upgrade` — from ever disagreeing.*
***The delivery:** the bit roost does claim is now true. `kitty_upgrade`
grew from "modified Enter only" to the two increments this finding named:
**Esc → `CSI 27 u`** (the headline — a bare `0x1b` is indistinguishable from
the first byte of an escape sequence, which is the whole reason an app asks
for this mode) and **`Ctrl`/`Alt` + printable → `CSI <code> ; <mods> u`**,
where the legacy encodings collide (`Ctrl+I` ≡ Tab, `Ctrl+M` ≡ Enter,
`Ctrl+[` ≡ Esc) or don't exist at all. That last case closes P12's hole for
negotiated panes at no extra cost: `Ctrl+-` had no C0 identity and was
dropped rather than mis-sent; CSI-u says it exactly (`45;5u`). The key code
is the unshifted codepoint, with shift in the modifier, per the spec.*
*Deliberately still legacy, because roost does not claim the bits that would
cover them: unmodified printables, and modified navigation/function keys
whose legacy forms (`CSI 1;5C`) are already unambiguous — sending everything
as CSI-u is bit `0x8`. Release events (bit `0x2`) stay unclaimed: they are
filtered before `translate` sees them and would need host-side kitty flags,
which is an architecture decision rather than a patch.*
*The unit test that used to pin the flag-echoing reply as intended now pins
the mask instead. e2e `tests/pane_queries.rs`: a pane pushes 7 and queries
through a real PTY — the reply is `?1u`, never `?7u` — and then Ctrl+B
arrives as `98;5u` rather than `0x02`. Both halves were verified to fail
against the code they replace.*
After a pane pushes flags 7, roost's query reply echoes `?7u` — contractually
promising disambiguated Esc (`CSI 27u`), CSI-u modifier combos, and release
events — while only modified-Enter is actually CSI-u-encoded; Esc goes out as
bare `0x1b`; Release events are filtered before translate ever sees them.
Apps that trust the ACK (fish 4, helix, kakoune) misparse. Note: a unit test
currently pins the flag-echoing reply as intended, and release events require
host-side kitty flags — an architecture decision, not a patch.
**Contract:** reply with the flags roost *implements* (mask to the honest
subset), and grow the subset deliberately: next increments are CSI-u Esc
(`\x1b[27u`) and Ctrl/Alt letter combos under disambiguate.

### P9 · FIXED (this branch) · Med — wheel is dead over alternate-screen apps
*Fixed: `route_mouse` now takes a `PaneMouseState` (protocol + alternate
screen + DECCKM) instead of the protocol alone, and a wheel tick over a pane
that is on the alternate screen with `MouseProto::None` becomes
`ALT_SCROLL_KEYS` (3) Up/Down presses forwarded to the app — the DECSET 1007
convention, and the same three lines the wheel moves roost's own scrollback
by, so it feels identical either side of the alternate screen. The bytes come
from the keyboard path's own encoders (`encode_raw` + `app_cursor_upgrade`),
so a pager driven through `smkx` gets the SS3 forms it is listening for. The
other two branches are untouched: a primary-screen pane still scrolls roost's
scrollback, and an app that asked for mouse reporting still gets its SGR
event verbatim (asking for the wheel means wanting the wheel). New
`PaneBackend::alternate_screen` reads vt100's existing 1049/47 tracking off
the LIVE screen, never the P1 presentation veneer. DESIGN-ui §5 amended
2026-07-27. e2e `tests/pane_wheel_alt_screen.rs`: before, fifteen wheel-down
events over `less` on a 400-line file left it on `L001`; after, the file
scrolls.*
`man`/`less` without mouse mode: wheel events route to roost-side scrollback,
but the alternate grid has scrollback capacity 0 — the view never moves and
the app receives nothing (measured byte-level: zero bytes from six wheel
events; `less` screen byte-identical). tmux #1333 → #4952 is this exact saga.
**Contract:** when `alternate_screen() && MouseProto::None`, translate wheel
to 3× arrow keys per tick (the DECSET 1007 convention). All needed state is
already exposed on the backend.

### P10 · FIXED (this branch) · Med — focus reporting (?1004) absent at all three layers
*Fixed at all three: the vendored parser tracks mode 1004 (`Screen::focus_events`,
DECRQM now answers it honestly), `main` enables/disables host focus events
alongside mouse+paste+kitty (panic hook included), and `App::set_focus` — now
the single writer of `focused` — sends `CSI O` to the pane leaving and `CSI I`
to the pane arriving, to subscribers only, via `write_input_raw` so a scrolled
view survives. A blurred window withholds pane-level reports; the focused pane
collects its `CSI I` when the window returns. e2e `tests/pane_focus.rs`: a
`?1004h` + `cat -v` pane saw neither `^[[O` nor `^[[I` across a focus
round-trip before, both after.*
roost never enables host focus events, has no FocusGained/Lost arms, doesn't
parse `?1004h` from panes, and synthesizes nothing on pane switches —
verified end to end (a `?1004h` pane saw no `CSI I`/`CSI O` across a focus
round-trip). Supersedes the SPEC-ux "deliberately-scoped" note with evidence.
**Contract:** track `?1004` per pane; deliver `CSI I`/`CSI O` on roost pane
focus changes to panes that asked; enable host focus events and forward them
to the focused pane; vim autoread / TUI dim-on-blur start working.

### P11 · FIXED (this branch) · Med — host identity env leaks into panes
*Fixed: `PtyPane::spawn` scrubs the 13 known identity vars and sets
`TERM_PROGRAM=roost` + `TERM_PROGRAM_VERSION`; verified end to end by
`tests/pane_env.rs` (a pane dumps its env under a fully-leaky host).*
`TERM_PROGRAM=iTerm.app`, `KITTY_WINDOW_ID`, `ITERM_SESSION_ID`, `TMUX` all
arrive verbatim inside panes (only `TERM` is overridden). Apps then negotiate
proprietary protocols (iTerm2 images, kitty graphics, tmux DCS passthrough)
that roost swallows — enlarging P1/P2/P3/P8's blast radius.
**Contract:** scrub the known identity vars at spawn; set
`TERM_PROGRAM=roost` (+ version var) so apps can adapt deliberately.
Sequence this first — it's small and de-risks everything in W2/W3.

### P12 · FIXED (this branch) · Med — Ctrl+`-` forwards a bare Enter (accidental submit)
*Fixed: `encode_key`'s Ctrl arm now runs through a C0 collision gate
(`ctrl_byte`) — letters/`@ [ \ ] ^ _`/space keep the mask, `?` → DEL,
`-`/`/` → 0x1F, everything else forwards nothing.*
`encode_key`'s blanket `& 0x1f` maps Ctrl+`-` → `0x0D` (CR — submits the
half-written prompt in any agent CLI) and Ctrl+`/` → `0x0F` (readline
operate-and-get-next). The same mask is *coincidentally correct* for
`[ ] \ ^ _` — the fix is a collision gate (letters + those five), not removal.
**Contract:** Ctrl+`-` and Ctrl+`/` encode `0x1F` (xterm/kitty legacy);
Ctrl+digits and other non-C0-mappable punctuation forward nothing rather than
a wrong control byte.

### P13 · FIXED (this branch) · Med (raised) — Ctrl+Alt+key triggers Alt actions
*Fixed with SPEC-ux U5 (one change): the chord table now requires CONTROL
absent — Ctrl+Alt forwards as meta-ESC + ctrl byte, and unmatched plain-Alt
printables forward as meta-ESC, both via `encode_raw`.*
The chord table matches on ALT alone: `C-M-f` toggles the float, **`C-M-w`
closes a pane** — destructive collision with emacs/readline muscle memory.
**Contract:** chords match only when CONTROL is absent; Ctrl+Alt+printables
forward as ESC+ctrl-byte. This is the same change as SPEC-ux U5's meta-ESC
fallthrough (`encode_raw` already computes exactly these bytes) — implement
them together.

### P14 · FIXED (this branch) · Med-Low — scrolled view jumps on interaction (mechanism corrected)
*Closed in two passes, and the second one is structural rather than
behavioural — stated plainly so nobody reads a refactor as a bug fix.*
***Pass 1 (SPEC-ux U9, already shipped):** every scroll step was re-pointed at
the grid — `PtyPane::scroll_by`, Scroll mode's keys, copy-mode paging and the
search's `scroll_view_to` all read `screen().scrollback()` immediately before
acting and let the grid clamp the result. That is what actually stopped the
4-line teleport this finding measured.*
***Pass 2 (this branch):** the two caches themselves are **deleted**, so the
defect stops being "currently handled" and becomes unrepresentable.
`PtyPane::scroll` is gone — its last reader was `write_input`'s snap-to-tail
guard, which now asks the grid — and `Mode::Scroll` no longer carries an
`offset` at all. Nothing observable changes: every path that could have read a
stale copy had already been re-seeded in pass 1, and the one fallback that
still consulted the cache (`unwrap_or(*offset)`, reached only when the pane has
no runtime) now does nothing instead of counting into a view that does not
exist. What changes is that a future scroll step **cannot** be written against
a cached offset, because there is no longer one to cache into.*
*The invariant is pinned by
`a_scroll_step_reads_the_grid_after_it_auto_advanced_under_new_output`: the
backend's offset is advanced under the mode (modelling the grid carrying the
view along as rows bank), and the next `↑` must land one row past **where the
view is**, not one past where the mode opened. Being a regression guard for an
invariant that already held, it passes before and after — it exists so the
next person cannot quietly reintroduce the mirror.*

### P14 (original finding) — mechanism, for the record
The research claimed passive drift; **measurement disproved it** — the
vendored grid auto-compensates the offset as lines scroll out (view stayed
anchored through new output). The real defect: roost keeps two shadow copies
of the offset (`PtyPane::scroll`, `Mode::Scroll { offset }`) that are never
reconciled with the grid's compensated value — the *next* wheel tick or
scroll-mode key teleports the view toward the tail (measured: one Up moved
the view 4 lines tailward). This amends SPEC-ux U9: reading back the clamped
value once is insufficient — **every scroll step must seed from the grid's
current `scrollback()` value**, not from a cached copy. (True drift remains
only at ring-buffer capacity — inherent, accept.)

## P2 — rendering & extraction fidelity

### P15 · FIXED (this branch) · Med-Low — yanked CJK/emoji text corrupted
*Fixed: `extract_selection` skips wide-continuation cells instead of spacing
them, and a selection that *starts* on a glyph's right half snaps back to its
left half so the glyph the user pointed at is yanked whole rather than lost
with the skipped cell. Measured before/after on the same grid: `日本語` yanked
`"日 本 語"`, now `"日本語"`; `ok ❤️ 😀 done` yanked `"ok ❤️  😀  done"`, now
verbatim. A unit pins the stated contract directly — a full-screen
`extract_selection` and `grab_all_text` now return identical lines.*
`extract_selection` pushes a space for every empty cell — wide-char
continuation cells included: selecting `日本語` yanks `"日 本 語"`. Pasting
yanked paths/code back into an agent breaks them.
**Contract:** skip wide-continuation cells during extraction (the
`grab_all_text` path already gets this right — the two must agree).

### P16 · FIXED (this branch) · Low-Med — SGR 2 (dim) and 9 (strikethrough) dropped
*Fixed: the vendored `Attrs` carries both, the SGR arms handle 2/9 and their
resets, and `cell_style` maps them to `Modifier::DIM`/`CROSSED_OUT`. One
wrinkle worth the note: ECMA-48 has no "faint off" — SGR 22 is *normal
intensity* and ends bold and dim together, so 22 clears both and the
escape-code diff writes the intensity pair as a unit (clear once, re-assert
whichever half survives) instead of as two independent toggles; without that,
dropping bold silently took a still-set dim with it (unit-proven via a
`contents_formatted` round trip). Dimmed and struck cells are now distinct
from unstyled ones, which they demonstrably were not. Amends DESIGN-ui C18
(2026-07-27).*
Verified attribute-identical to unstyled text end to end (vt100 `Attrs` has
no field; renderer maps only bold/italic/underline/inverse). Claude Code
leans on dim for secondary text — panes flatten into equal-weight walls.
**Contract:** add dim/strikethrough to the vendored `Attrs` + SGR arms and map
to ratatui `Modifier::DIM`/`CROSSED_OUT`.

### P17 · FIXED (this branch) · Low-Med — unicode-width table skew (0.1.14 vs 0.2.0)
*Fixed: the vendored parser is pinned to the workspace's unicode-width 0.2, and
cells are now measured as *strings* (`Cell::contents_width`) rather than by the
base char alone — the only way an emoji-presentation sequence (base + VS16)
scores the 2 columns the renderer already gives it. `Screen::widen_cell`
promotes such a cell once the VS16 lands, claiming its continuation (and
refusing at a row's end, where there is nothing to claim, rather than leaving a
wide flag with no continuation). Measured after: `"❤️"` is 2 cols to both, and
the next glyph lands at col 2, not col 1.*
The vendored vt100 measures with unicode-width 0.1.14 while roost/ratatui use
0.2.0 — VS16 emoji widths changed between them. Measured: `"❤️"` is 2 cols
to the renderer, 1 col to the grid (following char landed at col 1) — grid
bookkeeping, blit, and hitboxes disagree per glyph.
**Contract:** bump the vendored parser to the workspace's unicode-width in
the same change as P15/U24 so grid and renderer can never disagree again.

### P18 · FIXED (this branch) · Low-Med — shell panes are non-login shells
*Fixed: `ShellAdapter::launch` adds `-l`. **Why `-l` and not the dash-argv0
tmux and the emulators use:** roost spawns through portable-pty, whose
`CommandBuilder` derives both the executable to resolve and argv[0] from the
same `args[0]`, so `-zsh` would be looked up on `PATH` and fail — there is no
seam for an argv0 that differs from the program. The two spellings mean the
same thing to every shell involved. Guarded by a basename allowlist (sh, bash,
zsh, fish, dash, ash, ksh/mksh/pdksh, csh/tcsh); an unrecognized `$SHELL`
spawns bare, because a shell rejecting the flag would kill every pane at birth
— a much worse failure than the missing profile. Agent adapters unchanged.
e2e `tests/pane_env.rs`: the pane gets a private `$HOME` whose `.profile`
exports a marker; before, the pane's env had no marker, now it does.*
`$SHELL` is spawned bare (no `-l`, no dash-argv0). On macOS, `~/.zprofile`
(Homebrew PATH — where `claude`/`pi` often live) never runs; the classic
"works in a terminal tab, `command not found` in the mux" (tmux #1623).
**Contract:** spawn shell-adapter panes as login shells (dash-prefixed argv0
or `-l`), matching every terminal emulator's default.

### P19 · FIXED (this branch) · Low — REP (`CSI Ps b`) unimplemented while TERM advertises it
*Fixed: the `b` arm replays the last graphic character through `text`, so the
repeat inherits the live attrs and obeys wrapping, scroll regions and wide-char
placement instead of duplicating those rules. `"ab" + CSI 5 b` now renders
`"abbbbbb"`. No-op before anything is printed, `0` means 1 per ECMA-48, and the
count is bounded to `u16` by vte's parameter type. Known limitation, shared
with xterm: the unit repeated is the last base char, not the last cell, so a
combining or VS16 sequence repeats only its base.*
`TERM=xterm-256color` promises `rep`; ncurses 6 uses it; roost renders
`"ab" + CSI 5 b` as `"ab"` (expected `"abbbbbb"`) — dropped glyph runs in
htop-class TUIs. **Contract:** implement the `b` arm (repeat last graphic
char), part of the W6 vendored-parser batch.

### P20 · FIXED (this branch) · Low — mouse gestures don't latch to a pane
Every mouse event is re-hit-tested, so a drag that crosses a border switches
target mid-gesture (origin app never sees Up; neighbor gets orphan events);
SGR coords also aren't clamped to the pane's right/bottom edge. Copy mode
already latches — the pattern exists.
**Contract:** latch button-down's pane until release (copy-mode style);
clamp forwarded coords to the pane's inner grid.
**Fixed:** `App::mouse_latch` holds the pane a button-down landed in;
`handle_mouse` routes every subsequent Drag/Up to that pane wherever the
pointer has wandered, and clears the latch on release. A Drag/Up with no
latch behind it (the press landed on the tab bar, inside a modal, or off the
body) is delivered to nobody rather than hit-tested into whatever sits under
the pointer — that orphan delivery was the other half of the bug. Wheel and
bare motion still hit-test, since neither is part of a button gesture; copy
mode's own latch (`selection.pane`) is unchanged, this is the same rule for
the forwarding path. `mouse::cell_in_pane` now clamps right and bottom as
well as left and top, so a latched drag past its pane's edge reports the
pane's last cell instead of a coordinate outside the grid the inner app
believes it has.

### P21 · FIXED (search half, this branch) · Low — no scrollback search, no dump-to-editor
*Fixed: `/` in Scroll **and** Copy mode opens an incremental search over the
focused pane's `grab_all_text` (history + screen, captured once at open so
every keystroke filters a frozen haystack); typing narrows, Enter keeps the
result, Esc restores the pre-search view, `n`/`N` walk the hits with
wrap-around at both ends. Every jump writes through `set_scrollback` and
re-reads the grid's clamp (the U9-vetted path), so `↑N` and `↑N/M` stay
truthful even when the arithmetic asks for a row the ring has evicted. Hits
are `REVERSED` in-pane with the current one additionally `UNDERLINED` (C17's
modifier-only rule); the prompt `/query▏` + `i/n` counter + `SEARCH` rides
inside C9's right segment, so the pane stays fully visible while the query
narrows (C9/C15/§8 amended 2026-07-27). e2e `tests/scrollback_search.rs`: a
pane prints 300 numbered lines, `/mark-42` jumps the view to a line ~258 rows
back — before, `/` was an unbound no-op and no prompt existed.*
***Deferred:** dump-to-editor. It is a separate verb with its own questions
(which editor, which file, what happens to a 5000-line dump), and
`roost read --full` already answers the "get it out of roost" need it was
sketched against.*
Every peer ships search; roost's only history export is `roost read --full`.
**Contract (sketch):** `/` in Scroll/Copy mode = incremental search over
`grab_all_text`, jumps setting the (P14-vetted) offset; a dump-to-editor verb
can start as a thin affordance over the existing control-plane read.

## Not applicable — peer failure classes roost's architecture rules out
Daemon/attach crashes (no daemon by design) · session-resurrection duplicates
(single-instance lock + claimed-session set) · scrollback-serialization
corruption (scrollback never persisted) · post-crash wrecked terminals (panic
hook restores) · config-file breakage (zero-config) · output-flood starvation
(bounded channel + per-tick cap, firehose-gated) · heuristic-status wrongness
(documented tradeoff; sharpest edge tracked as U22).

---

## Appendix A — verification evidence (abridged)
Full receipts live in the verification run's output; per-item highlights:
P1 31/50 torn server-side samples · P2 status `waiting` after OSC 9 vs
`needs_input` after control BEL · P3/P6 target bytes absent from whole-session
host capture · P5 `Q7TAIL` destroyed across zoom round-trip · P7 host cursor
visible during pane `?25l` · P8 `\x1b[?7u` echo + bare-`0x1b` Esc ·
P9 zero bytes from six wheel events · P10 no `CSI I/O` across focus
round-trip · P11 four identity vars leaked verbatim · P12 `Ctrl+'-' →
[0x0D]` · P13 `Ctrl+Alt+w → Action(ClosePane)` · P14 anchored view, then
4-line jump on first keypress · P15 `"日 本 語"` · P16 attrs identical to
unstyled · P17 `X` at col 1 after a 2-col heart · P18 non-login probe ·
P19 `"ab"` unchanged · P4 measured in the startup-stall diagnosis (atuin
abort at 2045 ms; yazi watchdog message verbatim; crossterm hang > 10 s;
roost first frame 2014 ms vs 23 ms).

## Appendix B — W2 query responder: implementation-ready design (from the P4 deep dive)
Generalize `src/infra/kitty.rs` into a pane-side query responder fed from
`PtyPane::process_output`, with replies written via `write_input_raw`.
Load-bearing details: run `parser.process` *before* the responder so DSR/
DECRQM answers reflect post-chunk state, and emit replies in stream-encounter
order (crossterm's `?u`+DA1 burst requires kitty-reply-then-DA1 or it
misparses). Answers (modeled on tmux 3.5a `input.c`):
DA1 `CSI c` → `\x1b[?1;2c` (the single highest-value reply: unstalls
crossterm, both yazi rounds, atuin) · DA2 → `\x1b[>84;0;0c` or a roost
identity · DSR 5 → `\x1b[0n` · DSR 6 → `\x1b[{row+1};{col+1}R` from
`screen.cursor_position()` · DECRQM `?Pd$p` → honest values from tracked
state (2004/25/1 known; report 0 for untracked modes; claim 2026 only once
W3 ships it) · XTVERSION → `\x1bP>|roost {version}\x1b\\` · XTWINOPS
14/16/18t from plumbed pixel geometry (suppress 14/16 when unknown).
Deliberately silent: kitty-graphics probe (roost can't composite — with DA1
answered, yazi picks its fallback instantly and honestly), XTGETTCAP,
DECRQSS, and OSC 10/11 unless roost learns real host colors. Pixel plumbing:
read host winsize at startup/resize, derive per-pane pixels proportionally,
pass through `PaneBackend::spawn/resize`. Separately: roost's own startup
must not block on `supports_keyboard_enhancement()` under a non-answering
terminal (run concurrently with init); a first-frame-latency regression guard
belongs in the harness once fixed.

## Cross-references
U2 ← P6 (display_name prefers OSC title) · U3/N1 ← P7c (park cursor while
scrolled) · U5 = P13 (one change) · U9 ← P14 (amended contract: seed every
step from the grid) · U14 ↔ P3 (both halves of clipboard honesty) · U19 ← P5
(wrap flags survive only with reflow) · U24 = P15+P16+P17+P19 bundle (W6).
