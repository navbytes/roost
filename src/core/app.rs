//! App core: orchestrates workspace (precious) and pane backends
//! (disposable) purely through ports — no PTY, filesystem, or terminal
//! specifics here. Generic over `PaneBackend` so every behavior below is
//! unit-tested with fakes (see tests at the bottom).

use anyhow::Result;
use ratatui::layout::{Rect, Size};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant, SystemTime};

use crate::agents::Registry;
use crate::core::event::AppEvent;
use crate::core::layout::{self, LayoutNode, PaneId, PaneRect, SplitDir};
use crate::core::status::AgentStatus;
use crate::core::workspace::{PaneSpec, Tab, Workspace};
use crate::ports::{PaneBackend, StateStore};
use crate::ui::input::Action;

const DETECT_INTERVAL: Duration = Duration::from_secs(2);

/// A pane must stay usable after a split. These are the smallest *outer*
/// rects (borders included) we allow a split to produce; below them the new
/// pane would be a sliver, so the split is refused.
const MIN_SPLIT_COLS: u16 = 36; // two ~16-col inner panes + borders
const MIN_SPLIT_ROWS: u16 = 10; // two ~3-row inner panes + borders

/// Adapters offered by the quick-launch picker (Alt+Enter).
pub const PICKER_ITEMS: [&str; 3] = ["pi", "claude", "shell"];

/// What a rename overlay is editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameTarget {
    Pane,
    Tab,
}

/// UI mode: non-Normal modes capture all keys (see handle_mode_key).
pub enum Mode {
    Normal,
    Rename { buffer: String, target: RenameTarget },
    Picker { selection: usize },
    Scroll { offset: usize },
}

pub struct App<B: PaneBackend> {
    pub ws: Workspace,
    pub runtimes: HashMap<PaneId, B>,
    pub registry: Registry,
    pub focused: PaneId,
    pub quit: bool,
    /// Spawn errors for panes whose process never started.
    pub dead: HashMap<PaneId, String>,
    pub mode: Mode,
    /// Zellij-style shortcut hint bar at the bottom; on by default so keys
    /// are always discoverable. Session-only (not persisted).
    hints: bool,
    store: Box<dyn StateStore>,
    tx: Sender<AppEvent>,
    term_size: Size,
    /// Freshly launched agent panes we still owe a session id.
    pending_detect: HashMap<PaneId, SystemTime>,
    last_detect: Instant,
    sock_path: Option<PathBuf>,
}

impl<B: PaneBackend> App<B> {
    /// Restore the workspace (design doc §5): rebuild the tree and spawn
    /// every pane via its adapter — resume when a session id is known,
    /// fresh launch otherwise. A failed spawn degrades to a dead pane, it
    /// never aborts restore.
    pub fn new(
        ws: Workspace,
        registry: Registry,
        store: Box<dyn StateStore>,
        tx: Sender<AppEvent>,
        term_size: Size,
        sock_path: Option<PathBuf>,
    ) -> Result<Self> {
        let mut app = Self {
            focused: 0,
            ws,
            runtimes: HashMap::new(),
            registry,
            quit: false,
            dead: HashMap::new(),
            mode: Mode::Normal,
            hints: true,
            store,
            tx,
            term_size,
            pending_detect: HashMap::new(),
            last_detect: Instant::now(),
            sock_path,
        };
        app.spawn_active_tab();
        app.focused = app.pane_order().first().copied().unwrap_or(0);
        Ok(app)
    }

    fn save(&self) {
        let _ = self.store.save(&self.ws);
    }

    /// The pane area: below the tab bar (row 0), above the hint bar (last
    /// row) when it's shown. Single source of truth for both layout/PTY
    /// sizing and rendering.
    pub fn body_area(&self) -> Rect {
        let reserved = 1 + if self.hints_shown() { 1 } else { 0 };
        Rect::new(0, 1, self.term_size.width, self.term_size.height.saturating_sub(reserved))
    }

    /// Whether the hint bar is actually drawn: enabled, and the terminal is
    /// tall enough to spare the row (tab + hint + at least one body row).
    pub fn hints_shown(&self) -> bool {
        self.hints && self.term_size.height >= 3
    }

