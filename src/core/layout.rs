//! Pure layout domain: the tree (splits / stacks / panes), its operations,
//! and geometry. No I/O, no process state — fully unit-testable.

use std::collections::HashSet;

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

/// Flip the orientation (vertical ⇄ horizontal) of the innermost split that
/// directly contains `target` — turning a side-by-side pair into a stacked
/// one and vice versa. Ratios are preserved. No-op if the focused pane isn't
/// a direct child of a split (e.g. it's in a stack).
pub fn flip_split(node: &mut LayoutNode, target: PaneId) -> bool {
    let LayoutNode::Split { dir, children, .. } = node else {
        return false;
    };
    // Deeper split first, so nested layouts flip the split closest to focus.
    for c in children.iter_mut() {
        if flip_split(c, target) {
            return true;
        }
    }
    let direct = children.iter().any(|c| matches!(c, LayoutNode::Pane(id) if *id == target));
    if !direct {
        return false;
    }
    *dir = match dir {
        SplitDir::Vertical => SplitDir::Horizontal,
        SplitDir::Horizontal => SplitDir::Vertical,
    };
    true
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

/// A spatial focus direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// The pane nearest to `focused` in direction `dir`, by pane rectangles.
/// Returns None when nothing lies that way (focus then stays put — the
/// predictable tmux/zellij behavior, and the fix for cyclic focus feeling
/// "inverted" relative to arrow keys). `rects` are the laid-out pane rects.
pub fn neighbor(rects: &[PaneRect], focused: PaneId, dir: Dir) -> Option<PaneId> {
    let cur = rects.iter().find(|p| p.id == focused)?.rect;
    let cx = cur.x as i32 + cur.width as i32 / 2;
    let cy = cur.y as i32 + cur.height as i32 / 2;
    let mut best: Option<(i32, PaneId)> = None;
    for p in rects {
        if p.id == focused {
            continue;
        }
        let r = p.rect;
        let px = r.x as i32 + r.width as i32 / 2;
        let py = r.y as i32 + r.height as i32 / 2;
        let in_dir = match dir {
            Dir::Left => px < cx,
            Dir::Right => px > cx,
            Dir::Up => py < cy,
            Dir::Down => py > cy,
        };
        if !in_dir {
            continue;
        }
        // Distance dominated by the primary axis, perpendicular offset as a
        // tie-breaker so we pick the pane most directly in that direction.
        let (prim, perp) = match dir {
            Dir::Left | Dir::Right => ((cx - px).abs(), (cy - py).abs()),
            Dir::Up | Dir::Down => ((cy - py).abs(), (cx - px).abs()),
        };
        let score = prim * 4 + perp;
        if best.is_none_or(|(s, _)| score < s) {
            best = Some((score, p.id));
        }
    }
    best.map(|(_, id)| id)
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

/// A stack's header row (C6) — shown above its members, in the space it
/// borrows from them, when `stack_header_shown` says there's room. Not a
/// `PaneRect`: it belongs to no pane (clicks/wheel events over it hit
/// nothing — see `compute_rects_and_headers`, which never emits a pane rect
/// covering this row).
#[derive(Debug, Clone, Copy)]
pub struct StackHeader {
    pub rect: Rect,
    /// Member count, for the "STACK · N PANES" label.
    pub n: usize,
}

/// C6: a stack spares a header row only when every member still clears its
/// floor afterward — 1 row each for the `n-1` collapsed members, and at
/// least 3 for the expanded one, plus the header's own row. Below that,
/// geometry is exactly as if there were no header at all.
fn stack_header_shown(area_height: u16, n: usize) -> bool {
    area_height >= n as u16 + 3
}

/// Walk the layout tree and assign every pane a rectangle within `area`.
pub fn compute_rects(node: &LayoutNode, area: Rect, out: &mut Vec<PaneRect>) {
    compute_rects_and_headers(node, area, out, &mut Vec::new());
}

/// The one real geometry walk: pane rects (as `compute_rects`) plus every
/// stack's header row (C6), so the two can never disagree about where a
/// header lands or how much it shrinks its stack's expanded member.
pub fn compute_rects_and_headers(
    node: &LayoutNode,
    area: Rect,
    out: &mut Vec<PaneRect>,
    headers: &mut Vec<StackHeader>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    match node {
        LayoutNode::Pane(id) => out.push(PaneRect { id: *id, rect: area, collapsed: false }),
        LayoutNode::Stack { children, expanded } => {
            let n = children.len();
            if n == 0 {
                return;
            }
            let n16 = n as u16;
            let show_header = stack_header_shown(area.height, n);
            let mut y = area.y;
            if show_header {
                headers.push(StackHeader { rect: Rect::new(area.x, area.y, area.width, 1), n });
                y = y.saturating_add(1);
            }
            let avail = area.height.saturating_sub(if show_header { 1 } else { 0 });
            for (i, id) in children.iter().enumerate() {
                let h = if i == *expanded {
                    avail.saturating_sub(n16.saturating_sub(1))
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
                compute_rects_and_headers(child, rect, out, headers);
            }
        }
    }
}

/// Just the stack header rows (C6), for the renderer — which already gets
/// pane rects from `App::rects`/`compute_rects` and only needs the header
/// geometry alongside them. Same underlying walk, so it can't drift from
/// `compute_rects`.
pub fn stack_headers(node: &LayoutNode, area: Rect) -> Vec<StackHeader> {
    let mut headers = Vec::new();
    compute_rects_and_headers(node, area, &mut Vec::new(), &mut headers);
    headers
}

/// PaneIds that are the currently-expanded member of a `Stack` node (C7) —
/// distinct from an ordinary split-pane leaf, which is never a member of
/// this set. Independent of whether that stack's header row (C6) is shown.
pub fn stack_expanded_ids(node: &LayoutNode, out: &mut HashSet<PaneId>) {
    match node {
        LayoutNode::Pane(_) => {}
        LayoutNode::Stack { children, expanded } => {
            if let Some(&id) = children.get(*expanded) {
                out.insert(id);
            }
        }
        LayoutNode::Split { children, .. } => {
            for c in children {
                stack_expanded_ids(c, out);
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
    fn flip_split_toggles_orientation_innermost_first() {
        // Split[ 1, Split_h[2,3] ]: flipping on 1 flips the outer (vertical),
        // flipping on 3 flips the inner (horizontal), leaving the other alone.
        let inner = LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratios: vec![0.5, 0.5],
            children: vec![LayoutNode::Pane(2), LayoutNode::Pane(3)],
        };
        let mut root = LayoutNode::Split {
            dir: SplitDir::Vertical,
            ratios: vec![0.5, 0.5],
            children: vec![LayoutNode::Pane(1), inner],
        };
        assert!(flip_split(&mut root, 1));
        match &root {
            LayoutNode::Split { dir, children, .. } => {
                assert_eq!(*dir, SplitDir::Horizontal); // outer flipped
                // inner untouched
                assert!(matches!(&children[1], LayoutNode::Split { dir: SplitDir::Horizontal, .. }));
            }
            _ => panic!(),
        }
        // flipping on a nested pane flips only the inner split
        assert!(flip_split(&mut root, 3));
        if let LayoutNode::Split { children, .. } = &root {
            assert!(matches!(&children[1], LayoutNode::Split { dir: SplitDir::Vertical, .. }));
        }
        // ratios preserved (2 children still 0.5/0.5)
        if let LayoutNode::Split { ratios, .. } = &root {
            assert_eq!(ratios.len(), 2);
        }
    }

    #[test]
    fn flip_split_noop_in_a_stack() {
        let mut node = LayoutNode::Stack { children: vec![1, 2], expanded: 0 };
        assert!(!flip_split(&mut node, 1));
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
        // height 20 for 3 members clears the C6 header threshold (>= 3+3=6),
        // so this scenario carries a header row too — see the dedicated
        // header tests below for the threshold boundary itself.
        let node = LayoutNode::Stack { children: vec![1, 2, 3], expanded: 1 };
        let mut out = vec![];
        let mut headers = vec![];
        compute_rects_and_headers(&node, Rect::new(0, 0, 80, 20), &mut out, &mut headers);
        assert_eq!(headers.len(), 1);
        assert_eq!(out.iter().filter(|p| p.collapsed).count(), 2);
        let expanded = out.iter().find(|p| !p.collapsed).unwrap();
        assert_eq!(expanded.rect.height, 17); // 20 − 1 header − 2 collapsed
        let total: u16 = out.iter().map(|p| p.rect.height).sum::<u16>() + headers[0].rect.height;
        assert_eq!(total, 20);
    }

    #[test]
    fn stack_header_shown_iff_height_at_least_n_plus_3() {
        let node = LayoutNode::Stack { children: vec![1, 2, 3], expanded: 0 }; // n=3, threshold=6
        let mut out = vec![];
        let mut headers = vec![];
        compute_rects_and_headers(&node, Rect::new(0, 0, 80, 5), &mut out, &mut headers);
        assert!(headers.is_empty(), "height 5 < n+3=6 → no header");
        // Below threshold, geometry is exactly today's (no-header) formula:
        // height − (n−1) = 5 − 2 = 3.
        assert_eq!(out.iter().find(|p| !p.collapsed).unwrap().rect.height, 3);

        out.clear();
        headers.clear();
        compute_rects_and_headers(&node, Rect::new(0, 0, 80, 6), &mut out, &mut headers);
        assert_eq!(headers.len(), 1, "height 6 == n+3 → header shown");
        assert_eq!(headers[0].rect, Rect::new(0, 0, 80, 1));
        assert_eq!(headers[0].n, 3);
    }

    #[test]
    fn stack_header_shrinks_expanded_height_by_exactly_one() {
        let n = 3u16;
        let area = Rect::new(0, 0, 80, 20); // comfortably above the n+3 threshold
        let pre_c6_height = area.height - (n - 1); // the no-header formula: 18
        let node = LayoutNode::Stack { children: vec![1, 2, 3], expanded: 1 };
        let mut out = vec![];
        let mut headers = vec![];
        compute_rects_and_headers(&node, area, &mut out, &mut headers);
        assert_eq!(headers.len(), 1);
        let expanded = out.iter().find(|p| !p.collapsed).unwrap();
        assert_eq!(expanded.rect.height, pre_c6_height - 1);
    }

    #[test]
    fn stack_header_row_belongs_to_no_pane() {
        let node = LayoutNode::Stack { children: vec![1, 2, 3], expanded: 1 };
        let area = Rect::new(0, 0, 80, 20);
        let mut out = vec![];
        let mut headers = vec![];
        compute_rects_and_headers(&node, area, &mut out, &mut headers);
        let header_row = headers[0].rect.y;
        for pr in &out {
            let covers_header_row = pr.rect.y <= header_row && header_row < pr.rect.y + pr.rect.height;
            assert!(!covers_header_row, "pane {} covers the header row", pr.id);
        }
    }

    #[test]
    fn stack_expanded_ids_flags_only_the_expanded_member() {
        let root = LayoutNode::Split {
            dir: SplitDir::Vertical,
            ratios: vec![0.5, 0.5],
            children: vec![
                LayoutNode::Stack { children: vec![1, 2, 3], expanded: 1 },
                LayoutNode::Pane(9),
            ],
        };
        let mut ids = HashSet::new();
        stack_expanded_ids(&root, &mut ids);
        assert_eq!(ids, HashSet::from([2u64]));
    }

    #[test]
    fn expand_in_stacks_expands_target() {
        let mut node = LayoutNode::Stack { children: vec![1, 2, 3], expanded: 0 };
        expand_in_stacks(&mut node, 3);
        assert!(matches!(node, LayoutNode::Stack { expanded: 2, .. }));
    }

    #[test]
    fn neighbor_moves_spatially() {
        // Four-quadrant layout:
        //   1 | 2
        //   -----
        //   3 | 4
        let rects = vec![
            PaneRect { id: 1, rect: Rect::new(0, 0, 50, 12), collapsed: false },
            PaneRect { id: 2, rect: Rect::new(50, 0, 50, 12), collapsed: false },
            PaneRect { id: 3, rect: Rect::new(0, 12, 50, 12), collapsed: false },
            PaneRect { id: 4, rect: Rect::new(50, 12, 50, 12), collapsed: false },
        ];
        assert_eq!(neighbor(&rects, 1, Dir::Right), Some(2));
        assert_eq!(neighbor(&rects, 1, Dir::Down), Some(3));
        assert_eq!(neighbor(&rects, 4, Dir::Left), Some(3));
        assert_eq!(neighbor(&rects, 4, Dir::Up), Some(2));
        // Nothing to the left of pane 1 → stay put.
        assert_eq!(neighbor(&rects, 1, Dir::Left), None);
        assert_eq!(neighbor(&rects, 2, Dir::Up), None);
    }

    #[test]
    fn stack_header_shown_iff_height_at_least_n_plus_3_for_smallest_stack() {
        // n=2 is the smallest real stack (1 collapsed row) — a different
        // arithmetic regime from the n=3 case above; pin the threshold here
        // too. Threshold = n+3 = 5.
        let node = LayoutNode::Stack { children: vec![1, 2], expanded: 0 };
        let mut out = vec![];
        let mut headers = vec![];
        compute_rects_and_headers(&node, Rect::new(0, 0, 80, 4), &mut out, &mut headers);
        assert!(headers.is_empty(), "height 4 (n+2) < n+3=5 → no header");
        assert_eq!(out.iter().find(|p| !p.collapsed).unwrap().rect.height, 3);

        out.clear();
        headers.clear();
        compute_rects_and_headers(&node, Rect::new(0, 0, 80, 5), &mut out, &mut headers);
        assert_eq!(headers.len(), 1, "height 5 (n+3) → header shown");
        assert_eq!(headers[0].n, 2);
        assert_eq!(out.iter().find(|p| !p.collapsed).unwrap().rect.height, 3);
    }

    #[test]
    fn compute_rects_on_zero_area_yields_nothing() {
        // Degenerate terminal size (zero width or height): must not panic,
        // and must not fabricate rects/headers out of no area.
        let node = LayoutNode::Stack { children: vec![1, 2, 3], expanded: 0 };
        let mut out = vec![];
        let mut headers = vec![];
        compute_rects_and_headers(&node, Rect::new(0, 0, 0, 20), &mut out, &mut headers);
        assert!(out.is_empty());
        assert!(headers.is_empty());

        compute_rects_and_headers(&node, Rect::new(0, 0, 80, 0), &mut out, &mut headers);
        assert!(out.is_empty());
        assert!(headers.is_empty());
    }
}
