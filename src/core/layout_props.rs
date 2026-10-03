//! Property tests for the layout tree operations.
//!
//! `layout.rs`'s example tests pin the cases someone thought of. These pin
//! what the operations promise for *every* tree: that the tree they leave
//! behind is still one the rest of the program accepts (no one-child split,
//! no stack of one, ratios that still add up, an `expanded` that points at a
//! member), and that each operation does exactly what its doc says to the
//! set and order of panes.
//!
//! "A legal tree" is the contract the renderer, the focus code and
//! `workspace.json` loading all lean on; `check_legal` below is that
//! contract written once. Every property that mutates a tree ends by
//! running it.

use std::collections::HashSet;

use proptest::prelude::*;

use super::layout::{
    dedupe_pane_ids, explode_stack, flip_split, pane_order, remove_pane, resize_pane, split_pane,
    stack_pane, swap_panes, LayoutNode, PaneId, SplitDir,
};

// ---- the contract ----------------------------------------------------------

/// Why `node` is not a tree the rest of roost can be handed, or `Ok`.
fn check_legal(node: &LayoutNode) -> Result<(), String> {
    fn walk(node: &LayoutNode) -> Result<(), String> {
        match node {
            LayoutNode::Pane(_) => Ok(()),
            LayoutNode::Stack { children, expanded, .. } => {
                if children.len() < 2 {
                    return Err(format!("a stack of {} is a pane", children.len()));
                }
                if *expanded >= children.len() {
                    return Err(format!("expanded {expanded} of {}", children.len()));
                }
                Ok(())
            }
            LayoutNode::Split { ratios, children, .. } => {
                if children.len() < 2 {
                    return Err(format!("a split of {} collapses", children.len()));
                }
                if ratios.len() != children.len() {
                    return Err(format!("{} ratios for {} children", ratios.len(), children.len()));
                }
                if ratios.iter().any(|r| !r.is_finite() || *r <= 0.0) {
                    return Err(format!("non-positive ratio in {ratios:?}"));
                }
                let sum: f32 = ratios.iter().sum();
                if (sum - 1.0).abs() > 1e-3 {
                    return Err(format!("ratios {ratios:?} sum to {sum}"));
                }
                children.iter().try_for_each(walk)
            }
        }
    }
    walk(node)?;
    let ids = ids(node);
    let unique: HashSet<_> = ids.iter().collect();
    if unique.len() != ids.len() {
        return Err(format!("a pane id appears twice in {ids:?}"));
    }
    Ok(())
}

fn ids(node: &LayoutNode) -> Vec<PaneId> {
    let mut out = Vec::new();
    pane_order(node, &mut out);
    out
}

fn sorted(mut v: Vec<PaneId>) -> Vec<PaneId> {
    v.sort_unstable();
    v
}

fn split_count(node: &LayoutNode) -> usize {
    match node {
        LayoutNode::Split { children, .. } => 1 + children.iter().map(split_count).sum::<usize>(),
        _ => 0,
    }
}

/// Same tree, ratios compared within float noise (`remove_pane` renormalizes).
fn same_tree(a: &LayoutNode, b: &LayoutNode) -> bool {
    match (a, b) {
        (LayoutNode::Pane(x), LayoutNode::Pane(y)) => x == y,
        (
            LayoutNode::Stack { children: ca, expanded: ea, .. },
            LayoutNode::Stack { children: cb, expanded: eb, .. },
        ) => ca == cb && ea == eb,
        (
            LayoutNode::Split { dir: da, ratios: ra, children: ca },
            LayoutNode::Split { dir: db, ratios: rb, children: cb },
        ) => {
            da == db
                && ra.len() == rb.len()
                && ra.iter().zip(rb).all(|(x, y)| (x - y).abs() < 1e-4)
                && ca.len() == cb.len()
                && ca.iter().zip(cb).all(|(x, y)| same_tree(x, y))
        }
        _ => false,
    }
}

