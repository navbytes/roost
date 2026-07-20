//! Per-tool adapters: each knows how to launch its CLI fresh, resume a
//! specific session, detect a new session's id, and interpret status signals.
//!
//! Design doc §6. The pi adapter is the v1 flagship; `shell` is the generic
//! fallback for arbitrary commands.

pub mod claude;
pub mod pi;
pub mod shell;

use crate::core::workspace::PaneSpec;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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
            if let Some(id) = self.session_id_from_path(&path) {
                if !taken.contains(&id) {
                    return Some(id);
                }
            }
        }
        None
    }

    /// Pick launch vs resume based on what the pane spec knows.
    fn command_for(&self, spec: &PaneSpec) -> CommandSpec {
        match &spec.session {
            Some(s) => self.resume(&spec.cwd, s),
            None => self.launch(&spec.cwd),
        }
    }
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

pub fn registry() -> Registry {
    let mut m: Registry = HashMap::new();
    for a in [
        Box::new(shell::ShellAdapter) as Box<dyn AgentAdapter>,
        Box::new(pi::PiAdapter),
        Box::new(claude::ClaudeAdapter),
    ] {
        m.insert(a.id(), a);
    }
    m
}
