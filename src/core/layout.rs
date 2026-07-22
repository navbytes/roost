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

// ---------------------------------------------------------------------------
// Canned layouts (C25)
// ---------------------------------------------------------------------------
//
// Pure builders — no App/UI wiring here. Each takes `pane_order()` of the
// tab's current tree (`P` in the contract) and produces a brand-new tree;
// callers own applying it and advancing the cycle counter.

fn even_ratios(n: usize) -> Vec<f32> {
    vec![1.0 / n as f32; n]
}

/// One row of an even-grid: a bare `Pane` if it holds a single pane (this
/// file never builds a one-child `Split` — see `remove_pane`'s collapse
/// step), otherwise a `Split{Vertical}` of the row's panes side by side.
fn grid_row(panes: &[PaneId]) -> LayoutNode {
    match panes {
        [id] => LayoutNode::Pane(*id),
        _ => LayoutNode::Split {
            dir: SplitDir::Vertical,
            ratios: even_ratios(panes.len()),
            children: panes.iter().map(|&id| LayoutNode::Pane(id)).collect(),
        },
    }
}

/// C25 even-grid (Alt+g, arrangement 1/3): `c = ceil(sqrt(n))` columns,
/// `r = ceil(n / c)` rows, `panes` assigned row-major — so `pane_order()` of
/// the result is `panes` verbatim. A grid of one row collapses to that
/// row's `Split` directly (no pointless outer wrapper); `n = 1` collapses
/// all the way to a bare `Pane`. Worked shapes: n=2 side-by-side, n=3
/// two-over-one, n=4 2×2, n=5 three-over-two, n=7 three/three/one.
#[allow(dead_code)] // F1: pure builder, exercised by the tests here; wired onto Alt+g by F3 (C25)
pub fn grid_layout(panes: &[PaneId]) -> LayoutNode {
    match panes {
        [] => LayoutNode::Stack { children: Vec::new(), expanded: 0 },
        [id] => LayoutNode::Pane(*id),
        _ => {
            let n = panes.len();
            let cols = (1..=n).find(|c| c * c >= n).unwrap_or(1);
            let mut rows: Vec<LayoutNode> = panes.chunks(cols).map(grid_row).collect();
            if rows.len() == 1 {
                rows.remove(0)
            } else {
                LayoutNode::Split {
                    dir: SplitDir::Horizontal,
                    ratios: even_ratios(rows.len()),
                    children: rows,
                }
            }
        }
    }
}

/// C25 main+stack (arrangement 2/3): `focused` fills the left 0.6 column;
/// `rest` — `pane_order()` of the prior tree with `focused` already
/// excluded — fills the right 0.4 column, so `pane_order()` of the result
/// is `focused` followed by `rest`. Two panes total (`rest.len() == 1`) is
/// a plain 0.6/0.4 split, never a one-member `Stack`; zero panes in `rest`
/// (`n = 1`) collapses to a bare `Pane(focused)`.
#[allow(dead_code)] // F1: pure builder, exercised by the tests here; wired onto Alt+g by F3 (C25)
pub fn main_stack_layout(focused: PaneId, rest: &[PaneId]) -> LayoutNode {
    match rest {
        [] => LayoutNode::Pane(focused),
        [id] => LayoutNode::Split {
            dir: SplitDir::Vertical,
            ratios: vec![0.6, 0.4],
            children: vec![LayoutNode::Pane(focused), LayoutNode::Pane(*id)],
        },
        _ => LayoutNode::Split {
            dir: SplitDir::Vertical,
            ratios: vec![0.6, 0.4],
            children: vec![
                LayoutNode::Pane(focused),
                LayoutNode::Stack { children: rest.to_vec(), expanded: 0 },
            ],
        },
    }
}

/// C25 all-stack (arrangement 3/3): every pane in `panes` (`pane_order()`
/// of the prior tree, order preserved verbatim — unlike main+stack, nothing
/// moves to the front) joins one `Stack`, expanded on `focused`'s position.
/// A single pane collapses to a bare `Pane` — the same "no one-member
/// stack" rule the main+stack `n = 2` case states explicitly.
#[allow(dead_code)] // F1: pure builder, exercised by the tests here; wired onto Alt+g by F3 (C25)
pub fn all_stack_layout(panes: &[PaneId], focused: PaneId) -> LayoutNode {
    match panes {
        [] => LayoutNode::Stack { children: Vec::new(), expanded: 0 },
        [id] => LayoutNode::Pane(*id),
        _ => {
            let expanded = panes.iter().position(|&id| id == focused).unwrap_or(0);
            LayoutNode::Stack { children: panes.to_vec(), expanded }
        }
    }
}

