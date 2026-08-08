//! config.json — the key-bindings escape hatch (`ui::input::Keymap`), read
//! once at startup. Same directory resolution as `workspace.json`
//! (`FsStore::state_dir`), so `$ROOST_STATE` redirects this file too.

use std::path::{Path, PathBuf};

use crate::infra::store::FsStore;
use crate::ui::input::Keymap;

pub fn config_path() -> PathBuf {
    FsStore::state_dir().join("config.json")
}

/// Read + parse config.json from wherever `config_path` resolves to. Never
/// fatal (design doc): a missing file — the common case — is silently
/// today's defaults, no diagnostics; any other read failure (permissions, a
/// directory sitting where the file should be, ...) degrades the same way
/// malformed content does, via one diagnostic naming the problem. Parsing
/// itself is `Keymap::parse`'s job, unit-tested directly in `ui::input`.
pub fn load_keymap() -> (Keymap, Vec<String>) {
    load_keymap_from(&config_path())
}

fn load_keymap_from(path: &Path) -> (Keymap, Vec<String>) {
    let source = path.display().to_string();
    match std::fs::read_to_string(path) {
        Ok(raw) => Keymap::parse(&raw, &source),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Keymap::default(), Vec::new()),
        Err(e) => (
            Keymap::default(),
            vec![format!("{source}: {e} — using defaults")],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `config_path` itself reads `$ROOST_STATE` — a process-global env var,
    /// racy across tests running in parallel in one process (see
    /// `infra::sock`'s own tests for the same reason) — so this only pins
    /// `load_keymap_from`'s behavior for an injected path, never the real
    /// `ROOST_STATE`-driven one. That redirection is instead proven by the
    /// PTY-level `tests/config_keys.rs`, which drives it through the real
    /// binary in its own isolated process.
    #[test]
    fn a_missing_file_is_silent_defaults() {
        let dir = std::env::temp_dir().join(format!(
            "roost-config-test-missing-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let (keymap, diagnostics) = load_keymap_from(&dir.join("config.json"));
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(keymap, Keymap::default());
    }

    #[test]
    fn a_real_file_is_read_and_handed_to_keymap_parse() {
        let dir = std::env::temp_dir().join(format!(
            "roost-config-test-real-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"keys": {"alt+f": "disable"}}"#).unwrap();
        let (keymap, diagnostics) = load_keymap_from(&path);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_ne!(
            keymap,
            Keymap::default(),
            "the disable entry must have been applied"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A read failure that isn't "file doesn't exist" (here: the parent
    /// path is actually a file, not a directory, so opening `.../config.json`
    /// underneath it fails with NotADirectory) degrades to a diagnostic
    /// instead of propagating an error — never fatal, same contract as
    /// malformed content.
    #[test]
    fn an_unreadable_path_is_a_diagnostic_not_a_panic() {
        let dir = std::env::temp_dir().join(format!(
            "roost-config-test-unreadable-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::write(&dir, b"not a directory").unwrap(); // `dir` is a plain file
        let (keymap, diagnostics) = load_keymap_from(&dir.join("config.json"));
        assert_eq!(keymap, Keymap::default());
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        let _ = std::fs::remove_file(&dir);
    }
}
