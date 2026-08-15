//! Filesystem session detection (design doc §6.1): whether a pane's stored
//! session id is still resumable, and which not-yet-detected session file on
//! disk belongs to a freshly launched pane. Several agent CLIs each drop
//! their own session file wherever they please, and roost has to reconcile
//! that against whatever panes are asking — that reconciliation is the real
//! coordination problem of the daemonless model, and it used to be spread
//! through `App`'s private methods (`spawn_pane`, `tick`, `claimed_sessions`)
//! where it could only be exercised by spinning up a whole `App`. Pulled out
//! here so it's testable on its own (ROADMAP "[health] Extract
//! SessionResolver").
//!
//! `App` still owns the bookkeeping this leans on — which panes are
//! mid-detection, when they were spawned, persisting the result — since
//! that's tied to its own runtime/save lifecycle. What lives here is the
//! decision logic: given an adapter's view of disk (and the rest of the
//! workspace), what should happen.

use std::collections::HashSet;
use std::path::Path;

use crate::agents::{valid_session_id, AgentAdapter, SessionState};
use crate::core::workspace::Workspace;

/// What a newly spawning pane should do about a (possibly) stored session id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnResolution {
    /// `Some(id)` to resume, `None` to launch fresh.
    pub session: Option<String>,
    /// The stored id was unusable — malformed, or the adapter says it's
    /// definitively gone — so the caller should persist its removal so a
    /// dead id isn't retried on the next launch.
    pub stale: bool,
    /// No session to resume, and the adapter has somewhere to look for one
    /// — the caller should watch for a session file this launch creates.
    pub wants_detect: bool,
}

/// Resolve a pane's stored session id against the adapter's view of disk.
/// A malformed id (tampered `workspace.json`, poisoned socket message) never
/// reaches the adapter at all — invalid shape is stale on its own. Otherwise,
/// only a *definitive* `Gone` clears it: `Exists` and `Unknown` (no session
/// root, or the root is momentarily unreadable — can't tell) both attempt
/// resume, so a transient read error never discards a still-valid resume
/// pointer.
pub fn resolve(adapter: &dyn AgentAdapter, cwd: &Path, stored: Option<&str>) -> SpawnResolution {
    let (session, stale) = match stored {
        None => (None, false),
        Some(s) if !valid_session_id(s) => (None, true),
        Some(s) => match adapter.session_state(cwd, s) {
            SessionState::Gone => (None, true),
            _ => (Some(s.to_string()), false), // Exists or Unknown → try resume
        },
    };
    let wants_detect = session.is_none() && adapter.session_root(cwd).is_some();
    SpawnResolution { session, stale, wants_detect }
}

