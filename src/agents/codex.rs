//! Codex CLI (OpenAI) adapter.
//!
//! Ground truth is codex-rs's own source (the CLI ships no doc describing
//! this on-disk layout) — confirmed against the openai/codex repo:
//! - bare `codex` (no subcommand) launches the interactive TUI: "If no
//!   subcommand is specified, options will be forwarded to the interactive
//!   CLI" (cli/src/main.rs). `codex resume <SESSION_ID>` resumes an exact
//!   session; SESSION_ID is a UUID (a thread name also works, but a UUID
//!   takes precedence if it parses) — cli/src/main.rs `ResumeCommand`.
//! - sessions ("rollouts") persist under $CODEX_HOME/sessions (default
//!   ~/.codex/sessions), one subdirectory per date: `sessions/YYYY/MM/DD/
//!   rollout-<date_str>-<conversation_id>.jsonl`, `conversation_id` being
//!   the session's UUID (rollout/src/recorder.rs).
//! - that layout is date-scoped, not project-scoped. `codex resume`'s cwd
//!   filtering (the picker hides other projects' sessions unless you pass
//!   `--all`) is driven by a `cwd` field recorded *inside* each rollout
//!   (protocol::SessionMeta), which nothing here reads — see
//!   `owns_session_file` below.

use super::{AgentAdapter, CommandSpec};
use std::path::{Path, PathBuf};

pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn launch(&self, cwd: &Path) -> CommandSpec {
        CommandSpec::new("codex", cwd)
    }

    fn resume(&self, cwd: &Path, session: &str) -> CommandSpec {
        CommandSpec::new("codex", cwd).arg("resume").arg(session)
    }

    /// Global rather than per-cwd: codex has no per-project subdirectory to
    /// hand back the way pi/claude do (see the module doc). `since` (this
    /// pane's spawn time) is what actually narrows a scan down to the one
    /// file a fresh launch just wrote.
    fn session_root(&self, _cwd: &Path) -> Option<PathBuf> {
        Some(dirs::home_dir()?.join(".codex").join("sessions"))
    }

    /// `rollout-<date_str>-<conversation_id>.jsonl`, and `codex resume`
    /// wants the bare UUID. A UUID is always exactly 36 characters, so lift
    /// the tail rather than parse around it — `date_str` itself
    /// (`2026-08-06T10-30-00`) is dash-separated and would confuse a
    /// split-on-`-` approach the way pi's timestamp prefix does not.
    /// `.jsonl.zst` (codex transparently compresses rolled-over rollouts) is
    /// intentionally not matched: decompressing it would need a new
    /// dependency for a session that, by definition, isn't the freshest one.
    fn session_id_from_path(&self, path: &Path) -> Option<String> {
        if path.extension()?.to_str()? != "jsonl" {
            return None;
        }
        let stem = path.file_stem()?.to_str()?;
        if !stem.starts_with("rollout-") || stem.len() < 36 {
            return None;
        }
        // `.get` (not indexing) so a stray non-ASCII byte in a corrupt/
        // adversarial filename can't land mid-character and panic.
        stem.get(stem.len() - 36..).map(str::to_string)
    }

    // owns_session_file: left at the trait default (always true). Unlike
    // pi's dash-encoded project directory, codex's date-bucketed layout
    // carries no path component naming the project a rollout belongs to —
    // that lives only in the `cwd` field inside the file (see module doc),
    // which nothing here parses. Two codex panes launched in different
    // directories within the same `since` window could in principle
    // cross-detect each other's brand-new session; `taken` (ids already
    // claimed by other panes, checked by the caller) is the only guard
    // against that for codex.
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::SessionState;
    use std::collections::HashSet;
    use std::time::{Duration, SystemTime};

    #[test]
    fn launch_is_bare_codex() {
        let cmd = CodexAdapter.launch(Path::new("/tmp"));
        assert_eq!(cmd.program, "codex");
        assert!(cmd.args.is_empty());
    }

    #[test]
    fn resume_uses_resume_subcommand() {
        let cmd = CodexAdapter.resume(Path::new("/tmp"), "3f9a1c2e-7b4d-4a11-9c2e-0f1a2b3c4d5e");
        assert_eq!(cmd.program, "codex");
        assert_eq!(cmd.args, vec!["resume", "3f9a1c2e-7b4d-4a11-9c2e-0f1a2b3c4d5e"]);
    }

    #[test]
    fn extracts_uuid_tail_from_rollout_filename() {
        let p = Path::new(
            "/h/.codex/sessions/2026/08/06/rollout-2026-08-06T10-30-00-3f9a1c2e-7b4d-4a11-9c2e-0f1a2b3c4d5e.jsonl",
        );
        assert_eq!(
            CodexAdapter.session_id_from_path(p).as_deref(),
            Some("3f9a1c2e-7b4d-4a11-9c2e-0f1a2b3c4d5e")
        );
    }

    #[test]
    fn ignores_sibling_files_that_are_not_dated_rollouts() {
        // session_index.jsonl lives at the sessions root alongside the
        // date-bucketed rollouts; its stem is nowhere near 36 chars.
        assert_eq!(
            CodexAdapter.session_id_from_path(Path::new("/h/.codex/sessions/session_index.jsonl")),
            None
        );
        // A compressed, rolled-over rollout: not the freshest session, and
        // decompressing it isn't worth a new dependency (see module doc).
        assert_eq!(
            CodexAdapter.session_id_from_path(Path::new(
                "/h/.codex/sessions/2026/08/06/rollout-2026-08-06T10-30-00-3f9a1c2e-7b4d-4a11-9c2e-0f1a2b3c4d5e.jsonl.zst"
            )),
            None
        );
    }

    #[test]
    fn owns_session_file_is_unscoped_by_design() {
        // Confirmed limitation (see module doc): codex buckets rollouts by
        // date, not cwd, so there's no path-based cwd signal to check here —
        // this is `true` for every cwd, not an oversight in this test.
        let f = Path::new(
            "/h/.codex/sessions/2026/08/06/rollout-x-3f9a1c2e-7b4d-4a11-9c2e-0f1a2b3c4d5e.jsonl",
        );
        assert!(CodexAdapter.owns_session_file(f, Path::new("/home/nav/proj-x")));
        assert!(CodexAdapter.owns_session_file(f, Path::new("/home/nav/totally-unrelated")));
    }

    /// Delegates to the real adapter's parsing but points `session_root` at
    /// a fixture directory, so `session_state`/`detect_session` (trait
    /// defaults) can be exercised without touching the real ~/.codex.
    struct FixtureCodex(PathBuf);
    impl AgentAdapter for FixtureCodex {
        fn id(&self) -> &'static str {
            "codex"
        }
        fn launch(&self, cwd: &Path) -> CommandSpec {
            CodexAdapter.launch(cwd)
        }
        fn resume(&self, cwd: &Path, session: &str) -> CommandSpec {
            CodexAdapter.resume(cwd, session)
        }
        fn session_root(&self, _cwd: &Path) -> Option<PathBuf> {
            Some(self.0.clone())
        }
        fn session_id_from_path(&self, path: &Path) -> Option<String> {
            CodexAdapter.session_id_from_path(path)
        }
    }

    fn fixture_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("roost-codex-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn session_state_unknown_when_root_missing() {
        let missing =
            std::env::temp_dir().join(format!("roost-codex-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&missing);
        assert_eq!(
            FixtureCodex(missing).session_state(Path::new("/x"), "id"),
            SessionState::Unknown
        );
    }

    #[test]
    fn session_state_exists_then_gone() {
        let d = fixture_dir("state");
        let day = d.join("2026").join("08").join("06");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(
            day.join("rollout-2026-08-06T10-30-00-3f9a1c2e-7b4d-4a11-9c2e-0f1a2b3c4d5e.jsonl"),
            "",
        )
        .unwrap();
        let a = FixtureCodex(d.clone());
        assert_eq!(
            a.session_state(Path::new("/x"), "3f9a1c2e-7b4d-4a11-9c2e-0f1a2b3c4d5e"),
            SessionState::Exists
        );
        assert_eq!(a.session_state(Path::new("/x"), "not-there"), SessionState::Gone);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn detect_session_finds_the_freshly_written_rollout_and_skips_taken_ids() {
        let d = fixture_dir("detect");
        let day = d.join("2026").join("08").join("06");
        std::fs::create_dir_all(&day).unwrap();
        let since = SystemTime::now();
        let file =
            day.join("rollout-2026-08-06T10-30-00-3f9a1c2e-7b4d-4a11-9c2e-0f1a2b3c4d5e.jsonl");
        std::fs::write(&file, "").unwrap();
        std::fs::File::open(&file).unwrap().set_modified(since + Duration::from_millis(10)).unwrap();

        let a = FixtureCodex(d.clone());
        assert_eq!(
            a.detect_session(Path::new("/x"), since, &HashSet::new()).as_deref(),
            Some("3f9a1c2e-7b4d-4a11-9c2e-0f1a2b3c4d5e")
        );
        let taken: HashSet<String> = ["3f9a1c2e-7b4d-4a11-9c2e-0f1a2b3c4d5e".to_string()].into();
        assert_eq!(a.detect_session(Path::new("/x"), since, &taken), None);
        let _ = std::fs::remove_dir_all(&d);
    }
}
