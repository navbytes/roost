//! pi (badlogic/pi-mono coding agent) adapter.
//!
//! Ground truth (pi docs):
//! - sessions auto-persist to `~/.pi/agent/sessions/`, organized by cwd
//! - `pi --session <path|id>` resumes an exact session (partial UUID ok)
//! - `-c/--continue` resumes the most recent session in cwd
//! - extensions live in `~/.pi/agent/extensions/*.ts` and get lifecycle
//!   events (`session_start`, `agent_start`, `agent_end`, `session_shutdown`)
//!
//! Status/session detection: the bundled `extensions/roost.ts` pi extension
//! reports exact events over a unix socket ($XDG_RUNTIME_DIR/roost.sock),
//! tagged with the ROOST_PANE env var we set at spawn. See design doc §6.1.

use super::{AgentAdapter, CommandSpec};
use std::path::{Path, PathBuf};

pub struct PiAdapter;

impl AgentAdapter for PiAdapter {
    fn id(&self) -> &'static str {
        "pi"
    }

    fn launch(&self, cwd: &Path) -> CommandSpec {
        CommandSpec::new("pi", cwd)
    }

    fn resume(&self, cwd: &Path, session: &str) -> CommandSpec {
        CommandSpec::new("pi", cwd).arg("--session").arg(session)
    }

    /// pi organizes sessions under ~/.pi/agent/sessions/ by cwd. We scan the
    /// whole root: only files newer than our spawn time matter, and a fresh
    /// pane can only have produced one of those.
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
    /// with separators turned to dashes (and some dash-wrapping). Rather than
    /// hardcode that private encoding, compare the file's parent dir to the
    /// cwd with all non-alphanumerics stripped — robust to pi's exact dash
    /// convention while still scoping detection to this pane's project.
    fn owns_session_file(&self, path: &Path, cwd: &Path) -> bool {
        let key = |s: &str| -> String {
            s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
        };
        match path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()) {
            Some(dir) => key(dir) == key(&cwd.to_string_lossy()),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
