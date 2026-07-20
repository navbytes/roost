//! Pure layout domain: the tree (splits / stacks / panes), its operations,
//! and geometry. No I/O, no process state — fully unit-testable.

use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};

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

fn subtree_contains(node: &LayoutNode, target: PaneId) -> bool {
    let mut v = Vec::new();
    pane_order(node, &mut v);
    v.contains(&target)
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
    let collapse = matches!(node, LayoutNode::Split { children, .. } if children.len() == 1);
    if collapse {
        if let LayoutNode::Split { children, .. } = node {
            let child = children.remove(0);
            *node = child;
        }
    }
    empty
}

/// Alt+s: if `target` is in a stack, explode the stack back into an even
/// split; otherwise collapse the innermost split that directly contains
/// `target` into a stack of all its leaf panes (with `target` expanded).
pub fn toggle_stack(node: &mut LayoutNode, target: PaneId) -> bool {
    match node {
        LayoutNode::Pane(_) => false,
        LayoutNode::Stack { children, .. } => {
            if !children.contains(&target) {
                return false;
            }
            let n = children.len();
            let panes: Vec<LayoutNode> = children.iter().map(|id| LayoutNode::Pane(*id)).collect();
            *node = LayoutNode::Split {
                dir: SplitDir::Horizontal,
                ratios: vec![1.0 / n as f32; n],
                children: panes,
            };
            true
        }
        LayoutNode::Split { children, .. } => {
            for c in children.iter_mut() {
                if toggle_stack(c, target) {
                    return true;
                }
            }
            let direct = children
                .iter()
                .any(|c| matches!(c, LayoutNode::Pane(id) if *id == target));
            if !direct {
                return false;
            }
            let mut panes = Vec::new();
            for c in children.iter() {
                pane_order(c, &mut panes);
            }
            let expanded = panes.iter().position(|&p| p == target).unwrap_or(0);
            *node = LayoutNode::Stack { children: panes, expanded };
            true
        }
    }
}

/// Alt+Shift+arrows: grow/shrink the subtree containing `target` along the
/// given axis by `delta` (fraction of the parent split). Innermost matching
/// split wins. Ratios are clamped to [0.1, 0.9].
pub fn resize_pane(node: &mut LayoutNode, target: PaneId, axis: SplitDir, delta: f32) -> bool {
    let LayoutNode::Split { dir, ratios, children } = node else {
        return false;
    };
    let Some(i) = children.iter().position(|c| subtree_contains(c, target)) else {
        return false;
    };
    if resize_pane(&mut children[i], target, axis, delta) {
        return true;
    }
    if *dir != axis || children.len() < 2 || ratios.len() != children.len() {
        return false;
    }
    let j = if i + 1 < children.len() { i + 1 } else { i - 1 };
    let new_i = (ratios[i] + delta).clamp(0.1, 0.9);
    let diff = new_i - ratios[i];
    if ratios[j] - diff < 0.1 {
        return true; // at the limit; handled, no change
    }
    ratios[i] += diff;
    ratios[j] -= diff;
    true
}

/// If `target` is a collapsed member of any stack, expand it.
pub fn expand_in_stacks(node: &mut LayoutNode, target: PaneId) {
    match node {
        LayoutNode::Pane(_) => {}
        LayoutNode::Stack { children, expanded } => {
            if let Some(pos) = children.iter().position(|&c| c == target) {
                *expanded = pos;
            }
        }
        LayoutNode::Split { children, .. } => {
            for c in children {
                expand_in_stacks(c, target);
            }
        }
    }
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
    fn toggle_stack_roundtrip() {
        let mut root = tree();
        assert!(toggle_stack(&mut root, 2));
        match &root {
            LayoutNode::Stack { children, expanded } => {
                assert_eq!(children, &vec![1, 2]);
                assert_eq!(*expanded, 1);
            }
            _ => panic!("expected stack"),
        }
        assert!(toggle_stack(&mut root, 2));
        assert!(matches!(root, LayoutNode::Split { .. }));
        let mut order = vec![];
        pane_order(&root, &mut order);
        assert_eq!(order, vec![1, 2]);
    }

    #[test]
    fn resize_adjusts_matching_axis_only() {
        let mut root = tree();
        assert!(!resize_pane(&mut root, 1, SplitDir::Horizontal, 0.05));
        assert!(resize_pane(&mut root, 1, SplitDir::Vertical, 0.05));
        match &root {
            LayoutNode::Split { ratios, .. } => {
                assert!((ratios[0] - 0.55).abs() < 1e-6);
                assert!((ratios[1] - 0.45).abs() < 1e-6);
            }
            _ => panic!(),
        }
        for _ in 0..20 {
            resize_pane(&mut root, 1, SplitDir::Vertical, 0.05);
        }
        match &root {
            LayoutNode::Split { ratios, .. } => assert!(ratios[1] >= 0.1 - 1e-6),
            _ => panic!(),
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

    #[test]
    fn expand_in_stacks_expands_target() {
        let mut node = LayoutNode::Stack { children: vec![1, 2, 3], expanded: 0 };
        expand_in_stacks(&mut node, 3);
        assert!(matches!(node, LayoutNode::Stack { expanded: 2, .. }));
    }
}
