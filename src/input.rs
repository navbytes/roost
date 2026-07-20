//! Keybindings (design doc §7): roost owns the Alt layer, everything else is
//! forwarded raw to the focused pane so agents see a normal terminal.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    NewPane,
    ClosePane,
    FocusNext,
    FocusPrev,
    NewTab,
    GoToTab(usize),
    ToggleStack,
    /// Grow (+) or shrink (−) the focused pane along an axis.
    Resize { horizontal: bool, grow: bool },
    RenamePane,
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
            KeyCode::Char('r') => Some(Action::RenamePane),
            KeyCode::Enter => Some(Action::QuickLaunch),
            KeyCode::PageUp => Some(Action::ScrollMode),
            KeyCode::Char(c @ '1'..='9') => Some(Action::GoToTab(c as usize - '1' as usize)),
            KeyCode::Right | KeyCode::Down | KeyCode::Char('l') | KeyCode::Char('j') => {
                Some(Action::FocusNext)
            }
            KeyCode::Left | KeyCode::Up | KeyCode::Char('h') | KeyCode::Char('k') => {
                Some(Action::FocusPrev)
            }
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
