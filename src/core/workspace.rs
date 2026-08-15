//! The precious state: tabs + (adapter, cwd, session-id) per pane.
//! Pure data + queries; persistence lives behind `ports::StateStore`
//! (production impl: `infra::store::FsStore`).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

pub use crate::core::layout::{LayoutNode, PaneId};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneSpec {
    /// Adapter id: "pi", "claude", "shell", ...
    pub adapter: String,
    pub cwd: PathBuf,
    /// The agent CLI's own session id — the thing that makes panes resumable.
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// The pane that spawned this one via the control interface, if any. Roots
    /// the ownership-scoped capability model: a controlling pane may drive the
    /// panes in its own spawned subtree, but not panes it didn't create.
    #[serde(default)]
    pub spawned_by: Option<PaneId>,
    /// The pane's parking note (Alt+Shift+n): where this pane stands and
    /// what's next, left for a future session of the person. Newlines
    /// separate lines; the **first line is the headline** chrome shows —
    /// the C4 badge renders headline + age on the focused pane and a bare
    /// `¶` elsewhere. `None` = no note (an empty save clears back to this).
    #[serde(default)]
    pub note: Option<String>,
    /// When `note` was last saved, unix seconds — feeds the badge's age tag
    /// (`5m`/`3h`/`2d`), which is how a stale note confesses its age
    /// instead of reading as current. Set and cleared with `note`, never
    /// separately.
    #[serde(default)]
    pub noted_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    pub name: String,
    pub layout: LayoutNode,
    pub panes: HashMap<PaneId, PaneSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub version: u32,
    pub active_tab: usize,
    pub tabs: Vec<Tab>,
}

impl Workspace {
    /// Default workspace: a single tab with a single shell pane in `cwd`.
    pub fn default_in(cwd: PathBuf) -> Self {
        let mut panes = HashMap::new();
        panes.insert(
            1,
            PaneSpec { adapter: "shell".into(), cwd, session: None, title: None, spawned_by: None, note: None, noted_at: None },
        );
        Workspace {
            version: 1,
            active_tab: 0,
            tabs: vec![Tab { name: "main".into(), layout: LayoutNode::Pane(1), panes }],
        }
    }

    pub fn next_pane_id(&self) -> PaneId {
        self.tabs
            .iter()
            .flat_map(|t| t.panes.keys())
            .copied()
            .max()
            .unwrap_or(0)
            + 1
    }

    /// `tabs` is never actually empty (`validate_and_repair` guarantees it on
    /// load; `close_pane_id` only removes a tab when more than one remains),
    /// but this is called from ~40 sites, so the access itself stays
    /// underflow-proof rather than relying on that invariant: no
    /// `tabs.len() - 1`, which panics on subtraction overflow in debug and
    /// silently wraps to `usize::MAX` (before an equally confusing
    /// out-of-bounds panic) in release, the moment `tabs` is ever empty.
    pub fn active_tab(&self) -> &Tab {
        self.tabs
            .get(self.active_tab)
            .or(self.tabs.first())
            .expect("a workspace always has at least one tab (validate_and_repair)")
    }

    pub fn active_tab_mut(&mut self) -> &mut Tab {
        // Same guard as `active_tab`, adapted for `&mut`: `Option::or` would
        // need two live mutable borrows of `self.tabs` at once, so clamp the
        // index first (no subtraction) and take a single `get_mut`.
        let i = if self.active_tab < self.tabs.len() { self.active_tab } else { 0 };
        self.tabs
            .get_mut(i)
            .expect("a workspace always has at least one tab (validate_and_repair)")
    }

