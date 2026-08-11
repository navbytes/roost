//! Per-tool adapters: each knows how to launch its CLI fresh, resume a
//! specific session, detect a new session's id, and interpret status signals.
//!
//! Design doc §6. The pi adapter is the v1 flagship; `shell` is the generic
//! fallback for arbitrary commands.

pub mod claude;
pub mod codex;
pub mod gemini;
pub mod opencode;
pub mod pi;
pub mod shell;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Whether a stored session id still resolves to a resumable session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// The session file is present — resume it.
    Exists,
    /// The session is definitively gone (dir readable, id absent) — launch
    /// fresh and clear the dead id.
    Gone,
    /// Can't tell (no session root, or the root is momentarily unreadable) —
    /// attempt resume but do NOT clear the id.
    Unknown,
}

#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

impl CommandSpec {
    pub fn new(program: impl Into<String>, cwd: &Path) -> Self {
        Self { program: program.into(), args: vec![], cwd: cwd.to_path_buf(), env: vec![] }
    }
    pub fn arg(mut self, a: impl Into<String>) -> Self {
        self.args.push(a.into());
        self
    }

    /// The command as a pasteable one-liner for a plain terminal:
    /// `cd -- <cwd> && <program> <args…>`. `--` because quoting can't save a
    /// cwd that *starts* with `-` (`-` is shell_word-safe, and cd parses
    /// options after quote removal anyway). `env` is deliberately absent — it
    /// carries roost plumbing (ROOST_SOCK/ROOST_TOKEN), not anything a human
    /// resuming outside roost should replay.
    pub fn shell_line(&self) -> String {
        let cwd = shell_word(&self.cwd.to_string_lossy());
        let mut line = format!("cd -- {cwd} && {}", shell_word(&self.program));
        for a in &self.args {
            line.push(' ');
            line.push_str(&shell_word(a));
        }
        line
    }
}

/// Quote one word for a POSIX shell, only when it needs it: bare
/// `[A-Za-z0-9_./:=@%+,-]` tokens (paths, uuids, flags) pass through so the
/// pasteable line stays readable; anything else is single-quoted with
/// embedded `'` escaped as `'\''`.
fn shell_word(s: &str) -> String {
    let safe = !s.is_empty()
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_./:=@%+,-".contains(&b));
    if safe { s.to_string() } else { format!("'{}'", s.replace('\'', r"'\''")) }
}

pub trait AgentAdapter: Send + Sync {
    fn id(&self) -> &'static str;

    /// Command to start a brand-new session in `cwd`. Default: the bare
    /// program named by `id()` — true for every adapter here except `shell`
    /// (spawns `$SHELL`, not a program literally named "shell"), which
    /// overrides this directly.
    fn launch(&self, cwd: &Path) -> CommandSpec {
        CommandSpec::new(self.id(), cwd)
    }

    /// Command to resume the given session id in `cwd`. Default:
    /// `launch(cwd)` plus `resume_flag()` then the bare id — covers every
    /// "program [--]flag id" resume shape here (pi, claude, codex, gemini,
    /// opencode). `shell` ignores the session id entirely and overrides this
    /// directly instead.
    fn resume(&self, cwd: &Path, session: &str) -> CommandSpec {
        self.launch(cwd).arg(self.resume_flag()).arg(session)
    }

    /// Flag (or bare subcommand) the default `resume` above puts before the
    /// session id, e.g. `"--resume"`, `"--session"`, or `"resume"`.
    /// Deliberately no default: a new adapter must state its resume shape,
    /// not silently inherit one. Adapters that override `resume` directly
    /// (shell, test doubles) still declare it; theirs is never consulted.
    fn resume_flag(&self) -> &'static str;

    /// Where this tool stores its session files for `cwd`, if it has any.
    fn session_root(&self, _cwd: &Path) -> Option<PathBuf> {
        None
    }

    /// Turn a session file path into the id the resume command expects.
    fn session_id_from_path(&self, path: &Path) -> Option<String> {
        path.file_stem().map(|s| s.to_string_lossy().into_owned())
    }

    /// Does this session file belong to a pane in `cwd`? Adapters that store
    /// every session in one flat directory can't tell (default: yes). Those
    /// that organize by working directory override this to scope detection to
    /// the pane's own project, so two agents in different folders launched at
    /// once can't cross-detect.
    fn owns_session_file(&self, _path: &Path, _cwd: &Path) -> bool {
        true
    }

    /// Learn the session id of a freshly launched pane by finding the session
    /// file written since spawn. The exact channel (extension handshake over
    /// the status socket) takes precedence when available; this is the
    /// filesystem fallback.
    ///
    /// `taken` holds session ids already claimed by other panes. When several
    /// agents launch in the same directory at once they share one session
    /// root, so we walk candidate files newest-first and skip any id another
    /// pane already owns — otherwise two panes cross-wire onto one session.
    fn detect_session(
        &self,
        cwd: &Path,
        since: SystemTime,
        taken: &HashSet<String>,
    ) -> Option<String> {
        let root = self.session_root(cwd)?;
        newest_unclaimed_session(self, &root, cwd, since, taken)
    }

