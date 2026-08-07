# Current selection / copy / mouse behavior (scout, 2026-08-06)

Ground truth for the Phase 2N client requirement. Every claim file:line.

## 1. Mouse capture

`EnableMouseCapture` at startup (main.rs:64), `DisableMouseCapture` on exit
(main.rs:114), `EnableBracketedPaste` (main.rs:71). **Capture is binary
on/off** — no protocol negotiation (no explicit 1000/1002/1003/1006), SGR
encoding only (mouse.rs:447–466 `encode_sgr`). No conditional release, no
per-mode bypass. Raw pass-through (C23, Alt+Shift+p) forwards keys but
**leaves mouse roost-controlled**.

## 2. Drag selection

**Only inside copy mode (Alt+c).** Normal-mode click focuses the pane;
wheel scrolls or forwards; no selection state is maintained outside the mode.
`Selection {pane, anchor, cursor, dragging}` (app.rs:180);
`begin/extend_selection` (app.rs:2130–2145); `finish_selection` extracts and
**exits the mode** (app.rs:2179–2195); painted reversed by
`highlight_selection` (render.rs:1786–1792); text grabbed by
`extract_selection` (pty.rs:767–807, reading order, trailing spaces trimmed).

## 3. Double / triple click

**Not implemented.** No click-count tracking anywhere. Single click focuses.

## 4. Copy paths

`clipboard::copy()` (clipboard.rs:16–24) tries a native helper first, then
OSC 52, returning `ClipboardOutcome` Native/Osc52/Failed. Native chain
(clipboard.rs:34–56): pbcopy → wl-copy → xclip → xsel. OSC 52
(clipboard.rs:59–69) writes `ESC]52;c;<b64>BEL` to stdout. Callers: mouse
copy `handle_copy_mouse` (main.rs:648–676) and keyboard `y`/`Enter`
(main.rs:305–312). Both reach the **system** clipboard. U14 reports which
channel won so the hint can distinguish native vs OSC 52 (matters over SSH).

## 5. Copy mode (C24)

Enter Alt+c; exit Esc/q (app.rs:4404). Keys: hjkl/arrows, `v` anchor, `y`/
`Enter` yank, `0`/`$`, `w`/`b`/`e`, `V` line, `o` open URL (C24b). Cursor
starts bottom-left of the focused pane's inner grid (app.rs:3878–3903),
always REVERSED, UNDERLINED inside the selection (render.rs:1858–1867).

## 6. Paste

Bracketed paste on at startup (main.rs:65–71); embedded guard sequences
stripped before forwarding (app.rs:2003–2015) — paste-injection defense;
per-pane mode-2004 check (ports.rs:148–154); `forward_paste` re-wraps
(app.rs:2039–2046). Rename mode takes pastes into its buffer (main.rs:329).

## 7. Mouse forwarding to the child

`route_mouse` (main.rs:627–643) checks `rt.mouse_proto()` (ports.rs:204,
read from vt100 state, default None). When the app enabled SGR reporting,
events are encoded (mouse.rs:447–466) and forwarded raw (app.rs:2408–2415,
`write_input_raw` to avoid scrollback snap).

## 8. Wheel

No mouse mode + normal screen → roost scrollback (main.rs:641). No mouse
mode + alternate screen → arrow keys, 3 lines (`ALT_SCROLL_KEYS`,
mouse.rs:26–31; SPEC-parity P9). App with mouse reporting → forwarded SGR.
Fixed 3 lines/tick, no acceleration.

## 9. Contracts

- **C23** (DESIGN-ui.md:1676–1743): Alt+Shift+p raw toggle; "Mouse unaffected."
- **C24** (:1745–1825): copy mode keys + cursor rendering; "Selection
  semantics identical to mouse path."
- **P9** (SPEC-parity:296–317): wheel→arrows on alternate screen, measured
  zero bytes to the app from six wheel events.
- **P5** (:169): zoom lossless, selection survives.

## 10. Modifiers

Alt+left-click opens the URL under the cursor (main.rs:610–620). SGR encodes
Shift +4 / Alt +8 / Ctrl +16 (mouse.rs:433–445). **No bypass modifier is
checked to restore the emulator's own selection** — deliberate, per
README.md:138 "your terminal's Shift+drag still works", i.e. relying on the
emulator to *not report* Shift+drag to roost at all.

## Gaps the scout could not resolve

Whether roost ever writes mouse-mode sequences to the PTY itself (may rely
entirely on crossterm); no wheel-speed tuning; no validation that a given
terminal reports the modifiers roost encodes; no test that OSC 52 is honored
by the host or survives a tmux relay.

---

## Principal's reading — the native-parity gap

| Native macOS gesture | Plain terminal | roost today |
|---|---|---|
| Drag to select | works | **needs Alt+c first** |
| Double-click word | works | **missing entirely** |
| Triple-click line | works | **missing entirely** |
| Shift-click extend | works | **missing** |
| Cmd+C | copies emulator selection | **emulator selection is suppressed by capture → copies nothing useful** |
| Cmd+V | works | works |
| Shift+drag | n/a | native selection (undocumented escape hatch) |
| Wheel scrollback | works | works |
| Cmd+F find | emulator scrollback | alt-screen empties it; roost search is a chord |

Four of nine gestures force a roost-specific chord or simply don't exist.
That is the requirement's definition of a defect.
