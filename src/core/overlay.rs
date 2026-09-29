//! The two panes painted over the rest of the view, and the rules that
//! concern only them: the C22 scratch float (session-only, hides on focus
//! loss, process kept) and the C22a `spawn --float` popup (always shown,
//! dismissal = close). Both live outside every tab's layout tree; their
//! runtimes sit in `App::runtimes` like any pane's. Anything that must move
//! focus, kill a process or read the workspace stays in `App`.

use crate::core::layout::PaneId;
use crate::core::workspace::PaneSpec;

#[derive(Debug)]
struct Float {
    id: PaneId,
    spec: PaneSpec,
    shown: bool,
    /// Where focus returns when it goes away (C22 rules 2/3).
    prev_focus: PaneId,
}

#[derive(Debug, Default)]
pub struct Overlays {
    float: Option<Float>,
    /// Never hidden, only closed; never open together with a *shown* float.
    popup: Option<Float>,
    /// Set only while `ctl_spawn_child` moves focus around a control
    /// split-spawn, so that transient focus move does not dismiss the popup.
    pinned: bool,
}

impl Overlays {
    pub fn float_id(&self) -> Option<PaneId> {
        self.float.as_ref().map(|f| f.id)
    }

    pub fn popup_id(&self) -> Option<PaneId> {
        self.popup.as_ref().map(|f| f.id)
    }

    pub fn float_shown(&self) -> bool {
        self.float.as_ref().is_some_and(|f| f.shown)
    }

    #[cfg(test)]
    pub fn float_hidden(&self) -> bool {
        self.float.as_ref().is_some_and(|f| !f.shown)
    }

    pub fn is_float(&self, id: PaneId) -> bool {
        self.float_id() == Some(id)
    }

    pub fn is_popup(&self, id: PaneId) -> bool {
        self.popup_id() == Some(id)
    }

    pub fn is_overlay(&self, id: PaneId) -> bool {
        self.is_float(id) || self.is_popup(id)
    }

    fn top(&self) -> Option<&Float> {
        self.popup.iter().chain(self.float.iter().filter(|f| f.shown)).next()
    }

    /// The pane currently painted over the rest, popup first.
    pub fn top_id(&self) -> Option<PaneId> {
        self.top().map(|f| f.id)
    }

    /// Where focus returns when the shown overlay goes away.
    pub fn prev(&self) -> Option<PaneId> {
        self.top().map(|f| f.prev_focus)
    }

    pub fn noun(&self) -> &'static str {
        if self.popup.is_some() {
            "popup"
        } else {
            "scratch pane"
        }
    }

    /// `base` bumped past the ids the overlays own — they are not in `ws.tabs`,
    /// so `Workspace::next_pane_id` alone would let a split reuse one.
    pub fn next_id(&self, base: PaneId) -> PaneId {
        self.float.iter().chain(&self.popup).fold(base, |b, f| b.max(f.id + 1))
    }

    pub fn spec(&self, id: PaneId) -> Option<&PaneSpec> {
        self.float.iter().chain(&self.popup).find(|f| f.id == id).map(|f| &f.spec)
    }

    pub fn spec_mut(&mut self, id: PaneId) -> Option<&mut PaneSpec> {
        self.float.iter_mut().chain(&mut self.popup).find(|f| f.id == id).map(|f| &mut f.spec)
    }

    /// C22/M1: the one guard every control verb that resolves a pane id
    /// shares — the scratch float is the human's, never a control target.
    /// Checked before authz so a caller is told the truth, not "forbidden".
    pub fn refusal(&self, id: PaneId, verb: &str) -> Option<String> {
        self.is_float(id).then(|| format!("cannot {verb} the scratch pane"))
    }

    pub fn open_float(&mut self, id: PaneId, spec: PaneSpec, prev_focus: PaneId) {
        self.float = Some(Float { id, spec, shown: true, prev_focus });
    }

    pub fn open_popup(&mut self, id: PaneId, spec: PaneSpec, prev_focus: PaneId) {
        self.popup = Some(Float { id, spec, shown: true, prev_focus });
    }

    /// Re-show the existing float over `prev`; its id, or `None` if there is none.
    pub fn show_float(&mut self, prev: PaneId) -> Option<PaneId> {
        let f = self.float.as_mut()?;
        f.shown = true;
        f.prev_focus = prev;
        Some(f.id)
    }

    /// Hide the float if shown; the pane focus should return to.
    pub fn hide_float(&mut self) -> Option<PaneId> {
        let f = self.float.as_mut().filter(|f| f.shown)?;
        f.shown = false;
        Some(f.prev_focus)
    }

    /// Empty the float slot: `(id, prev_focus)`.
    pub fn take_float(&mut self) -> Option<(PaneId, PaneId)> {
        self.float.take().map(|f| (f.id, f.prev_focus))
    }

    /// Empty the popup slot: `(id, prev_focus)`.
    pub fn take_popup(&mut self) -> Option<(PaneId, PaneId)> {
        self.popup.take().map(|f| (f.id, f.prev_focus))
    }

    /// Begin a transient focus move that must not dismiss the popup or hide
    /// the float; hand the token back to `unpin`.
    pub fn pin(&mut self) -> bool {
        self.pinned = true;
        self.float_shown()
    }

    pub fn unpin(&mut self, float_shown: bool) {
        self.pinned = false;
        if let Some(f) = &mut self.float {
            f.shown = float_shown;
        }
    }

    /// C22 rule 1 in one place, called by the single writer of `App::focused`
    /// as focus moves `old` → `new`: leaving the popup closes it (returns its
    /// id for the caller to kill), and leaving a shown float for any other
    /// pane hides it.
    pub fn focus_moved(&mut self, old: PaneId, new: PaneId) -> Option<PaneId> {
        if old == new {
            return None;
        }
        let dropped = if !self.pinned && self.is_popup(old) {
            self.take_popup().map(|(id, _)| id)
        } else {
            None
        };
        if self.is_overlay(old) && !self.is_float(new) {
            if let Some(f) = &mut self.float {
                f.shown = false;
            }
        }
        dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> PaneSpec {
        PaneSpec {
            adapter: "shell".into(),
            cwd: ".".into(),
            session: None,
            title: None,
            spawned_by: None,
            note: None,
            noted_at: None,
        }
    }

    #[test]
    fn focus_leaving_hides_the_float_and_closes_the_popup() {
        let mut o = Overlays::default();
        o.open_float(7, spec(), 1);
        assert_eq!(o.focus_moved(7, 7), None);
        assert!(o.float_shown());
        assert_eq!(o.focus_moved(7, 1), None);
        assert!(!o.float_shown() && o.float_id() == Some(7));
        assert_eq!(o.next_id(3), 8);

        o.open_popup(9, spec(), 1);
        assert_eq!(o.top_id(), Some(9));
        o.pin();
        assert_eq!(o.focus_moved(9, 1), None);
        o.unpin(false);
        assert_eq!(o.focus_moved(9, 1), Some(9));
        assert!(o.popup_id().is_none());
    }
}
