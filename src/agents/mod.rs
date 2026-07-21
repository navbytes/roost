//! Per-tool adapters: each knows how to launch its CLI fresh, resume a
//! specific session, detect a new session's id, and interpret status signals.
//!
//! Design doc §6. The pi adapter is the v1 flagship; `shell` is the generic
//! fallback for arbitrary commands.

pub mod claude;
pub mod pi;
pub mod shell;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Whether a stored session id still resolves to a resumable session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// The session file is present — resume it.
    Exists,
    /// The session is definitively gone (dir readable, id absent) — launch
    /// fresh and clear the dead id.
    Gone,
    /// Can't tell (no session root, or the root is momentarily unreadable) —
    /// attempt resume but do NOT clear the id.
    Unknown,
}

#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

impl CommandSpec {
    pub fn new(program: impl Into<String>, cwd: &Path) -> Self {
        Self { program: program.into(), args: vec![], cwd: cwd.to_path_buf(), env: vec![] }
    }
    pub fn arg(mut self, a: impl Into<String>) -> Self {
        self.args.push(a.into());
        self
    }
}

pub trait AgentAdapter: Send + Sync {
    fn id(&self) -> &'static str;

    /// Command to start a brand-new session in `cwd`.
    fn launch(&self, cwd: &Path) -> CommandSpec;

    /// Command to resume the given session id in `cwd`.
    fn resume(&self, cwd: &Path, session: &str) -> CommandSpec;

    /// Where this tool stores its session files for `cwd`, if it has any.
    fn session_root(&self, _cwd: &Path) -> Option<PathBuf> {
        None
    }

    /// Turn a session file path into the id the resume command expects.
    fn session_id_from_path(&self, path: &Path) -> Option<String> {
        path.file_stem().map(|s| s.to_string_lossy().into_owned())
    }

    /// Does this session file belong to a pane in `cwd`? Adapters that store
    /// every session in one flat directory can't tell (default: yes). Those
    /// that organize by working directory override this to scope detection to
    /// the pane's own project, so two agents in different folders launched at
    /// once can't cross-detect.
    fn owns_session_file(&self, _path: &Path, _cwd: &Path) -> bool {
        true
    }

    /// Learn the session id of a freshly launched pane by finding the session
    /// file written since spawn. The exact channel (extension handshake over
    /// the status socket) takes precedence when available; this is the
    /// filesystem fallback.
    ///
    /// `taken` holds session ids already claimed by other panes. When several
    /// agents launch in the same directory at once they share one session
    /// root, so we walk candidate files newest-first and skip any id another
    /// pane already owns — otherwise two panes cross-wire onto one session.
    fn detect_session(
        &self,
        cwd: &Path,
        since: SystemTime,
        taken: &HashSet<String>,
    ) -> Option<String> {
        let root = self.session_root(cwd)?;
        for path in session_files_since(&root, since) {
            if !self.owns_session_file(&path, cwd) {
                continue;
            }
            if let Some(id) = self.session_id_from_path(&path) {
                if !taken.contains(&id) {
                    return Some(id);
                }
            }
        }
        None
    }

    /// Is a resumable session with this id still on disk? Distinguishes
    /// "definitely gone" from "can't tell" so a transient read error never
    /// discards a still-valid resume pointer. The default reuses
    /// `session_root` + `session_id_from_path`, so pi and claude get it for
    /// free; adapters without a session root (shell) return Unknown.
    fn session_state(&self, cwd: &Path, id: &str) -> SessionState {
        let Some(root) = self.session_root(cwd) else { return SessionState::Unknown };
        // A missing sessions dir means the session is truly gone; an
        // *unreadable* one (permission/transient) means we simply don't know.
        match std::fs::read_dir(&root) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return SessionState::Gone,
            Err(_) => return SessionState::Unknown,
        }
        let exists = session_files_since(&root, SystemTime::UNIX_EPOCH).iter().any(|p| {
            self.owns_session_file(p, cwd) && self.session_id_from_path(p).as_deref() == Some(id)
        });
        if exists {
            SessionState::Exists
        } else {
            SessionState::Gone
        }
    }

}

