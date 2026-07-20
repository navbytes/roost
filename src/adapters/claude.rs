//! Claude Code adapter (v1.1 target).
//!
//! - sessions: `~/.claude/projects/<encoded-cwd>/*.jsonl`
//! - resume exact session: `claude --resume <session-id>`
//! - clean status signals: Claude Code hooks (Notification / Stop /
//!   PreToolUse) can run a shell command → point them at roost's socket.

use super::{AgentAdapter, CommandSpec};
use std::path::{Path, PathBuf};

/// Claude Code encodes a project cwd into a directory name by replacing
/// path separators and dots with dashes: /home/nav/code.x → -home-nav-code-x
pub fn encode_cwd(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' || c == ' ' || c == '_' { '-' } else { c })
        .collect()
}

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

    fn session_root(&self, cwd: &Path) -> Option<PathBuf> {
        Some(dirs::home_dir()?.join(".claude").join("projects").join(encode_cwd(cwd)))
    }

    /// Only .jsonl files are sessions (ignore sidecar files).
    fn session_id_from_path(&self, path: &Path) -> Option<String> {
        if path.extension()?.to_str()? != "jsonl" {
            return None;
        }
        path.file_stem().map(|s| s.to_string_lossy().into_owned())
    }
}
