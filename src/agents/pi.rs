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
//!
//! Session-directory scoping (ROADMAP "[perf] Scope pi `session_state` to
//! the cwd"): pi's per-cwd subdirectory naming isn't a documented contract
//! (`encode_cwd` below is reverse-engineered, not spec'd), so `detect_session`
//! and `session_state` are overridden to *try* the directory it derives
//! first and always fall back to the full-root walk the trait default would
//! have done anyway — narrowing is a speed-up attempt, never the sole source
//! of truth. See the doc comments on those two overrides for the reasoning.

use super::{AgentAdapter, CommandSpec, SessionState};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// pi's own encoding of a cwd into a sessions subdirectory name. Reverse
/// engineered from `getDefaultSessionDirPath` in pi 0.81.1's
/// `session-manager.js` (`@earendil-works/pi-coding-agent`):
///
/// ```js
/// `--${resolvedCwd.replace(/^[/\\]/, "").replace(/[/\\:]/g, "-")}--`
/// ```
///
/// i.e. strip one leading path separator, turn every remaining `/`, `\`, or
/// `:` into `-`, and wrap the result in `--...--`. Verified byte-for-byte
/// against every directory under a real `~/.pi/agent/sessions` (macOS, pi
/// 0.81.1) — dots, case, and underscores all survive untouched, only the
/// three separator characters above turn into `-`.
///
/// Not trusted as the sole source of truth (see `owns_session_file`'s
/// fuzzy-compare, which is): pi computes this from *its own* resolved cwd
/// (tilde/`.`/`..` expansion, and whatever a future pi version changes this
/// format to), which we can't observe directly. Used only to guess a cheap
/// starting point for `detect_session`/`session_state` below.
pub fn encode_cwd(cwd: &Path) -> String {
    let s = cwd.to_string_lossy();
    let body = s.strip_prefix('/').or_else(|| s.strip_prefix('\\')).unwrap_or(&s);
    let encoded: String =
        body.chars().map(|c| if matches!(c, '/' | '\\' | ':') { '-' } else { c }).collect();
    format!("--{encoded}--")
}

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

    /// pi organizes sessions under ~/.pi/agent/sessions/ by cwd, in a
    /// subdirectory named by `encode_cwd`. This still returns the *whole*
    /// sessions root rather than that narrower guess: `detect_session` and
    /// `session_state` below are the ones that try the guess, and only after
    /// confirming it actually pans out — `session_root` alone has no way to
    /// signal "the guess missed, walk everything instead."
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

    /// Try `cwd`'s derived subdirectory (cheap: one `read_dir`, not the whole
    /// sessions tree) before falling back to the trait default's full-root
    /// walk. Correctness rides entirely on that fallback: a fresh launch
    /// always writes into whatever directory pi's *own* encoding of its cwd
    /// resolves to, so if our guess is right the file is there and we're
    /// done; if it's wrong (older/newer pi, or roost's cwd not matching pi's
    /// resolved one) the guess simply finds nothing and we fall through to
    /// exactly the unscoped scan this override replaces — same result, just
    /// not always paying the full O(all sessions) cost for it.
    fn detect_session(
        &self,
        cwd: &Path,
        since: SystemTime,
        taken: &HashSet<String>,
    ) -> Option<String> {
        let root = self.session_root(cwd)?;
        self.detect_session_in(&root, cwd, since, taken)
    }

    /// Same narrow-then-fallback shape as `detect_session`, but only the
    /// *positive* candidate result (`Exists`) is trusted on its own: finding
    /// the id there means it's genuinely on disk, full stop. Anything else
    /// the candidate directory says — missing, unreadable, id not in it —
    /// is NOT trusted, because unlike a fresh launch, a resume's session may
    /// have been written under an older/different cwd encoding than the one
    /// we'd derive today; only the unscoped fallback (querying the exact
    /// same whole root the un-narrowed code always did) gets to call Gone.
    fn session_state(&self, cwd: &Path, id: &str) -> SessionState {
        let Some(root) = self.session_root(cwd) else { return SessionState::Unknown };
        self.session_state_in(&root, cwd, id)
    }
}