/// The stack holding `id`: its members and the id of its expanded member.
fn stack_of(node: &LayoutNode, id: PaneId) -> Option<(Vec<PaneId>, PaneId)> {
    match node {
        LayoutNode::Pane(_) => None,
        LayoutNode::Stack { children, expanded, .. } => {
            children.contains(&id).then(|| (children.clone(), children[*expanded]))
        }
        LayoutNode::Split { children, .. } => children.iter().find_map(|c| stack_of(c, id)),
    }
}

/// Every stack in the tree as `(members, expanded member)`.
fn stacks(node: &LayoutNode, out: &mut Vec<(Vec<PaneId>, PaneId)>) {
    match node {
        LayoutNode::Pane(_) => {}
        LayoutNode::Stack { children, expanded, .. } => {
            out.push((children.clone(), children[*expanded]))
        }
        LayoutNode::Split { children, .. } => children.iter().for_each(|c| stacks(c, out)),
    }
}

// ---- generators -------------------------------------------------------------

/// A tree's shape before it has pane ids, so ids can be handed out afterwards
/// (unique for the legal-tree properties, repeated for the dedupe one).
#[derive(Debug, Clone)]
enum Shape {
    Pane,
    Stack { n: usize, expanded: usize },
    Split { dir: SplitDir, kids: Vec<(Shape, u32)> },
}

fn shape() -> impl Strategy<Value = Shape> {
    let leaf = prop_oneof![
        3 => Just(Shape::Pane),
        1 => (2usize..5).prop_flat_map(|n| (Just(n), 0..n))
            .prop_map(|(n, expanded)| Shape::Stack { n, expanded }),
    ];
    leaf.prop_recursive(3, 14, 3, |inner| {
        (any::<bool>(), prop::collection::vec((inner, 1u32..10), 2..4)).prop_map(|(v, kids)| {
            let dir = if v { SplitDir::Vertical } else { SplitDir::Horizontal };
            Shape::Split { dir, kids }
        })
    })
}

fn build(shape: &Shape, next: &mut impl FnMut() -> PaneId) -> LayoutNode {
    match shape {
        Shape::Pane => LayoutNode::Pane(next()),
        Shape::Stack { n, expanded } => LayoutNode::Stack {
            children: (0..*n).map(|_| next()).collect(),
            expanded: *expanded,
            from: None,
        },
        Shape::Split { dir, kids } => {
            let total: u32 = kids.iter().map(|(_, w)| w).sum();
            LayoutNode::Split {
                dir: *dir,
                ratios: kids.iter().map(|(_, w)| *w as f32 / total as f32).collect(),
                children: kids.iter().map(|(s, _)| build(s, next)).collect(),
            }
        }
    }
}

/// A legal tree with unique ids `0..n`.
fn tree() -> impl Strategy<Value = LayoutNode> {
    shape().prop_map(|s| {
        let mut n = 0;
        build(&s, &mut || {
            n += 1;
            n - 1
        })
    })
}

/// A legal tree plus an id that is in it.
fn tree_and_member() -> impl Strategy<Value = (LayoutNode, PaneId)> {
    tree().prop_flat_map(|t| {
        let n = ids(&t).len();
        (Just(t), 0..n as PaneId)
    })
}

fn dir() -> impl Strategy<Value = SplitDir> {
    prop_oneof![Just(SplitDir::Vertical), Just(SplitDir::Horizontal)]
}

// ---- properties -------------------------------------------------------------

