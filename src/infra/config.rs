//! config.json — the key-bindings escape hatch (`ui::input::Keymap`), read
//! once at startup.
//!
//! Two locations are accepted, because the honest answer to "where does this
//! file go" was for a long time the wrong one for anybody who hadn't read the
//! docs. `workspace.json` lives in the XDG **state** dir and belongs there —
//! it is machine-written on every pane split, exactly the spec's "current
//! layout". `config.json` is the opposite: hand-authored, edited once, the
//! sort of thing people keep in a dotfiles repo. It sat beside
//! `workspace.json` anyway, so the one file a user is ever expected to write
//! was the one file they could not find.
//!
//! So, in order:
//!
//! 1. `$ROOST_STATE` set → `$ROOST_STATE/config.json`, and **no fallback**.
//!    That variable's whole contract is that one knob redirects the entire
//!    instance (workspace, socket, keymap); an isolated instance silently
//!    inheriting the global keymap would break the "try a remap without
//!    touching your own" case the escape hatch exists for.
//! 2. Otherwise, whichever of `<state dir>/config.json` (where roost has
//!    always looked) and `<config dir>/roost/config.json` (`~/.config/roost/`
//!    on Linux) exists. An existing state-dir file **wins**, so no install's
//!    behavior changes under it; when both exist the ignored one gets a
//!    notice rather than vanishing silently. When neither exists there is
//!    nothing to read either way, and roost *names* the config-dir path —
//!    that is the answer a new user gets from `roost keys`, and it should
//!    point at `~/.config`.
//!
//! On macOS the two candidates are the **same directory**: `dirs::state_dir`
//! is `None` there, so the state dir falls back to `dirs::data_local_dir`
//! (`~/Library/Application Support`), which is also what `dirs::config_dir`
//! returns. `resolve_in` collapses that case rather than reporting a file as
//! shadowing itself.
//!
//! For a **named** workspace (`roost -w x`) the candidates change: it reads
//! the *root* `config.json`, and a `config.json` inside the workspace's own
//! directory replaces it **wholesale** — never merged, so a workspace-local
//! file is the whole keymap for that workspace and nothing bleeds in from
//! the root. When both exist the ignored root file is reported as the
//! shadow, exactly like the two-candidate case above. The default workspace
//! and plain `$ROOST_STATE` isolation keep the resolution above, unchanged.

use std::path::{Path, PathBuf};

use crate::infra::store::{FsStore, DEFAULT_WORKSPACE};
use crate::ui::input::{Diagnostics, Keymap};

const FILE: &str = "config.json";

/// Which `config.json` roost reads, and what that choice hid.
#[derive(Debug, PartialEq, Eq)]
pub struct Resolved {
    /// The file roost reads — or, when nothing exists, the one it would
    /// read, which is the path worth telling the user to create.
    pub path: PathBuf,
    /// Whether `path` is actually there. Distinguishes "reading your config"
    /// from "no config anywhere"; both are ordinary.
    pub exists: bool,
    /// A second `config.json` that exists but is being ignored. Only ever
    /// set when both candidates exist and are different files.
    pub shadowed: Option<PathBuf>,
}

/// The real resolution: reads the environment, stats the filesystem.
pub fn resolve_config() -> Resolved {
    // A named workspace never sees the two-candidate logic below: its base
    // is the root file, and its own directory replaces it wholesale.
    if FsStore::workspace_name() != DEFAULT_WORKSPACE {
        return resolve_named(
            FsStore::root_dir().join(FILE),
            FsStore::state_dir().join(FILE),
            &|p| p.is_file(),
        );
    }
    if let Some(dir) = std::env::var_os("ROOST_STATE") {
        let path = PathBuf::from(dir).join(FILE);
        let exists = path.is_file();
        return Resolved { path, exists, shadowed: None };
    }
    let state = FsStore::state_dir().join(FILE);
    let xdg = dirs::config_dir().map(|d| d.join("roost").join(FILE));
    resolve_in(state, xdg, &|p| p.is_file())
}

/// The named-workspace decision alone — no env, no filesystem — so it can
/// be unit-tested like `resolve_in`. The workspace directory's file, when
/// it exists, *replaces* the root's (D5: whole-file, never merged — the
/// keymap loader takes one file, and merging keymap tables is speculative).
/// Without one, the root file is what it reads; the ignored root file is
/// reported as the shadow so an edit with no effect is never silent.
fn resolve_named(root: PathBuf, local: PathBuf, exists: &dyn Fn(&Path) -> bool) -> Resolved {
    if exists(&local) {
        let shadowed = if exists(&root) { Some(root.clone()) } else { None };
        return Resolved { path: local, exists: true, shadowed };
    }
    let root_exists = exists(&root);
    Resolved { path: root, exists: root_exists, shadowed: None }
}

