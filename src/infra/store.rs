//! Filesystem `StateStore`: workspace.json under the state directory,
//! written atomically (temp file + rename).
//!
//! The state tree has one root and any number of workspaces under it. The
//! root is `$ROOST_STATE` or the XDG state dir (`root_dir`); the default
//! workspace keeps its files in the root itself and every named workspace
//! (`roost -w <name>`) gets `root/workspaces/<name>`, a complete instance
//! directory of its own. The workspace is chosen once per process at
//! startup (`init_workspace`) and read through `FsStore::state_dir`, so a
//! process that never selects one behaves exactly as before this existed.

use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::core::workspace::Workspace;
use crate::ports::StateStore;

/// The name of the workspace that keeps its files in the root itself —
/// the files a pre-workspaces roost used, no migration. Accepted wherever
/// a workspace name is (`roost -w default` is the plain TUI), but reserved
/// for `ws rm`/`ws mv`/creation paths: the default cannot be renamed or
/// deleted, only lived in.
pub const DEFAULT_WORKSPACE: &str = "default";

/// The workspace this process runs in, resolved once at startup by
/// `FsStore::init_workspace` and never touched again. Unset (unit tests,
/// any process that skipped startup resolution) means the default
/// workspace — which is why default behavior is bit-identical to before.
static WORKSPACE: OnceLock<String> = OnceLock::new();

pub struct FsStore {
    path: PathBuf,
}

impl FsStore {
    /// The state ROOT: `$ROOST_STATE` when set (isolated profiles /
    /// parallel instances), else the XDG state dir's `roost` subdirectory.
    /// Read fresh on every call — the variable is honored per call, the
    /// way it always was; only the *workspace* below the root is cached.
    /// Shared by `default_path` (workspace.json) and `infra::config`
    /// (config.json — the key-bindings escape hatch), so `$ROOST_STATE`
    /// redirects both together: an isolated fleet's config stays isolated
    /// with it.
    pub fn root_dir() -> PathBuf {
        if let Some(dir) = std::env::var_os("ROOST_STATE") {
            return PathBuf::from(dir);
        }
        dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("roost")
    }

    /// The directory of the workspace named `name`: the root itself for
    /// the default workspace, `root/workspaces/<name>` for a named one.
    /// One name, one directory, one complete instance of per-instance
    /// state (workspace.json, the lock, socket, token, logs).
    pub fn workspace_dir(name: &str) -> PathBuf {
        if name == DEFAULT_WORKSPACE {
            Self::root_dir()
        } else {
            Self::root_dir().join("workspaces").join(name)
        }
    }

    /// The directory of the workspace this process runs in — the root for
    /// the default workspace, `root/workspaces/<name>` for a named one.
    /// Every per-instance file hangs off it (workspace.json, the instance
    /// lock, the socket for named workspaces, control.token/log,
    /// perf.jsonl), and `infra::config` composes it with the root's
    /// config.json. The workspace is resolved once at startup
    /// (`init_workspace`); unset, this is exactly the old directory, so
    /// `$ROOST_STATE` still redirects a whole instance and a process that
    /// never selects a workspace behaves bit-identically to before.
    pub fn state_dir() -> PathBuf {
        Self::workspace_dir(Self::workspace_name())
    }

