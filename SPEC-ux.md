# SPEC-ux — roost UX gaps: an open spec

**Status: OPEN.** A living spec of user-facing gaps and the contracts proposed to
close them. Nothing here is implemented yet; each item carries a verification
status so implementation PRs can cite `U<n>` and flip the item to FIXED.

**Origin.** Two independent expert reviews (a Warp-trained lens: discoverability,
affordances, mouse parity, recovery flows; a lazygit-trained lens: keyboard
economy, modality, binding consistency, help completeness) over the code,
DESIGN.md §7–8, DESIGN-ui.md, and rendered frames — followed by a scripted live
QA session against the real binary in a real PTY (`tests/ux_nav_qa.rs`, an
`#[ignore]`d evidence drive: multiple panes, stacks, layout cycling, zoom, tabs,
every mode, mouse events). Appendix A records what the session confirmed.

**Item status legend.**
- `VERIFIED` — reproduced in the live QA session (or directly visible in its frames).
- `CODE-VERIFIED` — pinned to the exact code path; live probe not applicable or inconclusive.
- `NEW` — found during the live session, not predicted by either review.
- `OPEN` — proposed change; no dispute about current behavior.

Severity is impact for roost's target user: someone running a fleet of AI agents.

---

## P0 — both review lenses agreed independently