/// The decision alone — no env, no filesystem — so it can be unit-tested
/// exhaustively without the process-global raciness described on the tests
/// below. `exists` answers "is there a file here".
fn resolve_in(state: PathBuf, xdg: Option<PathBuf>, exists: &dyn Fn(&Path) -> bool) -> Resolved {
    // No config dir to fall back to (`dirs` can't name one): the state dir is
    // the only answer there has ever been.
    let Some(xdg) = xdg else {
        let exists = exists(&state);
        return Resolved { path: state, exists, shadowed: None };
    };
    // macOS: both candidates name the same file. One file, never a shadow.
    if xdg == state {
        let exists = exists(&state);
        return Resolved { path: state, exists, shadowed: None };
    }
    match (exists(&state), exists(&xdg)) {
        // Both: the long-standing location wins so that upgrading roost
        // never quietly swaps which file is in force. The other is named,
        // not dropped.
        (true, true) => Resolved { path: state, exists: true, shadowed: Some(xdg) },
        (true, false) => Resolved { path: state, exists: true, shadowed: None },
        (false, true) => Resolved { path: xdg, exists: true, shadowed: None },
        // Neither: nothing is read, and the path roost reports is advice
        // about where to put one — so it is the discoverable one.
        (false, false) => Resolved { path: xdg, exists: false, shadowed: None },
    }
}

/// Read + parse config.json from wherever `resolve_config` lands. Never
/// fatal (design doc): a missing file — the common case — is silently
/// today's defaults, no diagnostics; any other read failure (permissions, a
/// directory sitting where the file should be, ...) degrades the same way
/// malformed content does, via one diagnostic naming the problem. Parsing
/// itself is `Keymap::parse`'s job, unit-tested directly in `ui::input`.
pub fn load_keymap() -> (Keymap, Diagnostics) {
    let resolved = resolve_config();
    let (keymap, mut diagnostics) = load_keymap_from(&resolved.path);
    if let Some(shadowed) = resolved.shadowed {
        // A notice, not a problem: both files are valid, roost read one, and
        // nothing the config asked for was skipped. Problems gate `roost
        // keys`' exit code and this must not — but staying *silent* about an
        // edited file that has no effect is how someone loses an afternoon.
        diagnostics.notices.push(format!(
            "{} is the one being read; {} also exists and is ignored",
            resolved.path.display(),
            shadowed.display()
        ));
    }
    (keymap, diagnostics)
}