/// Session ids already assigned to any pane in `ws` — the exclusion set a
/// session-file scan needs so a newly detected session can never steal an id
/// another pane already owns.
pub fn claimed_sessions(ws: &Workspace) -> HashSet<String> {
    ws.tabs.iter().flat_map(|t| t.panes.values()).filter_map(|s| s.session.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::CommandSpec;
    use crate::core::layout::LayoutNode;
    use crate::core::workspace::{PaneSpec, Tab};
    use std::collections::HashMap;
    use std::path::PathBuf;

    /// Adapter whose session root and state are both caller-fixed, so every
    /// branch of `resolve` is driven directly rather than through a real
    /// filesystem — this proves the resolver's own decision table, not
    /// `agents::mod.rs`'s (separately tested) fs walk.
    struct FixedAdapter {
        root: Option<PathBuf>,
        state: SessionState,
    }
    impl AgentAdapter for FixedAdapter {
        fn id(&self) -> &'static str {
            "fixed"
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
        fn session_state(&self, _cwd: &Path, _id: &str) -> SessionState {
            self.state
        }
    }

    /// The stated goal: constructible and exercisable with no `App` in
    /// sight. Also doubles as the `Exists` branch (resume, unchanged).
    #[test]
    fn resolver_is_constructible_and_testable_without_an_app() {
        let adapter = FixedAdapter { root: Some(PathBuf::from("/tmp")), state: SessionState::Exists };
        let out = resolve(&adapter, Path::new("/proj"), Some("real-id"));
        assert_eq!(
            out,
            SpawnResolution { session: Some("real-id".into()), stale: false, wants_detect: false }
        );
    }

    #[test]
    fn gone_session_is_cleared_and_marked_stale() {
        let adapter = FixedAdapter { root: Some(PathBuf::from("/tmp")), state: SessionState::Gone };
        let out = resolve(&adapter, Path::new("/proj"), Some("ghost"));
        assert_eq!(out, SpawnResolution { session: None, stale: true, wants_detect: true });
    }

    #[test]
    fn unknown_session_attempts_resume_without_marking_stale() {
        // Can't tell (no root, or an unreadable one) must never be treated
        // as Gone — that would discard a possibly-still-valid resume
        // pointer over a transient read error.
        let adapter = FixedAdapter { root: Some(PathBuf::from("/tmp")), state: SessionState::Unknown };
        let out = resolve(&adapter, Path::new("/proj"), Some("kept"));
        assert_eq!(out, SpawnResolution { session: Some("kept".into()), stale: false, wants_detect: false });
    }

    #[test]
    fn invalid_session_id_shape_is_stale_without_asking_the_adapter() {
        /// Panics if `session_state` is ever called — proves a malformed id
        /// is rejected on shape alone, before any adapter/fs query.
        struct PanicsIfAsked;
        impl AgentAdapter for PanicsIfAsked {
            fn id(&self) -> &'static str {
                "panics"
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
                Some(PathBuf::from("/tmp"))
            }
            fn session_state(&self, _cwd: &Path, _id: &str) -> SessionState {
                panic!("a malformed id must never reach the adapter");
            }
        }
        let out = resolve(&PanicsIfAsked, Path::new("/proj"), Some("../../etc/passwd"));
        assert_eq!(out, SpawnResolution { session: None, stale: true, wants_detect: true });
    }

    #[test]
    fn a_session_root_with_nothing_stored_arms_detection() {
        let adapter = FixedAdapter { root: Some(PathBuf::from("/tmp")), state: SessionState::Unknown };
        let out = resolve(&adapter, Path::new("/proj"), None);
        assert_eq!(out, SpawnResolution { session: None, stale: false, wants_detect: true });
    }

    #[test]
    fn no_session_root_never_arms_detection_even_with_nothing_stored() {
        let adapter = FixedAdapter { root: None, state: SessionState::Unknown };
        let out = resolve(&adapter, Path::new("/proj"), None);
        assert_eq!(out, SpawnResolution { session: None, stale: false, wants_detect: false });
    }

    #[test]
    fn claimed_sessions_collects_every_pane_session_id_across_tabs() {
        let pane = |session: Option<&str>| PaneSpec {
            adapter: "shell".into(),
            cwd: PathBuf::from("/tmp"),
            session: session.map(String::from),
            title: None,
            spawned_by: None,
            note: None,
            noted_at: None,
        };
        let mut tab1_panes = HashMap::new();
        tab1_panes.insert(1, pane(Some("a")));
        tab1_panes.insert(2, pane(None));
        let mut tab2_panes = HashMap::new();
        tab2_panes.insert(3, pane(Some("b")));
        let ws = Workspace {
            version: 1,
            active_tab: 0,
            tabs: vec![
                Tab { name: "one".into(), layout: LayoutNode::Pane(1), panes: tab1_panes },
                Tab { name: "two".into(), layout: LayoutNode::Pane(3), panes: tab2_panes },
            ],
        };
        let claimed = claimed_sessions(&ws);
        assert_eq!(claimed, ["a".to_string(), "b".to_string()].into_iter().collect());
    }
}
