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
use std::path::{Path, PathBuf};

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

    /// Try to learn the session id of a freshly launched pane so it can be
    /// persisted (e.g. by diffing the tool's session directory before/after
    /// spawn, or via an extension handshake).
    ///
    /// TODO(M2): implement session-dir diffing for pi and claude.
    fn detect_session(&self, _cwd: &Path) -> Option<String> {
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
