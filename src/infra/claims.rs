//! Cross-instance session claims (design D7, spec `session-claims`): at
//! most one roost pane across all running instances may drive a given
//! `(adapter, session id)`, so two windows never resume or adopt the same
//! conversation.
//!
//! The mechanism is the instance lock's, reused: a file per claim under
//! `<state root>/claims/`, held with an exclusive advisory `flock`. The
//! lock is the truth — the file's contents (owner workspace, pid) exist
//! only so a conflict message can *name* the holder. Nothing needs
//! cleaning up: the OS drops the lock when the holding process dies for
//! any reason, and an unlocked file is simply a free claim, so stale files
//! are harmless and self-cleaning by semantics rather than by a sweeper.
//!
//! Claims are keyed by pane in `App` and released by dropping the handle
//! (pane close, shutdown) — `ports::ClaimHandle`. Files persist after
//! release; only the lock is the live state, so `claimed` (the exclusion
//! set for session detection) probes each file's lock briefly rather than
//! trusting the directory listing.

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::time::Duration;

use crate::infra::sock::dir_is_private_and_ours;
use crate::ports::{ClaimError, ClaimHandle, SessionClaims};

/// Retry budget for a lock that's held only for a moment (B6): a handful of
/// short tries, so a passing probe can't be mistaken for a real conflict —
/// without making a real conflict noticeably slower to report.
const ACQUIRE_ATTEMPTS: u32 = 3;
const ACQUIRE_RETRY_DELAY: Duration = Duration::from_millis(25);

pub struct FsClaims {
    /// The shared state root (`FsStore::root_dir()`) — claims live under
    /// `<root>/claims/` so every workspace's instances see the same store.
    root: PathBuf,
    /// This instance's workspace name, recorded in a claim file's owner
    /// line so a conflict can name the holder. `acquire` itself is NOT
    /// idempotent for it (see `SessionClaims`'s own doc) — `App::
    /// claim_session` is what makes a pane respawn safe, by releasing its
    /// existing handle before re-acquiring, so this port never needs to
    /// recognise a claim as "already mine".
    workspace: String,
}

impl FsClaims {
    pub fn new(root: PathBuf, workspace: String) -> Self {
        Self { root, workspace }
    }

    fn claims_dir(&self) -> Result<PathBuf> {
        let dir = self.root.join("claims");
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        // Owner-only, same reason the state dir itself is 0700 (store.rs):
        // claim files name workspaces and pids, and the locks are only a
        // guarantee if a stranger can't take one first. `create_dir_all`
        // with a restrictive mode is not enough (mkdir's mode is a umask
        // request), so set it explicitly, like `FsStore::save`.
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
        // ...and actually check it stuck: the same private-and-ours check
        // the control socket's directory gets (an attacker who pre-created
        // this directory, or a filesystem that ignores the chmod, is
        // exactly what that check exists to catch). A failure here becomes
        // `ClaimError::Failed` up in `acquire`, which is advisory-only —
        // the caller degrades to unclaimed with a feed line, never panics,
        // never blocks a restore (spec: claims are private to the user, not
        // a hard gate on using roost at all).
        if !dir_is_private_and_ours(&dir) {
            anyhow::bail!("{} is not private to this user", dir.display());
        }
        Ok(dir)
    }

    /// `<adapter>.<session-id>` — adapter ids never contain `.`, and
    /// session ids are validated (`agents::valid_session_id`) before they
    /// reach here, so the join is unambiguous.
    fn claim_path(dir: &Path, adapter: &str, session: &str) -> PathBuf {
        dir.join(format!("{adapter}.{session}"))
    }

    /// The claim file's owner line, `workspace\tpid`, parsed leniently — a
    /// partially written or foreign file is information, never a failure.
    fn owner_of(path: &Path) -> String {
        let Ok(raw) = fs::read_to_string(path) else {
            return "an unknown owner".into();
        };
        // The writer puts both fields on one line (`acquire`, above) — split
        // on the first tab rather than treating them as two separate lines.
        let line = raw.lines().next().unwrap_or_default();
        let (ws, pid) = match line.split_once('\t') {
            Some((ws, pid)) => (ws.trim(), pid.trim()),
            None => (line.trim(), ""),
        };
        if ws.is_empty() {
            return "an unknown owner".into();
        }
        if pid.is_empty() {
            format!("workspace '{ws}'")
        } else {
            format!("workspace '{ws}' (pid {pid})")
        }
    }
}

use std::path::Path;

impl SessionClaims for FsClaims {
    fn acquire(&self, adapter: &str, session: &str) -> Result<ClaimHandle, ClaimError> {
        let dir = self
            .claims_dir()
            .map_err(|e| ClaimError::Failed(format!("claims directory: {e:#}")))?;
        let path = Self::claim_path(&dir, adapter, session);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(|e| ClaimError::Failed(format!("opening {}: {e}", path.display())))?;
        // A concurrent `claimed()` probe (or another instance's own
        // `acquire` attempt) holds the lock for a moment, not indefinitely —
        // a few retries keep that from being mistaken for a real conflict
        // and turning a legitimate restore into a sticky "held by"
        // placeholder.
        let mut locked = file.try_lock();
        for _ in 1..ACQUIRE_ATTEMPTS {
            if locked.is_ok() {
                break;
            }
            std::thread::sleep(ACQUIRE_RETRY_DELAY);
            locked = file.try_lock();
        }
        match locked {
            Ok(()) => {
                // Informational only — the lock is the truth. Best-effort:
                // a read-only claims dir would still have granted the lock
                // had the file existed, and the owner line is a nicety.
                // Truncate first: without it, a shorter owner line than
                // whatever was written here last time leaves that write's
                // trailing bytes in place — e.g. a stale pid digit tacked
                // onto the new one.
                let _ = file.set_len(0);
                let _ = std::io::Write::write_all(
                    &mut file,
                    format!("{}\t{}\n", self.workspace, std::process::id()).as_bytes(),
                );
                Ok(ClaimHandle::new(Box::new(move || {
                    let _ = file.unlock();
                })))
            }
            Err(_) => Err(ClaimError::Held(Self::owner_of(&path))),
        }
    }

