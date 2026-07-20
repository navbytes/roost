//! App state + actions: the glue between workspace (precious) and pane
//! runtimes (disposable).

use anyhow::Result;
use ratatui::layout::{Rect, Size};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant, SystemTime};

use crate::adapters::Registry;
use crate::event::AppEvent;
use crate::input::Action;
use crate::pane::PaneRuntime;
use crate::status::AgentStatus;
use crate::workspace::{
    self, compute_rects, LayoutNode, PaneId, PaneRect, PaneSpec, SplitDir, Tab, Workspace,
};

const DETECT_INTERVAL: Duration = Duration::from_secs(2);

/// Adapters offered by the quick-launch picker (Alt+Enter).
pub const PICKER_ITEMS: [&str; 3] = ["pi", "claude", "shell"];

/// UI mode: non-Normal modes capture all keys (see handle_mode_key).
pub enum Mode {
    Normal,
    Rename { buffer: String },
    Picker { selection: usize },
    Scroll { offset: usize },
}

pub struct App {
    pub ws: Workspace,
    pub runtimes: HashMap<PaneId, PaneRuntime>,
    pub registry: Registry,
    pub focused: PaneId,
    pub quit: bool,
    /// Spawn errors for panes whose process never started.
    pub dead: HashMap<PaneId, String>,
    pub mode: Mode,
    tx: Sender<AppEvent>,
    term_size: Size,
    /// Freshly launched agent panes we still owe a session id.
    pending_detect: HashMap<PaneId, SystemTime>,
    last_detect: Instant,
    sock_path: Option<PathBuf>,
}