    /// Pane rectangles of the active tab (border-inclusive).
    pub fn rects(&self) -> Vec<PaneRect> {
        let mut v = Vec::new();
        layout::compute_rects(&self.ws.active_tab().layout, self.body_area(), &mut v);
        v
    }

    pub fn pane_order(&self) -> Vec<PaneId> {
        let mut v = Vec::new();
        layout::pane_order(&self.ws.active_tab().layout, &mut v);
        v
    }

    /// Spawn runtimes for every pane in the active tab that doesn't have one.
    pub fn spawn_active_tab(&mut self) {
        for pr in self.rects() {
            if self.runtimes.contains_key(&pr.id) {
                continue;
            }
            let Some(spec) = self.ws.active_tab().panes.get(&pr.id).cloned() else { continue };
            self.spawn_pane(pr.id, &spec, pr.rect);
        }
    }

    fn spawn_pane(&mut self, id: PaneId, spec: &PaneSpec, rect: Rect) {
        let Some(adapter) = self.registry.get(spec.adapter.as_str()) else { return };
        let mut cmd = adapter.command_for(spec);
        if let Some(sock) = &self.sock_path {
            cmd.env.push(("ROOST_SOCK".into(), sock.to_string_lossy().into_owned()));
        }
        let (rows, cols) = inner_dims(rect);
        match B::spawn(id, &cmd, rows, cols, self.tx.clone()) {
            Ok(rt) => {
                self.runtimes.insert(id, rt);
                self.dead.remove(&id);
                // Owe this pane a session id? Watch for one (socket reports
                // it exactly; the filesystem scan in tick() is the fallback).
                if spec.session.is_none() && adapter.session_root(&spec.cwd).is_some() {
                    self.pending_detect.insert(id, SystemTime::now());
                }
            }
            Err(e) => {
                self.dead.insert(id, e.to_string());
            }
        }
    }

    /// Periodic housekeeping: filesystem-based session detection (design doc
    /// §6.1 fallback). Called from the main loop; self-throttled.
    pub fn tick(&mut self) {
        if self.last_detect.elapsed() < DETECT_INTERVAL || self.pending_detect.is_empty() {
            return;
        }
        self.last_detect = Instant::now();
        let pending: Vec<(PaneId, SystemTime)> =
            self.pending_detect.iter().map(|(k, v)| (*k, *v)).collect();
        for (id, since) in pending {
            let Some((spec, adapter)) = self.find_spec(id).and_then(|s| {
                self.registry.get(s.adapter.as_str()).map(|a| (s.clone(), a))
            }) else {
                self.pending_detect.remove(&id);
                continue;
            };
            // Session ids already owned by other panes — never re-assign one
            // (concurrent same-cwd launches otherwise cross-wire onto it).
            let taken = self.claimed_sessions();
            if let Some(session) = adapter.detect_session(&spec.cwd, since, &taken) {
                self.set_session(id, session);
            }
        }
    }

    /// Session ids currently assigned to any pane.
    fn claimed_sessions(&self) -> std::collections::HashSet<String> {
        self.ws
            .tabs
            .iter()
            .flat_map(|t| t.panes.values())
            .filter_map(|s| s.session.clone())
            .collect()
    }

    pub fn find_spec(&self, id: PaneId) -> Option<&PaneSpec> {
        self.ws.tabs.iter().find_map(|t| t.panes.get(&id))
    }

    fn find_spec_mut(&mut self, id: PaneId) -> Option<&mut PaneSpec> {
        self.ws.tabs.iter_mut().find_map(|t| t.panes.get_mut(&id))
    }

    fn set_session(&mut self, id: PaneId, session: String) {
        if let Some(spec) = self.find_spec_mut(id) {
            spec.session = Some(session);
            self.pending_detect.remove(&id);
            self.save();
        }
    }

    // -- event handling ----------------------------------------------------

    pub fn on_pty_output(&mut self, id: PaneId, bytes: &[u8]) {
        if let Some(rt) = self.runtimes.get_mut(&id) {
            rt.process_output(bytes);
        }
    }