impl PiAdapter {
    /// Core of `detect_session`, taking `root` explicitly so tests can point
    /// it at a fixture instead of the real `~/.pi` (which `session_root`
    /// resolves via `$HOME` and isn't worth redirecting globally just for a
    /// test). See `newest_unclaimed_session` for the actual matching rules —
    /// this only decides which root(s) to run them against.
    fn detect_session_in(
        &self,
        root: &Path,
        cwd: &Path,
        since: SystemTime,
        taken: &HashSet<String>,
    ) -> Option<String> {
        let candidate = root.join(encode_cwd(cwd));
        super::newest_unclaimed_session(self, &candidate, cwd, since, taken)
            .or_else(|| super::newest_unclaimed_session(self, root, cwd, since, taken))
    }

    /// Core of `session_state` — see `detect_session_in` for why `root` is a
    /// parameter, and `session_state`'s doc comment for why only the
    /// candidate's `Exists` result is trusted without falling through.
    fn session_state_in(&self, root: &Path, cwd: &Path, id: &str) -> SessionState {
        let candidate = root.join(encode_cwd(cwd));
        if super::session_file_state(self, &candidate, cwd, id) == SessionState::Exists {
            return SessionState::Exists;
        }
        super::session_file_state(self, root, cwd, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

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

    #[test]
    fn encode_cwd_matches_pis_own_dash_and_wrap_convention() {
        // Multi-segment path: leading `/` dropped, the rest turned to `-`,
        // wrapped in `--...--` — the exact shape confirmed against a real
        // ~/.pi/agent/sessions tree (pi 0.81.1).
        assert_eq!(encode_cwd(Path::new("/Users/nav/repos/roost")), "--Users-nav-repos-roost--");
        // Dots and case survive untouched (only `/`, `\`, `:` are targets).
        assert_eq!(encode_cwd(Path::new("/tmp/proj.v2/Sub")), "--tmp-proj.v2-Sub--");
        // `:` is a target too (pi's regex is `[/\\:]`, not just the
        // separator) — a literal colon in a path segment still collapses.
        assert_eq!(encode_cwd(Path::new("/a/b:c")), "--a-b-c--");
    }

    /// Unique scratch dir per test, isolated from any real `~/.pi` and from
    /// other tests running concurrently (pid + monotonic counter, same
    /// scheme as `agents::tests`' temp-dir fixtures).
    fn scratch_root(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("roost-pi-scope-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `since`-gating mirrors the sibling adapters' fixtures (gemini.rs,
    /// codex.rs): set an explicit mtime strictly after `since` rather than
    /// relying on wall-clock write order, which two `fs::write`s a few
    /// nanoseconds apart can't be trusted to preserve.
    fn touch_since(path: &Path, since: SystemTime) {
        std::fs::write(path, "").unwrap();
        std::fs::File::open(path).unwrap().set_modified(since + Duration::from_millis(10)).unwrap();
    }

    #[test]
    fn detect_session_hits_the_narrowed_candidate_directory() {
        let a = PiAdapter;
        let root = scratch_root("detect-candidate");
        let cwd = Path::new("/work/proj-a");
        let dir = root.join(encode_cwd(cwd));
        std::fs::create_dir_all(&dir).unwrap();
        let since = SystemTime::now();
        touch_since(&dir.join("2026-01-01T00-00-00-000Z_id-fast.jsonl"), since);

        assert_eq!(
            a.detect_session_in(&root, cwd, since, &HashSet::new()).as_deref(),
            Some("id-fast")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The regression this whole change is about: if `detect_session_in`
    /// only ever looked at the derived candidate directory (no fallback),
    /// this test fails, because the session here lives under a directory
    /// name `encode_cwd` would never produce (no `--` wrapping) — exactly
    /// the shape an older pi version, or a pi cwd-resolution difference,
    /// could leave on disk. The pre-existing full-root walk finds it via
    /// `owns_session_file`'s fuzzy compare; the narrowed fast path must
    /// still reach that same walk when its own guess comes up empty.
    #[test]
    fn detect_session_falls_back_when_the_real_directory_does_not_match_the_guess() {
        let a = PiAdapter;
        let root = scratch_root("detect-fallback");
        let cwd = Path::new("/work/proj-b");
        assert_ne!(encode_cwd(cwd), "work-proj-b"); // sanity: guess really differs
        let real_dir = root.join("work-proj-b"); // no --wrapping: not what encode_cwd guesses
        std::fs::create_dir_all(&real_dir).unwrap();
        let since = SystemTime::now();
        touch_since(&real_dir.join("2026-01-01T00-00-00-000Z_id-slow.jsonl"), since);

        assert_eq!(
            a.detect_session_in(&root, cwd, since, &HashSet::new()).as_deref(),
            Some("id-slow")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn detect_session_still_skips_taken_ids_through_the_narrowed_path() {
        let a = PiAdapter;
        let root = scratch_root("detect-taken");
        let cwd = Path::new("/work/proj-c");
        let dir = root.join(encode_cwd(cwd));
        std::fs::create_dir_all(&dir).unwrap();
        let since = SystemTime::now();
        touch_since(&dir.join("2026-01-01T00-00-00-000Z_id-taken.jsonl"), since);

        assert_eq!(
            a.detect_session_in(&root, cwd, since, &HashSet::new()).as_deref(),
            Some("id-taken")
        );
        let taken: HashSet<String> = ["id-taken".to_string()].into();
        assert_eq!(a.detect_session_in(&root, cwd, since, &taken), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn session_state_exists_via_the_narrowed_candidate_directory() {
        let a = PiAdapter;
        let root = scratch_root("state-candidate");
        let cwd = Path::new("/work/proj-d");
        let dir = root.join(encode_cwd(cwd));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("the-id.jsonl"), "").unwrap();

        assert_eq!(a.session_state_in(&root, cwd, "the-id"), SessionState::Exists);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Same regression as `detect_session_falls_back_...` for the resume
    /// path: an id that only lives under a non-`encode_cwd`-shaped directory
    /// must still resolve to `Exists`, not `Gone` — a false `Gone` here
    /// clears a perfectly good session id and loses the user's resume point.
    #[test]
    fn session_state_exists_via_fallback_when_the_real_directory_does_not_match_the_guess() {
        let a = PiAdapter;
        let root = scratch_root("state-fallback");
        let cwd = Path::new("/work/proj-e");
        let real_dir = root.join("work-proj-e"); // no --wrapping, same as the detect_session case
        std::fs::create_dir_all(&real_dir).unwrap();
        std::fs::write(real_dir.join("the-id.jsonl"), "").unwrap();

        assert_eq!(a.session_state_in(&root, cwd, "the-id"), SessionState::Exists);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn session_state_gone_when_absent_everywhere_under_a_present_root() {
        let a = PiAdapter;
        let root = scratch_root("state-gone");
        let cwd = Path::new("/work/proj-f");
        // The cwd's candidate dir exists (so the fast path isn't just
        // hitting "missing root") but has no file for this id, and nothing
        // elsewhere in root owns this cwd either.
        std::fs::create_dir_all(root.join(encode_cwd(cwd))).unwrap();

        assert_eq!(a.session_state_in(&root, cwd, "nonexistent-id"), SessionState::Gone);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn session_state_unknown_when_root_is_missing() {
        let a = PiAdapter;
        let root = scratch_root("state-missing");
        std::fs::remove_dir_all(&root).unwrap(); // existed a moment ago, now gone
        assert_eq!(
            a.session_state_in(&root, Path::new("/work/proj-g"), "any-id"),
            SessionState::Unknown
        );
    }
}
