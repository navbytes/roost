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
    /// Grow (+) or shrink (−) the focused pane along an axis.
    Resize { horizontal: bool, grow: bool },
    RenamePane,
    RenameTab,
    QuickLaunch,
    ScrollMode,
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
            // Alt+r renames the pane; Alt+Shift+r (or Alt+R) renames the tab.
            KeyCode::Char('r') => Some(if shift { Action::RenameTab } else { Action::RenamePane }),
            KeyCode::Char('R') => Some(Action::RenameTab),
            KeyCode::Enter => Some(Action::QuickLaunch),
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

/// Encode a key event as the bytes a terminal would send. Covers the common
/// set; kitty-protocol fidelity is a later concern.
fn encode_key(key: KeyEvent) -> InputResult {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let bytes: Vec<u8> = match key.code {
        KeyCode::Char(c) if ctrl => {
            let b = (c.to_ascii_lowercase() as u8) & 0x1f;
            vec![b]
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
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
}
