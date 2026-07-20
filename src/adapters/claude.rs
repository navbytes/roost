//! Claude Code adapter (v1.1 target).
//!
//! - sessions: `~/.claude/projects/<encoded-cwd>/*.jsonl`
//! - resume exact session: `claude --resume <session-id>`
//! - clean status signals: Claude Code hooks (Notification / Stop /
//!   PreToolUse) can run a shell command → point them at roost's socket.

use super::{AgentAdapter, CommandSpec};
use std::path::Path;

pub struct ClaudeAdapter;

impl AgentAdapter for ClaudeAdapter {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn launch(&self, cwd: &Path) -> CommandSpec {
        CommandSpec::new("claude", cwd)
    }

    fn resume(&self, cwd: &Path, session: &str) -> CommandSpec {
        CommandSpec::new("claude", cwd).arg("--resume").arg(session)
    }

    fn detect_session(&self, _cwd: &Path) -> Option<String> {
        // TODO(M5): newest new .jsonl in ~/.claude/projects/<encoded-cwd>/
        None
    }
}