impl App {
    /// Restore the workspace (design doc §5): rebuild the tree and spawn
    /// every pane via its adapter — resume when a session id is known,
    /// fresh launch otherwise. A failed spawn degrades to a dead pane, it
    /// never aborts restore.
    pub fn new(
        registry: Registry,
        tx: Sender<AppEvent>,
        term_size: Size,
        sock_path: Option<PathBuf>,
    ) -> Result<Self> {
        let ws = Workspace::load_or_default()?;
        let mut app = Self {
            focused: 0,
            ws,
            runtimes: HashMap::new(),
            registry,
            quit: false,
            dead: HashMap::new(),
            mode: Mode::Normal,
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

    fn body_area(&self) -> Rect {
        Rect::new(0, 1, self.term_size.width, self.term_size.height.saturating_sub(1))
    }

    fn rects(&self) -> Vec<PaneRect> {
        let mut v = Vec::new();
        compute_rects(&self.ws.active_tab().layout, self.body_area(), &mut v);
        v
    }

    pub fn pane_order(&self) -> Vec<PaneId> {
        let mut v = Vec::new();
        workspace::pane_order(&self.ws.active_tab().layout, &mut v);
        v
    }

    /// Spawn runtimes for every pane in the active tab that doesn't have one.
    /// (Scaffold spawns per-tab lazily; all panes in a tab spawn eagerly.)
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
        match PaneRuntime::spawn(id, &cmd, rows, cols, self.tx.clone()) {
            Ok(rt) => {
                self.runtimes.insert(id, rt);
                self.dead.remove(&id);
                // Owe this pane a session id? Watch for one (socket reports
                // it exactly; the filesystem scan below is the fallback).
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
            if let Some(session) = adapter.detect_session(&spec.cwd, since) {
                self.set_session(id, session);
            }
        }
    }

    fn find_spec(&self, id: PaneId) -> Option<&PaneSpec> {
        self.ws.tabs.iter().find_map(|t| t.panes.get(&id))
    }

    fn find_spec_mut(&mut self, id: PaneId) -> Option<&mut PaneSpec> {
        self.ws.tabs.iter_mut().find_map(|t| t.panes.get_mut(&id))
    }

    fn set_session(&mut self, id: PaneId, session: String) {
        if let Some(spec) = self.find_spec_mut(id) {
            spec.session = Some(session);
            self.pending_detect.remove(&id);
            let _ = self.ws.save();
        }
    }

    /// Session id reported exactly by an agent-side extension.
    pub fn on_session(&mut self, id: PaneId, session: String) {
        self.set_session(id, session);
    }

    /// Exact status from an agent-side extension. Returns a notification
    /// message when a *non-focused* pane starts needing the user.
    pub fn on_status(&mut self, id: PaneId, status: AgentStatus) -> Option<String> {
        let prev = self.runtimes.get(&id).map(|rt| rt.status.current());
        if let Some(rt) = self.runtimes.get_mut(&id) {
            rt.status.set_extension_status(status);
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

    // -- dead panes --------------------------------------------------------

    /// True when the focused pane has no live process (spawn failed or the
    /// child exited) — its keys are then handled by roost, not forwarded.
    pub fn focused_dead(&self) -> bool {
        match self.runtimes.get(&self.focused) {
            None => true,
            Some(rt) => rt.status.current() == AgentStatus::Exited,
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
        let _ = self.ws.save();
    }

    // -- event handling ----------------------------------------------------

    pub fn on_pty_output(&mut self, id: PaneId, bytes: &[u8]) {
        if let Some(rt) = self.runtimes.get_mut(&id) {
            rt.process_output(bytes);
        }
    }

    pub fn on_pty_exit(&mut self, id: PaneId) {
        if let Some(rt) = self.runtimes.get_mut(&id) {
            rt.status.on_exit();
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

    /// Recompute rects and push new sizes to every PTY.
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

    // -- actions -----------------------------------------------------------

    pub fn apply(&mut self, action: Action) {
        match action {
            Action::Quit => self.quit = true,
            Action::NewPane => self.new_pane(),
            Action::ClosePane => self.close_pane(),
            Action::FocusNext => self.cycle_focus(1),
            Action::FocusPrev => self.cycle_focus(-1),
            Action::NewTab => self.new_tab(),
            Action::GoToTab(i) => self.go_to_tab(i),
            Action::ToggleStack => {
                let focused = self.focused;
                workspace::toggle_stack(&mut self.ws.active_tab_mut().layout, focused);
            }
            Action::Resize { horizontal, grow } => {
                let delta = if grow { 0.04 } else { -0.04 };
                let axis = if horizontal { SplitDir::Vertical } else { SplitDir::Horizontal };
                let focused = self.focused;
                workspace::resize_pane(&mut self.ws.active_tab_mut().layout, focused, axis, delta);
            }
            Action::RenamePane => {
                let current = self
                    .find_spec(self.focused)
                    .and_then(|s| s.title.clone())
                    .unwrap_or_default();
                self.mode = Mode::Rename { buffer: current };
            }
            Action::QuickLaunch => self.mode = Mode::Picker { selection: 0 },
            Action::ScrollMode => self.mode = Mode::Scroll { offset: 0 },
        }
        self.relayout();
        // Debounced in the design; scaffold saves eagerly on every mutation.
        let _ = self.ws.save();
    }

    fn cycle_focus(&mut self, dir: i64) {
        let order = self.pane_order();
        if order.is_empty() {
            return;
        }
        let cur = order.iter().position(|&p| p == self.focused).unwrap_or(0) as i64;
        let next = (cur + dir).rem_euclid(order.len() as i64) as usize;
        self.focused = order[next];
        // Focusing a collapsed stack member expands it.
        expand_in_stacks(&mut self.ws.active_tab_mut().layout, self.focused);
    }

    fn new_pane(&mut self) {
        self.new_pane_with("shell");
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
        let dir = self
            .rects()
            .iter()
            .find(|pr| pr.id == self.focused)
            .map(|pr| {
                if pr.rect.width >= pr.rect.height * 3 {
                    SplitDir::Vertical
                } else {
                    SplitDir::Horizontal
                }
            })
            .unwrap_or(SplitDir::Vertical);

        let focused = self.focused;
        let tab = self.ws.active_tab_mut();
        tab.panes.insert(id, spec.clone());
        if !workspace::split_pane(&mut tab.layout, focused, id, dir) {
            tab.layout = LayoutNode::Pane(id); // empty tab fallback
        }
        self.focused = id;
        // Spawn with a placeholder size; relayout() in apply() fixes it up.
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
        let empty = workspace::remove_pane(&mut tab.layout, id);
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
            Mode::Rename { buffer } => {
                match key.code {
                    KeyCode::Char(c) => buffer.push(c),
                    KeyCode::Backspace => {
                        buffer.pop();
                    }
                    KeyCode::Enter => {
                        let title = buffer.trim().to_string();
                        let focused = self.focused;
                        if let Some(spec) = self.find_spec_mut(focused) {
                            spec.title = if title.is_empty() { None } else { Some(title) };
                        }
                        let _ = self.ws.save();
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
                        let _ = self.ws.save();
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
                            rt.parser.set_scrollback(n);
                        }
                    }
                    None => {
                        if let Some(rt) = self.runtimes.get_mut(&focused) {
                            rt.parser.set_scrollback(0);
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
        let _ = self.ws.save();
        for rt in self.runtimes.values_mut() {
            rt.kill();
        }
    }
}

fn inner_dims(rect: Rect) -> (u16, u16) {
    (rect.height.saturating_sub(2).max(1), rect.width.saturating_sub(2).max(1))
}

/// If `target` is a collapsed member of any stack, expand it.
fn expand_in_stacks(node: &mut LayoutNode, target: PaneId) {
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
