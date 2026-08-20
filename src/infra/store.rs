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

impl StateStore for FsStore {
    fn load(&self) -> Result<Option<Workspace>> {
        if !self.path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(&self.path)
            .with_context(|| format!("reading {}", self.path.display()))?;
        match serde_json::from_str::<Workspace>(&raw) {
            Ok(ws) => Ok(if ws.tabs.is_empty() { None } else { Some(ws) }),
            // A corrupt or version-incompatible workspace.json must NOT brick
            // startup — the whole tool is that file. Move it aside (so it's
            // recoverable / debuggable) and start fresh rather than aborting
            // every tab. Naming by pid avoids clobbering a prior salvage.
            Err(_) => {
                let bak = self
                    .path
                    .with_extension(format!("json.corrupt-{}", std::process::id()));
                let _ = fs::rename(&self.path, &bak);
                Ok(None)
            }
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
