//! pi (badlogic/pi-mono coding agent) adapter.
//!
//! Ground truth (pi docs):
//! - sessions auto-persist to `~/.pi/agent/sessions/`, organized by cwd
//! - `pi --session <path|id>` resumes an exact session (partial UUID ok)
//! - `-c/--continue` resumes the most recent session in cwd
//! - extensions live in `~/.pi/agent/extensions/*.ts` and get lifecycle
//!   events (`session_start`, `agent_start`, `agent_settled`, `session_shutdown`)
//!
//! Status/session detection: the bundled `extensions/roost.ts` pi extension
//! reports exact events over a unix socket ($XDG_RUNTIME_DIR/roost.sock),
//! tagged with the ROOST_PANE env var we set at spawn. See design doc §6.1.

use super::AgentAdapter;
use std::path::{Path, PathBuf};

pub struct PiAdapter;

impl AgentAdapter for PiAdapter {
    fn id(&self) -> &'static str {
        "pi"
    }

    fn resume_flag(&self) -> &'static str {
        "--session"
    }

    /// pi organizes sessions under ~/.pi/agent/sessions/ by cwd, but its
    /// per-cwd subdirectory naming isn't a documented contract worth
    /// reverse-engineering — this returns the whole sessions root and lets
    /// `owns_session_file` (below) scope matches to `cwd`.
    // ponytail: O(all sessions) read_dir walk per detect (trait default);
    // re-add cwd-narrowing if a huge ~/.pi measurably hurts.
    fn session_root(&self, _cwd: &Path) -> Option<PathBuf> {
        Some(dirs::home_dir()?.join(".pi").join("agent").join("sessions"))
    }

    /// pi names session files `<iso-timestamp>_<uuid>.jsonl`, but
    /// `pi --session` only matches on the bare UUID (or a prefix of it) — the
    /// timestamp prefix makes it reject the id outright. Extract the segment
    /// after the last underscore. Files without an underscore (e.g. the
    /// pi-fake test fixture) fall back to the whole stem.
    fn session_id_from_path(&self, path: &Path) -> Option<String> {
        let stem = path.file_stem()?.to_str()?;
        Some(stem.rsplit('_').next().unwrap_or(stem).to_string())
    }

    /// pi stores sessions in a per-cwd subdirectory whose name is the path
    /// with separators turned to dashes (and some dash-wrapping). Rather
    /// than hardcode that private encoding, compare the file's parent dir
    /// to the cwd as a **sequence of alphanumeric runs** — robust to pi's
    /// exact dash convention while still scoping detection to this project.
    ///
    /// The runs are compared as a list, not concatenated. Concatenating
    /// (the original spelling: strip every non-alphanumeric from both
    /// sides) made `/home/me/my-project` and `/home/me/myproject` the same
    /// key, so a pane in either project matched the other's session files —
    /// and with several projects open at once that is a live way to resume
    /// a conversation from the wrong one.
    ///
    /// **Still lossy, unavoidably.** pi's encoding uses the same character
    /// for a separator as for a dash inside a directory name, so
    /// `/home/me/my/project` and `/home/me/my-project` both encode to
    /// `-home-me-my-project` and remain indistinguishable from the
    /// directory name alone. That collision is pi's to disambiguate, not
    /// roost's; the one this fixes is the one roost invented.
    fn owns_session_file(&self, path: &Path, cwd: &Path) -> bool {
        let runs = |s: &str| -> Vec<String> {
            s.split(|c: char| !c.is_alphanumeric())
                .filter(|part| !part.is_empty())
                .map(|part| part.to_lowercase())
                .collect()
        };
        match path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()) {
            Some(dir) => runs(dir) == runs(&cwd.to_string_lossy()),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `owns_session_file` strips **every** non-alphanumeric from both
    /// sides, so two project paths that differ only in punctuation
    /// normalize to the same key — and each pane then matches the other
    /// project's session files.
    #[test]
    fn two_projects_differing_only_in_punctuation_must_not_share_sessions() {
        let a = PiAdapter;
        // pi's own directory encoding for /home/me/my-project.
        let f = Path::new("/h/.pi/agent/sessions/-home-me-my-project/2026-01-01T00-00-00-000Z_aaaa.jsonl");
        assert!(a.owns_session_file(f, Path::new("/home/me/my-project")), "its own project matches");
        assert!(
            !a.owns_session_file(f, Path::new("/home/me/myproject")),
            "a different project whose path differs only by a dash must NOT match",
        );
    }

    #[test]
    fn extracts_bare_uuid_from_pi_filename() {
        let a = PiAdapter;
        let p = Path::new(
            "/h/.pi/agent/sessions/proj/2026-07-20T09-17-57-467Z_019f7ed1-5b5a-72cc-b89a-2c4fd41a0006.jsonl",
        );
        assert_eq!(
            a.session_id_from_path(p).as_deref(),
            Some("019f7ed1-5b5a-72cc-b89a-2c4fd41a0006")
        );
    }

    #[test]
    fn falls_back_to_stem_without_underscore() {
        let a = PiAdapter;
        let p = Path::new("/h/.pi/agent/sessions/proj/fake-uuid-999.jsonl");
        assert_eq!(a.session_id_from_path(p).as_deref(), Some("fake-uuid-999"));
    }

    #[test]
    fn resume_uses_session_flag() {
        let a = PiAdapter;
        let cmd = a.resume(Path::new("/tmp"), "abc-123");
        assert_eq!(cmd.program, "pi");
        assert_eq!(cmd.args, vec!["--session", "abc-123"]);
    }

    #[test]
    fn owns_session_file_scopes_to_cwd_ignoring_dash_encoding() {
        let a = PiAdapter;
        // pi's real dir name for /home/nav/proj-x, dash-wrapped
        let f = Path::new("/root/.pi/agent/sessions/--home-nav-proj-x--/ts_uuid.jsonl");
        assert!(a.owns_session_file(f, Path::new("/home/nav/proj-x")));
        // a different project must not match
        assert!(!a.owns_session_file(f, Path::new("/home/nav/other")));
    }
}