/// Is `id` a plausible session id we're willing to hand to `pi --session` /
/// `claude --resume`? No shell is ever involved (ids are passed as separate
/// argv tokens), so this is defense-in-depth, not the only guard: it rejects a
/// tampered `workspace.json` or a poisoned status-socket message trying to
/// steer resume at an attacker-chosen path, a flag (leading `-`), or something
/// that isn't an id at all. Real ids from pi/claude are UUID/hex-with-dashes;
/// we allow that plus `_`/`.` and cap the length, and reject empties, control
/// chars, path separators, `..`, and leading dashes.
pub fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && !id.starts_with('-')
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Files under `root` (recursive) modified after `since`, newest first.
/// Used to spot the session file a freshly launched agent just created.
pub fn session_files_since(root: &Path, since: SystemTime) -> Vec<PathBuf> {
    let mut found: Vec<(SystemTime, PathBuf)> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let Ok(mtime) = meta.modified() else { continue };
            if mtime > since {
                found.push((mtime, p));
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0)); // newest first
    found.into_iter().map(|(_, p)| p).collect()
}

pub type Registry = HashMap<&'static str, Box<dyn AgentAdapter>>;

/// The single source of truth for which adapters exist, in user-facing display
/// order (agents first, the generic shell last). `registry()` and the launch
/// picker both derive from this, so adding an adapter is a one-line change here
/// rather than three places that can silently diverge.
fn adapter_specs() -> Vec<Box<dyn AgentAdapter>> {
    vec![
        Box::new(pi::PiAdapter),
        Box::new(claude::ClaudeAdapter),
        Box::new(shell::ShellAdapter),
    ]
}

pub fn registry() -> Registry {
    let mut m: Registry = HashMap::new();
    for a in adapter_specs() {
        m.insert(a.id(), a);
    }
    m
}

/// Adapter ids in picker order, derived from `adapter_specs()`.
pub fn picker_ids() -> Vec<&'static str> {
    adapter_specs().iter().map(|a| a.id()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_validation_accepts_ids_rejects_paths_and_flags() {
        // Real-shaped ids pass.
        assert!(valid_session_id("3f9a1c2e-7b4d-4a11-9c2e-0f1a2b3c4d5e"));
        assert!(valid_session_id("abc_123.session"));
        // Hostile / malformed values are rejected.
        assert!(!valid_session_id("")); // empty
        assert!(!valid_session_id("../../etc/passwd")); // traversal
        assert!(!valid_session_id("/home/attacker/evil")); // path
        assert!(!valid_session_id("-oProxyCommand=evil")); // leading-dash flag
        assert!(!valid_session_id("has space")); // whitespace
        assert!(!valid_session_id("nul\0byte")); // control char
        assert!(!valid_session_id(&"x".repeat(257))); // too long
    }

    /// Adapter whose session root is a caller-supplied path, so session_state
    /// branches can be exercised deterministically against a temp dir.
    struct RootAdapter(Option<PathBuf>);
    impl AgentAdapter for RootAdapter {
        fn id(&self) -> &'static str {
            "root"
        }
        fn launch(&self, cwd: &Path) -> CommandSpec {
            CommandSpec::new("true", cwd)
        }
        fn resume(&self, cwd: &Path, session: &str) -> CommandSpec {
            CommandSpec::new("true", cwd).arg(session)
        }
        fn session_root(&self, _cwd: &Path) -> Option<PathBuf> {
            self.0.clone()
        }
    }

    #[test]
    fn session_state_unknown_without_a_root() {
        assert_eq!(RootAdapter(None).session_state(Path::new("/x"), "id"), SessionState::Unknown);
    }

    #[test]
    fn session_state_gone_when_dir_missing() {
        let d = std::env::temp_dir().join(format!("roost-ss-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(
            RootAdapter(Some(d)).session_state(Path::new("/x"), "id"),
            SessionState::Gone
        );
    }

    #[test]
    fn session_state_exists_when_file_present() {
        let d = std::env::temp_dir().join(format!("roost-ss-present-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("the-id.jsonl"), "").unwrap();
        // default session_id_from_path = file stem = "the-id"
        assert_eq!(
            RootAdapter(Some(d.clone())).session_state(Path::new("/x"), "the-id"),
            SessionState::Exists
        );
        assert_eq!(
            RootAdapter(Some(d.clone())).session_state(Path::new("/x"), "other"),
            SessionState::Gone
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
