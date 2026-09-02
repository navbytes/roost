//! Keybindings (design doc §7): roost owns the Alt layer, everything else is
//! forwarded raw to the focused pane so agents see a normal terminal.

use crate::core::layout::Dir;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    NewPane,
    ClosePane,
    /// Move focus spatially (arrows / hjkl).
    Focus(Dir),
    /// C36: open the broadcast composer — type one message, send it to
    /// every reachable pane (`Alt+'`). The verb roost is uniquely
    /// positioned for, and until now the only one you had to leave roost
    /// to use.
    ToggleBroadcast,
    /// C35: return focus to the pane it was on before this one — the
    /// alternate-pane toggle (`Alt+;`, tmux's `prefix ;`). Every other
    /// navigation chord is absolute or forward-directional; this is the
    /// only one that goes back.
    FocusAlternate,
    /// C33: swap the focused pane with its neighbour in that direction,
    /// within the active tab — `Alt+Shift+arrows`, the same verb
    /// `Alt+Shift+hjkl` has always spelled. The shifted siblings of the
    /// focus chords carry the *pane* the way the unshifted ones carry
    /// *you* (the 2026-09-01 map amendment unified the arrow and letter
    /// spellings; resize moved to vim's `-`/`=`/`<`/`>`). Focus follows
    /// the pane for free: the tree swaps two ids, so the focused id is
    /// unchanged and simply occupies a different slot.
    MovePane(Dir),
    NewTab,
    GoToTab(usize),
    /// U7: step to the next / previous tab, wrapping at both ends — the
    /// only keyboard route to tabs 10+ (`Alt+1..9` runs out at nine).
    NextTab,
    PrevTab,
    /// U7: jump to the last tab (`Alt+0`), whatever its number.
    LastTab,
    /// C28: carry the focused pane to the next / previous tab, wrapping —
    /// `Alt+i` / `Alt+Shift+i`, the letter family beside the `m` family
    /// that moves *you* between tabs (each a shift-reverse pair since the
    /// 2026-09-01 re-key). Focus follows the pane.
    MovePaneToTab {
        forward: bool,
    },
    /// C40: mark the focused pane (`Alt+Shift+x`) so `PullPane` can carry
    /// it to an arbitrary tab later — C28's move without the walk, for the
    /// workspace where the destination is six tabs away rather than next
    /// door. Nothing moves on the mark: a pane's process cannot sit in
    /// limbo, so this is tmux's `select-pane -m`, not a clipboard cut.
    /// Marking the already-marked pane unmarks it.
    MarkPane,
    /// C40: pull the marked pane into the active tab (`Alt+Shift+v`),
    /// landing it exactly the way `MovePaneToTab` lands one — same host,
    /// same split rule, same refusal when the tab has no room. Focus
    /// follows the pane.
    PullPane,
    /// Alt+s: collapse the focused pane's innermost split into a stack with
    /// the focused pane expanded (C6–C8). Refuses when the pane is already
    /// stacked — the 2026-09-01 re-key split the old `ToggleStack` toggle
    /// into two one-way chords, matching the shift-reverse idiom (C37/C28).
    StackPane,
    /// Alt+Shift+s: explode the stack containing the focused pane back into
    /// an even split. The other half of the old `ToggleStack` toggle.
    ExplodeStack,
    /// Flip the focused pane's split between vertical and horizontal.
    FlipSplit,
    /// Grow (+) or shrink (−) the focused pane along an axis.
    Resize {
        horizontal: bool,
        grow: bool,
    },
    /// Open the combined pane editor, Alt+r (C32) — name row + parking
    /// note lines in one dialog; the note's first line is the headline
    /// the C4 badge shows. Absorbed v0.1.7's separate Alt+Shift+n note
    /// chord after one release.
    EditPane,
    RenameTab,
    QuickLaunch,
    ScrollMode,
    CopyMode,
    ToggleHints,
    /// Reopen the most recently closed pane or tab (fat-finger undo).
    Undo,
    /// Toggle the full-keymap help overlay.
    Help,
    /// Focus the next pane that needs input, worst-first, wrapping across
    /// tabs (C19).
    JumpAttention,
    /// Toggle the C27 fleet roster overlay — every pane in the workspace,
    /// grouped by tab, opening on the pane `Alt+a` would jump to
    /// (Alt+Shift+a).
    ToggleRoster,
    /// Toggle a full-screen, focus-following view of the focused pane — a
    /// pure view transform, no layout change (C21).
    ToggleZoom,
    /// Snap the active tab to the next canned arrangement that fits (C25);
    /// `forward: false` walks the same cycle backwards (C37, `Alt+Shift+g`).
    CycleLayout {
        forward: bool,
    },
    /// Toggle the C20 activity-feed overlay (status/spawn/close/exit/control
    /// events), Alt+e.
    ToggleFeed,
    /// Toggle the app-wide floating scratch pane, Alt+f (C22).
    ToggleFloat,
    /// Toggle the focused pane's raw (hard pass-through) membership,
    /// Alt+Shift+p — also the chord that exits raw (C23).
    ToggleRaw,
}

#[derive(Debug, PartialEq)]
pub enum InputResult {
    Action(Action),
    Forward(Vec<u8>),
    Ignore,
}

fn translate(key: KeyEvent) -> InputResult {
    if key.kind == KeyEventKind::Release {
        return InputResult::Ignore;
    }

    if key.modifiers.contains(KeyModifiers::ALT) {
        // P13: the chord table matches only when CONTROL is absent —
        // Ctrl+Alt+key is emacs/readline vocabulary (C-M-f forward-word,
        // C-M-w append-kill …), not a roost chord; matching on ALT alone
        // made C-M-w close a pane. Forward as meta-ESC + ctrl byte instead,
        // exactly what `encode_raw` computes (it strips ALT, keeps CTRL).
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            let bytes = encode_raw(key);
            return if bytes.is_empty() {
                InputResult::Ignore
            } else {
                InputResult::Forward(bytes)
            };
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        return match default_chord_action(key.code, shift) {
            Some(a) => InputResult::Action(a),
            // U5 (SPEC-ux) / P13: an unbound Alt+printable is the pane's
            // vocabulary, not roost's — forward it as the meta-ESC bytes a
            // real terminal sends (readline M-b/M-d/M-. et al.) instead of
            // swallowing it. Bound chords stay roost's (matched above); raw
            // mode remains the escape hatch for those. Non-printables that
            // miss the table keep the old swallow — U5's contract is
            // printables, deliberately.
            None => unbound_alt(key),
        };
    }

    encode_key(key)
}

/// The hard-coded default Alt-chord table (design doc §7), factored out of
/// `translate` into a plain function of `(code, shift)` so config.json's
/// overrides (`translate_with`, `Keymap`) fall back to exactly this for any
/// chord they don't touch — the same match `translate` always ran, not a
/// second, re-derived copy of it. `shift` is `key.modifiers.contains(SHIFT)`;
/// several arms below don't test it at all, because the terminal delivers
/// some shifted chords as the plain lowercase code with the modifier bit set
/// and others as the bare uppercase codepoint with the bit unset — both are
/// handled, arm by arm, exactly as before this was split out of `translate`.
fn default_chord_action(code: KeyCode, shift: bool) -> Option<Action> {
    match code {
        // C33 + the 2026-09-01 map amendment: Shift+arrows move the *pane*,
        // exactly what Shift+hjkl has done since C33 — one family, one verb,
        // both spellings. (They sat on resize until resize moved to the
        // vim-idiom punctuation below.)
        KeyCode::Right if shift => Some(Action::MovePane(Dir::Right)),
        KeyCode::Left if shift => Some(Action::MovePane(Dir::Left)),
        KeyCode::Down if shift => Some(Action::MovePane(Dir::Down)),
        KeyCode::Up if shift => Some(Action::MovePane(Dir::Up)),
        // Vim's Ctrl-w resize idioms, on Alt: `-`/`=` height, `<`/`>` width.
        // One physical key, two delivery spellings — some terminals report
        // the shifted half as the glyph (`+` for Alt+Shift+=), others as the
        // base character with SHIFT set (`,` for Alt+<) — so both spellings
        // are bound, the arm ignoring shift where the glyph is the code.
        // The unshifted base keys `,`/`.`/`_` stay free on purpose: they are
        // readline's M-, / M-. / M-_ and must keep forwarding (U5).
        //
        // What the shifted half *does* cost, recorded rather than glossed:
        // `<`/`>` are readline's M-< / M-> (beginning- and end-of-history),
        // so this family does shadow two live bindings — U5 protects the
        // unshifted keys, not the whole punctuation vocabulary. The trade is
        // deliberate (vim's resize idiom is the one every user of a
        // multiplexer already knows, and history-start/end have PgUp-class
        // alternatives in every shell), and C23's raw mode is the escape
        // hatch, as it is for every chord that takes a key the pane wanted.
        KeyCode::Char('-') => Some(Action::Resize { horizontal: false, grow: false }),
        KeyCode::Char('=') | KeyCode::Char('+') => {
            Some(Action::Resize { horizontal: false, grow: true })
        }
        KeyCode::Char('<') => Some(Action::Resize { horizontal: true, grow: false }),
        KeyCode::Char(',') if shift => Some(Action::Resize { horizontal: true, grow: false }),
        KeyCode::Char('>') => Some(Action::Resize { horizontal: true, grow: true }),
        KeyCode::Char('.') if shift => Some(Action::Resize { horizontal: true, grow: true }),
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Char('n') => Some(Action::NewPane),
        KeyCode::Char('w') => Some(Action::ClosePane),
        KeyCode::Char('t') => Some(Action::NewTab),
        KeyCode::Char('s') => Some(if shift { Action::ExplodeStack } else { Action::StackPane }),
        KeyCode::Char('S') => Some(Action::ExplodeStack),
        KeyCode::Char('o') => Some(Action::FlipSplit), // orientation
        // Alt+r edits the pane (name + note, one dialog — C32; it also
        // retired Alt+Shift+n after one release, returning `n` to §8's
        // free pool); Alt+Shift+r (or Alt+R) renames the tab.
        KeyCode::Char('r') => Some(if shift { Action::RenameTab } else { Action::EditPane }),
        KeyCode::Char('R') => Some(Action::RenameTab),
        // Alt+Shift+p toggles raw; Alt+P tolerates the uppercase-delivery
        // quirk some terminals use for a shifted Alt+letter (same
        // tolerance as Alt+Shift+r / Alt+R above). Lowercase Alt+p (no
        // shift) is deliberately unmatched — it stays free (C23).
        KeyCode::Char('p') if shift => Some(Action::ToggleRaw),
        KeyCode::Char('P') => Some(Action::ToggleRaw),
        KeyCode::Enter => Some(Action::QuickLaunch),
        // `?` *is* Shift+`/`, and terminals disagree about which half of
        // that they report: some deliver `Char('?')` with the shift
        // already applied, others deliver `Char('/')` and leave SHIFT in
        // the modifiers. Matching only the first meant Alt+? silently
        // toggled the hint bar on the second kind — reported on macOS.
        // Same shape as the Alt+Shift+r / Alt+R tolerance above.
        KeyCode::Char('/') => Some(if shift { Action::Help } else { Action::ToggleHints }),
        KeyCode::Char('c') => Some(Action::CopyMode),
        KeyCode::Char('u') => Some(Action::Undo),
        KeyCode::Char('?') => Some(Action::Help),
        // C19/C27, the deliberate pair: Alt+a takes you to the next pane
        // that needs you; Alt+Shift+a shows you all of them and lets you
        // choose. Alt+'A' is the same uppercase-delivery tolerance
        // Alt+Shift+r / Alt+Shift+p already carry.
        KeyCode::Char('a') => {
            Some(if shift { Action::ToggleRoster } else { Action::JumpAttention })
        }
        KeyCode::Char('A') => Some(Action::ToggleRoster),
        KeyCode::Char('z') => Some(Action::ToggleZoom),
        // C37: the shifted sibling reverses the cycle — C28's idiom
        // (shift = the inverse of the unshifted chord), and the same
        // both-delivery-forms tolerance every shifted letter carries.
        KeyCode::Char('g') => Some(Action::CycleLayout { forward: !shift }),
        KeyCode::Char('G') => Some(Action::CycleLayout { forward: false }),
        KeyCode::Char('e') => Some(Action::ToggleFeed),
        KeyCode::Char('f') => Some(Action::ToggleFloat),
        KeyCode::PageUp => Some(Action::ScrollMode),
        KeyCode::Char(c @ '1'..='9') => Some(Action::GoToTab(c as usize - '1' as usize)),
        // U7: tabs 10+ had no keyboard route at all. `Alt+0` closes the
        // digit row's own gap (last tab, the "and the rest" slot), and
        // Alt+m steps the strip. The letters come from §8's free pool, and
        // both survive U5: they were unbound, so they used to forward to
        // the pane — now they're roost's, like every other chord in this
        // table. See §8's amendment for why these two.
        KeyCode::Char('0') => Some(Action::LastTab),
        // 2026-09-01 map amendment: both tab families are same-letter,
        // shift-reverses now — `Alt+m`/`Alt+Shift+m` step next/previous
        // tab, `Alt+i`/`Alt+Shift+i` carry the focused pane to the next/
        // previous tab and follow it (C28's carry verb, re-homed off the
        // Shift+m/Shift+i spellings so both families share the idiom C37's
        // `g`/`Shift+g` established). `Alt+I`/`Alt+M` are the uppercase-
        // delivery tolerance Alt+Shift+r / Alt+Shift+a / Alt+Shift+p carry.
        // (Alt+[ / Alt+] were the brief's suggestion and are rejected in
        // §8: `ESC [` is the CSI introducer.)
        KeyCode::Char('i') => Some(if shift {
            Action::MovePaneToTab { forward: false }
        } else {
            Action::MovePaneToTab { forward: true }
        }),
        KeyCode::Char('I') => Some(Action::MovePaneToTab { forward: false }),
        KeyCode::Char('m') => Some(if shift { Action::PrevTab } else { Action::NextTab }),
        KeyCode::Char('M') => Some(Action::PrevTab),
        // C40: the same "carry the pane without walking" rule one step
        // further — mark here, pull there, for a destination too far to
        // walk with `Alt+Shift+i`. `x`/`v` come from §8's free pool, where
        // they were reserved for clipboard vocabulary; this is that
        // vocabulary, on panes rather than on text. Only the *shifted*
        // forms are taken: bare `Alt+x`/`Alt+v` stay unbound so emacs'
        // `M-x` and `M-v` keep reaching the pane through U5, which is why
        // `b`/`d` were struck out of the pool in the first place. Both
        // delivery spellings, same reason as every shifted letter above.
        KeyCode::Char('x') if shift => Some(Action::MarkPane),
        KeyCode::Char('X') => Some(Action::MarkPane),
        KeyCode::Char('v') if shift => Some(Action::PullPane),
        KeyCode::Char('V') => Some(Action::PullPane),
        // C33: the shifted vim letters move the *pane*. These four arms
        // must sit above the focus arms below, which match `Char('l')` &c
        // with **no shift guard** — before C33 a shifted lowercase delivery
        // fell into them and silently did the unshifted thing, while the
        // uppercase delivery matched nothing at all and forwarded meta-L to
        // the pane. One physical chord, two behaviours, split by terminal.
        // Both spellings are bound here for the same reason Alt+Shift+r /
        // Alt+R and C28's Alt+Shift+i / Alt+I are: `twins` then pairs them
        // automatically (both now default to the same action), so a
        // config.json remap of `alt+shift+h` can no longer half-apply.
        // C35: `;` is punctuation, so it costs the §8 letter pool nothing —
        // the pool that is "empty" is the *letters*, and every chord outside
        // `/` and `?` was still free. tmux's own last-pane key, and no
        // readline binding to collide with.
        KeyCode::Char(';') => Some(Action::FocusAlternate),
        // C36: `'` neighbours `;` on the keyboard, and the two are the
        // fleet's pair of punctuation verbs — go back, and speak to
        // everyone. Punctuation costs the §8 letter pool nothing (C35).
        KeyCode::Char('\'') => Some(Action::ToggleBroadcast),
        KeyCode::Char('l') if shift => Some(Action::MovePane(Dir::Right)),
        KeyCode::Char('L') => Some(Action::MovePane(Dir::Right)),
        KeyCode::Char('h') if shift => Some(Action::MovePane(Dir::Left)),
        KeyCode::Char('H') => Some(Action::MovePane(Dir::Left)),
        KeyCode::Char('j') if shift => Some(Action::MovePane(Dir::Down)),
        KeyCode::Char('J') => Some(Action::MovePane(Dir::Down)),
        KeyCode::Char('k') if shift => Some(Action::MovePane(Dir::Up)),
        KeyCode::Char('K') => Some(Action::MovePane(Dir::Up)),
        KeyCode::Right | KeyCode::Char('l') => Some(Action::Focus(Dir::Right)),
        KeyCode::Left | KeyCode::Char('h') => Some(Action::Focus(Dir::Left)),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::Focus(Dir::Down)),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::Focus(Dir::Up)),
        _ => None,
    }
}