    pub fn on_pty_exit(&mut self, id: PaneId) {
        if let Some(rt) = self.runtimes.get_mut(&id) {
            rt.on_exit();
        }
    }

    /// Session id reported exactly by an agent-side extension.
    pub fn on_session(&mut self, id: PaneId, session: String) {
        self.set_session(id, session);
    }

    /// Exact status from an agent-side extension. Returns a notification
    /// message when a *non-focused* pane starts needing the user.
    pub fn on_status(&mut self, id: PaneId, status: AgentStatus) -> Option<String> {
        let prev = self.runtimes.get(&id).map(|rt| rt.status());
        if let Some(rt) = self.runtimes.get_mut(&id) {
            rt.set_extension_status(status);
        }
        let became_needy = matches!(status, AgentStatus::NeedsInput | AgentStatus::Waiting)
            && prev == Some(AgentStatus::Working);
        if became_needy && id != self.focused {
            let name = self
                .find_spec(id)
                .map(|s| s.title.clone().unwrap_or_else(|| s.adapter.clone()))
                .unwrap_or_else(|| format!("pane {id}"));
            Some(format!("{name} is waiting for you"))
        } else {
            None
        }
    }

    pub fn forward_bytes(&mut self, bytes: &[u8]) {
        let id = self.focused;
        if let Some(rt) = self.runtimes.get_mut(&id) {
            rt.write_input(bytes);
        }
    }

    pub fn on_resize(&mut self, size: Size) {
        self.term_size = size;
        self.relayout();
    }

    /// Recompute rects and push new sizes to every pane backend.
    pub fn relayout(&mut self) {
        for pr in self.rects() {
            if pr.collapsed {
                continue;
            }
            let (rows, cols) = inner_dims(pr.rect);
            if let Some(rt) = self.runtimes.get_mut(&pr.id) {
                rt.resize(rows, cols);
            }
        }
    }

    // -- mouse -------------------------------------------------------------

    /// Left click: focus the pane under the cursor (expanding stack members).
    pub fn on_click(&mut self, id: PaneId) {
        self.focused = id;
        layout::expand_in_stacks(&mut self.ws.active_tab_mut().layout, id);
        self.relayout();
        self.save();
    }

    /// Forward an encoded mouse event to a mouse-aware pane app.
    pub fn wheel_forward(&mut self, id: PaneId, bytes: &[u8]) {
        if let Some(rt) = self.runtimes.get_mut(&id) {
            // Not write_input(): a forwarded wheel event must not snap the
            // pane's scrollback to the live tail.
            rt.write_input_raw(bytes);
        }
    }

    /// Scroll roost's own scrollback for a pane (mouse-unaware app).
    pub fn wheel_scroll(&mut self, id: PaneId, delta: i32) {
        if let Some(rt) = self.runtimes.get_mut(&id) {
            rt.scroll_by(delta);
        }
    }

    // -- dead panes --------------------------------------------------------

    /// True when the focused pane has no live process (spawn failed or the
    /// child exited) — its keys are then handled by roost, not forwarded.
    pub fn focused_dead(&self) -> bool {
        match self.runtimes.get(&self.focused) {
            None => true,
            Some(rt) => rt.status() == AgentStatus::Exited,
        }
    }

    /// Relaunch the focused dead pane. `fresh` drops the session id first
    /// (for when resume fails because the session was deleted).
    pub fn respawn_focused(&mut self, fresh: bool) {
        let id = self.focused;
        if fresh {
            if let Some(spec) = self.find_spec_mut(id) {
                spec.session = None;
            }
        }
        if let Some(mut rt) = self.runtimes.remove(&id) {
            rt.kill();
        }
        self.dead.remove(&id);
        let Some(spec) = self.find_spec(id).cloned() else { return };
        if let Some(pr) = self.rects().iter().find(|pr| pr.id == id).copied() {
            self.spawn_pane(id, &spec, pr.rect);
        }
        self.save();
    }

    // -- actions -----------------------------------------------------------

