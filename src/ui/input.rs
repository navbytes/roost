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
    NewTab,
    GoToTab(usize),
    /// U7: step to the next / previous tab, wrapping at both ends — the
    /// only keyboard route to tabs 10+ (`Alt+1..9` runs out at nine).
    NextTab,
    PrevTab,
    /// U7: jump to the last tab (`Alt+0`), whatever its number.
    LastTab,
    /// C28: carry the focused pane to the previous / next tab, wrapping —
    /// `Alt+Shift+i` / `Alt+Shift+m`, the shifted siblings of the two chords
    /// that move *you* between tabs. Focus follows the pane.
    MovePaneToTab { forward: bool },
    ToggleStack,
    /// Flip the focused pane's split between vertical and horizontal.
    FlipSplit,
    /// Grow (+) or shrink (−) the focused pane along an axis.
    Resize { horizontal: bool, grow: bool },
    RenamePane,
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
    /// Snap the active tab to the next canned arrangement that fits (C25).
    CycleLayout,
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
        // Alt+Shift+arrows: resize
        KeyCode::Right if shift => Some(Action::Resize { horizontal: true, grow: true }),
        KeyCode::Left if shift => Some(Action::Resize { horizontal: true, grow: false }),
        KeyCode::Down if shift => Some(Action::Resize { horizontal: false, grow: true }),
        KeyCode::Up if shift => Some(Action::Resize { horizontal: false, grow: false }),
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Char('n') => Some(Action::NewPane),
        KeyCode::Char('w') => Some(Action::ClosePane),
        KeyCode::Char('t') => Some(Action::NewTab),
        KeyCode::Char('s') => Some(Action::ToggleStack),
        KeyCode::Char('o') => Some(Action::FlipSplit), // orientation
        // Alt+r renames the pane; Alt+Shift+r (or Alt+R) renames the tab.
        KeyCode::Char('r') => Some(if shift { Action::RenameTab } else { Action::RenamePane }),
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
        KeyCode::Char('g') => Some(Action::CycleLayout),
        KeyCode::Char('e') => Some(Action::ToggleFeed),
        KeyCode::Char('f') => Some(Action::ToggleFloat),
        KeyCode::PageUp => Some(Action::ScrollMode),
        KeyCode::Char(c @ '1'..='9') => Some(Action::GoToTab(c as usize - '1' as usize)),
        // U7: tabs 10+ had no keyboard route at all. `Alt+0` closes the
        // digit row's own gap (last tab, the "and the rest" slot), and
        // Alt+i/Alt+m step the strip. Both letters come from §8's free
        // pool, and both survive U5: they were unbound, so they used to
        // forward to the pane — now they're roost's, like every other
        // chord in this table. See §8's amendment for why these two.
        KeyCode::Char('0') => Some(Action::LastTab),
        // C28: the shifted siblings carry the *pane* the way the
        // unshifted ones carry *you* — Alt+i/Alt+m move focus between
        // tabs, Alt+Shift+i/Alt+Shift+m move the focused pane there and
        // follow it. `Alt+I`/`Alt+M` are the same uppercase-delivery
        // tolerance Alt+Shift+r / Alt+Shift+a / Alt+Shift+p carry.
        // (Alt+[ / Alt+] were the brief's suggestion and are rejected in
        // §8: `ESC [` is the CSI introducer.)
        KeyCode::Char('i') => {
            Some(if shift { Action::MovePaneToTab { forward: false } } else { Action::PrevTab })
        }
        KeyCode::Char('I') => Some(Action::MovePaneToTab { forward: false }),
        KeyCode::Char('m') => {
            Some(if shift { Action::MovePaneToTab { forward: true } } else { Action::NextTab })
        }
        KeyCode::Char('M') => Some(Action::MovePaneToTab { forward: true }),
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
        || key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT)
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
        KeyCode::Enter if m.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT) => {
            csi_u(13, m)
        }
        // The unshifted codepoint: crossterm hands us the *shifted* char for
        // `Ctrl+Shift+a` on some terminals, and kitty wants `97;6u`.
        KeyCode::Char(c) if ctrl_or_alt => csi_u(c.to_ascii_lowercase() as u32, m),
        _ => bytes,
    }
}