    /// Is a resumable session with this id still on disk? Distinguishes
    /// "definitely gone" from "can't tell" so a transient read error never
    /// discards a still-valid resume pointer. The default reuses
    /// `session_root` + `session_id_from_path`, so pi and claude get it for
    /// free; adapters without a session root (shell) return Unknown.
    fn session_state(&self, cwd: &Path, id: &str) -> SessionState {
        let Some(root) = self.session_root(cwd) else { return SessionState::Unknown };
        session_file_state(self, &root, cwd, id)
    }
}

/// Is `id` a plausible session id we're willing to hand to `pi --session` /
/// `claude --resume`? On the spawn path no shell is involved (ids are passed
/// as separate argv tokens) and this is defense-in-depth behind
/// SessionResolver; for `App::resume_command_line` — the dead-pane bar's `y`,
/// which renders the id into a pasteable *shell* line via `shell_line` — it
/// is the load-bearing guard: it rejects a
/// tampered `workspace.json` or a poisoned status-socket message trying to
/// steer resume at an attacker-chosen path, a flag (leading `-`), or something
/// that isn't an id at all. Real ids from pi/claude are UUID/hex-with-dashes;
/// we allow that plus `_`/`.` and cap the length, and reject empties, control
/// chars, path separators, `..`, and leading dashes.
pub fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && !id.starts_with('-')
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Files under `root` (recursive) modified after `since`, newest first.
/// Used to spot the session file a freshly launched agent just created.
pub fn session_files_since(root: &Path, since: SystemTime) -> Vec<PathBuf> {
    let mut found: Vec<(SystemTime, PathBuf)> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let Ok(mtime) = meta.modified() else { continue };
            if mtime > since {
                found.push((mtime, p));
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0)); // newest first
    found.into_iter().map(|(_, p)| p).collect()
}

/// Shared core of `AgentAdapter::detect_session`'s default: the newest file
/// under `root` that `adapter` owns for `cwd` and that isn't in `taken`.
/// Pulled out to a free function (rather than left inline in the trait
/// default) so an adapter that overrides `detect_session` to try a cheaper
/// candidate root first (pi, below) can fall back to scanning a wider root
/// through the exact same matching rules — not a re-derived copy of them.
pub fn newest_unclaimed_session<A: AgentAdapter + ?Sized>(
    adapter: &A,
    root: &Path,
    cwd: &Path,
    since: SystemTime,
    taken: &HashSet<String>,
) -> Option<String> {
    for path in session_files_since(root, since) {
        if !adapter.owns_session_file(&path, cwd) {
            continue;
        }
        if let Some(id) = adapter.session_id_from_path(&path) {
            if !taken.contains(&id) {
                return Some(id);
            }
        }
    }
    None
}

/// Shared core of `AgentAdapter::session_state`'s default, once a root is in
/// hand: is `id` present under `root` and owned by `cwd`? See
/// `newest_unclaimed_session` for why this is a free function.
///
/// A missing/unreadable `root` is Unknown, not Gone — don't wipe a possibly-
/// still-valid resume pointer just because we can't currently read the
/// directory (permission hiccup, or — for a narrowed root passed in by an
/// override — simply a cwd whose subdirectory doesn't exist yet). Only an id
/// absent from a root that's actually present and readable is Gone.
pub fn session_file_state<A: AgentAdapter + ?Sized>(adapter: &A, root: &Path, cwd: &Path, id: &str) -> SessionState {
    if std::fs::read_dir(root).is_err() {
        return SessionState::Unknown;
    }
    let exists = session_files_since(root, SystemTime::UNIX_EPOCH).iter().any(|p| {
        adapter.owns_session_file(p, cwd) && adapter.session_id_from_path(p).as_deref() == Some(id)
    });
    if exists {
        SessionState::Exists
    } else {
        SessionState::Gone
    }
}

pub type Registry = HashMap<&'static str, Box<dyn AgentAdapter>>;

/// The single source of truth for which adapters exist, in user-facing display
/// order (agents first, the generic shell last). `registry()` and the launch
/// picker both derive from this, so adding an adapter is a one-line change here
/// rather than three places that can silently diverge.
fn adapter_specs() -> Vec<Box<dyn AgentAdapter>> {
    vec![
        Box::new(pi::PiAdapter),
        Box::new(claude::ClaudeAdapter),
        Box::new(codex::CodexAdapter),
        Box::new(gemini::GeminiAdapter),
        Box::new(opencode::OpencodeAdapter),
        Box::new(shell::ShellAdapter),
    ]
}

pub fn registry() -> Registry {
    let mut m: Registry = HashMap::new();
    for a in adapter_specs() {
        m.insert(a.id(), a);
    }
    m
}

