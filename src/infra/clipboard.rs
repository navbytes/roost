//! Copy text to the system clipboard, robustly across environments:
//! a native helper (pbcopy on macOS, wl-copy / xclip / xsel on Linux) *and*
//! an OSC 52 escape to the terminal. Whichever the environment supports
//! lands the text; OSC 52 also covers SSH / tmux where no local helper runs.

use std::io::Write;
use std::process::{Command, Stdio};

/// Copy `text` to the clipboard via every available channel.
pub fn copy(text: &str) {
    let _ = native_copy(text);
    emit_osc52(text);
}

/// Pipe `text` into the first clipboard helper that exists. Returns whether
/// one was spawned successfully.
fn native_copy(text: &str) -> bool {
    // (program, args) candidates in preference order.
    let candidates: &[(&str, &[&str])] = &[
        ("pbcopy", &[]),
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];
    for (prog, args) in candidates {
        match Command::new(prog).args(*args).stdin(Stdio::piped()).spawn() {
            Ok(mut child) => {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(text.as_bytes());
                }
                // Reap so we don't leak a zombie.
                let _ = child.wait();
                return true;
            }
            Err(_) => continue, // not installed — try the next
        }
    }
    false
}

/// Write an OSC 52 clipboard-set sequence to stdout. Modern terminals
/// (iTerm2 w/ setting, kitty, wezterm, alacritty, tmux) copy it to the system
/// clipboard; terminals that don't support it ignore the sequence.
fn emit_osc52(text: &str) {
    let seq = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let mut out = std::io::stdout();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
}

/// Minimal standard base64 (no external dep).
pub fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"hello"), "aGVsbG8=");
    }
}
