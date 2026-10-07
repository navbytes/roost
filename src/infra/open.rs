//! Open what Alt+click / copy-mode `o` landed on: a URL (default browser
//! unless a rule says otherwise) or an existing file/dir (only via a rule).

use crate::core::open::{select, OpenRule, Target};
use std::ffi::OsString;

/// Stat a path target: canonicalize and learn file vs dir. `None` when it
/// does not exist (URLs pass through).
// ponytail: stat blocks the main loop on a hung network mount; thread+timeout if it ever bites
fn stat(target: Target) -> Option<Target> {
    let Target::Path { path, line, col, .. } = target else { return Some(target) };
    let path = path.canonicalize().ok()?;
    let is_dir = path.is_dir();
    Some(Target::Path { path, line, col, is_dir })
}

/// Flash for an existing path that no rule opens; without it a zero-config
/// Alt+click on a path is silent and indistinguishable from a miss.
pub const NO_RULE_HINT: &str = "no opener rule for this path (see README Openers)";

/// What `open_target` did. The caller falls through to an ordinary click on
/// anything but `Opened`.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Opened,
    /// The path exists but no rule handles it.
    NoRule,
    /// No such path.
    Missing,
}

/// `Err` is a short message for the flash when the handler could not be started.
pub fn open_target(rules: &[OpenRule], target: Target) -> Result<Outcome, String> {
    let Some(target) = stat(target) else { return Ok(Outcome::Missing) };
    match select(rules, &target)? {
        Some(argv) => spawn(&argv).map(|()| Outcome::Opened),
        None => Ok(Outcome::NoRule),
    }
}

/// Same B2-class fix as `infra::clipboard::copy` (PR #46 code review): a
/// test driving `handle_mouse`'s Alt+click path for real would otherwise
/// spawn the operator's actual browser on every `cargo test` run.
/// `#[cfg(test)]` swaps in a no-op — and, same as `clipboard::copy`, it only
/// reaches *this crate's own* test binary. `tests/*.rs` spawns the real
/// `roost` binary through a PTY; `host_io_disabled` (`infra/mod.rs`) is what
/// closes that door, honored by the real body here.
#[cfg(not(test))]
fn spawn(argv: &[OsString]) -> Result<(), String> {
    use std::process::{Command, Stdio};
    if super::host_io_disabled() {
        return Ok(());
    }
    let prog = argv[0].to_string_lossy();
    // Detach stdio so it can't disturb the TUI (and a handler that wants a
    // terminal fails fast instead of fighting roost for it), and reap in a
    // thread so we don't leak a zombie.
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!("open: {prog} not found"),
            k => format!("open: {prog}: {k}"),
        })?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
fn spawn(_argv: &[OsString]) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::open::{Kind, Target};
    use std::path::PathBuf;

    fn p(path: PathBuf) -> Target {
        Target::Path { path, line: Some(3), col: None, is_dir: false }
    }

    #[test]
    fn stat_canonicalizes_and_classifies() {
        let dir = std::env::temp_dir().join(format!("roost-open-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        let canon = dir.canonicalize().unwrap();
        let via_dotdot = dir.join("sub").join("..").join("a.txt");
        assert_eq!(
            stat(p(via_dotdot)),
            Some(Target::Path {
                path: canon.join("a.txt"),
                line: Some(3),
                col: None,
                is_dir: false
            })
        );
        assert!(matches!(stat(p(dir.join("sub"))), Some(Target::Path { is_dir: true, .. })));
        assert_eq!(stat(p(dir.join("missing.txt"))), None);

        let dir_rule = OpenRule { kind: Kind::Dir, ext: vec![], run: vec!["code".into()] };
        let opened = Ok(Outcome::Opened);
        assert_eq!(open_target(std::slice::from_ref(&dir_rule), p(dir.join("sub"))), opened);
        // an existing path nothing handles (no rules, or an ext filter) earns the hint...
        let ext_rule =
            OpenRule { kind: Kind::File, ext: vec!["pdf".into()], run: vec!["x".into()] };
        assert_eq!(open_target(&[dir_rule], p(dir.join("a.txt"))), Ok(Outcome::NoRule));
        assert_eq!(open_target(&[ext_rule], p(dir.join("a.txt"))), Ok(Outcome::NoRule));
        assert_eq!(open_target(&[], p(dir.join("a.txt"))), Ok(Outcome::NoRule));
        // ...prose (no such path) stays silent
        assert_eq!(open_target(&[], p(dir.join("missing.txt"))), Ok(Outcome::Missing));
        // URLs keep the default handler with no rules
        assert_eq!(open_target(&[], Target::Url("https://a.co".into())), opened);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