/// U5's unbound-Alt-printable fallback, factored out of `translate` so a
/// `"disable"` entry in config.json (`translate_with`) reuses it verbatim —
/// a disabled chord must forward exactly like a naturally unbound one, never
/// a hand-derived approximation of it.
/// What `{"alt+left": "disable"}` sends to the pane.
///
/// **Not** `unbound_alt`, and the difference is the whole point of the
/// keyword. U5 scoped roost's forward-the-key promise to *printables*, so an
/// unbound Alt+arrow is swallowed — a defensible default for a key the user
/// never said anything about. `disable` is the opposite situation: the user
/// went to a config file specifically to say "roost, stop taking this key",
/// and swallowing it turns the chord into a black hole that reaches neither
/// roost nor the shell — strictly worse than leaving it bound, and the exact
/// failure the canonical use case (giving Alt+arrow back to a shell that
/// binds it to word motion) is trying to avoid.
///
/// `encode_raw` is the right encoder and `encode_key` is not: `encode_key`
/// drops the ALT bit entirely (`Alt+Left` → a bare `ESC [ D`, i.e. plain
/// Left; `Alt+f` → `f`), which would send the pane a *different key*.
/// `encode_raw` keeps it as the meta-ESC prefix every one of these already
/// arrives in — `ESC f` for Alt+f, `ESC ESC [ D` for Alt+Left, `ESC CR` for
/// Alt+Enter. `Ignore` survives only for a chord with no encoding at all.
fn disabled_chord(key: KeyEvent) -> InputResult {
    let bytes = encode_raw(key);
    if bytes.is_empty() {
        InputResult::Ignore
    } else {
        InputResult::Forward(bytes)
    }
}

fn unbound_alt(key: KeyEvent) -> InputResult {
    match key.code {
        KeyCode::Char(_) => {
            let bytes = encode_raw(key);
            if bytes.is_empty() {
                InputResult::Ignore
            } else {
                InputResult::Forward(bytes)
            }
        }
        _ => InputResult::Ignore,
    }
}

/// Upgrade cursor-key bytes to the SS3 application encodings when the target
/// pane switched on DECCKM (`CSI ?1h` — zsh's line editor via `smkx`, vim,
/// most full-screen TUIs). A real terminal transmits `ESC O A` … for the
/// cursor keys (and xterm's PC-style Home/End) in that mode, and
/// terminfo-driven bindings listen for exactly those bytes — e.g. zsh widgets
/// bound to `$terminfo[kcuu1]` = `\EOA` (atuin's up-arrow search among them)
/// never fire on the normal-mode `\E[A`. Only unmodified keys upgrade:
/// Shift/Ctrl/Alt-modified cursor keys are never sent as SS3 by real
/// terminals regardless of DECCKM. Called from the forward path, where the
/// focused pane's state is known (same pattern as `kitty_upgrade`).
pub fn app_cursor_upgrade(key: KeyEvent, bytes: Vec<u8>, app_cursor: bool) -> Vec<u8> {
    if !app_cursor
        || key.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return bytes;
    }
    match key.code {
        KeyCode::Up => b"\x1bOA".to_vec(),
        KeyCode::Down => b"\x1bOB".to_vec(),
        KeyCode::Right => b"\x1bOC".to_vec(),
        KeyCode::Left => b"\x1bOD".to_vec(),
        KeyCode::Home => b"\x1bOH".to_vec(),
        KeyCode::End => b"\x1bOF".to_vec(),
        _ => bytes,
    }
}

/// kitty's modifier parameter: `1 + shift(1) + alt(2) + ctrl(4)`. Same shape
/// as xterm's (`encode_key`'s `xm`), and `1` means "no modifiers" — which the
/// CSI-u form then omits entirely.
fn kitty_mods(m: KeyModifiers) -> u8 {
    1 + u8::from(m.contains(KeyModifiers::SHIFT))
        + 2 * u8::from(m.contains(KeyModifiers::ALT))
        + 4 * u8::from(m.contains(KeyModifiers::CONTROL))
}

/// One key in the kitty CSI-u encoding: `CSI <code> ; <mods> u`, with the
/// `; <mods>` dropped when there are none. `code` is the key's **unshifted**
/// Unicode codepoint — kitty carries the shift in the modifier, not in the
/// code, so `Ctrl+Shift+A` is `97;6u` and never `65;…`.
fn csi_u(code: u32, m: KeyModifiers) -> Vec<u8> {
    match kitty_mods(m) {
        1 => format!("\x1b[{code}u").into_bytes(),
        xm => format!("\x1b[{code};{xm}u").into_bytes(),
    }
}

/// Re-encode a key in the kitty CSI-u form when the target pane negotiated
/// the **disambiguate** flag (`kitty`); panes that never opted in keep the
/// legacy bytes `encode_key` produced. Called from the forward path (raw and
/// cooked alike), where the focused pane's state is known.
///
/// [Amended, P8] This used to upgrade *only* modified Enter, while
/// `queries.rs` echoed the app's whole pushed flag word back at it — so a
/// pane that asked for disambiguation was told "yes" and then sent bare
/// `0x1b` for Esc and legacy control bytes for `Ctrl+key`. The flag is now
/// masked to what roost implements (`queries::SUPPORTED`), and this function
/// is the other half of that bargain: **disambiguate now means what it
/// says.**
///
/// What the flag actually promises, and what is delivered here:
/// - **Esc → `CSI 27 u`.** The headline of the whole protocol: a bare `0x1b`
///   is indistinguishable from the first byte of an escape sequence, which
///   is why an app asks for this mode at all.
/// - **`Ctrl`/`Alt` + a printable → `CSI <code> ; <mods> u`.** The legacy
///   encodings collide (`Ctrl+I` ≡ Tab, `Ctrl+M` ≡ Enter, `Ctrl+[` ≡ Esc)
///   and for much of the keyboard they don't exist at all — P12 has roost
///   *dropping* `Ctrl+-` rather than sending a wrong control byte. CSI-u has
///   an exact answer for every one of them, so under this flag the P12 hole
///   closes on its own.
/// - **Modified Enter → `CSI 13 ; <mods> u`**, as before, now through the
///   same general encoder rather than two hand-written constants.
///
/// Deliberately still legacy: unmodified printables (the spec keeps those
/// legacy under disambiguate alone), and the navigation/function keys, whose
/// modified legacy forms (`CSI 1;5C`) are already unambiguous. Sending
/// *everything* as CSI-u is bit `0x8`, which roost does not claim.
pub fn kitty_upgrade(key: KeyEvent, bytes: Vec<u8>, kitty: bool) -> Vec<u8> {
    if !kitty {
        return bytes;
    }
    let m = key.modifiers;
    let ctrl_or_alt = m.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    match key.code {
        KeyCode::Esc => csi_u(27, m),
        KeyCode::Enter
            if m.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            csi_u(13, m)
        }
        // The unshifted codepoint: crossterm hands us the *shifted* char for
        // `Ctrl+Shift+a` on some terminals, and kitty wants `97;6u`.
        KeyCode::Char(c) if ctrl_or_alt => csi_u(c.to_ascii_lowercase() as u32, m),
        // Functional keys keep their **legacy CSI shape** under the kitty
        // protocol — `CSI 1;3D` for Alt+Left, not a `u` form. Only keys with
        // no legacy encoding move to `CSI code;mods u`, which is why the
        // arms above (Esc, Enter, printables) are the ones that do.
        //
        // Reached because `disable` forwards these now: a pane that
        // negotiated disambiguation asked to be told which modifiers were
        // held, and the meta-ESC fallback (`ESC ESC [ D`) does not say. This
        // is the one case where roost can pick the encoding without
        // guessing — the pane declared what it wants — so it is the only
        // case where it picks something other than meta-ESC.
        KeyCode::Left
        | KeyCode::Right
        | KeyCode::Up
        | KeyCode::Down
        | KeyCode::Home
        | KeyCode::End
        | KeyCode::PageUp
        | KeyCode::PageDown
        | KeyCode::Insert
        | KeyCode::Delete
        | KeyCode::BackTab
            if ctrl_or_alt || m.contains(KeyModifiers::SHIFT) =>
        {
            match encode_key(key) {
                InputResult::Forward(b) => b,
                _ => bytes,
            }
        }
        _ => bytes,
    }
}

/// Encode a key event as the bytes a terminal would send. Covers the common
/// set; a pane that negotiated kitty gets modified Enter upgraded to CSI-u by
/// `kitty_upgrade` on the way out.
fn encode_key(key: KeyEvent) -> InputResult {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    // xterm's modifier parameter, in full: 1 + shift(1) + alt(2) + ctrl(4).
    // xm == 1 means "unmodified": the keys below keep their bare legacy
    // forms, exactly as a terminal would.
    //
    // Alt used to be excluded here, on the true observation that nothing
    // ever passed this function an Alt key — in cooked routing Alt is
    // roost's own chord layer, and `encode_raw` strips it before delegating.
    // `kitty_upgrade` now does pass one (a forwarded Alt+arrow for a pane
    // that negotiated the kitty protocol), and it wants exactly the arithmetic
    // xterm documents. The `Char` arms below are unaffected either way: they
    // never consult `xm`, so an Alt+printable still goes out as the meta-ESC
    // `encode_raw` builds rather than through here.
    let xm = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    // A navigation key with xm > 1 becomes `CSI 1;xm X` (Ctrl+Right = word
    // jump in readline/zsh arrives as `\x1b[1;5C`); a tilde key becomes
    // `CSI n;xm ~`. Unmodified, both keep the exact bytes they always sent.
    let csi = |ch: u8| -> Vec<u8> {
        if xm == 1 {
            vec![0x1b, b'[', ch]
        } else {
            format!("\x1b[1;{xm}{}", ch as char).into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if xm == 1 {
            format!("\x1b[{n}~").into_bytes()
        } else {
            format!("\x1b[{n};{xm}~").into_bytes()
        }
    };
    let bytes: Vec<u8> = match key.code {
        // Safety (C29): a SUPER-modified char (⌘-anything) must never leak
        // into a pane as a bare keystroke. Terminal.app/kitty/Ghostty keep ⌘
        // for their own menu/copy-paste bindings and it never reaches roost
        // at all; iTerm2 only delivers it if the user remapped it away from
        // those. But *if* one ever does — e.g. under the kitty keyboard
        // protocol roost negotiates — forwarding just the base char would
        // silently drop the modifier: a ⌘C the emulator didn't intercept
        // would type a bare `c` into the agent's prompt instead of doing
        // nothing. Swallow it rather than bind or forward it. Checked ahead
        // of the Ctrl arm below so Ctrl+⌘+char is swallowed too.
        KeyCode::Char(_) if key.modifiers.contains(KeyModifiers::SUPER) => {
            return InputResult::Ignore;
        }
        KeyCode::Char(c) if ctrl => match ctrl_byte(c) {
            Some(b) => vec![b],
            // P12: no real C0 mapping — forward nothing rather than a wrong
            // control byte (the blanket `& 0x1f` used to turn Ctrl+`-` into
            // CR, submitting half-written prompts).
            None => return InputResult::Ignore,
        },
        KeyCode::Char(c) => c.to_string().into_bytes(),
        // Shift+Enter / Ctrl+Enter → ESC+CR ("meta-enter") as the *fallback*
        // for panes that never negotiated the kitty keyboard protocol: pi's
        // editor matches the literal `\x1b\r`, and it's macOS Option+Enter,
        // which Claude Code accepts too. A pane that DID negotiate kitty gets
        // the precise CSI-u form instead — that upgrade happens in
        // `kitty_upgrade` (called from main with the focused pane's state),
        // since key encoding here has no per-pane context. Either way this only
        // fires when the *outer* terminal delivers Shift/Ctrl+Enter as a
        // distinct key (the enhancement negotiation in main.rs); without that,
        // plain Enter (submit) is unaffected.
        KeyCode::Enter if key.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL) => {
            b"\x1b\r".to_vec()
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => csi(b'A'),
        KeyCode::Down => csi(b'B'),
        KeyCode::Right => csi(b'C'),
        KeyCode::Left => csi(b'D'),
        KeyCode::Home => csi(b'H'),
        KeyCode::End => csi(b'F'),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::Delete => tilde(3),
        KeyCode::Insert => tilde(2),
        // xterm PC-style function keys. F1–F4 are SS3 P/Q/R/S bare (that's
        // independent of DECCKM) and `CSI 1;xm P` … modified; F5–F12 are
        // tilde keys with xterm's historical gaps (no 16~, no 22~).
        KeyCode::F(n @ 1..=4) => {
            let ch = b'P' + (n - 1);
            if xm == 1 {
                vec![0x1b, b'O', ch]
            } else {
                format!("\x1b[1;{xm}{}", ch as char).into_bytes()
            }
        }
        KeyCode::F(n @ 5..=12) => {
            const TILDE: [u8; 8] = [15, 17, 18, 19, 20, 21, 23, 24];
            tilde(TILDE[n as usize - 5])
        }
        _ => return InputResult::Ignore,
    };
    InputResult::Forward(bytes)
}

/// P12: the byte Ctrl+`c` transmits — only where the C0 mapping is real.
/// The classic `& 0x1f` mask is correct for the ASCII letters and for
/// `@ [ \ ] ^ _` (and space ≡ `@` → NUL); `?` is DEL by the same xterm
/// convention; and terminals map Ctrl+`-` and Ctrl+`/` to 0x1F (US, the
/// readline undo / help bindings). Everything else — digits, the remaining
/// punctuation — has no C0 identity: `None`, forward nothing, because the
/// masked byte would be a *different* control character (Ctrl+`-` used to
/// come out as CR and submit the prompt).
fn ctrl_byte(c: char) -> Option<u8> {
    match c.to_ascii_lowercase() {
        c @ ('a'..='z' | '@' | '[' | '\\' | ']' | '^' | '_' | ' ') => Some((c as u8) & 0x1f),
        '?' => Some(0x7f),
        '-' | '/' => Some(0x1f),
        _ => None,
    }
}

/// C23: encode a key event the way a raw-focused pane sees it — the
/// meta-ESC convention readline/agent CLIs bind. A non-Alt key forwards
/// exactly as `encode_key` already would (raw mode doesn't change those).
/// An Alt-modified key forwards as `0x1b` followed by `encode_key`'s bytes
/// for the *same* key with only the Alt bit cleared (Shift/Ctrl still
/// apply) — `Alt+Enter` → `1b 0d`, `Alt+q` → `1b 71`. A key with no sensible
/// base encoding (e.g. a bare function key) forwards nothing rather than a
/// lone, meaning-laden ESC byte. Called from `main.rs`'s raw bypass, ahead
/// of `translate()` — see its module doc.
pub fn encode_raw(key: KeyEvent) -> Vec<u8> {
    if !key.modifiers.contains(KeyModifiers::ALT) {
        return match encode_key(key) {
            InputResult::Forward(bytes) => bytes,
            _ => Vec::new(),
        };
    }
    let unmodified = KeyEvent::new(key.code, key.modifiers & !KeyModifiers::ALT);
    match encode_key(unmodified) {
        InputResult::Forward(bytes) => {
            let mut out = Vec::with_capacity(bytes.len() + 1);
            out.push(0x1b);
            out.extend(bytes);
            out
        }
        _ => Vec::new(),
    }
}

// -- config.json: the key-bindings escape hatch --------------------------
//
// roost puts every shortcut on Alt (module doc), which collides with
// readline/shell word-motion for shell-heavy users — Alt+f/Alt+b/Alt+d most
// visibly, and until now there was no way to disable or move a single
// binding. `config.json`, read once at startup from the same directory as
// `workspace.json` (`infra::config`), is that escape hatch: disable or remap
// individual Alt chords. Nothing else — no theming, no scripting, no layout.
//
// Absent file = today's behavior, byte for byte: `Keymap::default()` is
// empty, so `translate_with` always misses the override lookup below and
// falls straight through to plain `translate`.

/// One physical Alt chord, as `translate_with` sees it: a key code plus
/// whether SHIFT was held. Every roost binding lives on Alt, so that's not
/// part of this — a chord string always starts `alt+` (`Chord::parse`), and
/// P13's Ctrl+Alt exclusion means the override lookup never even runs for a
/// Ctrl+Alt key (see `translate_with`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Chord {
    code: KeyCode,
    shift: bool,
}

