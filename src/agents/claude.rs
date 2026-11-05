//! Claude Code adapter (v1.1 target).
//!
//! - sessions: `~/.claude/projects/<encoded-cwd>/*.jsonl`
//! - resume exact session: `claude --resume <session-id>`, run from the
//!   directory the session is filed under (which can differ from the pane's
//!   stored cwd once Claude Code has changed directory — see `locate`)
//! - clean status signals: Claude Code hooks (Notification / Stop /
//!   PreToolUse) can run a shell command → point them at roost's socket.

use super::{AgentAdapter, CommandSpec, SessionState};
use std::io::{BufRead, BufReader};
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

fn projects_dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".claude").join("projects"))
}

/// Where a session's transcript lives relative to the pane's cwd.
#[derive(Debug, PartialEq, Eq)]
enum Located {
    /// Under `projects/<encode(cwd)>/` — the plain case.
    Here,
    /// Under a different project directory (the path of that directory).
    Elsewhere(PathBuf),
    /// `projects/` is readable and no project holds the id.
    Nowhere,
    /// Can't tell (`projects/` missing or unreadable).
    Unknown,
}

/// Find `id`'s transcript, starting with the pane's own project.
///
/// The pane's cwd and the session's project drift apart because roost
/// persists the *live* cwd of the pane's process (`observe_panes`), and
/// Claude Code changes its own process cwd mid-session — entering a git
/// worktree is the common way. The transcript stays filed under the
/// directory the session started in, so after a restart roost looked in
/// `projects/<encode(new cwd)>/`: if that directory existed (a worktree
/// with sessions of its own, the repo the worktree came from) the id was
/// "Gone" and silently dropped, and if it did not, `claude --resume <id>`
/// ran in a directory where Claude cannot find the session and failed.
fn locate(projects: &Path, cwd: &Path, id: &str) -> Located {
    let file = format!("{id}.jsonl");
    if projects.join(encode_cwd(cwd)).join(&file).is_file() {
        return Located::Here;
    }
    let Ok(rd) = std::fs::read_dir(projects) else { return Located::Unknown };
    for entry in rd.flatten() {
        let dir = entry.path();
        if dir.join(&file).is_file() {
            return Located::Elsewhere(dir);
        }
    }
    Located::Nowhere
}

/// The directory `claude --resume <id>` must run in: `cwd` when the session
/// is filed under it, otherwise the directory its project was created for.
/// `None` means "no better answer than `cwd`".
fn resume_cwd(projects: &Path, cwd: &Path, id: &str) -> Option<PathBuf> {
    match locate(projects, cwd, id) {
        Located::Elsewhere(dir) => project_cwd(&dir.join(format!("{id}.jsonl")), &dir),
        _ => None,
    }
}

