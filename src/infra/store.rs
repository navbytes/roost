//! Filesystem `StateStore`: workspace.json under the XDG state dir,
//! written atomically (temp file + rename).

use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

use crate::core::workspace::Workspace;
use crate::ports::StateStore;

pub struct FsStore {
    path: PathBuf,
}

impl FsStore {
    pub fn default_path() -> PathBuf {
        dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("roost")
            .join("workspace.json")
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
        let ws: Workspace = serde_json::from_str(&raw).context("parsing workspace.json")?;
        Ok(if ws.tabs.is_empty() { None } else { Some(ws) })
    }

    fn save(&self, ws: &Workspace) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(ws)?)?;
        fs::rename(&tmp, &self.path)?;
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
        let _ = fs::remove_dir_all(dir);
    }
}