impl Chord {
    /// `alt+<key>` or `alt+shift+<key>` — config.json's chord grammar.
    /// `<key>` is one of the named specials below or exactly one character
    /// (`alt+f`, `alt+3`, `alt+/`, ...). `None` for anything else, which
    /// `Keymap::parse` turns into a non-fatal diagnostic rather than a panic.
    fn parse(s: &str) -> Option<Chord> {
        let rest = s.strip_prefix("alt+")?;
        let (shift, key) = match rest.strip_prefix("shift+") {
            Some(k) => (true, k),
            None => (false, rest),
        };
        Some(Chord { code: named_key(key)?, shift })
    }
}

/// The non-Alt half of a chord string: a named special key, or exactly one
/// character — covers every key any default binding actually uses.
fn named_key(s: &str) -> Option<KeyCode> {
    Some(match s {
        "enter" => KeyCode::Enter,
        "pageup" => KeyCode::PageUp,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        _ => {
            let mut chars = s.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None; // more than one character: not a key name
            }
            KeyCode::Char(c)
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Override {
    /// The chord passes straight through to the focused pane, exactly like
    /// an unbound key does today — the readline fix this whole file is for.
    Disabled,
    Bound(Action),
}

/// config.json's parsed keybinding overrides, applied by `translate_with` on
/// top of the default table. `Keymap::default()` is empty — absent
/// config.json — and makes `translate_with` identical to `translate`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Keymap {
    overrides: HashMap<Chord, Override>,
}

/// What `Keymap::parse` has to say about a `config.json`, split by whether
/// roost *did what the file asked*.
///
/// Two channels rather than one list, because a caller has to be able to
/// tell them apart and the single list made that impossible. `roost keys`
/// exits non-zero so a dotfile test can gate on a config being broken
/// (README), and the startup toast has exactly one slot — both of those are
/// answers to "did something go wrong?", and neither may be triggered by a
/// remark about a config that worked perfectly.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Diagnostics {
    /// Entries roost could **not** use: bad JSON, an unparseable chord, an
    /// unknown action. The config asked for something and did not get it.
    /// These, and only these, gate `roost keys`' exit code.
    pub problems: Vec<String>,
    /// True and worth saying about a config that *worked*. Never gates
    /// anything, never outranks a problem for the one toast slot.
    pub notices: Vec<String>,
}

impl Diagnostics {
    /// Nothing at all to report — the quiet case a valid, unremarkable
    /// config produces.
    pub fn is_empty(&self) -> bool {
        self.problems.is_empty() && self.notices.is_empty()
    }

    pub fn len(&self) -> usize {
        self.problems.len() + self.notices.len()
    }

    /// Everything, **problems first**: a surface with one line to spend
    /// spends it on the thing that went wrong, not on a remark.
    pub fn all(&self) -> impl Iterator<Item = &String> {
        self.problems.iter().chain(self.notices.iter())
    }
}

impl Keymap {
    /// Parse config.json's contents. Never fails outright: invalid JSON,
    /// `"keys"` not an object, an unparseable chord, a non-string value, and
    /// an unknown action name are all non-fatal — the offending entry is
    /// skipped and named in the returned diagnostics (`source`, typically
    /// the file's path, prefixed onto each one) rather than blocking
    /// startup. A chord listed twice keeps only its last value — inherited
    /// for free from `serde_json`'s object parsing, which already collapses
    /// a repeated JSON key the same way (documented in README). A chord
    /// with more than one terminal-delivery encoding (`twins`) is disabled
    /// or remapped on *every* encoding at once — never just the one the
    /// entry happened to name. Rebinding a chord that already carries a
    /// different default action is legal — it displaces that default — and
    /// is reported as a **notice**, not a problem, and then only when the
    /// displaced action is left with no chord at all (see below).
    pub fn parse(raw: &str, source: &str) -> (Keymap, Diagnostics) {
        let mut diagnostics = Diagnostics::default();
        let value: serde_json::Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(e) => {
                diagnostics.problems.push(format!("{source}: invalid JSON ({e}) — using defaults"));
                return (Keymap::default(), diagnostics);
            }
        };
        let mut keymap = Keymap::default();
        let mut displaced: Vec<(String, Action)> = Vec::new();
        match value.get("keys") {
            None => {}
            Some(serde_json::Value::Object(keys)) => {
                for (chord_str, action_val) in keys {
                    let Some(chord) = Chord::parse(chord_str) else {
                        diagnostics
                            .problems
                            .push(format!("{source}: unrecognized chord {chord_str:?} — skipped"));
                        continue;
                    };
                    let Some(action_str) = action_val.as_str() else {
                        diagnostics.problems.push(format!(
                            "{source}: {chord_str:?}: value must be a string — skipped"
                        ));
                        continue;
                    };
                    if action_str == "disable" {
                        for t in twins(chord.code, chord.shift) {
                            keymap.overrides.insert(t, Override::Disabled);
                        }
                        continue;
                    }
                    match action_by_name(action_str) {
                        Some(action) => {
                            // Recorded, not reported: whether this
                            // displacement actually costs the user anything
                            // cannot be known until every entry is merged.
                            // A deliberate swap (`alt+w`→new_pane plus
                            // `alt+n`→close_pane) displaces two defaults and
                            // orphans neither.
                            if let Some(old) =
                                default_keymap().get(&chord).filter(|old| **old != action)
                            {
                                displaced.push((chord_str.clone(), *old));
                            }
                            for t in twins(chord.code, chord.shift) {
                                keymap.overrides.insert(t, Override::Bound(action));
                            }
                        }
                        None => diagnostics.problems.push(format!(
                            "{source}: {chord_str:?}: unknown action {action_str:?} — skipped"
                        )),
                    }
                }
            }
            Some(_) => {
                diagnostics.problems.push(format!("{source}: \"keys\" must be an object — ignored"))
            }
        }
        // Now that every entry is merged, ask the only question worth
        // warning about: is the displaced action reachable at all any more?
        // `effective_bindings` is the same defaults-plus-overrides merge
        // `translate_with` dispatches on, so this reads the map the user
        // will actually be typing against rather than re-deriving one.
        if !displaced.is_empty() {
            let live = effective_bindings(&keymap);
            for (chord_str, old) in displaced {
                if !live.iter().any(|(_, a)| *a == old) {
                    diagnostics.notices.push(format!(
                        "{source}: {chord_str:?}: replaces default {} — which now has no chord",
                        action_name(&old)
                    ));
                }
            }
        }
        (keymap, diagnostics)
    }
}

/// Same as `translate`, but a chord in `keymap` overrides the default table
/// first. An empty `Keymap` makes this byte-identical to `translate`: every
/// chord misses the lookup below and falls through to the exact same call.
pub fn translate_with(key: KeyEvent, keymap: &Keymap) -> InputResult {
    if key.kind != KeyEventKind::Release
        && key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
    {
        let chord = Chord { code: key.code, shift: key.modifiers.contains(KeyModifiers::SHIFT) };
        match keymap.overrides.get(&chord) {
            Some(Override::Disabled) => return disabled_chord(key),
            Some(Override::Bound(action)) => return InputResult::Action(*action),
            None => {}
        }
    }
    translate(key)
}

/// Config-facing action names (config.json's `"keys"` values), both
/// directions at once: `action_name` looks up by `Action`, `action_by_name`
/// by string — one table instead of a name-producing match plus a separate
/// reverse scan. `GoToTab` only goes to 9 because that's all the default
/// chord table ever binds (`Alt+1..9`; `Alt+0` is `LastTab`), same limit the
/// old `format!("go_to_tab_{}", n + 1)` arm had no way to enforce.
const NAMES: &[(&str, Action)] = &[
    ("quit", Action::Quit),
    ("new_pane", Action::NewPane),
    ("close_pane", Action::ClosePane),
    ("focus_left", Action::Focus(Dir::Left)),
    ("focus_right", Action::Focus(Dir::Right)),
    ("focus_up", Action::Focus(Dir::Up)),
    ("focus_down", Action::Focus(Dir::Down)),
    ("focus_alternate", Action::FocusAlternate),
    ("toggle_broadcast", Action::ToggleBroadcast),
    ("move_pane_left", Action::MovePane(Dir::Left)),
    ("move_pane_right", Action::MovePane(Dir::Right)),
    ("move_pane_up", Action::MovePane(Dir::Up)),
    ("move_pane_down", Action::MovePane(Dir::Down)),
    ("new_tab", Action::NewTab),
    ("go_to_tab_1", Action::GoToTab(0)),
    ("go_to_tab_2", Action::GoToTab(1)),
    ("go_to_tab_3", Action::GoToTab(2)),
    ("go_to_tab_4", Action::GoToTab(3)),
    ("go_to_tab_5", Action::GoToTab(4)),
    ("go_to_tab_6", Action::GoToTab(5)),
    ("go_to_tab_7", Action::GoToTab(6)),
    ("go_to_tab_8", Action::GoToTab(7)),
    ("go_to_tab_9", Action::GoToTab(8)),
    ("next_tab", Action::NextTab),
    ("prev_tab", Action::PrevTab),
    ("last_tab", Action::LastTab),
    ("move_pane_to_tab_next", Action::MovePaneToTab { forward: true }),
    ("move_pane_to_tab_prev", Action::MovePaneToTab { forward: false }),
    ("mark_pane", Action::MarkPane),
    ("pull_pane", Action::PullPane),
    ("stack_pane", Action::StackPane),
    ("explode_stack", Action::ExplodeStack),
    // Parse-only alias: config.json files written against the pre-2026-09-01
    // toggle keep working, mapped to the collapse half (the half Alt+s kept).
    ("toggle_stack", Action::StackPane),
    ("flip_split", Action::FlipSplit),
    ("resize_horizontal_grow", Action::Resize { horizontal: true, grow: true }),
    ("resize_horizontal_shrink", Action::Resize { horizontal: true, grow: false }),
    ("resize_vertical_grow", Action::Resize { horizontal: false, grow: true }),
    ("resize_vertical_shrink", Action::Resize { horizontal: false, grow: false }),
    ("edit_pane", Action::EditPane),
    // Parse-only aliases: config.json files written against v0.1.7's two
    // separate actions keep working. Listed after the canonical name so
    // the reverse lookup (`action_name`, first match wins) never emits
    // them.
    ("rename_pane", Action::EditPane),
    ("note_pane", Action::EditPane),
    ("rename_tab", Action::RenameTab),
    ("quick_launch", Action::QuickLaunch),
    ("scroll_mode", Action::ScrollMode),
    ("copy_mode", Action::CopyMode),
    ("toggle_hints", Action::ToggleHints),
    ("undo", Action::Undo),
    ("help", Action::Help),
    ("jump_attention", Action::JumpAttention),
    ("toggle_roster", Action::ToggleRoster),
    ("toggle_zoom", Action::ToggleZoom),
    ("cycle_layout", Action::CycleLayout { forward: true }),
    ("cycle_layout_back", Action::CycleLayout { forward: false }),
    ("toggle_feed", Action::ToggleFeed),
    ("toggle_float", Action::ToggleFloat),
    ("toggle_raw", Action::ToggleRaw),
];

/// An action's config.json name — the reverse of `action_by_name`, and the
/// spelling `roost keys` prints so its output can be pasted straight into a
/// `"keys"` block.
///
/// Where `NAMES` carries aliases for one action (`edit_pane` /
/// `rename_pane` / `note_pane`), the **first** entry wins: it is the name
/// the table leads with, so it is the canonical one to print.
///
/// **[F11, 2026-08-19]** Promoted out of `#[cfg(test)]`. It had one test
/// caller — the round-trip check against `default_keymap` — until `roost
/// keys` needed to name what each chord does.
pub fn action_name(action: &Action) -> String {
    NAMES
        .iter()
        .find(|(_, a)| a == action)
        .map(|(name, _)| (*name).to_string())
        .unwrap_or_else(|| panic!("no config name for action {action:?} — add it to NAMES"))
}

/// Reverse of `action_name` (config.json parsing's `"keys"` values): a
/// straight `NAMES` scan.
fn action_by_name(name: &str) -> Option<Action> {
    NAMES.iter().find(|(n, _)| *n == name).map(|(_, a)| *a)
}

/// Every (chord → action) pair `default_chord_action` actually binds, swept
/// across every key `Chord::parse` can name, crossed with both shift states.
/// This *is* "the existing default map" — built by asking the same function
/// `translate` itself calls, not by re-deriving the table by hand — so it is
/// what tests assert absent-config behavior against, and (the round-trip
/// test) `NAMES`'s independent check.
///
/// **[F1, 2026-08-19]** This was `#[cfg(test)]` for as long as the only
/// thing that needed the swept table was a test. It is production now
/// because `effective_bindings` needs it: the help overlay and hint bar
/// used to hard-code their chord spellings, so a `config.json` remap left
/// both surfaces teaching a chord that no longer worked. Deriving what they
/// draw from the same table `translate` dispatches on is the only way those
/// two can't drift, and this function is that table.
///
/// Memoized: it is a pure function of the compiled-in chord table, and the
/// render path asks for it every frame.
fn default_keymap() -> &'static HashMap<Chord, Action> {
    static TABLE: std::sync::OnceLock<HashMap<Chord, Action>> = std::sync::OnceLock::new();
    TABLE.get_or_init(build_default_keymap)
}

fn build_default_keymap() -> HashMap<Chord, Action> {
    let mut codes: Vec<KeyCode> = (0x20u8..=0x7e).map(|b| KeyCode::Char(b as char)).collect();
    codes.extend([
        KeyCode::Enter,
        KeyCode::PageUp,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Left,
        KeyCode::Right,
    ]);
    let mut map = HashMap::new();
    for code in codes {
        for shift in [false, true] {
            if let Some(action) = default_chord_action(code, shift) {
                map.insert(Chord { code, shift }, action);
            }
        }
    }
    map
}

impl Chord {
    /// The chord's **UI spelling** — what the help overlay and hint bar
    /// print. `Alt+Shift+h`, `Alt+←`, `Alt+Enter`, `Alt+PgUp`.
    ///
    /// Deliberately *not* `Chord::parse`'s inverse. `parse` accepts
    /// config.json's grammar, which is lowercase and spelled-out
    /// (`alt+pageup`, `alt+left`) because it is typed into a JSON file;
    /// this is the chrome's vocabulary, which uses the arrow glyphs and
    /// title-case names the key table has always used. Two spellings of one
    /// chord, each in the register its reader is in — the README documents
    /// the config one.
    fn label(&self) -> String {
        let key = match self.code {
            KeyCode::Enter => "Enter".to_string(),
            KeyCode::PageUp => "PgUp".to_string(),
            KeyCode::Up => "↑".to_string(),
            KeyCode::Down => "↓".to_string(),
            KeyCode::Left => "←".to_string(),
            KeyCode::Right => "→".to_string(),
            KeyCode::Char(c) => c.to_string(),
            other => format!("{other:?}"),
        };
        if self.shift {
            format!("Alt+Shift+{key}")
        } else {
            format!("Alt+{key}")
        }
    }

    /// Sort key giving `effective_bindings` a stable, readable order:
    /// specials before printables (so a focus row reads `Alt+← / Alt+h`, the
    /// order the key table has always written), then unshifted before
    /// shifted, then by the case-folded character.
    fn order(&self) -> (u8, u8, char) {
        let (rank, ch) = match self.code {
            KeyCode::Left => (0, ' '),
            KeyCode::Down => (1, ' '),
            KeyCode::Up => (2, ' '),
            KeyCode::Right => (3, ' '),
            KeyCode::Enter => (4, ' '),
            KeyCode::PageUp => (5, ' '),
            KeyCode::Char(c) => (6, c.to_ascii_lowercase()),
            _ => (7, ' '),
        };
        (rank, u8::from(self.shift), ch)
    }
}

/// The character a US keyboard produces when Shift is held with `c` —
/// `h`→`H`, `/`→`?`, `1`→`!`. The two halves of one physical keypress, which
/// is why `effective_bindings` needs it: terminals disagree about which half
/// they report, and the default table binds both so either works.
fn shifted_char(c: char) -> Option<char> {
    if c.is_ascii_lowercase() {
        return Some(c.to_ascii_uppercase());
    }
    Some(match c {
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        ';' => ':',
        '\'' => '"',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        '`' => '~',
        _ => return None,
    })
}

/// The inverse of `shifted_char`: the base key whose shifted form is `c`
/// (`+` → `=`, `<` → `,`, `?` → `/`). Derived from `shifted_char` itself,
/// not a second table, so the two can never disagree.
fn glyph_base(c: char) -> Option<char> {
    (0x20u8..=0x7e).map(|b| b as char).find(|&b| shifted_char(b) == Some(c))
}