proptest! {
    #[test]
    fn generated_trees_are_legal(t in tree()) {
        prop_assert_eq!(check_legal(&t), Ok(()));
    }

    // remove_pane ------------------------------------------------------------

    /// Closing a pane drops exactly that pane, keeps the others in order, and
    /// leaves a legal tree — which, for a stack that shrinks to one, means a
    /// bare pane again.
    #[test]
    fn remove_drops_exactly_the_target_and_leaves_a_legal_tree(
        (mut t, target) in tree_and_member()
    ) {
        let before = ids(&t);
        prop_assume!(before.len() > 1);
        let emptied = remove_pane(&mut t, target);
        prop_assert!(!emptied, "a tree with other panes is not empty");
        prop_assert_eq!(check_legal(&t), Ok(()));
        let want: Vec<_> = before.into_iter().filter(|p| *p != target).collect();
        prop_assert_eq!(ids(&t), want);
    }

    /// Closing a pane never moves the *other* panes' expanded slots: a stack
    /// that did not lose its expanded member still shows the same pane.
    #[test]
    fn remove_keeps_every_other_stacks_expanded_pane((mut t, target) in tree_and_member()) {
        prop_assume!(ids(&t).len() > 1);
        let mut before = Vec::new();
        stacks(&t, &mut before);
        remove_pane(&mut t, target);
        let mut after = Vec::new();
        stacks(&t, &mut after);
        for (members, expanded) in &after {
            // Stacks only shrink, so this one was a stack before; find it by
            // any surviving member.
            let was = before.iter().find(|(m, _)| m.contains(&members[0])).unwrap();
            prop_assert!(
                was.1 == *expanded || was.1 == target,
                "stack {members:?} now shows {expanded}, it showed {} (closed: {target})",
                was.1
            );
        }
    }

    #[test]
    fn remove_of_an_absent_id_changes_nothing(t in tree()) {
        let mut u = t.clone();
        let absent = ids(&t).len() as PaneId + 100;
        let emptied = remove_pane(&mut u, absent);
        prop_assert!(!emptied);
        prop_assert!(same_tree(&t, &u));
    }

    // split_pane -------------------------------------------------------------

    /// Splitting adds exactly one pane. Beside a bare pane it lands directly
    /// after it; inside a stack it joins that stack, expanded.
    #[test]
    fn split_adds_one_pane_where_its_doc_says(
        (mut t, target) in tree_and_member(),
        d in dir(),
    ) {
        let new = ids(&t).len() as PaneId + 100;
        let stacked_before = stack_of(&t, target);
        let before = ids(&t);
        prop_assert!(split_pane(&mut t, target, new, d));
        prop_assert_eq!(check_legal(&t), Ok(()));
        let mut want = before.clone();
        match stacked_before {
            Some(_) => {
                let (members, expanded) = stack_of(&t, target).unwrap();
                prop_assert_eq!(members.last(), Some(&new), "joins the end of the stack");
                prop_assert_eq!(expanded, new, "and is the one shown");
                prop_assert_eq!(sorted(ids(&t)), sorted([before, vec![new]].concat()));
            }
            None => {
                let at = want.iter().position(|p| *p == target).unwrap();
                want.insert(at + 1, new);
                prop_assert_eq!(ids(&t), want);
            }
        }
    }

    #[test]
    fn split_of_an_absent_id_changes_nothing(t in tree(), d in dir()) {
        let mut u = t.clone();
        let absent = ids(&t).len() as PaneId + 100;
        prop_assert!(!split_pane(&mut u, absent, absent + 1, d));
        prop_assert!(same_tree(&t, &u));
    }

    /// Open a pane then close it: same panes in the same order, still legal.
    /// (Equality of the whole tree is not promised — closing a pane out of a
    /// stack that was a bare pane's split sibling renormalizes ratios.)
    #[test]
    fn split_then_remove_gets_the_panes_back((mut t, target) in tree_and_member(), d in dir()) {
        let before = ids(&t);
        let new = before.len() as PaneId + 100;
        prop_assert!(split_pane(&mut t, target, new, d));
        remove_pane(&mut t, new);
        prop_assert_eq!(check_legal(&t), Ok(()));
        prop_assert_eq!(ids(&t), before);
    }

    // stack_pane / explode_stack ----------------------------------------------

    /// Alt+s climbs one enclosing split per press until the whole tab is one
    /// stack, and then — only then — refuses. Every rung is legal, keeps the
    /// same panes, and shows the target.
    #[test]
    fn stacking_climbs_to_a_single_stack_then_refuses((mut t, target) in tree_and_member()) {
        let all = sorted(ids(&t));
        let splits = split_count(&t);
        let mut presses = 0;
        while stack_pane(&mut t, target) {
            presses += 1;
            prop_assert!(presses <= splits, "more presses than splits to absorb");
            prop_assert_eq!(check_legal(&t), Ok(()));
            prop_assert_eq!(sorted(ids(&t)), all.clone());
            let (_, shown) = stack_of(&t, target).expect("the target is in a stack after a press");
            prop_assert_eq!(shown, target, "the pane Alt+s was pressed on stays the one shown");
        }
        // At the ceiling: nothing above to absorb.
        match &t {
            LayoutNode::Pane(_) => prop_assert_eq!(all.len(), 1),
            LayoutNode::Stack { .. } => {
                prop_assert_eq!(sorted(stack_of(&t, target).unwrap().0), all);
            }
            LayoutNode::Split { .. } => prop_assert!(false, "refused with a split still above"),
        }
    }

    /// A refused stack_pane leaves the tree exactly as it was.
    #[test]
    fn a_refused_stack_changes_nothing((t, target) in tree_and_member()) {
        let mut u = t.clone();
        if !stack_pane(&mut u, target) {
            prop_assert!(same_tree(&t, &u));
        }
    }

    /// C42: Alt+s then Alt+Shift+s on a split of bare panes puts back the same
    /// direction and the same widths — it used to rotate the split and reset
    /// the ratios.
    #[test]
    fn stack_then_explode_restores_a_flat_split(
        d in dir(),
        weights in prop::collection::vec(1u32..10, 2..6),
        pick in any::<prop::sample::Index>(),
    ) {
        let total: u32 = weights.iter().sum();
        let n = weights.len();
        let original = LayoutNode::Split {
            dir: d,
            ratios: weights.iter().map(|w| *w as f32 / total as f32).collect(),
            children: (0..n as PaneId).map(LayoutNode::Pane).collect(),
        };
        let target = pick.index(n) as PaneId;
        let mut t = original.clone();
        prop_assert!(stack_pane(&mut t, target));
        prop_assert!(explode_stack(&mut t, target));
        prop_assert_eq!(check_legal(&t), Ok(()));
        prop_assert!(same_tree(&original, &t), "{original:?} became {t:?}");
    }

    /// Exploding always yields a legal split of the stack's own members, and
    /// refuses (changing nothing) for a pane that is not stacked.
    #[test]
    fn explode_splits_the_stack_into_its_members((t, target) in tree_and_member()) {
        let mut u = t.clone();
        match stack_of(&t, target) {
            Some((members, _)) => {
                prop_assert!(explode_stack(&mut u, target));
                prop_assert_eq!(check_legal(&u), Ok(()));
                prop_assert_eq!(sorted(ids(&u)), sorted(ids(&t)));
                // The members are now adjacent bare panes in a split, in order.
                let all = ids(&u);
                let at = all.iter().position(|p| *p == members[0]).unwrap();
                prop_assert_eq!(&all[at..at + members.len()], members.as_slice());
            }
            None => {
                prop_assert!(!explode_stack(&mut u, target));
                prop_assert!(same_tree(&t, &u));
            }
        }
    }

    // flip_split ------------------------------------------------------------------

    /// Flipping twice is the identity; flipping changes only a direction.
    #[test]
    fn flip_twice_is_the_identity((t, target) in tree_and_member()) {
        let mut u = t.clone();
        if flip_split(&mut u, target) {
            prop_assert!(!same_tree(&t, &u) || split_count(&t) == 0);
            prop_assert_eq!(check_legal(&u), Ok(()));
            prop_assert_eq!(ids(&u), ids(&t));
            prop_assert!(flip_split(&mut u, target));
            prop_assert!(same_tree(&t, &u));
        } else {
            prop_assert!(same_tree(&t, &u));
        }
    }

    // resize_pane -----------------------------------------------------------------

    /// Resizing moves share between two siblings and nothing else: still
    /// legal, same panes, ratios still add up, and a ratio that was at least
    /// the 0.1 floor is still at least the floor.
    #[test]
    fn resize_keeps_ratios_summing_to_one_and_above_the_floor(
        (mut t, target) in tree_and_member(),
        axis in dir(),
        delta in -1.0f32..1.0,
    ) {
        fn min_ratio(n: &LayoutNode) -> f32 {
            match n {
                LayoutNode::Split { ratios, children, .. } => ratios
                    .iter()
                    .copied()
                    .chain(children.iter().map(min_ratio))
                    .fold(f32::MAX, f32::min),
                _ => f32::MAX,
            }
        }
        let before = t.clone();
        let floor_held = min_ratio(&t) >= 0.1;
        resize_pane(&mut t, target, axis, delta);
        prop_assert_eq!(check_legal(&t), Ok(()));
        prop_assert_eq!(ids(&t), ids(&before));
        if floor_held {
            prop_assert!(min_ratio(&t) >= 0.1 - 1e-4, "{} after resizing {before:?}", min_ratio(&t));
        }
    }

    // swap_panes ------------------------------------------------------------------

    /// A swap exchanges two ids and changes nothing about the tree's shape;
    /// swapping the same pair back is the identity.
    #[test]
    fn swap_exchanges_two_panes_and_is_its_own_inverse(
        (t, a) in tree_and_member(),
        pick in any::<prop::sample::Index>(),
    ) {
        let all = ids(&t);
        let b = all[pick.index(all.len())];
        let mut u = t.clone();
        if a == b {
            prop_assert!(!swap_panes(&mut u, a, b));
            prop_assert!(same_tree(&t, &u));
            return Ok(());
        }
        prop_assert!(swap_panes(&mut u, a, b));
        prop_assert_eq!(check_legal(&u), Ok(()));
        // Same multiset, and `a` and `b` have traded slots.
        prop_assert_eq!(sorted(ids(&u)), sorted(all.clone()));
        let (ia, ib) = (
            all.iter().position(|p| *p == a).unwrap(),
            all.iter().position(|p| *p == b).unwrap(),
        );
        let mut want = all;
        want.swap(ia, ib);
        prop_assert_eq!(ids(&u), want);
        // Within one stack, the pane that was shown is still the one shown.
        if let (Some((ma, ea)), Some((mb, _))) = (stack_of(&t, a), stack_of(&t, b)) {
            if ma == mb {
                prop_assert_eq!(stack_of(&u, a).unwrap().1, ea, "expanded pane follows the swap");
            }
        }
        prop_assert!(swap_panes(&mut u, a, b));
        prop_assert!(same_tree(&t, &u));
    }

    #[test]
    fn swap_with_an_absent_id_changes_nothing((t, a) in tree_and_member()) {
        let mut u = t.clone();
        prop_assert!(!swap_panes(&mut u, a, ids(&t).len() as PaneId + 100));
        prop_assert!(same_tree(&t, &u));
    }

    // dedupe_pane_ids ----------------------------------------------------------------

    /// A corrupt workspace.json can repeat a pane id (one PTY driven from two
    /// slots). Dedupe keeps the first of each in DFS order and drops the rest,
    /// pruning whatever that empties, and what remains is legal.
    #[test]
    fn dedupe_keeps_first_occurrences_in_order(
        s in shape(),
        pool in 1u64..6,
        salt in any::<u64>(),
    ) {
        // Draw ids from a small pool so repeats are the norm, not the exception.
        let mut state = salt;
        let mut t = build(&s, &mut || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) % pool
        });
        let before = ids(&t);
        let mut seen = HashSet::new();
        let emptied = dedupe_pane_ids(&mut t, &mut seen);
        prop_assert!(!emptied, "an empty `seen` keeps at least the first pane");
        let mut once = HashSet::new();
        let want: Vec<_> = before.into_iter().filter(|p| once.insert(*p)).collect();
        prop_assert_eq!(ids(&t), want);
        // `check_legal` also demands unique ids and no one-child split/stack.
        // The generator can produce a stack whose members repeat before the
        // pass; after it they are unique, so it must hold.
        prop_assert_eq!(check_legal(&t), Ok(()));
    }

    /// `seen` is shared across tabs: ids already claimed elsewhere are
    /// stripped, and a tree left with nothing says so.
    #[test]
    fn dedupe_strips_ids_another_tab_already_owns(t in tree()) {
        let all = ids(&t);
        let mut u = t.clone();
        let mut seen: HashSet<PaneId> = all.iter().copied().collect();
        prop_assert!(dedupe_pane_ids(&mut u, &mut seen), "every pane was already claimed");
        // And claiming nothing leaves a tree unchanged.
        let mut v = t.clone();
        prop_assert!(!dedupe_pane_ids(&mut v, &mut HashSet::new()));
        prop_assert!(same_tree(&t, &v));
    }
}
