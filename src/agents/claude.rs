//! Claude Code adapter (v1.1 target).
//!
//! - sessions: `~/.claude/projects/<encoded-cwd>/*.jsonl`
//! - resume exact session: `claude --resume <session-id>`
//! - clean status signals: Claude Code hooks (Notification / Stop /
//!   PreToolUse) can run a shell command → point them at roost's socket.

use super::AgentAdapter;
use std::path::{Path, PathBuf};

/// Claude Code encodes a project cwd into a directory name by replacing
/// **every** character that is not an ASCII letter, digit or dash with a
/// dash: `/home/nav/code.x` → `-home-nav-code-x`.
///
/// This used to name four characters explicitly — `/`, `.`, ` `, `_` — and
/// pass everything else through. That is not a stricter rule than Claude
/// Code's, it is a *different* one, and being wrong in this direction fails
/// silently and totally: roost computes a session root directory that does
/// not exist, `read_dir` returns `NotFound`, and the pane simply never
/// resumes. No error, no flash — it just always starts fresh.
///
/// Checked against a real `~/.claude/projects` listing (44 directories,
/// macOS): not one name contains a character outside `[A-Za-z0-9-]`, and
/// `/Users/naveen/Downloads/IconKitchen-Output (2)/MotoPark…` appears there
/// as `-Users-naveen-Downloads-IconKitchen-Output--2--MotoPark…` — the
/// parens mapped to dashes, where the old rule would have kept them and
/// looked for `…Output-(2)-MotoPark…`. Parentheses in a path are not exotic;
/// neither are `&`, `+`, `'`, `,` or `#`.
///
/// Non-ASCII is mapped one dash per `char`. That is the consistent reading
/// of the rule, but it is the one case the listing above could not confirm
/// (no such directory in it) — if a project with an accented path ever fails
/// to resume, this is the line to revisit.
pub fn encode_cwd(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect()
}

pub struct ClaudeAdapter;

impl AgentAdapter for ClaudeAdapter {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn resume_flag(&self) -> &'static str {
        "--resume"
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture is a real `~/.claude/projects` listing, so this pins the
    /// encoding against what Claude Code actually writes rather than against
    /// what roost assumed it wrote.
    ///
    /// Every one of those 44 directory names is `[A-Za-z0-9-]` only — no
    /// dot, underscore, space or paren survives anywhere in the set. The old
    /// four-character rule passed everything else through, so any project
    /// path containing `(`, `)`, `&`, `+`, `'`, `,` or `#` produced a
    /// session root that does not exist: `read_dir` NotFound, no session
    /// found, pane never resumes, nothing said.
    #[test]
    fn encode_cwd_matches_a_real_claude_projects_listing() {
        for (cwd, want) in [
            ("/Users/naveen", "-Users-naveen"),
            (
                "/Users/naveen/.claude/worktrees/Nourish-determined-easley",
                "-Users-naveen--claude-worktrees-Nourish-determined-easley",
            ),
            (
                "/Users/naveen/repos/roost/.claude/worktrees/roost-design-build-5f234f",
                "-Users-naveen-repos-roost--claude-worktrees-roost-design-build-5f234f",
            ),
            ("/Users/naveen/workspace/SpotHK", "-Users-naveen-workspace-SpotHK"),
            // The one that proves the rule is not roost's four characters:
            // the parens became dashes, so the encoded name has `--2--`.
            (
                "/Users/naveen/Downloads/IconKitchen-Output (2)/MotoPark",
                "-Users-naveen-Downloads-IconKitchen-Output--2--MotoPark",
            ),
        ] {
            assert_eq!(encode_cwd(Path::new(cwd)), want, "encoding {cwd}");
        }
    }

    /// Nothing outside `[A-Za-z0-9-]` may survive — the property the listing
    /// exhibits, stated directly so a future tweak that re-admits one
    /// character is caught even if it isn't in the fixture above.
    #[test]
    fn encode_cwd_emits_only_alphanumerics_and_dashes() {
        let encoded = encode_cwd(Path::new("/a b/c.d/e_f/g(h)i/j&k+l'm,n#o/π"));
        assert!(
            encoded.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "leaked a character Claude Code would have replaced: {encoded}",
        );
    }
}
