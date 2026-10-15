//! omp (oh-my-pi, can1357/oh-my-pi) adapter.
//!
//! omp is a Bun-based fork of pi, but it is **not** pi with a new name: the
//! on-disk layout, the bucket encoding and the extension API all differ, so
//! this adapter is deliberately not a re-skin of `pi.rs`. Ground truth
//! (oh-my-pi v18.11.0 source):
//! - binary `omp`; `omp --resume <id>` (aliases `-r`, `--session`) takes an id
//!   prefix or a path, looks in the cwd's bucket first and then searches every
//!   project's bucket. `autoResume` defaults to false, so bare `omp` is fresh.
//! - sessions live at `~/.omp/agent/sessions/<bucket>/<timestamp>_<uuidv7>.jsonl`.
//!   Subagent transcripts and artifacts sit in a per-session directory *inside*
//!   the bucket (`<bucket>/<sessionId>/…`, also `.jsonl`); they are never
//!   top-level sessions and must not be mistaken for one.
//! - the bucket name is `getDefaultSessionDirName`
//!   (`packages/coding-agent/src/session/session-paths.ts`), see
//!   [`bucket_name`]. It is NOT pi's encoding.
//! - extensions are auto-discovered from `~/.omp/agent/extensions/*.ts`; roost
//!   installs `extensions/omp-roost.ts` there as `roost.ts`.
//!
//! Status/session detection is the same two-channel shape as pi: the bundled
//! extension reports exact events over roost's unix socket, and the newest
//! session file under the bucket is the filesystem fallback.

use super::{AgentAdapter, SessionState};
use std::path::{Path, PathBuf};

pub struct OmpAdapter;

/// `~/.omp/agent/sessions`, the whole root (buckets are scoped by
/// `owns_session_file`).
fn sessions_root() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".omp").join("agent").join("sessions"))
}

/// omp's `encodeRelativeSessionDirName`: separators (`/`, `\`, `:`) become
/// dashes; an empty remainder is just the prefix, otherwise the prefix and the
/// encoded remainder are joined with exactly one dash.
fn encode_relative(prefix: &str, rel: &Path) -> String {
    let enc: String = rel
        .to_string_lossy()
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':') { '-' } else { c })
        .collect();
    if enc.is_empty() {
        prefix.to_string()
    } else if prefix.ends_with('-') {
        format!("{prefix}{enc}")
    } else {
        format!("{prefix}-{enc}")
    }
}

/// The session bucket directory name omp derives from a working directory.
/// Pure: the caller canonicalizes all three paths (omp realpaths cwd, home and
/// the temp root before comparing) and supplies `home` / `tmp`.
///
/// The temp root is tested FIRST (it is the more specific root where it nests
/// inside home), then home, then the absolute fallback:
/// - `/tmp/foo/bar` -> `-tmp-foo-bar`, `/tmp` -> `-tmp`
/// - `$HOME/code/proj` -> `-code-proj`, `$HOME` -> `-`
/// - `/opt/work` -> `--opt-work--`
pub(crate) fn bucket_name(cwd: &Path, home: &Path, tmp: &Path) -> String {
    if let Ok(rel) = cwd.strip_prefix(tmp) {
        return encode_relative("-tmp", rel);
    }
    if let Ok(rel) = cwd.strip_prefix(home) {
        return encode_relative("-", rel);
    }
    let abs = cwd.to_string_lossy();
    let abs = abs.strip_prefix(['/', '\\']).unwrap_or(&abs);
    let enc: String =
        abs.chars().map(|c| if matches!(c, '/' | '\\' | ':') { '-' } else { c }).collect();
    format!("--{enc}--")
}

/// `std::fs::canonicalize` with omp's own fallback to the input.
fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// [`bucket_name`] for a live `cwd`: canonicalizes everything and supplies the
/// real home and temp dirs. `None` only when there is no home directory.
fn omp_bucket_name(cwd: &Path) -> Option<String> {
    let home = canon(&dirs::home_dir()?);
    let tmp = canon(&std::env::temp_dir());
    Some(bucket_name(&canon(cwd), &home, &tmp))
}

/// Session id from `<timestamp>_<uuid>.jsonl`: the segment after the last `_`
/// of the stem, and only for a `.jsonl` file. `--resume` matches on the bare id
/// (or a prefix), not the timestamped file name.
fn id_from_path(path: &Path) -> Option<String> {
    if path.extension()?.to_str()? != "jsonl" {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    Some(stem.rsplit('_').next().unwrap_or(stem).to_string())
}

/// Is `id` a top-level session under `root`, in ANY bucket? omp's
/// `--resume <id>` falls back to a global search across all projects, so a
/// session that lives in another bucket (the project moved, or the pane's cwd
/// is spelled differently) is still resumable and must not be declared Gone.
///
/// Only `<root>/<bucket>/<file>.jsonl`, exactly two levels deep, counts: the
/// per-session directories inside a bucket hold subagent transcripts and
/// artifacts, not sessions. An unreadable root is Unknown (don't wipe a
/// possibly-good id on a permission hiccup); an absent id is Gone.
fn omp_session_state(root: &Path, id: &str) -> SessionState {
    let Ok(buckets) = std::fs::read_dir(root) else { return SessionState::Unknown };
    for bucket in buckets.flatten() {
        let dir = bucket.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&dir) else { continue };
        for f in files.flatten() {
            let p = f.path();
            if p.is_file() && id_from_path(&p).as_deref() == Some(id) {
                return SessionState::Exists;
            }
        }
    }
    SessionState::Gone
}

