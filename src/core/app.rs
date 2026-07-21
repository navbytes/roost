//! App core: orchestrates workspace (precious) and pane backends
//! (disposable) purely through ports — no PTY, filesystem, or terminal
//! specifics here. Generic over `PaneBackend` so every behavior below is
//! unit-tested with fakes (see tests at the bottom).

use anyhow::Result;
use ratatui::layout::{Rect, Size};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Sender, SyncSender};
use std::time::{Duration, Instant, SystemTime};

use crate::agents::Registry;
use crate::core::control::{Actor, Method, ReadMode, Reply, Request};
use crate::core::event::AppEvent;
use crate::core::layout::{self, LayoutNode, PaneId, PaneRect, SplitDir};
use crate::core::status::AgentStatus;
use crate::core::workspace::{PaneSpec, Tab, Workspace};
use crate::ports::{Observation, PaneBackend, StateStore};
use crate::ui::input::Action;

const DETECT_INTERVAL: Duration = Duration::from_secs(2);

/// How long the "Alt keys aren't reaching roost" hint stays up on a fresh
/// launch before we assume the user isn't going to press one / already saw it.
const ALT_HINT_WINDOW: Duration = Duration::from_secs(8);

/// A pane must stay usable after a split. These are the smallest *outer*
/// rects (borders included) we allow a split to produce; below them the new
/// pane would be a sliver, so the split is refused.
const MIN_SPLIT_COLS: u16 = 36; // two ~16-col inner panes + borders
const MIN_SPLIT_ROWS: u16 = 10; // two ~3-row inner panes + borders

/// Adapters offered by the quick-launch picker (Alt+Enter), derived from the
/// single adapter list in `agents` so the picker can never drift out of sync
/// with the registry.
pub fn picker_items() -> Vec<&'static str> {
    crate::agents::picker_ids()
}

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
    /// Mouse-drag text selection; copies to the clipboard on release.
    Copy,
    /// Full-keymap overlay (Alt+?); any key dismisses it.
    Help,
}

/// An in-progress / completed text selection within one pane. Coordinates are
/// (row, col) in the pane's inner (border-excluded) cell space, 0-based.
#[derive(Debug, Clone, Copy)]
pub struct Selection {
    pub pane: PaneId,
    pub anchor: (u16, u16),
    pub cursor: (u16, u16),
    pub dragging: bool,
}

/// How long the "copied" flash stays in the hint bar.
const FLASH_WINDOW: Duration = Duration::from_secs(2);

/// How long an armed destructive-close confirmation stays live: press the same
/// key again within this window to actually close/quit.
const CONFIRM_WINDOW: Duration = Duration::from_secs(3);

/// How many closed panes/tabs the undo stack keeps.
const UNDO_DEPTH: usize = 20;

/// Cap on concurrently parked `wait` requests. Each parked wait holds a
/// socket connection slot for up to its whole timeout, and one-shot calls
/// like status/list share that same pool (`MAX_CONN` in sock.rs) — without a
/// cap, enough parked waits could starve those of a slot to even report in
/// on. Left well below that pool size so plenty always stay free.
const MAX_WAITS: usize = 16;

/// A closed pane or tab, kept on the undo stack so `Alt+u` can reopen it —
/// crucially with its session id intact, so the agent resumes where it was.
#[derive(Debug, Clone)]
enum Closed {
    /// A single pane closed out of a tab that still exists.
    Pane { tab_index: usize, spec: PaneSpec },
    /// A whole tab (its last pane was closed), captured before removal.
    Tab { index: usize, tab: Tab },
}

/// A parked `wait` control request: reply when any of `panes` reaches `until`,
/// or when `deadline` passes. Holds the client's reply channel until then.
struct Waiter {
    panes: Vec<PaneId>,
    until: AgentStatus,
    reply: Sender<Reply>,
    deadline: Instant,
}