/// Is this entry a *second spelling* of a chord already in the table, rather
/// than a chord of its own? Four shapes, all of which would otherwise make
/// the help overlay print one physical key twice — or print a key that is
/// not really bound at all.
///
/// 1. **The uppercase-delivery twin of a shifted letter.** Some terminals
///    send `Alt+Shift+p` as `('p', SHIFT)`, others as bare `'P'`; the table
///    binds both (C23/C27/C28/C33). The `Alt+Shift+p` spelling is the one
///    roost documents, so the bare-uppercase entry is the redundant one.
/// 2. **The shifted-punctuation twin.** Same duality, other half of the
///    keyboard: `Alt+?` arrives as `('?', no shift)` or `('/', SHIFT)`. Here
///    the *glyph* spelling is the one roost documents (`Alt+?`, not
///    `Alt+Shift+/`), so it is the `('/', SHIFT)` entry that drops — the
///    mirror image of rule 1, because that is which half reads naturally in
///    each case.
/// 3. **A shift state the binding never tested.** Most arms in
///    `default_chord_action` don't guard `shift`, so `('1', SHIFT)` resolves
///    to `GoToTab(0)` exactly as `('1', no shift)` does. That is not a
///    binding anyone should be taught — nothing is bound to `Alt+Shift+1`;
///    the arm simply ignores shift. The unshifted spelling is the chord.
/// 4. **The shiftless glyph of a bound base key.** `Alt+=` and `Alt++` are
///    one physical chord (`+` is Shift+`=`); when both spellings carry the
///    same action, the base-key spelling is the one roost documents
///    (vim's resize idiom is written `=`), so bare `'+'` drops — the mirror
///    of rule 2, which drops the *shifted* base spelling.
///
/// Every rule fires only when both spellings carry the **same action**: when
/// they differ, both are real and distinct (`Alt+←` focuses, `Alt+Shift+←`
/// moves the pane; `Alt+h` focuses, `Alt+Shift+h` moves the pane).
fn is_redundant_spelling(chord: &Chord, action: &Action, table: &HashMap<Chord, Action>) -> bool {
    let same = |code: KeyCode, shift: bool| table.get(&Chord { code, shift }) == Some(action);
    // 3: the arm ignored shift.
    if chord.shift && same(chord.code, false) {
        return true;
    }
    let KeyCode::Char(c) = chord.code else { return false };
    if chord.shift {
        // 2: `('/', SHIFT)` when `('?', no shift)` says the same thing.
        // **Letters are excluded here, and the exclusion is load-bearing.**
        // Rules 1 and 2 are mirror images, so without it a letter pair
        // satisfies both — rule 1 drops `'P'` because `('p', SHIFT)` exists,
        // rule 2 drops `('p', SHIFT)` because `'P'` exists — and the chord
        // vanishes from the table entirely rather than being spelled once.
        // Each rule owns the half of the keyboard whose *surviving* spelling
        // it names: letters keep `Alt+Shift+p`, punctuation keeps `Alt+?`.
        !c.is_ascii_lowercase() && shifted_char(c).is_some_and(|g| same(KeyCode::Char(g), false))
    } else {
        // 1: bare `'P'` when `('p', SHIFT)` says the same thing.
        c.is_ascii_uppercase() && same(KeyCode::Char(c.to_ascii_lowercase()), true)
            // 4: bare `'+'` when `('=', no shift)` says the same thing.
            || glyph_base(c).is_some_and(|b| same(KeyCode::Char(b), false))
    }
}

/// F1: every chord roost **actually** binds right now, as `(label, action)`,
/// with `keymap`'s config.json overrides applied on top of the default
/// table — the same merge `translate_with` dispatches on, asked as a
/// question instead of answered one key at a time.
///
/// This exists so the chrome can *derive* what it teaches. Before F1 the
/// help overlay and the hint bar spelled their chords as `&'static str`
/// literals, so `{"keys": {"alt+f": "disable"}}` — the README's own escape
/// hatch — produced a roost whose `Alt+?` still taught `Alt+f`. Any surface
/// that renders from this cannot say that.
///
/// **Twins are collapsed.** A shifted letter is delivered as `('h', SHIFT)`
/// by some terminals and bare `'H'` by others, and the default table binds
/// both (C23/C27/C28/C33). They are one physical chord, so the bare
/// uppercase form is dropped whenever its shifted-lowercase sibling carries
/// the same action — otherwise every such row would print its chord twice.
///
/// Sorted by `Chord::order`, so the output is deterministic and a row that
/// joins several chords reads in the order the key table always wrote them.
pub fn effective_bindings(keymap: &Keymap) -> Vec<(String, Action)> {
    let mut merged: HashMap<Chord, Action> = default_keymap().clone();
    for (chord, ov) in &keymap.overrides {
        match ov {
            Override::Disabled => {
                merged.remove(chord);
            }
            Override::Bound(a) => {
                merged.insert(*chord, *a);
            }
        }
    }
    let dropped: Vec<Chord> = merged
        .iter()
        .filter(|(chord, action)| is_redundant_spelling(chord, action, &merged))
        .map(|(chord, _)| *chord)
        .collect();
    let mut out: Vec<(Chord, Action)> =
        merged.into_iter().filter(|(c, _)| !dropped.contains(c)).collect();
    out.sort_by_key(|(c, _)| c.order());
    out.into_iter().map(|(c, a)| (c.label(), a)).collect()
}

/// F1/C34: the chord a single action is on, for the chrome surfaces that
/// name one chord in a sentence rather than listing a row's worth — C9's
/// attention segment (`· Alt+a`), C16's dead-pane bar, C10's confirm
/// flashes. `None` when the action has no chord at all (disabled in
/// config.json), which those callers render by dropping the clause rather
/// than naming a key that does nothing.
///
/// The *first* label in `Chord::order`, so a doubly-bound action names its
/// arrow/specials form before its letter form — the same order the key
/// table has always written.
pub fn chord_for(keymap: &Keymap, action: Action) -> Option<String> {
    effective_bindings(keymap).into_iter().find(|(_, a)| *a == action).map(|(l, _)| l)
}