impl AgentAdapter for OmpAdapter {
    fn id(&self) -> &'static str {
        "omp"
    }

    fn resume_flag(&self) -> &'static str {
        "--resume"
    }

    /// The whole sessions root; `owns_session_file` scopes matches to `cwd`.
    // ponytail: O(all sessions) walk per detect, same trade-off as pi.
    fn session_root(&self, _cwd: &Path) -> Option<PathBuf> {
        sessions_root()
    }

    fn session_id_from_path(&self, path: &Path) -> Option<String> {
        id_from_path(path)
    }

    /// True iff the file sits directly in this cwd's bucket. Comparing the
    /// parent directory's name to the exact computed bucket (not a lossy
    /// alphanumeric-runs match like pi's) also excludes subagent transcripts
    /// nested under `<bucket>/<sessionId>/`, whose parent is the session id.
    ///
    /// Still lossy where omp itself is: `a/b` and `a-b` encode identically.
    fn owns_session_file(&self, path: &Path, cwd: &Path) -> bool {
        let Some(want) = omp_bucket_name(cwd) else { return false };
        path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()) == Some(want.as_str())
    }

    /// Global, not per-cwd — see [`omp_session_state`].
    fn session_state(&self, _cwd: &Path, id: &str) -> SessionState {
        let Some(root) = sessions_root() else { return SessionState::Unknown };
        omp_session_state(&root, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::newest_unclaimed_session;
    use crate::agents::test_support::scratch_dir;
    use std::collections::HashSet;
    use std::time::SystemTime;

    fn b(cwd: &str) -> String {
        bucket_name(Path::new(cwd), Path::new("/home/me"), Path::new("/tmp"))
    }

    #[test]
    fn bucket_home_relative() {
        assert_eq!(b("/home/me/code/proj-x"), "-code-proj-x");
    }

    #[test]
    fn bucket_home_itself_is_a_single_dash() {
        assert_eq!(b("/home/me"), "-");
    }

    #[test]
    fn bucket_tmp_relative_and_tmp_itself() {
        assert_eq!(b("/tmp/foo/bar"), "-tmp-foo-bar");
        assert_eq!(b("/tmp"), "-tmp");
    }

    #[test]
    fn bucket_checks_tmp_before_home_when_tmp_is_under_home() {
        let name = bucket_name(
            Path::new("/home/me/tmp/x"),
            Path::new("/home/me"),
            Path::new("/home/me/tmp"),
        );
        assert_eq!(name, "-tmp-x");
        // and the tmp root itself, which is also inside home
        let name = bucket_name(
            Path::new("/home/me/tmp"),
            Path::new("/home/me"),
            Path::new("/home/me/tmp"),
        );
        assert_eq!(name, "-tmp");
    }

    #[test]
    fn bucket_absolute_is_double_dash_wrapped() {
        assert_eq!(b("/opt/work"), "--opt-work--");
    }

    #[test]
    fn bucket_prefix_match_is_component_wise() {
        // /home/me2 is not inside /home/me, /tmpfoo is not inside /tmp.
        assert_eq!(b("/home/me2/x"), "--home-me2-x--");
        assert_eq!(b("/tmpfoo"), "--tmpfoo--");
    }

    #[test]
    fn bucket_distinguishes_a_dash_only_difference() {
        assert_ne!(b("/home/me/my-project"), b("/home/me/myproject"));
        assert_ne!(b("/opt/my-project"), b("/opt/myproject"));
    }

    #[test]
    fn bucket_replaces_colon_and_backslash() {
        assert_eq!(b("/opt/a:b/c"), "--opt-a-b-c--");
        assert_eq!(b("/home/me/a:b/c"), "-a-b-c");
        assert_eq!(
            bucket_name(Path::new("/home/me/x\\y"), Path::new("/home/me"), Path::new("/tmp")),
            "-x-y"
        );
    }

    #[test]
    fn extracts_bare_uuid_from_omp_filename() {
        let a = OmpAdapter;
        let p = Path::new(
            "/h/.omp/agent/sessions/-proj/2026-10-15T09-17-57-467Z_019f7ed1-5b5a-72cc-b89a-2c4fd41a0006.jsonl",
        );
        assert_eq!(
            a.session_id_from_path(p).as_deref(),
            Some("019f7ed1-5b5a-72cc-b89a-2c4fd41a0006")
        );
    }

    #[test]
    fn non_jsonl_files_are_not_sessions() {
        let a = OmpAdapter;
        assert_eq!(a.session_id_from_path(Path::new("/h/s/-p/ts_abc.json")), None);
        assert_eq!(a.session_id_from_path(Path::new("/h/s/-p/ts_abc.jsonl.tmp")), None);
        assert_eq!(a.session_id_from_path(Path::new("/h/s/-p/ts_abc")), None);
    }

    #[test]
    fn resume_uses_resume_flag() {
        let cmd = OmpAdapter.resume(Path::new("/tmp"), "abc-123");
        assert_eq!(cmd.program, "omp");
        assert_eq!(cmd.args, vec!["--resume", "abc-123"]);
        assert_eq!(OmpAdapter.launch(Path::new("/tmp")).args, Vec::<String>::new());
    }

    /// Real wrapper against a real directory: the bucket is derived from the
    /// canonicalized cwd, a sibling whose path differs only by a dash must not
    /// match, and a nested subagent transcript is not owned.
    #[test]
    fn owns_session_file_matches_the_exact_bucket_only() {
        let a = OmpAdapter;
        let base = scratch_dir("omp-own");
        let cwd = base.join("my-project");
        std::fs::create_dir_all(&cwd).unwrap();
        let bucket = omp_bucket_name(&cwd).unwrap();
        let sessions = Path::new("/h/.omp/agent/sessions");

        let f = sessions.join(&bucket).join("ts_aaaa.jsonl");
        assert!(a.owns_session_file(&f, &cwd), "its own bucket matches");
        assert!(
            !a.owns_session_file(&f, &base.join("myproject")),
            "a different project whose path differs only by a dash must NOT match"
        );
        assert!(!a.owns_session_file(&f, &base), "a parent directory is a different bucket");
        // subagent transcript nested under <bucket>/<sessionId>/
        let nested = sessions.join(&bucket).join("019f7ed1-aaaa").join("sub_bbbb.jsonl");
        assert!(!a.owns_session_file(&nested, &cwd), "nested subagent file is not a session");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn session_state_finds_a_session_in_any_bucket() {
        let root = scratch_dir("omp-state");
        let other = root.join("--opt-elsewhere--");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("2026-10-15T00-00-00-000Z_the-id.jsonl"), "").unwrap();
        assert_eq!(omp_session_state(&root, "the-id"), SessionState::Exists);
        assert_eq!(omp_session_state(&root, "other-id"), SessionState::Gone);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn session_state_ignores_nested_subagent_and_non_jsonl_files() {
        let root = scratch_dir("omp-state-nested");
        let nested = root.join("-proj").join("019f7ed1-aaaa");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("ts_sub-id.jsonl"), "").unwrap();
        std::fs::write(root.join("-proj").join("ts_txt-id.txt"), "").unwrap();
        // a stray file directly in the root is not a bucket either
        std::fs::write(root.join("ts_root-id.jsonl"), "").unwrap();
        assert_eq!(omp_session_state(&root, "sub-id"), SessionState::Gone);
        assert_eq!(omp_session_state(&root, "txt-id"), SessionState::Gone);
        assert_eq!(omp_session_state(&root, "root-id"), SessionState::Gone);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn session_state_unknown_when_the_root_is_unreadable() {
        let root = scratch_dir("omp-state-missing");
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(omp_session_state(&root, "id"), SessionState::Unknown);
    }

    /// Detection through the shared scanner on a temp tree laid out like
    /// omp's: the pane's own bucket wins, other buckets and nested subagent
    /// files are ignored, and a claimed id is skipped.
    #[test]
    fn detects_the_newest_unclaimed_session_in_the_cwd_bucket() {
        let a = OmpAdapter;
        let base = scratch_dir("omp-detect");
        let cwd = base.join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let root = base.join("sessions");
        let mine = root.join(omp_bucket_name(&cwd).unwrap());
        let theirs = root.join("--opt-other--");
        let nested = mine.join("019f7ed1-aaaa");
        for d in [&mine, &theirs, &nested] {
            std::fs::create_dir_all(d).unwrap();
        }
        let before = SystemTime::UNIX_EPOCH;
        std::fs::write(theirs.join("ts_other-proj.jsonl"), "").unwrap();
        std::fs::write(nested.join("ts_subagent.jsonl"), "").unwrap();
        std::fs::write(mine.join("ts_first.jsonl"), "").unwrap();
        // make `second` strictly newer than `first`
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(mine.join("ts_second.jsonl"), "").unwrap();

        let mut taken = HashSet::new();
        assert_eq!(
            newest_unclaimed_session(&a, &root, &cwd, before, &taken).as_deref(),
            Some("second")
        );
        taken.insert("second".to_string());
        assert_eq!(
            newest_unclaimed_session(&a, &root, &cwd, before, &taken).as_deref(),
            Some("first")
        );
        taken.insert("first".to_string());
        assert_eq!(newest_unclaimed_session(&a, &root, &cwd, before, &taken), None);
        let _ = std::fs::remove_dir_all(&base);
    }
}
