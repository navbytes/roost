//! The precious state: layout tree + (adapter, cwd, session-id) per pane.
//! Everything here is serde-serializable and persisted to
//! `~/.local/state/roost/workspace.json` (atomic, debounced).

use anyhow::{Context, Result};
use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

pub type PaneId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDir {
    /// Split along a vertical line → children sit side by side.
    Vertical,
    /// Split along a horizontal line → children stack top/bottom.
    Horizontal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutNode {
    Split {
        dir: SplitDir,
        ratios: Vec<f32>,
        children: Vec<LayoutNode>,
    },
    /// Zellij-style stack: collapsed panes are 1-row title bars, one pane expanded.
    Stack { children: Vec<PaneId>, expanded: usize },
    Pane(PaneId),
}

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
    pub fn state_path() -> PathBuf {
        dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("roost")
            .join("workspace.json")
    }

    /// Load the saved workspace, or build a default one: a single tab with a
    /// single shell pane in the current directory.
    pub fn load_or_default() -> Result<Self> {
        let path = Self::state_path();
        if path.exists() {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let ws: Workspace = serde_json::from_str(&raw).context("parsing workspace.json")?;
            if !ws.tabs.is_empty() {
                return Ok(ws);
            }
        }
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let mut panes = HashMap::new();
        panes.insert(
            1,
            PaneSpec { adapter: "shell".into(), cwd, session: None, title: None },
        );
        Ok(Workspace {
            version: 1,
            active_tab: 0,
            tabs: vec![Tab { name: "main".into(), layout: LayoutNode::Pane(1), panes }],
        })
    }

    /// Atomic save: write temp file, then rename over the old one.
    pub fn save(&self) -> Result<()> {
        let path = Self::state_path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        fs::rename(&tmp, &path)?;
        Ok(())
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

// ---------------------------------------------------------------------------
// Layout operations
// ---------------------------------------------------------------------------

/// DFS pane order — used for focus cycling.
pub fn pane_order(node: &LayoutNode, out: &mut Vec<PaneId>) {
    match node {
        LayoutNode::Pane(id) => out.push(*id),
        LayoutNode::Stack { children, .. } => out.extend(children.iter().copied()),
        LayoutNode::Split { children, .. } => {
            for c in children {
                pane_order(c, out);
            }
        }
    }
}

/// Replace `Pane(target)` with a two-way split containing the old and new pane.
/// If the target lives in a stack, the new pane joins the stack instead.
pub fn split_pane(node: &mut LayoutNode, target: PaneId, new: PaneId, dir: SplitDir) -> bool {
    match node {
        LayoutNode::Pane(id) if *id == target => {
            *node = LayoutNode::Split {
                dir,
                ratios: vec![0.5, 0.5],
                children: vec![LayoutNode::Pane(target), LayoutNode::Pane(new)],
            };
            true
        }
        LayoutNode::Pane(_) => false,
        LayoutNode::Stack { children, expanded } => {
            if children.contains(&target) {
                children.push(new);
                *expanded = children.len() - 1;
                true
            } else {
                false
            }
        }
        LayoutNode::Split { children, .. } => {
            children.iter_mut().any(|c| split_pane(c, target, new, dir))
        }
    }
}

/// Remove a pane from the tree, pruning empty splits/stacks and collapsing
/// single-child splits. Returns true if `node` itself became empty and should
/// be removed by its parent (at the root this means the tab is empty).
pub fn remove_pane(node: &mut LayoutNode, target: PaneId) -> bool {
    let empty = match node {
        LayoutNode::Pane(id) => *id == target,
        LayoutNode::Stack { children, expanded } => {
            if let Some(pos) = children.iter().position(|c| *c == target) {
                children.remove(pos);
                if !children.is_empty() && *expanded >= children.len() {
                    *expanded = children.len() - 1;
                }
            }
            children.is_empty()
        }
        LayoutNode::Split { children, ratios, .. } => {
            let mut i = 0;
            while i < children.len() {
                if remove_pane(&mut children[i], target) {
                    children.remove(i);
                    if i < ratios.len() {
                        ratios.remove(i);
                    }
                } else {
                    i += 1;
                }
            }
            let sum: f32 = ratios.iter().sum();
            if sum > 0.0 {
                for r in ratios.iter_mut() {
                    *r /= sum;
                }
            }
            children.is_empty()
        }
    };
    // Collapse `Split` with a single child into that child.
    let collapse = matches!(node, LayoutNode::Split { children, .. } if children.len() == 1);
    if collapse {
        if let LayoutNode::Split { children, .. } = node {
            let child = children.remove(0);
            *node = child;
        }
    }
    empty
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct PaneRect {
    pub id: PaneId,
    pub rect: Rect,
    /// Collapsed stack member: rendered as a 1-row title bar only.
    pub collapsed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> LayoutNode {
        LayoutNode::Split {
            dir: SplitDir::Vertical,
            ratios: vec![0.5, 0.5],
            children: vec![LayoutNode::Pane(1), LayoutNode::Pane(2)],
        }
    }

    #[test]
    fn split_then_remove_collapses() {
        let mut root = LayoutNode::Pane(1);
        assert!(split_pane(&mut root, 1, 2, SplitDir::Vertical));
        let mut order = vec![];
        pane_order(&root, &mut order);
        assert_eq!(order, vec![1, 2]);
        assert!(!remove_pane(&mut root, 2));
        assert!(matches!(root, LayoutNode::Pane(1)));
        assert!(remove_pane(&mut root, 1)); // root empty
    }

    #[test]
    fn stack_gains_new_pane_and_expands_it() {
        let mut root = LayoutNode::Stack { children: vec![1, 2], expanded: 0 };
        assert!(split_pane(&mut root, 2, 3, SplitDir::Horizontal));
        match &root {
            LayoutNode::Stack { children, expanded } => {
                assert_eq!(children, &vec![1, 2, 3]);
                assert_eq!(*expanded, 2);
            }
            _ => panic!("stack expected"),
        }
    }

    #[test]
    fn rects_cover_area_without_overlap() {
        let mut out = vec![];
        compute_rects(&tree(), Rect::new(0, 0, 80, 24), &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].rect, Rect::new(0, 0, 40, 24));
        assert_eq!(out[1].rect, Rect::new(40, 0, 40, 24));
    }

    #[test]
    fn stack_rects_collapse_to_title_bars() {
        let node = LayoutNode::Stack { children: vec![1, 2, 3], expanded: 1 };
        let mut out = vec![];
        compute_rects(&node, Rect::new(0, 0, 80, 20), &mut out);
        assert_eq!(out.iter().filter(|p| p.collapsed).count(), 2);
        let expanded = out.iter().find(|p| !p.collapsed).unwrap();
        assert_eq!(expanded.rect.height, 18);
        let total: u16 = out.iter().map(|p| p.rect.height).sum();
        assert_eq!(total, 20);
    }
}

/// Walk the layout tree and assign every pane a rectangle within `area`.
pub fn compute_rects(node: &LayoutNode, area: Rect, out: &mut Vec<PaneRect>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    match node {
        LayoutNode::Pane(id) => out.push(PaneRect { id: *id, rect: area, collapsed: false }),
        LayoutNode::Stack { children, expanded } => {
            let n = children.len() as u16;
            if n == 0 {
                return;
            }
            let mut y = area.y;
            for (i, id) in children.iter().enumerate() {
                let h = if i == *expanded {
                    area.height.saturating_sub(n.saturating_sub(1))
                } else {
                    1
                };
                let h = h.min((area.y + area.height).saturating_sub(y));
                out.push(PaneRect {
                    id: *id,
                    rect: Rect::new(area.x, y, area.width, h),
                    collapsed: i != *expanded,
                });
                y = y.saturating_add(h);
            }
        }
        LayoutNode::Split { dir, ratios, children } => {
            let total = match dir {
                SplitDir::Vertical => area.width,
                SplitDir::Horizontal => area.height,
            };
            let mut offset = 0u16;
            let n = children.len();
            for (i, child) in children.iter().enumerate() {
                let ratio = ratios.get(i).copied().unwrap_or(1.0 / n as f32);
                let size = if i == n - 1 {
                    total.saturating_sub(offset)
                } else {
                    ((total as f32 * ratio).round() as u16).min(total.saturating_sub(offset))
                };
                let rect = match dir {
                    SplitDir::Vertical => Rect::new(area.x + offset, area.y, size, area.height),
                    SplitDir::Horizontal => Rect::new(area.x, area.y + offset, area.width, size),
                };
                offset = offset.saturating_add(size);
                compute_rects(child, rect, out);
            }
        }
    }
}
