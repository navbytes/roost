//! Filesystem `StateStore`: workspace.json under the XDG state dir,
//! written atomically (temp file + rename).

use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

use crate::core::workspace::Workspace;
use crate::ports::StateStore;

pub struct FsStore {
    path: PathBuf,
}

impl FsStore {
    /// The directory roost's per-instance state lives in: `$ROOST_STATE`
    /// when set (isolated profiles / parallel instances), else the XDG
    /// state dir's `roost` subdirectory. Shared by `default_path`
    /// (workspace.json) and `infra::config` (config.json — the
    /// key-bindings escape hatch), so `$ROOST_STATE` redirects both
    /// together: an isolated fleet's config stays isolated with it.
    pub fn state_dir() -> PathBuf {
        if let Some(dir) = std::env::var_os("ROOST_STATE") {
            return PathBuf::from(dir);
        }
        dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("roost")
    }

    /// `$ROOST_STATE/workspace.json` when set (isolated profiles / parallel
    /// instances), else the XDG state dir.
    pub fn default_path() -> PathBuf {
        Self::state_dir().join("workspace.json")
    }

    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
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
}
