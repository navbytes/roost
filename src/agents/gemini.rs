//! Gemini CLI (Google) adapter.
//!
//! Ground truth is gemini-cli's own source at the currently published
//! release (v0.54.0) — the public docs describe an on-disk layout that no
//! longer matches what's actually installed, so source, not docs, is cited
//! below:
//! - `gemini --resume <id|index|latest>` resumes a session
//!   (docs/cli/session-management.md — this part the docs get right). We
//!   always pass the full session UUID, never the picker's transient
//!   1-based index.
//! - sessions live under `~/.gemini/tmp/<slug>/chats/`, but `<slug>` is
//!   *not* a deterministic hash of the cwd — that scheme (sha256(cwd)) was
//!   retired. `<slug>` is an opaque id gemini hands out and records in a
//!   registry, `~/.gemini/projects.json`: `{"projects": {"<abs-cwd>":
//!   "<slug>"}}` (config/projectRegistry.ts). We read that mapping rather
//!   than trying to reproduce how it's generated.
//! - a session file is named `session-<timestamp>-<id[..8]>.jsonl`
//!   (services/chatRecordingService.ts) — only the first 8 characters of
//!   the id are in the filename. The id `--resume` actually matches against
//!   (`SessionInfo.id === content.sessionId`, cli/src/utils/
//!   sessionUtils.ts `findSession`) is the full UUID recorded in the file's
//!   first JSONL record, so unlike pi/claude, turning a path into a
//!   resumable id here means reading that one line, not just parsing the
//!   filename.

use super::AgentAdapter;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Prefix gemini gives every top-level session file; subagent transcripts
/// (nested one directory deeper, under their parent session's id) don't
/// carry it, which is what lets us tell the two apart from the path alone.
const SESSION_FILE_PREFIX: &str = "session-";

#[derive(Deserialize)]
struct ProjectRegistry {
    projects: HashMap<String, String>,
}

/// Resolves the slug gemini's project registry assigned to `cwd`, if any.
/// Split out from `session_root` so the parsing is unit-testable against a
/// fixture registry file instead of the real `~/.gemini`. No entry (or no
/// registry file at all) means gemini has never run in `cwd` — a real "no
/// sessions", not something to guess a path for.
fn project_slug(gemini_dir: &Path, cwd: &Path) -> Option<String> {
    let raw = fs::read_to_string(gemini_dir.join("projects.json")).ok()?;
    let registry: ProjectRegistry = serde_json::from_str(&raw).ok()?;
    let key = cwd.to_string_lossy();
    registry.projects.get(key.as_ref()).cloned()
}

#[derive(Deserialize)]
struct SessionIdLine {
    #[serde(rename = "sessionId")]
    session_id: String,
}

/// The id `--resume` matches on lives in the first JSONL record, not the
/// filename (see module doc). Bounded read, mirroring how gemini-cli's own
/// cleanup code recovers this same field (utils/sessionOperations.ts): a
/// well-formed metadata line is a few hundred bytes, so 4KB is generous
/// without risking reading an entire multi-MB transcript into memory for a
/// malformed one.
fn session_id_from_first_line(path: &Path) -> Option<String> {
    let mut buf = [0u8; 4096];
    let mut f = fs::File::open(path).ok()?;
    let n = f.read(&mut buf).ok()?;
    let first_line = std::str::from_utf8(&buf[..n]).ok()?.lines().next()?;
    serde_json::from_str::<SessionIdLine>(first_line).ok().map(|l| l.session_id)
}

pub struct GeminiAdapter;

