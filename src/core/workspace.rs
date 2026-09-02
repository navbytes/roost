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
    /// The pane's parking note (the Alt+r editor): where this pane stands and
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
            PaneSpec {
                adapter: "shell".into(),
                cwd,
                session: None,
                title: None,
                spawned_by: None,
                note: None,
                noted_at: None,
            },
        );
        Workspace {
            version: 1,
            active_tab: 0,
            tabs: vec![Tab { name: "main".into(), layout: LayoutNode::Pane(1), panes }],
        }
    }

    pub fn next_pane_id(&self) -> PaneId {
        self.tabs.iter().flat_map(|t| t.panes.keys()).copied().max().unwrap_or(0) + 1
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
        self.tabs.get_mut(i).expect("a workspace always has at least one tab (validate_and_repair)")
    }

    /// Repair layout ↔ panes inconsistencies after loading a (possibly
    /// hand-edited or migrated) workspace.json: drop pane specs that have no
    /// place in the layout tree, and give any layout leaf that lacks a spec a
    /// minimal shell spec so it renders and spawns instead of being a blank
    /// hole. A well-formed workspace is unchanged. Also clamps `active_tab`.
    pub fn validate_and_repair(&mut self) {
        // A pane id keys `runtimes` globally, so one id in two positions —
        // repeated inside a tree, or shared between two tabs — means both
        // draw and type through a single PTY, `tab_of` answers with
        // whichever tab comes first, and closing either takes the other with
        // it. Keep the first occurrence, drop the rest; the per-tab
        // reconciliation below then drops the orphaned specs, and an emptied
        // tab goes with the `retain` further down.
        let mut seen: std::collections::HashSet<PaneId> = std::collections::HashSet::new();
        // `dedupe_pane_ids` answers "this subtree is now empty", which is how
        // a parent knows to drop the child. The **root** has no parent, and
        // this call site used to throw the answer away — so a tab whose whole
        // layout is a bare `Pane` holding an id an earlier tab already
        // claimed survived deduping untouched, and the reconciliation below
        // then minted it a fresh spec. The duplicate this pass exists to
        // remove, recreated one loop later, and with it every consequence the
        // comment above lists. (Only the bare-root shape escaped: a duplicate
        // inside a `Split` or `Stack` is removed by its parent, and a
        // container left empty reports empty in turn.)
        let emptied: Vec<bool> = self
            .tabs
            .iter_mut()
            .map(|tab| crate::core::layout::dedupe_pane_ids(&mut tab.layout, &mut seen))
            .collect();
        let mut i = 0;
        self.tabs.retain(|_| {
            let keep = !emptied[i];
            i += 1;
            keep
        });
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
        // no key that makes a pane. Drop such tabs, and start over if none
        // survive.
        //
        // roost no longer *writes* one: closing the last pane used to remove
        // it and then quit, and with one tab left there was no tab to drop
        // either, so the emptied tab reached disk — where the refill loop
        // above minted the id a blank `shell` spec and the pane's session,
        // cwd, title and note were gone (see `App::close_pane`, which now
        // quits without removing, exactly as `Alt+q` does). What is left
        // here is the boundary this function exists to be: `workspace.json`
        // is hand-editable, and a file written by an older roost, an editor,
        // or a bad merge still has to become something roost can run.
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

/// `workspace.json` is the one file the whole tool is, and it is
/// hand-editable — so `validate_and_repair` is the boundary where an
/// arbitrary one becomes a workspace roost can run. These generate
/// structurally valid but deliberately broken files and check that what
/// comes out the other side satisfies every invariant the rest of the code
/// then assumes.
#[cfg(test)]
mod repair_fuzz {
    use serde_json::json;

    /// A tab whose entire layout is one pane an earlier tab already claimed.
    ///
    /// `dedupe_pane_ids` answers "this subtree is now empty" so a parent can
    /// drop the child — but a root has no parent, and `validate_and_repair`
    /// used to discard that answer. The tab survived deduping untouched and
    /// the reconciliation then minted it a fresh spec, so the id ended up in
    /// two tabs: one PTY drawn in two places, keystrokes landing in both,
    /// `tab_of` answering with whichever comes first, and closing either
    /// taking the other with it. Exactly the failure the dedupe pass was
    /// added for, through the one shape it could not reach.
    #[test]
    fn a_tab_that_is_only_a_duplicate_pane_does_not_survive_the_repair() {
        let raw = r#"{"version":1,"active_tab":0,"tabs":[
            {"name":"a","layout":{"split":{"dir":"vertical","ratios":[],
              "children":[{"pane":3},{"pane":0}]}},"panes":{}},
            {"name":"b","layout":{"pane":0},"panes":{"0":{"adapter":"pi","cwd":"/tmp"}}}
        ]}"#;
        let mut ws: super::Workspace = serde_json::from_str(raw).unwrap();
        ws.validate_and_repair();
        let mut owner = std::collections::HashMap::new();
        for (i, tab) in ws.tabs.iter().enumerate() {
            let mut ids = Vec::new();
            crate::core::layout::pane_order(&tab.layout, &mut ids);
            for id in ids {
                if let Some(prev) = owner.insert(id, i) {
                    panic!("pane {id} is in tab {prev} and tab {i} after the repair");
                }
            }
        }
    }

    struct R(u64);
    impl R {
        fn n(&mut self, m: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % m.max(1)
        }
    }

    /// A layout tree with every shape a hand-edited file can hold: duplicate
    /// ids drawn from a tiny pool, containers with zero and one child,
    /// `ratios` that do not match `children`, and `expanded` out of range.
    fn node(r: &mut R, depth: u32) -> serde_json::Value {
        if depth == 0 || r.n(3) == 0 {
            return json!({ "pane": r.n(4) });
        }
        let kids: Vec<_> = (0..r.n(4)).map(|_| node(r, depth - 1)).collect();
        if r.n(2) == 0 {
            let ratios: Vec<f64> = (0..r.n(5)).map(|_| r.n(100) as f64 / 100.0).collect();
            json!({"split": {
                "dir": if r.n(2) == 0 { "vertical" } else { "horizontal" },
                "ratios": ratios,
                "children": kids,
            }})
        } else {
            json!({"stack": { "children": kids, "expanded": r.n(6) }})
        }
    }

    /// Whatever goes in, what comes out must satisfy every invariant the
    /// rest of roost assumes of a loaded workspace — the same set
    /// `core::app`'s layout fuzzer re-checks after each of its operations,
    /// asked here of the parse-and-repair boundary instead.
    #[test]
    fn any_workspace_repairs_into_one_roost_can_run() {
        let mut r = R(0xC0FFEE123);
        let mut parsed = 0u32;
        for _ in 0..15_000u32 {
            let tabs: Vec<_> = (0..r.n(4))
                .map(|_| {
                    let mut panes = serde_json::Map::new();
                    for _ in 0..r.n(5) {
                        panes
                            .insert(r.n(4).to_string(), json!({"adapter": "shell", "cwd": "/tmp"}));
                    }
                    json!({"name": "t", "layout": node(&mut r, 3), "panes": panes})
                })
                .collect();
            let raw = json!({"version": 1, "active_tab": r.n(6), "tabs": tabs}).to_string();
            let Ok(mut ws) = serde_json::from_str::<super::Workspace>(&raw) else { continue };
            parsed += 1;
            ws.validate_and_repair();

            assert!(!ws.tabs.is_empty(), "a repair always leaves a tab to show: {raw}");
            assert!(ws.active_tab < ws.tabs.len(), "active_tab names a real tab: {raw}");
            let _ = ws.active_tab(); // the accessor that documents the two above
            let mut owner = std::collections::HashMap::new();
            for (i, tab) in ws.tabs.iter().enumerate() {
                let mut ids = Vec::new();
                crate::core::layout::pane_order(&tab.layout, &mut ids);
                let tree: std::collections::HashSet<_> = ids.iter().copied().collect();
                assert_eq!(tree.len(), ids.len(), "a tree lists a pane twice: {raw}");
                let keys: std::collections::HashSet<_> = tab.panes.keys().copied().collect();
                assert_eq!(tree, keys, "tree ids and panes map disagree: {raw}");
                for id in ids {
                    if let Some(prev) = owner.insert(id, i) {
                        panic!("pane {id} is in tab {prev} AND tab {i}: {raw}");
                    }
                }
            }
        }
        eprintln!("workspace repair fuzz: {parsed} adversarial workspaces");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pane id must name exactly one pane. `workspace.json` is a plain
    /// file the user can edit (and the format roost's own repair pass exists
    /// to survive), but the repair reconciled each tab's `panes` map against
    /// its own layout only — it never asked whether an id appeared twice.
    ///
    /// One id in two trees means one entry in `runtimes`, so both positions
    /// drive a single PTY: the pane draws in two places, keystrokes aimed at
    /// one land in both, `tab_of` answers with whichever tab comes first,
    /// and closing either takes the other with it. The same holds for an id
    /// repeated inside a single tab.
    /// C6: a one-member stack must not survive a load.
    ///
    /// `remove_pane` collapses a stack that *shrinks* to one member, but a
    /// `workspace.json` can hold one outright — and so can a stack whose
    /// other members were duplicate ids the repair pass stripped. Either way
    /// the renderer gets a `StackHeader { n: 1 }`: a header row reading
    /// "STACK · 1 PANES", stolen from the only pane it describes.
    #[test]
    fn a_one_member_stack_does_not_survive_a_load() {
        for (what, json) in [
            (
                "written that way",
                r#"{"version":1,"active_tab":0,"tabs":[
                    {"name":"main","layout":{"stack":{"children":[1],"expanded":0}},
                     "panes":{"1":{"adapter":"shell","cwd":"/tmp"}}}]}"#,
            ),
            (
                "left that way by the duplicate-id repair",
                r#"{"version":1,"active_tab":0,"tabs":[
                    {"name":"main","layout":{"stack":{"children":[1,1,1],"expanded":0}},
                     "panes":{"1":{"adapter":"shell","cwd":"/tmp"}}}]}"#,
            ),
        ] {
            let mut ws: Workspace = serde_json::from_str(json).expect("parses");
            ws.validate_and_repair();
            assert!(
                matches!(ws.tabs[0].layout, LayoutNode::Pane(1)),
                "{what}: a stack of one reached the renderer: {:?}",
                ws.tabs[0].layout,
            );
        }
    }

    #[test]
    fn a_repaired_workspace_never_has_one_id_in_two_places() {
        let json = r#"{"version":1,"active_tab":0,"tabs":[
            {"name":"main","layout":{"split":{"dir":"vertical","ratios":[0.5,0.5],
             "children":[{"pane":1},{"pane":1}]}},
             "panes":{"1":{"adapter":"shell","cwd":"/tmp"}}},
            {"name":"api","layout":{"split":{"dir":"vertical","ratios":[0.5,0.5],
             "children":[{"pane":1},{"pane":2}]}},
             "panes":{"1":{"adapter":"shell","cwd":"/tmp"},
                      "2":{"adapter":"shell","cwd":"/tmp"}}}]}"#;
        let mut ws: Workspace = serde_json::from_str(json).expect("parses");
        ws.validate_and_repair();

        let mut all = Vec::new();
        for tab in &ws.tabs {
            let mut ids = Vec::new();
            crate::core::layout::pane_order(&tab.layout, &mut ids);
            for id in &ids {
                assert!(
                    tab.panes.contains_key(id),
                    "tab {} has {id} in its layout with no spec",
                    tab.name,
                );
            }
            all.extend(ids);
        }
        let mut uniq = all.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(all.len(), uniq.len(), "one id names two panes: {all:?}");
        // The duplicate is dropped, not the pane: the first occurrence and
        // every distinct id survive.
        assert!(all.contains(&1) && all.contains(&2), "repair lost a real pane: {all:?}");
    }

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
            PaneSpec {
                adapter: "shell".into(),
                cwd: "/tmp".into(),
                session: None,
                title: None,
                spawned_by: None,
                note: None,
                noted_at: None,
            },
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
        ws.tabs[0].layout = LayoutNode::Stack { children: Vec::new(), expanded: 0, from: None };
        ws.validate_and_repair();
        assert_eq!(ws.tabs.len(), 1);
        assert!(!ws.tabs[0].panes.is_empty(), "a loaded workspace always has a pane to draw");
    }

    #[test]
    fn validate_drops_an_empty_tab_beside_a_live_one() {
        let mut ws = Workspace::default_in(PathBuf::from("/tmp"));
        ws.tabs.push(Tab {
            name: "tab2".into(),
            layout: LayoutNode::Stack { children: Vec::new(), expanded: 0, from: None },
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