    /// The name of the workspace this process runs in: the `-w`/
    /// `--workspace` flag or `ROOST_WORKSPACE` resolved at startup,
    /// `default` when neither was given (including every unit test and
    /// every subprocess that skipped `init_workspace`).
    pub fn workspace_name() -> &'static str {
        WORKSPACE.get().map(String::as_str).unwrap_or(DEFAULT_WORKSPACE)
    }

    /// Resolve the workspace selection once, at startup. `flag` is the
    /// `-w`/`--workspace` value, `env` the `ROOST_WORKSPACE` value (pass
    /// `None` when unset); the flag wins. First call wins and later calls
    /// are no-ops, so both entry points (the CLI pre-pass and the TUI
    /// startup in main) can call it unconditionally. Invalid names are an
    /// `Err` before anything is set — the caller exits 2, touching no
    /// state. This deliberately does NOT set `ROOST_STATE` in the
    /// environment: `std::env::set_var` is unsafe under Rust 2024 once
    /// threads exist, and panes would inherit a workspace-scoped value
    /// that nests workspaces inside workspaces.
    pub fn init_workspace(flag: Option<&str>, env: Option<&str>) -> Result<(), String> {
        if WORKSPACE.get().is_some() {
            return Ok(());
        }
        let name = select_workspace(flag, env)?;
        let _ = WORKSPACE.set(name);
        Ok(())
    }

    /// `$ROOST_STATE/workspace.json` when set (isolated profiles / parallel
    /// instances), else the XDG state dir — now the workspace-aware
    /// directory, since workspace.json is per-workspace state.
    pub fn default_path() -> PathBuf {
        Self::state_dir().join("workspace.json")
    }

    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Does an instance hold this workspace directory's lock right now?
    /// Non-blocking: opens `<dir>/workspace.lock` (creating the file is
    /// harmless — an instance does the same) and attempts `try_lock`; a
    /// failure means a live instance holds the lock. The handle drops
    /// immediately, so the probe never holds the lock and a crashed
    /// instance's stale file reads as free. The one liveness signal
    /// without a daemon.
    pub fn instance_running(dir: &Path) -> bool {
        // truncate(false) is the point: the file is a lock *handle*, its
        // contents are never written — truncating another instance's file
        // would be the one thing this probe must not do.
        let Ok(file) = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("workspace.lock"))
        else {
            return false;
        };
        file.try_lock().is_err()
    }

    /// Every workspace that could hold state: the default plus one name
    /// per directory under `root/workspaces`. Names that fail validation
    /// are skipped — a stray directory there is not a workspace roost
    /// created.
    pub fn workspace_names() -> Vec<String> {
        let mut out = vec![DEFAULT_WORKSPACE.to_string()];
        let mut named: Vec<String> = fs::read_dir(Self::root_dir().join("workspaces"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| check_creatable_name(n).is_ok())
            .collect();
        named.sort();
        out.extend(named);
        out
    }

    /// Workspace names with a live instance — the `ws ls` running flag and
    /// the control client's unreachable-error list share this probe.
    pub fn running_workspaces() -> Vec<String> {
        Self::workspace_names()
            .into_iter()
            .filter(|n| Self::instance_running(&Self::workspace_dir(n)))
            .collect()
    }
}

/// Workspace-name syntax: 1–32 characters, starting with a lowercase
/// letter or digit, then only lowercase letters, digits, `.`, `_` and `-`.
/// Lowercase is not pedantry — the name is a directory name and part of a
/// socket path, and case-folding filesystems (APFS) would collide two
/// names ext4 would keep apart. Pure: no env, no filesystem.
pub fn check_workspace_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(
            "workspace name cannot be empty — use 1-32 characters of a-z, 0-9, '.', '_' or '-'"
                .into(),
        );
    }
    if name.chars().count() > 32 {
        let n = name.chars().count();
        return Err(format!("workspace name '{name}' is {n} characters — the limit is 32"));
    }
    if name.chars().any(|c| c.is_ascii_uppercase()) {
        let lower = name.to_lowercase();
        return Err(format!(
            "workspace name '{name}' must be lowercase — try '{lower}' (allowed: a-z, 0-9, '.', '_', '-')"
        ));
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap_or('?');
    let rest_ok =
        |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-');
    // The grammar is `^[a-z0-9][a-z0-9._-]{0,31}$`: the FIRST character is
    // letters/digits only, so a name can never start with `.` (hidden files)
    // or `-` (flag-shaped).
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) || !chars.all(rest_ok) {
        return Err(format!(
            "invalid workspace name '{name}' — use 1-32 characters of a-z, 0-9, '.', '_' or '-', starting with a letter or digit"
        ));
    }
    Ok(())
}