/// A tab's aggregate state for the tab bar, worst-relevant-first. `Unknown`
/// is a lazily-loaded tab whose panes haven't been spawned — deliberately
/// distinct from `Quiet` (spawned, nothing happening) so a background tab
/// never masquerades as idle. `render` maps each to a glyph + colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabSummary {
    NeedsInput,
    Working,
    Unknown,
    Waiting,
    Quiet,
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
    tx: SyncSender<AppEvent>,
    term_size: Size,
    /// Freshly launched agent panes we still owe a session id.
    pending_detect: HashMap<PaneId, SystemTime>,
    last_detect: Instant,
    sock_path: Option<PathBuf>,
    started: Instant,
    /// Set the first time an Alt-modified key event arrives, so the
    /// "Alt keys aren't reaching roost" startup hint can stop once we know
    /// they are (or the window has simply run out).
    alt_seen: bool,
    /// Active/last text selection (copy mode).
    pub selection: Option<Selection>,
    /// Transient status message shown in the hint bar (e.g. "copied").
    flash: Option<(String, Instant)>,
    /// Recently closed panes/tabs, for `Alt+u` undo (most-recent last).
    undo: Vec<Closed>,
    /// When a destructive close (busy pane / last pane) has been armed and is
    /// awaiting a confirming second keypress.
    confirm_close: Option<Instant>,
    /// Per-spawn secret handed to each pane's child via `ROOST_TOKEN`. A
    /// socket message is only honored if its token matches the one issued to
    /// the pane it claims to be — so a process in one pane can't spoof another
    /// pane's status/session (they share the socket path via `ROOST_SOCK`).
    tokens: HashMap<PaneId, String>,
    /// The fleet control-interface token. Written to `<state>/control.token`
    /// (0600) and NEVER placed in any pane's environment, so only a deliberately
    /// authorized external client can drive panes it doesn't own.
    control_token: String,
    /// Parked `wait` requests, polled each event-loop iteration.
    waiters: Vec<Waiter>,
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
        tx: SyncSender<AppEvent>,
        term_size: Size,
        sock_path: Option<PathBuf>,
    ) -> Result<Self> {
        // A loaded workspace.json may be hand-edited, partially migrated, or
        // otherwise inconsistent — repair layout ↔ panes before spawning.
        let mut ws = ws;
        ws.validate_and_repair();
        // The fleet control token authorizes driving the whole workspace, so it
        // must be genuinely unpredictable — refuse to start rather than fall
        // back to a weak (time-seeded) secret if the CSPRNG is unavailable.
        let control_token = gen_secret()
            .ok_or_else(|| anyhow::anyhow!("cannot read /dev/urandom for the control token"))?;
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
            started: Instant::now(),
            alt_seen: false,
            selection: None,
            flash: None,
            undo: Vec::new(),
            confirm_close: None,
            tokens: HashMap::new(),
            control_token,
            waiters: Vec::new(),
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

    /// Record that an Alt-modified key actually arrived, so the startup hint
    /// (below) knows it doesn't need to warn.
    pub fn note_alt_seen(&mut self) {
        self.alt_seen = true;
    }

    /// Stock Terminal.app doesn't send Alt as a modifier until the user turns
    /// on "Use Option as Meta Key" — with it off, every Alt+key roost relies
    /// on silently does nothing, and there's no other signal to tell the user
    /// why. `TERM_PROGRAM` reliably names Terminal.app, so nudge for the first
    /// few seconds unless an Alt key has already gotten through.
    pub fn show_alt_hint(&self) -> bool {
        wants_alt_hint(
            self.alt_seen,
            self.started.elapsed(),
            std::env::var("TERM_PROGRAM").ok().as_deref(),
        )
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

        // Validate a stored session id: only launch fresh + clear it when the
        // session is *definitively* gone. If we can't tell (root momentarily
        // unreadable), attempt resume and keep the id — a transient error must
        // not discard a still-valid resume pointer. All adapter queries happen
        // here, before we borrow self mut.
        let (session, stale) = match &spec.session {
            None => (None, false),
            // A malformed/hostile id (tampered workspace.json, poisoned socket)
            // never reaches the resume command — treat it as gone and launch
            // fresh, clearing it from disk.
            Some(s) if !crate::agents::valid_session_id(s) => (None, true),
            Some(s) => match adapter.session_state(&spec.cwd, s) {
                crate::agents::SessionState::Gone => (None, true),
                _ => (Some(s.clone()), false), // Exists or Unknown → try resume
            },
        };
        let mut cmd = match &session {
            Some(s) => adapter.resume(&spec.cwd, s),
            None => adapter.launch(&spec.cwd),
        };
        if let Some(sock) = &self.sock_path {
            cmd.env.push(("ROOST_SOCK".into(), sock.to_string_lossy().into_owned()));
            // Fresh per-spawn token: the pane authenticates its socket messages
            // with it, and no other pane knows it. Reissued on every (re)spawn.
            let token = gen_token();
            cmd.env.push(("ROOST_TOKEN".into(), token.clone()));
            self.tokens.insert(id, token);
        }
        let wants_detect = session.is_none() && adapter.session_root(&spec.cwd).is_some();
        // adapter / registry borrow ends here.

        if stale {
            // Persist the correction so the dead id isn't retried next launch.
            if let Some(s) = self.find_spec_mut(id) {
                s.session = None;
            }
            self.save();
        }

        let (rows, cols) = inner_dims(rect);
        match B::spawn(id, &cmd, rows, cols, self.tx.clone()) {
            Ok(rt) => {
                self.runtimes.insert(id, rt);
                self.dead.remove(&id);
                // Owe this pane a session id? Watch for one (socket reports
                // it exactly; the filesystem scan in tick() is the fallback).
                if wants_detect {
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
        if self.last_detect.elapsed() < DETECT_INTERVAL {
            return;
        }
        self.last_detect = Instant::now();
        // Persist what each pane is actually running (live cwd, typed agent).
        self.observe_panes();
        if self.pending_detect.is_empty() {
            return;
        }
        let mut pending: Vec<(PaneId, SystemTime)> =
            self.pending_detect.iter().map(|(k, v)| (*k, *v)).collect();
        // Newest spawn first: two panes launched into the same cwd share one
        // session root, and `detect_session` just grabs the newest unclaimed
        // file in its window. Processing oldest-first let an earlier pane's
        // wider window see (and steal) a later pane's not-yet-claimed file,
        // starving that pane of a session id forever (HashMap iteration order
        // made this non-deterministic). Claiming newest-spawned-first mirrors
        // file-creation order, so each pane gets its own file.
        pending.sort_by(|a, b| b.1.cmp(&a.1));
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

    /// Persist what each pane is *actually* running — its live working
    /// directory (after `cd`) and any known agent CLI started inside it
    /// (typed `pi` at a shell prompt, not just picker-launched) — so a
    /// restart brings back reality, not merely what roost first launched.
    /// A backend that can't inspect its process returns None and is left
    /// untouched (so a momentarily-unreadable pane is never clobbered).
    fn observe_panes(&mut self) {
        let known: Vec<String> =
            self.registry.keys().filter(|k| **k != "shell").map(|k| k.to_string()).collect();
        if known.is_empty() {
            return;
        }
        let observations: Vec<(PaneId, Observation)> = self
            .runtimes
            .iter()
            .filter_map(|(id, rt)| rt.observe(&known).map(|o| (*id, o)))
            .collect();

        let mut dirty = false;
        let mut promoted: Vec<PaneId> = Vec::new();
        for (id, o) in observations {
            let Some(spec) = self.find_spec_mut(id) else { continue };
            if let Some(cwd) = o.cwd {
                if spec.cwd != cwd {
                    spec.cwd = cwd;
                    dirty = true;
                }
            }
            // Reflect the running agent: promote a shell that's now running pi
            // to the pi adapter; demote back to shell when the agent exits.
            let want = o.agent.unwrap_or_else(|| "shell".to_string());
            if spec.adapter != want {
                let demoting = want == "shell";
                spec.adapter = want;
                // Keep spec.session even when demoting to shell. A single missed
                // observation (a transient argv miss, a subprocess reparent, the
                // agent's startup window) must not destroy the resume pointer —
                // that's the H1-class data-loss path on a different route. The
                // shell adapter simply ignores a stored session; if the pane is
                // re-promoted to the agent, the id is still there to resume.
                if !demoting {
                    promoted.push(id);
                }
                dirty = true;
            }
        }
        // A newly-recognized agent needs its already-created session file
        // located; a wide window (epoch) plus the taken-set finds it without
        // cross-wiring against other panes.
        for id in promoted {
            self.pending_detect.entry(id).or_insert(SystemTime::UNIX_EPOCH);
        }
        if dirty {
            self.save();
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

    /// One-glyph summary of a tab's panes for the tab bar. Background tabs are
    /// spawned lazily (only when first visited), so their panes have no runtime
    /// and no known status — we report `Unknown` for those rather than letting
    /// the tab look idle/quiet, which would be a lie. A pane that's neither
    /// running nor a recorded spawn-failure is "not spawned yet".
    pub fn tab_summary(&self, tab_index: usize) -> TabSummary {
        let Some(tab) = self.ws.tabs.get(tab_index) else { return TabSummary::Quiet };
        let mut any_unknown = false;
        let (mut needs, mut working, mut waiting) = (false, false, false);
        for id in tab.panes.keys() {
            match self.runtimes.get(id) {
                Some(rt) => match rt.status() {
                    AgentStatus::NeedsInput => needs = true,
                    AgentStatus::Working => working = true,
                    AgentStatus::Waiting => waiting = true,
                    _ => {}
                },
                // No runtime and not a known spawn-failure ⇒ not spawned yet.
                None if !self.dead.contains_key(id) => any_unknown = true,
                None => {}
            }
        }
        if needs {
            TabSummary::NeedsInput // a real, actionable signal wins outright
        } else if any_unknown {
            TabSummary::Unknown // honest: we haven't run these panes
        } else if working {
            TabSummary::Working
        } else if waiting {
            TabSummary::Waiting
        } else {
            TabSummary::Quiet
        }
    }

    /// Whether the focused pane negotiated the kitty keyboard protocol, so the
    /// input layer knows whether to send modified Enter as CSI-u or the legacy
    /// fallback.
    pub fn focused_kitty(&self) -> bool {
        self.runtimes.get(&self.focused).map(|rt| rt.kitty_disambiguate()).unwrap_or(false)
    }

    /// Is a socket message claiming to be `id` carrying that pane's token?
    /// Guards session/status updates against cross-pane spoofing over the
    /// shared socket. Fails closed: unknown pane or missing token → rejected.
    pub fn socket_authorized(&self, id: PaneId, token: &str) -> bool {
        !token.is_empty() && self.tokens.get(&id).map(|t| t == token).unwrap_or(false)
    }

    // -- control interface -------------------------------------------------

    /// The fleet control token (written to `<state>/control.token` by startup).
    pub fn control_token(&self) -> &str {
        &self.control_token
    }

    /// Resolve a token to the caller it represents: the fleet control token, or
    /// a pane acting via its own `ROOST_TOKEN`. Fails closed on empty/unknown.
    fn resolve_actor(&self, token: &str) -> Option<Actor> {
        if token.is_empty() {
            return None;
        }
        if token == self.control_token {
            return Some(Actor::Fleet);
        }
        self.tokens.iter().find(|(_, t)| t.as_str() == token).map(|(id, _)| Actor::Pane(*id))
    }

    /// Is `node` `ancestor`, or somewhere in `ancestor`'s spawned subtree?
    fn in_subtree(&self, ancestor: PaneId, node: PaneId) -> bool {
        let mut cur = Some(node);
        let mut hops = 0;
        while let Some(id) = cur {
            if id == ancestor {
                return true;
            }
            cur = self.find_spec(id).and_then(|s| s.spawned_by);
            hops += 1;
            if hops > 4096 {
                break; // cycle guard
            }
        }
        false
    }

    /// May `actor` act on `target`? Fleet may act on any pane; a pane may act
    /// only within its own spawned subtree (itself included).
    fn may_target(&self, actor: Actor, target: PaneId) -> bool {
        match actor {
            Actor::Fleet => true,
            Actor::Pane(a) => self.in_subtree(a, target),
        }
    }

    fn tab_of(&self, id: PaneId) -> Option<usize> {
        self.ws.tabs.iter().position(|t| t.panes.contains_key(&id))
    }

    fn status_str(&self, id: PaneId) -> &'static str {
        if let Some(rt) = self.runtimes.get(&id) {
            match rt.status() {
                AgentStatus::Working => "working",
                AgentStatus::NeedsInput => "needs_input",
                AgentStatus::Waiting => "waiting",
                AgentStatus::Idle => "idle",
                AgentStatus::Exited => "exited",
            }
        } else if self.dead.contains_key(&id) || self.find_spec(id).is_none() {
            // Spawn failed, or the pane is closed/never existed — either way
            // nothing further will happen to it.
            "exited"
        } else {
            "unknown" // a background pane not spawned yet (lazy)
        }
    }

    /// Execute a control request synchronously: authorize, then dispatch.
    /// (`wait` is asynchronous and goes through `handle_control_msg`.) The
    /// socket path uses `handle_control_msg`; this is the direct/in-process
    /// entry, exercised by the unit tests.
    #[allow(dead_code)]
    pub fn handle_control(&mut self, req: Request) -> Reply {
        match self.resolve_actor(&req.token) {
            Some(actor) => self.dispatch(actor, req.method),
            None => Reply::err("unauthorized: unknown or missing token"),
        }
    }

    fn dispatch(&mut self, actor: Actor, method: Method) -> Reply {
        match method {
            Method::List => self.ctl_list(actor),
            Method::Status { pane } => self.ctl_status(actor, pane),
            Method::Spawn { adapter, cwd, initial_input } => {
                self.ctl_spawn(actor, &adapter, cwd, initial_input)
            }
            Method::Fork { pane } => self.ctl_fork(actor, pane),
            Method::Send { pane, text, submit } => self.ctl_send(actor, pane, &text, submit),
            Method::Read { pane, mode } => self.ctl_read(actor, pane, mode),
            Method::Close { pane, force } => self.ctl_close(actor, pane, force),
            // `wait` is handled asynchronously; only reached if a caller sends
            // it down the synchronous path.
            Method::Wait { .. } => Reply::err("wait is asynchronous; issue it over the socket"),
        }
    }

    /// Socket entry point. Handles the asynchronous `wait` (parks a waiter and
    /// replies later) and delegates every other verb to synchronous dispatch.
    pub fn handle_control_msg(&mut self, req: Request, reply: Sender<Reply>) {
        let actor = self.resolve_actor(&req.token);
        let summary = method_summary(&req.method);
        match (actor, req.method) {
            (None, _) => {
                self.audit(None, &summary, false, "unauthorized");
                let _ = reply.send(Reply::err("unauthorized: unknown or missing token"));
            }
            (Some(actor), Method::Wait { panes, until, timeout_ms }) => {
                // Audit the real outcome, not an assumed "parked" — a
                // rejected wait (bad pane, forbidden, at the concurrency cap)
                // never parks, and must show up as a denial, not a success
                // (M3).
                match self.register_waiter(actor, panes, &until, timeout_ms, reply) {
                    Ok(outcome) => self.audit(Some(actor), &summary, true, outcome),
                    Err(reason) => self.audit(Some(actor), &summary, false, &reason),
                }
            }
            (Some(actor), method) => {
                let r = self.dispatch(actor, method);
                let (ok, detail) = match &r {
                    Reply::Ok { .. } => (true, String::new()),
                    Reply::Err { err } => (false, err.clone()),
                };
                self.audit(Some(actor), &summary, ok, &detail);
                let _ = reply.send(r);
            }
        }
    }

    /// Append a control action to `<state>/control.log` (unconditional — every
    /// spawn/send/read/close/etc. that touches the fleet is recorded with who
    /// did it, what, and the outcome). No-op when there's no socket dir.
    fn audit(&self, actor: Option<Actor>, summary: &str, ok: bool, detail: &str) {
        let Some(dir) = self.sock_path.as_ref().and_then(|p| p.parent()) else { return };
        let principal = match actor {
            Some(Actor::Fleet) => "fleet".to_string(),
            Some(Actor::Pane(id)) => format!("pane:{id}"),
            None => "?".to_string(),
        };
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let outcome = if ok { "ok" } else { "err" };
        let line =
            format!("{ts} {principal} {} -> {outcome} {}\n", sanitize(summary), sanitize(detail));
        use std::io::Write;
        if let Ok(mut f) =
            std::fs::OpenOptions::new().create(true).append(true).open(dir.join("control.log"))
        {
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// Does pane `id` currently satisfy `until`? A pane with a runtime
    /// matches on an exact status comparison. One with none is either lazy
    /// (present in the workspace, just not spawned yet — still going to run,
    /// so it never self-resolves a wait) or terminal (its spawn failed, or
    /// it's gone from the workspace entirely) — which can't reach any
    /// further status, so it satisfies any `until` right away instead of
    /// blocking a parked wait to its deadline (M2).
    fn pane_matches(&self, id: PaneId, until: AgentStatus) -> bool {
        match self.runtimes.get(&id) {
            Some(rt) => rt.status() == until,
            None => self.dead.contains_key(&id) || self.find_spec(id).is_none(),
        }
    }

    /// Validate and register a `wait`. The reply is always sent from here —
    /// immediately if already satisfied, rejected, or over the parked-wait
    /// cap, later from `poll_waiters` otherwise — the return value only tells
    /// the caller what really happened, so it can audit that instead of
    /// assuming success (M3).
    fn register_waiter(
        &mut self,
        actor: Actor,
        panes: Vec<PaneId>,
        until: &str,
        timeout_ms: Option<u64>,
        reply: Sender<Reply>,
    ) -> Result<&'static str, String> {
        if panes.is_empty() {
            let msg = "wait needs at least one pane";
            let _ = reply.send(Reply::err(msg));
            return Err(msg.into());
        }
        let Some(until) = crate::core::control::parse_status(until) else {
            let msg = "unknown status; use working|needs_input|waiting|idle|exited";
            let _ = reply.send(Reply::err(msg));
            return Err(msg.into());
        };
        for &p in &panes {
            if self.find_spec(p).is_none() {
                let msg = format!("no such pane: {p}");
                let _ = reply.send(Reply::err(msg.clone()));
                return Err(msg);
            }
            if !self.may_target(actor, p) {
                let msg = "forbidden: pane not in your subtree";
                let _ = reply.send(Reply::err(msg));
                return Err(msg.into());
            }
        }
        // Already satisfied → reply immediately.
        if let Some(id) = panes.iter().copied().find(|&p| self.pane_matches(p, until)) {
            let _ = reply
                .send(Reply::ok(serde_json::json!({ "pane": id, "status": self.status_str(id) })));
            return Ok("immediate");
        }
        // Cap concurrently parked waiters well below the socket's global
        // connection limit: rejecting here closes the connection right away,
        // freeing its slot back to the pool instead of holding it for the
        // full timeout (see MAX_WAITS).
        if self.waiters.len() >= MAX_WAITS {
            let msg = "too many concurrent waits";
            let _ = reply.send(Reply::err(msg));
            return Err(msg.into());
        }
        // Default 5 min, capped at 24 h, so a parked reply can't live forever.
        let ms = timeout_ms.unwrap_or(300_000).min(24 * 3600 * 1000);
        let deadline = Instant::now() + Duration::from_millis(ms);
        self.waiters.push(Waiter { panes, until, reply, deadline });
        Ok("parked")
    }

    /// Fire any parked `wait` whose condition is met (or which timed out).
    /// Called every event-loop iteration; cheap when there are no waiters.
    pub fn poll_waiters(&mut self) {
        if self.waiters.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut i = 0;
        while i < self.waiters.len() {
            let w = &self.waiters[i];
            let hit = w.panes.iter().copied().find(|&p| self.pane_matches(p, w.until));
            let timed_out = now >= w.deadline;
            if let Some(id) = hit {
                let status = self.status_str(id);
                let w = self.waiters.remove(i);
                let _ = w.reply.send(Reply::ok(serde_json::json!({ "pane": id, "status": status })));
            } else if timed_out {
                let w = self.waiters.remove(i);
                let _ = w.reply.send(Reply::ok(serde_json::json!({ "timed_out": true })));
            } else {
                i += 1;
            }
        }
    }

    fn pane_json(&self, id: PaneId, tab: usize) -> serde_json::Value {
        let spec = self.find_spec(id);
        serde_json::json!({
            "pane": id,
            "tab": tab,
            "adapter": spec.map(|s| s.adapter.clone()),
            "cwd": spec.map(|s| s.cwd.to_string_lossy().into_owned()),
            "title": spec.and_then(|s| s.title.clone()),
            "session": spec.and_then(|s| s.session.clone()),
            "spawned_by": spec.and_then(|s| s.spawned_by),
            "status": self.status_str(id),
            "focused": id == self.focused,
        })
    }

    fn ctl_list(&self, actor: Actor) -> Reply {
        let visible: Vec<(PaneId, usize)> = self
            .ws
            .tabs
            .iter()
            .enumerate()
            .flat_map(|(ti, tab)| tab.panes.keys().map(move |id| (*id, ti)))
            .filter(|(id, _)| self.may_target(actor, *id))
            .collect();
        let arr: Vec<_> = visible.into_iter().map(|(id, ti)| self.pane_json(id, ti)).collect();
        Reply::ok(serde_json::json!(arr))
    }

    fn ctl_status(&self, actor: Actor, pane: Option<PaneId>) -> Reply {
        match pane {
            Some(p) => {
                if self.find_spec(p).is_none() {
                    return Reply::err("no such pane");
                }
                if !self.may_target(actor, p) {
                    return Reply::err("forbidden: pane not in your subtree");
                }
                Reply::ok(serde_json::json!({ "pane": p, "status": self.status_str(p) }))
            }
            None => self.ctl_list(actor),
        }
    }

    fn ctl_spawn(
        &mut self,
        actor: Actor,
        adapter: &str,
        cwd: Option<String>,
        initial_input: Option<String>,
    ) -> Reply {
        if self.registry.get(adapter).is_none() {
            return Reply::err(format!("unknown adapter: {adapter}"));
        }
        let owner = match actor {
            Actor::Fleet => None,
            Actor::Pane(a) => Some(a),
        };
        // spawn_child (shared with the interactive Alt+n path) splits off
        // self.focused and moves focus to the new pane — fine for a human
        // keystroke, but the control API must never steal the human's focus
        // or jump their active tab out from under them (DESIGN-control
        // §5.2). Save + restore around the call; the new pane is still
        // created, spawned, and its id returned either way.
        let (focused, active_tab) = (self.focused, self.ws.active_tab);
        let id = self.spawn_child(adapter, cwd.map(PathBuf::from), owner);
        self.focused = focused;
        self.ws.active_tab = active_tab;
        let Some(id) = id else {
            return Reply::err("spawn refused: not enough room to split");
        };
        if let Some(text) = initial_input {
            let mut bytes = text.into_bytes();
            bytes.push(b'\r');
            if let Some(rt) = self.runtimes.get_mut(&id) {
                rt.write_input(&bytes);
            }
        }
        self.relayout();
        self.save();
        Reply::ok(serde_json::json!({ "pane": id }))
    }

    fn ctl_fork(&mut self, actor: Actor, pane: Option<PaneId>) -> Reply {
        let target = match (pane, actor) {
            (Some(p), _) => p,
            (None, Actor::Pane(a)) => a,
            (None, Actor::Fleet) => return Reply::err("fork requires a pane id for a fleet caller"),
        };
        if !self.may_target(actor, target) {
            return Reply::err("forbidden: pane not in your subtree");
        }
        let Some(spec) = self.find_spec(target).cloned() else {
            return Reply::err("no such pane");
        };
        let owner = match actor {
            Actor::Fleet => None,
            Actor::Pane(a) => Some(a),
        };
        // Same adapter + cwd. Session-branching (a true fork of the agent's
        // conversation) lands with the bidirectional pi extension; for now this
        // opens a fresh sibling in the same context.
        // See ctl_spawn: the control path must never steal the human's focus
        // or active tab.
        let (focused, active_tab) = (self.focused, self.ws.active_tab);
        let id = self.spawn_child(&spec.adapter, Some(spec.cwd), owner);
        self.focused = focused;
        self.ws.active_tab = active_tab;
        let Some(id) = id else {
            return Reply::err("fork refused: not enough room to split");
        };
        self.relayout();
        self.save();
        Reply::ok(serde_json::json!({ "pane": id }))
    }

    fn ctl_send(&mut self, actor: Actor, pane: PaneId, text: &str, submit: bool) -> Reply {
        if self.find_spec(pane).is_none() {
            return Reply::err("no such pane");
        }
        if !self.may_target(actor, pane) {
            return Reply::err("forbidden: pane not in your subtree");
        }
        let Some(rt) = self.runtimes.get_mut(&pane) else {
            return Reply::err("pane is not running");
        };
        let mut bytes = text.as_bytes().to_vec();
        if submit {
            bytes.push(b'\r');
        }
        rt.write_input(&bytes);
        Reply::ok(serde_json::json!({ "sent": bytes.len() }))
    }

    fn ctl_read(&self, actor: Actor, pane: PaneId, mode: ReadMode) -> Reply {
        if self.find_spec(pane).is_none() {
            return Reply::err("no such pane");
        }
        if !self.may_target(actor, pane) {
            return Reply::err("forbidden: pane not in your subtree");
        }
        let Some(rt) = self.runtimes.get(&pane) else {
            return Reply::err("pane is not running");
        };
        // grab_text clamps to the screen, so (0,0)..MAX is the whole grid.
        let full = rt.grab_text((0, 0), (u16::MAX, u16::MAX));
        let text = match mode {
            ReadMode::Screen | ReadMode::Full => full,
            ReadMode::Tail(n) => {
                let lines: Vec<&str> = full.lines().filter(|l| !l.trim().is_empty()).collect();
                let start = lines.len().saturating_sub(n);
                lines[start..].join("\n")
            }
        };
        Reply::ok(serde_json::json!({ "pane": pane, "text": text }))
    }

    fn ctl_close(&mut self, actor: Actor, pane: PaneId, force: bool) -> Reply {
        if self.find_spec(pane).is_none() {
            return Reply::err("no such pane");
        }
        if !self.may_target(actor, pane) {
            return Reply::err("forbidden: pane not in your subtree");
        }
        // The API must never quit roost by closing its last pane.
        if self.ws.tabs.len() == 1 && self.ws.active_tab().panes.len() == 1 {
            return Reply::err("cannot close the last pane via the control interface");
        }
        let working =
            self.runtimes.get(&pane).map(|rt| rt.status() == AgentStatus::Working).unwrap_or(false);
        if working && !force {
            return Reply::err("pane is working; pass force to close it");
        }
        self.close_pane_id(pane);
        self.relayout();
        self.save();
        Reply::ok(serde_json::json!({ "closed": pane }))
    }

    /// Close a specific pane (any tab) — the single removal path shared by
    /// the control interface and the interactive close (`close_pane`, which
    /// wraps this with its confirm/quit handling). Captures it for undo,
    /// never quits roost, and keeps the human's on-screen tab and focus
    /// consistent even when the pane it removes isn't either of those.
    fn close_pane_id(&mut self, id: PaneId) -> bool {
        let Some(ti) = self.tab_of(id) else { return false };
        let spec = self.ws.tabs[ti].panes.get(&id).cloned();
        let tab_snapshot = self.ws.tabs[ti].clone();
        if let Some(mut rt) = self.runtimes.remove(&id) {
            rt.kill();
        }
        self.tokens.remove(&id);
        // Drop any spawn-error record for this pane too; otherwise a pane that
        // failed to spawn and is then closed leaves a stale `dead` entry that
        // never gets cleaned (pane ids are not reused for it).
        self.dead.remove(&id);
        let tab = &mut self.ws.tabs[ti];
        tab.panes.remove(&id);
        let empty = layout::remove_pane(&mut tab.layout, id);
        if empty && self.ws.tabs.len() > 1 {
            self.ws.tabs.remove(ti);
            // A tab removed *before* the active one shifts every later index
            // down by one; adjust so the human's on-screen tab doesn't
            // silently change underneath them (regression: the renderer kept
            // showing the old active-tab index while focus pointed at a pane
            // that had shifted into a different, off-screen tab). Removing
            // the active tab itself, or one after it, needs only the usual
            // out-of-range clamp.
            if ti < self.ws.active_tab {
                self.ws.active_tab -= 1;
            } else if self.ws.active_tab >= self.ws.tabs.len() {
                self.ws.active_tab = self.ws.tabs.len().saturating_sub(1);
            }
            self.remember_closed(Closed::Tab { index: ti, tab: tab_snapshot });
            self.spawn_active_tab();
        } else if !empty {
            if let Some(spec) = spec {
                self.remember_closed(Closed::Pane { tab_index: ti, spec });
            }
        }
        // The active tab's membership may have changed out from under
        // `focused` (its pane closed, or its tab shifted/removed above) —
        // keep focus inside whatever tab is now on screen rather than
        // routing keystrokes to a pane in a tab nobody's looking at.
        if !self.ws.active_tab().panes.contains_key(&self.focused) {
            self.focused = self.pane_order().first().copied().unwrap_or(0);
        }
        true
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

    /// Returns a notification message when a *non-focused* pane exits — an
    /// exited pane is as attention-worthy as one needing input, but its
    /// recovery hint is only visible inside its own borders, so it otherwise
    /// gets no pull toward it (regression: same fix as `on_status`).
    pub fn on_pty_exit(&mut self, id: PaneId) -> Option<String> {
        if let Some(rt) = self.runtimes.get_mut(&id) {
            rt.on_exit();
        }
        if id == self.focused {
            return None;
        }
        // A pane the user just closed (Alt+w) is already gone from the
        // workspace by the time its process EOFs — that Exit is expected, not
        // attention-worthy. Only nudge for a still-present, unfocused pane
        // that exited on its own (its recovery hint is hidden in its borders).
        let spec = self.find_spec(id)?;
        let name = spec.title.clone().unwrap_or_else(|| spec.adapter.clone());
        Some(format!("{name} exited"))
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
        // NeedsInput is an explicit "I need you" and always pulls attention;
        // Waiting is softer (turn ended) — only notify when it follows active
        // work, so a resume that lands straight on Waiting doesn't nag.
        let became_needy = match status {
            AgentStatus::NeedsInput => true,
            AgentStatus::Waiting => prev == Some(AgentStatus::Working),
            _ => false,
        };
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

    // -- copy mode / selection --------------------------------------------

    pub fn in_copy_mode(&self) -> bool {
        matches!(self.mode, Mode::Copy)
    }

    /// Start a selection in pane `id` at inner cell (row, col).
    pub fn begin_selection(&mut self, id: PaneId, row: u16, col: u16) {
        self.selection = Some(Selection { pane: id, anchor: (row, col), cursor: (row, col), dragging: true });
    }

    /// Extend the active drag to inner cell (row, col).
    pub fn extend_selection(&mut self, row: u16, col: u16) {
        if let Some(sel) = &mut self.selection {
            if sel.dragging {
                sel.cursor = (row, col);
            }
        }
    }

    /// Finish the drag: extract the selected text, set a "copied" flash, and
    /// leave copy mode. Returns the text to hand to the clipboard (None when
    /// the selection is empty).
    pub fn finish_selection(&mut self) -> Option<String> {
        let sel = self.selection.as_mut()?;
        sel.dragging = false;
        let (pane, anchor, cursor) = (sel.pane, sel.anchor, sel.cursor);
        let text = self.runtimes.get(&pane).map(|rt| rt.grab_text(anchor, cursor)).unwrap_or_default();
        self.mode = Mode::Normal;
        self.selection = None;
        if text.is_empty() {
            return None;
        }
        self.flash =
            Some((format!("copied {} chars", text.chars().count()), Instant::now()));
        Some(text)
    }

    /// Set a transient hint-bar message (e.g. a startup notice).
    pub fn set_flash(&mut self, msg: impl Into<String>) {
        self.flash = Some((msg.into(), Instant::now()));
    }

    /// Current transient hint-bar message, if still within its window.
    pub fn flash(&self) -> Option<&str> {
        self.flash
            .as_ref()
            .filter(|(_, at)| at.elapsed() < FLASH_WINDOW)
            .map(|(m, _)| m.as_str())
    }

    /// The URL under inner cell (row, col) of pane `id`, if any (for
    /// Alt+click-to-open). Reads that row's text from the pane grid.
    pub fn url_at(&self, id: PaneId, row: u16, col: u16) -> Option<String> {
        let line = self.runtimes.get(&id)?.grab_text((row, 0), (row, u16::MAX));
        find_url_at(&line, col as usize)
    }

    // -- mouse -------------------------------------------------------------

    /// Left click: focus the pane under the cursor (expanding stack members).
    pub fn on_click(&mut self, id: PaneId) {
        self.focused = id;
        layout::expand_in_stacks(&mut self.ws.active_tab_mut().layout, id);
        self.relayout();
        self.save();
    }

    /// Forward an encoded mouse event (wheel / click / drag) to a mouse-aware
    /// pane app.
    pub fn forward_mouse(&mut self, id: PaneId, bytes: &[u8]) {
        if let Some(rt) = self.runtimes.get_mut(&id) {
            // Not write_input(): a forwarded mouse event must not snap the
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
            Action::FlipSplit => {
                let focused = self.focused;
                layout::flip_split(&mut self.ws.active_tab_mut().layout, focused);
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
            Action::CopyMode => {
                self.mode = Mode::Copy;
                self.selection = None;
            }
            Action::ToggleHints => self.hints = !self.hints,
            Action::Undo => self.undo_close(),
            Action::Help => self.mode = Mode::Help,
        }
        // Any action other than a repeated close disarms a pending close
        // confirmation, so a stale "press again" can't leak onto a later key.
        if !matches!(action, Action::ClosePane) {
            self.confirm_close = None;
        }
        self.relayout();
        self.save();
    }

    /// Push a closed pane/tab onto the bounded undo stack.
    fn remember_closed(&mut self, closed: Closed) {
        self.undo.push(closed);
        if self.undo.len() > UNDO_DEPTH {
            self.undo.remove(0);
        }
    }

    /// Reopen the most recently closed pane or tab, resuming its session.
    fn undo_close(&mut self) {
        let Some(closed) = self.undo.pop() else {
            self.flash = Some(("nothing to reopen".into(), Instant::now()));
            return;
        };
        match closed {
            Closed::Tab { index, tab } => {
                let i = index.min(self.ws.tabs.len());
                self.ws.tabs.insert(i, tab);
                self.ws.active_tab = i;
                self.spawn_active_tab();
                self.focused = self.pane_order().first().copied().unwrap_or(0);
                self.flash = Some(("reopened tab".into(), Instant::now()));
            }
            Closed::Pane { tab_index, spec } => {
                // Restore into its original tab if it still exists, else the
                // active one; split the focused pane and reuse the saved spec
                // (session id preserved ⇒ the agent resumes).
                self.ws.active_tab = tab_index.min(self.ws.tabs.len().saturating_sub(1));
                self.focused = self.pane_order().first().copied().unwrap_or(0);
                self.restore_pane(spec);
                self.flash = Some(("reopened pane".into(), Instant::now()));
            }
        }
    }

    /// Insert `spec` as a new pane split off the focused pane, spawning it.
    /// Shared by undo (reuses a saved spec, session and all).
    fn restore_pane(&mut self, spec: PaneSpec) {
        let id = self.ws.next_pane_id();
        let focused = self.focused;
        let dir = self
            .rects()
            .iter()
            .find(|pr| pr.id == focused)
            .map(|pr| {
                if pr.rect.width >= pr.rect.height * 3 {
                    SplitDir::Vertical
                } else {
                    SplitDir::Horizontal
                }
            })
            .unwrap_or(SplitDir::Vertical);
        let tab = self.ws.active_tab_mut();
        tab.panes.insert(id, spec.clone());
        if !layout::split_pane(&mut tab.layout, focused, id, dir) {
            tab.layout = LayoutNode::Pane(id);
        }
        self.focused = id;
        if let Some(pr) = self.rects().iter().find(|pr| pr.id == id).copied() {
            self.spawn_pane(id, &spec, pr.rect);
        }
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
        self.spawn_child(adapter, None, None);
    }

    /// Split the focused pane and spawn a new one running `adapter`. `cwd`
    /// overrides the inherited working directory; `spawned_by` records the
    /// owner for the control-interface capability model. Returns the new pane
    /// id, or None if the split was refused (pane too small).
    fn spawn_child(
        &mut self,
        adapter: &str,
        cwd: Option<PathBuf>,
        spawned_by: Option<PaneId>,
    ) -> Option<PaneId> {
        let id = self.ws.next_pane_id();
        let cwd = cwd.unwrap_or_else(|| {
            self.ws
                .active_tab()
                .panes
                .get(&self.focused)
                .map(|s| s.cwd.clone())
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
        });
        let spec = PaneSpec { adapter: adapter.into(), cwd, session: None, title: None, spawned_by };

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
                return None;
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
        Some(id)
    }

    fn close_pane(&mut self) {
        let id = self.focused;

        // Destructive-close guard. Closing a *busy* agent loses its in-flight
        // turn, and closing the last pane quits roost outright (which undo
        // can't recover). In those cases, arm a confirmation and require a
        // second Alt+w within the window; a non-busy pane closes immediately
        // (undo covers an accidental one).
        let is_working =
            self.runtimes.get(&id).map(|rt| rt.status() == AgentStatus::Working).unwrap_or(false);
        let would_quit = self.ws.tabs.len() == 1 && self.ws.active_tab().panes.len() == 1;
        let armed = self.confirm_close.is_some_and(|t| t.elapsed() < CONFIRM_WINDOW);
        if (is_working || would_quit) && !armed {
            self.confirm_close = Some(Instant::now());
            let msg = if would_quit {
                "last pane — Alt+w again to quit roost"
            } else {
                "agent busy — Alt+w again to close"
            };
            self.flash = Some((msg.into(), Instant::now()));
            return;
        }
        self.confirm_close = None;

        // The actual removal (kill the runtime, capture undo, fix up
        // tab/focus bookkeeping) is close_pane_id's job — shared with the
        // control interface. This wrapper only adds the confirm guard above
        // and the quit flag below: closing the very last pane exits roost
        // outright, so (unlike every other close) there's nothing to reopen.
        self.close_pane_id(id);
        if would_quit {
            self.quit = true;
        }
    }

    fn new_tab(&mut self) {
        let id = self.ws.next_pane_id();
        let cwd = std::env::current_dir().unwrap_or_default();
        let mut panes = HashMap::new();
        panes.insert(id, PaneSpec { adapter: "shell".into(), cwd, session: None, title: None, spawned_by: None });
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
        // anywhere), so any mode yields to them. If we were scrolling, snap the
        // pane back to its live tail first — otherwise moving focus (Alt+arrow)
        // would leave the pane you were reading frozen mid-history while scroll
        // keys silently drive a different pane.
        if key.modifiers.contains(crossterm::event::KeyModifiers::ALT) {
            if matches!(self.mode, Mode::Scroll { .. }) {
                let focused = self.focused;
                if let Some(rt) = self.runtimes.get_mut(&focused) {
                    rt.set_scrollback(0);
                }
            }
            self.mode = Mode::Normal;
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
                let items = picker_items();
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        *selection = selection.checked_sub(1).unwrap_or(items.len() - 1)
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        *selection = (*selection + 1) % items.len()
                    }
                    KeyCode::Enter => {
                        let adapter = items[(*selection).min(items.len() - 1)];
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
            Mode::Copy => {
                // Selection is mouse-driven; keys just exit.
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
                    self.mode = Mode::Normal;
                    self.selection = None;
                }
                true
            }
            Mode::Help => {
                // Any key dismisses the keymap overlay.
                self.mode = Mode::Normal;
                true
            }
        }
    }

    /// Clean shutdown: save workspace, kill children (their sessions live on).
    pub fn shutdown(&mut self) {
        self.save();
        // Graceful stop: SIGHUP everything (agents flush their final turn like
        // a closed terminal would allow), a short grace window, then the
        // guaranteed SIGKILL + reap for anything that ignored the hangup.
        if self.runtimes.is_empty() {
            return;
        }
        for rt in self.runtimes.values_mut() {
            rt.hangup();
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        for rt in self.runtimes.values_mut() {
            rt.kill();
        }
    }
}

/// A fresh, unguessable per-pane socket token. 16 bytes from /dev/urandom,
/// hex-encoded. The socket is already owner-only (0600); this token is the
/// extra guard against a process in one pane spoofing another pane over the
/// shared socket, so it needs to be unpredictable to a sibling pane.
/// A compact, log-safe summary of a control verb + its target. Deliberately
/// omits `send` text (may contain secrets) — records only its length.
fn method_summary(m: &Method) -> String {
    let opt = |p: &Option<PaneId>| p.map(|p| p.to_string()).unwrap_or_else(|| "all".into());
    match m {
        Method::List => "list".into(),
        Method::Status { pane } => format!("status pane={}", opt(pane)),
        Method::Spawn { adapter, .. } => format!("spawn adapter={adapter}"),
        Method::Fork { pane } => format!("fork pane={}", opt(pane)),
        Method::Send { pane, text, submit } => {
            format!("send pane={pane} len={} submit={submit}", text.len())
        }
        Method::Read { pane, .. } => format!("read pane={pane}"),
        Method::Close { pane, force } => format!("close pane={pane} force={force}"),
        Method::Wait { panes, until, .. } => format!("wait panes={panes:?} until={until}"),
    }
}

/// Neutralize a string before it's written to `control.log`: CR/LF (or any
/// ASCII control char) in an attacker-controlled value — an adapter name, a
/// `wait` `until`, an error detail — could otherwise forge a fake extra log
/// line attributed to whoever made the real call. One entry stays one line.
fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_control() { ' ' } else { c }).collect()
}

/// 16 CSPRNG bytes from /dev/urandom, hex-encoded. `None` if urandom is
/// unreadable — the caller decides whether that's fatal.
fn gen_secret() -> Option<String> {
    use std::io::Read;
    let mut buf = [0u8; 16];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)).ok()?;
    let mut s = String::with_capacity(32);
    for b in buf {
        s.push_str(&format!("{b:02x}"));
    }
    Some(s)
}

/// A per-pane status token. Unlike the fleet control token, a weak fallback is
/// tolerable here: it only authenticates a pane's *own* status/session reports
/// and sits behind the 0600 socket. The control token, which can drive the
/// whole fleet, hard-fails instead (see `App::new`).
fn gen_token() -> String {
    gen_secret().unwrap_or_else(|| {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{n:032x}")
    })
}

/// Find an http(s) URL that covers character index `col` in `line`. The URL
/// is the surrounding non-whitespace run, with wrapping/trailing punctuation
/// stripped. Pure, so it's unit-tested.
pub fn find_url_at(line: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    if col >= chars.len() || chars[col].is_whitespace() {
        return None;
    }
    let mut start = col;
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    let mut end = col;
    while end + 1 < chars.len() && !chars[end + 1].is_whitespace() {
        end += 1;
    }
    let token: String = chars[start..=end].iter().collect();
    // Strip wrapping brackets/quotes and trailing sentence punctuation.
    let trimmed = token.trim_matches(|c: char| "()[]{}<>\"'`.,;:!?".contains(c));
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        Some(trimmed.to_string())
    } else {
        None
    }
}

fn inner_dims(rect: Rect) -> (u16, u16) {
    (rect.height.saturating_sub(2).max(1), rect.width.saturating_sub(2).max(1))
}

/// Pure decision behind `App::show_alt_hint`, split out so it's testable
/// without depending on process env vars or wall-clock time.
fn wants_alt_hint(alt_seen: bool, elapsed: Duration, term_program: Option<&str>) -> bool {
    !alt_seen && elapsed < ALT_HINT_WINDOW && term_program == Some("Apple_Terminal")
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
        let (tx, _rx) = mpsc::sync_channel(64);
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
    fn tab_summary_unknown_for_unspawned_needs_input_wins() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.pane_order()[0];
        // Freshly spawned shell, nothing happening → Quiet (not Unknown).
        assert_eq!(app.tab_summary(0), TabSummary::Quiet);
        // A pane needing input dominates the summary.
        app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::NeedsInput);
        assert_eq!(app.tab_summary(0), TabSummary::NeedsInput);
        // Not spawned (no runtime, no recorded failure) → Unknown, never idle.
        app.runtimes.remove(&id);
        assert_eq!(app.tab_summary(0), TabSummary::Unknown);
    }

    #[test]
    fn undo_reopens_a_closed_pane_with_its_session() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // 2 panes; focus on the new one
        let id = app.focused;
        app.set_session(id, "sess-xyz".into());
        assert_eq!(app.runtimes.len(), 2);
        // A non-busy pane closes immediately (undo covers accidents).
        app.apply(Action::ClosePane);
        assert_eq!(app.runtimes.len(), 1);
        // Undo reopens it, and the restored pane keeps its resume id.
        app.apply(Action::Undo);
        assert_eq!(app.runtimes.len(), 2);
        let restored = app.focused;
        assert_eq!(app.find_spec(restored).unwrap().session.as_deref(), Some("sess-xyz"));
    }

    #[test]
    fn closing_a_busy_pane_needs_a_confirming_second_press() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        let id = app.focused;
        app.on_pty_output(id, b"x"); // FakePane: output ⇒ Working
        assert_eq!(app.runtimes.len(), 2);
        app.apply(Action::ClosePane); // armed, not closed
        assert_eq!(app.runtimes.len(), 2);
        app.apply(Action::ClosePane); // confirmed ⇒ closed
        assert_eq!(app.runtimes.len(), 1);
    }

    #[test]
    fn scrolling_then_a_global_chord_snaps_the_pane_back_to_live() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.apply(Action::ScrollMode);
        app.handle_mode_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)); // scroll back 1
        assert!(matches!(app.mode, Mode::Scroll { .. }));
        assert_eq!(app.runtimes.get(&id).unwrap().scrollback, 1);
        // A global Alt chord (e.g. focus move) exits scroll mode AND resets the
        // pane's scrollback, so it isn't left frozen mid-history.
        let consumed = app.handle_mode_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::ALT));
        assert!(!consumed); // Alt passes through to the global binding
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.runtimes.get(&id).unwrap().scrollback, 0);
    }

    #[test]
    fn undo_reopens_a_closed_tab() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewTab);
        assert_eq!(app.ws.tabs.len(), 2);
        app.apply(Action::ClosePane); // last pane of tab 2 ⇒ tab removed
        assert_eq!(app.ws.tabs.len(), 1);
        app.apply(Action::Undo);
        assert_eq!(app.ws.tabs.len(), 2);
    }

    #[test]
    fn control_fleet_can_spawn_send_read_list_and_close() {
        use crate::core::control::{Method, ReadMode, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        let ct = app.control_token().to_string();
        let ok = |r: Reply| match r {
            Reply::Ok { ok } => ok,
            Reply::Err { err } => panic!("expected ok, got err: {err}"),
        };
        // spawn
        let v = ok(app.handle_control(Request {
            token: ct.clone(),
            method: Method::Spawn { adapter: "shell".into(), cwd: None, initial_input: None },
        }));
        let p = v["pane"].as_u64().unwrap();
        assert_eq!(app.runtimes.len(), 2);
        // send
        ok(app.handle_control(Request {
            token: ct.clone(),
            method: Method::Send { pane: p, text: "hello".into(), submit: true },
        }));
        assert!(app.runtimes.get(&p).unwrap().input.ends_with(b"hello\r"));
        // list shows both panes for the fleet actor
        let list = ok(app.handle_control(Request { token: ct.clone(), method: Method::List }));
        assert_eq!(list.as_array().unwrap().len(), 2);
        // read (FakePane grab is empty but the call succeeds)
        ok(app.handle_control(Request {
            token: ct.clone(),
            method: Method::Read { pane: p, mode: ReadMode::Screen },
        }));
        // close the spawned pane
        ok(app.handle_control(Request {
            token: ct.clone(),
            method: Method::Close { pane: p, force: false },
        }));
        assert_eq!(app.runtimes.len(), 1);
    }

    #[test]
    fn control_pane_actor_is_scoped_to_its_subtree() {
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        let ct = app.control_token().to_string();
        // Pane 1 acts via its own token.
        app.tokens.insert(1, "tok1".into());
        // Pane 1 spawns a child → child.spawned_by == 1.
        let child = match app.handle_control(Request {
            token: "tok1".into(),
            method: Method::Spawn { adapter: "shell".into(), cwd: None, initial_input: None },
        }) {
            Reply::Ok { ok } => ok["pane"].as_u64().unwrap(),
            Reply::Err { err } => panic!("{err}"),
        };
        assert_eq!(app.find_spec(child).unwrap().spawned_by, Some(1));
        // Pane 1 may drive its own child.
        assert!(matches!(
            app.handle_control(Request {
                token: "tok1".into(),
                method: Method::Send { pane: child, text: "x".into(), submit: false },
            }),
            Reply::Ok { .. }
        ));
        // A pane spawned by the *fleet* is not in pane 1's subtree.
        let other = match app.handle_control(Request {
            token: ct,
            method: Method::Spawn { adapter: "shell".into(), cwd: None, initial_input: None },
        }) {
            Reply::Ok { ok } => ok["pane"].as_u64().unwrap(),
            Reply::Err { err } => panic!("{err}"),
        };
        assert!(matches!(
            app.handle_control(Request {
                token: "tok1".into(),
                method: Method::Send { pane: other, text: "x".into(), submit: false },
            }),
            Reply::Err { .. } // forbidden — not in subtree
        ));
        // An unknown token is unauthorized outright.
        assert!(matches!(
            app.handle_control(Request { token: "nope".into(), method: Method::List }),
            Reply::Err { .. }
        ));
    }

    #[test]
    fn control_cannot_close_the_last_pane() {
        use crate::core::control::{Method, Request};
        let (mut app, _) = mk_app(shell_ws());
        let ct = app.control_token().to_string();
        assert!(matches!(
            app.handle_control(Request { token: ct, method: Method::Close { pane: 1, force: true } }),
            crate::core::control::Reply::Err { .. }
        ));
    }

    #[test]
    fn control_spawn_and_fork_preserve_human_focus_and_active_tab() {
        // The control API must never steal the human's focus or jump their
        // active tab (DESIGN-control §5.2) — spawn_child (shared with the
        // interactive Alt+n path) does both internally; ctl_spawn/ctl_fork
        // must undo it (H1/H2).
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewTab); // active_tab=1, focused = the new tab's pane
        let (focused, active_tab) = (app.focused, app.ws.active_tab);
        let ct = app.control_token().to_string();

        let spawned = match app.handle_control(Request {
            token: ct.clone(),
            method: Method::Spawn { adapter: "shell".into(), cwd: None, initial_input: None },
        }) {
            Reply::Ok { ok } => ok["pane"].as_u64().unwrap(),
            Reply::Err { err } => panic!("{err}"),
        };
        assert_eq!(app.focused, focused, "spawn must not move the human's focus");
        assert_eq!(app.ws.active_tab, active_tab, "spawn must not switch the human's tab");

        match app.handle_control(Request { token: ct, method: Method::Fork { pane: Some(spawned) } }) {
            Reply::Ok { .. } => {}
            Reply::Err { err } => panic!("{err}"),
        }
        assert_eq!(app.focused, focused, "fork must not move the human's focus");
        assert_eq!(app.ws.active_tab, active_tab, "fork must not switch the human's tab");
    }

    #[test]
    fn audit_summary_omits_send_text() {
        use crate::core::control::Method;
        let s = super::method_summary(&Method::Send {
            pane: 5,
            text: "SUPER_SECRET_VALUE".into(),
            submit: true,
        });
        assert!(!s.contains("SECRET")); // text is never logged
        assert!(s.contains("pane=5") && s.contains("len=18") && s.contains("submit=true"));
    }

    #[test]
    fn audit_log_sanitizes_lines_and_reflects_real_outcome() {
        use crate::core::control::{Method, Reply, Request};
        let dir = std::env::temp_dir().join(format!("roost-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = MemStore::default();
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut app = App::<FakePane>::new(
            shell_ws(),
            agents::registry(),
            Box::new(store),
            tx,
            Size::new(100, 30),
            Some(dir.join("roost.sock")),
        )
        .unwrap();
        let ct = app.control_token().to_string();

        // A denied wait (unknown pane) must be audited as a denial (M3), not
        // the unconditional "ok ... parked" it used to log regardless of
        // outcome.
        let (rtx, rrx) = std::sync::mpsc::channel();
        app.handle_control_msg(
            Request {
                token: ct.clone(),
                method: Method::Wait { panes: vec![999], until: "idle".into(), timeout_ms: None },
            },
            rtx,
        );
        assert!(matches!(rrx.recv().unwrap(), Reply::Err { .. }));

        // An attacker-controlled field (adapter name) with an embedded
        // newline must not forge a second, fake log line.
        let (rtx2, rrx2) = std::sync::mpsc::channel();
        app.handle_control_msg(
            Request {
                token: ct,
                method: Method::Spawn {
                    adapter: "evil\nFORGED fleet spawn -> ok pane=1".into(),
                    cwd: None,
                    initial_input: None,
                },
            },
            rtx2,
        );
        assert!(matches!(rrx2.recv().unwrap(), Reply::Err { .. })); // unknown adapter

        let log = std::fs::read_to_string(dir.join("control.log")).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2, "embedded control chars must not add log lines: {log:?}");
        assert!(lines[0].contains(" err "), "denied wait must audit as err: {}", lines[0]);
        assert!(!lines[0].contains("parked"), "denied wait must not claim it parked: {}", lines[0]);
        assert!(lines[1].contains("FORGED"), "content preserved, just de-lined: {}", lines[1]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn control_wait_immediate_parks_and_fires() {
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        let ct = app.control_token().to_string();
        let p = 1; // the initial shell pane (FakePane starts Idle)
        let wait = |until: &str, ms: u64| Request {
            token: ct.clone(),
            method: Method::Wait { panes: vec![p], until: until.into(), timeout_ms: Some(ms) },
        };

        // Already at the target status → reply comes back immediately.
        let (tx, rx) = std::sync::mpsc::channel();
        app.handle_control_msg(wait("idle", 1000), tx);
        match rx.recv().unwrap() {
            Reply::Ok { ok } => assert_eq!(ok["status"], "idle"),
            Reply::Err { err } => panic!("{err}"),
        }
        assert!(app.waiters.is_empty());

        // Not yet at target → parks (no reply), then fires when it transitions.
        let (tx, rx) = std::sync::mpsc::channel();
        app.handle_control_msg(wait("working", 60_000), tx);
        assert!(rx.try_recv().is_err());
        assert_eq!(app.waiters.len(), 1);
        app.on_pty_output(p, b"x"); // FakePane: output ⇒ Working
        app.poll_waiters();
        match rx.recv().unwrap() {
            Reply::Ok { ok } => assert_eq!(ok["status"], "working"),
            Reply::Err { err } => panic!("{err}"),
        }
        assert!(app.waiters.is_empty());

        // A 0ms timeout on an unreachable status → times out on the next poll.
        let (tx, rx) = std::sync::mpsc::channel();
        app.handle_control_msg(wait("exited", 0), tx);
        app.poll_waiters();
        match rx.recv().unwrap() {
            Reply::Ok { ok } => assert_eq!(ok["timed_out"], true),
            Reply::Err { err } => panic!("{err}"),
        }
    }

    #[test]
    fn wait_on_a_closed_pane_resolves_instead_of_hanging() {
        // M2 regression: a wait parked on a pane that's then closed must
        // resolve right away (reported as "exited") instead of blocking to
        // the deadline and holding its connection slot the whole time.
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1 & 2, focus = 2
        let target = app.focused;
        let ct = app.control_token().to_string();

        let (tx, rx) = std::sync::mpsc::channel();
        app.handle_control_msg(
            Request {
                token: ct,
                method: Method::Wait {
                    panes: vec![target],
                    until: "needs_input".into(),
                    timeout_ms: Some(60_000),
                },
            },
            tx,
        );
        assert!(rx.try_recv().is_err()); // idle pane, not yet needs_input → parked
        assert_eq!(app.waiters.len(), 1);

        app.apply(Action::ClosePane); // closes `target` (non-busy → closes immediately)
        app.poll_waiters();

        match rx.recv().unwrap() {
            Reply::Ok { ok } => {
                assert_eq!(ok["pane"], target);
                assert_eq!(ok["status"], "exited");
            }
            Reply::Err { err } => panic!("{err}"),
        }
        assert!(app.waiters.is_empty());
    }

    #[test]
    fn socket_auth_requires_matching_pane_token() {
        let (mut app, _) = mk_app(shell_ws());
        app.tokens.insert(1, "secret-1".into());
        app.tokens.insert(2, "secret-2".into());
        // Correct pane+token pair is authorized.
        assert!(app.socket_authorized(1, "secret-1"));
        // Pane 2's process presenting pane 1's id with its own token is
        // rejected — the cross-pane spoof the token exists to stop.
        assert!(!app.socket_authorized(1, "secret-2"));
        // Wrong / empty / unknown-pane tokens all fail closed.
        assert!(!app.socket_authorized(1, "wrong"));
        assert!(!app.socket_authorized(1, ""));
        assert!(!app.socket_authorized(99, "secret-1"));
    }

    #[test]
    fn close_last_pane_confirms_then_quits() {
        let (mut app, _) = mk_app(shell_ws());
        // First press arms the "this quits roost" confirmation — does not quit.
        app.apply(Action::ClosePane);
        assert!(!app.quit);
        // Second press within the window confirms and quits.
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
    fn stale_session_falls_back_to_fresh_launch() {
        // A pi pane whose stored session id has no backing file on disk must
        // launch fresh instead of resuming into a dead pane, and the dead id
        // must be cleared from the workspace (regression: two concurrent pi
        // panes where one session was never persisted).
        //
        // Hermetic: point $HOME at a scratch dir with an *empty* (but
        // present) .pi/agent/sessions, so `session_state` deterministically
        // lands on "root present, id absent" → Gone, regardless of what the
        // real machine's actual ~/.pi looks like. Since a missing root now
        // legitimately means Unknown (not Gone — see agents::session_state),
        // a dev machine without ~/.pi at all would otherwise flip this test
        // into a resume attempt instead of a fresh launch.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let home = std::env::temp_dir().join(format!("roost-home-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".pi").join("agent").join("sessions")).unwrap();
        let real_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);

        let mut ws = shell_ws();
        let spec = ws.tabs[0].panes.get_mut(&1).unwrap();
        spec.adapter = "pi".into();
        spec.session = Some("roost-test-nonexistent-uuid-zzzz".into());
        let (app, store) = mk_app(ws);
        let program = app.runtimes.get(&1).unwrap().cmd.program.clone();
        let args = app.runtimes.get(&1).unwrap().cmd.args.clone();
        let saved_session = store.0.lock().unwrap().clone().unwrap().tabs[0].panes[&1].session.clone();

        // Restore before asserting, so a failure here can't leave $HOME
        // redirected for the rest of the test binary.
        match real_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&home);

        assert_eq!(program, "pi");
        assert!(args.is_empty(), "expected fresh launch, got {args:?}");
        assert!(saved_session.is_none());
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
    fn needs_input_notifies_even_without_a_prior_working_phase() {
        // An agent that asks for you immediately on resume (straight to
        // NeedsInput, no Working) must still pull attention when unfocused.
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // focus = 2, pane 1 unfocused & idle
        assert!(app.on_status(1, AgentStatus::NeedsInput).is_some());
        // ...but still never for the focused pane.
        assert!(app.on_status(2, AgentStatus::NeedsInput).is_none());
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
    fn copy_mode_selection_extracts_text_and_flashes() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::CopyMode);
        assert!(app.in_copy_mode());
        app.runtimes.get_mut(&1).unwrap().grab = "selected text".into();
        app.begin_selection(1, 0, 0);
        app.extend_selection(0, 5);
        assert_eq!(app.finish_selection().as_deref(), Some("selected text"));
        assert!(!app.in_copy_mode()); // exited on copy
        assert!(app.selection.is_none());
        assert!(app.flash().is_some()); // "copied N chars"
    }

    #[test]
    fn find_url_detects_and_trims() {
        use super::find_url_at;
        let line = "see https://example.com/path for details";
        // click anywhere within the URL (cols 4..=28) returns it
        assert_eq!(find_url_at(line, 4).as_deref(), Some("https://example.com/path"));
        assert_eq!(find_url_at(line, 20).as_deref(), Some("https://example.com/path"));
        // click on surrounding words → nothing
        assert_eq!(find_url_at(line, 0), None); // "see"
        assert_eq!(find_url_at(line, 30), None); // "for"
        // trailing punctuation and wrapping parens are stripped
        assert_eq!(find_url_at("(https://a.co).", 3).as_deref(), Some("https://a.co"));
        assert_eq!(find_url_at("go to https://a.co!", 10).as_deref(), Some("https://a.co"));
        // non-http tokens ignored
        assert_eq!(find_url_at("ftp://x.co here", 2), None);
    }

    #[test]
    fn copy_mode_empty_selection_copies_nothing() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::CopyMode);
        app.begin_selection(1, 0, 0); // grab defaults to ""
        assert!(app.finish_selection().is_none());
        assert!(!app.in_copy_mode());
    }

    #[test]
    fn flip_split_changes_focused_pane_orientation() {
        use crate::core::layout::SplitDir;
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // vertical split (side by side)
        assert!(matches!(
            app.ws.tabs[0].layout,
            LayoutNode::Split { dir: SplitDir::Vertical, .. }
        ));
        app.apply(Action::FlipSplit);
        assert!(matches!(
            app.ws.tabs[0].layout,
            LayoutNode::Split { dir: SplitDir::Horizontal, .. }
        ));
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

    /// Test-only adapter whose session root is the pane's own cwd, so a test
    /// can drop session files directly in a temp dir without touching a real
    /// `~/.pi/agent/sessions`.
    struct DetectAdapter;
    impl crate::agents::AgentAdapter for DetectAdapter {
        fn id(&self) -> &'static str {
            "detect"
        }
        fn launch(&self, cwd: &std::path::Path) -> crate::agents::CommandSpec {
            crate::agents::CommandSpec::new("true", cwd)
        }
        fn resume(&self, cwd: &std::path::Path, session: &str) -> crate::agents::CommandSpec {
            crate::agents::CommandSpec::new("true", cwd).arg(session)
        }
        fn session_root(&self, cwd: &std::path::Path) -> Option<PathBuf> {
            Some(cwd.to_path_buf())
        }
    }

    #[test]
    fn tick_lets_each_concurrently_launched_pane_claim_its_own_session_file() {
        // Regression: two panes launched into the same cwd around the same
        // time share one session root. `tick()` used to process pending
        // panes in HashMap (i.e. arbitrary) order; whichever pane got
        // processed first could steal the *other* pane's newer, not-yet-
        // claimed session file, leaving that other pane with none at all —
        // it would then relaunch fresh instead of resuming on the next run.
        let dir = std::env::temp_dir().join(format!("roost-detect-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let base = SystemTime::now();
        let file_a = dir.join("a.jsonl");
        let file_b = dir.join("b.jsonl");
        std::fs::write(&file_a, "").unwrap();
        std::fs::write(&file_b, "").unwrap();
        std::fs::File::open(&file_a).unwrap().set_modified(base + Duration::from_millis(10)).unwrap();
        std::fs::File::open(&file_b).unwrap().set_modified(base + Duration::from_millis(20)).unwrap();

        let mut panes = HashMap::new();
        panes.insert(1, PaneSpec { adapter: "detect".into(), cwd: dir.clone(), session: None, title: None, spawned_by: None });
        panes.insert(2, PaneSpec { adapter: "detect".into(), cwd: dir.clone(), session: None, title: None, spawned_by: None });
        let layout = LayoutNode::Split {
            dir: SplitDir::Vertical,
            ratios: vec![0.5, 0.5],
            children: vec![LayoutNode::Pane(1), LayoutNode::Pane(2)],
        };
        let ws = Workspace { version: 1, active_tab: 0, tabs: vec![Tab { name: "main".into(), layout, panes }] };

        let mut registry = agents::registry();
        registry.insert("detect", Box::new(DetectAdapter));
        let store = MemStore::default();
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut app =
            App::<FakePane>::new(ws, registry, Box::new(store), tx, Size::new(100, 30), None).unwrap();

        // Pane 1 "spawned" before either file existed (widest window); pane 2
        // "spawned" after file_a but before file_b — the precise ordering
        // that used to starve whichever pane got processed second.
        app.pending_detect.clear();
        app.pending_detect.insert(1, base);
        app.pending_detect.insert(2, base + Duration::from_millis(15));
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);

        app.tick();

        assert_eq!(app.find_spec(1).unwrap().session.as_deref(), Some("a"));
        assert_eq!(app.find_spec(2).unwrap().session.as_deref(), Some("b"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exit_notifies_only_when_unfocused() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // focus = 2
        assert!(app.on_pty_exit(1).is_some()); // pane 1 exits, unfocused
        assert!(app.on_pty_exit(2).is_none()); // pane 2 exits, focused
    }

    #[test]
    fn observe_promotes_shell_to_agent_and_tracks_cwd() {
        let (mut app, store) = mk_app(shell_ws());
        // pane 1: user cd'd to /work/proj and typed `pi`
        app.runtimes.get_mut(&1).unwrap().observation = Some(Observation {
            cwd: Some(PathBuf::from("/work/proj")),
            agent: Some("pi".into()),
        });
        app.observe_panes();
        let spec = app.find_spec(1).unwrap();
        assert_eq!(spec.adapter, "pi");
        assert_eq!(spec.cwd, PathBuf::from("/work/proj"));
        let saved = store.0.lock().unwrap().clone().unwrap();
        assert_eq!(saved.tabs[0].panes[&1].adapter, "pi"); // persisted
        assert!(app.pending_detect.contains_key(&1)); // queued for session detection
    }

    #[test]
    fn observe_demotes_to_shell_when_agent_exits() {
        let mut ws = shell_ws();
        ws.tabs[0].panes.get_mut(&1).unwrap().adapter = "pi".into();
        let (mut app, _) = mk_app(ws);
        // pi exited; the pane is a plain shell again
        app.runtimes.get_mut(&1).unwrap().observation =
            Some(Observation { cwd: None, agent: None });
        app.observe_panes();
        assert_eq!(app.find_spec(1).unwrap().adapter, "shell");
    }

    #[test]
    fn observe_none_leaves_pane_untouched() {
        // A momentarily-unreadable process must not clobber persisted state.
        let mut ws = shell_ws();
        ws.tabs[0].panes.get_mut(&1).unwrap().adapter = "pi".into();
        let (mut app, _) = mk_app(ws);
        app.runtimes.get_mut(&1).unwrap().observation = None;
        app.observe_panes();
        assert_eq!(app.find_spec(1).unwrap().adapter, "pi");
    }

    #[test]
    fn closing_a_pane_clears_its_dead_record() {
        // A spawn-failed pane's error lives in `dead`; closing the pane must
        // drop it so the map doesn't accumulate stale entries over a session.
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1 & 2, focus = 2
        app.dead.insert(2, "spawn failed".into());
        app.apply(Action::ClosePane); // closes focused pane 2
        assert!(!app.dead.contains_key(&2));
    }

    #[test]
    fn closing_a_pane_does_not_notify_on_its_eof() {
        // Alt+w removes the pane, then its process EOFs and delivers Exit.
        // That deliberate close must not ring the bell / fire a notification.
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1 & 2, focus = 2
        app.apply(Action::ClosePane); // closes pane 2, focus -> 1
        assert!(app.on_pty_exit(2).is_none()); // its late EOF is silent
    }

    #[test]
    fn alt_hint_gates_on_seen_time_and_terminal() {
        assert!(wants_alt_hint(false, Duration::from_secs(1), Some("Apple_Terminal")));
        assert!(!wants_alt_hint(true, Duration::from_secs(1), Some("Apple_Terminal")));
        assert!(!wants_alt_hint(false, ALT_HINT_WINDOW, Some("Apple_Terminal")));
        assert!(!wants_alt_hint(false, Duration::from_secs(1), Some("iTerm.app")));
        assert!(!wants_alt_hint(false, Duration::from_secs(1), None));
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