    pub fn apply(&mut self, action: Action) {
        match action {
            Action::Quit => self.quit = true,
            Action::NewPane => self.new_pane_with("shell"),
            Action::ClosePane => self.close_pane(),
            Action::Focus(dir) => self.focus_dir(dir),
            Action::NewTab => self.new_tab(),
            Action::GoToTab(i) => self.go_to_tab(i),
            Action::ToggleStack => {
                let focused = self.focused;
                layout::toggle_stack(&mut self.ws.active_tab_mut().layout, focused);
            }
            Action::Resize { horizontal, grow } => {
                let delta = if grow { 0.04 } else { -0.04 };
                let axis = if horizontal { SplitDir::Vertical } else { SplitDir::Horizontal };
                let focused = self.focused;
                layout::resize_pane(&mut self.ws.active_tab_mut().layout, focused, axis, delta);
            }
            Action::RenamePane => {
                let current = self
                    .find_spec(self.focused)
                    .and_then(|s| s.title.clone())
                    .unwrap_or_default();
                self.mode = Mode::Rename { buffer: current, target: RenameTarget::Pane };
            }
            Action::RenameTab => {
                let current = self.ws.active_tab().name.clone();
                self.mode = Mode::Rename { buffer: current, target: RenameTarget::Tab };
            }
            Action::QuickLaunch => self.mode = Mode::Picker { selection: 0 },
            Action::ScrollMode => self.mode = Mode::Scroll { offset: 0 },
            Action::ToggleHints => self.hints = !self.hints,
        }
        self.relayout();
        self.save();
    }

    /// Move focus spatially to the nearest pane in `dir`; stay put if none.
    fn focus_dir(&mut self, dir: layout::Dir) {
        let rects = self.rects();
        if let Some(id) = layout::neighbor(&rects, self.focused, dir) {
            self.focused = id;
            layout::expand_in_stacks(&mut self.ws.active_tab_mut().layout, id);
        }
    }

    fn new_pane_with(&mut self, adapter: &str) {
        let id = self.ws.next_pane_id();
        let cwd = self
            .ws
            .active_tab()
            .panes
            .get(&self.focused)
            .map(|s| s.cwd.clone())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let spec = PaneSpec { adapter: adapter.into(), cwd, session: None, title: None };

        // Split in the widest direction of the focused pane's rect.
        let focused_rect = self.rects().iter().find(|pr| pr.id == self.focused).map(|pr| pr.rect);
        let dir = focused_rect
            .map(|r| {
                if r.width >= r.height * 3 {
                    SplitDir::Vertical
                } else {
                    SplitDir::Horizontal
                }
            })
            .unwrap_or(SplitDir::Vertical);

        // Refuse a split that would produce unusably tiny panes (also the
        // trigger for the vt100 underflow crash). Silent no-op — the layout
        // is left untouched. See MIN_SPLIT_* below.
        if let Some(r) = focused_rect {
            let too_small = match dir {
                SplitDir::Vertical => r.width < MIN_SPLIT_COLS,
                SplitDir::Horizontal => r.height < MIN_SPLIT_ROWS,
            };
            if too_small {
                return;
            }
        }

        let focused = self.focused;
        let tab = self.ws.active_tab_mut();
        tab.panes.insert(id, spec.clone());
        if !layout::split_pane(&mut tab.layout, focused, id, dir) {
            tab.layout = LayoutNode::Pane(id); // empty tab fallback
        }
        self.focused = id;
        if let Some(pr) = self.rects().iter().find(|pr| pr.id == id).copied() {
            self.spawn_pane(id, &spec, pr.rect);
        }
    }

    fn close_pane(&mut self) {
        let id = self.focused;
        if let Some(mut rt) = self.runtimes.remove(&id) {
            rt.kill();
        }
        let tab = self.ws.active_tab_mut();
        tab.panes.remove(&id);
        let empty = layout::remove_pane(&mut tab.layout, id);
        if empty {
            if self.ws.tabs.len() > 1 {
                let i = self.ws.active_tab;
                self.ws.tabs.remove(i);
                self.ws.active_tab = i.saturating_sub(1);
                self.spawn_active_tab();
            } else {
                self.quit = true;
                return;
            }
        }
        self.focused = self.pane_order().first().copied().unwrap_or(0);
    }