/// The creation-path check (`ws rm`/`ws mv`/later verbs): valid syntax AND
/// not the reserved default, which can be selected (`-w default`) but never
/// renamed or deleted.
pub fn check_creatable_name(name: &str) -> Result<(), String> {
    check_workspace_name(name)?;
    if name == DEFAULT_WORKSPACE {
        return Err("'default' is reserved for the default workspace".into());
    }
    Ok(())
}

/// Selection precedence, pure: the `-w` flag, then `ROOST_WORKSPACE`, then
/// the default. `default` is a legal selection (it names the default
/// workspace), so the syntax check is the only gate here; the reserved
/// check is the creation paths' (`check_creatable_name`). Callers read the
/// environment themselves and pass it in — in-process env writes are
/// process-global and racy across parallel tests.
pub fn select_workspace(flag: Option<&str>, env: Option<&str>) -> Result<String, String> {
    if let Some(name) = flag {
        check_workspace_name(name)?;
        return Ok(name.to_string());
    }
    if let Some(name) = env {
        check_workspace_name(name)?;
        return Ok(name.to_string());
    }
    Ok(DEFAULT_WORKSPACE.to_string())
}

impl Default for FsStore {
    fn default() -> Self {
        Self::new(Self::default_path())
    }
}

/// The newest `Workspace` schema this build writes and understands. A file
/// stamped higher than this was written by a newer roost, whose shape this
/// build cannot be trusted to round-trip: serde ignores fields it does not
/// know, so parsing "succeeds" and the first save silently rewrites the file
/// with everything new stripped out.
const SCHEMA_VERSION: u32 = 1;

impl StateStore for FsStore {
    fn load(&self) -> Result<Option<Workspace>> {
        self.load_reporting().map(|(ws, _)| ws)
    }

    /// What it had to do to the file to get there.
    ///
    /// Losing a workspace is the one failure this tool cannot be quiet
    /// about: resurrection is the whole product, and a user who launches
    /// roost to an empty tab needs to know their fleet was set aside rather
    /// than believe it evaporated — the salvage is only useful if they are
    /// told it exists. Reported the same way a bad `config.json` is (main.rs
    /// turns it into a startup flash + a feed line), rather than being
    /// swallowed here.
    fn load_reporting(&self) -> Result<(Option<Workspace>, Option<String>)> {
        if !self.path.exists() {
            return Ok((None, None));
        }
        let raw = fs::read_to_string(&self.path)
            .with_context(|| format!("reading {}", self.path.display()))?;
        // A corrupt or version-incompatible workspace.json must NOT brick
        // startup — the whole tool is that file. Move it aside (so it's
        // recoverable / debuggable) and start fresh rather than aborting
        // every tab. Naming by pid avoids clobbering a prior salvage.
        let salvage = |why: &str| -> (Option<Workspace>, Option<String>) {
            let bak = self.path.with_extension(format!("json.corrupt-{}", std::process::id()));
            let moved = fs::rename(&self.path, &bak).is_ok();
            let where_ = if moved {
                format!("saved as {}", bak.display())
            } else {
                "could not be saved aside".to_string()
            };
            (None, Some(format!("workspace.json {why} — {where_}, starting fresh")))
        };
        match serde_json::from_str::<Workspace>(&raw) {
            Ok(ws) if ws.version > SCHEMA_VERSION => {
                Ok(salvage(&format!("was written by a newer roost (version {})", ws.version)))
            }
            Ok(ws) => Ok((if ws.tabs.is_empty() { None } else { Some(ws) }, None)),
            Err(e) => Ok(salvage(&format!("could not be read ({e})"))),
        }
    }

