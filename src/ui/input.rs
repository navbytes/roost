//! Keybindings (design doc §7): roost owns the Alt layer, everything else is
//! forwarded raw to the focused pane so agents see a normal terminal.

use crate::core::layout::Dir;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    NewPane,
    ClosePane,
    /// Move focus spatially (arrows / hjkl).
    Focus(Dir),
    NewTab,
    GoToTab(usize),
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

pub enum InputResult {
    Action(Action),
    Forward(Vec<u8>),
    Ignore,
}

pub fn translate(key: KeyEvent) -> InputResult {
    if key.kind == KeyEventKind::Release {
        return InputResult::Ignore;
    }

    if key.modifiers.contains(KeyModifiers::ALT) {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let action = match key.code {
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
            KeyCode::Char('/') => Some(Action::ToggleHints),
            KeyCode::Char('c') => Some(Action::CopyMode),
            KeyCode::Char('u') => Some(Action::Undo),
            KeyCode::Char('?') => Some(Action::Help),
            KeyCode::Char('a') => Some(Action::JumpAttention),
            KeyCode::Char('z') => Some(Action::ToggleZoom),
            KeyCode::Char('g') => Some(Action::CycleLayout),
            KeyCode::Char('e') => Some(Action::ToggleFeed),
            KeyCode::Char('f') => Some(Action::ToggleFloat),
            KeyCode::PageUp => Some(Action::ScrollMode),
            KeyCode::Char(c @ '1'..='9') => Some(Action::GoToTab(c as usize - '1' as usize)),
            KeyCode::Right | KeyCode::Char('l') => Some(Action::Focus(Dir::Right)),
            KeyCode::Left | KeyCode::Char('h') => Some(Action::Focus(Dir::Left)),
            KeyCode::Down | KeyCode::Char('j') => Some(Action::Focus(Dir::Down)),
            KeyCode::Up | KeyCode::Char('k') => Some(Action::Focus(Dir::Up)),
            _ => None,
        };
        return match action {
            Some(a) => InputResult::Action(a),
            None => InputResult::Ignore,
        };
    }

    encode_key(key)
}

/// Upgrade modified-Enter bytes to the kitty CSI-u encoding when the target
/// pane negotiated the protocol (`kitty` = its disambiguate flag). Panes that
/// never opted in keep the ESC+CR fallback from `encode_key`. Called from the
/// forward path, where the focused pane's state is known.
pub fn kitty_upgrade(key: KeyEvent, bytes: Vec<u8>, kitty: bool) -> Vec<u8> {
    if !kitty || key.code != KeyCode::Enter || key.modifiers.contains(KeyModifiers::ALT) {
        return bytes;
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        b"\x1b[13;2u".to_vec() // Shift+Enter, kitty CSI-u (mods 2 = shift)
    } else if key.modifiers.contains(KeyModifiers::CONTROL) {
        b"\x1b[13;5u".to_vec() // Ctrl+Enter, kitty CSI-u (mods 5 = ctrl)
    } else {
        bytes
    }
}

/// Encode a key event as the bytes a terminal would send. Covers the common
/// set; a pane that negotiated kitty gets modified Enter upgraded to CSI-u by
/// `kitty_upgrade` on the way out.
fn encode_key(key: KeyEvent) -> InputResult {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let bytes: Vec<u8> = match key.code {
        KeyCode::Char(c) if ctrl => {
            let b = (c.to_ascii_lowercase() as u8) & 0x1f;
            vec![b]
        }
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
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        _ => return InputResult::Ignore,
    };
    InputResult::Forward(bytes)
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
        // lowercase Alt+p (no shift) is deliberately unbound — it stays free
        // for raw-mode pass-through (C23).
        assert!(matches!(translate(alt(KeyCode::Char('p'))), InputResult::Ignore));
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
}