/// Recover the real path a project directory encodes. The encoding is lossy
/// (`/`, `.`, `_`, space all become `-`), so it cannot be decoded; instead
/// read the `cwd` field Claude Code writes on transcript entries and take the
/// first one that encodes back to this project's name and still exists.
/// Bounded: the session's starting cwd is on its first entries, and a
/// transcript can be very large.
fn project_cwd(transcript: &Path, project: &Path) -> Option<PathBuf> {
    const MAX_LINES: usize = 200;
    let want = project.file_name()?.to_str()?;
    let f = std::fs::File::open(transcript).ok()?;
    for line in BufReader::new(f).lines().take(MAX_LINES) {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let Some(c) = v.get("cwd").and_then(|c| c.as_str()) else { continue };
        let c = PathBuf::from(c);
        if encode_cwd(&c) == want && c.is_dir() {
            return Some(c);
        }
    }
    None
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
        Some(projects_dir()?.join(encode_cwd(cwd)))
    }

    /// Resume from the directory the session actually belongs to, which is
    /// not always the pane's stored cwd — see [`locate`].
    fn resume(&self, cwd: &Path, session: &str) -> CommandSpec {
        let dir = projects_dir().and_then(|p| resume_cwd(&p, cwd, session));
        self.launch(dir.as_deref().unwrap_or(cwd)).arg(self.resume_flag()).arg(session)
    }

    /// Exists if the session is under *any* project, not just the pane's
    /// current one — see [`locate`] for why the two drift apart.
    fn session_state(&self, cwd: &Path, id: &str) -> SessionState {
        let Some(projects) = projects_dir() else { return SessionState::Unknown };
        match locate(&projects, cwd, id) {
            Located::Here | Located::Elsewhere(_) => SessionState::Exists,
            // Not filed anywhere: keep the pre-existing per-project rule, so
            // a missing project directory stays Unknown (resume attempted,
            // id kept) and only a readable one without the id is Gone.
            Located::Nowhere | Located::Unknown => {
                super::session_file_state(self, &projects.join(encode_cwd(cwd)), cwd, id)
            }
        }
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

    /// A scratch `~/.claude/projects` holding session `id` filed under
    /// `home`, plus a pane that has since moved to `home/wt` (what entering
    /// a worktree does to Claude Code's process cwd).
    fn drifted(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let base = super::super::test_support::scratch_dir(tag);
        let projects = base.join("projects");
        let home = base.join("repo_x");
        let moved = home.join(".claude").join("worktrees").join("wt");
        std::fs::create_dir_all(&moved).unwrap();
        let proj = projects.join(encode_cwd(&home));
        std::fs::create_dir_all(&proj).unwrap();
        let entry = |cwd: &Path| {
            serde_json::json!({ "type": "user", "cwd": cwd.to_string_lossy() }).to_string()
        };
        // First line carries no cwd, the moved-to cwd appears later: the
        // reader must pick the one that encodes back to the project name.
        let body = format!("{{\"type\":\"summary\"}}\n{}\n{}\n", entry(&home), entry(&moved));
        std::fs::write(proj.join("sid.jsonl"), body).unwrap();
        (projects, home, moved)
    }

    #[test]
    fn a_session_filed_under_the_pane_cwd_is_here() {
        let (projects, home, _) = drifted("claude-here");
        assert_eq!(locate(&projects, &home, "sid"), Located::Here);
        assert_eq!(resume_cwd(&projects, &home, "sid"), None);
    }

    /// The restart bug: the pane's stored cwd moved, the session didn't.
    /// It must still be found, and resumed from where it was filed.
    #[test]
    fn a_pane_that_changed_directory_resumes_from_the_sessions_project() {
        let (projects, home, moved) = drifted("claude-moved");
        assert!(matches!(locate(&projects, &moved, "sid"), Located::Elsewhere(_)));
        assert_eq!(resume_cwd(&projects, &moved, "sid"), Some(home));
    }

    /// The moved-to directory having a project of its own (the worktree
    /// ran other sessions) used to read as Gone and drop the id for good.
    #[test]
    fn a_readable_project_without_the_id_does_not_make_it_gone() {
        let (projects, _, moved) = drifted("claude-other-proj");
        std::fs::create_dir_all(projects.join(encode_cwd(&moved))).unwrap();
        std::fs::write(projects.join(encode_cwd(&moved)).join("other.jsonl"), "").unwrap();
        assert!(matches!(locate(&projects, &moved, "sid"), Located::Elsewhere(_)));
    }

    #[test]
    fn an_id_in_no_project_is_nowhere_and_no_projects_dir_is_unknown() {
        let (projects, home, _) = drifted("claude-nowhere");
        assert_eq!(locate(&projects, &home, "ghost"), Located::Nowhere);
        assert_eq!(locate(&projects.join("absent"), &home, "sid"), Located::Unknown);
    }

    /// The session's own directory was deleted (a removed worktree): no
    /// usable cwd to recover, so the caller keeps the pane's.
    #[test]
    fn a_deleted_project_directory_falls_back_to_the_pane_cwd() {
        let (projects, home, moved) = drifted("claude-deleted");
        std::fs::remove_dir_all(&home).unwrap();
        assert_eq!(resume_cwd(&projects, &moved, "sid"), None);
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