    fn save(&self, ws: &Workspace) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
            // The state dir holds session resume tokens — keep it private.
            let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        }
        let tmp = self.path.with_extension("json.tmp");
        // Create the temp file 0600 *before* writing, so the resume tokens
        // inside are never briefly world-readable between write and rename.
        {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(&serde_json::to_vec_pretty(ws)?)?;
            // **Not** `flush()`. `Write::flush` on a `std::fs::File` is a
            // no-op — it reads like a durability barrier and is not one.
            // Without a real fsync the rename below can reach the disk
            // before the bytes do, so a power cut or kernel panic can leave
            // `workspace.json` truncated or zero-length. `load()` treats an
            // unparseable file as "no workspace", so that outcome costs the
            // user their whole fleet: the durability gap and the
            // discard-on-corrupt rule compound into real data loss.
            //
            // One fsync of a small file per save, and only the *data* — see
            // the directory note below for why that half is best-effort.
            f.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        // The rename's own durability, best-effort and deliberately so:
        // losing it means the *previous* save is what comes back, which is
        // stale but intact. That is a different and far milder failure than
        // the truncation the data fsync above prevents, and some
        // filesystems refuse fsync on a directory handle outright.
        if let Some(dir) = self.path.parent() {
            if let Ok(d) = fs::File::open(dir) {
                let _ = d.sync_all();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_missing_file() {
        let dir = std::env::temp_dir().join(format!("roost-store-test-{}", std::process::id()));
        let store = FsStore::new(dir.join("ws.json"));
        assert!(store.load().unwrap().is_none());
        let ws = Workspace::default_in(PathBuf::from("/tmp"));
        store.save(&ws).unwrap();
        let back = store.load().unwrap().unwrap();
        assert_eq!(back.tabs[0].name, "main");
        // atomic write leaves no temp file behind
        assert!(!dir.join("ws.json.tmp").exists());
        // saved file is private (0600)
        let mode = fs::metadata(dir.join("ws.json")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = fs::remove_dir_all(dir);
    }

    /// Setting a workspace aside is a recovery, not a non-event: the user
    /// launched roost and their whole fleet is not there. Silence made that
    /// indistinguishable from the file having evaporated, and left the
    /// salvage — the only copy of their tabs — undiscoverable.
    #[test]
    fn a_discarded_workspace_says_so_and_says_where() {
        let dir = std::env::temp_dir().join(format!("roost-store-say-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("ws.json");
        fs::write(&path, b"{ this is not valid json").unwrap();
        let (ws, why) = FsStore::new(path.clone()).load_reporting().unwrap();
        assert!(ws.is_none());
        let why = why.expect("a discarded workspace is reported");
        assert!(why.contains("workspace.json"), "names the file: {why}");
        assert!(why.contains("corrupt-"), "names where the salvage went: {why}");
        let _ = fs::remove_dir_all(dir);
    }

    /// A file from a newer roost parses — serde ignores fields it does not
    /// know — so "it loaded" is not the same as "this build understands it".
    /// Taking it at face value means the next save rewrites the file with
    /// everything newer stripped out, silently downgrading state that the
    /// roost the user usually runs still needs.
    #[test]
    fn a_workspace_from_a_newer_roost_is_not_silently_downgraded() {
        let dir = std::env::temp_dir().join(format!("roost-store-newer-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("ws.json");
        let mut ws = Workspace::default_in(PathBuf::from("/tmp"));
        ws.version = SCHEMA_VERSION + 1;
        fs::write(&path, serde_json::to_vec_pretty(&ws).unwrap()).unwrap();

        let (loaded, why) = FsStore::new(path.clone()).load_reporting().unwrap();
        assert!(loaded.is_none(), "a future-version file is not adopted");
        assert!(!path.exists(), "and it is not left in place to be overwritten");
        let why = why.expect("the user is told");
        assert!(why.contains("newer roost"), "says what happened: {why}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_file_is_moved_aside_not_fatal() {
        let dir = std::env::temp_dir().join(format!("roost-store-corrupt-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("ws.json");
        fs::write(&path, b"{ this is not valid json").unwrap();
        let store = FsStore::new(path.clone());
        // load() recovers (fresh start) instead of erroring...
        assert!(store.load().unwrap().is_none());
        // ...and the bad file was preserved under a .corrupt-* name.
        assert!(!path.exists());
        let salvaged = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains("corrupt"));
        assert!(salvaged);
        let _ = fs::remove_dir_all(dir);
    }

    // --- workspace names (pure: no env, no filesystem) ---

    #[test]
    fn valid_names_pass_the_grammar() {
        for name in ["a", "0x", "my-ws_1.2", "tripto", "default", &"a".repeat(32)] {
            assert!(check_workspace_name(name).is_ok(), "{name} must be valid");
        }
    }

    #[test]
    fn an_empty_name_is_rejected() {
        let err = check_workspace_name("").unwrap_err();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn a_33_character_name_is_rejected_with_the_limit_named() {
        let name = "a".repeat(33);
        let err = check_workspace_name(&name).unwrap_err();
        assert!(err.contains("32"), "must state the limit: {err}");
    }

    /// The hint must show the lowercase form: the name is a directory name
    /// and case-folding filesystems (APFS) would collide the two anyway.
    #[test]
    fn an_uppercase_name_is_rejected_with_the_lowercase_hint() {
        for (bad, fixed) in [("Tripto", "tripto"), ("My_ws", "my_ws")] {
            let err = check_workspace_name(bad).unwrap_err();
            assert!(err.contains("lowercase"), "{err}");
            assert!(err.contains(fixed), "must suggest '{fixed}': {err}");
        }
    }

    #[test]
    fn invalid_characters_are_rejected_with_what_is_allowed() {
        for bad in ["my ws", ".hidden", "_x", "-x", "a/b", "café"] {
            let err = check_workspace_name(bad).unwrap_err();
            assert!(err.contains("a-z"), "must name the allowed characters: {err}");
        }
    }

    /// `default` is a legal *selection* — it names the default workspace —
    /// but never a creatable one: `ws rm/mv default` must refuse.
    #[test]
    fn default_is_selectable_but_not_creatable() {
        assert_eq!(select_workspace(Some("default"), None), Ok("default".into()));
        assert!(check_creatable_name("default").is_err());
        assert!(check_creatable_name("scratch").is_ok());
        // The creatable check is the syntax check plus the reservation.
        assert!(check_creatable_name("Bad Name").is_err());
    }

    #[test]
    fn selection_precedence_is_flag_then_env_then_default() {
        assert_eq!(select_workspace(Some("flag"), Some("env")), Ok("flag".into()));
        assert_eq!(select_workspace(None, Some("env")), Ok("env".into()));
        assert_eq!(select_workspace(None, None), Ok("default".into()));
        // An invalid value from either source is rejected, not silently
        // fallen through: a typo'd ROOST_WORKSPACE must not open the
        // default workspace and look like everything vanished.
        assert!(select_workspace(Some("Bad Name"), Some("ok-name")).is_err());
        assert!(select_workspace(None, Some("no good")).is_err());
    }

    // --- the lock probe (filesystem, but no env: the dir is explicit) ---

    /// `instance_running` is the whole liveness mechanism: lock held means
    /// running, lock free (including a stale file from a crash) means
    /// idle, and the probe itself must never hold the lock afterwards.
    #[test]
    fn the_lock_probe_reports_held_and_releases_cleanly() {
        let dir = std::env::temp_dir().join(format!(
            "roost-store-probe-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert!(!FsStore::instance_running(&dir));
        let holder = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("workspace.lock"))
            .unwrap();
        holder.try_lock().unwrap();
        assert!(FsStore::instance_running(&dir), "a held lock reads as running");
        drop(holder);
        assert!(!FsStore::instance_running(&dir), "a freed lock reads as idle");
        let _ = fs::remove_dir_all(dir);
    }
}