impl AgentAdapter for GeminiAdapter {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn resume_flag(&self) -> &'static str {
        "--resume"
    }

    /// Already scoped to `cwd` by construction — the slug lookup is keyed on
    /// the cwd — the same way claude's per-project directory is, so like
    /// claude (and unlike pi), `owns_session_file` needs no override.
    fn session_root(&self, cwd: &Path) -> Option<PathBuf> {
        let gemini_dir = dirs::home_dir()?.join(".gemini");
        let slug = project_slug(&gemini_dir, cwd)?;
        Some(gemini_dir.join("tmp").join(slug).join("chats"))
    }

    /// See module doc: the filename only carries an 8-char id prefix, so the
    /// real work is opening the file and reading the id gemini itself would
    /// match on.
    fn session_id_from_path(&self, path: &Path) -> Option<String> {
        if path.extension()?.to_str()? != "jsonl" {
            return None; // legacy single-object `.json` sessions: not handled
        }
        let stem = path.file_stem()?.to_str()?;
        if !stem.starts_with(SESSION_FILE_PREFIX) {
            return None; // a subagent transcript, not a top-level session
        }
        session_id_from_first_line(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::test_support::{scratch_dir, RootAdapter};
    use super::super::SessionState;
    use std::collections::HashSet;
    use std::time::{Duration, SystemTime};

    #[test]
    fn launch_is_bare_gemini() {
        let cmd = GeminiAdapter.launch(Path::new("/tmp"));
        assert_eq!(cmd.program, "gemini");
        assert!(cmd.args.is_empty());
    }

    #[test]
    fn resume_uses_resume_flag() {
        let cmd = GeminiAdapter.resume(Path::new("/tmp"), "a1b2c3d4-e5f6-7890-abcd-ef1234567890");
        assert_eq!(cmd.program, "gemini");
        assert_eq!(cmd.args, vec!["--resume", "a1b2c3d4-e5f6-7890-abcd-ef1234567890"]);
    }

    #[test]
    fn project_slug_resolves_the_registered_cwd_and_nothing_else() {
        let d = scratch_dir("gemini-registry");
        std::fs::write(
            d.join("projects.json"),
            r#"{"projects":{"/home/nav/proj-x":"crimson-otter-42"}}"#,
        )
        .unwrap();
        assert_eq!(
            project_slug(&d, Path::new("/home/nav/proj-x")).as_deref(),
            Some("crimson-otter-42")
        );
        // A cwd gemini has never run in: no guessed slug.
        assert_eq!(project_slug(&d, Path::new("/home/nav/other")), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn project_slug_none_when_registry_file_is_absent() {
        let d = scratch_dir("gemini-no-registry");
        // No projects.json written — gemini has never run at all.
        assert_eq!(project_slug(&d, Path::new("/x")), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn session_id_round_trips_through_the_first_jsonl_record() {
        let d = scratch_dir("gemini-content");
        let file = d.join("session-2026-08-06T10-30-a1b2c3d4.jsonl");
        std::fs::write(
            &file,
            "{\"sessionId\":\"a1b2c3d4-e5f6-7890-abcd-ef1234567890\",\"projectHash\":\"x\"}\n{\"type\":\"user\"}\n",
        )
        .unwrap();
        assert_eq!(
            GeminiAdapter.session_id_from_path(&file).as_deref(),
            Some("a1b2c3d4-e5f6-7890-abcd-ef1234567890")
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ignores_subagent_transcripts_and_non_jsonl_files() {
        let d = scratch_dir("gemini-filter");
        // Subagent transcript: no `session-` prefix.
        let subagent = d.join("a1b2c3d4-e5f6-7890-abcd-ef1234567890.jsonl");
        std::fs::write(&subagent, "{\"sessionId\":\"nope\"}\n").unwrap();
        assert_eq!(GeminiAdapter.session_id_from_path(&subagent), None);
        // Legacy `.json` (not `.jsonl`) — intentionally unhandled.
        let legacy = d.join("session-2026-08-06T10-30-a1b2c3d4.json");
        std::fs::write(&legacy, "{\"sessionId\":\"a1b2c3d4-e5f6-7890-abcd-ef1234567890\"}").unwrap();
        assert_eq!(GeminiAdapter.session_id_from_path(&legacy), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn owns_session_file_is_true_because_the_root_is_already_cwd_scoped() {
        // Real cwd scoping happens one level up, in `project_slug` — by the
        // time a path reaches here it's already inside this cwd's own
        // `chats/` directory, so unconditional `true` is correct (matches
        // claude's model), not an unscoped default (contrast codex).
        let f = Path::new("/h/.gemini/tmp/crimson-otter-42/chats/session-x-a1b2c3d4.jsonl");
        assert!(GeminiAdapter.owns_session_file(f, Path::new("/home/nav/proj-x")));
        assert!(GeminiAdapter.owns_session_file(f, Path::new("/home/nav/unrelated")));
    }

    fn fixture(root: PathBuf) -> RootAdapter {
        RootAdapter::with_id_from_path(Some(root), |p| GeminiAdapter.session_id_from_path(p))
    }

    #[test]
    fn session_state_unknown_when_root_missing() {
        let missing = scratch_dir("gemini-missing");
        std::fs::remove_dir_all(&missing).unwrap();
        assert_eq!(fixture(missing).session_state(Path::new("/x"), "id"), SessionState::Unknown);
    }

    #[test]
    fn session_state_exists_then_gone() {
        let d = scratch_dir("gemini-state");
        std::fs::write(
            d.join("session-2026-08-06T10-30-a1b2c3d4.jsonl"),
            "{\"sessionId\":\"a1b2c3d4-e5f6-7890-abcd-ef1234567890\"}\n",
        )
        .unwrap();
        let a = fixture(d.clone());
        assert_eq!(
            a.session_state(Path::new("/x"), "a1b2c3d4-e5f6-7890-abcd-ef1234567890"),
            SessionState::Exists
        );
        assert_eq!(a.session_state(Path::new("/x"), "not-there"), SessionState::Gone);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn detect_session_finds_the_freshly_written_session_and_skips_taken_ids() {
        let d = scratch_dir("gemini-detect");
        let since = SystemTime::now();
        let file = d.join("session-2026-08-06T10-30-a1b2c3d4.jsonl");
        std::fs::write(&file, "{\"sessionId\":\"a1b2c3d4-e5f6-7890-abcd-ef1234567890\"}\n").unwrap();
        std::fs::File::open(&file).unwrap().set_modified(since + Duration::from_millis(10)).unwrap();

        let a = fixture(d.clone());
        assert_eq!(
            a.detect_session(Path::new("/x"), since, &HashSet::new()).as_deref(),
            Some("a1b2c3d4-e5f6-7890-abcd-ef1234567890")
        );
        let taken: HashSet<String> = ["a1b2c3d4-e5f6-7890-abcd-ef1234567890".to_string()].into();
        assert_eq!(a.detect_session(Path::new("/x"), since, &taken), None);
        let _ = std::fs::remove_dir_all(&d);
    }
}
