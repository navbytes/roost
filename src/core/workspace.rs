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
            PaneSpec { adapter: "shell".into(), cwd, session: None, title: None },
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

    pub fn active_tab(&self) -> &Tab {
        &self.tabs[self.active_tab.min(self.tabs.len() - 1)]
    }

    pub fn active_tab_mut(&mut self) -> &mut Tab {
        let i = self.active_tab.min(self.tabs.len() - 1);
        &mut self.tabs[i]
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

    #[test]
    fn roundtrips_through_json() {
        let ws = Workspace::default_in(PathBuf::from("/tmp"));
        let json = serde_json::to_string(&ws).unwrap();
        let back: Workspace = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tabs[0].name, "main");
        assert!(back.tabs[0].panes[&1].session.is_none());
    }
}