    fn new_tab(&mut self) {
        let id = self.ws.next_pane_id();
        let cwd = std::env::current_dir().unwrap_or_default();
        let mut panes = HashMap::new();
        panes.insert(id, PaneSpec { adapter: "shell".into(), cwd, session: None, title: None });
        self.ws.tabs.push(Tab {
            name: format!("tab{}", self.ws.tabs.len() + 1),
            layout: LayoutNode::Pane(id),
            panes,
        });
        self.ws.active_tab = self.ws.tabs.len() - 1;
        self.spawn_active_tab();
        self.focused = id;
    }

    fn go_to_tab(&mut self, i: usize) {
        if i < self.ws.tabs.len() {
            self.ws.active_tab = i;
            self.spawn_active_tab();
            self.focused = self.pane_order().first().copied().unwrap_or(self.focused);
        }
    }

    // -- modes -------------------------------------------------------------

    /// Keys while in a non-Normal mode. Returns true when consumed.
    pub fn handle_mode_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::KeyCode;
        // Alt-chords always reach the global bindings (Alt+q must quit from
        // anywhere). Overlay modes cancel; scroll mode survives.
        if key.modifiers.contains(crossterm::event::KeyModifiers::ALT) {
            if !matches!(self.mode, Mode::Scroll { .. }) {
                self.mode = Mode::Normal;
            }
            return false;
        }
        match &mut self.mode {
            Mode::Normal => false,
            Mode::Rename { buffer, target } => {
                let target = *target;
                match key.code {
                    KeyCode::Char(c) => buffer.push(c),
                    KeyCode::Backspace => {
                        buffer.pop();
                    }
                    KeyCode::Enter => {
                        let text = buffer.trim().to_string();
                        match target {
                            RenameTarget::Pane => {
                                let focused = self.focused;
                                if let Some(spec) = self.find_spec_mut(focused) {
                                    // Empty clears back to the adapter name.
                                    spec.title = if text.is_empty() { None } else { Some(text) };
                                }
                            }
                            RenameTarget::Tab => {
                                // A tab always needs a name; ignore an empty one.
                                if !text.is_empty() {
                                    self.ws.active_tab_mut().name = text;
                                }
                            }
                        }
                        self.save();
                        self.mode = Mode::Normal;
                    }
                    KeyCode::Esc => self.mode = Mode::Normal,
                    _ => {}
                }
                true
            }
            Mode::Picker { selection } => {
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        *selection = selection.checked_sub(1).unwrap_or(PICKER_ITEMS.len() - 1)
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        *selection = (*selection + 1) % PICKER_ITEMS.len()
                    }
                    KeyCode::Enter => {
                        let adapter = PICKER_ITEMS[*selection];
                        self.mode = Mode::Normal;
                        self.new_pane_with(adapter);
                        self.relayout();
                        self.save();
                    }
                    KeyCode::Esc => self.mode = Mode::Normal,
                    _ => {}
                }
                true
            }
            Mode::Scroll { offset } => {
                let page = (self.term_size.height / 2).max(1) as usize;
                let new_offset = match key.code {
                    KeyCode::Up | KeyCode::Char('k') => Some(*offset + 1),
                    KeyCode::Down | KeyCode::Char('j') => Some(offset.saturating_sub(1)),
                    KeyCode::PageUp => Some(*offset + page),
                    KeyCode::PageDown => Some(offset.saturating_sub(page)),
                    KeyCode::Esc | KeyCode::Char('q') => None,
                    _ => return true,
                };
                let focused = self.focused;
                match new_offset {
                    Some(n) => {
                        *offset = n;
                        if let Some(rt) = self.runtimes.get_mut(&focused) {
                            rt.set_scrollback(n);
                        }
                    }
                    None => {
                        if let Some(rt) = self.runtimes.get_mut(&focused) {
                            rt.set_scrollback(0);
                        }
                        self.mode = Mode::Normal;
                    }
                }
                true
            }
        }
    }

    /// Clean shutdown: save workspace, kill children (their sessions live on).
    pub fn shutdown(&mut self) {
        self.save();
        for rt in self.runtimes.values_mut() {
            rt.kill();
        }
    }
}