/// Mirrors `MIN_SPLIT_COLS`/`MIN_SPLIT_ROWS` in `app.rs` — the same split
/// floors, applied here to the C25 fit predicate. `layout.rs` has no
/// dependency on `app.rs` to import them from; kept in sync by hand.
const MIN_SPLIT_COLS: u16 = 36;
const MIN_SPLIT_ROWS: u16 = 10;

/// C25 fit predicate: true iff every non-collapsed rect the arrangement
/// would produce in `area` is at least `MIN_SPLIT_COLS` × `MIN_SPLIT_ROWS` —
/// reuses `compute_rects`, so it can't drift from the real geometry walk.
/// Collapsed stack rows (1-row title bars) are exempt by design.
#[allow(dead_code)] // F1: pure predicate, exercised by the tests here; wired onto Alt+g by F3 (C25)
pub fn arrangement_fits(node: &LayoutNode, area: Rect) -> bool {
    let mut rects = Vec::new();
    compute_rects(node, area, &mut rects);
    rects
        .iter()
        .all(|pr| pr.collapsed || (pr.rect.width >= MIN_SPLIT_COLS && pr.rect.height >= MIN_SPLIT_ROWS))
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

    // -- C25 canned layouts --------------------------------------------------

    /// Pane count one level down: a bare `Pane` counts as 1, a `Split`/`Stack`
    /// counts its direct children — i.e. a grid row's width.
    fn row_pane_count(node: &LayoutNode) -> usize {
        match node {
            LayoutNode::Pane(_) => 1,
            LayoutNode::Split { children, .. } => children.len(),
            LayoutNode::Stack { children, .. } => children.len(),
        }
    }

    #[test]
    fn grid_layout_n1_is_bare_pane() {
        assert!(matches!(grid_layout(&[1]), LayoutNode::Pane(1)));
    }

    #[test]
    fn grid_layout_n2_is_side_by_side() {
        let node = grid_layout(&[1, 2]);
        match &node {
            LayoutNode::Split { dir: SplitDir::Vertical, ratios, children } => {
                assert_eq!(ratios, &vec![0.5, 0.5]);
                assert_eq!(children.len(), 2);
            }
            other => panic!("expected side-by-side split, got {other:?}"),
        }
        let mut order = vec![];
        pane_order(&node, &mut order);
        assert_eq!(order, vec![1, 2]);
    }

    #[test]
    fn grid_layout_n3_is_two_over_one() {
        let node = grid_layout(&[1, 2, 3]);
        let LayoutNode::Split { dir: SplitDir::Horizontal, children: rows, .. } = &node else {
            panic!("expected an outer horizontal split, got {node:?}");
        };
        assert_eq!(rows.iter().map(row_pane_count).collect::<Vec<_>>(), vec![2, 1]);
        assert!(matches!(&rows[0], LayoutNode::Split { dir: SplitDir::Vertical, .. }));
        assert!(matches!(&rows[1], LayoutNode::Pane(3)));
    }

    #[test]
    fn grid_layout_n4_is_two_by_two() {
        let node = grid_layout(&[1, 2, 3, 4]);
        let LayoutNode::Split { dir: SplitDir::Horizontal, children: rows, .. } = &node else {
            panic!("expected an outer horizontal split, got {node:?}");
        };
        assert_eq!(rows.iter().map(row_pane_count).collect::<Vec<_>>(), vec![2, 2]);
        for row in rows {
            assert!(matches!(row, LayoutNode::Split { dir: SplitDir::Vertical, .. }));
        }
    }

    #[test]
    fn grid_layout_n5_is_three_over_two() {
        let node = grid_layout(&[1, 2, 3, 4, 5]);
        let LayoutNode::Split { dir: SplitDir::Horizontal, children: rows, .. } = &node else {
            panic!("expected an outer horizontal split, got {node:?}");
        };
        assert_eq!(rows.iter().map(row_pane_count).collect::<Vec<_>>(), vec![3, 2]);
    }

    #[test]
    fn grid_layout_n7_is_three_three_one() {
        let node = grid_layout(&[1, 2, 3, 4, 5, 6, 7]);
        let LayoutNode::Split { dir: SplitDir::Horizontal, children: rows, .. } = &node else {
            panic!("expected an outer horizontal split, got {node:?}");
        };
        assert_eq!(rows.iter().map(row_pane_count).collect::<Vec<_>>(), vec![3, 3, 1]);
        assert!(matches!(&rows[2], LayoutNode::Pane(7)));
    }

    #[test]
    fn grid_layout_preserves_pane_order() {
        for n in [1usize, 2, 3, 4, 5, 7] {
            let panes: Vec<PaneId> = (1..=n as PaneId).collect();
            let mut order = vec![];
            pane_order(&grid_layout(&panes), &mut order);
            assert_eq!(order, panes, "n={n}");
        }
    }

    #[test]
    fn main_stack_layout_n1_is_bare_pane() {
        assert!(matches!(main_stack_layout(1, &[]), LayoutNode::Pane(1)));
    }

    #[test]
    fn main_stack_layout_n2_is_plain_split_no_stack() {
        match main_stack_layout(1, &[2]) {
            LayoutNode::Split { dir: SplitDir::Vertical, ratios, children } => {
                assert_eq!(ratios, vec![0.6, 0.4]);
                assert!(matches!(&children[0], LayoutNode::Pane(1)));
                assert!(matches!(&children[1], LayoutNode::Pane(2)), "must not be a one-member stack");
            }
            other => panic!("expected a plain 0.6/0.4 split, got {other:?}"),
        }
    }

    #[test]
    fn main_stack_layout_stacks_the_rest_from_three_panes_up() {
        match main_stack_layout(1, &[2, 3, 4]) {
            LayoutNode::Split { dir: SplitDir::Vertical, ratios, children } => {
                assert_eq!(ratios, vec![0.6, 0.4]);
                assert!(matches!(&children[0], LayoutNode::Pane(1)));
                match &children[1] {
                    LayoutNode::Stack { children, expanded } => {
                        assert_eq!(children, &vec![2, 3, 4]);
                        assert_eq!(*expanded, 0);
                    }
                    other => panic!("expected a stack, got {other:?}"),
                }
            }
            other => panic!("expected a split, got {other:?}"),
        }
    }

    #[test]
    fn main_stack_layout_puts_focused_first_in_pane_order() {
        for rest_len in [0usize, 1, 2, 3, 4, 6] {
            let rest: Vec<PaneId> = (2..2 + rest_len as PaneId).collect();
            let mut order = vec![];
            pane_order(&main_stack_layout(1, &rest), &mut order);
            let mut expected = vec![1];
            expected.extend(&rest);
            assert_eq!(order, expected, "rest_len={rest_len}");
        }
    }

    #[test]
    fn all_stack_layout_n1_is_bare_pane() {
        assert!(matches!(all_stack_layout(&[1], 1), LayoutNode::Pane(1)));
    }

    #[test]
    fn all_stack_layout_expands_the_focused_member() {
        match all_stack_layout(&[10, 20, 30], 20) {
            LayoutNode::Stack { children, expanded } => {
                assert_eq!(children, vec![10, 20, 30]);
                assert_eq!(expanded, 1);
            }
            other => panic!("expected a stack, got {other:?}"),
        }
    }

    #[test]
    fn all_stack_layout_preserves_pane_order() {
        for n in [1usize, 2, 3, 4, 5, 7] {
            let panes: Vec<PaneId> = (1..=n as PaneId).collect();
            let mut order = vec![];
            pane_order(&all_stack_layout(&panes, 1), &mut order);
            assert_eq!(order, panes, "n={n}");
        }
    }

    #[test]
    fn arrangement_fits_true_at_exact_boundary_36x10() {
        let node = grid_layout(&[1]); // bare Pane — rect == area, no rounding to reason about
        assert!(arrangement_fits(&node, Rect::new(0, 0, 36, 10)));
    }

    #[test]
    fn arrangement_fits_false_just_under_the_boundary() {
        let node = grid_layout(&[1]);
        assert!(!arrangement_fits(&node, Rect::new(0, 0, 35, 10)));
        assert!(!arrangement_fits(&node, Rect::new(0, 0, 36, 9)));
    }

    #[test]
    fn arrangement_fits_false_when_a_grid_column_is_too_narrow() {
        // 2x2 grid in 40 cols: each column gets 20 < 36.
        let node = grid_layout(&[1, 2, 3, 4]);
        assert!(!arrangement_fits(&node, Rect::new(0, 0, 40, 20)));
    }

    #[test]
    fn arrangement_fits_exempts_collapsed_stack_rows() {
        // all-stack of 5: collapsed members are 1 row tall (under the
        // 10-row floor) but exempt; only the expanded member (15 rows
        // here) has to clear the floor.
        let node = all_stack_layout(&[1, 2, 3, 4, 5], 1);
        assert!(arrangement_fits(&node, Rect::new(0, 0, 40, 20)));
    }
}
