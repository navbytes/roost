//! Durable atomic file replacement: the one implementation behind
//! `workspace.json` (`infra::store`) and the guest config files roost edits
//! (`infra::extension`). Both used to carry their own copy of the same
//! write-fsync-rename sequence, with the same long comment about why each
//! step is there; a fix to one had to be remembered in the other.
//!
//! The callers still own what genuinely differs: *where* the temp file goes
//! (a fixed name for roost's own file, a pid-qualified one for a file other
//! roosts and other tools may be writing) and *what mode* the result gets.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// The mode the replacement file ends up with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Perms {
    /// Created 0600 *before* anything is written, so the contents are never
    /// briefly readable by anyone else between write and rename. For files
    /// roost owns that hold resume tokens.
    Private,
    /// Copy the existing target's permissions onto the temp file before the
    /// rename; a target that does not exist yet gets the process umask's
    /// default. A fresh temp file does not inherit the target's mode, so
    /// without this an existing 0600 file (`~/.claude/settings.json` can hold
    /// `env` and `apiKeyHelper` secrets) would be silently downgraded.
    KeepTarget,
}

/// Replace `target` with `bytes` via `tmp`, so a crash can never leave a
/// half-written file for the next reader.
///
/// `tmp` must be on the same filesystem as `target` (name it from the same
/// directory) or the rename cannot be atomic; this does not check.
///
/// The bytes are **fsynced**, not `flush`ed. `Write::flush` on a
/// `std::fs::File` is a no-op that reads like a durability barrier and is
/// not one. Without a real fsync the rename can reach the disk before the
/// data does, so a power cut or kernel panic can leave `target` truncated or
/// zero-length — and for `workspace.json`, whose loader treats an
/// unparseable file as "no workspace", that costs the user their whole
/// fleet. One fsync of a small file per save.
///
/// The directory's own fsync (the rename's durability) is best-effort and
/// deliberately so: losing it means the *previous* contents come back,
/// stale but intact — a far milder failure than the truncation the data
/// fsync prevents, and some filesystems refuse fsync on a directory handle
/// outright.
///
/// If the rename fails the temp file is removed rather than left behind.
pub fn write_atomic(target: &Path, tmp: &Path, bytes: &[u8], perms: Perms) -> io::Result<()> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    if perms == Perms::Private {
        opts.mode(0o600);
    }
    {
        let mut f = opts.open(tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    if perms == Perms::KeepTarget {
        if let Ok(meta) = fs::metadata(target) {
            let _ = fs::set_permissions(tmp, meta.permissions());
        }
    }
    if let Err(e) = fs::rename(tmp, target) {
        let _ = fs::remove_file(tmp);
        return Err(e);
    }
    if let Some(dir) = target.parent() {
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// Write `bytes` to `path` and fsync them — the data half of
/// [`write_atomic`], for a file nothing replaces (a one-off backup).
pub fn write_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut f = fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("roost-atomic-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn replaces_the_contents_and_leaves_no_temp_file() {
        let dir = scratch("replace");
        let (target, tmp) = (dir.join("f.json"), dir.join("f.json.tmp"));
        fs::write(&target, "old").unwrap();
        write_atomic(&target, &tmp, b"new", Perms::Private).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        assert!(!tmp.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn private_files_are_0600_even_when_the_umask_would_allow_more() {
        let dir = scratch("private");
        let (target, tmp) = (dir.join("f.json"), dir.join("f.json.tmp"));
        write_atomic(&target, &tmp, b"x", Perms::Private).unwrap();
        assert_eq!(mode(&target), 0o600);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn keep_target_preserves_an_existing_mode() {
        let dir = scratch("keep");
        let (target, tmp) = (dir.join("f.json"), dir.join("f.json.tmp"));
        fs::write(&target, "old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        write_atomic(&target, &tmp, b"new", Perms::KeepTarget).unwrap();
        assert_eq!(mode(&target), 0o640);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_rename_cleans_up_its_temp_file_and_reports_the_error() {
        let dir = scratch("rename-fails");
        let (target, tmp) = (dir.join("f.json"), dir.join("f.json.tmp"));
        // A non-empty directory in the target's place: the write succeeds,
        // the rename cannot. (A permissions trick would not do — the tests
        // run as root in some environments.)
        fs::create_dir(&target).unwrap();
        fs::write(target.join("occupant"), "x").unwrap();
        assert!(write_atomic(&target, &tmp, b"new", Perms::Private).is_err());
        assert!(!tmp.exists(), "the temp file must not be left behind");
        assert!(target.join("occupant").exists(), "and the target is untouched");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_directory_is_an_error_not_a_panic() {
        let dir = scratch("no-dir");
        let (target, tmp) = (dir.join("gone/f.json"), dir.join("gone/f.json.tmp"));
        assert!(write_atomic(&target, &tmp, b"x", Perms::Private).is_err());
        let _ = fs::remove_dir_all(dir);
    }
}