/// Encode a key event as the bytes a terminal would send. Covers the common
/// set; a pane that negotiated kitty gets modified Enter upgraded to CSI-u by
/// `kitty_upgrade` on the way out.
fn encode_key(key: KeyEvent) -> InputResult {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    // xterm's modifier parameter: 1 + shift(1) + alt(2) + ctrl(4). Alt never
    // reaches this table — in cooked routing it's roost's own chord layer,
    // and `encode_raw` strips it before delegating here — so only Shift/Ctrl
    // contribute (xm ∈ {1, 2, 5, 6}). xm == 1 means "unmodified": the keys
    // below keep their bare legacy forms, exactly as a terminal would.
    let xm = 1 + u8::from(shift) + 4 * u8::from(ctrl);
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
        Some(Chord {
            code: named_key(key)?,
            shift,
        })
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
    /// entry happened to name.
    pub fn parse(raw: &str, source: &str) -> (Keymap, Vec<String>) {
        let mut diagnostics = Vec::new();
        let value: serde_json::Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(e) => {
                diagnostics.push(format!("{source}: invalid JSON ({e}) — using defaults"));
                return (Keymap::default(), diagnostics);
            }
        };
        let mut keymap = Keymap::default();
        match value.get("keys") {
            None => {}
            Some(serde_json::Value::Object(keys)) => {
                for (chord_str, action_val) in keys {
                    let Some(chord) = Chord::parse(chord_str) else {
                        diagnostics.push(format!(
                            "{source}: unrecognized chord {chord_str:?} — skipped"
                        ));
                        continue;
                    };
                    let Some(action_str) = action_val.as_str() else {
                        diagnostics.push(format!(
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
                            for t in twins(chord.code, chord.shift) {
                                keymap.overrides.insert(t, Override::Bound(action));
                            }
                        }
                        None => diagnostics.push(format!(
                            "{source}: {chord_str:?}: unknown action {action_str:?} — skipped"
                        )),
                    }
                }
            }
            Some(_) => diagnostics.push(format!("{source}: \"keys\" must be an object — ignored")),
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
        let chord = Chord {
            code: key.code,
            shift: key.modifiers.contains(KeyModifiers::SHIFT),
        };
        match keymap.overrides.get(&chord) {
            Some(Override::Disabled) => return unbound_alt(key),
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
    ("toggle_stack", Action::ToggleStack),
    ("flip_split", Action::FlipSplit),
    ("resize_horizontal_grow", Action::Resize { horizontal: true, grow: true }),
    ("resize_horizontal_shrink", Action::Resize { horizontal: true, grow: false }),
    ("resize_vertical_grow", Action::Resize { horizontal: false, grow: true }),
    ("resize_vertical_shrink", Action::Resize { horizontal: false, grow: false }),
    ("rename_pane", Action::RenamePane),
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
    ("cycle_layout", Action::CycleLayout),
    ("toggle_feed", Action::ToggleFeed),
    ("toggle_float", Action::ToggleFloat),
    ("toggle_raw", Action::ToggleRaw),
];

/// Test-only: `action_by_name` is the production reverse lookup (a straight
/// `NAMES` scan); this — the other direction — only has a caller left in the
/// round-trip test below, which checks `NAMES` itself against
/// `default_keymap`'s independently-derived source of truth.
#[cfg(test)]
fn action_name(action: &Action) -> String {
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

/// Test-only: every (chord → action) pair `default_chord_action` actually
/// binds, swept across every key `Chord::parse` can name, crossed with both
/// shift states. This *is* "the existing default map" — built by asking the
/// same function `translate` itself calls, not by re-deriving the table by
/// hand — so it is what tests assert absent-config behavior against, and
/// (the round-trip test) `NAMES`'s independent check.
#[cfg(test)]
fn default_keymap() -> HashMap<Chord, Action> {
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

/// Every `(code, shift)` the default table already treats as the *same*
/// logical chord as `(code, shift)` itself — the terminal-delivery duality
/// `default_chord_action` already carries for a handful of letters (p/P,
/// r/R, a/A, i/I, m/M: some terminals send `Alt+Shift+p` as `('p', SHIFT)`,
/// others as `('P', no SHIFT)`, and the default table binds both to
/// `ToggleRaw` so either works). A config entry naming *one* delivery form
/// must disable or remap *all* of them — otherwise it silently no-ops on
/// whichever terminal happens to send the other one.
///
/// Derived from `default_chord_action`, not a hand-kept letter list: two
/// `Char` chords are twins exactly when they share a case-folded letter and
/// both already default to the same action. That reuses the one source of
/// truth `default_keymap`/`translate` already run on, so a future letter
/// gaining this pattern is covered automatically — see
/// `twin_derivation_finds_every_same_letter_same_action_default_chord`.
/// Always includes `(code, shift)` itself; non-`Char` codes and chords with
/// no default action (nothing to be a twin of) have no other twins.
fn twins(code: KeyCode, shift: bool) -> Vec<Chord> {
    let (Some(action), KeyCode::Char(c)) = (default_chord_action(code, shift), code) else {
        return vec![Chord { code, shift }];
    };
    let mut found = Vec::new();
    for cased in [c.to_ascii_lowercase(), c.to_ascii_uppercase()] {
        for s in [false, true] {
            let candidate = Chord {
                code: KeyCode::Char(cased),
                shift: s,
            };
            if !found.contains(&candidate)
                && default_chord_action(candidate.code, s) == Some(action)
            {
                found.push(candidate);
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
        assert!(matches!(translate(alt(KeyCode::Char('s'))), InputResult::Action(Action::ToggleStack)));
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
            InputResult::Action(Action::CycleLayout)
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

    /// C28: the shifted siblings of U7's tab steps carry the *pane*. The
    /// unshifted pair must stay exactly what it was — the whole idiom is
    /// "shift makes the chord take the pane with you", so a regression here
    /// would silently move panes when the user meant to change tabs.
    #[test]
    fn alt_shift_i_and_m_move_the_pane_with_uppercase_delivery_tolerance() {
        for (code, forward) in [('i', false), ('m', true)] {
            assert!(
                matches!(
                    translate(alt_shift(KeyCode::Char(code))),
                    InputResult::Action(Action::MovePaneToTab { forward: f }) if f == forward
                ),
                "Alt+Shift+{code}",
            );
            let upper = code.to_ascii_uppercase();
            assert!(
                matches!(
                    translate(alt(KeyCode::Char(upper))),
                    InputResult::Action(Action::MovePaneToTab { forward: f }) if f == forward
                ),
                "Alt+{upper} (uppercase delivery)",
            );
        }
        assert!(matches!(
            translate(alt(KeyCode::Char('i'))),
            InputResult::Action(Action::PrevTab)
        ));
        assert!(matches!(
            translate(alt(KeyCode::Char('m'))),
            InputResult::Action(Action::NextTab)
        ));
    }

    #[test]
    fn alt_r_renames_pane_alt_shift_r_renames_tab() {
        assert!(matches!(
            translate(alt(KeyCode::Char('r'))),
            InputResult::Action(Action::RenamePane)
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

    #[test]
    fn alt_shift_arrows_resize_not_focus() {
        assert!(matches!(
            translate(alt_shift(KeyCode::Right)),
            InputResult::Action(Action::Resize { horizontal: true, grow: true })
        ));
        assert!(matches!(
            translate(alt_shift(KeyCode::Up)),
            InputResult::Action(Action::Resize { horizontal: false, grow: false })
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
        assert_eq!(app_cursor_upgrade(plain(KeyCode::PageUp), b"\x1b[5~".to_vec(), true), b"\x1b[5~");
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
        assert_eq!(encode_raw(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)), vec![0x03]);
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
        let ctrl_cmd_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER | KeyModifiers::CONTROL);
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
            (alt(KeyCode::Char('s')), Action::ToggleStack),
            (alt(KeyCode::Char('o')), Action::FlipSplit),
            (alt(KeyCode::Char('r')), Action::RenamePane),
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
            (alt(KeyCode::Char('g')), Action::CycleLayout),
            (alt(KeyCode::Char('e')), Action::ToggleFeed),
            (alt(KeyCode::Char('f')), Action::ToggleFloat),
            (alt(KeyCode::PageUp), Action::ScrollMode),
            (alt(KeyCode::Char('3')), Action::GoToTab(2)),
            (alt(KeyCode::Char('0')), Action::LastTab), // U7
            (alt(KeyCode::Char('i')), Action::PrevTab),
            (alt(KeyCode::Char('m')), Action::NextTab),
            (alt(KeyCode::Right), Action::Focus(Dir::Right)),
            (alt(KeyCode::Char('h')), Action::Focus(Dir::Left)),
            (alt(KeyCode::Char('j')), Action::Focus(Dir::Down)),
            (alt(KeyCode::Char('k')), Action::Focus(Dir::Up)),
            (alt_shift(KeyCode::Right), Action::Resize { horizontal: true, grow: true }),
            (alt_shift(KeyCode::Up), Action::Resize { horizontal: false, grow: false }),
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
        for (chord, _) in default_keymap() {
            let mut mods = KeyModifiers::ALT;
            if chord.shift {
                mods |= KeyModifiers::SHIFT;
            }
            let key = KeyEvent::new(chord.code, mods);
            assert_eq!(
                translate_with(key, &empty),
                translate(key),
                "chord {chord:?}"
            );
        }
        let extra = [
            alt(KeyCode::Char('b')),   // unbound Alt printable (readline M-b)
            alt(KeyCode::Char('d')),   // unbound Alt printable (readline M-d)
            plain(KeyCode::Char('a')), // no Alt at all
            KeyEvent::new(
                KeyCode::Char('w'),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            ), // P13
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
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(matches!(
            translate_with(alt(KeyCode::Char('g')), &keymap),
            InputResult::Action(Action::Quit)
        ));
        assert!(!matches!(
            translate_with(alt(KeyCode::Char('g')), &keymap),
            InputResult::Action(Action::CycleLayout)
        ));
    }

    /// A chord listed twice keeps only its last value (README) — inherited
    /// for free from `serde_json`, which already collapses a repeated JSON
    /// key to its last-written one.
    #[test]
    fn a_chord_listed_twice_keeps_only_the_last_value() {
        let json = r#"{"keys": {"alt+g": "quit", "alt+g": "toggle_zoom"}}"#;
        let (keymap, diagnostics) = Keymap::parse(json, "config.json");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
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
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(diagnostics[0].contains("config.json"), "{diagnostics:?}");
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
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(
            diagnostics[0].contains("not_a_real_action"),
            "{diagnostics:?}"
        );
        assert!(diagnostics[0].contains("alt+z"), "{diagnostics:?}");
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
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(diagnostics[0].contains("ctrl+f"), "{diagnostics:?}");
    }

    /// The Action-name mapping is exhaustive: `action_name`'s match has no
    /// wildcard arm (a new `Action` variant left unnamed is a compile
    /// error), and this pins that every name it actually produces — for
    /// every action any default chord binds — round-trips back through
    /// `action_by_name`, so a future variant is a compile error *or* this
    /// test failing, never a silent gap either way.
    #[test]
    fn every_default_bound_actions_name_round_trips() {
        for (chord, action) in default_keymap() {
            let name = action_name(&action);
            assert_eq!(
                action_by_name(&name),
                Some(action),
                "chord {chord:?}'s action {action:?} named {name:?} didn't round-trip"
            );
        }
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

    /// (b) Remapping a twinned chord moves EVERY delivery form to the new
    /// action. Named via the `"alt+A"` spelling here (rather than
    /// `"alt+shift+a"`) to prove both spellings reach the identical twin
    /// set — they're derived from the same default binding either way.
    #[test]
    fn remapping_a_twinned_chord_moves_every_delivery_form() {
        let (keymap, diagnostics) = Keymap::parse(r#"{"keys": {"alt+A": "quit"}}"#, "config.json");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
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

    /// (c) The twin derivation is mechanical, not a hand-kept letter list:
    /// for every chord the default table binds, its twin set must contain
    /// every OTHER default-bound chord that shares its action and its
    /// letter (case-insensitively) — so a future letter gaining this same
    /// delivery-duality pattern is covered automatically, with nothing here
    /// that has to be remembered and updated by hand.
    #[test]
    fn twin_derivation_finds_every_same_letter_same_action_default_chord() {
        let table = default_keymap();
        for (&chord, &action) in &table {
            let KeyCode::Char(c) = chord.code else {
                continue;
            };
            let found = twins(chord.code, chord.shift);
            for (&other, &other_action) in &table {
                let KeyCode::Char(oc) = other.code else {
                    continue;
                };
                if oc.eq_ignore_ascii_case(&c) && other_action == action {
                    assert!(
                        found.contains(&other),
                        "{chord:?} ({action:?})'s twins must include {other:?}"
                    );
                }
            }
        }
    }
}
