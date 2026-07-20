//! Per-tool adapters: each knows how to launch its CLI fresh, resume a
//! specific session, detect a new session's id, and interpret status signals.
//!
//! Design doc §6. The pi adapter is the v1 flagship; `shell` is the generic
//! fallback for arbitrary commands.

pub mod claude;
pub mod pi;
pub mod shell;

use crate::workspace::PaneSpec;
use std::collections::HashMap;
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
    fn detect_session(&self, cwd: &Path, since: SystemTime) -> Option<String> {
        let root = self.session_root(cwd)?;
        let path = newest_session_file_since(&root, since)?;
        self.session_id_from_path(&path)
    }

    /// Pick launch vs resume based on what the pane spec knows.
    fn command_for(&self, spec: &PaneSpec) -> CommandSpec {
        match &spec.session {
            Some(s) => self.resume(&spec.cwd, s),
            None => self.launch(&spec.cwd),
        }
    }
}

/// Newest file under `root` (recursive) modified after `since`. Used to spot
/// the session file a freshly launched agent just created.
pub fn newest_session_file_since(root: &Path, since: SystemTime) -> Option<PathBuf> {
    let mut best: Option<(SystemTime, PathBuf)> = None;
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
            if mtime > since && best.as_ref().is_none_or(|(t, _)| mtime > *t) {
                best = Some((mtime, p));
            }
        }
    }
    best.map(|(_, p)| p)
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
