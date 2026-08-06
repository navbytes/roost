//! OpenCode CLI (anomalyco/opencode, formerly sst/opencode) adapter.
//!
//! Ground truth: the live CLI reference (opencode.ai/docs/cli, generated
//! from the CLI's own flag definitions — packages/web/src/content/docs/
//! cli.mdx) confirmed against opencode's own source at the current `dev`
//! tip (github.com/anomalyco/opencode). The published troubleshooting page
//! still describes an older per-project `storage/` directory layout that
//! predates a since-shipped move to SQLite (the same kind of docs-vs-source
//! drift gemini.rs hit) — source wins below.
//! - bare `opencode` starts the interactive TUI; like codex/claude/gemini/
//!   pi here, it reads the cwd the process is launched in rather than
//!   needing the optional `[project]` positional.
//! - `opencode --session <id>` (`-s`) resumes an exact session.
//!   `--continue`/`-c` resumes "the last session" instead, and is exactly
//!   the flag anomalyco/opencode#2086 ("opencode --continue should not
//!   resume from subagent tasks") reported hijacking a *subagent's* thread
//!   instead of the parent's — `resume` below only ever emits `--session`,
//!   never `--continue`.
//! - session ids are `"ses_" + descending()`, a 26-char base62 suffix
//!   (packages/schema/src/session-id.ts, .../identifier.ts): 30 ASCII
//!   alphanumeric-plus-underscore characters, well inside
//!   `valid_session_id`'s allowed charset.
//! - sessions are not files: opencode keeps all session/message data in one
//!   SQLite database per machine, at `$XDG_DATA_HOME/opencode/opencode.db`
//!   (packages/core/src/global.ts, .../database/database.ts). There is no
//!   per-cwd directory of session files to hand back from `session_root`
//!   the way pi/claude/gemini/codex have — see the impl below for the
//!   resulting (researched, not guessed) limitation.

use super::{AgentAdapter, CommandSpec};
use std::path::Path;

pub struct OpencodeAdapter;

impl AgentAdapter for OpencodeAdapter {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn launch(&self, cwd: &Path) -> CommandSpec {
        CommandSpec::new("opencode", cwd)
    }

    /// Resume by explicit session id only — never `--continue` (module doc:
    /// anomalyco/opencode#2086 can resume a subagent's thread instead of
    /// the parent's).
    fn resume(&self, cwd: &Path, session: &str) -> CommandSpec {
        CommandSpec::new("opencode", cwd).arg("--session").arg(session)
    }

    // session_root / session_id_from_path / owns_session_file: all left at
    // the trait default. Confirmed (see module doc) that opencode's
    // sessions live in one global SQLite database, not a directory of
    // per-session files — there's nothing for `session_files_since` to
    // walk, and pointing `session_root` at the .db file itself would just
    // make `read_dir` fail on a plain file (session_state would read that
    // as Unknown anyway, which is the honest answer; fabricating a path to
    // get there is not). Querying the database would need a new SQL
    // dependency, or shelling out to `opencode session list` to ask it —
    // neither of which any other adapter here does — so opencode panes take
    // the fully-honest degraded path: `core/app.rs`'s `wants_detect` is
    // always false (no automatic post-launch session-id detection), and a
    // manually-known session id resolves to `SessionState::Unknown` (roost
    // still attempts resume, it just never auto-clears a dead id).
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::SessionState;
    use std::collections::HashSet;
    use std::time::SystemTime;

    #[test]
    fn launch_is_bare_opencode() {
        let cmd = OpencodeAdapter.launch(Path::new("/tmp"));
        assert_eq!(cmd.program, "opencode");
        assert!(cmd.args.is_empty());
    }

    #[test]
    fn resume_uses_session_flag_never_continue() {
        let cmd = OpencodeAdapter.resume(Path::new("/tmp"), "ses_0mfx3k9qa1bc2de3fg4hj5klmn");
        assert_eq!(cmd.program, "opencode");
        assert_eq!(cmd.args, vec!["--session", "ses_0mfx3k9qa1bc2de3fg4hj5klmn"]);
        // anomalyco/opencode#2086: `--continue` can resume a subagent's
        // thread instead of the parent's — must never appear here.
        assert!(!cmd.args.iter().any(|a| a.as_str() == "--continue" || a.as_str() == "-c"));
    }

    #[test]
    fn real_shaped_session_id_round_trips_through_the_validity_guard() {
        // "ses_" + 26-char base62 (packages/schema/src/{session-id,
        // identifier}.ts): confirms opencode's real id shape passes
        // `valid_session_id` and flows unchanged into the resume command,
        // the same way a pi/codex UUID does.
        let id = "ses_0mfx3k9qa1bc2de3fg4hj5klmn";
        assert!(crate::agents::valid_session_id(id));
        assert_eq!(OpencodeAdapter.resume(Path::new("/tmp"), id).args, vec!["--session", id]);
    }

    #[test]
    fn session_root_is_none_for_every_cwd() {
        // Confirmed limitation (see module doc): opencode has no per-cwd
        // session directory at all — every session lives in one global
        // SQLite database. `None` everywhere, not an accident of one path.
        assert_eq!(OpencodeAdapter.session_root(Path::new("/home/nav/proj-x")), None);
        assert_eq!(OpencodeAdapter.session_root(Path::new("/home/nav/other")), None);
    }

    #[test]
    fn session_state_is_always_unknown() {
        // No session_root means the trait default short-circuits to
        // Unknown before touching disk — roost still attempts resume, it
        // just never auto-clears a dead id (core/app.rs's `stale` branch).
        assert_eq!(
            OpencodeAdapter.session_state(Path::new("/x"), "ses_anything"),
            SessionState::Unknown
        );
        assert_eq!(
            OpencodeAdapter.session_state(Path::new("/x"), "not-a-real-id"),
            SessionState::Unknown
        );
    }

    #[test]
    fn detect_session_never_finds_anything_taken_or_not() {
        // No session_root means `detect_session` short-circuits to None —
        // opencode panes never get an auto-detected session id
        // (core/app.rs's `wants_detect` stays false), taken-id or not.
        let since = SystemTime::now();
        assert_eq!(OpencodeAdapter.detect_session(Path::new("/x"), since, &HashSet::new()), None);
        let taken: HashSet<String> = ["ses_someoneelses".to_string()].into();
        assert_eq!(OpencodeAdapter.detect_session(Path::new("/x"), since, &taken), None);
    }
}