fn load_keymap_from(path: &Path) -> (Keymap, Diagnostics) {
    let source = path.display().to_string();
    match std::fs::read_to_string(path) {
        Ok(raw) => Keymap::parse(&raw, &source),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            (Keymap::default(), Diagnostics::default())
        }
        Err(e) => (
            Keymap::default(),
            // A read failure is a problem, not a notice: the file exists and
            // roost could not honour it.
            Diagnostics {
                problems: vec![format!("{source}: {e} — using defaults")],
                notices: Vec::new(),
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> PathBuf {
        PathBuf::from("/state/roost/config.json")
    }
    fn xdg() -> PathBuf {
        PathBuf::from("/config/roost/config.json")
    }
    /// A named workspace's root and local candidates (distinct, unlike the
    /// macOS default-workspace case).
    fn root() -> PathBuf {
        PathBuf::from("/state/roost/config.json")
    }
    fn local() -> PathBuf {
        PathBuf::from("/state/roost/workspaces/a/config.json")
    }
    /// An `exists` oracle over a fixed set of present files — the filesystem
    /// the resolver is allowed to see.
    fn present(files: &[PathBuf]) -> impl Fn(&Path) -> bool + '_ {
        move |p: &Path| files.iter().any(|f| f == p)
    }

    #[test]
    fn the_state_dir_file_wins_when_both_exist_and_the_other_is_reported() {
        let r = resolve_in(state(), Some(xdg()), &present(&[state(), xdg()]));
        assert_eq!(
            r,
            Resolved { path: state(), exists: true, shadowed: Some(xdg()) },
            "an existing install's file must keep winning, and the ignored one must be named"
        );
    }

    #[test]
    fn only_the_state_dir_file_is_read_alone() {
        let r = resolve_in(state(), Some(xdg()), &present(&[state()]));
        assert_eq!(r, Resolved { path: state(), exists: true, shadowed: None });
    }

    /// The point of the whole change: a file in `~/.config/roost/` is found.
    #[test]
    fn only_the_config_dir_file_is_read_alone() {
        let r = resolve_in(state(), Some(xdg()), &present(&[xdg()]));
        assert_eq!(r, Resolved { path: xdg(), exists: true, shadowed: None });
    }

    /// Nothing to read — but the path roost *names* is the one a user should
    /// create, and that is the discoverable one, not the state dir.
    #[test]
    fn with_no_file_anywhere_the_config_dir_path_is_the_one_named() {
        let r = resolve_in(state(), Some(xdg()), &present(&[]));
        assert_eq!(r, Resolved { path: xdg(), exists: false, shadowed: None });
    }

    #[test]
    fn without_a_config_dir_the_state_dir_is_the_only_answer() {
        let r = resolve_in(state(), None, &present(&[state()]));
        assert_eq!(r, Resolved { path: state(), exists: true, shadowed: None });
        let r = resolve_in(state(), None, &present(&[]));
        assert_eq!(r, Resolved { path: state(), exists: false, shadowed: None });
    }

    /// macOS: `dirs::config_dir()` and the state dir's `data_local_dir`
    /// fallback are the same directory, so the two candidates are one file.
    /// It must not be reported as shadowing itself.
    #[test]
    fn identical_candidates_are_one_file_not_a_shadow() {
        let r = resolve_in(state(), Some(state()), &present(&[state()]));
        assert_eq!(
            r,
            Resolved { path: state(), exists: true, shadowed: None },
            "a file cannot shadow itself"
        );
    }

    // --- named workspaces (pure: explicit paths, no env) ---

    /// The base case: no workspace-local file, so the root's config is
    /// what the workspace reads — a remap in the root reaches `roost -w a`
    /// without being copied anywhere.
    #[test]
    fn a_named_workspace_reads_the_root_config_by_default() {
        let r = resolve_named(root(), local(), &present(&[root()]));
        assert_eq!(r, Resolved { path: root(), exists: true, shadowed: None });
        let r = resolve_named(root(), local(), &present(&[]));
        assert_eq!(
            r,
            Resolved { path: root(), exists: false, shadowed: None },
            "with neither file, the root path is the one named (that is where a config goes)"
        );
    }

    /// D5: whole-file, never merged. The workspace-local file is the
    /// entire keymap for that workspace, and the ignored root file is
    /// named — an edit there must not look like it took effect.
    #[test]
    fn a_workspace_local_config_replaces_the_root_whole_file() {
        let r = resolve_named(root(), local(), &present(&[root(), local()]));
        assert_eq!(
            r,
            Resolved { path: local(), exists: true, shadowed: Some(root()) },
            "the local file is read and the root file is the shadow"
        );
        let r = resolve_named(root(), local(), &present(&[local()]));
        assert_eq!(r, Resolved { path: local(), exists: true, shadowed: None });
    }

    /// The shadow is a *notice*: `roost keys` prints it but must still exit
    /// 0, since nothing the config asked for was skipped.
    #[test]
    fn the_shadow_report_is_a_notice_and_never_a_problem() {
        let dir = std::env::temp_dir().join(format!(
            "roost-config-test-shadow-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let live = dir.join("config.json");
        std::fs::write(&live, r#"{"keys": {"alt+f": "disable"}}"#).unwrap();

        // `load_keymap` reads the environment, so exercise its shadow arm
        // through the same two steps it takes rather than by setting env
        // vars (racy across parallel unit tests — see below).
        let resolved = Resolved { path: live.clone(), exists: true, shadowed: Some(xdg()) };
        let (keymap, mut diagnostics) = load_keymap_from(&resolved.path);
        if let Some(shadowed) = resolved.shadowed {
            diagnostics.notices.push(format!(
                "{} is the one being read; {} also exists and is ignored",
                resolved.path.display(),
                shadowed.display()
            ));
        }

        assert_ne!(keymap, Keymap::default(), "the live file must still be applied");
        assert!(diagnostics.problems.is_empty(), "a shadow must not gate the exit code");
        assert_eq!(diagnostics.notices.len(), 1, "{diagnostics:?}");
        assert!(diagnostics.notices[0].contains("ignored"), "{diagnostics:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `resolve_config` itself reads `$ROOST_STATE` — a process-global env
    /// var, racy across tests running in parallel in one process (see
    /// `infra::sock`'s own tests for the same reason) — so these only pin
    /// `load_keymap_from`'s behavior for an injected path, never the real
    /// env-driven resolution. That is instead proven by the PTY-level
    /// `tests/config_keys.rs`, which drives both the `ROOST_STATE` and the
    /// `~/.config` route through the real binary in its own process.
    #[test]
    fn a_missing_file_is_silent_defaults() {
        let dir = std::env::temp_dir().join(format!(
            "roost-config-test-missing-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let (keymap, diagnostics) = load_keymap_from(&dir.join("config.json"));
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(keymap, Keymap::default());
    }

    #[test]
    fn a_real_file_is_read_and_handed_to_keymap_parse() {
        let dir = std::env::temp_dir().join(format!(
            "roost-config-test-real-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"keys": {"alt+f": "disable"}}"#).unwrap();
        let (keymap, diagnostics) = load_keymap_from(&path);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_ne!(keymap, Keymap::default(), "the disable entry must have been applied");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A read failure that isn't "file doesn't exist" (here: the parent
    /// path is actually a file, not a directory, so opening `.../config.json`
    /// underneath it fails with NotADirectory) degrades to a diagnostic
    /// instead of propagating an error — never fatal, same contract as
    /// malformed content.
    #[test]
    fn an_unreadable_path_is_a_diagnostic_not_a_panic() {
        let dir = std::env::temp_dir().join(format!(
            "roost-config-test-unreadable-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::write(&dir, b"not a directory").unwrap(); // `dir` is a plain file
        let (keymap, diagnostics) = load_keymap_from(&dir.join("config.json"));
        assert_eq!(keymap, Keymap::default());
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        let _ = std::fs::remove_file(&dir);
    }
}