    /// Repair layout ↔ panes inconsistencies after loading a (possibly
    /// hand-edited or migrated) workspace.json: drop pane specs that have no
    /// place in the layout tree, and give any layout leaf that lacks a spec a
    /// minimal shell spec so it renders and spawns instead of being a blank
    /// hole. A well-formed workspace is unchanged. Also clamps `active_tab`.
    pub fn validate_and_repair(&mut self) {
        for tab in &mut self.tabs {
            let mut ids = Vec::new();
            crate::core::layout::pane_order(&tab.layout, &mut ids);
            let in_layout: std::collections::HashSet<PaneId> = ids.iter().copied().collect();
            tab.panes.retain(|id, _| in_layout.contains(id));
            for id in ids {
                tab.panes.entry(id).or_insert_with(|| PaneSpec {
                    adapter: "shell".into(),
                    cwd: std::env::current_dir().unwrap_or_else(|_| "/".into()),
                    session: None,
                    title: None,
                    spawned_by: None,
                    note: None,
                    noted_at: None,
                });
            }
        }
        // A tab whose layout holds no panes at all draws as a blank body with
        // no key that makes a pane — and roost saves exactly that: closing the
        // last pane quits, but the removal still runs, and with one tab left
        // there is no tab to drop, so an emptied `Stack` root reaches disk (a
        // bare `Pane` root gets refilled by the loop above; a `Stack` has no
        // leaf left to refill). Drop such tabs, and start over if none survive.
        self.tabs.retain(|t| !t.panes.is_empty());
        if self.tabs.is_empty() {
            *self = Workspace::default_in(std::env::current_dir().unwrap_or_else(|_| "/".into()));
            return;
        }
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len().saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_one_shell_pane() {
        let ws = Workspace::default_in(PathBuf::from("/tmp"));
        assert_eq!(ws.tabs.len(), 1);
        assert_eq!(ws.active_tab().panes[&1].adapter, "shell");
        assert_eq!(ws.next_pane_id(), 2);
    }

    /// P3: an empty `tabs` (unreachable via the public API, but not via a
    /// direct struct literal) has no tab to hand back, so this still panics
    /// — but it must be OUR clear panic, not the `len() - 1` underflow one.
    /// `#[should_panic(expected = ...)]` fails if the message reverts to the
    /// arithmetic-overflow wording, which is exactly the regression this pins.
    #[test]
    #[should_panic(expected = "a workspace always has at least one tab")]
    fn active_tab_on_an_empty_workspace_panics_clearly_not_via_underflow() {
        let empty = Workspace { version: 1, active_tab: 0, tabs: Vec::new() };
        let _ = empty.active_tab();
    }

    #[test]
    #[should_panic(expected = "a workspace always has at least one tab")]
    fn active_tab_mut_on_an_empty_workspace_panics_clearly_not_via_underflow() {
        let mut empty = Workspace { version: 1, active_tab: 0, tabs: Vec::new() };
        let _ = empty.active_tab_mut();
    }

    #[test]
    fn validate_repairs_orphans_both_ways() {
        let mut ws = Workspace::default_in(PathBuf::from("/tmp"));
        // Orphan spec: a pane id with no place in the layout tree.
        ws.tabs[0].panes.insert(
            5,
            PaneSpec { adapter: "shell".into(), cwd: "/tmp".into(), session: None, title: None, spawned_by: None, note: None, noted_at: None },
        );
        // Orphan layout leaf: the layout references pane 1, but drop its spec.
        ws.tabs[0].panes.remove(&1);
        ws.validate_and_repair();
        // Orphan spec dropped...
        assert!(!ws.tabs[0].panes.contains_key(&5));
        // ...and the layout leaf refilled with a minimal shell spec.
        assert_eq!(ws.tabs[0].panes.get(&1).map(|s| s.adapter.as_str()), Some("shell"));
    }

    /// The blank-screen bug: a tab left with an empty `Stack` root (closing
    /// the last pane of the last tab) must never load as zero panes.
    #[test]
    fn validate_replaces_a_workspace_of_empty_tabs() {
        let mut ws = Workspace::default_in(PathBuf::from("/tmp"));
        ws.tabs[0].layout = LayoutNode::Stack { children: Vec::new(), expanded: 0 };
        ws.validate_and_repair();
        assert_eq!(ws.tabs.len(), 1);
        assert!(!ws.tabs[0].panes.is_empty(), "a loaded workspace always has a pane to draw");
    }

    #[test]
    fn validate_drops_an_empty_tab_beside_a_live_one() {
        let mut ws = Workspace::default_in(PathBuf::from("/tmp"));
        ws.tabs.push(Tab {
            name: "tab2".into(),
            layout: LayoutNode::Stack { children: Vec::new(), expanded: 0 },
            panes: HashMap::new(),
        });
        ws.active_tab = 1;
        ws.validate_and_repair();
        assert_eq!(ws.tabs.len(), 1);
        assert_eq!(ws.tabs[0].name, "main");
        assert_eq!(ws.active_tab, 0);
    }

    #[test]
    fn roundtrips_through_json() {
        let ws = Workspace::default_in(PathBuf::from("/tmp"));
        let json = serde_json::to_string(&ws).unwrap();
        let back: Workspace = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tabs[0].name, "main");
        assert!(back.tabs[0].panes[&1].session.is_none());
    }

    /// A parking note — newlines included — survives the save/load cycle,
    /// timestamp attached. This is the whole overnight story: the note is
    /// ordinary `PaneSpec` data, so it rides the same auto-save the
    /// session id does.
    #[test]
    fn note_and_timestamp_roundtrip_through_json() {
        let mut ws = Workspace::default_in(PathBuf::from("/tmp"));
        let spec = ws.tabs[0].panes.get_mut(&1).unwrap();
        spec.note = Some("tests green, PR up\nnext: rebase, merge".into());
        spec.noted_at = Some(1_755_200_000);
        let json = serde_json::to_string(&ws).unwrap();
        let back: Workspace = serde_json::from_str(&json).unwrap();
        let spec = &back.tabs[0].panes[&1];
        assert_eq!(spec.note.as_deref(), Some("tests green, PR up\nnext: rebase, merge"));
        assert_eq!(spec.noted_at, Some(1_755_200_000));
    }

    /// A workspace.json written before notes existed loads with both fields
    /// `None` — `#[serde(default)]`, pinned so the fields can never become
    /// load-breaking for the state file everyone already has on disk.
    #[test]
    fn pre_note_workspace_json_loads_with_no_note() {
        let json = r#"{"version":1,"active_tab":0,"tabs":[{"name":"main",
            "layout":{"pane":1},
            "panes":{"1":{"adapter":"shell","cwd":"/tmp"}}}]}"#;
        let back: Workspace = serde_json::from_str(json).unwrap();
        let spec = &back.tabs[0].panes[&1];
        assert!(spec.note.is_none());
        assert!(spec.noted_at.is_none());
    }
}