### U1 · High · FIXED (this branch) — Alt+q kills the fleet unguarded
Closing one busy pane double-confirms (`app.rs` `confirm_close`), but Alt+q
quits instantly — live QA quit in ~318 ms while a pane was mid-firehose. ESC+`q`
fusion can synthesize the chord (documented in `tests/live_qa.rs`).
**Proposed contract:** when any pane is `Working` (or `NeedsInput`, see U12),
arm the existing second-press confirm: flash `N agents working — Alt+q again to
quit`. Instant quit stays when the fleet is quiet.
**Fixed:** `App::quit_guarded` arms a separate `confirm_quit` with the same
machinery as busy-close whenever any pane is busy (U12's `Working ||
NeedsInput` predicate, shared `is_busy`), flashing `N agent(s) busy — Alt+q
again to quit` ("busy", matching the Alt+w flash family, since the guard
covers ◆ too); a quiet fleet still quits on one press.

### U2 · High · FIXED (this branch) — panes have no identity on fleet surfaces
Live feed frame: `spawned shell (shell)` ×2 and `shell: working → your turn` ×4,
indistinguishable; notifications say "shell is waiting for you"; the pane id —
the join key for `roost send <id>` — appears nowhere in the TUI. The corner
badge already disambiguates with a cwd tag (`render.rs` badge fallback); feed,
notifications, and flashes don't use it.
**Proposed contract:** one `display_name(id)` helper (title, else
`adapter · cwd-tag`), used by feed lines, notifications, and flashes; pane id
shown in the badge, collapsed rows, and feed entries.
**Fixed:** the badge's fallback moved to a shared `core::app::display_name_of`
(+ `App::display_name`/`feed_label`); feed lines lead with `{id} {name}`,
notifications and pane-referencing flashes carry the display name, and the
corner badge and collapsed rows lead with the pane id (C4/C8/C20 amended
2026-07-27).

### U3 · High · FIXED (this branch) — a scrolled pane looks live (and still pulses ●)
Live frame E1: a wheel-scrolled pane frozen at old output shows no indicator
anywhere — and its badge keeps the pulsing `●` working glyph, actively asserting
liveness while the view is stale. Wheel scrolling never enters Scroll mode, so
the `SCROLL` mode word never covers it; Scroll mode itself shows no position.
**Proposed contract:** whenever a pane's scrollback offset > 0, its badge gains
a dim `↑N` token (same family as the `raw` token); Scroll mode's hint shows
`line N/M`. Any path that resets the offset removes the token.
**Fixed:** the badge carries `↑N` (ACCENT_DIM, glyph-adjacent) whenever the
grid-clamped offset is > 0, and Scroll mode's right segment shows `↑N/M` —
both read via the new `PaneBackend::scroll_offset`/`scroll_total` accessors,
so they reflect the view, never a phantom counter (C4/C9/§2 amended
2026-07-27; N1's pulse rule rides along).

### U4 · High · OPEN — the macOS Alt trap is only caught on Apple Terminal
`wants_alt_hint` fires solely on `TERM_PROGRAM == "Apple_Terminal"`; default
iTerm2 (the README's recommended terminal) swallows Option identically and gets
no warning — every advertised chord silently no-ops in the first minute.
**Proposed contract:** warn on any terminal when keys are arriving but zero Alt
has ever been seen (or on the Option-accent signature `ñƒå∂…`), with a
per-terminal instruction line.

### U5 · High · FIXED (this branch) — unbound Alt chords are swallowed, not forwarded
*Fixed with SPEC-parity P13 (one change): `translate()`'s unmatched-Alt arm
now forwards printables as meta-ESC via `encode_raw`; the QA drive's Alt+b
probe flipped from an observation to a passing check.*
Live QA: with `cat` running, Alt+b produced nothing in the pane (`^[b` absent);
in raw mode the same key forwards fine. `Alt+b/d` are documented as "left free
for readline" yet die at `translate()`'s `Ignore` arm — not chrome, not
passthrough.
**Contract (now implemented):** unmatched Alt+printables forward as meta-ESC
(reuse `encode_raw`); bound chords stay roost's; raw mode remains the escape
hatch for the chords roost does own.

### U6 · Med · VERIFIED — the discoverability hint is dropped first
The hint bar's yield order drops `Alt+? keys` exactly when the bar gets busy
(`◆ N needs you` appearing at 120 cols) — the moment a new user most needs it.
**Proposed contract:** `Alt+? keys` yields last among pairs (or `Alt+r rename`
drops first).

### U7 · Med · VERIFIED — tab reachability dead ends
Live QA: Alt+9 (no such tab) and Alt+0 (unbound) both no-op silently; no
next/prev-tab chord exists; overflow-clipped tabs aren't clickable. Tabs ≥ 10
are unreachable by keyboard entirely.
**Proposed contract:** next/prev-tab chords from the documented free-key pool;
`Alt+0` → tab 10 or last-tab; the strip scrolls to keep the active tab visible.

## P1 — high-value, single-lens

### U8 · Med · FIXED (this branch) — modals don't own non-keyboard input
Three live confirmations: (a) with Rename open, clicking another pane moved
focus and Enter committed the title to the clicked pane — frame E1 shows `ZZZ`
on the pane that was clicked, not the pane Alt+r opened on; (b) a bracketed
paste during Rename landed in the hidden pane underneath (`PSTX` typed into the
shell) while the dialog buffer ignored it; (c) wheel with the feed overlay open
scrolls the pane under the overlay (no feed gate in `handle_mouse`).
**Proposed contract:** while a modal mode is active — mouse: wheel scrolls the
overlay, clicks hit overlay rows or dismiss, never mutate focus/tabs beneath;
paste: routes to the text field if one exists, else is swallowed.
**Fixed:** `App::modal_active` gates both non-keyboard paths in the
composition root — `handle_mouse` routes to `App::handle_modal_mouse` (with
the dialog's drawn rect from the new `render::modal_rect`, so hit-tests match
the screen) and `Event::Paste` to `App::handle_paste`. Nothing beneath a
modal is mutated any more: the wheel pages the feed by its own PgUp/PgDn step
and is swallowed elsewhere, a click launches the picker row under it
(`mouse::picker_row_at`) or dismisses Picker/Help/Feed, and a paste fills the
Rename buffer (printables only) or is swallowed (C12/C14 amended 2026-07-27).
One deviation from the proposal: an outside click does **not** cancel Rename
— it is swallowed. Rename's buffer is unsaved work, so discarding it on a
stray click is (a)'s harm inverted; the drive's own click-during-rename check
wants the commit to land on the pane the dialog opened on, which a cancel
would silently drop.

### U9 · Med · FIXED (this branch) — scrollback is keyboard-hostile
Three connected live confirmations:
- **Overshoot:** Scroll-mode offset is unclamped while the view clamps — after
  paging past the top, ~240 Down presses were burned before the view moved.
- **Wheel/keys desync:** entering Scroll mode after wheeling starts `offset: 0`
  — the first Up snapped the view from the wheeled position back to the tail.
- **Scroll→Copy snap:** Alt+c from Scroll mode snapped to the live tail (the
  Alt-chord branch resets scrollback before the chord applies), so history
  can't be yanked by keyboard; wheel→Alt+c preserves the view (inconsistent).
**Proposed contract:** after every `set_scrollback`, read the grid-clamped
value back into both the runtime and the mode offset; Scroll-mode entry seeds
`offset` from the pane's current offset; Scroll→Copy preserves the view; copy
mode gains PgUp/PgDn view paging (and the wheel while selecting).
**Fixed:** every `set_scrollback`/`scroll_by` reads the grid clamp back (and
bases arithmetic on the view's current offset); Scroll-mode entry seeds from
the pane's offset and every keypress mirrors the clamp into the mode offset;
Alt+c is exempted from the Alt-chord snap so Scroll→Copy keeps the frozen
view; copy mode pages the view with PgUp/PgDn by pane height (C24 amended
2026-07-27). The wheel-while-selecting half of the proposal is not in this
wave (mouse-path routing in copy mode is untouched).

### U10 · Med · VERIFIED — dead-pane `f` destroys the session, bare
Live QA: `f` on a dead pane respawned fresh instantly — a single unshifted key,
no confirm, no undo, and the dropped resume id is the one thing roost exists to
protect.
**Proposed contract:** keep the old session id until the fresh session produces
a new one, or guard `f` with the same second-press confirm as busy-close.

### U11 · Med · CODE-VERIFIED (zoom kill VERIFIED) — same-tab digit resets state; no per-tab focus memory
Live QA confirmed the observable half: pressing Alt+1 while already on tab 1
exited zoom. The same unguarded `go_to_tab` path resets focus to the tab's
first pane and hides the float, and tab switches always land on the first pane
(`focused = pane_order().first()`) — the live focus probes happened to sit on
the first pane, so those halves are pinned by code, not frames.
**Proposed contract:** same-tab digit is a no-op; each tab remembers
`last_focused` (session-only) and returns to it.

### U12 · Med · FIXED (this branch) — `◆ NeedsInput` panes close without the busy guard
The Alt+w confirm checks only `Working`; an agent blocked on your approval is
mid-turn all the same.
**Proposed contract:** the guard predicate is `Working || NeedsInput`.
**Fixed:** `close_pane`'s guard now uses the shared `is_busy` predicate
(`Working || NeedsInput`), the same one Alt+q's U1 guard counts with.

### U13 · Med · OPEN — tab summary has no Exited state
A background tab full of dead agents shows a blank glyph (SPEC-GAP-2,
DESIGN-ui §7): one transient bell, then silence.
**Proposed contract:** `TabSummary::Exited` → `✕` (ACCENT_DIM), ranked between
Waiting and Quiet.

### U14 · Med · OPEN — "copied N chars" can be a lie
The flash fires on extraction; `clipboard::copy` discards both channels'
results (live_qa historically showed the flash with an empty clipboard).
**Proposed contract:** report the channel that actually ran — `copied N chars
(OSC 52)` when no native helper succeeded, an explicit failure otherwise.

### U15 · Med · VERIFIED — hiding hints hides the only mode indication
Live QA: with hints hidden (Alt+/), a zoomed pane shows `ZOOM` nowhere — it is
indistinguishable from a one-pane tab; SCROLL is likewise invisible.
**Proposed contract:** with hints hidden, non-Normal mode words (and ZOOM) move
to the tab-bar right status; or any non-Normal mode temporarily reshows the bar.

## P2 — polish

### U16 · Low · VERIFIED — rename input swallows edit chords, append-only
Live QA: typing `abc` + Ctrl+W + Ctrl+U produced the title `abcwu` — modifier
chords insert their letter. No cursor motion of any kind.
**Proposed contract:** discard modified chars except Ctrl+U (clear) and Ctrl+W
(word-erase); accept paste into the buffer (see U8); cursor motion later.

### U17 · Low · VERIFIED — copy-mode vocabulary
`V` (line select) and `w/b/e` are unbound no-ops (live-confirmed); `0`/`$`
exist but appear in no hint and not in the help overlay.
**Proposed contract:** add `V` and `w/b/e`; list `0/$` in the copy hint.

### U18 · Low · CODE-VERIFIED — mode entry chords don't uniformly toggle off
Alt+e closes the feed (special-cased); Alt+c in copy mode re-enters with a
fresh cursor; Alt+PageUp in scroll mode resets to tail instead of exiting.
**Proposed contract:** a mode's own entry chord exits it (generalize the Alt+e
pattern).

### U19 · Low · OPEN — URLs: wrapped links break Alt+click; no keyboard open
`url_at` reads one grid row; agents constantly print wrapping URLs into narrow
panes. No keyboard path opens a URL at all.
**Proposed contract:** join wrapped rows (vt100 `row_wrapped`) before
tokenizing; `o` in copy mode opens the URL under the cursor.

### U20 · Low · OPEN — picker is arrows/Enter only
No number accelerators, no type-ahead, no click; `j/k` work unhinted; the
DESIGN.md §7 "recent cwd" column never shipped.
**Proposed contract:** `1..9` accelerators + click-to-launch; cwd column later.
*(Click-to-launch landed with U8's modal mouse ownership — C14 amended
2026-07-27. The accelerators, type-ahead and cwd column stay OPEN.)*

### U21 · Low · OPEN — mouse can't resize; tabs are click-to-switch only
No border drag-resize, no middle-click close, no drag-reorder.
**Proposed contract:** drag a shared border to resize; middle-click closes a
tab through the same confirm guard.

### U22 · Low · OPEN (window mismatch FIXED, this branch) — busy-close confirm fires on heuristic Working; window mismatch
Any PTY output in the last ~2 s counts as Working, so closing a shell right
after your own `ls` double-prompts (the live QA script codes around this); the
confirm stays armed 3 s but its flash dies at 2 s — a final second accepts the
second press with no visible prompt.
**Proposed contract:** confirm only on extension-confirmed Working (or exempt
`shell`); flash window == confirm window for confirm flashes.
**Fixed (window half only):** flashes now carry their own window — confirm
prompts live exactly `CONFIRM_WINDOW`, and a confirm that dies early (consumed
second press, or disarmed by another action) takes its prompt down with it.
The heuristic-Working half stays OPEN.

### U23 · Low · VERIFIED — help overlay teaches chords, nothing else
Live QA: the overlay contains no status-glyph legend (`●◆○·✕` — the product's
core language) and no mouse documentation.
**Proposed contract:** one legend row and one mouse row; scroll the overlay on
short terminals instead of capping content.

### U24 · Low · OPEN — wide-char blit is approximate
`render.rs` self-documents approximate CJK/emoji handling (continuation cells
overwritten with spaces); agent TUIs are emoji-heavy.
**Proposed contract:** skip continuation cells after wide symbols; add a
CJK/emoji golden frame to the harness.

### U25 · Low · OPEN — the feed can't act
Feed entries aren't actionable (no jump-to-pane) and its hint omits its own
working keys (PgUp/PgDn/q).
**Proposed contract:** entries carry `PaneId`; Enter focuses that pane; hint
lists the real keys.

## New findings from the live session (not predicted by either review)

### N1 · Med · FIXED (this branch) — the frozen-scroll badge still pulses ●
Compounds U3: frame E1 shows the wheel-scrolled (frozen) pane's badge pulsing
`●` working — the UI actively asserts liveness for a stale view. The U3 `↑N`
token must also suppress or co-locate honestly with the status glyph.
**Fixed:** while `↑N` shows, the badge's Working glyph holds its steady base
color instead of pulsing — status stays truthful (the agent IS working), but
the "alive right now" animation never plays over a frozen view; resetting the
offset resumes it (C4 amendment 2026-07-27, `badge_glyph_color`).

### N2 · Low · NEW — Alt+o silently no-ops outside a split
With the focused pane in a stack, Alt+o changed nothing and said nothing (the
persisted layout was byte-identical). Every other no-op'ish state in roost gets
a flash.
**Proposed contract:** flash `flip needs a split` (or flip the nearest ancestor
split).

### N3 · Low · NEW — Alt+Shift+p's ESC+`P` form is ambiguous with DCS
On terminals without the kitty disambiguate negotiation, Alt+Shift+p arrives as
`ESC P` — the DCS introducer. In the harness the raw toggle worked in one run
and was consumed as DCS in another (1-of-2 flake). Real legacy terminals share
the ambiguity.
**Proposed contract:** document the limitation; prefer advertising the chord's
CSI-u form where negotiated; consider a fallback chord that isn't `ESC`+uppercase.

### N4 · Low · NEW — Alt+Left/Right silently no-op inside stacks
Focus between stack members moves with Alt+Up/Down only; horizontal focus keys
do nothing, silently — during the session this made a focus probe sit on one
pane through four keypresses. Harmless but disorienting mid-muscle-memory.
**Proposed contract:** in a stack, Alt+Left/Right move to the neighboring
layout node (leave the stack), or flash the stack hint.

## Appendix A — live QA session (method + confirmations)

Driver: `tests/ux_nav_qa.rs` (`#[ignore]`d evidence drive; run with
`cargo test --test ux_nav_qa -- --ignored --nocapture`). Real binary, real PTY
(120×40), control-plane ground truth via `roost list` against the instance's
state dir. Session shape: 1→4 panes; spatial focus walk incl. boundaries;
persisted-ratio resize; stack toggle + member cycling; 3× layout cycle; zoom
(incl. focus-move while zoomed); flip; second/third tabs; tab round-trips;
same-tab digit; rename (modifier chords, paste-during-modal, click-during-modal);
picker; 300-line scroll workload (overshoot, wheel/key handoff, scroll→copy);
copy selection + yank; bell → ◆ → Alt+a; feed overlay (+wheel-under); help;
hints-off + zoom; cooked vs raw Alt+b; pane death + `f`; close + undo; Alt+q
mid-firehose.

Confirmed live: U1 U2 U3 U5 U6* U7 U8(a,b,c) U9(all three) U10 U11(zoom half)
U15 U16 U17 U23, plus N1–N4. (*U6 confirmed in the earlier review's frame pair;
the QA session ran at 120 cols with the segment visible.)

Notable frames (abridged; full frames in the driver's output):

```
E1 — wheel-scrolled pane: frozen at line 242, no indicator, badge still ●:
│  1 main ○ │   2 tab2 ○ │ ▎ 3 tab3 ● │            /home/user/roost · saved ✓
│┌──────────────────────────────────────┐┌─────────────────────────────────┐
││242                       ZZZ · shell ● ││#                shell · roost ● │
││243                                     ││                                 │
   (…302 is the live tail; nothing marks this view as history)

F2 — activity feed: four identical transitions, two identical spawns:
││ 11:13:10  spawned shell (shell)
││ 11:13:10  shell: working → your turn
││ 11:13:10  shell: working → your turn
││ 11:13:10  shell: working → your turn
││ 11:13:10  shell: working → your turn
││ 11:13:10  spawned shell (shell)
```

The `ZZZ` title on E1's left pane is itself evidence: it was committed by a
Rename dialog opened on the *right* pane, redirected by a mid-dialog click (U8a).

## Appendix B — what must not regress (both reviews, live-confirmed)

The per-mode hint bar that never lies (incl. the single-pair raw bar); the
`◆ N needs you · Alt+a` segment sharing one predicate with the jump ring
(live: bell → ◆ → Alt+a landed on the right pane); dead panes as recovery
flows with working undo (live: Alt+w → Alt+u round-trip); the single
status-glyph table across every surface; byte-level input fidelity (DECCKM,
kitty CSI-u, xterm modifiers, bracketed-paste guards — all live-verified in
`tests/cursor_mode.rs` / `tests/paste_mode.rs`); and `roost list/status/send`
agreeing with the TUI (used as ground truth throughout the QA).