/// Every `(code, shift)` the default table already treats as the *same*
/// logical chord as `(code, shift)` itself — the terminal-delivery duality
/// `default_chord_action` already carries for the shifted letters (p/P,
/// r/R, a/A, i/I, m/M: some terminals send `Alt+Shift+p` as `('p', SHIFT)`,
/// others as `('P', no SHIFT)`, and the default table binds both to
/// `ToggleRaw` so either works) **and** for the shifted punctuation (the
/// `/`↔`?` and `,`↔`<`, `.`↔`>`, `=`↔`+` pairs). A config entry naming *one*
/// delivery form must disable or remap *all* of them — otherwise it
/// silently no-ops on whichever terminal happens to send the other one.
///
/// Derived from `default_chord_action`, not a hand-kept list: starting from
/// the chord itself, candidates are its case-folded siblings and (via
/// `shifted_char`/`glyph_base`) the other half of its physical key, kept
/// while they default to the same action, to a fixed point. That reuses the
/// one source of truth `default_keymap`/`translate` already run on, so a
/// future chord gaining this pattern is covered automatically — see
/// `twin_derivation_finds_every_same_letter_same_action_default_chord`.
/// Always includes `(code, shift)` itself; non-`Char` codes and chords with
/// no default action (nothing to be a twin of) have no other twins.
fn twins(code: KeyCode, shift: bool) -> Vec<Chord> {
    let (Some(action), KeyCode::Char(c)) = (default_chord_action(code, shift), code) else {
        return vec![Chord { code, shift }];
    };
    let mut found = vec![Chord { code, shift }];
    let mut frontier = vec![(c, shift)];
    while let Some((ch, _)) = frontier.pop() {
        // Case-folded siblings arrive in either shift state (some terminals
        // report Alt+Shift+f as bare 'F', others as 'f'+SHIFT — the duality
        // is a cross-product, not a flip).
        let mut candidates = Vec::new();
        for cased in [ch.to_ascii_lowercase(), ch.to_ascii_uppercase()] {
            candidates.push((cased, false));
            candidates.push((cased, true));
        }
        // The other half of the physical key, in the delivery form it
        // arrives as: this key's glyph is reported as the bare character
        // (Shift applied by the terminal), and this glyph's base key
        // arrives with SHIFT still set. Both directions, from either half.
        if let Some(g) = shifted_char(ch) {
            candidates.push((g, false));
        }
        if let Some(b) = glyph_base(ch) {
            candidates.push((b, true));
        }
        for (cc, cs) in candidates {
            let candidate = Chord { code: KeyCode::Char(cc), shift: cs };
            if !found.contains(&candidate)
                && default_chord_action(candidate.code, cs) == Some(action)
            {
                found.push(candidate);
                frontier.push((cc, cs));
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};

    fn alt(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::ALT)
    }
    fn alt_shift(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::ALT | KeyModifiers::SHIFT)
    }
    fn plain(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn alt_chords_map_to_actions() {
        assert!(matches!(translate(alt(KeyCode::Char('q'))), InputResult::Action(Action::Quit)));
        assert!(matches!(translate(alt(KeyCode::Char('n'))), InputResult::Action(Action::NewPane)));
        assert!(matches!(
            translate(alt(KeyCode::Char('s'))),
            InputResult::Action(Action::StackPane)
        ));
        assert!(matches!(
            translate(alt_shift(KeyCode::Char('s'))),
            InputResult::Action(Action::ExplodeStack)
        ));
        assert!(matches!(
            translate(alt(KeyCode::Char('S'))),
            InputResult::Action(Action::ExplodeStack)
        ));
        assert!(matches!(translate(alt(KeyCode::Enter)), InputResult::Action(Action::QuickLaunch)));
        assert!(matches!(
            translate(alt(KeyCode::Char('3'))),
            InputResult::Action(Action::GoToTab(2))
        ));
    }

    #[test]
    fn alt_a_z_g_map_to_jump_zoom_and_cycle_layout() {
        assert!(matches!(
            translate(alt(KeyCode::Char('a'))),
            InputResult::Action(Action::JumpAttention)
        ));
        assert!(matches!(
            translate(alt(KeyCode::Char('z'))),
            InputResult::Action(Action::ToggleZoom)
        ));
        assert!(matches!(
            translate(alt(KeyCode::Char('g'))),
            InputResult::Action(Action::CycleLayout { forward: true })
        ));
    }

    #[test]
    fn alt_e_maps_to_toggle_feed() {
        assert!(matches!(
            translate(alt(KeyCode::Char('e'))),
            InputResult::Action(Action::ToggleFeed)
        ));
    }

    #[test]
    fn alt_f_maps_to_toggle_float() {
        assert!(matches!(
            translate(alt(KeyCode::Char('f'))),
            InputResult::Action(Action::ToggleFloat)
        ));
    }

    #[test]
    fn alt_shift_p_toggles_raw_with_uppercase_delivery_tolerance() {
        assert!(matches!(
            translate(alt_shift(KeyCode::Char('p'))),
            InputResult::Action(Action::ToggleRaw)
        ));
        // some terminals deliver Alt+Shift+p as an uppercase 'P' without the
        // SHIFT modifier bit set (same tolerance as Alt+Shift+r / Alt+R).
        assert!(matches!(
            translate(alt(KeyCode::Char('P'))),
            InputResult::Action(Action::ToggleRaw)
        ));
        // lowercase Alt+p (no shift) is deliberately unbound — and per U5 an
        // unbound Alt+printable now belongs to the pane: it forwards as
        // meta-ESC instead of being swallowed (C23's "stays free" honored by
        // forwarding, the stronger form of free).
        match translate(alt(KeyCode::Char('p'))) {
            InputResult::Forward(b) => assert_eq!(b, vec![0x1b, b'p']),
            _ => panic!("unbound Alt+p must forward as meta-ESC (U5)"),
        }
    }

    /// C27: the shifted sibling of C19's jump. Alt+a stays the jump, the
    /// shifted form opens the roster, and the uppercase-delivery form
    /// (`Alt+'A'`, no SHIFT bit) reaches it too — the same tolerance
    /// Alt+Shift+r / Alt+Shift+p carry.
    #[test]
    fn alt_shift_a_opens_the_roster_with_uppercase_delivery_tolerance() {
        assert!(matches!(
            translate(alt_shift(KeyCode::Char('a'))),
            InputResult::Action(Action::ToggleRoster)
        ));
        assert!(matches!(
            translate(alt(KeyCode::Char('A'))),
            InputResult::Action(Action::ToggleRoster)
        ));
        // ...and the unshifted chord is untouched: it is still the jump.
        assert!(matches!(
            translate(alt(KeyCode::Char('a'))),
            InputResult::Action(Action::JumpAttention)
        ));
    }

    /// C28's carry verb, re-homed by the 2026-09-01 map amendment: the two
    /// tab families are same-letter, shift-reverses — `m`/`Shift+m` step
    /// next/previous tab, `i`/`Shift+i` carry the pane to the next/previous
    /// tab. `Shift+i` keeps the direction it always had (carry-prev); bare
    /// `i` and both `m` spellings moved, so a regression here would silently
    /// carry panes when the user meant to change tabs (or vice versa).
    #[test]
    fn alt_i_carries_and_alt_m_cycles_tabs_with_uppercase_delivery_tolerance() {
        // Carry family: bare `i` = next; `Shift+i` / uppercase `I` = previous
        // (Shift+i keeps the direction it always had).
        assert!(matches!(
            translate(alt(KeyCode::Char('i'))),
            InputResult::Action(Action::MovePaneToTab { forward: true })
        ));
        for key in [alt_shift(KeyCode::Char('i')), alt(KeyCode::Char('I'))] {
            assert!(
                matches!(
                    translate(key),
                    InputResult::Action(Action::MovePaneToTab { forward: false })
                ),
                "{key:?} carries to the previous tab",
            );
        }
        // Tab-step family: bare `m` = next; `Shift+m` / uppercase `M` = previous.
        assert!(matches!(translate(alt(KeyCode::Char('m'))), InputResult::Action(Action::NextTab)));
        for key in [alt_shift(KeyCode::Char('m')), alt(KeyCode::Char('M'))] {
            assert!(matches!(translate(key), InputResult::Action(Action::PrevTab)), "{key:?}");
        }
    }

    #[test]
    fn alt_r_renames_pane_alt_shift_r_renames_tab() {
        assert!(matches!(
            translate(alt(KeyCode::Char('r'))),
            InputResult::Action(Action::EditPane)
        ));
        assert!(matches!(
            translate(alt_shift(KeyCode::Char('r'))),
            InputResult::Action(Action::RenameTab)
        ));
        // some terminals deliver Alt+Shift+r as an uppercase 'R'
        assert!(matches!(
            translate(alt(KeyCode::Char('R'))),
            InputResult::Action(Action::RenameTab)
        ));
    }

    /// C32 (combined editor): Alt+Shift+n is RETIRED — it lived one
    /// release (v0.1.7) before Alt+r absorbed the note editor, and the
    /// chord went back to §8's free pool, meaning U5's unbound-printable
    /// rule now forwards it to the pane. Plain Alt+n still makes a pane.
    #[test]
    fn alt_shift_n_is_retired_back_to_the_free_pool() {
        // Pre-v0.1.7 behavior restored exactly: a terminal reporting the
        // chord as 'n'+SHIFT falls through to the unshifted arm (new
        // pane, shift ignored); one reporting uppercase 'N' hits U5's
        // unbound-printable forward.
        assert!(matches!(
            translate(alt_shift(KeyCode::Char('n'))),
            InputResult::Action(Action::NewPane)
        ));
        assert!(matches!(translate(alt(KeyCode::Char('N'))), InputResult::Forward(_)));
        assert!(matches!(translate(alt(KeyCode::Char('n'))), InputResult::Action(Action::NewPane)));
        assert!(matches!(
            translate(alt(KeyCode::Char('r'))),
            InputResult::Action(Action::EditPane)
        ));
    }

    /// 2026-09-01 map amendment: Shift+arrows joined Shift+hjkl as
    /// move-the-pane — one family, one verb, both spellings — and resize
    /// moved to the vim idiom punctuation (`-`/`=` height, `<`/`>` width).
    #[test]
    fn alt_shift_arrows_move_pane_not_focus() {
        assert!(matches!(
            translate(alt_shift(KeyCode::Right)),
            InputResult::Action(Action::MovePane(Dir::Right))
        ));
        assert!(matches!(
            translate(alt_shift(KeyCode::Up)),
            InputResult::Action(Action::MovePane(Dir::Up))
        ));
        // ...and the letter spellings still say the same thing.
        assert!(matches!(
            translate(alt_shift(KeyCode::Char('h'))),
            InputResult::Action(Action::MovePane(Dir::Left))
        ));
        // plain Alt+arrow still moves focus
        assert!(matches!(
            translate(alt(KeyCode::Right)),
            InputResult::Action(Action::Focus(Dir::Right))
        ));
        assert!(matches!(
            translate(alt(KeyCode::Char('h'))),
            InputResult::Action(Action::Focus(Dir::Left))
        ));
    }

    /// Vim's Ctrl-w resize idioms, on Alt: `-` shrink / `=` grow the height,
    /// `<` shrink / `>` grow the width. Every arm ignores shift (the glyph
    /// spellings `+` and the `,`/`.` base deliveries land on the same
    /// arms), and the unshifted base keys `,`/`.` stay free for readline.
    #[test]
    fn alt_vim_punctuation_resizes_height_and_width() {
        let cases: &[(KeyEvent, Action)] = &[
            (alt(KeyCode::Char('-')), Action::Resize { horizontal: false, grow: false }),
            (alt(KeyCode::Char('=')), Action::Resize { horizontal: false, grow: true }),
            (alt(KeyCode::Char('+')), Action::Resize { horizontal: false, grow: true }),
            (alt_shift(KeyCode::Char('=')), Action::Resize { horizontal: false, grow: true }),
            (alt(KeyCode::Char('<')), Action::Resize { horizontal: true, grow: false }),
            (alt_shift(KeyCode::Char(',')), Action::Resize { horizontal: true, grow: false }),
            (alt(KeyCode::Char('>')), Action::Resize { horizontal: true, grow: true }),
            (alt_shift(KeyCode::Char('.')), Action::Resize { horizontal: true, grow: true }),
        ];
        for (key, want) in cases {
            assert_eq!(translate(*key), InputResult::Action(*want), "{key:?}");
        }
        // The unshifted base keys are the pane's vocabulary, not roost's:
        // M-, and M-. must keep reaching the shell (U5).
        assert!(matches!(translate(alt(KeyCode::Char(','))), InputResult::Forward(_)));
        assert!(matches!(translate(alt(KeyCode::Char('.'))), InputResult::Forward(_)));
        // The shifted spelling of `-` (`_`) was never claimed: vim has no
        // `Ctrl-w _` grow, and M-_ stays the pane's.
        assert!(matches!(translate(alt(KeyCode::Char('_'))), InputResult::Forward(_)));
    }

    #[test]
    fn plain_keys_encode_as_terminal_bytes() {
        match translate(plain(KeyCode::Char('a'))) {
            InputResult::Forward(b) => assert_eq!(b, b"a"),
            _ => panic!(),
        }
        match translate(plain(KeyCode::Enter)) {
            InputResult::Forward(b) => assert_eq!(b, b"\r"),
            _ => panic!(),
        }
        match translate(plain(KeyCode::Up)) {
            InputResult::Forward(b) => assert_eq!(b, b"\x1b[A"),
            _ => panic!(),
        }
        match translate(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)) {
            InputResult::Forward(b) => assert_eq!(b, vec![0x03]),
            _ => panic!(),
        }
    }

    #[test]
    fn shift_and_ctrl_enter_insert_newline_via_esc_cr() {
        // Shift+Enter → ESC+CR, which agent TUIs read as "insert newline".
        match translate(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)) {
            InputResult::Forward(b) => assert_eq!(b, b"\x1b\r"),
            _ => panic!(),
        }
        // Ctrl+Enter → same ESC+CR newline.
        match translate(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)) {
            InputResult::Forward(b) => assert_eq!(b, b"\x1b\r"),
            _ => panic!(),
        }
        // Plain Enter still submits (bare CR), unchanged.
        match translate(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            InputResult::Forward(b) => assert_eq!(b, b"\r"),
            _ => panic!(),
        }
        // Alt+Enter remains the quick-launch chord, not a newline.
        assert!(matches!(
            translate(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)),
            InputResult::Action(Action::QuickLaunch)
        ));
    }

    #[test]
    fn app_cursor_upgrade_sends_ss3_only_when_the_pane_asked() {
        // DECCKM off: bytes pass through untouched.
        assert_eq!(app_cursor_upgrade(plain(KeyCode::Up), b"\x1b[A".to_vec(), false), b"\x1b[A");
        // DECCKM on: every cursor key (xterm's PC-style Home/End included)
        // switches from its `encode_key` CSI form to SS3.
        let cases: &[(KeyCode, &[u8])] = &[
            (KeyCode::Up, b"\x1bOA"),
            (KeyCode::Down, b"\x1bOB"),
            (KeyCode::Right, b"\x1bOC"),
            (KeyCode::Left, b"\x1bOD"),
            (KeyCode::Home, b"\x1bOH"),
            (KeyCode::End, b"\x1bOF"),
        ];
        for (code, want) in cases {
            let base = match translate(plain(*code)) {
                InputResult::Forward(b) => b,
                _ => panic!("{code:?} must forward"),
            };
            assert_eq!(&app_cursor_upgrade(plain(*code), base, true), want, "{code:?}");
        }
        // Modified cursor keys never become SS3 (real terminals keep CSI for
        // those regardless of DECCKM), and non-cursor keys pass through.
        let shift_up = KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT);
        assert_eq!(app_cursor_upgrade(shift_up, b"\x1b[A".to_vec(), true), b"\x1b[A");
        assert_eq!(app_cursor_upgrade(plain(KeyCode::Char('a')), b"a".to_vec(), true), b"a");
        assert_eq!(
            app_cursor_upgrade(plain(KeyCode::PageUp), b"\x1b[5~".to_vec(), true),
            b"\x1b[5~"
        );
    }

    #[test]
    fn function_keys_encode_xterm_pc_style() {
        let cases: &[(u8, &[u8])] = &[
            (1, b"\x1bOP"),
            (2, b"\x1bOQ"),
            (3, b"\x1bOR"),
            (4, b"\x1bOS"),
            (5, b"\x1b[15~"),
            (6, b"\x1b[17~"),
            (7, b"\x1b[18~"),
            (8, b"\x1b[19~"),
            (9, b"\x1b[20~"),
            (10, b"\x1b[21~"),
            (11, b"\x1b[23~"),
            (12, b"\x1b[24~"),
        ];
        for (n, want) in cases {
            match translate(plain(KeyCode::F(*n))) {
                InputResult::Forward(b) => assert_eq!(&b, want, "F{n}"),
                _ => panic!("F{n} must forward, not be swallowed"),
            }
        }
        // Modified F-keys follow the same xterm modifier scheme as nav keys.
        match translate(KeyEvent::new(KeyCode::F(1), KeyModifiers::SHIFT)) {
            InputResult::Forward(b) => assert_eq!(b, b"\x1b[1;2P"),
            _ => panic!(),
        }
        match translate(KeyEvent::new(KeyCode::F(5), KeyModifiers::CONTROL)) {
            InputResult::Forward(b) => assert_eq!(b, b"\x1b[15;5~"),
            _ => panic!(),
        }
    }

    #[test]
    fn modified_navigation_keys_carry_xterm_modifiers() {
        let ctrl = KeyModifiers::CONTROL;
        let shift = KeyModifiers::SHIFT;
        let cases: &[(KeyCode, KeyModifiers, &[u8])] = &[
            (KeyCode::Right, ctrl, b"\x1b[1;5C"), // readline/zsh word jump
            (KeyCode::Left, ctrl, b"\x1b[1;5D"),
            (KeyCode::Up, shift, b"\x1b[1;2A"),
            (KeyCode::Down, shift.union(ctrl), b"\x1b[1;6B"),
            (KeyCode::Home, ctrl, b"\x1b[1;5H"),
            (KeyCode::End, shift, b"\x1b[1;2F"),
            (KeyCode::Delete, ctrl, b"\x1b[3;5~"),
            (KeyCode::Insert, shift, b"\x1b[2;2~"),
            (KeyCode::PageUp, shift, b"\x1b[5;2~"),
            (KeyCode::PageDown, ctrl, b"\x1b[6;5~"),
        ];
        for (code, mods, want) in cases {
            match translate(KeyEvent::new(*code, *mods)) {
                InputResult::Forward(b) => assert_eq!(&b, want, "{code:?} + {mods:?}"),
                _ => panic!("{code:?} + {mods:?} must forward"),
            }
        }
        // Unmodified forms keep the exact bytes they always sent.
        match translate(plain(KeyCode::Right)) {
            InputResult::Forward(b) => assert_eq!(b, b"\x1b[C"),
            _ => panic!(),
        }
        // And a modified cursor key never takes the DECCKM SS3 form.
        let ctrl_right = KeyEvent::new(KeyCode::Right, ctrl);
        assert_eq!(app_cursor_upgrade(ctrl_right, b"\x1b[1;5C".to_vec(), true), b"\x1b[1;5C");
    }

    #[test]
    fn kitty_upgrade_uses_csi_u_only_for_negotiated_panes() {
        let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        let ctrl_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);
        // Non-kitty pane: keep the ESC+CR fallback bytes untouched.
        assert_eq!(kitty_upgrade(shift_enter, b"\x1b\r".to_vec(), false), b"\x1b\r");
        // Kitty pane: upgrade to the precise CSI-u encodings.
        assert_eq!(kitty_upgrade(shift_enter, b"\x1b\r".to_vec(), true), b"\x1b[13;2u");
        assert_eq!(kitty_upgrade(ctrl_enter, b"\x1b\r".to_vec(), true), b"\x1b[13;5u");
        // A plain letter is never touched, kitty or not.
        let a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(kitty_upgrade(a, b"a".to_vec(), true), b"a");
    }

    /// P8: **the headline of the protocol.** An app asks for disambiguation
    /// precisely because a bare `0x1b` cannot be told apart from the first
    /// byte of an escape sequence — and roost was answering "yes" and then
    /// sending the bare byte anyway.
    #[test]
    fn a_negotiated_pane_gets_esc_as_csi_27u() {
        let esc = plain(KeyCode::Esc);
        assert_eq!(kitty_upgrade(esc, vec![0x1b], false), vec![0x1b], "legacy pane: untouched");
        assert_eq!(kitty_upgrade(esc, vec![0x1b], true), b"\x1b[27u");
        // Modified Esc carries kitty's modifier parameter like any other key.
        let ctrl_esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::CONTROL);
        assert_eq!(kitty_upgrade(ctrl_esc, vec![0x1b], true), b"\x1b[27;5u");
    }

    /// P8: `Ctrl`/`Alt` + printable. The legacy encodings collide
    /// (`Ctrl+I` ≡ Tab, `Ctrl+M` ≡ Enter, `Ctrl+[` ≡ Esc) and for much of the
    /// keyboard don't exist at all — P12 has roost *dropping* `Ctrl+-`
    /// rather than sending a wrong byte. CSI-u answers all of them.
    #[test]
    fn a_negotiated_pane_gets_ctrl_and_alt_printables_as_csi_u() {
        let case = |code, mods| kitty_upgrade(KeyEvent::new(code, mods), b"legacy".to_vec(), true);
        // 99 = 'c'. mods: 5 = ctrl, 3 = alt, 7 = both.
        assert_eq!(case(KeyCode::Char('c'), KeyModifiers::CONTROL), b"\x1b[99;5u");
        assert_eq!(case(KeyCode::Char('b'), KeyModifiers::ALT), b"\x1b[98;3u");
        assert_eq!(
            case(KeyCode::Char('w'), KeyModifiers::CONTROL | KeyModifiers::ALT),
            b"\x1b[119;7u",
        );
        // The code is the *unshifted* codepoint — kitty carries shift in the
        // modifier, so Ctrl+Shift+A is 97;6u and never 65;anything.
        assert_eq!(
            case(KeyCode::Char('A'), KeyModifiers::CONTROL | KeyModifiers::SHIFT),
            b"\x1b[97;6u",
        );
        // The collisions the flag exists to resolve, each now distinct from
        // the bare key it used to be indistinguishable from.
        assert_eq!(case(KeyCode::Char('i'), KeyModifiers::CONTROL), b"\x1b[105;5u");
        assert_eq!(case(KeyCode::Char('m'), KeyModifiers::CONTROL), b"\x1b[109;5u");
        assert_eq!(case(KeyCode::Char('['), KeyModifiers::CONTROL), b"\x1b[91;5u");
        // P12's hole closes on its own here: `Ctrl+-` has no C0 identity, so
        // `encode_key` forwards nothing — but CSI-u can say it exactly.
        assert_eq!(case(KeyCode::Char('-'), KeyModifiers::CONTROL), b"\x1b[45;5u");
    }

    /// …and the keys the flag does *not* cover keep their legacy bytes: an
    /// unmodified printable, and a modified navigation key whose legacy form
    /// (`CSI 1;5C`) is already unambiguous. Sending everything as CSI-u is
    /// bit `0x8`, which roost does not claim — so it must not behave as if
    /// it had.
    #[test]
    fn a_negotiated_pane_keeps_legacy_for_what_disambiguate_does_not_cover() {
        let keep = |code, mods, bytes: &[u8]| {
            assert_eq!(kitty_upgrade(KeyEvent::new(code, mods), bytes.to_vec(), true), bytes);
        };
        keep(KeyCode::Char('a'), KeyModifiers::NONE, b"a");
        keep(KeyCode::Char('A'), KeyModifiers::SHIFT, b"A");
        keep(KeyCode::Right, KeyModifiers::CONTROL, b"\x1b[1;5C");
        keep(KeyCode::Up, KeyModifiers::NONE, b"\x1b[A");
        keep(KeyCode::Tab, KeyModifiers::NONE, b"\t");
        keep(KeyCode::Backspace, KeyModifiers::NONE, &[0x7f]);
        keep(KeyCode::Enter, KeyModifiers::NONE, b"\r");
        keep(KeyCode::F(5), KeyModifiers::NONE, b"\x1b[15~");
    }

    // -- C23 raw pass-through: encode_raw -------------------------------

    #[test]
    fn encode_raw_leaves_non_alt_keys_exactly_as_encode_key_would() {
        assert_eq!(encode_raw(plain(KeyCode::Char('a'))), b"a");
        assert_eq!(encode_raw(plain(KeyCode::Enter)), b"\r");
        assert_eq!(encode_raw(plain(KeyCode::Up)), b"\x1b[A");
        assert_eq!(
            encode_raw(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            vec![0x03]
        );
    }

    #[test]
    fn encode_raw_table_covers_every_current_action_chord_with_meta_esc() {
        // Alt+q -> 1b 71, etc.: every currently-bound Alt chord (the toggle
        // itself excluded — that one never reaches encode_raw, see main.rs)
        // forwards as ESC + its unmodified encoding.
        let cases: &[(KeyEvent, &[u8])] = &[
            (alt(KeyCode::Char('q')), &[0x1b, b'q']),
            (alt(KeyCode::Char('n')), &[0x1b, b'n']),
            (alt(KeyCode::Char('w')), &[0x1b, b'w']),
            (alt(KeyCode::Char('t')), &[0x1b, b't']),
            (alt(KeyCode::Char('s')), &[0x1b, b's']),
            (alt(KeyCode::Char('o')), &[0x1b, b'o']),
            (alt(KeyCode::Char('r')), &[0x1b, b'r']),
            (alt(KeyCode::Char('u')), &[0x1b, b'u']),
            (alt(KeyCode::Char('a')), &[0x1b, b'a']),
            (alt(KeyCode::Char('z')), &[0x1b, b'z']),
            (alt(KeyCode::Char('g')), &[0x1b, b'g']),
            (alt(KeyCode::Char('e')), &[0x1b, b'e']),
            (alt(KeyCode::Char('f')), &[0x1b, b'f']),
            (alt(KeyCode::Char('c')), &[0x1b, b'c']),
            (alt(KeyCode::Char('b')), &[0x1b, b'b']), // free today, still forwards
            (alt(KeyCode::Enter), &[0x1b, 0x0d]),
            (alt(KeyCode::PageUp), &[0x1b, 0x1b, b'[', b'5', b'~']),
        ];
        for (key, expected) in cases {
            assert_eq!(&encode_raw(*key), expected, "key {key:?}");
        }
    }

    #[test]
    fn encode_raw_strips_only_the_alt_bit_shift_and_ctrl_still_apply() {
        // Alt+Shift+r delivered with an explicit shift flag: the meta-ESC
        // byte is prefixed onto whatever encode_key produces for Shift+r
        // (Shift is not consulted for a plain Char, same as encode_key's
        // existing behavior — this pins that encode_raw doesn't invent new
        // shift-handling of its own).
        assert_eq!(encode_raw(alt_shift(KeyCode::Char('r'))), vec![0x1b, b'r']);
        // The uppercase-delivery variant (Alt+'R', no shift bit) forwards
        // the uppercase byte.
        assert_eq!(encode_raw(alt(KeyCode::Char('R'))), vec![0x1b, b'R']);
    }

    // -- P12: Ctrl+char encodes only real C0 mappings ---------------------

    #[test]
    fn ctrl_chars_encode_only_real_c0_mappings() {
        let ctrl = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        // The mappings every terminal agrees on.
        let cases: &[(char, u8)] = &[
            ('a', 0x01),
            ('c', 0x03),
            ('z', 0x1a),
            ('A', 0x01), // shift/uppercase delivery still lands on the letter's C0
            ('@', 0x00),
            (' ', 0x00), // Ctrl+Space ≡ Ctrl+@ → NUL (emacs set-mark)
            ('[', 0x1b), // Ctrl+[ IS ESC — must survive the gate
            ('\\', 0x1c),
            (']', 0x1d),
            ('^', 0x1e),
            ('_', 0x1f),
            ('?', 0x7f), // Ctrl+? → DEL, xterm convention
            ('-', 0x1f), // P12: was 0x0D (CR — accidental submit!)
            ('/', 0x1f), // P12: was 0x0F (readline operate-and-get-next)
        ];
        for (c, want) in cases {
            match translate(ctrl(*c)) {
                InputResult::Forward(b) => assert_eq!(b, vec![*want], "Ctrl+{c:?}"),
                _ => panic!("Ctrl+{c:?} must forward"),
            }
        }
        // No C0 identity → nothing, never a wrong control byte.
        for c in ['1', '2', '9', '0', '.', ',', ';', '\'', '=', '`', '~', '!'] {
            assert!(
                matches!(translate(ctrl(c)), InputResult::Ignore),
                "Ctrl+{c:?} must forward nothing"
            );
        }
    }

    // -- C29: a SUPER-modified char is swallowed, never forwarded ----------

    /// Safety fix alongside C29 (native selection): most emulators keep ⌘ for
    /// their own bindings so this never fires in practice, but if a ⌘-chord
    /// ever does reach roost, it must not leak the base char into whatever
    /// pane is focused (a `⌘C` that missed the emulator's own copy binding
    /// typing a bare `c` into an agent's prompt).
    #[test]
    fn super_modified_chars_are_swallowed_not_forwarded() {
        let cmd_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER);
        assert!(matches!(translate(cmd_c), InputResult::Ignore));
        // Composed with Ctrl too: must not fall into the C0 ctrl-byte path.
        let ctrl_cmd_c =
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER | KeyModifiers::CONTROL);
        assert!(matches!(translate(ctrl_cmd_c), InputResult::Ignore));
        // The C23 raw-mode path shares the guard (it delegates to the same
        // `encode_key`), so a raw-focused pane is equally protected.
        assert_eq!(encode_raw(cmd_c), Vec::<u8>::new());
        // A plain, unmodified char is unaffected.
        assert!(matches!(translate(plain(KeyCode::Char('c'))), InputResult::Forward(_)));
    }

    // -- P13: Ctrl+Alt is never a chord -----------------------------------

    #[test]
    fn ctrl_alt_forwards_meta_esc_ctrl_bytes_instead_of_chord_actions() {
        let ctrl_alt =
            |code: KeyCode| KeyEvent::new(code, KeyModifiers::CONTROL | KeyModifiers::ALT);
        // The destructive collision from the spec: C-M-w must never close a
        // pane; C-M-f (emacs forward-word) must never toggle the float.
        let cases: &[(KeyCode, &[u8])] = &[
            (KeyCode::Char('w'), &[0x1b, 0x17]),
            (KeyCode::Char('f'), &[0x1b, 0x06]),
            (KeyCode::Char('q'), &[0x1b, 0x11]), // and never quit
            (KeyCode::Char('-'), &[0x1b, 0x1f]), // composes with the P12 gate
            (KeyCode::Right, b"\x1b\x1b[1;5C"),  // meta-ESC + ctrl CSI form
        ];
        for (code, want) in cases {
            match translate(ctrl_alt(*code)) {
                InputResult::Forward(b) => assert_eq!(&b, want, "{code:?}"),
                InputResult::Action(a) => {
                    panic!("Ctrl+Alt+{code:?} matched chord action {a:?}")
                }
                InputResult::Ignore => panic!("Ctrl+Alt+{code:?} swallowed"),
            }
        }
        // A Ctrl+Alt combo with no base encoding forwards nothing — but
        // still never an action.
        assert!(matches!(translate(ctrl_alt(KeyCode::Char('9'))), InputResult::Ignore));
    }

    // -- U5 (SPEC-ux): unbound plain-Alt printables forward ---------------

    #[test]
    fn unbound_alt_printables_forward_as_meta_esc() {
        // The documented "left free for readline" chords, plus a symbol.
        let cases: &[(char, &[u8])] = &[
            ('b', &[0x1b, b'b']), // readline backward-word — the live-QA probe
            ('d', &[0x1b, b'd']), // readline kill-word
            ('.', &[0x1b, b'.']), // readline yank-last-arg
            (',', &[0x1b, b',']), // its neighbour: still free after Alt+< took the glyph
            ('_', &[0x1b, b'_']), // the shifted spelling of Alt+- was never claimed
            ('p', &[0x1b, b'p']),
            ('x', &[0x1b, b'x']),
            ('v', &[0x1b, b'v']), // still free after U7 took i/m/0
        ];
        for (c, want) in cases {
            match translate(alt(KeyCode::Char(*c))) {
                InputResult::Forward(b) => assert_eq!(&b, want, "Alt+{c:?}"),
                _ => panic!("Alt+{c:?} must forward as meta-ESC, not be swallowed"),
            }
        }
        // Unmatched Alt+non-printables keep the old swallow — U5's contract
        // is printables.
        assert!(matches!(translate(alt(KeyCode::Backspace)), InputResult::Ignore));
        assert!(matches!(translate(alt(KeyCode::F(5))), InputResult::Ignore));
    }

    /// P13/U5 must not disturb a single bound chord: every entry in the
    /// action table still maps to its action with plain ALT (and its
    /// shifted variants where those are part of the binding).
    #[test]
    fn every_bound_chord_still_maps_to_its_action() {
        let bound: &[(KeyEvent, Action)] = &[
            (alt(KeyCode::Char('q')), Action::Quit),
            (alt(KeyCode::Char('n')), Action::NewPane),
            (alt(KeyCode::Char('w')), Action::ClosePane),
            (alt(KeyCode::Char('t')), Action::NewTab),
            (alt(KeyCode::Char('s')), Action::StackPane),
            (alt_shift(KeyCode::Char('s')), Action::ExplodeStack),
            (alt(KeyCode::Char('S')), Action::ExplodeStack),
            (alt(KeyCode::Char('o')), Action::FlipSplit),
            (alt(KeyCode::Char('r')), Action::EditPane),
            (alt_shift(KeyCode::Char('r')), Action::RenameTab),
            (alt(KeyCode::Char('R')), Action::RenameTab),
            (alt_shift(KeyCode::Char('p')), Action::ToggleRaw),
            (alt(KeyCode::Char('P')), Action::ToggleRaw),
            (alt(KeyCode::Enter), Action::QuickLaunch),
            (alt(KeyCode::Char('/')), Action::ToggleHints),
            (alt(KeyCode::Char('c')), Action::CopyMode),
            (alt(KeyCode::Char('u')), Action::Undo),
            (alt(KeyCode::Char('?')), Action::Help),
            // The other half of the same physical chord: a terminal that
            // reports Alt+? as `/`+SHIFT must reach Help, not the hint bar.
            (alt_shift(KeyCode::Char('/')), Action::Help),
            (alt(KeyCode::Char('a')), Action::JumpAttention),
            (alt(KeyCode::Char('z')), Action::ToggleZoom),
            (alt(KeyCode::Char('g')), Action::CycleLayout { forward: true }),
            (alt(KeyCode::Char('e')), Action::ToggleFeed),
            (alt(KeyCode::Char('f')), Action::ToggleFloat),
            (alt(KeyCode::PageUp), Action::ScrollMode),
            (alt(KeyCode::Char('3')), Action::GoToTab(2)),
            (alt(KeyCode::Char('0')), Action::LastTab), // U7
            (alt(KeyCode::Char('i')), Action::MovePaneToTab { forward: true }),
            (alt_shift(KeyCode::Char('i')), Action::MovePaneToTab { forward: false }),
            (alt(KeyCode::Char('m')), Action::NextTab),
            (alt_shift(KeyCode::Char('m')), Action::PrevTab),
            (alt(KeyCode::Right), Action::Focus(Dir::Right)),
            (alt(KeyCode::Char('h')), Action::Focus(Dir::Left)),
            (alt(KeyCode::Char('j')), Action::Focus(Dir::Down)),
            (alt(KeyCode::Char('k')), Action::Focus(Dir::Up)),
            (alt_shift(KeyCode::Right), Action::MovePane(Dir::Right)),
            (alt_shift(KeyCode::Up), Action::MovePane(Dir::Up)),
            (alt(KeyCode::Char('-')), Action::Resize { horizontal: false, grow: false }),
            (alt(KeyCode::Char('=')), Action::Resize { horizontal: false, grow: true }),
            (alt(KeyCode::Char('<')), Action::Resize { horizontal: true, grow: false }),
            (alt_shift(KeyCode::Char(',')), Action::Resize { horizontal: true, grow: false }),
            (alt(KeyCode::Char('>')), Action::Resize { horizontal: true, grow: true }),
            (alt_shift(KeyCode::Char('.')), Action::Resize { horizontal: true, grow: true }),
        ];
        for (key, want) in bound {
            match translate(*key) {
                InputResult::Action(a) => assert_eq!(a, *want, "{key:?}"),
                _ => panic!("bound chord {key:?} no longer maps to {want:?}"),
            }
            // ...and adding CONTROL takes every one of them out of the
            // chord table (P13), without exception.
            let mut with_ctrl = *key;
            with_ctrl.modifiers |= KeyModifiers::CONTROL;
            assert!(
                !matches!(translate(with_ctrl), InputResult::Action(_)),
                "{key:?} + CONTROL must not chord"
            );
        }
    }

    #[test]
    fn encode_raw_key_release_events_forward_nothing() {
        let mut key = alt(KeyCode::Char('q'));
        key.kind = KeyEventKind::Release;
        // encode_raw itself doesn't filter releases (that happens earlier,
        // in the event loop, same as translate()) — but it must not panic
        // and it produces the same bytes a Press would, since KeyEventKind
        // isn't part of encode_key's match. Documented via this pin so a
        // future refactor notices if that assumption ever changes.
        assert_eq!(encode_raw(key), vec![0x1b, b'q']);
    }

    // -- config.json: the key-bindings escape hatch -------------------------

    /// Absent config.json ⇒ `Keymap::default()` (empty), which must make
    /// `translate_with` byte-identical to `translate` — asserted against
    /// `default_keymap()` itself (the existing default map `translate`
    /// already runs), not a hand-copied literal of what today's bindings
    /// are, plus a sample of everything a chord table doesn't cover.
    #[test]
    fn absent_config_keeps_translate_byte_for_byte() {
        let empty = Keymap::default();
        for &chord in default_keymap().keys() {
            let mut mods = KeyModifiers::ALT;
            if chord.shift {
                mods |= KeyModifiers::SHIFT;
            }
            let key = KeyEvent::new(chord.code, mods);
            assert_eq!(translate_with(key, &empty), translate(key), "chord {chord:?}");
        }
        let extra = [
            alt(KeyCode::Char('b')),   // unbound Alt printable (readline M-b)
            alt(KeyCode::Char('d')),   // unbound Alt printable (readline M-d)
            plain(KeyCode::Char('a')), // no Alt at all
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL | KeyModifiers::ALT), // P13
        ];
        for key in extra {
            assert_eq!(translate_with(key, &empty), translate(key), "{key:?}");
        }
    }

    /// `"disable"` ⇒ the chord produces no Action and forwards to the pane
    /// exactly like an unbound key does today (the readline fix itself).
    #[test]
    fn disable_forwards_the_chord_instead_of_its_default_action() {
        let (keymap, diagnostics) =
            Keymap::parse(r#"{"keys": {"alt+f": "disable"}}"#, "config.json");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        // Without the override, alt+f is its documented default: ToggleFloat.
        assert!(matches!(
            translate(alt(KeyCode::Char('f'))),
            InputResult::Action(Action::ToggleFloat)
        ));
        match translate_with(alt(KeyCode::Char('f')), &keymap) {
            InputResult::Forward(b) => assert_eq!(b, vec![0x1b, b'f']),
            other => panic!("disabled alt+f must forward to the pane, got {other:?}"),
        }
    }

    /// Remap ⇒ the new chord triggers the action, and the old one — only
    /// disabled here, not itself remapped — no longer does. This is the
    /// design doc's own worked example: move ToggleFloat off alt+f (the
    /// readline collision) onto a free chord instead.
    #[test]
    fn remap_moves_an_action_off_its_default_chord_onto_a_new_one() {
        let json = r#"{"keys": {"alt+f": "disable", "alt+v": "toggle_float"}}"#;
        let (keymap, diagnostics) = Keymap::parse(json, "config.json");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(matches!(
            translate_with(alt(KeyCode::Char('v')), &keymap),
            InputResult::Action(Action::ToggleFloat)
        ));
        assert!(!matches!(
            translate_with(alt(KeyCode::Char('f')), &keymap),
            InputResult::Action(Action::ToggleFloat)
        ));
    }

    /// The "unless it was also remapped" half: a chord reassigned to a
    /// *different* action (rather than disabled) naturally stops producing
    /// its old one too — replaced, not disabled.
    #[test]
    fn a_chord_remapped_to_a_different_action_stops_producing_its_old_one() {
        let (keymap, diagnostics) = Keymap::parse(r#"{"keys": {"alt+g": "quit"}}"#, "config.json");
        assert!(diagnostics.problems.is_empty(), "a valid remap is not a problem: {diagnostics:?}");
        assert!(matches!(
            translate_with(alt(KeyCode::Char('g')), &keymap),
            InputResult::Action(Action::Quit)
        ));
        assert!(!matches!(
            translate_with(alt(KeyCode::Char('g')), &keymap),
            InputResult::Action(Action::CycleLayout { forward: true })
        ));
    }

    /// A bind that takes a chord away from its default action names the
    /// displaced default — informational only, never a rejection (the
    /// binding applies). A chord with no default binding stays silent:
    /// nothing was displaced.
    #[test]
    fn a_displacing_bind_names_the_default_it_replaces_and_binds_anyway() {
        let (keymap, diagnostics) =
            Keymap::parse(r#"{"keys": {"alt+w": "new_pane"}}"#, "config.json");
        // A notice, never a problem — nothing was skipped, and `roost keys`
        // must still exit 0 for this file.
        assert_eq!(
            diagnostics.notices,
            vec![r#"config.json: "alt+w": replaces default close_pane — which now has no chord"#
                .to_string()],
        );
        assert!(diagnostics.problems.is_empty(), "{diagnostics:?}");
        assert!(matches!(
            translate_with(alt(KeyCode::Char('w')), &keymap),
            InputResult::Action(Action::NewPane)
        ));

        let (_, diagnostics) = Keymap::parse(r#"{"keys": {"alt+x": "quit"}}"#, "config.json");
        assert!(diagnostics.is_empty(), "alt+x has no default to displace: {diagnostics:?}");
    }

    /// The notice is about an action losing its **last** chord, not about
    /// displacement as such — so a deliberate swap, where both actions keep
    /// one, says nothing at all. This was the false positive that made the
    /// warning noisy on exactly the configs an advanced user writes.
    #[test]
    fn a_swap_displaces_two_defaults_and_warns_about_neither() {
        let (keymap, diagnostics) = Keymap::parse(
            r#"{"keys": {"alt+w": "new_pane", "alt+n": "close_pane"}}"#,
            "config.json",
        );
        assert!(diagnostics.is_empty(), "a swap orphans nothing: {diagnostics:?}");
        assert!(matches!(
            translate_with(alt(KeyCode::Char('w')), &keymap),
            InputResult::Action(Action::NewPane)
        ));
        assert!(matches!(
            translate_with(alt(KeyCode::Char('n')), &keymap),
            InputResult::Action(Action::ClosePane)
        ));
    }

    /// ...and the notice still fires when the displaced action really is
    /// left unreachable, so the test above cannot be satisfied by simply
    /// never warning.
    #[test]
    fn displacing_an_actions_only_chord_still_says_so() {
        let (_, diagnostics) = Keymap::parse(r#"{"keys": {"alt+t": "quit"}}"#, "config.json");
        assert!(diagnostics.problems.is_empty(), "{diagnostics:?}");
        assert_eq!(diagnostics.notices.len(), 1, "{diagnostics:?}");
        assert!(
            diagnostics.notices[0].contains("replaces default new_tab"),
            "it names the action that lost its chord: {diagnostics:?}",
        );
    }

    /// A chord listed twice keeps only its last value (README) — inherited
    /// for free from `serde_json`, which already collapses a repeated JSON
    /// key to its last-written one.
    #[test]
    fn a_chord_listed_twice_keeps_only_the_last_value() {
        let json = r#"{"keys": {"alt+g": "quit", "alt+g": "toggle_zoom"}}"#;
        let (keymap, diagnostics) = Keymap::parse(json, "config.json");
        assert!(diagnostics.problems.is_empty(), "a valid remap is not a problem: {diagnostics:?}");
        assert!(matches!(
            translate_with(alt(KeyCode::Char('g')), &keymap),
            InputResult::Action(Action::ToggleZoom)
        ));
    }

    /// Malformed JSON never blocks startup: defaults stay intact, and the
    /// single diagnostic names the file.
    #[test]
    fn malformed_json_keeps_defaults_and_names_the_file() {
        let (keymap, diagnostics) = Keymap::parse("{ not json", "config.json");
        assert_eq!(diagnostics.problems.len(), 1, "{diagnostics:?}");
        assert!(diagnostics.problems[0].contains("config.json"), "{diagnostics:?}");
        assert!(matches!(
            translate_with(alt(KeyCode::Char('q')), &keymap),
            InputResult::Action(Action::Quit)
        ));
    }

    /// An unknown action name is skipped, not fatal, and named in the
    /// diagnostic alongside the chord it was attached to; that chord keeps
    /// its default.
    #[test]
    fn unknown_action_name_is_skipped_and_named_in_the_diagnostic() {
        let json = r#"{"keys": {"alt+z": "not_a_real_action"}}"#;
        let (keymap, diagnostics) = Keymap::parse(json, "config.json");
        assert_eq!(diagnostics.problems.len(), 1, "{diagnostics:?}");
        assert!(diagnostics.problems[0].contains("not_a_real_action"), "{diagnostics:?}");
        assert!(diagnostics.problems[0].contains("alt+z"), "{diagnostics:?}");
        assert!(matches!(
            translate_with(alt(KeyCode::Char('z')), &keymap),
            InputResult::Action(Action::ToggleZoom)
        ));
    }

    /// An unparseable chord (no `ctrl+` grammar at all — P13 keeps Ctrl+Alt
    /// out of the chord table entirely) is skipped, not fatal, and named.
    #[test]
    fn unrecognized_chord_is_skipped_and_named_in_the_diagnostic() {
        let json = r#"{"keys": {"ctrl+f": "quit"}}"#;
        let (_keymap, diagnostics) = Keymap::parse(json, "config.json");
        assert_eq!(diagnostics.problems.len(), 1, "{diagnostics:?}");
        assert!(diagnostics.problems[0].contains("ctrl+f"), "{diagnostics:?}");
    }

    /// The Action-name mapping is exhaustive: `action_name`'s match has no
    /// wildcard arm (a new `Action` variant left unnamed is a compile
    /// error), and this pins that every name it actually produces — for
    /// every action any default chord binds — round-trips back through
    /// `action_by_name`, so a future variant is a compile error *or* this
    /// test failing, never a silent gap either way.
    #[test]
    fn every_default_bound_actions_name_round_trips() {
        for (&chord, &action) in default_keymap() {
            let name = action_name(&action);
            assert_eq!(
                action_by_name(&name),
                Some(action),
                "chord {chord:?}'s action {action:?} named {name:?} didn't round-trip"
            );
        }
    }

    /// C33, the regression this chord family exists to close. Before it,
    /// `Alt+Shift+h` resolved **two different ways depending on the
    /// terminal**: one delivering `('h', SHIFT)` fell into the unguarded
    /// focus arm and moved focus left (a duplicate of plain `Alt+h`), while
    /// one delivering `'H'` matched nothing and forwarded meta-H into the
    /// agent. Both spellings now mean the same thing, for all four letters.
    #[test]
    fn the_shifted_vim_letters_move_the_pane_on_both_delivery_forms() {
        for (lower, upper, dir) in [
            ('h', 'H', Dir::Left),
            ('j', 'J', Dir::Down),
            ('k', 'K', Dir::Up),
            ('l', 'L', Dir::Right),
        ] {
            assert_eq!(
                translate(alt_shift(KeyCode::Char(lower))),
                InputResult::Action(Action::MovePane(dir)),
                "Alt+Shift+{lower} on a terminal that sends the shift bit",
            );
            assert_eq!(
                translate(alt(KeyCode::Char(upper))),
                InputResult::Action(Action::MovePane(dir)),
                "Alt+Shift+{lower} on a terminal that sends {upper}",
            );
        }
    }

    /// C37: the reverse cycle resolves on both delivery forms, like every
    /// other shifted letter. Noted by the C37 audit as the one thing no
    /// input-layer test asserted — the render row and the app behaviour were
    /// covered, but not the translation, which is exactly the layer C33's
    /// cross-terminal split lived in.
    #[test]
    fn the_shifted_g_reverses_the_cycle_on_both_delivery_forms() {
        assert_eq!(
            translate(alt(KeyCode::Char('g'))),
            InputResult::Action(Action::CycleLayout { forward: true }),
        );
        assert_eq!(
            translate(alt_shift(KeyCode::Char('g'))),
            InputResult::Action(Action::CycleLayout { forward: false }),
            "the shift bit reverses it",
        );
        assert_eq!(
            translate(alt(KeyCode::Char('G'))),
            InputResult::Action(Action::CycleLayout { forward: false }),
            "and so does a terminal that sends bare uppercase",
        );
    }

    /// The unshifted half is untouched — C33 took the *shifted* letters,
    /// which were a redundant second spelling of exactly this.
    #[test]
    fn the_unshifted_vim_letters_still_move_focus() {
        for (c, dir) in [('h', Dir::Left), ('j', Dir::Down), ('k', Dir::Up), ('l', Dir::Right)] {
            assert_eq!(translate(alt(KeyCode::Char(c))), InputResult::Action(Action::Focus(dir)),);
        }
    }

    /// C33's payoff for the escape hatch: because both delivery forms now
    /// default to the same action, `twins` pairs them on its existing rule
    /// ("same case-folded letter and both already default to the same
    /// action") with no change to `twins` itself. Before C33 a remap of
    /// `alt+shift+h` bound only the `('h', SHIFT)` form and silently
    /// no-opped on terminals sending `'H'` — the exact failure `twins`'
    /// own doc comment says must never happen.
    #[test]
    fn remapping_a_shifted_vim_letter_reaches_both_delivery_forms() {
        let (keymap, diagnostics) =
            Keymap::parse(r#"{"keys": {"alt+shift+h": "quit"}}"#, "config.json");
        assert!(diagnostics.problems.is_empty(), "a valid remap is not a problem: {diagnostics:?}");
        assert_eq!(
            translate_with(alt_shift(KeyCode::Char('h')), &keymap),
            InputResult::Action(Action::Quit),
        );
        assert_eq!(
            translate_with(alt(KeyCode::Char('H')), &keymap),
            InputResult::Action(Action::Quit),
            "the uppercase delivery must move too, or the remap half-applies",
        );
    }

    // ---- C34: no surface may spell a chord it did not resolve -----------

    /// The literals a chord spelling is allowed to appear as in production
    /// code, each because it is **handed to a resolver as a default**
    /// rather than printed as-is.
    ///
    /// Why an allowlist rather than a ban: the two authoring sites below are
    /// legitimate and permanent, so a flat ban would need suppressions that
    /// mean nothing. Exact-match on the *whole* literal is what gives this
    /// gate its teeth — a bare `"Alt+w"` is what a resolver argument looks
    /// like, while a chord embedded in a sentence
    /// (`"last pane — Alt+w again to quit roost"`) is what a hard-coded UI
    /// string looks like, and only the first form can be listed here.
    /// Every C34 deviation the design audit found was of the second form.
    const CHORD_LITERALS: &[&str] = &[
        // `Chord::label`, the producer every other surface resolves through.
        "Alt+Shift+{key}",
        "Alt+{key}",
        // C9 hint-bar defaults, passed to `alt(..)` — the bar prints what
        // the keymap returns and falls back to these only when they are
        // still accurate.
        "Alt+?",
        "Alt+n",
        "Alt+↵",
        "Alt+s",
        "Alt+←↓↑→",
        "Alt+w",
        "Alt+r",
        "Alt+q",
        "Alt+Shift+p",
        // C40's standing pull pair, same shape as the seven above.
        "Alt+Shift+v",
        // C15 `HelpKey::Family` shorthands, which give way to enumeration
        // the moment any member moves.
        "Alt+←↓↑→ / hjkl",
        "Alt+Shift+←↓↑→ / hjkl",
        "Alt+- / Alt+=",
        "Alt+< / Alt+>",
        "Alt+1..9 / Alt+0",
        "Alt+m / Alt+Shift+m",
        "Alt+i / Alt+Shift+i",
        // The one authored `HelpKey::Text` row that names a chord: a mouse
        // chord, outside config.json's grammar, so it cannot move (C34).
        "Alt+click / o",
    ];

    /// C34's rule, made mechanical: *any chrome that names a chord derives
    /// that name from the live keymap.*
    ///
    /// Two runtime gates already check that the help overlay covers every
    /// bound chord, but neither can see a chord spelled into an arbitrary
    /// `format!` somewhere else in the tree — which is exactly where the
    /// design audit found C34's rule broken in five places (C10's confirm
    /// flashes) plus two more surfaces. Each was caught by a human reading
    /// code. This is the check that would have caught all seven.
    ///
    /// A new spelling fails here until it is either resolved or added to
    /// `CHORD_LITERALS` — and adding it is the moment its author has to ask
    /// whether the surface should be deriving instead.
    ///
    /// Not a proof: `format!("press {} now", "Alt+w")` would slip through.
    /// It is a guardrail against the accident, not a defence against
    /// deliberate circumvention.
    #[test]
    fn no_surface_spells_a_chord_it_did_not_resolve() {
        use crate::ui::srcscan::{is_comment, production, src_files, string_literals};
        let mut offenders = Vec::new();
        for (path, text) in src_files() {
            for (i, line) in production(&text).lines().enumerate() {
                if is_comment(line) {
                    continue;
                }
                for lit in string_literals(line) {
                    if lit.contains("Alt+") && !CHORD_LITERALS.contains(&lit) {
                        offenders.push(format!("{}:{}: {lit:?}", path.display(), i + 1));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "C34: a chord spelled into a literal instead of resolved from the keymap.\n\
             Resolve it (`input::chord_for` / `App::chord_label`), or add the literal to \
             CHORD_LITERALS if it is a resolver default:\n{}",
            offenders.join("\n"),
        );
    }

    /// The gate's own teeth, since a passing scan proves nothing about what
    /// it would reject: the shapes C34's audit actually found must fail it.
    #[test]
    fn the_chord_gate_rejects_a_chord_buried_in_a_sentence() {
        let caught = |lit: &str| lit.contains("Alt+") && !CHORD_LITERALS.contains(&lit);
        // The five C10 flashes, verbatim from before they were resolved.
        assert!(caught("last pane — Alt+w again to quit roost"));
        assert!(caught("{} busy — Alt+w again to close"));
        assert!(caught("{busy} {noun} busy — Alt+q again to quit"));
        assert!(caught("no room to rearrange; stack a pane with Alt+s first"));
        // C9's attention segment and C16's dead-pane bar (D2, D3).
        assert!(caught("◆ {n} needs you · Alt+a"));
        assert!(caught(" ✕ exited — Enter: relaunch/resume · Alt+w: close "));
        // And a resolver default still passes, or the gate would be unusable.
        assert!(!caught("Alt+w"));
    }

    // ---- F1: effective_bindings -----------------------------------------

    /// The property the whole of F1 rests on: what `effective_bindings`
    /// reports **is** what `translate_with` dispatches on. Swept over every
    /// chord it returns rather than spot-checked, so a binding that renders
    /// one way and fires another is a failure, not a gap in the test.
    #[test]
    fn every_reported_binding_is_what_the_chord_actually_does() {
        let keymap = Keymap::default();
        for (label, action) in effective_bindings(&keymap) {
            let chord = parse_label(&label);
            assert_eq!(
                translate_with(chord, &keymap),
                InputResult::Action(action),
                "{label} is reported as {action:?} but does something else",
            );
        }
    }

    /// Rebuild a `KeyEvent` from a label, so the sweep above can press what
    /// it read. The inverse of `Chord::label` for the forms that table
    /// produces.
    fn parse_label(label: &str) -> KeyEvent {
        let rest = label.strip_prefix("Alt+").expect("every label is an Alt chord");
        let (shift, key) = match rest.strip_prefix("Shift+") {
            Some(k) => (true, k),
            None => (false, rest),
        };
        let code = match key {
            "Enter" => KeyCode::Enter,
            "PgUp" => KeyCode::PageUp,
            "↑" => KeyCode::Up,
            "↓" => KeyCode::Down,
            "←" => KeyCode::Left,
            "→" => KeyCode::Right,
            k => KeyCode::Char(k.chars().next().unwrap()),
        };
        let mut m = KeyModifiers::ALT;
        if shift {
            m |= KeyModifiers::SHIFT;
        }
        KeyEvent::new(code, m)
    }

    /// Each collapse rule, named. Without them the overlay would print one
    /// physical key twice (rules 1 and 2) or advertise a chord nothing is
    /// bound to (rule 3).
    #[test]
    fn one_physical_chord_is_reported_exactly_once() {
        let labels: Vec<String> =
            effective_bindings(&Keymap::default()).into_iter().map(|(l, _)| l).collect();
        let has = |l: &str| labels.iter().any(|x| x == l);

        // Rule 1 — letters keep the Alt+Shift+<lower> spelling.
        assert!(has("Alt+Shift+p"), "the documented spelling survives");
        assert!(!has("Alt+P"), "its uppercase-delivery twin does not");
        // Rule 2 — punctuation keeps the glyph spelling, the mirror choice.
        assert!(has("Alt+?"), "the documented spelling survives");
        assert!(!has("Alt+Shift+/"), "its shifted-base twin does not");
        // Rule 3 — an arm that never tested shift binds no shifted chord.
        assert!(has("Alt+1"));
        assert!(!has("Alt+Shift+1"), "nothing is bound to Alt+Shift+1");
        // ... and where the two shift states really differ, both survive.
        assert!(has("Alt+h") && has("Alt+Shift+h"), "focus and move are distinct");
        assert!(has("Alt+←") && has("Alt+Shift+←"), "focus and pane-move are distinct");
        // Rule 4 — the shiftless glyph of a bound base key keeps the base
        // spelling: `Alt+=` is the resize chord, bare `Alt++` its delivery
        // twin, and the shifted spellings bind no row of their own.
        assert!(has("Alt+=") && has("Alt+<"));
        assert!(!has("Alt++"), "the + glyph collapses into Alt+=");
        assert!(!has("Alt+Shift+=") && !has("Alt+Shift+,") && !has("Alt+Shift+<"));

        let mut sorted = labels.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), labels.len(), "no label appears twice");
    }

    /// The point of the exercise: a remap moves what the chrome will draw,
    /// and a disable removes it. Before F1 the help overlay and hint bar
    /// spelled their chords as `&'static str`, so neither could.
    #[test]
    fn a_remap_moves_the_reported_binding_and_a_disable_removes_it() {
        let (keymap, diagnostics) = Keymap::parse(
            r#"{"keys": {"alt+f": "disable", "alt+v": "toggle_float"}}"#,
            "config.json",
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let bindings = effective_bindings(&keymap);
        let float: Vec<&String> =
            bindings.iter().filter(|(_, a)| *a == Action::ToggleFloat).map(|(l, _)| l).collect();
        assert_eq!(float, vec!["Alt+v"], "the float is on Alt+v now, and only there");
        assert!(
            !bindings.iter().any(|(l, _)| l == "Alt+f"),
            "the disabled chord is gone entirely — it forwards to the pane now",
        );
    }

    /// The literal `"disable"` keyword is never itself a valid action name,
    /// and a nonsense name resolves to nothing — both are how
    /// `Keymap::parse` tells the two apart.
    #[test]
    fn disable_keyword_and_nonsense_names_are_not_actions() {
        assert_eq!(action_by_name("disable"), None);
        assert_eq!(action_by_name("nope"), None);
    }

    /// (a) Disabling a twinned chord disables EVERY delivery form a
    /// terminal might use for it, not just the one the entry named. p/P is
    /// the design doc's own example: some terminals send Alt+Shift+p as
    /// `('p', SHIFT)`, others as `('P', no SHIFT)` — both must go dead, or
    /// the escape hatch silently fails on whichever terminal sends the
    /// other one.
    #[test]
    fn disabling_a_twinned_chord_disables_every_delivery_form() {
        let (keymap, diagnostics) =
            Keymap::parse(r#"{"keys": {"alt+shift+p": "disable"}}"#, "config.json");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        // Without the override, both forms are ToggleRaw (the
        // uppercase-delivery tolerance already in the default table).
        assert!(matches!(
            translate(alt_shift(KeyCode::Char('p'))),
            InputResult::Action(Action::ToggleRaw)
        ));
        assert!(matches!(
            translate(alt(KeyCode::Char('P'))),
            InputResult::Action(Action::ToggleRaw)
        ));
        // Disabled: neither form produces an Action anymore — both forward.
        assert!(!matches!(
            translate_with(alt_shift(KeyCode::Char('p')), &keymap),
            InputResult::Action(_)
        ));
        assert!(!matches!(
            translate_with(alt(KeyCode::Char('P')), &keymap),
            InputResult::Action(_)
        ));
    }

    /// (a') The glyph/base delivery pairs close the same way: `Alt+<` is
    /// delivered by some terminals as `,`+SHIFT, `Alt+=` as bare `+` or
    /// `=`+SHIFT — and `Alt+?` as `/`+SHIFT, the pre-existing member of
    /// this family whose gap this twin closure closed (a `{"alt+?":
    /// "disable"}` used to leave the `('/')+SHIFT` delivery live).
    #[test]
    fn disabling_a_glyph_chord_disables_every_delivery_form() {
        let (keymap, diagnostics) = Keymap::parse(
            r#"{"keys": {"alt+<": "disable", "alt+=": "disable", "alt+?": "disable"}}"#,
            "config.json",
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        for key in [
            alt(KeyCode::Char('<')),
            alt_shift(KeyCode::Char('<')),
            alt_shift(KeyCode::Char(',')),
            alt(KeyCode::Char('=')),
            alt_shift(KeyCode::Char('=')),
            alt(KeyCode::Char('+')),
            alt_shift(KeyCode::Char('+')),
            alt(KeyCode::Char('?')),
            alt_shift(KeyCode::Char('?')),
            alt_shift(KeyCode::Char('/')),
        ] {
            assert!(
                !matches!(translate_with(key, &keymap), InputResult::Action(_)),
                "{key:?} must be dead — every delivery form of a disabled chord goes with it",
            );
        }
        // The chords on the *other* half of those keys stay alive: Alt+,
        // (M-,) forwards and Alt+/ still toggles the hint bar.
        assert!(matches!(
            translate_with(alt(KeyCode::Char(',')), &keymap),
            InputResult::Forward(_)
        ));
        assert!(matches!(
            translate_with(alt(KeyCode::Char('/')), &keymap),
            InputResult::Action(Action::ToggleHints)
        ));
    }

    /// (b) Remapping a twinned chord moves EVERY delivery form to the new
    /// action. Named via the `"alt+A"` spelling here (rather than
    /// `"alt+shift+a"`) to prove both spellings reach the identical twin
    /// set — they're derived from the same default binding either way.
    #[test]
    fn remapping_a_twinned_chord_moves_every_delivery_form() {
        let (keymap, diagnostics) = Keymap::parse(r#"{"keys": {"alt+A": "quit"}}"#, "config.json");
        assert!(diagnostics.problems.is_empty(), "a valid remap is not a problem: {diagnostics:?}");
        // Both delivery forms of the shifted chord (default: ToggleRoster)
        // now quit instead...
        assert!(matches!(
            translate_with(alt_shift(KeyCode::Char('a')), &keymap),
            InputResult::Action(Action::Quit)
        ));
        assert!(matches!(
            translate_with(alt(KeyCode::Char('A')), &keymap),
            InputResult::Action(Action::Quit)
        ));
        // ...and the unshifted chord (JumpAttention) is untouched: it isn't
        // a twin of the shifted one, just another binding on the same
        // letter — twin expansion must not overreach into it.
        assert!(matches!(
            translate_with(alt(KeyCode::Char('a')), &keymap),
            InputResult::Action(Action::JumpAttention)
        ));
    }

    /// (c) The twin derivation is mechanical, not a hand-kept list:
    /// for every chord the default table binds, its twin set must contain
    /// every OTHER default-bound chord that shares its action and its
    /// physical key — case-folded siblings (h/H) and glyph/base pairs
    /// (`+`/`=`, `,`/`<`) alike — so a future chord gaining this same
    /// delivery-duality pattern is covered automatically, with nothing here
    /// that has to be remembered and updated by hand.
    #[test]
    fn twin_derivation_finds_every_same_letter_same_action_default_chord() {
        let table = default_keymap();
        // Two characters name the same physical key when one is the other's
        // case-folded sibling or its shifted glyph (either direction).
        let same_key = |a: char, b: char| {
            a.eq_ignore_ascii_case(&b)
                || shifted_char(a) == Some(b)
                || glyph_base(a).is_some_and(|base| base == b)
        };
        for (&chord, &action) in table {
            let KeyCode::Char(c) = chord.code else {
                continue;
            };
            let found = twins(chord.code, chord.shift);
            for (&other, &other_action) in table {
                let KeyCode::Char(oc) = other.code else {
                    continue;
                };
                if same_key(c, oc) && other_action == action {
                    assert!(
                        found.contains(&other),
                        "{chord:?} ({action:?})'s twins must include {other:?}"
                    );
                }
            }
        }
    }

    /// The 2026-09-01 re-key split `toggle_stack` into the `Alt+s` /
    /// `Alt+Shift+s` pair, on both delivery forms — the same
    /// uppercase-delivery tolerance every other shifted letter carries.
    #[test]
    fn alt_s_stacks_and_alt_shift_s_explodes_on_both_delivery_forms() {
        assert_eq!(
            translate(alt(KeyCode::Char('s'))),
            InputResult::Action(Action::StackPane),
            "the unshifted chord keeps the collapse half the toggle had",
        );
        assert_eq!(
            translate(alt_shift(KeyCode::Char('s'))),
            InputResult::Action(Action::ExplodeStack),
            "Alt+Shift+s on a terminal that sends the shift bit",
        );
        assert_eq!(
            translate(alt(KeyCode::Char('S'))),
            InputResult::Action(Action::ExplodeStack),
            "Alt+Shift+s on a terminal that sends bare uppercase S",
        );
    }

    /// The re-key's compatibility promise, which nothing else pins: a
    /// `config.json` written against the pre-2026-09-01 `toggle_stack`
    /// action still parses — silently, with no diagnostic — and lands on
    /// the collapse half (`Alt+s`'s, the half that kept the chord).
    /// The reverse direction is the other half of the promise: `roost keys`
    /// and the overlay must print the *canonical* name, never the alias, or
    /// the printed map would not paste back as itself.
    #[test]
    fn the_toggle_stack_config_alias_still_parses_and_never_prints_back() {
        let (keymap, diagnostics) =
            Keymap::parse(r#"{"keys": {"alt+x": "toggle_stack"}}"#, "config.json");
        assert!(diagnostics.is_empty(), "a v0.1.16 config must not warn: {diagnostics:?}");
        assert_eq!(
            translate_with(alt(KeyCode::Char('x')), &keymap),
            InputResult::Action(Action::StackPane),
            "the retired toggle name maps to the collapse half",
        );
        // Reverse lookup: `stack_pane` is first in NAMES, so it wins.
        assert_eq!(action_name(&Action::StackPane), "stack_pane");
        assert_eq!(action_name(&Action::ExplodeStack), "explode_stack");
        assert!(
            !effective_bindings(&keymap).iter().any(|(_, a)| action_name(a) == "toggle_stack"),
            "no surface may spell the parse-only alias",
        );
        // And it is a real alias, not a second action: both names resolve
        // to the same variant.
        assert_eq!(action_by_name("toggle_stack"), action_by_name("stack_pane"));
    }

    // ---- the override contract, swept ------------------------------------

    /// config.json's spelling of any chord the default table binds — the
    /// inverse of `Chord::parse`, test-side, so the sweeps below go through
    /// the real grammar instead of hand-building keymaps.
    fn config_spelling(chord: Chord) -> String {
        let key = match chord.code {
            KeyCode::Enter => "enter".to_string(),
            KeyCode::PageUp => "pageup".to_string(),
            KeyCode::Up => "up".to_string(),
            KeyCode::Down => "down".to_string(),
            KeyCode::Left => "left".to_string(),
            KeyCode::Right => "right".to_string(),
            KeyCode::Char(c) => c.to_string(),
            other => panic!("default table binds {other:?}, which the grammar cannot spell"),
        };
        format!("alt+{}{}", if chord.shift { "shift+" } else { "" }, key)
    }

    /// Every chord the default table binds is expressible in the config
    /// grammar and disable-able with no diagnostic, and what a disabled
    /// chord then does is pinned: a `Char` forwards as ESC+char (the
    /// readline fix — M-b/M-f/M-d keep working), while a non-Char (arrows,
    /// enter) is swallowed — roost has no faithful byte encoding to hand
    /// back for those, so `disable` there means "roost stops owning it" and
    /// nothing more.
    #[test]
    fn every_default_chord_is_disableable_through_the_config_grammar() {
        for (&chord, &default) in default_keymap() {
            let spelling = config_spelling(chord);
            let json = format!(r#"{{"keys": {{"{spelling}": "disable"}}}}"#);
            let (keymap, diagnostics) = Keymap::parse(&json, "config.json");
            assert!(diagnostics.is_empty(), "{spelling}: {diagnostics:?}");
            let key = if chord.shift { alt_shift(chord.code) } else { alt(chord.code) };
            let result = translate_with(key, &keymap);
            assert_ne!(
                result,
                InputResult::Action(default),
                "{spelling} must stop producing its default {default:?}"
            );
            // Every default chord — Char or not — has a meta-ESC encoding,
            // so `disable` reaches the pane in every single case. The
            // earlier version of this test asserted that non-Char chords
            // were *swallowed*, which pinned the bug rather than the
            // contract: it made `disable` a black hole on exactly the
            // Alt+arrow chords the keyword exists to hand back.
            assert!(
                matches!(result, InputResult::Forward(_)),
                "{spelling}: a disabled chord reaches the pane, got {result:?}"
            );
        }
    }

    /// Every action name `roost keys` can print parses and dispatches end to
    /// end on a chord the default table never bound — the full pipe
    /// (`Chord::parse` → `action_by_name` → `translate_with`), not just the
    /// name table's round-trip.
    #[test]
    fn every_config_action_name_parses_and_dispatches_on_a_free_chord() {
        for (name, expected) in NAMES {
            let json = format!(r#"{{"keys": {{"alt+x": "{name}"}}}}"#);
            let (keymap, diagnostics) = Keymap::parse(&json, "config.json");
            assert!(diagnostics.is_empty(), "{name}: {diagnostics:?}");
            assert_eq!(
                translate_with(alt(KeyCode::Char('x')), &keymap),
                InputResult::Action(*expected),
                "{name} must bind and dispatch"
            );
        }
    }

    /// The canonical readline rescue: take the arrows away from roost (some
    /// shells bind Alt+arrow to word motion), and the vim letters keep
    /// focus while the *shifted* arrows keep moving panes — an override is
    /// per-chord, never per-family.
    ///
    /// The rescue only rescues if the key actually *arrives*: this used to
    /// assert `Ignore`, i.e. roost stopped acting on Alt+arrow and the shell
    /// never received it either. `disable` forwards the meta-ESC bytes now
    /// (`disabled_chord`), so the shell gets its word motion back — which is
    /// the entire scenario this test is named for.
    #[test]
    fn disabling_the_arrows_leaves_the_rest_of_the_map_alone() {
        let json = r#"{"keys": {
            "alt+left": "disable", "alt+right": "disable",
            "alt+up": "disable", "alt+down": "disable" }}"#;
        let (keymap, diagnostics) = Keymap::parse(json, "config.json");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        for (code, bytes) in [
            (KeyCode::Left, b"\x1b\x1b[D".as_slice()),
            (KeyCode::Right, b"\x1b\x1b[C".as_slice()),
            (KeyCode::Up, b"\x1b\x1b[A".as_slice()),
            (KeyCode::Down, b"\x1b\x1b[B".as_slice()),
        ] {
            assert_eq!(
                translate_with(alt(code), &keymap),
                InputResult::Forward(bytes.to_vec()),
                "{code:?} must reach the shell as meta-ESC, not vanish",
            );
        }
        for (code, dir) in [
            (KeyCode::Left, Dir::Left),
            (KeyCode::Right, Dir::Right),
            (KeyCode::Up, Dir::Up),
            (KeyCode::Down, Dir::Down),
        ] {
            assert_eq!(
                translate_with(alt_shift(code), &keymap),
                InputResult::Action(Action::MovePane(dir)),
                "only the unshifted chord was disabled: {code:?}"
            );
        }
        for (code, dir) in [
            (KeyCode::Char('h'), Dir::Left),
            (KeyCode::Char('j'), Dir::Down),
            (KeyCode::Char('k'), Dir::Up),
            (KeyCode::Char('l'), Dir::Right),
        ] {
            assert_eq!(
                translate_with(alt(code), &keymap),
                InputResult::Action(Action::Focus(dir)),
                "the vim focus letters are untouched: {code:?}"
            );
        }
    }
    /// A pane that negotiated the kitty keyboard protocol asked to be told
    /// which modifiers were held. Meta-ESC cannot say: `ESC ESC [ D` is the
    /// same bytes whether or not Alt was down, which is precisely the
    /// ambiguity disambiguation exists to remove. So a forwarded functional
    /// key gets the CSI-modifier form for those panes.
    ///
    /// This is the *only* place roost picks an encoding other than meta-ESC
    /// for a forwarded chord, and it is the only place it can do so without
    /// guessing — the pane declared what it wants. A pane that never
    /// negotiated keeps meta-ESC, unchanged.
    #[test]
    fn a_kitty_pane_gets_functional_keys_in_the_csi_modifier_form() {
        // Alt+Left. mods = 1 + alt(2) = 3.
        let key = alt(KeyCode::Left);
        let meta_esc = encode_raw(key);
        assert_eq!(meta_esc, b"\x1b\x1b[D", "the fallback every other pane keeps");
        assert_eq!(
            kitty_upgrade(key, meta_esc.clone(), false),
            meta_esc,
            "a pane that never negotiated is untouched",
        );
        assert_eq!(
            kitty_upgrade(key, meta_esc, true),
            b"\x1b[1;3D".to_vec(),
            "a kitty pane is told the modifier",
        );

        // The whole family, and the arithmetic behind each: 1 + shift(1) +
        // alt(2) + ctrl(4). A tilde key keeps its number, a cursor key its
        // letter — kitty leaves functional keys in their legacy CSI shape.
        let cases: &[(KeyEvent, &[u8])] = &[
            (alt(KeyCode::Right), b"\x1b[1;3C"),
            (alt(KeyCode::Up), b"\x1b[1;3A"),
            (alt(KeyCode::Down), b"\x1b[1;3B"),
            (alt_shift(KeyCode::Left), b"\x1b[1;4D"),
            (alt(KeyCode::PageUp), b"\x1b[5;3~"),
            (alt(KeyCode::Home), b"\x1b[1;3H"),
        ];
        for (key, want) in cases {
            assert_eq!(kitty_upgrade(*key, encode_raw(*key), true), want.to_vec(), "{key:?}",);
        }
    }

    /// The change to `encode_key`'s modifier arithmetic must not reach any
    /// existing caller: Alt still never gets there through `translate` (it is
    /// roost's chord layer) or through `encode_raw` (which strips it), and an
    /// Alt+printable still leaves as meta-ESC rather than acquiring a
    /// modifier parameter it never had.
    #[test]
    fn admitting_alt_to_the_modifier_arithmetic_changes_no_existing_path() {
        // Unmodified and Shift/Ctrl-modified navigation: byte-identical.
        for key in [
            plain(KeyCode::Left),
            KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT),
            plain(KeyCode::PageUp),
        ] {
            assert_eq!(translate(key), encode_key(key), "{key:?} still round-trips");
        }
        assert_eq!(translate(plain(KeyCode::Left)), InputResult::Forward(b"\x1b[D".to_vec()));
        assert_eq!(
            translate(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL)),
            InputResult::Forward(b"\x1b[1;5C".to_vec()),
            "Ctrl+Right is readline's word jump — unchanged",
        );
        // An unbound Alt+printable is still meta-ESC, not a CSI form.
        assert_eq!(encode_raw(alt(KeyCode::Char('f'))), b"\x1bf".to_vec());
    }
}