    fn claimed(&self, adapter: &str) -> HashSet<String> {
        let mut out = HashSet::new();
        let Ok(dir) = fs::read_dir(self.root.join("claims")) else {
            return out; // no claims yet (or unreadable): nothing claimed
        };
        let prefix = format!("{adapter}.");
        for entry in dir.flatten() {
            let name = entry.file_name();
            let Some(session) = name.to_str().and_then(|n| n.strip_prefix(&prefix)) else {
                continue;
            };
            let session = session.to_string();
            // A free (unlocked) file is not a claim — probe, don't trust
            // the listing. The momentary lock is dropped immediately.
            let Ok(f) = fs::OpenOptions::new().write(true).open(entry.path()) else {
                continue;
            };
            if f.try_lock().is_ok() {
                let _ = f.unlock();
            } else {
                out.insert(session.to_string());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("roost-claims-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn acquire_excludes_and_release_frees() {
        let root = temp_root("basic");
        let a = FsClaims::new(root.clone(), "a".into());
        let b = FsClaims::new(root.clone(), "b".into());

        let handle = a.acquire("pi", "s1").unwrap();
        assert!(b.acquire("pi", "s1").is_err(), "a held claim is refused");
        assert_eq!(a.claimed("pi"), ["s1".to_string()].into_iter().collect());
        assert!(a.claimed("claude").is_empty(), "claims are per adapter");

        drop(handle); // a quits or closes the pane
        let handle = b.acquire("pi", "s1").unwrap();
        assert!(a.acquire("pi", "s1").is_err(), "the claim moved with the release");
        drop(handle);
        let _ = fs::remove_dir_all(root);
    }

    /// The same workspace re-acquiring its own claim must succeed — a pane
    /// respawn re-runs the restore path: the app releases the old handle
    /// before re-acquiring (one handle per pane), so the re-acquire must
    /// succeed — a single instance never blocks itself.
    #[test]
    fn re_acquiring_after_release_succeeds() {
        let root = temp_root("idempotent");
        let a = FsClaims::new(root.clone(), "a".into());
        let first = a.acquire("pi", "s1").unwrap();
        drop(first);
        let second = a.acquire("pi", "s1").unwrap();
        drop(second);
        assert!(a.acquire("pi", "s1").is_ok(), "still free after the second release");
        let _ = fs::remove_dir_all(root);
    }

    /// A conflict names the holder, from the file's contents — the only
    /// place the owner's identity lives.
    #[test]
    fn a_conflict_names_the_holding_workspace_and_pid() {
        let root = temp_root("owner");
        let a = FsClaims::new(root.clone(), "alpha".into());
        let b = FsClaims::new(root.clone(), "beta".into());
        let _handle = a.acquire("claude", "s2").unwrap();
        let err = b.acquire("claude", "s2").unwrap_err();
        match err {
            ClaimError::Held(desc) => {
                assert!(desc.contains("alpha"), "names the workspace: {desc}");
                assert!(desc.contains(&std::process::id().to_string()), "names the pid: {desc}");
            }
            other => panic!("expected Held, got {other:?}"),
        }
        let _ = fs::remove_dir_all(root);
    }

    /// Released (or crashed) claims leave their files behind — harmless, a
    /// free claim — and `claimed` must not report them as held.
    #[test]
    fn a_released_claim_file_is_not_reported_as_claimed() {
        let root = temp_root("stale");
        let a = FsClaims::new(root.clone(), "a".into());
        drop(a.acquire("pi", "s3").unwrap());
        assert!(a.claimed("pi").is_empty(), "an unlocked file is nobody's claim");
        let _ = fs::remove_dir_all(root);
    }

    /// The claims directory is owner-only — spec: "Claim state SHALL live
    /// under the owner-only state root".
    #[test]
    fn the_claims_directory_is_owner_only() {
        let root = temp_root("perms");
        let a = FsClaims::new(root.clone(), "a".into());
        let _ = a.acquire("pi", "s4").unwrap();
        let mode = fs::metadata(root.join("claims")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        let _ = fs::remove_dir_all(root);
    }

    /// `owner_of` parses the writer's one-line `workspace\tpid` form, not
    /// two lines — regression coverage for the mismatch this fixes.
    #[test]
    fn owner_of_parses_the_one_line_workspace_and_pid() {
        let root = temp_root("owner-of-both");
        let path = root.join("claim");
        fs::write(&path, "a\t41335\n").unwrap();
        assert_eq!(FsClaims::owner_of(&path), "workspace 'a' (pid 41335)");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn owner_of_with_no_pid_names_just_the_workspace() {
        let root = temp_root("owner-of-nopid");
        let path = root.join("claim");
        fs::write(&path, "a\n").unwrap();
        assert_eq!(FsClaims::owner_of(&path), "workspace 'a'");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn owner_of_an_empty_file_is_an_unknown_owner() {
        let root = temp_root("owner-of-empty");
        let path = root.join("claim");
        fs::write(&path, "").unwrap();
        assert_eq!(FsClaims::owner_of(&path), "an unknown owner");
        let _ = fs::remove_dir_all(root);
    }
}