fn inner_dims(rect: Rect) -> (u16, u16) {
    (rect.height.saturating_sub(2).max(1), rect.width.saturating_sub(2).max(1))
}

// ---------------------------------------------------------------------------
// Unit tests — the whole app core runs against fakes, no PTYs involved.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents;
    use crate::ports::fakes::{FakePane, MemStore};
    use std::path::PathBuf;
    use std::sync::mpsc;

    fn mk_app(ws: Workspace) -> (App<FakePane>, MemStore) {
        let store = MemStore::default();
        let (tx, _rx) = mpsc::channel();
        let app = App::<FakePane>::new(
            ws,
            agents::registry(),
            Box::new(store.clone()),
            tx,
            Size::new(100, 30),
            None,
        )
        .unwrap();
        (app, store)
    }

    fn shell_ws() -> Workspace {
        Workspace::default_in(PathBuf::from("/tmp"))
    }

    #[test]
    fn new_pane_splits_focuses_and_persists() {
        let (mut app, store) = mk_app(shell_ws());
        assert_eq!(app.focused, 1);
        app.apply(Action::NewPane);
        assert_eq!(app.focused, 2);
        assert_eq!(app.runtimes.len(), 2);
        let saved = store.0.lock().unwrap().clone().unwrap();
        assert_eq!(saved.tabs[0].panes.len(), 2);
    }

    #[test]
    fn close_last_pane_quits() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ClosePane);
        assert!(app.quit);
    }

    #[test]
    fn close_pane_returns_focus_to_remaining() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::ClosePane);
        assert_eq!(app.focused, 1);
        assert_eq!(app.runtimes.len(), 1);
        assert!(!app.quit);
    }

    #[test]
    fn session_reported_by_socket_is_persisted() {
        let (mut app, store) = mk_app(shell_ws());
        app.on_session(1, "sess-42".into());
        let saved = store.0.lock().unwrap().clone().unwrap();
        assert_eq!(saved.tabs[0].panes[&1].session.as_deref(), Some("sess-42"));
    }

    #[test]
    fn resume_command_uses_saved_session() {
        let mut ws = shell_ws();
        let spec = ws.tabs[0].panes.get_mut(&1).unwrap();
        spec.adapter = "pi".into();
        spec.session = Some("abc".into());
        let (app, _) = mk_app(ws);
        let rt = app.runtimes.get(&1).unwrap();
        assert_eq!(rt.cmd.program, "pi");
        assert_eq!(rt.cmd.args, vec!["--session", "abc"]);
    }

    #[test]
    fn respawn_fresh_drops_session() {
        let mut ws = shell_ws();
        ws.tabs[0].panes.get_mut(&1).unwrap().session = Some("old".into());
        let (mut app, store) = mk_app(ws);
        app.on_pty_exit(1);
        assert!(app.focused_dead());
        app.respawn_focused(true);
        assert!(!app.focused_dead());
        let saved = store.0.lock().unwrap().clone().unwrap();
        assert!(saved.tabs[0].panes[&1].session.is_none());
    }

    #[test]
    fn notification_only_for_unfocused_working_to_waiting() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // focus = 2
        app.on_status(1, AgentStatus::Working);
        assert!(app.on_status(1, AgentStatus::Waiting).is_some());
        // Focused pane never notifies.
        app.on_status(2, AgentStatus::Working);
        assert!(app.on_status(2, AgentStatus::NeedsInput).is_none());
        // Idle → waiting (no working phase) doesn't notify.
        assert!(app.on_status(1, AgentStatus::Waiting).is_none());
    }

    #[test]
    fn toggle_stack_then_click_expands_member() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::ToggleStack);
        assert!(matches!(app.ws.tabs[0].layout, LayoutNode::Stack { .. }));
        app.on_click(1);
        assert_eq!(app.focused, 1);
        assert!(matches!(app.ws.tabs[0].layout, LayoutNode::Stack { expanded: 0, .. }));
    }

    #[test]
    fn wheel_scroll_reaches_backend_and_typing_resets() {
        let (mut app, _) = mk_app(shell_ws());
        app.wheel_scroll(1, 3);
        app.wheel_scroll(1, 3);
        assert_eq!(app.runtimes[&1].scrollback, 6);
        app.wheel_scroll(1, -10); // clamped at 0
        assert_eq!(app.runtimes[&1].scrollback, 0);
        app.wheel_scroll(1, 5);
        app.forward_bytes(b"x"); // typing snaps to live tail
        assert_eq!(app.runtimes[&1].scrollback, 0);
    }

    #[test]
    fn quick_launch_picker_spawns_selected_adapter() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::QuickLaunch);
        assert!(matches!(app.mode, Mode::Picker { .. }));
        // pick item 1 ("claude")
        app.handle_mode_key(KeyEvent::from(KeyCode::Down));
        app.handle_mode_key(KeyEvent::from(KeyCode::Enter));
        let id = app.focused;
        assert_eq!(app.runtimes[&id].cmd.program, "claude");
    }

    #[test]
    fn splits_refuse_when_panes_get_too_small() {
        let (mut app, _) = mk_app(shell_ws()); // 100x30 terminal
        for _ in 0..60 {
            app.apply(Action::NewPane);
        }
        let n = app.ws.tabs[0].panes.len();
        // Splits must stop well before 60 panes — the guard refuses slivers.
        assert!(n < 40, "expected splits to be refused, got {n} panes");
        // Every surviving pane still has a non-degenerate rect.
        for pr in app.rects() {
            assert!(pr.rect.width >= 2 && pr.rect.height >= 1);
        }
    }

    #[test]
    fn directional_focus_moves_by_position() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // vertical split → panes 1 (left) | 2 (right), focus 2
        app.apply(Action::Focus(crate::core::layout::Dir::Left));
        assert_eq!(app.focused, 1);
        app.apply(Action::Focus(crate::core::layout::Dir::Right));
        assert_eq!(app.focused, 2);
    }

    #[test]
    fn rename_sets_title() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, store) = mk_app(shell_ws());
        app.apply(Action::RenamePane);
        for c in "build".chars() {
            app.handle_mode_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_mode_key(KeyEvent::from(KeyCode::Enter));
        let saved = store.0.lock().unwrap().clone().unwrap();
        assert_eq!(saved.tabs[0].panes[&1].title.as_deref(), Some("build"));
    }

    #[test]
    fn hint_bar_reserves_one_body_row_and_toggles() {
        let (mut app, _) = mk_app(shell_ws()); // 100x30, hints on by default
        assert!(app.hints_shown());
        let with = app.body_area().height;
        app.apply(Action::ToggleHints);
        assert!(!app.hints_shown());
        let without = app.body_area().height;
        assert_eq!(without, with + 1); // reclaimed the hint row
    }

    #[test]
    fn hint_bar_hidden_on_tiny_terminal() {
        let (mut app, _) = mk_app(shell_ws());
        app.on_resize(Size::new(80, 2)); // no room for tab + hint + body
        assert!(!app.hints_shown());
        // body_area must not underflow
        assert!(app.body_area().height <= 2);
    }

    #[test]
    fn rename_tab_sets_name_and_persists() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, store) = mk_app(shell_ws());
        assert_eq!(app.ws.active_tab().name, "main");
        app.apply(Action::RenameTab);
        // overlay prefills the current name ("main") for editing — clear it
        for _ in 0..4 {
            app.handle_mode_key(KeyEvent::from(KeyCode::Backspace));
        }
        for c in "roost-repo".chars() {
            app.handle_mode_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_mode_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.ws.active_tab().name, "roost-repo");
        let saved = store.0.lock().unwrap().clone().unwrap();
        assert_eq!(saved.tabs[0].name, "roost-repo");
    }

    #[test]
    fn rename_tab_ignores_empty_name() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::RenameTab);
        // clear the prefilled "main" then commit empty
        for _ in 0..8 {
            app.handle_mode_key(KeyEvent::from(KeyCode::Backspace));
        }
        app.handle_mode_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.ws.active_tab().name, "main"); // unchanged
    }
}