/// Adapter ids in picker order, derived from `adapter_specs()`.
pub fn picker_ids() -> Vec<&'static str> {
    adapter_specs().iter().map(|a| a.id()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_validation_accepts_ids_rejects_paths_and_flags() {
        // Real-shaped ids pass.
        assert!(valid_session_id("3f9a1c2e-7b4d-4a11-9c2e-0f1a2b3c4d5e"));
        assert!(valid_session_id("abc_123.session"));
        // Hostile / malformed values are rejected.
        assert!(!valid_session_id("")); // empty
        assert!(!valid_session_id("../../etc/passwd")); // traversal
        assert!(!valid_session_id("/home/attacker/evil")); // path
        assert!(!valid_session_id("-oProxyCommand=evil")); // leading-dash flag
        assert!(!valid_session_id("has space")); // whitespace
        assert!(!valid_session_id("nul\0byte")); // control char
        assert!(!valid_session_id(&"x".repeat(257))); // too long
    }

    #[test]
    fn shell_line_quotes_only_what_needs_it() {
        // The common case — plain path, uuid, flag — reads clean, no quotes.
        let cmd = CommandSpec::new("pi", Path::new("/Users/x/repos/roost"))
            .arg("--session")
            .arg("019fe044-8236");
        assert_eq!(cmd.shell_line(), "cd -- /Users/x/repos/roost && pi --session 019fe044-8236");
        // Spaces and embedded quotes get POSIX single-quoting ('\'' escape).
        let cmd = CommandSpec::new("pi", Path::new("/tmp/my proj")).arg("it's");
        assert_eq!(cmd.shell_line(), r"cd -- '/tmp/my proj' && pi 'it'\''s'");
    }

    #[test]
    fn session_state_unknown_without_a_root() {
        assert_eq!(
            test_support::RootAdapter::new(None).session_state(Path::new("/x"), "id"),
            SessionState::Unknown
        );
    }

    #[test]
    fn session_state_unknown_when_root_dir_missing() {
        // A missing session ROOT (as opposed to an absent id within a present
        // one) can't tell us the id is stale — the tool may have changed its
        // on-disk layout, or $HOME may have resolved differently. Treat it as
        // Unknown so the caller still attempts resume instead of nuking a
        // possibly-good id.
        let d = test_support::scratch_dir("ss-missing");
        std::fs::remove_dir_all(&d).unwrap();
        assert_eq!(
            test_support::RootAdapter::new(Some(d)).session_state(Path::new("/x"), "id"),
            SessionState::Unknown
        );
    }

    #[test]
    fn session_state_exists_when_file_present() {
        let d = test_support::scratch_dir("ss-present");
        std::fs::write(d.join("the-id.jsonl"), "").unwrap();
        // default session_id_from_path = file stem = "the-id"
        assert_eq!(
            test_support::RootAdapter::new(Some(d.clone())).session_state(Path::new("/x"), "the-id"),
            SessionState::Exists
        );
        assert_eq!(
            test_support::RootAdapter::new(Some(d.clone())).session_state(Path::new("/x"), "other"),
            SessionState::Gone
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// Shared by adapters' `#[cfg(test)]` modules: every adapter's session-state
/// tests need (a) an adapter whose `session_root` points at a temp dir
/// instead of the real `~/.<tool>`, and (b) that temp dir itself. One double
/// + one helper here rather than each adapter file re-inventing both.
#[cfg(test)]
pub(crate) mod test_support {
    use super::{AgentAdapter, CommandSpec};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Fresh, empty temp directory, isolated from other tests (pid +
    /// monotonic counter) and from the real filesystem the adapter would
    /// otherwise touch.
    pub(crate) fn scratch_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("roost-agents-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Adapter whose session root is a caller-supplied path, so
    /// `session_state`/`detect_session` (trait defaults) can be exercised
    /// deterministically against a temp dir instead of the real `~/.<tool>`.
    /// `id_from_path` defaults to the trait's own default (file stem);
    /// `with_id_from_path` swaps in a real adapter's parsing (e.g. codex's
    /// rollout-filename UUID extraction, gemini's first-line JSON read) via a
    /// non-capturing closure, which coerces to a plain `fn` pointer.
    pub(crate) struct RootAdapter {
        root: Option<PathBuf>,
        id_from_path: fn(&Path) -> Option<String>,
    }
    impl RootAdapter {
        pub(crate) fn new(root: Option<PathBuf>) -> Self {
            Self::with_id_from_path(root, |p| {
                p.file_stem().map(|s| s.to_string_lossy().into_owned())
            })
        }
        pub(crate) fn with_id_from_path(
            root: Option<PathBuf>,
            id_from_path: fn(&Path) -> Option<String>,
        ) -> Self {
            Self { root, id_from_path }
        }
    }
    impl AgentAdapter for RootAdapter {
        fn id(&self) -> &'static str {
            "root"
        }
        fn launch(&self, cwd: &Path) -> CommandSpec {
            CommandSpec::new("true", cwd)
        }
        fn resume(&self, cwd: &Path, session: &str) -> CommandSpec {
            CommandSpec::new("true", cwd).arg(session)
        }
        fn resume_flag(&self) -> &'static str {
            "--resume" // unused: resume() overridden above
        }
        fn session_root(&self, _cwd: &Path) -> Option<PathBuf> {
            self.root.clone()
        }
        fn session_id_from_path(&self, path: &Path) -> Option<String> {
            (self.id_from_path)(path)
        }
    }
}
