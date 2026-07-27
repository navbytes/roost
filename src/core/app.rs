//! App core: orchestrates workspace (precious) and pane backends
//! (disposable) purely through ports — no PTY, filesystem, or terminal
//! specifics here. Generic over `PaneBackend` so every behavior below is
//! unit-tested with fakes (see tests at the bottom).

use anyhow::Result;
use ratatui::layout::{Rect, Size};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, SyncSender};
use std::time::{Duration, Instant, SystemTime};

use crate::agents::Registry;
use crate::core::control::{Actor, Method, ReadMode, Reply, Request};
use crate::core::event::AppEvent;
use crate::core::layout::{self, LayoutNode, PaneId, PaneRect, SplitDir};
use crate::core::status::AgentStatus;
use crate::core::workspace::{PaneSpec, Tab, Workspace};
use crate::ports::{ClipboardOutcome, Observation, PaneBackend, StateStore};
use crate::ui::input::Action;
use crate::ui::render::state_word;

const DETECT_INTERVAL: Duration = Duration::from_secs(2);

/// P6: how much of a pane's live OSC 0/2 title is adopted as its display
/// name. Claude Code publishes `spinner + task` continuously, and a task
/// line can be a paragraph; every fleet surface that shows a display name
/// (badge, collapsed row, feed, notification, host title) needs a bound it
/// can lay out against. The corner badge clips visually on top of this.
const LIVE_TITLE_CAP: usize = 48;

/// P6: minimum spacing between host-terminal title updates. An agent that
/// republishes its OSC title on every spinner frame would otherwise have
/// roost repaint the host's title bar ~30x a second.
const HOST_TITLE_INTERVAL: Duration = Duration::from_millis(200);

/// How long the "Alt keys aren't reaching roost" hint stays up on a fresh
/// launch before we assume the user isn't going to press one / already saw it.
const ALT_HINT_WINDOW: Duration = Duration::from_secs(8);

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
    /// Text selection — mouse drag, or the C24 keyboard cursor. `cursor` is
    /// (row, col) in the focused pane's inner cell space; both input
    /// methods write the same `App::selection`.
    Copy { cursor: (u16, u16) },
    /// Full-keymap overlay (Alt+?); any key dismisses it.
    Help,
    /// C20 activity-feed overlay; `offset` counts entries back from the
    /// newest (0 = live tail).
    Feed { offset: usize },
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

/// One C20 activity-feed entry, preformatted at push time — the ring stores
/// only the rendered line (no raw event data: no filtering/search, by
/// design). `needs_input` is the one styling exception: true only for a
/// status-transition line landing on `NeedsInput` (the `◆ ` ACCENT prefix,
/// FG text — same meaning as everywhere else, C5).
#[derive(Debug, Clone)]
pub struct FeedEntry {
    pub at: SystemTime,
    pub text: String,
    pub needs_input: bool,
}

/// How long the "copied" flash stays in the hint bar.
const FLASH_WINDOW: Duration = Duration::from_secs(2);

/// How long an armed destructive-close confirmation stays live: press the same
/// key again within this window to actually close/quit. A confirm's *flash*
/// carries this same window (U22): the prompt must be visible for exactly as
/// long as the second press would fire — the old FLASH_WINDOW-sized prompt
/// left a silent final second where Alt+w/Alt+q still confirmed.
const CONFIRM_WINDOW: Duration = Duration::from_secs(3);

/// How many closed panes/tabs the undo stack keeps.
const UNDO_DEPTH: usize = 20;

/// Cap on concurrently parked `wait` requests. Each parked wait holds a
/// socket connection slot for up to its whole timeout, and one-shot calls
/// like status/list share that same pool (`MAX_CONN` in sock.rs) — without a
/// cap, enough parked waits could starve those of a slot to even report in
/// on. Left well below that pool size so plenty always stay free.
const MAX_WAITS: usize = 16;

/// C20: activity-feed ring buffer capacity — oldest evicted first.
const FEED_CAP: usize = 200;

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
    /// Who registered this wait — re-checked against `may_target` when a
    /// candidate pane fires (see `poll_waiters`). Pane ids are recycled
    /// once free (`Workspace::next_pane_id` is a max+1, not a monotonic
    /// counter): the id this waiter was authorized for at registration can
    /// belong to a different, unrelated pane by the time it fires.
    actor: Actor,
    panes: Vec<PaneId>,
    until: AgentStatus,
    reply: Sender<Reply>,
    deadline: Instant,
}

/// C22: the app-wide floating scratch pane. Lives outside every tab's
/// layout tree — its runtime sits in the same `App::runtimes` map as any
/// other pane, but `spec` here (not `Tab::panes`) is its source of truth,
/// and it is never written to `workspace.json` (session-only by design).
#[derive(Debug)]
struct Float {
    id: PaneId,
    spec: PaneSpec,
    shown: bool,
    /// Whatever was focused right before the float last became shown —
    /// where focus returns when it hides (C22 rules 2/3).
    prev_focus: PaneId,
}

/// C22: below this body size the geometry formula (`App::float_rect`) has
/// no room to place a sane rect — the toggle refuses instead.
const MIN_FLOAT_BODY_COLS: u16 = 40;
const MIN_FLOAT_BODY_ROWS: u16 = 10;

/// A tab's aggregate state for the tab bar, worst-relevant-first. `Unknown`
/// is a lazily-loaded tab whose panes haven't been spawned — deliberately
/// distinct from `Quiet` (spawned, nothing happening) so a background tab
/// never masquerades as idle. `Exited` (U13, closing SPEC-GAP-2) is the
/// same honesty one step further: a tab whose agents are *dead* used to
/// render the Quiet blank, so a background tab full of corpses looked
/// exactly like a background tab with nothing to say. `render` maps each to
/// a glyph + colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabSummary {
    NeedsInput,
    Working,
    Unknown,
    Waiting,
    Exited,
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
    /// C21: a pure view transform. While true, the render / PTY-resize /
    /// mouse paths see only the focused pane at the full body area (see
    /// `display_rects`); the layout tree itself is never touched.
    /// Session-only, never persisted.
    zoomed: bool,
    /// C25: index of the arrangement Alt+g tries first on the next press
    /// (grid=0 / main+stack=1 / all-stack=2). Session-only.
    layout_cycle: usize,
    store: Box<dyn StateStore>,
    /// Whether the most recent workspace save succeeded — the tab bar's
    /// right status area (C2) shows "saved ✓" while this is true and
    /// "save failed ✕" once a save errors. Starts `true`: on load we just
    /// read `ws` from disk, so "saved" is accurate until proven otherwise.
    last_save_ok: bool,
    tx: SyncSender<AppEvent>,
    term_size: Size,
    /// P4: the host window's pixel geometry (width, height) as last reported
    /// by the terminal; (0, 0) when the host doesn't report pixels. Panes get
    /// a proportional share (`pane_pixels`) at spawn and on every resize.
    host_pixels: (u16, u16),
    /// Freshly launched agent panes we still owe a session id.
    pending_detect: HashMap<PaneId, SystemTime>,
    last_detect: Instant,
    sock_path: Option<PathBuf>,
    /// `$HOME`, resolved once at startup — `focused_cwd()`'s `~`-abbreviation
    /// reads this instead of asking the environment on every frame (D4).
    home: Option<PathBuf>,
    started: Instant,
    /// Set the first time an Alt-modified key event arrives, so the
    /// "Alt keys aren't reaching roost" startup hint can stop once we know
    /// they are (or the window has simply run out).
    alt_seen: bool,
    /// U4: set the first time ANY key event arrives. Keys flowing with no
    /// Alt among them is the evidence the alt-trap warning now triggers on,
    /// instead of an allowlist of terminals known to eat Option.
    keys_seen: bool,
    /// Active/last text selection (copy mode).
    pub selection: Option<Selection>,
    /// Transient status message shown in the hint bar (e.g. "copied"), with
    /// the window it stays visible for: `FLASH_WINDOW` for ordinary notices,
    /// `CONFIRM_WINDOW` for confirm-arm prompts (U22 — prompt and arm live
    /// exactly as long as each other; the window doubles as the marker that
    /// tells a confirm prompt apart, see `clear_confirm_flash`).
    flash: Option<(String, Instant, Duration)>,
    /// Recently closed panes/tabs, for `Alt+u` undo (most-recent last).
    undo: Vec<Closed>,
    /// When a destructive close (busy pane / last pane) has been armed and is
    /// awaiting a confirming second keypress.
    confirm_close: Option<Instant>,
    /// When a busy-fleet quit (U1) has been armed and is awaiting a
    /// confirming second Alt+q. Separate from `confirm_close` so an armed
    /// close can never confirm a quit (or vice versa).
    confirm_quit: Option<Instant>,
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
    /// `initial_input` from a control `spawn`, buffered until the pane's
    /// first PTY output rather than written the instant it spawns — see
    /// `on_pty_output` for why.
    pending_input: HashMap<PaneId, Vec<u8>>,
    /// C20: last-known status per spawned pane, diffed each tick to produce
    /// a feed transition line (`diff_statuses`). Pruned to just the
    /// currently-running panes on the same cadence.
    last_status: HashMap<PaneId, AgentStatus>,
    /// C20: activity-feed ring buffer (status/spawn/close/exit/ctl),
    /// capacity `FEED_CAP`, oldest evicted first. Session-only — never
    /// persisted.
    feed: VecDeque<FeedEntry>,
    /// C22: the one app-wide floating scratch pane slot, spawned on first
    /// Alt+f. `None` until then.
    float: Option<Float>,
    /// C23: panes currently in raw (hard pass-through) mode, by id.
    /// Per-pane, session-only — never persisted.
    raw: HashSet<PaneId>,
    /// U11: the pane each tab returns focus to when you come back to it —
    /// session-only, never persisted. Held as a **set of pane ids** rather
    /// than a map keyed by tab index, because `ws.tabs` has no stable
    /// identity and indexes shift on close/reorder/undo: a tab's entry is
    /// simply the one remembered id that tab still owns (pane ids are
    /// unique workspace-wide), so a vanished tab can never hand its memory
    /// to a different one. The invariant "at most one entry per tab" is
    /// maintained by `remember_tab_focus`, and `close_pane_id` forgets a
    /// closed pane so a recycled id can't resurrect a stale memory.
    tab_focus: HashSet<PaneId>,
    /// C24: text most recently yanked via the keyboard copy-mode `y`/`Enter`
    /// chord, waiting for the composition root to hand it to the OS
    /// clipboard — core has no I/O of its own (module doc). The mouse path
    /// copies directly from `finish_selection`'s return value in `main.rs`
    /// instead, since it already runs there; this field exists only because
    /// `handle_mode_key` has no such caller-side return channel.
    pending_yank: Option<String>,
    /// W3: bytes roost owes its OWN terminal — pane OSC 9 notifications
    /// (P2) and OSC 52 clipboard writes (P3) forwarded on a pane's behalf.
    /// Queued rather than written because core does no I/O (module doc) and
    /// because host writes must land *between* frames, never inside one.
    /// Drained each loop iteration by `take_host_output`.
    host_out: Vec<u8>,
    /// P6: the host terminal title roost last published, and when. The outer
    /// tab/window title otherwise goes stale the moment roost starts — it
    /// should read `roost · <focused pane>` and follow both focus moves and
    /// the pane's own live OSC title.
    host_title: String,
    last_host_title: Option<Instant>,
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
        host_pixels: (u16, u16),
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
            zoomed: false,
            layout_cycle: 0,
            store,
            last_save_ok: true,
            tx,
            term_size,
            host_pixels,
            pending_detect: HashMap::new(),
            last_detect: Instant::now(),
            sock_path,
            home: dirs::home_dir(),
            started: Instant::now(),
            alt_seen: false,
            keys_seen: false,
            selection: None,
            flash: None,
            undo: Vec::new(),
            confirm_close: None,
            confirm_quit: None,
            tokens: HashMap::new(),
            control_token,
            waiters: Vec::new(),
            pending_input: HashMap::new(),
            last_status: HashMap::new(),
            feed: VecDeque::new(),
            float: None,
            raw: HashSet::new(),
            tab_focus: HashSet::new(),
            pending_yank: None,
            host_out: Vec::new(),
            host_title: String::new(),
            last_host_title: None,
        };
        app.spawn_active_tab();
        app.focused = app.pane_order().first().copied().unwrap_or(0);
        Ok(app)
    }

    fn save(&mut self) {
        self.last_save_ok = self.store.save(&self.ws).is_ok();
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

    /// C21: is the focused pane currently zoomed (full-body view)? Drives
    /// the hint bar's `ZOOM` pseudo-state word (amended C9).
    pub fn zoomed(&self) -> bool {
        self.zoomed
    }

    /// C20: the activity-feed ring, oldest first — read by the renderer
    /// while `Mode::Feed` is open.
    pub fn feed(&self) -> &VecDeque<FeedEntry> {
        &self.feed
    }

    /// C20: append one preformatted line to the activity feed, evicting the
    /// oldest entry once the ring is at capacity. The single entry point
    /// every hook (spawn/close/exit/status-diff/ctl) pushes through.
    fn push_feed(&mut self, text: String, needs_input: bool) {
        self.feed.push_back(FeedEntry { at: SystemTime::now(), text, needs_input });
        if self.feed.len() > FEED_CAP {
            self.feed.pop_front();
        }
    }

    /// Record that an Alt-modified key actually arrived, so the startup hint
    /// (below) knows it doesn't need to warn.
    pub fn note_alt_seen(&mut self) {
        self.alt_seen = true;
    }

    /// U4: record that *some* key arrived — the evidence half of the
    /// alt-trap trigger. Keys flowing with no Alt among them is what
    /// "the terminal is eating the Alt layer" looks like from in here.
    pub fn note_key_seen(&mut self) {
        self.keys_seen = true;
    }

    /// C11/U4: should the "Alt keys aren't reaching roost" bar be up? Many
    /// terminals don't send Alt as a modifier until told to (stock
    /// Terminal.app's "Use Option as Meta Key", iTerm2's Left Option =
    /// `Esc+`) — with it off, every Alt chord roost relies on silently does
    /// nothing and there is no other signal saying why. Gating this on
    /// `TERM_PROGRAM == "Apple_Terminal"` only meant the README's own
    /// recommended terminal (iTerm2) never warned; the trigger is now the
    /// evidence itself, on any terminal (see `wants_alt_hint`).
    pub fn show_alt_hint(&self) -> bool {
        wants_alt_hint(self.alt_seen, self.keys_seen, self.started.elapsed())
    }

    /// C11/U4: the warning's text, with the real menu path for terminals
    /// whose setting we know. Reads roost's OWN `TERM_PROGRAM` — the host
    /// terminal's identity. (Panes are handed `TERM_PROGRAM=roost` by
    /// `infra::pty`, but that is the child's environment; this process still
    /// sees what the host set, which is exactly what must be named here.)
    pub fn alt_hint_line(&self) -> &'static str {
        alt_hint_line(std::env::var("TERM_PROGRAM").ok().as_deref())
    }

    /// Time since app start — the shared clock chrome uses for the
    /// Working-glyph pulse (`theme::pulse_phase`, C5), so every pulsing
    /// glyph on screen flips in unison.
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Whether the last workspace save succeeded — the tab bar's right
    /// status area (C2).
    pub fn last_save_ok(&self) -> bool {
        self.last_save_ok
    }

    /// Pane rectangles of the active tab (border-inclusive).
    pub fn rects(&self) -> Vec<PaneRect> {
        let mut v = Vec::new();
        layout::compute_rects(&self.ws.active_tab().layout, self.body_area(), &mut v);
        v
    }

    /// C21/C22/§5: the one display list the renderer, PTY-resize, and
    /// mouse-hit paths all consume — float first when shown (topmost wins
    /// in `hit_test`; the renderer instead paints it *last*, see
    /// `render::draw`), then the zoomed singleton `[zoom target @ body]`
    /// while zoomed, else the real tree's `rects()`. Focus math
    /// (`layout::neighbor`, `focus_dir`) deliberately keeps reading
    /// `rects()` instead — that's what makes zoom follow focus rather than
    /// freeze it.
    pub fn display_rects(&self) -> Vec<PaneRect> {
        let body = self.body_area();
        let mut v = Vec::new();
        if let Some(f) = &self.float {
            if f.shown {
                v.push(PaneRect { id: f.id, rect: Self::float_rect(body), collapsed: false });
            }
        }
        if self.zoomed {
            // C21 "keeps zoom" + C22 rule 1: while the float is shown, focus
            // belongs to it, not the zoomed pane — the real zoom target is
            // whatever was focused right before the float appeared.
            let target = match &self.float {
                Some(f) if f.shown => f.prev_focus,
                _ => self.focused,
            };
            v.push(PaneRect { id: target, rect: body, collapsed: false });
        } else {
            v.extend(self.rects());
        }
        v
    }

    /// C22: the float's rect for a given body area — centered,
    /// `w = clamp(3·body.width/5, 36, body.width−4)`,
    /// `h = clamp(3·body.height/5, 8, body.height−2)`. Written without
    /// `.clamp()` (whose panic-on-`min>max` would fire below the refusal
    /// floor) so a body that shrank *after* the float was shown still
    /// produces a rect, never a panic; callers gate showing/spawning a new
    /// float on `float_fits` first.
    fn float_rect(body: Rect) -> Rect {
        let w = (3 * body.width / 5).max(36).min(body.width.saturating_sub(4));
        let h = (3 * body.height / 5).max(8).min(body.height.saturating_sub(2));
        let x = body.x + body.width.saturating_sub(w) / 2;
        let y = body.y + body.height.saturating_sub(h) / 2;
        Rect::new(x, y, w, h)
    }

    /// C22: is `body` big enough for `float_rect` to place a sane rect?
    fn float_fits(body: Rect) -> bool {
        body.width >= MIN_FLOAT_BODY_COLS && body.height >= MIN_FLOAT_BODY_ROWS
    }

    /// C22: pane-id allocation must account for the float, which lives
    /// outside `ws.tabs` — `ws.next_pane_id()` alone would eventually let a
    /// split reuse its id. The one allocator every spawn path goes through.
    fn alloc_pane_id(&self) -> PaneId {
        let base = self.ws.next_pane_id();
        match &self.float {
            Some(f) => base.max(f.id + 1),
            None => base,
        }
    }

    /// Is `id` the float's pane?
    fn is_float(&self, id: PaneId) -> bool {
        self.float.as_ref().is_some_and(|f| f.id == id)
    }

    /// Is the float currently the focused pane? Equivalent to "is it
    /// shown" (rule 1: shown ⇒ focused) — checking focus directly is
    /// simpler and doesn't also require reading `shown`.
    fn float_focused(&self) -> bool {
        self.is_float(self.focused)
    }

    /// C23: does `id` currently have raw pass-through enabled? Read by the
    /// renderer for the badge/row token and hint-bar word (C4/C8/C9).
    pub fn is_raw(&self, id: PaneId) -> bool {
        self.raw.contains(&id)
    }

    /// C23's routing predicate, verbatim: raw key-forwarding applies only
    /// when the mode is Normal, the focused pane is flagged raw, and it's
    /// alive. Checked by `main.rs`'s key path before it ever calls
    /// `translate()`.
    pub fn raw_routing_active(&self) -> bool {
        matches!(self.mode, Mode::Normal) && self.raw.contains(&self.focused) && !self.focused_dead()
    }

    /// C24: take (clearing) the pending keyboard-copy yank, if any — see
    /// `pending_yank`. Polled once per tick from the composition root.
    pub fn take_pending_yank(&mut self) -> Option<String> {
        self.pending_yank.take()
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
        let pixels = self.pane_pixels(rows, cols);
        match B::spawn(id, &cmd, rows, cols, pixels, self.tx.clone()) {
            Ok(rt) => {
                self.runtimes.insert(id, rt);
                self.dead.remove(&id);
                // Owe this pane a session id? Watch for one (socket reports
                // it exactly; the filesystem scan in tick() is the fallback).
                if wants_detect {
                    self.pending_detect.insert(id, SystemTime::now());
                }
                // C20: spawn owns the pane's "birth" line — diff_statuses
                // deliberately stays silent on a pane's first observation.
                // U2: led by the pane id; the `({adapter})` suffix only for
                // titled panes (an untitled display name already ends in the
                // adapter/cwd tag — C4's no-dup rule).
                let label = format!("{id} {}", display_name_of(spec));
                let line = match &spec.title {
                    Some(_) => format!("spawned {label} ({})", spec.adapter),
                    None => format!("spawned {label}"),
                };
                self.push_feed(line, false);
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
        // C20: one status-transition feed line per pane per tick, diffed
        // against each pane's last-known status. Placed before the
        // `pending_detect` early-return below so it always runs, whether or
        // not any pane is mid-session-detection.
        self.diff_statuses();
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

    /// C20: diff every spawned pane's current status against its last-known
    /// one, pushing `{name}: {old} → {new}` for each real transition. First
    /// observation of a pane is silently baselined (`spawn_pane` owns
    /// birth); a transition landing on `Exited` is suppressed (`on_pty_exit`
    /// owns that line) — one source per transition, no double-reporting.
    fn diff_statuses(&mut self) {
        let current: Vec<(PaneId, AgentStatus)> =
            self.runtimes.iter().map(|(id, rt)| (*id, rt.status())).collect();
        for (id, status) in current {
            let prev = self.last_status.insert(id, status);
            match prev {
                None => {} // first observation: spawn owns birth, no line
                Some(old) if old == status || status == AgentStatus::Exited => {}
                Some(old) => {
                    // U2: `{id} {display_name}`, so four identical shells'
                    // transitions stop being indistinguishable in the feed.
                    let text = format!(
                        "{}: {} → {}",
                        self.feed_label(id),
                        state_word(old),
                        state_word(status)
                    );
                    self.push_feed(text, status == AgentStatus::NeedsInput);
                }
            }
        }
        // Drop entries for panes that no longer have a runtime (closed) —
        // ids are never reused, so without this the map would grow by one
        // stale entry per pane ever spawned over a long session.
        self.last_status.retain(|id, _| self.runtimes.contains_key(id));
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

    /// U2 (amended, P6): the one display name for pane `id`, used by every
    /// fleet surface (corner badge, collapsed rows, feed lines,
    /// notifications, flashes, and the host terminal's own title).
    ///
    /// The chain is: explicit Alt+r title → the pane's **live OSC 0/2
    /// title** → `adapter · cwd-tag`. The live title is the cheapest fleet
    /// status text there is — Claude Code publishes `spinner + task` through
    /// it continuously — and vt100 has been storing it all along with zero
    /// call sites reading it (P6). `pane {id}` for a pane that no longer has
    /// a spec (already closed by the time an event lands).
    pub fn display_name(&self, id: PaneId) -> String {
        let Some(spec) = self.find_spec(id) else { return format!("pane {id}") };
        display_name_live(spec, self.live_title(id).as_deref())
    }

    /// P6: the pane's current OSC 0/2 title, sanitized and bounded — `None`
    /// when the pane has published none (or nothing usable). Split out so
    /// the naming chain stays a pure function of (spec, live title).
    fn live_title(&self, id: PaneId) -> Option<String> {
        let raw = self.runtimes.get(&id)?.screen()?.title();
        let title = sanitize_title(raw, LIVE_TITLE_CAP);
        (!title.is_empty()).then_some(title)
    }

    /// U2: a feed entry's pane label — the pane id (the join key for
    /// `roost send <id>`) ahead of the display name, e.g. `3 shell · roost`.
    /// Falls back to `pane {id}` when the spec is already gone (no bare
    /// doubled `{id} pane {id}`).
    fn feed_label(&self, id: PaneId) -> String {
        match self.find_spec(id) {
            // P6: through `display_name`, so a feed line names a pane by its
            // live OSC title exactly like the badge does.
            Some(_) => format!("{id} {}", self.display_name(id)),
            None => format!("pane {id}"),
        }
    }

    /// C22: learns the float — `send`/`read`/badges/rename/respawn by id
    /// all route through this, so they work on the scratch pane exactly
    /// like any other, with zero extra code at those call sites.
    pub fn find_spec(&self, id: PaneId) -> Option<&PaneSpec> {
        if let Some(f) = &self.float {
            if f.id == id {
                return Some(&f.spec);
            }
        }
        self.ws.tabs.iter().find_map(|t| t.panes.get(&id))
    }

    /// The focused pane's working directory, `$HOME` abbreviated to `~`,
    /// for the tab bar's right status area (C2). `None` when the focused
    /// pane has no spec.
    pub fn focused_cwd(&self) -> Option<String> {
        let cwd = &self.find_spec(self.focused)?.cwd;
        Some(abbreviate_home(cwd, self.home.as_deref()))
    }

    /// C22: mirrors `find_spec` — the float's spec is mutable too (rename).
    fn find_spec_mut(&mut self, id: PaneId) -> Option<&mut PaneSpec> {
        if let Some(f) = &mut self.float {
            if f.id == id {
                return Some(&mut f.spec);
            }
        }
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
        let (mut needs, mut working, mut waiting, mut exited) = (false, false, false, false);
        for id in tab.panes.keys() {
            match self.runtimes.get(id) {
                Some(rt) => match rt.status() {
                    AgentStatus::NeedsInput => needs = true,
                    AgentStatus::Working => working = true,
                    AgentStatus::Waiting => waiting = true,
                    AgentStatus::Exited => exited = true, // U13
                    AgentStatus::Idle => {}
                },
                // No runtime and not a known spawn-failure ⇒ not spawned yet.
                None if !self.dead.contains_key(id) => any_unknown = true,
                // A recorded spawn failure is a dead pane too (U13).
                None => exited = true,
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
        } else if exited {
            // U13: ranked below Waiting (a live agent outranks a corpse) and
            // above Quiet (a dead pane is news; an idle one isn't).
            TabSummary::Exited
        } else {
            TabSummary::Quiet
        }
    }

    /// Count of panes across **every** tab whose runtime status is
    /// `NeedsInput` — the hint bar's aggregate "◆ N needs you" (C9). Scans
    /// `runtimes` directly rather than `tab_summary` (which reports one tab
    /// at a time and collapses multiple needs-input panes to a single flag),
    /// so a pane in a background tab still counts.
    pub fn needs_input_count(&self) -> usize {
        self.runtimes.values().filter(|rt| rt.status() == AgentStatus::NeedsInput).count()
    }

    /// Whether the focused pane negotiated the kitty keyboard protocol, so the
    /// input layer knows whether to send modified Enter as CSI-u or the legacy
    /// fallback.
    pub fn focused_kitty(&self) -> bool {
        self.runtimes.get(&self.focused).map(|rt| rt.kitty_disambiguate()).unwrap_or(false)
    }

    /// Whether the focused pane's app enabled DECCKM application cursor keys
    /// mode, so the input layer sends cursor keys as SS3 the way a real
    /// terminal would (zsh's `smkx`-bound widgets — atuin's up-arrow among
    /// them — listen for exactly those sequences).
    pub fn focused_app_cursor(&self) -> bool {
        self.runtimes.get(&self.focused).map(|rt| rt.app_cursor_keys()).unwrap_or(false)
    }

    /// P7: the DECSCUSR shape the focused pane asked for, `None` for roost's
    /// own default. There is one real cursor, so only the focused pane's
    /// shape is ever mirrored to the host — moving focus to a pane that
    /// asked for nothing therefore restores the default, with no special
    /// case. A pane whose view is scrolled back, or that hid its cursor,
    /// isn't showing one at all; the renderer's placement gate covers that,
    /// and a shape without a cursor is invisible either way.
    pub fn focused_cursor_shape(&self) -> Option<u8> {
        self.runtimes.get(&self.focused)?.cursor_shape()
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
            Method::Broadcast { text, submit } => self.ctl_broadcast(actor, &text, submit),
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
            // Broadcast self-audits inside ctl_broadcast with the real
            // fan-out count — the generic post-dispatch audit below only
            // ever sees an opaque `Reply::Ok`, so it can't produce that
            // detail. Handled here, mirroring the Wait special-case above,
            // so a broadcast is never audited (or fed) twice.
            (Some(actor), Method::Broadcast { text, submit }) => {
                let r = self.ctl_broadcast(actor, &text, submit);
                let _ = reply.send(r);
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

    /// Append a control action to `<state>/control.log` (unconditional —
    /// every spawn/send/read/close/etc. that touches the fleet is recorded
    /// with who did it, what, and the outcome) AND to the C20 activity feed
    /// (the feed push happens even when there's no socket dir — session-only
    /// state doesn't need one).
    fn audit(&mut self, actor: Option<Actor>, summary: &str, ok: bool, detail: &str) {
        let principal = match actor {
            Some(Actor::Fleet) => "fleet".to_string(),
            Some(Actor::Pane(id)) => format!("pane:{id}"),
            None => "?".to_string(),
        };
        let outcome = if ok { "ok" } else { "err" };
        self.push_feed(format!("ctl {principal}: {} → {outcome}", sanitize(summary)), false);

        let Some(dir) = self.sock_path.as_ref().and_then(|p| p.parent()) else { return };
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
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
        self.waiters.push(Waiter { actor, panes, until, reply, deadline });
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
            // LOW-3: re-verify authorization at fire time, not just at
            // registration. If `p` now resolves to a *live* spec (it may
            // have been closed and recycled to an unrelated pane since
            // this waiter registered) that the waiter isn't authorized for,
            // never disclose its status — treat it as not-yet-matched. A
            // fully gone id (no live spec at all) still resolves normally
            // below: "it's gone" discloses nothing pane-specific, so it
            // needs no re-check.
            let hit = w.panes.iter().copied().find(|&p| {
                if self.find_spec(p).is_some() && !self.may_target(w.actor, p) {
                    return false;
                }
                self.pane_matches(p, w.until)
            });
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
            // Don't write yet: the agent's stdin reader may not be up this
            // instant after spawn, silently dropping the bytes. Buffer and
            // flush on the pane's first output (see on_pty_output) — the
            // minimal reliable "it's alive and reading" signal.
            self.pending_input.insert(id, bytes);
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
        // LOW-2: a pane can look running (status snapshot) yet have a dead
        // write pipe (e.g. it just exited) — don't report bytes as sent
        // unless the write actually succeeded.
        if !rt.write_input(&bytes) {
            return Reply::err("pane did not accept input");
        }
        Reply::ok(serde_json::json!({ "sent": bytes.len() }))
    }

    /// Deliver `text` (+ CR when `submit`) to every **running** pane `actor`
    /// may target. Fleet reaches every spawned pane by iterating `runtimes`
    /// directly (not `ws.tabs`) — except the float (C22): it's the human's
    /// private interactive scratch shell, never a fleet member, so it is
    /// always excluded from the fan-out regardless of actor; a pane actor
    /// reaches only its own spawned subtree, itself included — the exact
    /// same `may_target`/`in_subtree` rule as every other verb. "Running"
    /// excludes a pane whose process already exited but hasn't been closed
    /// (its runtime lingers with `AgentStatus::Exited` — see `on_pty_exit`):
    /// writing to it would silently go nowhere, so counting it as "sent"
    /// would be a dishonest reply/audit — same reasoning covers a pane that
    /// *looked* running in that snapshot but whose write pipe is actually
    /// dead (it exited a moment later): `sent`/`count` only include panes
    /// whose write call actually succeeded. There's no per-target id to
    /// validate (unlike send/read/close), so this never errors — zero
    /// matching panes is `ok` with count 0.
    fn ctl_broadcast(&mut self, actor: Actor, text: &str, submit: bool) -> Reply {
        let mut bytes = text.as_bytes().to_vec();
        if submit {
            bytes.push(b'\r');
        }
        let mut targets: Vec<PaneId> = Vec::new();
        for (&id, rt) in self.runtimes.iter() {
            if rt.status() != AgentStatus::Exited && !self.is_float(id) && self.may_target(actor, id)
            {
                targets.push(id);
            }
        }
        targets.sort_unstable();
        let mut sent: Vec<PaneId> = Vec::new();
        for &id in &targets {
            if let Some(rt) = self.runtimes.get_mut(&id) {
                if rt.write_input(&bytes) {
                    sent.push(id);
                }
            }
        }
        let count = sent.len();
        // Self-audit with the real count — the generic post-dispatch audit
        // in handle_control_msg only ever sees an opaque `Reply::Ok` (it
        // can't produce this contract's `count=` detail), so
        // handle_control_msg special-cases Broadcast to call this directly
        // instead of going through dispatch + that generic audit. This is
        // the only audit call for a broadcast: one `ctl` line per call, in
        // both control.log and the C20 feed.
        let summary = method_summary(&Method::Broadcast { text: text.to_string(), submit });
        self.audit(Some(actor), &summary, true, &format!("count={count}"));
        Reply::ok(serde_json::json!({ "sent": sent, "count": count }))
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
        let text = match mode {
            // grab_text clamps to the screen, so (0,0)..MAX is the whole
            // visible grid — deliberately bounded, unlike Full/Tail below.
            ReadMode::Screen => rt.grab_text((0, 0), (u16::MAX, u16::MAX)),
            ReadMode::Full => rt.grab_all_text(),
            ReadMode::Tail(n) => {
                let full = rt.grab_all_text();
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
        // C22: the scratch pane never closes via the control plane —
        // checked for every caller (including Fleet), before authz, since
        // the refusal is unconditional either way.
        if self.is_float(pane) {
            return Reply::err("cannot close the scratch pane");
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
        // C21: closing the pane you're zoomed on ends the zoomed view —
        // there's nothing left to show full-screen. A control-plane close of
        // some *other* pane leaves an unrelated zoom alone.
        if self.zoomed && id == self.focused {
            self.exit_zoom();
        }
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
        // A pane closed before its first output never got to flush its
        // buffered initial_input (see on_pty_output) — drop it too, rather
        // than leaking an entry keyed on an id that's gone for good.
        self.pending_input.remove(&id);
        // C23: pane ids are never reused, so a stale raw-membership entry is
        // harmless — but cheap to clean up alongside every other per-pane
        // side table above.
        self.raw.remove(&id);
        let tab = &mut self.ws.tabs[ti];
        tab.panes.remove(&id);
        // U11: a closed pane is no longer anyone's focus memory — pane ids
        // are recycled (`next_pane_id` is a max+1), so a stale entry could
        // otherwise be inherited by an unrelated pane in another tab.
        self.tab_focus.remove(&id);
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
            // C20: one line per close_pane_id call — a tab removal doesn't
            // also get a pane-level "closed" line for the pane that emptied it.
            self.push_feed(format!("closed tab {}", tab_snapshot.name), false);
            self.remember_closed(Closed::Tab { index: ti, tab: tab_snapshot });
            self.spawn_active_tab();
        } else if !empty {
            if let Some(spec) = spec {
                // U2: the spec is already out of the tree, so the label is
                // built from the captured spec (same `{id} {name}` shape as
                // `feed_label`).
                self.push_feed(format!("closed {id} {}", display_name_of(&spec)), false);
                self.remember_closed(Closed::Pane { tab_index: ti, spec });
            }
        }
        // The active tab's membership may have changed out from under
        // `focused` (its pane closed, or its tab shifted/removed above) —
        // keep focus inside whatever tab is now on screen rather than
        // routing keystrokes to a pane in a tab nobody's looking at. U11:
        // being moved onto a tab is a tab switch like any other, so honor
        // that tab's focus memory before falling back to its first pane.
        if !self.ws.active_tab().panes.contains_key(&self.focused) {
            let active = self.ws.active_tab;
            self.focused = self
                .tab_focus_target(active)
                .or_else(|| self.pane_order().first().copied())
                .unwrap_or(0);
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

    /// Returns a notification message when the pane's escape traffic asked
    /// for one (P2: OSC 9 / OSC 777) and the pane isn't focused — same
    /// policy as `on_status`/`on_pty_exit`: a pane you're already looking at
    /// doesn't need to be announced. The pane's *attention state* (◆) and
    /// the host re-emission are unconditional; only the roost-side nudge is
    /// gated.
    pub fn on_pty_output(&mut self, id: PaneId, bytes: &[u8]) -> Option<String> {
        let effects = {
            let rt = self.runtimes.get_mut(&id)?;
            rt.process_output(bytes);
            // First output ⇒ the agent is alive and reading its stdin: flush
            // any buffered spawn-time initial_input now (exactly once). A
            // pane that never emits any output would never get it, but every
            // agent prints a banner/prompt, so that's acceptable — a
            // tick-based fallback flush would be the upgrade path if not.
            if let Some(pending) = self.pending_input.remove(&id) {
                rt.write_input(&pending);
            }
            rt.take_effects()
        };
        // W3: bytes bound for the host terminal are queued, never written
        // from here — core has no I/O of its own, and the composition root
        // must place them between frames rather than mid-draw.
        self.host_out.extend_from_slice(&effects.host_writes);
        // A chunk can carry several notifications (an agent that re-notifies
        // as its state changes); the newest is the one that's still true, so
        // it's the one the user is told about — the rest already did their
        // job by marking the pane as needing attention.
        let body = effects.notifications.into_iter().next_back()?;
        if id == self.focused {
            return None;
        }
        // U2: the shared display name, so a notification names the pane the
        // same way the badge and the feed do.
        Some(format!("{}: {}", self.display_name(id), body))
    }

    /// W3: drain the bytes roost owes the HOST terminal — pane OSC 9
    /// notifications (P2), OSC 52 clipboard writes (P3) and the window title
    /// (P6), already rate-limited/capped/sanitized. The composition root
    /// writes these between draws; queueing them keeps core I/O-free and
    /// guarantees nothing lands in the middle of a frame.
    pub fn take_host_output(&mut self) -> Vec<u8> {
        self.sync_host_title();
        std::mem::take(&mut self.host_out)
    }

    /// P6: keep the host terminal's title at `roost · {focused pane}`. Queues
    /// an OSC 2 only when the text actually changes, and no more often than
    /// `HOST_TITLE_INTERVAL` — an agent that republishes its OSC title on
    /// every spinner frame would otherwise repaint the host title bar ~30x a
    /// second. A change skipped by the throttle is picked up on the next
    /// call, since the comparison is against what was last *published*.
    fn sync_host_title(&mut self) {
        if self.last_host_title.is_some_and(|t| t.elapsed() < HOST_TITLE_INTERVAL) {
            return;
        }
        // `display_name` is already sanitized and bounded (`live_title`), and
        // an Alt+r title comes from roost's own rename buffer — so nothing
        // here can carry a control byte into the sequence.
        let want = format!("roost · {}", self.display_name(self.focused));
        if want == self.host_title {
            return;
        }
        self.last_host_title = Some(Instant::now());
        self.host_title = want;
        self.host_out.extend_from_slice(b"\x1b]2;");
        self.host_out.extend_from_slice(self.host_title.as_bytes());
        self.host_out.push(0x07);
    }

    /// Returns a notification message when a *non-focused* pane exits — an
    /// exited pane is as attention-worthy as one needing input, but its
    /// recovery hint is only visible inside its own borders, so it otherwise
    /// gets no pull toward it (regression: same fix as `on_status`).
    pub fn on_pty_exit(&mut self, id: PaneId) -> Option<String> {
        if let Some(rt) = self.runtimes.get_mut(&id) {
            rt.on_exit();
        }
        // A pane the user just closed (Alt+w) is already gone from the
        // workspace by the time its process EOFs — that Exit is expected, so
        // there's no name to report: nothing to log (the close hook already
        // did) or notify about.
        self.find_spec(id)?;
        // C20: the feed logs every exit, focused pane included — unlike the
        // notification below, which only nudges for an *unfocused* pane (a
        // focused one's recovery hint is already on screen). U2: the feed
        // line carries the pane id; the notification the display name.
        self.push_feed(format!("{} exited", self.feed_label(id)), false);
        if id == self.focused {
            return None;
        }
        Some(format!("{} exited", self.display_name(id)))
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
            // U2: the shared display name — "shell · roost is waiting for
            // you", not an anonymous "shell is waiting for you".
            Some(format!("{} is waiting for you", self.display_name(id)))
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

    /// Forward a host paste to the focused pane. A pane whose app switched on
    /// bracketed paste (mode 2004) gets the text wrapped in the
    /// `ESC[200~`/`ESC[201~` guards it asked for — that's what lets zsh/vim/
    /// agent TUIs insert pasted newlines instead of executing them line by
    /// line. Any guard sequence *inside* the pasted text is stripped first:
    /// left alone, an embedded `ESC[201~` would end the bracket early and the
    /// remainder would be interpreted as typed input (paste injection — tmux
    /// strips these too). A pane without the mode gets the bytes verbatim,
    /// exactly like a terminal with bracketed paste off.
    /// U8(b): route a host paste by mode — the composition root's whole
    /// `Event::Paste` handling. A modal owns the paste: `Rename` takes the
    /// text into its buffer (printables only, so a pasted newline can't
    /// commit the rename and no control byte can reach a title), and the
    /// other three modals swallow it; every other mode forwards to the
    /// focused pane exactly as before. Pre-U8 a paste during Rename went
    /// to the pane *under* the dialog — live QA typed `PSTX` into a hidden
    /// shell while the dialog's own buffer ignored it.
    pub fn handle_paste(&mut self, text: &str) {
        if let Mode::Rename { buffer, .. } = &mut self.mode {
            buffer.extend(text.chars().filter(|c| !c.is_control()));
            return;
        }
        if self.modal_active() {
            return; // Picker / Help / Feed: nothing to type into
        }
        self.forward_paste(text);
    }

    pub fn forward_paste(&mut self, text: &str) {
        let id = self.focused;
        let Some(rt) = self.runtimes.get_mut(&id) else { return };
        if rt.bracketed_paste() {
            let clean = text.replace("\x1b[200~", "").replace("\x1b[201~", "");
            let mut bytes = Vec::with_capacity(clean.len() + 12);
            bytes.extend_from_slice(b"\x1b[200~");
            bytes.extend_from_slice(clean.as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
            rt.write_input(&bytes);
        } else {
            rt.write_input(text.as_bytes());
        }
    }

    /// `pixels` is the host window's pixel geometry, re-read alongside the
    /// cell size — the two travel together on every real resize.
    pub fn on_resize(&mut self, size: Size, pixels: (u16, u16)) {
        self.term_size = size;
        self.host_pixels = pixels;
        self.relayout();
    }

    /// Recompute rects and push new sizes to every pane backend. C21: driven
    /// by `display_rects`, so while zoomed only the zoomed pane's PTY is
    /// resized (to the full body) — every other pane keeps its last size
    /// until unzoom relayouts them (no reflow churn while reading).
    pub fn relayout(&mut self) {
        for pr in self.display_rects() {
            if pr.collapsed {
                continue;
            }
            let (rows, cols) = inner_dims(pr.rect);
            let pixels = self.pane_pixels(rows, cols);
            if let Some(rt) = self.runtimes.get_mut(&pr.id) {
                rt.resize(rows, cols, pixels);
            }
        }
    }

    /// P4: a pane's pixel geometry — a proportional share of the host
    /// window's, scaled by the pane's cell size (`host_px * pane_cells /
    /// host_cells`). (0, 0) whenever the host geometry is unknown: the
    /// honest "no pixels" stays 0 all the way down to the PTY winsize and
    /// the XTWINOPS 14/16t silence.
    fn pane_pixels(&self, rows: u16, cols: u16) -> (u16, u16) {
        let (host_w, host_h) = self.host_pixels;
        let (host_cols, host_rows) = (self.term_size.width, self.term_size.height);
        if host_w == 0 || host_h == 0 || host_cols == 0 || host_rows == 0 {
            return (0, 0);
        }
        let scale = |px: u16, cells: u16, host_cells: u16| -> u16 {
            (u32::from(px) * u32::from(cells) / u32::from(host_cells))
                .min(u32::from(u16::MAX)) as u16
        };
        (scale(host_w, cols, host_cols), scale(host_h, rows, host_rows))
    }

    /// The focused pane's inner (border-excluded) cell dimensions as
    /// `(rows, cols)`, from the current display list — so a zoomed or
    /// floated pane's *actual* on-screen size is used, matching the
    /// (row, col) convention `Mode::Copy`'s cursor and `Selection` share.
    /// `(1, 1)` if the focused pane isn't currently displayed (shouldn't
    /// happen, but keeps the C24 cursor math panic-free either way).
    fn focused_inner_dims(&self) -> (u16, u16) {
        self.display_rects()
            .iter()
            .find(|pr| pr.id == self.focused)
            .map(|pr| inner_dims(pr.rect))
            .unwrap_or((1, 1))
    }

    // -- copy mode / selection --------------------------------------------

    pub fn in_copy_mode(&self) -> bool {
        matches!(self.mode, Mode::Copy { .. })
    }

    /// Start a selection in pane `id` at inner cell (row, col). C24: also
    /// moves the keyboard cursor there, so a fresh mouse drag and the
    /// keyboard cursor never disagree about where the selection is.
    pub fn begin_selection(&mut self, id: PaneId, row: u16, col: u16) {
        self.selection = Some(Selection { pane: id, anchor: (row, col), cursor: (row, col), dragging: true });
        if let Mode::Copy { cursor } = &mut self.mode {
            *cursor = (row, col);
        }
    }

    /// Extend the active drag to inner cell (row, col). C24: a drag "also
    /// moves the cursor to the drag point" — the two input methods write
    /// the same state either way.
    pub fn extend_selection(&mut self, row: u16, col: u16) {
        if let Some(sel) = &mut self.selection {
            if sel.dragging {
                sel.cursor = (row, col);
            }
        }
        if let Mode::Copy { cursor } = &mut self.mode {
            *cursor = (row, col);
        }
    }

    /// U14: the hint-bar text for a finished copy of `chars` characters,
    /// given which clipboard channel actually took it. The flash used to
    /// fire at extraction time and claim `copied N chars` unconditionally —
    /// while `clipboard::copy` discarded both channels' results, so an
    /// empty clipboard reported a successful copy. Pure, so the wording is
    /// pinned without touching a clipboard.
    pub fn copy_flash_text(chars: usize, outcome: ClipboardOutcome) -> String {
        match outcome {
            ClipboardOutcome::Native => format!("copied {chars} chars"),
            // Sent, unacknowledged: OSC 52 has no reply, and the terminal
            // may not be listening. The qualifier is the whole point — it
            // tells you where to look when the paste comes up empty.
            ClipboardOutcome::Osc52 => format!("copied {chars} chars (OSC 52)"),
            ClipboardOutcome::Failed => "copy failed".to_string(),
        }
    }

    /// U14: flash the result of a copy the composition root just performed
    /// (core has no clipboard I/O of its own — same division as
    /// `pending_yank`). Both copy paths, mouse and keyboard, end here.
    pub fn flash_copy(&mut self, chars: usize, outcome: ClipboardOutcome) {
        self.set_flash(Self::copy_flash_text(chars, outcome));
    }

    /// Finish the drag: extract the selected text and leave copy mode.
    /// Returns the text to hand to the clipboard (None when the selection
    /// is empty). U14: the "copied" flash is *not* set here — it is set by
    /// the caller once the clipboard has actually answered (`flash_copy`).
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
        Some(text)
    }

    /// Set a transient hint-bar message (e.g. a startup notice).
    pub fn set_flash(&mut self, msg: impl Into<String>) {
        self.flash = Some((msg.into(), Instant::now(), FLASH_WINDOW));
    }

    /// Set a confirm-arm prompt ("… again to close/quit"). Carries
    /// `CONFIRM_WINDOW`, not `FLASH_WINDOW`: the prompt must stay visible for
    /// exactly as long as the second press would still fire (U22 — the old
    /// 2s flash under a 3s arm left a silent final second that accepted a
    /// destructive confirm with no visible prompt).
    fn set_confirm_flash(&mut self, msg: impl Into<String>) {
        self.flash = Some((msg.into(), Instant::now(), CONFIRM_WINDOW));
    }

    /// Drop a confirm-arm prompt from the bar, if that's what's showing.
    /// U22's contract read in both directions: when the arm dies early (a
    /// confirmed second press, or any other action disarming it), its prompt
    /// must die with it — a visible "again to close" with nothing armed is
    /// the mismatch lie inverted. Only `set_confirm_flash` produces
    /// `CONFIRM_WINDOW`-sized flashes, so the window is the marker; an
    /// ordinary flash some action just set is left alone.
    fn clear_confirm_flash(&mut self) {
        if self.flash.as_ref().is_some_and(|(_, _, w)| *w == CONFIRM_WINDOW) {
            self.flash = None;
        }
    }

    /// Current transient hint-bar message, if still within its window.
    pub fn flash(&self) -> Option<&str> {
        self.flash
            .as_ref()
            .filter(|(_, at, window)| at.elapsed() < *window)
            .map(|(m, _, _)| m.as_str())
    }

    /// The URL under inner cell (row, col) of pane `id`, if any (for
    /// Alt+click-to-open). Reads that row's text from the pane grid.
    pub fn url_at(&self, id: PaneId, row: u16, col: u16) -> Option<String> {
        let line = self.runtimes.get(&id)?.grab_text((row, 0), (row, u16::MAX));
        find_url_at(&line, col as usize)
    }

    // -- mouse -------------------------------------------------------------

    /// Left click: focus the pane under the cursor (expanding stack members).
    /// C22 rule 2: a click outside the float's rect hides it first — the
    /// click itself still lands normally on whatever it hit (below).
    /// Clicking the float's own rect (id == the already-focused float) is
    /// not "outside" and leaves it shown.
    pub fn on_click(&mut self, id: PaneId) {
        if self.float_focused() && id != self.focused {
            self.hide_float();
        }
        self.focused = id;
        layout::expand_in_stacks(&mut self.ws.active_tab_mut().layout, id);
        self.relayout();
        self.save();
    }

    /// U8: is a modal overlay up? These four modes own the whole
    /// non-keyboard input surface — mouse and paste alike — while they are
    /// active. Scroll/Copy are deliberately excluded: they draw no dialog,
    /// nothing is hidden underneath them, and their mouse behavior (wheel
    /// scrolls, drag selects) is the point.
    pub fn modal_active(&self) -> bool {
        matches!(
            self.mode,
            Mode::Rename { .. } | Mode::Picker { .. } | Mode::Help | Mode::Feed { .. }
        )
    }

    /// U8(a): the whole mouse path while a modal is up — the composition
    /// root routes here instead of the pane/tab path (gated on
    /// `modal_active`, the same shape copy mode already uses), so nothing
    /// beneath the dialog can be mutated: no focus change, no tab switch, no
    /// pane scroll, nothing forwarded to a pane's app. `dialog` is the
    /// modal's *drawn* rect (`render::modal_rect`), so hit-testing can never
    /// disagree with what's on screen (renderer/hitbox lockstep, §4/§5).
    ///
    /// Rules: the wheel scrolls the feed by its own PgUp/PgDn step and is
    /// swallowed by every other modal; a left click outside the dialog
    /// dismisses Picker/Feed and any click dismisses Help (C15's "any key
    /// closes it", in mouse form); a click on a picker row selects and
    /// launches it. `Rename` is the one carve-out — see below.
    pub fn handle_modal_mouse(&mut self, me: crossterm::event::MouseEvent, dialog: Option<Rect>) {
        use crossterm::event::{MouseButton, MouseEventKind};
        // The feed owns the wheel wherever the pointer sits: the panes
        // beneath are unreachable while it's up, so position-routing would
        // only mean "the wheel does nothing here" — and reaching the pane
        // under the overlay is U8(c), the bug itself.
        let page = self.feed_page();
        let cap = self.feed.len().saturating_sub(1);
        if let Mode::Feed { offset } = &mut self.mode {
            match me.kind {
                MouseEventKind::ScrollUp => *offset = (*offset + page).min(cap),
                MouseEventKind::ScrollDown => *offset = offset.saturating_sub(page),
                _ => {}
            }
        }
        let inside = dialog.is_some_and(|r| {
            me.column >= r.x
                && me.column < r.x.saturating_add(r.width)
                && me.row >= r.y
                && me.row < r.y.saturating_add(r.height)
        });
        if !matches!(me.kind, MouseEventKind::Down(MouseButton::Left)) {
            return; // wheel handled above; drag/release/motion are swallowed
        }
        // Rename is deliberately NOT dismissed by an outside click: its
        // buffer is unsaved work, and throwing it away on a stray click is
        // U8(a)'s harm inverted (the live QA script types `ZZZ`, clicks
        // another pane, and expects the commit to land on the pane the
        // dialog opened on). Esc/Enter stay the ways out.
        if matches!(self.mode, Mode::Rename { .. }) {
            return;
        }
        if matches!(self.mode, Mode::Picker { .. }) {
            let rows = picker_items().len();
            match dialog.and_then(|r| crate::ui::mouse::picker_row_at(r, rows, me.column, me.row)) {
                Some(i) => self.picker_launch(i),
                None if !inside => self.mode = Mode::Normal,
                None => {}
            }
            return;
        }
        if matches!(self.mode, Mode::Help) {
            self.mode = Mode::Normal;
            return;
        }
        if matches!(self.mode, Mode::Feed { .. }) && !inside {
            self.mode = Mode::Normal;
        }
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

    /// U3: pane `id`'s grid-clamped view offset — > 0 means its on-screen
    /// grid is frozen history, which the corner badge must mark (`↑N`) and
    /// the Working pulse must stop asserting liveness over (N1). 0 for a
    /// pane with no runtime.
    pub fn scroll_offset(&self, id: PaneId) -> usize {
        self.runtimes.get(&id).map(|rt| rt.scroll_offset()).unwrap_or(0)
    }

    /// U3: the focused pane's `(view offset, banked history rows)` — the
    /// scroll-mode hint's `↑N/M`. Read from the backend (the view's truth),
    /// not from `Mode::Scroll`'s own counter, so the hint can never report
    /// a phantom position the grid refused (U9's overshoot).
    pub fn scroll_position(&self) -> (usize, usize) {
        self.runtimes
            .get(&self.focused)
            .map(|rt| (rt.scroll_offset(), rt.scroll_total()))
            .unwrap_or((0, 0))
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
        // display_rects, not rects: this bypasses apply()'s trailing
        // relayout(), so a zoomed dead pane must be spawned at the size it's
        // actually shown at (full body) — not its stale tiled rect.
        if let Some(pr) = self.display_rects().iter().find(|pr| pr.id == id).copied() {
            self.spawn_pane(id, &spec, pr.rect);
        }
        self.save();
    }

    // -- actions -----------------------------------------------------------

    pub fn apply(&mut self, action: Action) {
        // Any action other than a repeated close/quit disarms that pending
        // confirmation, so a stale "press again" can't leak onto a later
        // key — and a disarmed confirm's prompt goes with it (U22: prompt
        // and arm live and die together). Runs BEFORE the action: a confirm
        // the action itself arms below sets a fresh prompt that must not be
        // swept up with the stale one (Alt+w-armed then Alt+q must leave
        // the quit's own prompt standing).
        let mut disarmed = false;
        if !matches!(action, Action::ClosePane) {
            disarmed |= self.confirm_close.take().is_some();
        }
        if !matches!(action, Action::Quit) {
            disarmed |= self.confirm_quit.take().is_some();
        }
        if disarmed {
            self.clear_confirm_flash();
        }
        // C21/C22: a structural layout action must not change the tab
        // invisibly behind a full-screen zoomed pane, nor target the float
        // (which lives outside the layout tree — leaving it focused here is
        // `spawn_child`'s empty-tab-fallback layout-wipe hazard waiting to
        // happen). Leave zoom, then hide the float (restoring `prev_focus`),
        // then apply below — exhaustive trigger list; tab changes exit zoom
        // and hide the float too, handled inside `new_tab`/`go_to_tab` since
        // only a *real* switch should count; Focus/JumpAttention hide the
        // float from inside their own functions (rule 2); Alt+z is
        // deliberately excluded from the float rule — it gets its own
        // "can't zoom the float" no-op below instead of retargeting to
        // `prev_focus`.
        if matches!(
            action,
            Action::NewPane
                | Action::ToggleStack
                | Action::FlipSplit
                | Action::Resize { .. }
                | Action::CycleLayout
        ) {
            self.exit_zoom();
            self.hide_float();
        }
        match action {
            Action::Quit => self.quit_guarded(),
            Action::NewPane => self.new_pane_with("shell"),
            Action::ClosePane => self.close_pane(),
            Action::Focus(dir) => self.focus_dir(dir),
            Action::NewTab => self.new_tab(),
            Action::GoToTab(i) => self.go_to_tab(i),
            Action::NextTab => self.step_tab(1),
            Action::PrevTab => self.step_tab(-1),
            Action::LastTab => self.go_to_tab(self.ws.tabs.len().saturating_sub(1)),
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
            Action::ScrollMode => {
                // U9: seed from the pane's CURRENT view offset — entering
                // Scroll mode after wheeling continues from the wheeled
                // position; a zero seed made the first keypress snap the
                // view from there back toward the live tail.
                let offset = self.scroll_offset(self.focused);
                self.mode = Mode::Scroll { offset };
            }
            Action::CopyMode => {
                // C24: cursor starts bottom-left of the focused pane's inner
                // grid (C22 rule 1: targets the float like any pane when
                // it's the one focused).
                let (h, _w) = self.focused_inner_dims();
                self.mode = Mode::Copy { cursor: (h.saturating_sub(1), 0) };
                self.selection = None;
            }
            Action::ToggleHints => self.hints = !self.hints,
            Action::Undo => self.undo_close(),
            Action::Help => self.mode = Mode::Help,
            Action::JumpAttention => self.jump_attention(),
            Action::ToggleZoom => {
                // C21: zooming the float makes no sense (it's already a
                // floating full-focus surface) — refuse rather than hide it
                // and zoom whatever's behind it.
                if self.float_focused() {
                    self.set_flash("can't zoom the float");
                } else {
                    self.toggle_zoom();
                }
            }
            Action::CycleLayout => self.cycle_layout(),
            Action::ToggleFeed => self.toggle_feed(),
            Action::ToggleFloat => self.toggle_float(),
            Action::ToggleRaw => self.toggle_raw(),
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
            self.set_flash("nothing to reopen");
            return;
        };
        match closed {
            Closed::Tab { index, tab } => {
                let name = tab.name.clone();
                self.remember_tab_focus(); // U11: reopening a tab leaves this one
                let i = index.min(self.ws.tabs.len());
                self.ws.tabs.insert(i, tab);
                self.ws.active_tab = i;
                self.spawn_active_tab();
                self.focused = self.pane_order().first().copied().unwrap_or(0);
                self.push_feed(format!("reopened tab {name}"), false);
                // U2: flashes name what they acted on.
                self.set_flash(format!("reopened tab {name}"));
            }
            Closed::Pane { tab_index, spec } => {
                // Restore into its original tab if it still exists, else the
                // active one; split the focused pane and reuse the saved spec
                // (session id preserved ⇒ the agent resumes).
                self.ws.active_tab = tab_index.min(self.ws.tabs.len().saturating_sub(1));
                self.focused = self.pane_order().first().copied().unwrap_or(0);
                self.restore_pane(spec);
                // U2: `restore_pane` allocated the pane's NEW id and left it
                // focused — label with that id, not the closed one's.
                let restored = self.focused;
                self.push_feed(format!("reopened {}", self.feed_label(restored)), false);
                self.set_flash(format!("reopened {}", self.display_name(restored)));
            }
        }
    }

    /// Insert `spec` as a new pane split off the focused pane, spawning it.
    /// Shared by undo (reuses a saved spec, session and all).
    fn restore_pane(&mut self, spec: PaneSpec) {
        let id = self.alloc_pane_id();
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
        // C22 rule 2: the float has no spatial position in the tiled tree,
        // so leaving it via a directional key always just returns focus to
        // `prev_focus` — `dir` doesn't apply to it.
        if self.float_focused() {
            self.hide_float();
            return;
        }
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
        let id = self.alloc_pane_id();
        // C22: split off a pane actually in the active tab's tree.
        // `self.focused` can be the float, which lives outside it — the
        // interactive callers (Alt+n, picker launch) already hide the float
        // before reaching here (apply()'s C22 rule-3 pre-step), but the
        // control-plane spawn/fork paths have no such pre-step (they
        // save/restore `focused` only to protect the human's on-screen
        // view, not to pick a safe split target). Splitting "off" an id the
        // tree doesn't contain is exactly what trips `split_pane`'s
        // empty-tab fallback below and wipes the whole tab's layout — this
        // fallback closes that hole for every caller at once.
        let split_target = if self.ws.active_tab().panes.contains_key(&self.focused) {
            self.focused
        } else {
            self.pane_order().first().copied()?
        };
        let cwd = cwd.unwrap_or_else(|| {
            self.ws
                .active_tab()
                .panes
                .get(&split_target)
                .map(|s| s.cwd.clone())
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
        });
        let spec = PaneSpec { adapter: adapter.into(), cwd, session: None, title: None, spawned_by };

        // Split in the widest direction of the split target's rect.
        let target_rect = self.rects().iter().find(|pr| pr.id == split_target).map(|pr| pr.rect);
        let dir = target_rect
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
        // is left untouched. See layout::MIN_SPLIT_* (also the C25 fit floor).
        if let Some(r) = target_rect {
            let too_small = match dir {
                SplitDir::Vertical => r.width < layout::MIN_SPLIT_COLS,
                SplitDir::Horizontal => r.height < layout::MIN_SPLIT_ROWS,
            };
            if too_small {
                return None;
            }
        }

        let tab = self.ws.active_tab_mut();
        tab.panes.insert(id, spec.clone());
        if !layout::split_pane(&mut tab.layout, split_target, id, dir) {
            tab.layout = LayoutNode::Pane(id); // empty tab fallback (now only for a genuinely-empty tab)
        }
        self.focused = id;
        if let Some(pr) = self.rects().iter().find(|pr| pr.id == id).copied() {
            self.spawn_pane(id, &spec, pr.rect);
        }
        Some(id)
    }

    fn close_pane(&mut self) {
        // C22 rule 4: Alt+w on the float kills it for real, no confirm
        // guard (scratch is not precious — the whole point of the confirm
        // guard below is protecting work the float explicitly isn't).
        if self.float_focused() {
            self.close_float();
            return;
        }
        let id = self.focused;

        // Destructive-close guard. Closing a *busy* agent loses its in-flight
        // turn, and closing the last pane quits roost outright (which undo
        // can't recover). In those cases, arm a confirmation and require a
        // second Alt+w within the window; a non-busy pane closes immediately
        // (undo covers an accidental one). Busy is `Working || NeedsInput`
        // (U12): an agent blocked on your approval is mid-turn all the same.
        let is_busy = self.runtimes.get(&id).map(|rt| is_busy(rt.status())).unwrap_or(false);
        let would_quit = self.ws.tabs.len() == 1 && self.ws.active_tab().panes.len() == 1;
        let armed = self.confirm_close.is_some_and(|t| t.elapsed() < CONFIRM_WINDOW);
        if (is_busy || would_quit) && !armed {
            self.confirm_close = Some(Instant::now());
            let msg = if would_quit {
                "last pane — Alt+w again to quit roost".to_string()
            } else {
                // U2: name which agent the close would interrupt.
                format!("{} busy — Alt+w again to close", self.display_name(id))
            };
            self.set_confirm_flash(msg);
            return;
        }
        self.confirm_close = None;
        self.clear_confirm_flash(); // the arm was consumed; its prompt dies with it (U22)

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

    /// Alt+q. U1: quitting kills every pane's process at once, so it gets
    /// the same second-press machinery as `close_pane`'s busy guard — while
    /// any pane is busy (`Working || NeedsInput`, the shared U12 predicate),
    /// the first press arms and prompts; the second within the window quits.
    /// A quiet fleet still quits instantly: sessions resume on relaunch, so
    /// there's nothing in flight to protect.
    fn quit_guarded(&mut self) {
        let busy = self.runtimes.values().filter(|rt| is_busy(rt.status())).count();
        let armed = self.confirm_quit.is_some_and(|t| t.elapsed() < CONFIRM_WINDOW);
        if busy > 0 && !armed {
            self.confirm_quit = Some(Instant::now());
            let noun = if busy == 1 { "agent" } else { "agents" };
            self.set_confirm_flash(format!("{busy} {noun} busy — Alt+q again to quit"));
            return;
        }
        self.quit = true;
    }

    fn new_tab(&mut self) {
        self.exit_zoom(); // C21: any tab change exits zoom
        self.hide_float(); // C22 rule 2: "any tab change" hides the float too
        self.remember_tab_focus(); // U11: Alt+t leaves a tab like any switch
        let id = self.alloc_pane_id();
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

    /// U11: snapshot where focus sits in the tab we're about to leave, so
    /// coming back returns here. Called with `ws.active_tab` still pointing
    /// at the tab being left. Clearing that tab's panes out of the set
    /// first keeps the "at most one entry per tab" invariant `tab_focus`
    /// relies on; the float (which belongs to no tab) is never stored.
    fn remember_tab_focus(&mut self) {
        let focused = self.focused;
        let ids: Vec<PaneId> = self.ws.active_tab().panes.keys().copied().collect();
        if !ids.contains(&focused) {
            return; // the float, or a tab mid-edit — nothing honest to store
        }
        for id in ids {
            self.tab_focus.remove(&id);
        }
        self.tab_focus.insert(focused);
    }

    /// U11: the pane tab `i` should return focus to — its remembered pane
    /// if that pane is still alive in it, else None (callers fall back to
    /// the tab's first pane).
    fn tab_focus_target(&self, i: usize) -> Option<PaneId> {
        let tab = self.ws.tabs.get(i)?;
        tab.panes.keys().find(|id| self.tab_focus.contains(id)).copied()
    }

    fn go_to_tab(&mut self, i: usize) {
        if i >= self.ws.tabs.len() {
            return;
        }
        // U11: the digit for the tab you're already on is a no-op. It used
        // to run the whole switch: live QA pressed Alt+1 on tab 1 and lost
        // zoom, and the same path hid the float and reset focus to the
        // tab's first pane — a "switch" to nowhere, destroying view state.
        if i == self.ws.active_tab {
            return;
        }
        self.exit_zoom(); // C21: any (real) tab change exits zoom
        self.hide_float(); // C22 rule 2: any tab change hides the float
        self.remember_tab_focus(); // after hide_float: never store the float
        self.ws.active_tab = i;
        self.spawn_active_tab();
        // U11: land on the pane this tab was left on; a first visit (or a
        // remembered pane that has since closed) falls back to its first.
        self.focused = self
            .tab_focus_target(i)
            .or_else(|| self.pane_order().first().copied())
            .unwrap_or(self.focused);
    }

    /// U7: step `delta` tabs, wrapping at both ends — Alt+m forward, Alt+i
    /// back. Wrapping is what makes the strip navigable with two keys: no
    /// dead end at either edge, and tabs past the ninth (unreachable by
    /// digit) are always a few presses away. A single tab wraps onto itself
    /// and `go_to_tab`'s same-tab rule (U11) makes that the no-op it should
    /// be — zoom and the float survive.
    fn step_tab(&mut self, delta: isize) {
        let n = self.ws.tabs.len();
        if n == 0 {
            return;
        }
        let next = (self.ws.active_tab as isize + delta).rem_euclid(n as isize) as usize;
        self.go_to_tab(next);
    }

    /// C21: leave zoom (a pure view flag). Called by every documented exit
    /// trigger before applying whatever caused it, so the layout never
    /// changes invisibly behind a full-screen zoomed pane.
    fn exit_zoom(&mut self) {
        self.zoomed = false;
    }

    /// Alt+z: toggle the zoomed view. A collapsed stack member is expanded
    /// first, so zooming always lands on something visible.
    fn toggle_zoom(&mut self) {
        if self.zoomed {
            self.zoomed = false;
            return;
        }
        let focused = self.focused;
        layout::expand_in_stacks(&mut self.ws.active_tab_mut().layout, focused);
        self.zoomed = true;
    }

    /// C19: every pane whose runtime status is `NeedsInput` — the exact
    /// predicate `needs_input_count` uses, so the ring's size can never
    /// disagree with the hint bar's advertised N. Ordered by (tab index
    /// ascending, position within that tab's `pane_order()`); the float
    /// (C22), if needy, is last — `needs_input_count` counts `runtimes`
    /// directly (float included), so it must be too.
    fn attention_ring(&self) -> Vec<PaneId> {
        let mut ring = Vec::new();
        for tab in &self.ws.tabs {
            let mut order = Vec::new();
            layout::pane_order(&tab.layout, &mut order);
            ring.extend(order.into_iter().filter(|id| {
                self.runtimes.get(id).map(|rt| rt.status() == AgentStatus::NeedsInput).unwrap_or(false)
            }));
        }
        if let Some(f) = &self.float {
            if self.runtimes.get(&f.id).map(|rt| rt.status() == AgentStatus::NeedsInput).unwrap_or(false) {
                ring.push(f.id);
            }
        }
        ring
    }

    /// Alt+a: cycle-focus to the next ring member after the focused pane,
    /// wrapping past the end back to the first; a focused pane that isn't in
    /// the ring jumps straight to the first member.
    fn jump_attention(&mut self) {
        // C22 rule 2: leaving the float via Alt+a always just returns focus
        // to `prev_focus` — it does not additionally ring-jump in the same
        // press (no explicit target was requested, unlike a tab switch).
        if self.float_focused() {
            self.hide_float();
            return;
        }
        let ring = self.attention_ring();
        if ring.is_empty() {
            self.set_flash("nothing needs you");
            return;
        }
        let next = match ring.iter().position(|&id| id == self.focused) {
            Some(k) => ring[(k + 1) % ring.len()],
            None => ring[0],
        };
        if next == self.focused {
            // The only ring member is the pane we're already on.
            self.set_flash("nothing else needs you");
            return;
        }
        self.focus_attention_target(next);
    }

    /// Land focus on `target`, switching tabs first (go_to_tab semantics,
    /// lazy spawn included) when it lives outside the active one — which
    /// also exits zoom, per C21's tab-switch rule. A same-tab jump keeps
    /// zoom (zoom follows focus). Either way, expand the target out of any
    /// collapsed stack, same as any other focus move. C22: a jump landing on
    /// the float shows it (recording where focus came from as `prev_focus`).
    fn focus_attention_target(&mut self, target: PaneId) {
        if let Some(ti) = self.tab_of(target) {
            if ti != self.ws.active_tab {
                self.go_to_tab(ti);
            }
        }
        if self.is_float(target) {
            let from = self.focused;
            if let Some(f) = &mut self.float {
                f.shown = true;
                f.prev_focus = from;
            }
        }
        self.focused = target;
        layout::expand_in_stacks(&mut self.ws.active_tab_mut().layout, target);
    }

    /// Alt+g: step the active tab through the three canned arrangements,
    /// skipping ones that don't fit the current body area. No-ops (without
    /// advancing the cycle counter) when there's nothing to arrange or
    /// nothing fits.
    fn cycle_layout(&mut self) {
        let order = self.pane_order();
        if order.len() < 2 {
            self.set_flash("one pane — nothing to arrange");
            return;
        }
        let focused = self.focused;
        let area = self.body_area();
        for step in 0..3 {
            let idx = (self.layout_cycle + step) % 3;
            let node = arrangement_for(idx, &order, focused);
            if layout::arrangement_fits(&node, area) {
                self.ws.active_tab_mut().layout = node;
                self.layout_cycle = (idx + 1) % 3;
                return;
            }
        }
        self.set_flash("no room to rearrange");
    }

    /// Alt+e: open the C20 activity feed at the live tail, or close it if
    /// already open. In practice the close direction is intercepted earlier,
    /// in `handle_mode_key` (its doc comment explains why); this still
    /// toggles honestly either way.
    fn toggle_feed(&mut self) {
        self.mode = match self.mode {
            Mode::Feed { .. } => Mode::Normal,
            _ => Mode::Feed { offset: 0 },
        };
    }

    /// C20: the feed overlay's paging step — half its drawn height, at least
    /// one entry. The single source for the keyboard's PgUp/PgDn
    /// (`handle_mode_key`) and, since U8, one wheel notch over the overlay.
    fn feed_page(&self) -> usize {
        (feed_overlay_size(self.body_area()).1 / 2).max(1) as usize
    }

    /// C14: launch the picker's item `index` — leave the modal, then spawn
    /// it with the same C21/C22 pre-steps a picker launch has always had.
    /// Shared by the keyboard's Enter and U8's click-a-row path so the two
    /// can't drift.
    fn picker_launch(&mut self, index: usize) {
        let items = picker_items();
        let Some(adapter) = items.get(index.min(items.len().saturating_sub(1))).copied() else {
            return;
        };
        self.mode = Mode::Normal;
        self.exit_zoom(); // C21: "picker launch" is a structural action
        self.hide_float(); // C22 rule 3: ditto
        self.new_pane_with(adapter);
        self.relayout();
        self.save();
    }

    // -- float (C22) ---------------------------------------------------------

    /// Alt+f: first press spawns the float (a shell in the focused pane's
    /// cwd, preset title "scratch") and shows+focuses it; later presses
    /// hide/show it (the process stays alive while hidden). Refuses (flash,
    /// no state change) when the body is too small for the geometry
    /// formula to place a sane rect — checked here so both the first spawn
    /// and a later re-show are covered (the terminal may have shrunk while
    /// the float was hidden).
    fn toggle_float(&mut self) {
        if self.float_focused() {
            self.hide_float();
            return;
        }
        let body = self.body_area();
        if !Self::float_fits(body) {
            self.set_flash("no room for float");
            return;
        }
        match &mut self.float {
            Some(f) => {
                f.shown = true;
                f.prev_focus = self.focused;
                self.focused = f.id;
            }
            None => self.spawn_float(),
        }
    }

    fn spawn_float(&mut self) {
        let id = self.alloc_pane_id();
        let cwd = self
            .find_spec(self.focused)
            .map(|s| s.cwd.clone())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let prev_focus = self.focused;
        let spec = PaneSpec {
            adapter: "shell".into(),
            cwd,
            session: None,
            title: Some("scratch".into()),
            spawned_by: None,
        };
        self.float = Some(Float { id, spec: spec.clone(), shown: true, prev_focus });
        self.focused = id;
        // display_rects, not rects: the float isn't in the tiled tree, so
        // only the zoom-aware/float-aware display list knows its rect.
        if let Some(pr) = self.display_rects().iter().find(|pr| pr.id == id).copied() {
            self.spawn_pane(id, &spec, pr.rect);
        }
    }

    /// C22 rules 2/3: hide the float (if shown) and restore focus to
    /// whatever was focused before it appeared — the single mechanism
    /// behind Alt+f's toggle-off, every focus-moving/structural action that
    /// must not leave it focused, and a mouse click landing outside its
    /// rect. A no-op when the float isn't currently shown.
    fn hide_float(&mut self) {
        let Some(f) = &mut self.float else { return };
        if !f.shown {
            return;
        }
        f.shown = false;
        let back = f.prev_focus;
        self.focused = if self.ws.active_tab().panes.contains_key(&back) {
            back
        } else {
            // prev_focus may have been closed via the control plane while
            // the float was up — fall back to whatever's on screen now,
            // same recovery `close_pane_id` uses.
            self.pane_order().first().copied().unwrap_or(0)
        };
    }

    /// C22 rule 4: Alt+w on the float kills it for real and clears the slot
    /// — unlike hiding, no undo entry (scratch is not precious).
    fn close_float(&mut self) {
        let Some(f) = self.float.take() else { return };
        if let Some(mut rt) = self.runtimes.remove(&f.id) {
            rt.kill();
        }
        self.tokens.remove(&f.id);
        self.dead.remove(&f.id);
        self.pending_input.remove(&f.id);
        self.raw.remove(&f.id);
        self.focused = if self.ws.active_tab().panes.contains_key(&f.prev_focus) {
            f.prev_focus
        } else {
            self.pane_order().first().copied().unwrap_or(0)
        };
        self.set_flash("scratch closed");
    }

    // -- raw pass-through (C23) -----------------------------------------------

    /// Alt+Shift+p: toggle the focused pane's raw membership — the same
    /// chord both enters and exits it ("the key that got you in gets you
    /// out"), and it is reachable via ordinary `translate()` dispatch
    /// (`apply`) regardless of whether the pane is currently raw, since
    /// entering raw must work from a cooked pane in the first place.
    fn toggle_raw(&mut self) {
        let focused = self.focused;
        if !self.raw.remove(&focused) {
            self.raw.insert(focused);
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
            // C20: Alt+e both opens and closes the feed. Handled here, before
            // the generic reset below clears `self.mode` out from under it —
            // otherwise `Action::ToggleFeed`, reached via the fallthrough
            // path, could never tell the feed had just been open and would
            // always re-open it instead of closing.
            if matches!(self.mode, Mode::Feed { .. }) && key.code == KeyCode::Char('e') {
                self.mode = Mode::Normal;
                return true;
            }
            // U9: Alt+c is exempt from the snap below — Scroll→Copy hands
            // the frozen view over intact, because scroll→select→yank is
            // THE keyboard path for copying history (the wheel→Alt+c route
            // already preserved the view; the keyboard route snapped to the
            // live tail at the handoff, so history could never be yanked by
            // keyboard). Same special-case shape as Alt+e above; the chord
            // still falls through to the global binding, which enters Copy.
            let scroll_to_copy =
                matches!(self.mode, Mode::Scroll { .. }) && key.code == KeyCode::Char('c');
            if matches!(self.mode, Mode::Scroll { .. }) && !scroll_to_copy {
                let focused = self.focused;
                if let Some(rt) = self.runtimes.get_mut(&focused) {
                    rt.set_scrollback(0);
                }
            }
            self.mode = Mode::Normal;
            return false;
        }
        // C20: half the feed overlay's own height, for PgUp/PgDn — computed
        // here (needs a whole `&self` via `body_area()`) rather than inside
        // the match below, where `Mode::Feed`'s arm already holds
        // `self.mode` mutably borrowed.
        let feed_page = self.feed_page();
        // C24: the focused pane's current inner grid, for clamping the copy
        // cursor — same borrow-ordering reason as `feed_page` above.
        let (copy_h, copy_w) = self.focused_inner_dims();
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
                        let choice = *selection;
                        self.picker_launch(choice);
                    }
                    KeyCode::Esc => self.mode = Mode::Normal,
                    _ => {}
                }
                true
            }
            Mode::Scroll { offset } => {
                let page = (self.term_size.height / 2).max(1) as usize;
                let focused = self.focused;
                // U9: base the arithmetic on the view's CURRENT offset (the
                // grid auto-advances it as new lines bank while scrolled),
                // and after every write read the grid-clamped value back —
                // the mode offset is a mirror of the view, never an
                // independent counter free to overshoot into a phantom
                // (~240 banked Down presses before the screen moved).
                let cur = self
                    .runtimes
                    .get(&focused)
                    .map(|rt| rt.scroll_offset())
                    .unwrap_or(*offset);
                let new_offset = match key.code {
                    KeyCode::Up | KeyCode::Char('k') => Some(cur + 1),
                    KeyCode::Down | KeyCode::Char('j') => Some(cur.saturating_sub(1)),
                    KeyCode::PageUp => Some(cur + page),
                    KeyCode::PageDown => Some(cur.saturating_sub(page)),
                    KeyCode::Esc | KeyCode::Char('q') => None,
                    _ => return true,
                };
                match new_offset {
                    Some(n) => {
                        *offset = n;
                        if let Some(rt) = self.runtimes.get_mut(&focused) {
                            rt.set_scrollback(n);
                            *offset = rt.scroll_offset();
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
            // C24: keyboard copy cursor + selection, alongside the existing
            // mouse-drag path (both write `self.selection`; a drag also
            // moves the cursor, see `begin_selection`/`extend_selection`).
            Mode::Copy { cursor } => {
                match key.code {
                    KeyCode::Char('h') | KeyCode::Left => cursor.1 = cursor.1.saturating_sub(1),
                    KeyCode::Char('l') | KeyCode::Right => {
                        cursor.1 = (cursor.1 + 1).min(copy_w.saturating_sub(1))
                    }
                    KeyCode::Char('k') | KeyCode::Up => cursor.0 = cursor.0.saturating_sub(1),
                    KeyCode::Char('j') | KeyCode::Down => {
                        cursor.0 = (cursor.0 + 1).min(copy_h.saturating_sub(1))
                    }
                    KeyCode::Char('0') => cursor.1 = 0,
                    KeyCode::Char('$') => cursor.1 = copy_w.saturating_sub(1),
                    _ => {}
                }
                // A motion above extends an active selection to the new
                // cursor (v's "movement extends the selection" rule) — kept
                // out of the motion arms themselves so it applies uniformly
                // without repeating it six times.
                if matches!(
                    key.code,
                    KeyCode::Char('h' | 'l' | 'k' | 'j' | '0' | '$')
                        | KeyCode::Left
                        | KeyCode::Right
                        | KeyCode::Up
                        | KeyCode::Down
                ) {
                    if let Some(sel) = &mut self.selection {
                        sel.cursor = *cursor;
                    }
                    return true;
                }
                match key.code {
                    KeyCode::Char('v') => {
                        let focused = self.focused;
                        let anchor = *cursor;
                        if self.selection.is_some() {
                            self.selection = None; // toggle off: clear the anchor
                        } else {
                            self.selection =
                                Some(Selection { pane: focused, anchor, cursor: anchor, dragging: false });
                        }
                    }
                    KeyCode::Char('y') | KeyCode::Enter => {
                        if self.selection.is_some() {
                            // finish_selection needs the whole &mut self —
                            // `cursor`'s last use was above, so its borrow
                            // of `self.mode` has already ended.
                            self.pending_yank = self.finish_selection();
                        } else {
                            self.set_flash("nothing selected");
                        }
                    }
                    // U9: page the VIEW by the pane's inner height (grid-
                    // clamped) — history is now reachable while selecting.
                    // The cursor and selection stay in visible-grid cell
                    // space and extraction still reads the visible grid
                    // (C24's what-you-see-is-what-yanks limit stands).
                    KeyCode::PageUp | KeyCode::PageDown => {
                        let focused = self.focused;
                        let view_page = copy_h.max(1) as usize;
                        if let Some(rt) = self.runtimes.get_mut(&focused) {
                            let cur = rt.scroll_offset();
                            let want = match key.code {
                                KeyCode::PageUp => cur + view_page,
                                _ => cur.saturating_sub(view_page),
                            };
                            rt.set_scrollback(want);
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        self.mode = Mode::Normal;
                        self.selection = None;
                    }
                    _ => {}
                }
                true
            }
            Mode::Help => {
                // Any key dismisses the keymap overlay.
                self.mode = Mode::Normal;
                true
            }
            Mode::Feed { offset } => {
                let cap = self.feed.len().saturating_sub(1);
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => *offset = (*offset + 1).min(cap),
                    KeyCode::Down | KeyCode::Char('j') => *offset = offset.saturating_sub(1),
                    KeyCode::PageUp => *offset = (*offset + feed_page).min(cap),
                    KeyCode::PageDown => *offset = offset.saturating_sub(feed_page),
                    KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
                    _ => {}
                }
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
        Method::Broadcast { text, submit } => {
            format!("broadcast len={} submit={submit}", text.len())
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

/// U2: a pane's display name from its spec — the custom title when set, else
/// `{adapter} · {cwd-tag}` (the cwd's last path component), so a bank of
/// untitled shells on the same adapter stays tellable apart. This was the
/// corner badge's render-local fallback; it's shared here so the badge, the
/// collapsed rows, the feed, notifications, and flashes can never drift
/// apart on what a pane is called (C4's amendment points at this fn).
pub fn display_name_of(spec: &PaneSpec) -> String {
    display_name_live(spec, None)
}

/// U2 + P6: the full naming chain as a pure function — explicit Alt+r title,
/// else the pane's live OSC 0/2 title (agent panes only), else
/// `{adapter} · {cwd-tag}` (the cwd's last path component, so a bank of
/// untitled shells on the same adapter stays tellable apart).
///
/// The explicit title still wins: a name the user typed is a decision, and
/// an app that repaints its OSC title every frame must not overwrite it.
/// `live` is expected pre-sanitized and bounded (`App::live_title`).
///
/// A plain `shell` pane deliberately ignores its live title. P6 adopted OSC
/// titles because agent CLIs publish *task status* through them (Claude
/// Code repaints `spinner + task` continuously) — that is fleet status worth
/// a badge. A shell's title comes from `PS1`, which by default restates
/// `user@host: /path`: chrome that duplicates the cwd tag already shown and
/// crowds the far narrower badge. This costs nothing for a hand-launched
/// agent: `observe_panes` promotes a shell pane's adapter to `pi`/`claude`
/// once it sees the agent running, so its titles start counting then (and
/// stop when it exits and the pane demotes back).
pub fn display_name_live(spec: &PaneSpec, live: Option<&str>) -> String {
    if let Some(title) = &spec.title {
        return title.clone();
    }
    if spec.adapter != "shell" {
        if let Some(live) = live.filter(|t| !t.is_empty()) {
            return live.to_string();
        }
    }
    let cwd_tag = spec
        .cwd
        .file_name()
        .and_then(|f| f.to_str())
        .map(|f| format!(" · {f}"))
        .unwrap_or_default();
    format!("{}{cwd_tag}", spec.adapter)
}

/// P6: a pane's OSC title is untrusted text bound for roost's chrome (and,
/// for the focused pane, for a sequence roost writes to its own terminal).
/// Drop control characters, collapse surrounding whitespace, and bound the
/// length in characters — on char boundaries, so a multi-byte glyph is never
/// split. Pure, so the bound is testable without a terminal.
fn sanitize_title(raw: &str, cap: usize) -> String {
    raw.chars().filter(|c| !c.is_control()).take(cap).collect::<String>().trim().to_string()
}

/// U12's busy predicate, shared by the Alt+w and Alt+q destructive guards so
/// the two can never disagree about what "busy" means: an agent mid-turn is
/// one actively producing (`Working`) *or* blocked on your approval
/// (`NeedsInput`) — closing either loses the in-flight turn.
fn is_busy(status: AgentStatus) -> bool {
    matches!(status, AgentStatus::Working | AgentStatus::NeedsInput)
}

/// C20: the feed overlay's `(width, height)` at the given body area —
/// `w = min(72, body.width − 4)`, `h = min(16, body.height − 4)`. Shared by
/// the renderer (geometry) and `handle_mode_key` (PgUp/PgDn page size) so
/// both agree on what's actually on screen. Pure so the 72×16 formula is
/// unit-tested without a `Frame`.
pub fn feed_overlay_size(body: Rect) -> (u16, u16) {
    let w = body.width.saturating_sub(4).min(72);
    let h = body.height.saturating_sub(4).min(16);
    (w, h)
}

/// C25: the arrangement at cycle index `idx` (0=grid, 1=main+stack,
/// 2=all-stack) for pane order `order` with `focused` kept in place — the
/// pure per-index dispatch `cycle_layout` walks while searching for a fit.
fn arrangement_for(idx: usize, order: &[PaneId], focused: PaneId) -> LayoutNode {
    match idx {
        0 => layout::grid_layout(order),
        1 => {
            let rest: Vec<PaneId> = order.iter().copied().filter(|&id| id != focused).collect();
            layout::main_stack_layout(focused, &rest)
        }
        _ => layout::all_stack_layout(order, focused),
    }
}

/// Pure decision behind `App::show_alt_hint`, split out so it's testable
/// without depending on process env vars or wall-clock time.
///
/// U4: the trigger is evidence, not an allowlist — keys are arriving and
/// not one of them has carried Alt, inside the startup window. That covers
/// every terminal with the setting off (the old `TERM_PROGRAM ==
/// "Apple_Terminal"` test left iTerm2, the README's recommendation, silent),
/// and it fires at exactly the right moment: when Option-as-Meta is off, the
/// chord you just tried arrives as an unmodified key (Option+b → `∫`), so
/// the failed chord is itself the evidence. One Alt key ever, or the window
/// running out, ends it for the session.
fn wants_alt_hint(alt_seen: bool, keys_seen: bool, elapsed: Duration) -> bool {
    !alt_seen && keys_seen && elapsed < ALT_HINT_WINDOW
}

/// C11/U4: the warning line for a given host `TERM_PROGRAM` — the real menu
/// path where we know it, a terminal-agnostic line otherwise. Pure, so the
/// wording is pinned without touching the environment.
fn alt_hint_line(term_program: Option<&str>) -> &'static str {
    match term_program {
        Some("Apple_Terminal") => {
            " Alt keys aren't reaching roost? Enable \"Use Option as Meta Key\" in Terminal > Settings > Profiles > Keyboard "
        }
        Some("iTerm.app") => {
            " Alt keys aren't reaching roost? Set Left Option key to \"Esc+\" in iTerm2 > Settings > Profiles > Keys "
        }
        _ => {
            " Alt keys aren't reaching roost? Turn on your terminal's Option/Alt-as-Meta setting (send Esc+) "
        }
    }
}

/// Pure decision behind `App::focused_cwd`: abbreviate a `$HOME`-rooted path
/// to `~`, split out so it's testable without depending on the real
/// environment's home directory.
fn abbreviate_home(cwd: &Path, home: Option<&Path>) -> String {
    match home {
        Some(home) if !home.as_os_str().is_empty() => match cwd.strip_prefix(home) {
            Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => cwd.display().to_string(),
        },
        _ => cwd.display().to_string(),
    }
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
            (0, 0),
            None,
        )
        .unwrap();
        (app, store)
    }

    fn shell_ws() -> Workspace {
        Workspace::default_in(PathBuf::from("/tmp"))
    }

    /// P2: a pane's OSC 9/777 notification reaches the user through the same
    /// two channels a bell does — a nudge naming the pane (U2's display
    /// name), and the host re-emission queued for the composition root. The
    /// focused pane is deliberately not announced (you're looking at it),
    /// matching `on_status`/`on_pty_exit`.
    #[test]
    fn a_pane_notification_names_the_pane_and_queues_the_host_re_emission() {
        let (mut app, _s) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1 | 2, focus 2
        assert_eq!(app.focused, 2);

        let name = app.display_name(1);
        app.runtimes.get_mut(&1).unwrap().effects = crate::ports::PaneEffects {
            notifications: vec!["NEEDS-YOU".to_string()],
            host_writes: b"\x1b]9;NEEDS-YOU\x07".to_vec(),
        };
        let msg = app.on_pty_output(1, b"whatever").expect("unfocused pane notifies");
        assert_eq!(msg, format!("{name}: NEEDS-YOU"));
        // `contains`, not equality: the same queue also carries P6's host
        // title, whose own gate is `host_title_follows_focus_and_live_title`.
        assert!(host_contains(&mut app, b"\x1b]9;NEEDS-YOU\x07"));

        // The focused pane's notification still forwards to the host (the
        // app asked its terminal for it) but raises no roost-side nudge.
        app.runtimes.get_mut(&2).unwrap().effects = crate::ports::PaneEffects {
            notifications: vec!["also me".to_string()],
            host_writes: b"\x1b]9;also me\x07".to_vec(),
        };
        assert!(app.on_pty_output(2, b"x").is_none());
        assert!(host_contains(&mut app, b"\x1b]9;also me\x07"));
        // Draining takes it: nothing is re-emitted on the next pass.
        assert!(!host_contains(&mut app, b"\x1b]9;"));
    }

    /// Drain the host queue once and report whether it carried `needle`.
    fn host_contains(app: &mut App<FakePane>, needle: &[u8]) -> bool {
        let out = app.take_host_output();
        out.windows(needle.len()).any(|w| w == needle)
    }

    /// P2: several notifications in one chunk — the newest is the one still
    /// true, so that's what the user is told; the rest have already done
    /// their job by marking the pane as needing attention.
    #[test]
    fn the_newest_notification_in_a_chunk_is_the_one_reported() {
        let (mut app, _s) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.runtimes.get_mut(&1).unwrap().effects = crate::ports::PaneEffects {
            notifications: vec!["stale".into(), "current".into()],
            host_writes: Vec::new(),
        };
        let msg = app.on_pty_output(1, b"x").expect("notifies");
        assert!(msg.ends_with(": current"), "got {msg}");
        // No notification at all ⇒ no nudge, and output handling is
        // otherwise unchanged.
        assert!(app.on_pty_output(1, b"x").is_none());
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

    /// P4 pixel plumbing: a pane's pixel geometry is a proportional share of
    /// the host window's, delivered at spawn and refreshed by every resize;
    /// an unknown host geometry stays an honest (0, 0) throughout.
    #[test]
    fn pane_pixels_flow_proportionally_through_spawn_and_resize() {
        let store = MemStore::default();
        let (tx, _rx) = mpsc::sync_channel(64);
        // Host: 100x30 cells, 1000x600 px → 10 px/col, 20 px/row.
        let mut app = App::<FakePane>::new(
            shell_ws(),
            agents::registry(),
            Box::new(store),
            tx,
            Size::new(100, 30),
            (1000, 600),
            None,
        )
        .unwrap();
        let id = app.pane_order()[0];
        // Single pane: body 100x28 (tab + hint rows off), inner 98x26 →
        // (1000*98/100, 600*26/30).
        let px = app.runtimes.get(&id).unwrap().pixels;
        assert_eq!(px, (980, 520), "spawn-time pixels");

        // Host resize: both cell and pixel geometry refresh proportionally —
        // inner 48x26 of a 50x30 host → (500*48/50, 600*26/30).
        app.on_resize(Size::new(50, 30), (500, 600));
        let px = app.runtimes.get(&id).unwrap().pixels;
        assert_eq!(px, (480, 520), "post-resize pixels");

        // Host stops reporting pixels: panes drop to the honest zero.
        app.on_resize(Size::new(50, 30), (0, 0));
        assert_eq!(app.runtimes.get(&id).unwrap().pixels, (0, 0));
    }

    /// P4: with no host pixel geometry (the common CI/harness case), spawn
    /// delivers (0, 0) — never an invented size.
    #[test]
    fn unknown_host_pixels_spawn_as_zero() {
        let (app, _) = mk_app(shell_ws()); // mk_app passes host_pixels (0, 0)
        let id = app.pane_order()[0];
        assert_eq!(app.runtimes.get(&id).unwrap().pixels, (0, 0));
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

    /// U13 (closes SPEC-GAP-2): a tab whose agents are dead says so instead
    /// of rendering the Quiet blank — one transient bell, then silence, was
    /// the entire signal a background tab full of corpses used to get.
    #[test]
    fn tab_summary_reports_exited_and_ranks_it_between_waiting_and_quiet() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1 | 2 in one tab
        assert_eq!(app.tab_summary(0), TabSummary::Quiet);

        app.runtimes.get_mut(&1).unwrap().kill(); // pane 1 dies
        assert_eq!(app.tab_summary(0), TabSummary::Exited, "a dead pane is news");

        // A live pane with something to say outranks a corpse...
        app.runtimes.get_mut(&2).unwrap().set_extension_status(AgentStatus::Waiting);
        assert_eq!(app.tab_summary(0), TabSummary::Waiting);
        app.runtimes.get_mut(&2).unwrap().set_extension_status(AgentStatus::Working);
        assert_eq!(app.tab_summary(0), TabSummary::Working);
        app.runtimes.get_mut(&2).unwrap().set_extension_status(AgentStatus::NeedsInput);
        assert_eq!(app.tab_summary(0), TabSummary::NeedsInput);
        // ...and an idle one does not: Idle is "nothing to report", the
        // corpse is the only news on the tab.
        app.runtimes.get_mut(&2).unwrap().set_extension_status(AgentStatus::Idle);
        assert_eq!(app.tab_summary(0), TabSummary::Exited);
        // Every pane dead is still Exited, never Quiet.
        app.runtimes.get_mut(&2).unwrap().kill();
        assert_eq!(app.tab_summary(0), TabSummary::Exited);
    }

    /// A pane whose *spawn* failed has no runtime at all — it is dead, not
    /// unspawned, and must not read as `Unknown` (nor drag the tab to it).
    #[test]
    fn tab_summary_counts_a_failed_spawn_as_exited_not_unknown() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.pane_order()[0];
        app.runtimes.remove(&id);
        app.dead.insert(id, "spawn-fail requested".into());
        assert_eq!(app.tab_summary(0), TabSummary::Exited);
    }

    /// U14: the flash says what actually happened. A native helper that
    /// exited clean is an unqualified copy; OSC 52 alone is flagged (sent,
    /// unacknowledged — the one channel that works over SSH, and the one
    /// that can silently do nothing); neither is an honest failure.
    #[test]
    fn copy_flash_text_names_the_channel_that_took_it() {
        assert_eq!(App::<FakePane>::copy_flash_text(13, ClipboardOutcome::Native), "copied 13 chars");
        assert_eq!(
            App::<FakePane>::copy_flash_text(13, ClipboardOutcome::Osc52),
            "copied 13 chars (OSC 52)"
        );
        assert_eq!(App::<FakePane>::copy_flash_text(13, ClipboardOutcome::Failed), "copy failed");
        // A failure never quotes a count — there is no N to have copied.
        assert!(!App::<FakePane>::copy_flash_text(0, ClipboardOutcome::Failed).contains('0'));
    }

    /// The flash lands on the bar for both copy paths, and only after the
    /// clipboard has answered.
    #[test]
    fn flash_copy_puts_the_real_outcome_on_the_bar() {
        let (mut app, _) = mk_app(shell_ws());
        app.flash_copy(4, ClipboardOutcome::Osc52);
        assert_eq!(app.flash(), Some("copied 4 chars (OSC 52)"));
        app.flash_copy(4, ClipboardOutcome::Failed);
        assert_eq!(app.flash(), Some("copy failed"));
    }

    #[test]
    fn needs_input_count_is_0_1_many_and_spans_every_tab() {
        let (mut app, _) = mk_app(shell_ws());
        let tab0_pane = app.pane_order()[0];
        assert_eq!(app.needs_input_count(), 0); // 0: omitted from the hint bar

        app.apply(Action::NewTab); // tab 1 now active; tab 0's pane stays spawned
        let tab1_pane = app.focused;
        app.runtimes.get_mut(&tab0_pane).unwrap().set_extension_status(AgentStatus::NeedsInput);
        assert_eq!(app.needs_input_count(), 1); // 1

        app.runtimes.get_mut(&tab1_pane).unwrap().set_extension_status(AgentStatus::NeedsInput);
        assert_eq!(app.needs_input_count(), 2); // many, and counted while tab 1 is active

        app.go_to_tab(0); // switching the active tab must not change the count
        assert_eq!(app.needs_input_count(), 2);
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
    fn closing_a_needs_input_pane_needs_a_confirming_second_press() {
        // U12: an agent blocked on your approval (◆) is mid-turn all the
        // same — the close guard's busy predicate is Working || NeedsInput,
        // not Working alone.
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.apply(Action::ClosePane); // armed, not closed
        assert_eq!(app.runtimes.len(), 2);
        app.apply(Action::ClosePane); // confirmed ⇒ closed
        assert_eq!(app.runtimes.len(), 1);
    }

    #[test]
    fn quit_with_a_quiet_fleet_is_instant() {
        // U1: the guard protects in-flight turns; a quiet fleet has none —
        // sessions resume on relaunch, so Alt+q stays a single press.
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::Quit);
        assert!(app.quit);
    }

    #[test]
    fn quit_with_a_busy_pane_arms_then_a_second_press_quits() {
        // U1: closing one busy pane double-confirms, so killing the whole
        // fleet must too — first press arms + prompts, second quits.
        let (mut app, _) = mk_app(shell_ws());
        app.on_pty_output(1, b"x"); // Working
        app.apply(Action::Quit);
        assert!(!app.quit);
        assert_eq!(app.flash(), Some("1 agent busy — Alt+q again to quit"));
        app.apply(Action::Quit);
        assert!(app.quit);
    }

    #[test]
    fn quit_guard_counts_needs_input_panes_and_pluralizes() {
        // U1 counts with U12's predicate: a ◆ pane is as mid-turn as a
        // working one, and the prompt reports how much is at stake.
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.on_pty_output(1, b"x"); // Working
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.apply(Action::Quit);
        assert!(!app.quit);
        assert_eq!(app.flash(), Some("2 agents busy — Alt+q again to quit"));
        app.apply(Action::Quit);
        assert!(app.quit);
    }

    #[test]
    fn another_action_disarms_the_quit_confirm_and_drops_its_prompt() {
        let (mut app, _) = mk_app(shell_ws());
        app.on_pty_output(1, b"x"); // Working
        app.apply(Action::Quit); // armed
        assert!(app.flash().is_some());
        app.apply(Action::ToggleHints); // any other action disarms...
        assert_eq!(app.flash(), None, "the confirm prompt dies with its arm (U22)");
        app.apply(Action::Quit); // ...so the next Alt+q re-arms, not quits
        assert!(!app.quit);
        app.apply(Action::Quit);
        assert!(app.quit);
    }

    #[test]
    fn an_armed_close_never_confirms_a_quit_nor_vice_versa() {
        // The two confirms are separate state: Alt+w-then-Alt+q must not
        // treat the close arm as the quit's first press (or the other way
        // around) — each destructive path earns its own second press.
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        let id = app.focused;
        app.on_pty_output(id, b"x"); // Working
        app.apply(Action::ClosePane); // close armed
        app.apply(Action::Quit); // must ARM the quit, not confirm-quit
        assert!(!app.quit);
        // ...and the quit press disarmed the close: the next Alt+w re-arms
        // instead of closing.
        app.apply(Action::ClosePane);
        assert_eq!(app.runtimes.len(), 2);
    }

    #[test]
    fn confirm_flash_lives_exactly_as_long_as_its_confirm_window() {
        // U22: FLASH_WINDOW (2s) < CONFIRM_WINDOW (3s) used to leave a
        // silent final second where the second press still fired. A confirm
        // prompt now carries the confirm window itself; ordinary flashes
        // keep FLASH_WINDOW; a consumed arm takes its prompt down with it.
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        let id = app.focused;
        app.on_pty_output(id, b"x"); // Working
        app.apply(Action::ClosePane); // armed
        assert_eq!(app.flash.as_ref().unwrap().2, CONFIRM_WINDOW);
        app.apply(Action::ClosePane); // confirmed ⇒ closed, prompt gone
        assert_eq!(app.flash(), None);
        app.set_flash("copied 3 chars");
        assert_eq!(app.flash.as_ref().unwrap().2, FLASH_WINDOW);
    }

    #[test]
    fn scroll_mode_offset_clamps_to_banked_history_no_overshoot() {
        // U9 (overshoot): paging past the top of history must clamp BOTH
        // the view and the mode's mirrored offset — one Down after that
        // moves the view immediately, instead of burning ~240 presses
        // against a phantom counter.
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().scroll_total = 10;
        app.apply(Action::ScrollMode);
        // term height 30 ⇒ page 15; two PageUps ask for 30, history has 10.
        app.handle_mode_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        app.handle_mode_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(app.runtimes.get(&id).unwrap().scrollback, 10);
        let Mode::Scroll { offset } = app.mode else { panic!("still in scroll mode") };
        assert_eq!(offset, 10, "mode offset mirrors the clamp, not the ask");
        // One Down moves the view immediately.
        app.handle_mode_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.runtimes.get(&id).unwrap().scrollback, 9);
        let Mode::Scroll { offset } = app.mode else { panic!("still in scroll mode") };
        assert_eq!(offset, 9);
    }

    #[test]
    fn entering_scroll_mode_seeds_from_the_wheeled_offset() {
        // U9 (wheel/keys desync): Scroll mode continues from the wheeled
        // position — the first Up goes one line FURTHER back, instead of
        // snapping the view from the wheeled offset to 1.
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.wheel_scroll(id, 24);
        app.apply(Action::ScrollMode);
        let Mode::Scroll { offset } = app.mode else { panic!("scroll mode") };
        assert_eq!(offset, 24, "seeded from the pane's current view offset");
        app.handle_mode_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.runtimes.get(&id).unwrap().scrollback, 25);
    }

    #[test]
    fn alt_c_from_scroll_mode_preserves_the_scrolled_view() {
        // U9 (scroll→copy snap): the Alt-chord snap exempts the copy
        // transition, so scroll→select→yank works on history — the view a
        // wheel→Alt+c handoff always kept now survives the keyboard route
        // too. (Any other Alt chord still snaps: pinned by
        // `scrolling_then_a_global_chord_snaps_the_pane_back_to_live`.)
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.wheel_scroll(id, 5);
        app.apply(Action::ScrollMode);
        let consumed = app.handle_mode_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::ALT));
        assert!(!consumed, "Alt+c falls through to the global CopyMode binding");
        assert_eq!(app.runtimes.get(&id).unwrap().scrollback, 5, "no snap at the handoff");
        app.apply(Action::CopyMode); // what the fallthrough dispatches
        assert!(matches!(app.mode, Mode::Copy { .. }));
        assert_eq!(app.runtimes.get(&id).unwrap().scrollback, 5, "copy opens on the frozen view");
    }

    #[test]
    fn copy_mode_pages_the_view_by_pane_height_and_clamps() {
        // U9: PgUp/PgDn in copy mode walk the view through history by the
        // pane's inner height, clamped to what's banked; motions/selection
        // stay in visible-grid cell space (C24).
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        let (rows, _) = inner_dims(app.rects()[0].rect);
        let page = rows as usize;
        app.runtimes.get_mut(&id).unwrap().scroll_total = page + 5;
        app.apply(Action::CopyMode);
        app.handle_mode_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(app.runtimes.get(&id).unwrap().scrollback as usize, page);
        app.handle_mode_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(
            app.runtimes.get(&id).unwrap().scrollback as usize,
            page + 5,
            "second page clamps to the banked history"
        );
        app.handle_mode_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.runtimes.get(&id).unwrap().scrollback as usize, 5);
        assert!(matches!(app.mode, Mode::Copy { .. }), "paging never leaves copy mode");
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
    fn control_read_full_and_tail_reach_scrollback_not_just_the_screen() {
        // M1 regression: Full/Tail used to alias Screen (grab_text only ever
        // sees the visible grid), so an orchestrator's read(tail:20) missed
        // anything that had already scrolled off. FakePane's `grab` (screen)
        // and `all_text` (full history) are deliberately different here so a
        // test can tell them apart.
        use crate::core::control::{Method, ReadMode, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        let ct = app.control_token().to_string();
        let id = app.focused;
        {
            let rt = app.runtimes.get_mut(&id).unwrap();
            rt.grab = "visible screen only".into();
            rt.all_text = "line1\nline2\nline3\nline4\nline5".into();
        }
        let ok = |r: Reply| match r {
            Reply::Ok { ok } => ok,
            Reply::Err { err } => panic!("expected ok, got err: {err}"),
        };
        let mut text = |mode: ReadMode| -> String {
            let req = Request { token: ct.clone(), method: Method::Read { pane: id, mode } };
            let v = ok(app.handle_control(req));
            v["text"].as_str().unwrap().to_string()
        };

        assert_eq!(text(ReadMode::Full), "line1\nline2\nline3\nline4\nline5");
        assert_eq!(text(ReadMode::Tail(2)), "line4\nline5");
        assert_eq!(text(ReadMode::Screen), "visible screen only");
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
    fn control_broadcast_reaches_every_running_pane_for_fleet_actor() {
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        let ct = app.control_token().to_string();
        let p1 = app.focused;
        app.apply(Action::NewPane);
        let p2 = app.focused;

        let ok = match app.handle_control(Request {
            token: ct,
            method: Method::Broadcast { text: "hi there".into(), submit: true },
        }) {
            Reply::Ok { ok } => ok,
            Reply::Err { err } => panic!("{err}"),
        };
        let mut sent: Vec<u64> =
            ok["sent"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
        sent.sort();
        assert_eq!(sent, vec![p1, p2]);
        assert_eq!(ok["count"], 2);
        assert!(app.runtimes.get(&p1).unwrap().input.ends_with(b"hi there\r"));
        assert!(app.runtimes.get(&p2).unwrap().input.ends_with(b"hi there\r"));
    }

    #[test]
    fn control_broadcast_stays_inside_pane_actors_subtree() {
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        let ct = app.control_token().to_string();
        app.tokens.insert(1, "tok1".into());
        // Pane 1 spawns a child → the child is in its subtree.
        let child = match app.handle_control(Request {
            token: "tok1".into(),
            method: Method::Spawn { adapter: "shell".into(), cwd: None, initial_input: None },
        }) {
            Reply::Ok { ok } => ok["pane"].as_u64().unwrap(),
            Reply::Err { err } => panic!("{err}"),
        };
        // The fleet spawns a sibling, outside pane 1's subtree.
        let other = match app.handle_control(Request {
            token: ct,
            method: Method::Spawn { adapter: "shell".into(), cwd: None, initial_input: None },
        }) {
            Reply::Ok { ok } => ok["pane"].as_u64().unwrap(),
            Reply::Err { err } => panic!("{err}"),
        };

        let ok = match app.handle_control(Request {
            token: "tok1".into(),
            method: Method::Broadcast { text: "x".into(), submit: false },
        }) {
            Reply::Ok { ok } => ok,
            Reply::Err { err } => panic!("{err}"),
        };
        let mut sent: Vec<u64> =
            ok["sent"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
        sent.sort();
        // Itself (1) + its spawned subtree (child) — not the fleet's sibling.
        assert_eq!(sent, vec![1, child]);
        assert_eq!(ok["count"], 2);
        assert!(app.runtimes.get(&1).unwrap().input.ends_with(b"x"));
        assert!(app.runtimes.get(&child).unwrap().input.ends_with(b"x"));
        assert!(app.runtimes.get(&other).unwrap().input.is_empty());
    }

    #[test]
    fn control_broadcast_skips_non_running_panes() {
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        let ct = app.control_token().to_string();
        let running = app.focused;
        app.apply(Action::NewPane);
        let exited = app.focused;
        app.apply(Action::NewPane);
        let never_spawned = app.focused;
        // Process exited but the pane hasn't been closed — its runtime
        // lingers with AgentStatus::Exited (see on_pty_exit); still present
        // in `runtimes`, so a naive "present ⇒ running" check would wrongly
        // include it.
        app.on_pty_exit(exited);
        // Lazy, never-spawned pane: spec present, no runtime at all — same
        // idiom as tab_summary_unknown_for_unspawned_needs_input_wins.
        app.runtimes.remove(&never_spawned);

        let ok = match app.handle_control(Request {
            token: ct,
            method: Method::Broadcast { text: "hi".into(), submit: false },
        }) {
            Reply::Ok { ok } => ok,
            Reply::Err { err } => panic!("{err}"),
        };
        let sent: Vec<u64> =
            ok["sent"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
        assert_eq!(sent, vec![running]);
        assert_eq!(ok["count"], 1);
        assert!(app.runtimes.get(&exited).unwrap().input.is_empty());
    }

    #[test]
    fn control_broadcast_counts_only_successful_writes() {
        // LOW-2: a pane can look running (status snapshot says not Exited)
        // yet have a dead write pipe (it died a moment later) — `sent`/
        // `count` must reflect delivery, not just "looked targetable".
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        let ct = app.control_token().to_string();
        let alive = app.focused;
        app.apply(Action::NewPane);
        let dying = app.focused;
        app.runtimes.get_mut(&dying).unwrap().fail_write = true;

        let ok = match app.handle_control(Request {
            token: ct,
            method: Method::Broadcast { text: "hi".into(), submit: false },
        }) {
            Reply::Ok { ok } => ok,
            Reply::Err { err } => panic!("{err}"),
        };
        let sent: Vec<u64> =
            ok["sent"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
        assert_eq!(sent, vec![alive], "the dying pane's failed write must not count as sent");
        assert_eq!(ok["count"], 1);
    }

    #[test]
    fn control_send_errors_when_the_write_fails() {
        // LOW-2 (send side): same honesty rule as broadcast — a pane whose
        // write pipe is dead must not be reported as having received input.
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        let ct = app.control_token().to_string();
        let p = app.focused;
        app.runtimes.get_mut(&p).unwrap().fail_write = true;

        let reply = app.handle_control(Request {
            token: ct,
            method: Method::Send { pane: p, text: "hi".into(), submit: false },
        });
        assert!(matches!(reply, Reply::Err { .. }), "a failed write must not be reported as sent");
        assert!(app.runtimes.get(&p).unwrap().input.is_empty());
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
            (0, 0),
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
    fn broadcast_audit_line_has_the_contracted_shape_and_omits_the_text() {
        use crate::core::control::{Method, Reply, Request};
        let dir = std::env::temp_dir().join(format!("roost-broadcast-audit-{}", std::process::id()));
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
            (0, 0),
            Some(dir.join("roost.sock")),
        )
        .unwrap();
        let ct = app.control_token().to_string();

        let (rtx, rrx) = std::sync::mpsc::channel();
        app.handle_control_msg(
            Request {
                token: ct,
                method: Method::Broadcast { text: "secret payload".into(), submit: true },
            },
            rtx,
        );
        assert!(matches!(rrx.recv().unwrap(), Reply::Ok { .. }));

        let log = std::fs::read_to_string(dir.join("control.log")).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        // F4 carry-forward fix: ctl_broadcast's own self-audit line (the
        // contracted shape, carrying the real fan-out count) must be the
        // ONLY line — handle_control_msg's generic post-dispatch audit used
        // to also fire for Broadcast, writing a second, count-less line for
        // every fleet-wide send (and, once C20 landed, a second near-
        // identical feed row too).
        assert_eq!(lines.len(), 1, "exactly one ctl line per broadcast: {log:?}");
        // len=14 is "secret payload".len(); count=1 is the lone auto-spawned pane.
        assert!(lines[0].contains("fleet broadcast len=14 submit=true -> ok count=1"), "{}", lines[0]);
        assert!(!log.contains("secret payload"), "broadcast text must never be logged: {log:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn broadcast_pushes_exactly_one_ctl_feed_line() {
        // Same carry-forward fix, pinned on the C20 feed side: the socket
        // path used to call `audit()` twice for a broadcast, which — once
        // `audit()` started feeding C20 — would have shown up as two
        // near-identical `ctl` rows in the activity overlay.
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        let ct = app.control_token().to_string();
        let (rtx, rrx) = std::sync::mpsc::channel();
        app.handle_control_msg(
            Request { token: ct, method: Method::Broadcast { text: "hi".into(), submit: false } },
            rtx,
        );
        assert!(matches!(rrx.recv().unwrap(), Reply::Ok { .. }));
        let ctl_lines: Vec<&FeedEntry> = app.feed().iter().filter(|e| e.text.starts_with("ctl ")).collect();
        assert_eq!(ctl_lines.len(), 1, "exactly one ctl feed line per broadcast: {:?}", app.feed());
        assert!(ctl_lines[0].text.contains("fleet: broadcast len=2 submit=false → ok"), "{}", ctl_lines[0].text);
    }

    #[test]
    fn broadcast_then_independent_status_transitions_dont_cross_contaminate_the_feed() {
        // One ctl line from a broadcast to N panes, then N independent
        // status transitions on a later tick — each must get its own
        // correctly-named line (no merging, no borrowed identity), and an
        // unchanged pane must stay silent.
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        let a = app.focused;
        app.apply(Action::NewPane);
        let b = app.focused;
        app.apply(Action::NewPane);
        let c = app.focused;
        app.find_spec_mut(a).unwrap().title = Some("alpha".into());
        app.find_spec_mut(b).unwrap().title = Some("bravo".into());
        app.find_spec_mut(c).unwrap().title = Some("charlie".into());

        let ct = app.control_token().to_string();
        let reply = app.handle_control(Request {
            token: ct,
            method: Method::Broadcast { text: "go".into(), submit: false },
        });
        assert!(matches!(reply, Reply::Ok { .. }));
        assert_eq!(app.feed().iter().filter(|e| e.text.starts_with("ctl ")).count(), 1);

        // A status-transition line (`"{name}: {old} → {new}"`) vs. the ctl
        // audit line, which *also* contains '→' in its own "... → ok/err"
        // outcome — `contains('→')` alone can't tell them apart.
        let is_transition = |e: &&FeedEntry| e.text.contains('→') && !e.text.starts_with("ctl ");

        // Baseline tick: first observation of each pane is silently seeded
        // (spawn owns the birth line), so this must add no transition line.
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();
        assert_eq!(app.feed().iter().filter(is_transition).count(), 0, "{:?}", app.feed());

        app.runtimes.get_mut(&a).unwrap().set_extension_status(AgentStatus::Working);
        app.runtimes.get_mut(&c).unwrap().set_extension_status(AgentStatus::NeedsInput);
        // b stays Idle — must not produce a line.
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();

        let transitions: Vec<&FeedEntry> = app.feed().iter().filter(is_transition).collect();
        assert_eq!(transitions.len(), 2, "{:?}", app.feed());
        // U2: transition lines lead with the pane id, then the display name.
        let alpha = transitions.iter().find(|e| e.text.contains("alpha:")).expect("alpha's own line");
        assert_eq!(alpha.text, format!("{a} alpha: idle → working"));
        assert!(!alpha.needs_input);
        let charlie =
            transitions.iter().find(|e| e.text.contains("charlie:")).expect("charlie's own line");
        assert_eq!(charlie.text, format!("{c} charlie: idle → needs you"));
        assert!(charlie.needs_input);
        assert!(!transitions.iter().any(|e| e.text.contains("bravo:")), "unchanged pane must stay silent");

        // The broadcast's one ctl line must still be exactly one — untouched by the tick.
        assert_eq!(app.feed().iter().filter(|e| e.text.starts_with("ctl ")).count(), 1);
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
    fn wait_does_not_leak_a_recycled_pane_ids_status_cross_subtree() {
        // LOW-3: `Workspace::next_pane_id` is max(current ids)+1, not a
        // monotonic counter — closing the highest-numbered pane frees its
        // id for reuse. A waiter registered on that id must not be handed
        // the *new*, unrelated pane's status just because the numeric id
        // matches.
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        app.tokens.insert(1, "tok1".into());
        let ct = app.control_token().to_string();

        // Pane 1 spawns its own child (pane 2, in its subtree) and waits on it.
        let child = match app.handle_control(Request {
            token: "tok1".into(),
            method: Method::Spawn { adapter: "shell".into(), cwd: None, initial_input: None },
        }) {
            Reply::Ok { ok } => ok["pane"].as_u64().unwrap(),
            Reply::Err { err } => panic!("{err}"),
        };
        assert_eq!(child, 2, "test assumes pane 2 is the highest id so far");

        let (tx, rx) = std::sync::mpsc::channel();
        app.handle_control_msg(
            Request {
                token: "tok1".into(),
                method: Method::Wait {
                    panes: vec![child],
                    until: "needs_input".into(),
                    timeout_ms: Some(60_000),
                },
            },
            tx,
        );
        assert!(rx.try_recv().is_err()); // idle child, not yet needs_input → parked
        assert_eq!(app.waiters.len(), 1);

        // Pane 2 closes (it's the highest id) and the fleet spawns an
        // unrelated pane that recycles id 2 — same number, different owner.
        app.handle_control(Request {
            token: ct.clone(),
            method: Method::Close { pane: 2, force: true },
        });
        let recycled = match app.handle_control(Request {
            token: ct,
            method: Method::Spawn { adapter: "shell".into(), cwd: None, initial_input: None },
        }) {
            Reply::Ok { ok } => ok["pane"].as_u64().unwrap(),
            Reply::Err { err } => panic!("{err}"),
        };
        assert_eq!(recycled, 2, "test assumes the id is actually recycled");

        // The new (fleet-owned, unrelated) pane 2 hits the exact status the
        // stale waiter is armed for.
        app.runtimes.get_mut(&2).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.poll_waiters();

        // Must NOT fire: pane 1 is not authorized for the fleet's new pane 2.
        assert!(rx.try_recv().is_err(), "must not leak the recycled pane's status cross-subtree");
        assert_eq!(app.waiters.len(), 1, "the waiter stays parked, not silently dropped");
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

    /// Regression: a descendant pi (a subagent, a one-shot `pi -p` tool
    /// call, a pi run by hand inside a shell pane) inherits the pane's
    /// ROOST_* env, so the moment it finishes its work and exits, its
    /// extension reports session_shutdown → "exited" *as this pane* over
    /// the socket — while the pane's real child is alive. That report must
    /// not brick the pane: no dead-pane keyboard interception (Enter there
    /// kills the live agent to "relaunch" it), no exit notification, and
    /// the next real signal recovers the badge. Only the PTY EOF is death.
    #[test]
    fn socket_exited_from_a_nested_agent_does_not_kill_a_live_pane() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.on_status(id, AgentStatus::Working);
        // The nested pi finishes its work: session_shutdown → "exited".
        assert!(app.on_status(id, AgentStatus::Exited).is_none());
        assert!(!app.focused_dead(), "socket 'exited' must not enter the dead-pane key path");
        assert_eq!(app.runtimes.get(&id).unwrap().status(), AgentStatus::Waiting);
        // The pane's own agent keeps going — status recovers, nothing sticky.
        app.on_status(id, AgentStatus::Working);
        assert_eq!(app.runtimes.get(&id).unwrap().status(), AgentStatus::Working);
        // The real death still lands: PTY EOF is the one true exit signal.
        app.on_pty_exit(id);
        assert!(app.focused_dead());
    }

    /// Host pastes: a pane whose app enabled bracketed paste (mode 2004)
    /// gets the guards it asked for — with any guard embedded in the pasted
    /// text stripped, so a hostile paste can't end the bracket early and
    /// have its remainder land as typed keystrokes (paste injection). A pane
    /// without the mode gets the bytes verbatim.
    #[test]
    fn paste_is_guarded_only_for_bracketed_panes_and_embedded_guards_strip() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.forward_paste("a\nb");
        assert_eq!(app.runtimes.get(&id).unwrap().input, b"a\nb");

        app.runtimes.get_mut(&id).unwrap().input.clear();
        app.runtimes.get_mut(&id).unwrap().bracketed = true;
        app.forward_paste("a\nb");
        assert_eq!(app.runtimes.get(&id).unwrap().input, b"\x1b[200~a\nb\x1b[201~");

        app.runtimes.get_mut(&id).unwrap().input.clear();
        app.forward_paste("x\x1b[201~evil\r");
        assert_eq!(app.runtimes.get(&id).unwrap().input, b"\x1b[200~xevil\r\x1b[201~");
    }

    /// U8(b): a paste while the rename dialog is up belongs to the dialog's
    /// buffer, not the pane hidden underneath (live QA: `PSTX` landed in the
    /// shell below while the buffer ignored it). Control bytes are stripped:
    /// a pasted newline must not commit the rename, and no ESC/CR may reach
    /// a pane title.
    #[test]
    fn paste_during_rename_fills_the_buffer_with_printables_only() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.apply(Action::RenamePane);
        app.handle_paste("api\n\x1b[0mbox\t");
        match &app.mode {
            Mode::Rename { buffer, .. } => assert_eq!(buffer, "api[0mbox"),
            _ => panic!("the rename dialog must still be up, holding the pasted text"),
        }
        assert!(app.runtimes.get(&id).unwrap().input.is_empty(), "the pane must see nothing");
        app.handle_mode_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.find_spec(id).and_then(|s| s.title.clone()), Some("api[0mbox".into()));
    }

    /// The other three modals have no text field: the paste is swallowed,
    /// never forwarded to the pane beneath.
    #[test]
    fn paste_is_swallowed_by_the_picker_help_and_feed_modals() {
        for open in [Action::QuickLaunch, Action::Help, Action::ToggleFeed] {
            let (mut app, _) = mk_app(shell_ws());
            let id = app.focused;
            app.apply(open);
            app.handle_paste("PSTX");
            assert!(
                app.runtimes.get(&id).unwrap().input.is_empty(),
                "{open:?} must swallow the paste"
            );
        }
    }

    /// ...and with no modal up, the paste still reaches the focused pane
    /// through the unchanged `forward_paste` path (guards and all).
    #[test]
    fn paste_without_a_modal_still_reaches_the_focused_pane() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.handle_paste("hello");
        assert_eq!(app.runtimes.get(&id).unwrap().input, b"hello");
        // Scroll mode draws no dialog and hides nothing — it keeps forwarding.
        app.apply(Action::ScrollMode);
        app.runtimes.get_mut(&id).unwrap().input.clear();
        app.handle_paste("more");
        assert_eq!(app.runtimes.get(&id).unwrap().input, b"more");
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
        // U14: the flash is the caller's job now — extraction claims
        // nothing until the clipboard has actually answered.
        assert!(app.flash().is_none());
        app.flash_copy(13, ClipboardOutcome::Native);
        assert_eq!(app.flash(), Some("copied 13 chars"));
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
        app.on_resize(Size::new(80, 2), (0, 0)); // no room for tab + hint + body
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
            App::<FakePane>::new(ws, registry, Box::new(store), tx, Size::new(100, 30), (0, 0), None)
                .unwrap();

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
    fn spawn_initial_input_is_buffered_until_first_output() {
        // M6 regression: initial_input used to be written the instant the
        // pane spawned, before the agent's stdin reader was necessarily up —
        // silently dropping it. It must sit buffered until the pane's first
        // output (a reliable "it's alive and reading" signal) and be flushed
        // exactly once then.
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        let ct = app.control_token().to_string();
        let id = match app.handle_control(Request {
            token: ct,
            method: Method::Spawn {
                adapter: "shell".into(),
                cwd: None,
                initial_input: Some("hello".into()),
            },
        }) {
            Reply::Ok { ok } => ok["pane"].as_u64().unwrap(),
            Reply::Err { err } => panic!("{err}"),
        };
        // Not written yet — buffered, not lost.
        assert!(app.runtimes.get(&id).unwrap().input.is_empty());
        assert!(app.pending_input.contains_key(&id));

        app.on_pty_output(id, b"agent banner\n");
        assert!(app.runtimes.get(&id).unwrap().input.ends_with(b"hello\r"));
        assert!(!app.pending_input.contains_key(&id)); // flushed exactly once

        // A second output must not re-send it.
        let len_before = app.runtimes.get(&id).unwrap().input.len();
        app.on_pty_output(id, b"more\n");
        assert_eq!(app.runtimes.get(&id).unwrap().input.len(), len_before);
    }

    #[test]
    fn closing_a_pane_before_first_output_drops_its_pending_initial_input() {
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        let ct = app.control_token().to_string();
        let id = match app.handle_control(Request {
            token: ct,
            method: Method::Spawn {
                adapter: "shell".into(),
                cwd: None,
                initial_input: Some("hello".into()),
            },
        }) {
            Reply::Ok { ok } => ok["pane"].as_u64().unwrap(),
            Reply::Err { err } => panic!("{err}"),
        };
        assert!(app.pending_input.contains_key(&id));
        app.close_pane_id(id); // closed before any output ever arrived
        assert!(!app.pending_input.contains_key(&id));
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
        // U4: keys arriving with no Alt among them fires it — on ANY
        // terminal, since that IS the symptom (the old Apple_Terminal-only
        // test left iTerm2, the README's recommendation, silent).
        assert!(wants_alt_hint(false, true, Duration::from_secs(1)));
        // One Alt key ever ends it, and it never fires before any key: an
        // untouched roost has no evidence of anything.
        assert!(!wants_alt_hint(true, true, Duration::from_secs(1)));
        assert!(!wants_alt_hint(false, false, Duration::from_secs(1)));
        // The startup window still bounds it, at the boundary and past it.
        assert!(!wants_alt_hint(false, true, ALT_HINT_WINDOW));
        assert!(!wants_alt_hint(false, true, ALT_HINT_WINDOW + Duration::from_secs(60)));
        assert!(wants_alt_hint(false, true, ALT_HINT_WINDOW - Duration::from_millis(1)));
    }

    /// C11/U4: the bar names the real setting for terminals we know, and
    /// stays terminal-agnostic (never a wrong menu path) otherwise.
    #[test]
    fn the_alt_warning_names_the_right_menu_for_the_terminals_it_knows() {
        let apple = alt_hint_line(Some("Apple_Terminal"));
        assert!(apple.contains("Use Option as Meta Key") && apple.contains("Terminal > Settings"));
        let iterm = alt_hint_line(Some("iTerm.app"));
        assert!(iterm.contains("Esc+") && iterm.contains("iTerm2 > Settings > Profiles > Keys"));
        for other in [Some("WezTerm"), Some("ghostty"), None] {
            let line = alt_hint_line(other);
            assert!(line.contains("Alt keys aren't reaching roost?"), "{other:?}");
            assert!(!line.contains("Terminal > Settings"), "{other:?} must not get a wrong path");
            assert!(!line.contains("iTerm2"), "{other:?} must not get a wrong path");
        }
        // Every variant is a full-width bar line: padded both ends (C11).
        for t in [Some("Apple_Terminal"), Some("iTerm.app"), None] {
            let line = alt_hint_line(t);
            assert!(line.starts_with(' ') && line.ends_with(' '), "{t:?}");
        }
    }

    /// The trigger end-to-end on a real `App`: silent until a key arrives,
    /// up while keys flow without Alt, gone for good once one carries Alt.
    #[test]
    fn the_alt_warning_appears_on_the_first_alt_less_key_and_dies_on_the_first_alt() {
        let (mut app, _) = mk_app(shell_ws());
        assert!(!app.show_alt_hint(), "nothing typed yet — nothing to warn about");
        app.note_key_seen();
        assert!(app.show_alt_hint());
        app.note_alt_seen();
        assert!(!app.show_alt_hint(), "Alt got through — the warning is wrong now");
        app.note_key_seen();
        assert!(!app.show_alt_hint(), "and it stays gone for the session");
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

    #[test]
    fn abbreviate_home_replaces_the_home_prefix_with_a_tilde() {
        let home = PathBuf::from("/home/nav");
        assert_eq!(abbreviate_home(&PathBuf::from("/home/nav/work"), Some(&home)), "~/work");
        assert_eq!(abbreviate_home(&PathBuf::from("/home/nav"), Some(&home)), "~");
        assert_eq!(abbreviate_home(&PathBuf::from("/etc"), Some(&home)), "/etc");
        assert_eq!(abbreviate_home(&PathBuf::from("/home/nav/work"), None), "/home/nav/work");
    }

    #[test]
    fn abbreviate_home_ignores_empty_home_and_partial_component_matches() {
        // An empty $HOME (unset/misconfigured env) must not turn every path
        // into "~" — falls back to the plain path, same as `home: None`.
        assert_eq!(abbreviate_home(&PathBuf::from("/foo/bar"), Some(&PathBuf::new())), "/foo/bar");
        // Path::strip_prefix is component-wise: "/home/navvy" is NOT inside
        // "/home/nav" even though the *string* "/home/nav" is a byte-prefix
        // of it — guards against a naive str::starts_with reimplementation.
        let home = PathBuf::from("/home/nav");
        assert_eq!(abbreviate_home(&PathBuf::from("/home/navvy/work"), Some(&home)), "/home/navvy/work");
    }

    #[test]
    fn focused_cwd_reports_the_focused_panes_directory_or_none() {
        let (mut app, _) = mk_app(shell_ws()); // default_in(/tmp): pane 1's cwd is /tmp
        assert!(app.focused_cwd().is_some());
        app.focused = 999; // no such pane
        assert!(app.focused_cwd().is_none());
    }

    /// A `StateStore` whose `save` always fails — exercises the "save
    /// failed" path (C2's honest save indicator) without touching disk.
    struct FailingStore;
    impl StateStore for FailingStore {
        fn load(&self) -> Result<Option<Workspace>> {
            Ok(None)
        }
        fn save(&self, _ws: &Workspace) -> Result<()> {
            Err(anyhow::anyhow!("disk full"))
        }
    }

    #[test]
    fn failed_save_flips_the_tab_bar_indicator() {
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut app = App::<FakePane>::new(
            shell_ws(),
            agents::registry(),
            Box::new(FailingStore),
            tx,
            Size::new(100, 30),
            (0, 0),
            None,
        )
        .unwrap();
        assert!(app.last_save_ok()); // startup counts as saved (we just loaded)
        app.apply(Action::NewPane); // triggers a save() that fails
        assert!(!app.last_save_ok());
    }

    // -- C19 jump-to-attention ----------------------------------------------

    #[test]
    fn jump_attention_empty_ring_flashes_and_does_not_move_focus() {
        let (mut app, _) = mk_app(shell_ws());
        let focused = app.focused;
        app.apply(Action::JumpAttention);
        assert_eq!(app.focused, focused);
        assert_eq!(app.flash(), Some("nothing needs you"));
    }

    #[test]
    fn jump_attention_only_member_is_focused_flashes_and_stays() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.apply(Action::JumpAttention);
        assert_eq!(app.focused, id);
        assert_eq!(app.flash(), Some("nothing else needs you"));
    }

    #[test]
    fn jump_attention_jumps_to_the_lone_needy_pane_when_not_already_focused() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1|2, focus=2
        app.runtimes.get_mut(&1).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.apply(Action::JumpAttention);
        assert_eq!(app.focused, 1);
    }

    #[test]
    fn jump_attention_visits_the_ring_in_tab_then_pane_order_and_wraps() {
        let (mut app, _) = mk_app(shell_ws()); // tab0: pane 1
        app.apply(Action::NewPane); // tab0: panes 1,2 — focus=2
        app.apply(Action::NewTab); // tab1: pane 3 — focus=3, active_tab=1
        for id in [1u64, 2, 3] {
            app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::NeedsInput);
        }
        // Ring order: tab0's pane_order() ([1,2]) then tab1's ([3]) — and its
        // size can never disagree with the hint bar's advertised count.
        assert_eq!(app.attention_ring(), vec![1, 2, 3]);
        assert_eq!(app.attention_ring().len(), app.needs_input_count());

        app.go_to_tab(0);
        app.focused = 1; // start of the ring

        app.apply(Action::JumpAttention); // same-tab hop
        assert_eq!(app.focused, 2);
        assert_eq!(app.ws.active_tab, 0);

        app.apply(Action::JumpAttention); // crosses into tab1
        assert_eq!(app.focused, 3);
        assert_eq!(app.ws.active_tab, 1);

        app.apply(Action::JumpAttention); // wraps back to the first member
        assert_eq!(app.focused, 1);
        assert_eq!(app.ws.active_tab, 0);
    }

    #[test]
    fn attention_ring_flattens_multiple_needy_panes_across_multiple_tabs_in_order() {
        // The wrap test above only ever put one needy pane in the second
        // tab. This pins the full (tab index ascending, then position in
        // that tab's pane_order()) predicate when *every* tab contributes
        // more than one ring member, with a quiet pane interleaved in each
        // tab to prove it's skipped without disturbing its neighbors' order.
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::NewPane); // tab0: 3 panes
        let tab0_order = app.pane_order();
        assert_eq!(tab0_order.len(), 3);

        app.apply(Action::NewTab);
        app.apply(Action::NewPane); // tab1: 2 panes
        let tab1_order = app.pane_order();
        assert_eq!(tab1_order.len(), 2);

        // tab0: needy at positions 0 and 2, quiet at position 1.
        app.runtimes.get_mut(&tab0_order[0]).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.runtimes.get_mut(&tab0_order[2]).unwrap().set_extension_status(AgentStatus::NeedsInput);
        // tab1: both needy.
        app.runtimes.get_mut(&tab1_order[0]).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.runtimes.get_mut(&tab1_order[1]).unwrap().set_extension_status(AgentStatus::NeedsInput);

        let expected = vec![tab0_order[0], tab0_order[2], tab1_order[0], tab1_order[1]];
        assert_eq!(app.attention_ring(), expected);
        assert_eq!(app.needs_input_count(), expected.len());
    }

    // -- C21 zoom -------------------------------------------------------------

    #[test]
    fn zoom_display_list_is_focused_pane_at_body_area_only_when_zoomed() {
        // This is exactly what `relayout()` (PTY resize) and the renderer
        // consume — see `display_rects`.
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        assert_eq!(app.display_rects().len(), app.rects().len()); // not zoomed: mirrors the real tree

        app.apply(Action::ToggleZoom);
        assert!(app.zoomed());
        let dr = app.display_rects();
        assert_eq!(dr.len(), 1);
        assert_eq!(dr[0].id, app.focused);
        assert_eq!(dr[0].rect, app.body_area());
        assert!(!dr[0].collapsed);

        app.apply(Action::ToggleZoom);
        assert!(!app.zoomed());
        assert_eq!(app.display_rects().len(), app.rects().len());
    }

    #[test]
    fn zoom_follows_focus_when_focus_moves_within_the_tab() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1|2 side by side, focus=2
        app.apply(Action::ToggleZoom);
        assert_eq!(app.display_rects()[0].id, 2);
        app.apply(Action::Focus(crate::core::layout::Dir::Left));
        assert_eq!(app.focused, 1);
        assert!(app.zoomed(), "focus moves keep zoom");
        assert_eq!(app.display_rects()[0].id, 1);
    }

    #[test]
    fn zoom_expands_a_collapsed_stack_member_first() {
        let mut panes = HashMap::new();
        for id in [1u64, 2, 3] {
            panes.insert(
                id,
                PaneSpec { adapter: "shell".into(), cwd: "/tmp".into(), session: None, title: None, spawned_by: None },
            );
        }
        let layout = LayoutNode::Stack { children: vec![1, 2, 3], expanded: 0 };
        let ws = Workspace { version: 1, active_tab: 0, tabs: vec![Tab { name: "main".into(), layout, panes }] };
        let (mut app, _) = mk_app(ws);
        app.focused = 3; // a collapsed member — expanded is currently 0 (pane 1)

        app.apply(Action::ToggleZoom);

        match &app.ws.tabs[0].layout {
            LayoutNode::Stack { expanded, children } => assert_eq!(children[*expanded], 3),
            other => panic!("expected a stack, got {other:?}"),
        }
        assert!(app.zoomed());
        assert_eq!(app.display_rects()[0].id, 3);
    }

    #[test]
    fn zoom_exits_on_every_structural_or_new_tab_action() {
        let triggers = [
            Action::NewPane,
            Action::ToggleStack,
            Action::FlipSplit,
            Action::Resize { horizontal: true, grow: true },
            Action::CycleLayout,
            Action::NewTab,
        ];
        for action in triggers {
            let (mut app, _) = mk_app(shell_ws());
            app.apply(Action::NewPane); // >=2 panes, so none of these are themselves no-ops
            app.apply(Action::ToggleZoom);
            assert!(app.zoomed(), "setup failed before {action:?}");
            app.apply(action);
            assert!(!app.zoomed(), "{action:?} must exit zoom (C21)");
        }
    }

    #[test]
    fn zoom_exits_on_tab_switch() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewTab); // 2 tabs, active=1
        app.apply(Action::ToggleZoom);
        assert!(app.zoomed());
        app.apply(Action::GoToTab(0));
        assert!(!app.zoomed());
    }

    // ---- U7: tab reachability -------------------------------------------

    /// Alt+m / Alt+i step the strip and wrap at both ends — with `Alt+1..9`
    /// stopping at nine and no cycle chord at all, tabs 10+ were reachable
    /// by nothing but the mouse (and only if they happened to be drawn).
    #[test]
    fn next_and_prev_tab_chords_step_and_wrap_at_both_ends() {
        let (mut app, _) = mk_app(shell_ws());
        for _ in 0..2 {
            app.apply(Action::NewTab); // three tabs, active = 2
        }
        assert_eq!(app.ws.active_tab, 2);
        app.apply(Action::NextTab);
        assert_eq!(app.ws.active_tab, 0, "next wraps past the end");
        app.apply(Action::PrevTab);
        assert_eq!(app.ws.active_tab, 2, "previous wraps past the start");
        app.apply(Action::PrevTab);
        assert_eq!(app.ws.active_tab, 1);
        app.apply(Action::NextTab);
        assert_eq!(app.ws.active_tab, 2);
    }

    /// `Alt+0` is the digit row's "and the rest" slot: the last tab,
    /// whatever its number — including past the ninth.
    #[test]
    fn alt_zero_jumps_to_the_last_tab_however_many_there_are() {
        let (mut app, _) = mk_app(shell_ws());
        for _ in 0..11 {
            app.apply(Action::NewTab); // twelve tabs
        }
        app.apply(Action::GoToTab(0));
        assert_eq!(app.ws.active_tab, 0);
        app.apply(Action::LastTab);
        assert_eq!(app.ws.active_tab, 11, "tab 12 has no digit of its own");
        // Alt+9 still means the ninth tab, not the last.
        app.apply(Action::GoToTab(8));
        assert_eq!(app.ws.active_tab, 8);
    }

    /// With one tab, every reach chord is the same no-op the same-tab digit
    /// is (U11) — zoom and the float survive, nothing "switches".
    #[test]
    fn tab_reach_chords_on_a_lone_tab_preserve_view_state() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleZoom);
        for action in [Action::NextTab, Action::PrevTab, Action::LastTab] {
            app.apply(action);
            assert_eq!(app.ws.active_tab, 0, "{action:?}");
            assert!(app.zoomed(), "{action:?} must not exit zoom on a lone tab");
        }
    }

    // ---- U11: per-tab focus memory + same-tab digit ----------------------

    /// The digit for the tab you're already on changes nothing: zoom, the
    /// float and focus all survive. (Live QA: Alt+1 on tab 1 exited zoom;
    /// the same path also hid the float and reset focus to the first pane.)
    #[test]
    fn a_same_tab_digit_is_a_no_op_and_keeps_zoom_float_and_focus() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1 | 2 in tab 0, focus 2
        app.apply(Action::ToggleZoom);
        let focused = app.focused;
        app.apply(Action::GoToTab(0)); // already here
        assert!(app.zoomed(), "the same-tab digit must not exit zoom");
        assert_eq!(app.focused, focused, "...nor reset focus to the first pane");

        app.apply(Action::ToggleZoom); // leave zoom, raise the float instead
        app.apply(Action::ToggleFloat);
        let float = app.focused;
        app.apply(Action::GoToTab(0));
        assert_eq!(app.focused, float, "the same-tab digit must not hide the float");
        // An out-of-range digit stays the silent no-op it always was.
        app.apply(Action::GoToTab(9));
        assert_eq!(app.focused, float);
    }

    /// Each tab returns to the pane it was left on, both ways, repeatedly.
    #[test]
    fn each_tab_returns_focus_to_the_pane_it_was_left_on() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // tab 0: panes 1 | 2
        let tab0 = app.focused; // 2 — deliberately not the first pane
        app.apply(Action::NewTab); // tab 1
        app.apply(Action::NewPane);
        app.apply(Action::NewPane); // tab 1: three panes
        let tab1 = app.focused;
        assert_ne!(tab0, tab1);

        app.apply(Action::GoToTab(0));
        assert_eq!(app.focused, tab0, "tab 0 must return to the pane it was left on");
        app.apply(Action::GoToTab(1));
        assert_eq!(app.focused, tab1, "tab 1 likewise");
        // ...and the memory keeps tracking, not just the first round trip.
        app.apply(Action::Focus(layout::Dir::Up));
        let moved = app.focused;
        assert_ne!(moved, tab1);
        app.apply(Action::GoToTab(0));
        app.apply(Action::GoToTab(1));
        assert_eq!(app.focused, moved);
    }

    /// A first visit — and a tab whose remembered pane has since closed —
    /// falls back to the tab's first pane, never to a stale or foreign id.
    #[test]
    fn tab_focus_falls_back_to_the_first_pane_when_the_memory_is_gone() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // tab 0: panes 1 | 2, focus 2
        app.apply(Action::NewTab); // tab 1 — never visited before
        assert_eq!(app.focused, 3, "a brand-new tab focuses its own pane");
        app.apply(Action::GoToTab(0));
        assert_eq!(app.focused, 2);
        // Close the remembered pane from the other side, then come back.
        app.apply(Action::GoToTab(1));
        let removed = app.handle_control(Request {
            token: app.control_token().to_string(),
            method: Method::Close { pane: 2, force: true },
        });
        assert!(matches!(removed, Reply::Ok { .. }), "{removed:?}");
        app.apply(Action::GoToTab(0));
        assert_eq!(app.focused, 1, "a closed memory falls back to the tab's first pane");
    }

    /// Closing a tab shifts every later index down — the memory must follow
    /// the tab, not the index it happened to sit at.
    #[test]
    fn tab_focus_memory_survives_a_tab_closing_ahead_of_it() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewTab); // tab 1
        app.apply(Action::NewTab); // tab 2
        app.apply(Action::NewPane); // tab 2: two panes
        let remembered = app.focused;
        app.apply(Action::GoToTab(0));
        // Close tab 1 outright: tab 2's panes shift down to index 1.
        app.apply(Action::GoToTab(1));
        let doomed = app.focused;
        app.handle_control(Request {
            token: app.control_token().to_string(),
            method: Method::Close { pane: doomed, force: true },
        });
        assert_eq!(app.ws.tabs.len(), 2, "the emptied tab is gone");
        // Being moved onto the surviving tab honors its memory...
        assert_eq!(app.focused, remembered, "landing on a tab uses its memory");
        // ...and so does a deliberate switch back, at its new index.
        app.apply(Action::GoToTab(0));
        app.apply(Action::GoToTab(1)); // the tab formerly known as 2
        assert_eq!(app.focused, remembered, "the memory travelled with the tab");
    }

    /// Undo reopens a closed tab with fresh pane ids, so there is nothing
    /// to remember — it must land on the reopened tab's first pane and
    /// leave every other tab's memory intact.
    #[test]
    fn reopening_a_closed_tab_lands_on_its_first_pane_and_leaks_no_memory() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // tab 0: panes 1 | 2
        let tab0 = app.focused;
        app.apply(Action::NewTab); // tab 1, pane 3
        let doomed = app.focused;
        app.handle_control(Request {
            token: app.control_token().to_string(),
            method: Method::Close { pane: doomed, force: true },
        });
        assert_eq!(app.ws.tabs.len(), 1);
        app.apply(Action::Undo);
        assert_eq!(app.ws.tabs.len(), 2);
        assert_eq!(app.ws.active_tab, 1);
        let reopened = app.pane_order().first().copied().unwrap();
        assert_eq!(app.focused, reopened, "the reopened tab lands on its first pane");
        app.apply(Action::GoToTab(0));
        assert_eq!(app.focused, tab0, "the surviving tab kept its own memory");
    }

    #[test]
    fn zoom_exits_on_picker_launch_but_not_on_opening_the_picker() {
        // "picker launch" (C21) is the Enter-to-spawn step inside Mode::Picker
        // — a separate code path from `apply()`'s structural actions. Merely
        // opening the picker overlay (QuickLaunch) must not exit zoom on its
        // own; only the actual spawn is structural.
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::ToggleZoom);
        assert!(app.zoomed());
        app.apply(Action::QuickLaunch);
        assert!(app.zoomed(), "opening the picker overlay must not exit zoom on its own");
        app.handle_mode_key(KeyEvent::from(KeyCode::Enter));
        assert!(!app.zoomed(), "picker launch must exit zoom (C21)");
    }

    #[test]
    fn zoom_exits_when_the_zoomed_pane_itself_closes() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1|2, focus=2
        app.apply(Action::ToggleZoom); // zoom on pane 2
        assert!(app.zoomed());
        app.apply(Action::ClosePane); // Alt+w always closes the focused (zoomed) pane
        assert!(!app.zoomed());
    }

    #[test]
    fn zoom_survives_a_control_close_of_a_different_pane() {
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1|2, focus=2
        app.apply(Action::ToggleZoom); // zoom on pane 2
        let ct = app.control_token().to_string();
        let reply = app.handle_control(Request {
            token: ct,
            method: Method::Close { pane: 1, force: false },
        });
        assert!(matches!(reply, Reply::Ok { .. }));
        assert!(app.zoomed(), "closing an unrelated pane must not touch an unrelated zoom");
    }

    #[test]
    fn zoom_exits_on_a_cross_tab_jump() {
        let (mut app, _) = mk_app(shell_ws()); // tab0: pane 1, focus=1
        app.apply(Action::NewTab); // tab1: pane 2, focus=2, active_tab=1
        app.runtimes.get_mut(&1).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.apply(Action::ToggleZoom); // zoom on pane 2 (tab1)
        assert!(app.zoomed());
        app.apply(Action::JumpAttention); // only ring member is pane 1, in a different tab
        assert_eq!(app.focused, 1);
        assert_eq!(app.ws.active_tab, 0);
        assert!(!app.zoomed(), "cross-tab jump must exit zoom (C19/C21 interplay)");
    }

    #[test]
    fn zoom_survives_a_same_tab_jump() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // tab0: panes 1,2 — focus=2
        app.runtimes.get_mut(&1).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.apply(Action::ToggleZoom); // zoom on pane 2
        app.apply(Action::JumpAttention); // same-tab jump to pane 1
        assert_eq!(app.focused, 1);
        assert!(app.zoomed(), "same-tab jump keeps zoom (zoom follows focus)");
        assert_eq!(app.display_rects()[0].id, 1);
    }

    // -- C25 canned layout cycle ----------------------------------------------

    #[test]
    fn cycle_layout_advances_grid_then_main_stack_then_all_stack_then_wraps() {
        let (mut app, _) = mk_app(shell_ws()); // 100x30 — comfortable for all 3 shapes
        app.apply(Action::NewPane);
        app.apply(Action::NewPane); // 3 panes total

        app.apply(Action::CycleLayout);
        assert!(matches!(app.ws.tabs[0].layout, LayoutNode::Split { dir: SplitDir::Horizontal, .. }));

        app.apply(Action::CycleLayout);
        match &app.ws.tabs[0].layout {
            LayoutNode::Split { ratios, .. } => assert_eq!(ratios, &vec![0.6, 0.4]),
            other => panic!("expected a main+stack split, got {other:?}"),
        }

        app.apply(Action::CycleLayout);
        assert!(matches!(app.ws.tabs[0].layout, LayoutNode::Stack { .. }));

        app.apply(Action::CycleLayout); // wraps back to grid
        assert!(matches!(app.ws.tabs[0].layout, LayoutNode::Split { dir: SplitDir::Horizontal, .. }));
    }

    #[test]
    fn cycle_layout_skips_an_unfit_arrangement_and_applies_the_next() {
        let (mut app, _) = mk_app(shell_ws());
        app.on_resize(Size::new(80, 22), (0, 0)); // body ~80x20: main+stack's 0.4 column (32) < the 36 floor
        app.apply(Action::NewPane); // 2 panes

        app.apply(Action::CycleLayout); // 1st: grid (0.5/0.5) fits
        match &app.ws.tabs[0].layout {
            LayoutNode::Split { ratios, .. } => assert_eq!(ratios, &vec![0.5, 0.5]),
            other => panic!("expected grid (0.5/0.5 split), got {other:?}"),
        }

        app.apply(Action::CycleLayout); // 2nd: main+stack unfit here → skipped → all-stack applied
        assert!(matches!(app.ws.tabs[0].layout, LayoutNode::Stack { .. }));
    }

    #[test]
    fn cycle_layout_noop_when_fewer_than_two_panes() {
        let (mut app, _) = mk_app(shell_ws()); // 1 pane
        app.apply(Action::CycleLayout);
        assert!(matches!(app.ws.tabs[0].layout, LayoutNode::Pane(_)));
        assert_eq!(app.flash(), Some("one pane — nothing to arrange"));
    }

    #[test]
    fn cycle_layout_noop_when_nothing_fits() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::NewPane); // 3 panes, organically nested splits
        app.on_resize(Size::new(30, 10), (0, 0)); // body far below the 36x10 floor for any arrangement
        let before = format!("{:?}", app.ws.tabs[0].layout);
        app.apply(Action::CycleLayout);
        assert_eq!(format!("{:?}", app.ws.tabs[0].layout), before, "layout must be untouched");
        assert_eq!(app.flash(), Some("no room to rearrange"));
    }

    #[test]
    fn cycle_layout_preserves_focus() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::NewPane);
        let focused = app.focused;
        for _ in 0..3 {
            app.apply(Action::CycleLayout);
            assert_eq!(app.focused, focused);
        }
    }

    #[test]
    fn cycle_layout_persists_like_any_layout_edit() {
        let (mut app, store) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::CycleLayout);
        let saved = store.0.lock().unwrap().clone().unwrap();
        match &saved.tabs[0].layout {
            LayoutNode::Split { ratios, .. } => assert_eq!(ratios, &vec![0.5, 0.5]), // grid, n=2
            other => panic!("expected the cycled layout to be saved, got {other:?}"),
        }
    }

    // -- C26 tab-undo pinning ---------------------------------------------------

    #[test]
    fn c26_multi_pane_tab_undo_restores_all_panes_with_sessions_and_tab_name() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewTab);
        let tab_index = app.ws.active_tab;
        app.apply(Action::NewPane);
        app.apply(Action::NewPane); // the new tab now has 3 panes

        let ids = app.pane_order();
        assert_eq!(ids.len(), 3);
        for (i, &id) in ids.iter().enumerate() {
            app.set_session(id, format!("sess-{i}"));
        }
        app.ws.tabs[tab_index].name = "fleet".into();

        // Close pane-by-pane: the third close empties (and removes) the tab.
        app.apply(Action::ClosePane);
        app.apply(Action::ClosePane);
        assert_eq!(app.ws.tabs.len(), 2, "sanity: tab not gone yet");
        app.apply(Action::ClosePane);
        assert_eq!(app.ws.tabs.len(), 1, "tab emptied and removed");

        // 3x Alt+u restores it: the tab (by name), all three panes, sessions
        // intact (C26's honest scope: re-split off the focused pane, not at
        // their original geometry — only identity and sessions are pinned).
        app.apply(Action::Undo);
        app.apply(Action::Undo);
        app.apply(Action::Undo);

        assert_eq!(app.ws.tabs.len(), 2);
        let restored = app.ws.tabs.iter().find(|t| t.name == "fleet").expect("tab reopened by name");
        assert_eq!(restored.panes.len(), 3);
        let sessions: std::collections::HashSet<Option<String>> =
            restored.panes.values().map(|s| s.session.clone()).collect();
        assert_eq!(
            sessions,
            std::collections::HashSet::from([
                Some("sess-0".to_string()),
                Some("sess-1".to_string()),
                Some("sess-2".to_string()),
            ])
        );
    }

    // -- C20 activity feed ---------------------------------------------------

    #[test]
    fn feed_ring_evicts_the_oldest_entry_past_200() {
        let (mut app, _) = mk_app(shell_ws());
        for i in 0..(FEED_CAP + 5) {
            app.push_feed(format!("entry {i}"), false);
        }
        assert_eq!(app.feed().len(), FEED_CAP);
        assert_eq!(app.feed().front().unwrap().text, "entry 5"); // oldest 5 evicted
        assert_eq!(app.feed().back().unwrap().text, format!("entry {}", FEED_CAP + 4));
    }

    #[test]
    fn tick_status_diff_feeds_one_line_per_transition_and_suppresses_exited() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;

        // First tick after spawn: baseline only — spawn owns the birth line,
        // so no transition line yet even though this is the pane's first
        // observation by diff_statuses.
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();
        assert!(app.feed().iter().all(|e| !e.text.contains('→')));

        // A real transition: one line, using the C8 state words.
        app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::Working);
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();
        let transitions: Vec<_> = app.feed().iter().filter(|e| e.text.contains('→')).collect();
        assert_eq!(transitions.len(), 1);
        assert!(transitions[0].text.contains("idle → working"), "{}", transitions[0].text);

        // A repeat tick with no status change adds no further line.
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();
        assert_eq!(app.feed().iter().filter(|e| e.text.contains('→')).count(), 1);

        // A transition *to* Exited is suppressed — the exit hook owns it.
        app.on_pty_exit(id);
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();
        assert_eq!(
            app.feed().iter().filter(|e| e.text.contains('→')).count(),
            1,
            "transition to Exited must not add a feed line: {:?}",
            app.feed()
        );
    }

    #[test]
    fn tick_status_diff_flags_a_needs_input_transition_for_the_feed_styling_rule() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick(); // baseline

        app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();

        let line = app.feed().iter().find(|e| e.text.contains('→')).expect("no transition line");
        assert!(line.needs_input);
    }

    #[test]
    fn scroll_offset_and_position_report_the_grid_clamped_view() {
        // U3: honesty surfaces (badge ↑N, hint ↑N/M) read the VIEW's truth —
        // a stored offset beyond the banked history reports as the clamp,
        // never the caller's phantom (U9's overshoot).
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        let rt = app.runtimes.get_mut(&id).unwrap();
        rt.scroll_total = 300;
        rt.set_scrollback(5000); // caller overshoots
        assert_eq!(app.scroll_offset(id), 300);
        assert_eq!(app.scroll_position(), (300, 300));
        // Back at the tail: no offset, no token.
        app.runtimes.get_mut(&id).unwrap().set_scrollback(0);
        assert_eq!(app.scroll_offset(id), 0);
    }

    // -- U2 pane identity ---------------------------------------------------

    #[test]
    fn display_name_of_is_title_else_adapter_cwd_tag() {
        let mut spec = PaneSpec {
            adapter: "pi".into(),
            cwd: PathBuf::from("/home/user/rqa-work"),
            session: None,
            title: None,
            spawned_by: None,
        };
        assert_eq!(display_name_of(&spec), "pi · rqa-work");
        spec.title = Some("worker1".into());
        assert_eq!(display_name_of(&spec), "worker1");
        // A cwd with no final component (e.g. `/`) degrades to the bare
        // adapter, no dangling separator.
        spec.title = None;
        spec.cwd = PathBuf::from("/");
        assert_eq!(display_name_of(&spec), "pi");
    }

    /// P6: the amended chain — explicit Alt+r title beats the pane's live
    /// OSC 0/2 title, which beats `adapter · cwd-tag`. The precedence is the
    /// whole contract: an app repainting its title every frame must never
    /// overwrite a name the user typed.
    #[test]
    fn display_name_chain_prefers_explicit_title_then_live_osc_title() {
        let mut spec = PaneSpec {
            adapter: "claude".into(),
            cwd: PathBuf::from("/home/user/rqa-work"),
            session: None,
            title: None,
            spawned_by: None,
        };
        // No title anywhere: the adapter/cwd fallback, unchanged.
        assert_eq!(display_name_live(&spec, None), "claude · rqa-work");
        // The pane publishes one: it wins over the fallback.
        assert_eq!(display_name_live(&spec, Some("TASK-42 refactor")), "TASK-42 refactor");
        // An empty live title is not a name — fall through.
        assert_eq!(display_name_live(&spec, Some("")), "claude · rqa-work");
        // An explicit rename beats a live title, however busy the app is.
        spec.title = Some("worker1".into());
        assert_eq!(display_name_live(&spec, Some("TASK-42 refactor")), "worker1");
        // `display_name_of` is exactly the chain with no live title.
        spec.title = None;
        assert_eq!(display_name_of(&spec), display_name_live(&spec, None));
    }

    /// P6: a plain `shell` pane ignores its live OSC title. A shell's title
    /// comes from `PS1` — by default `user@host: /path`, which restates the
    /// cwd tag already on the badge and crowds it. Agent panes keep adopting
    /// theirs (that's the whole point of P6), and a hand-launched agent
    /// inside a shell pane starts counting the moment `observe_panes`
    /// promotes the pane's adapter.
    #[test]
    fn a_shell_pane_ignores_its_ps1_title_but_an_agent_pane_adopts_one() {
        let mut spec = PaneSpec {
            adapter: "shell".into(),
            cwd: PathBuf::from("/home/user/rqa-work"),
            session: None,
            title: None,
            spawned_by: None,
        };
        let ps1 = Some("user@host: ~/rqa-work");
        assert_eq!(display_name_live(&spec, ps1), "shell · rqa-work");
        // An explicit rename still wins over everything.
        spec.title = Some("build".into());
        assert_eq!(display_name_live(&spec, ps1), "build");
        // Promoted to an agent (observe_panes saw pi running): titles count.
        spec.title = None;
        spec.adapter = "pi".into();
        assert_eq!(display_name_live(&spec, Some("TASK-9 tests")), "TASK-9 tests");
        // ...and demoting back to a shell drops the adoption again.
        spec.adapter = "shell".into();
        assert_eq!(display_name_live(&spec, Some("TASK-9 tests")), "shell · rqa-work");
    }

    /// P6: an OSC title is untrusted text headed for roost's chrome and for
    /// a sequence roost writes to its own terminal — bounded and stripped.
    #[test]
    fn a_live_title_is_sanitized_and_bounded() {
        assert_eq!(sanitize_title("  spinner · build  ", 48), "spinner · build");
        // Control bytes (an ESC that could break out of roost's own OSC 2)
        // never survive.
        assert_eq!(sanitize_title("safe\x1b]0;PWNED\x07", 48), "safe]0;PWNED");
        // Bounded in characters, on char boundaries.
        assert_eq!(sanitize_title(&"x".repeat(200), 48).chars().count(), 48);
        let wide = sanitize_title(&"日".repeat(200), 48);
        assert_eq!(wide.chars().count(), 48);
        // Nothing usable left ⇒ empty, which the chain treats as "no title".
        assert_eq!(sanitize_title("\x07\x1b", 48), "");
    }

    /// P6: the host terminal's title follows the focused pane — published
    /// once per change, through the same queue as every other host write.
    #[test]
    fn host_title_follows_focus_and_live_title() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1 | 2, focus 2

        let want = format!("\x1b]2;roost · {}\x07", app.display_name(2));
        assert!(host_contains(&mut app, want.as_bytes()), "first frame publishes a title");
        // Unchanged ⇒ nothing republished (the throttle's companion rule).
        app.last_host_title = None;
        assert!(!host_contains(&mut app, b"\x1b]2;"));

        // A focus move renames the host window. (Both fixture panes are
        // `shell · tmp`, so give the target a name of its own first —
        // otherwise the title genuinely doesn't change and republishing
        // would be the bug.)
        app.find_spec_mut(1).unwrap().title = Some("TASK-7".into());
        app.focused = 1;
        app.last_host_title = None;
        assert!(
            host_contains(&mut app, b"\x1b]2;roost \xc2\xb7 TASK-7\x07"),
            "focus change republishes"
        );

        // A rename of the focused pane takes the same path — as does a live
        // OSC title, since both resolve through `display_name`.
        app.find_spec_mut(1).unwrap().title = Some("TASK-8".into());
        app.last_host_title = None;
        assert!(host_contains(&mut app, b"\x1b]2;roost \xc2\xb7 TASK-8\x07"));
    }

    /// P6: the throttle — an agent republishing its OSC title on every
    /// spinner frame must not repaint the host title bar ~30x a second, and
    /// a change it skips must still be published on the next pass.
    #[test]
    fn host_title_updates_are_throttled_but_never_lost() {
        let (mut app, _) = mk_app(shell_ws());
        let _ = app.take_host_output(); // publish the initial title

        app.find_spec_mut(app.focused).unwrap().title = Some("first".into());
        assert!(!host_contains(&mut app, b"\x1b]2;"), "too soon after the last publish");

        app.last_host_title = Some(Instant::now() - HOST_TITLE_INTERVAL - Duration::from_millis(1));
        app.find_spec_mut(app.focused).unwrap().title = Some("second".into());
        // The skipped "first" is not replayed; the *current* name is.
        assert!(host_contains(&mut app, b"\x1b]2;roost \xc2\xb7 second\x07"));
    }

    /// P7: only the focused pane's DECSCUSR shape is ever mirrored, so
    /// moving focus to a pane that asked for nothing restores roost's
    /// default with no special case.
    #[test]
    fn only_the_focused_panes_cursor_shape_is_mirrored() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1 | 2, focus 2
        assert_eq!(app.focused_cursor_shape(), None, "nothing asked for by default");

        app.runtimes.get_mut(&1).unwrap().cursor_shape = Some(5); // blinking bar
        assert_eq!(app.focused_cursor_shape(), None, "pane 1 isn't focused");

        app.focused = 1;
        assert_eq!(app.focused_cursor_shape(), Some(5));

        // Focusing a shape-less pane reports None — which is what restores
        // the host default; no separate "reset" path can drift from it.
        app.focused = 2;
        assert_eq!(app.focused_cursor_shape(), None);
    }

    #[test]
    fn display_name_and_feed_label_fall_back_for_a_specless_pane() {
        let (app, _) = mk_app(shell_ws());
        assert_eq!(app.display_name(99), "pane 99");
        assert_eq!(app.feed_label(99), "pane 99"); // not "99 pane 99"
        assert_eq!(app.feed_label(1), "1 shell · tmp");
    }

    #[test]
    fn notifications_carry_the_display_name() {
        // U2: "shell · tmp is waiting for you", not an anonymous "shell is
        // waiting for you" — same helper as every other fleet surface.
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // focus = 2, so pane 1 is unfocused
        let msg = app.on_status(1, AgentStatus::NeedsInput);
        assert_eq!(msg.as_deref(), Some("shell · tmp is waiting for you"));
    }

    #[test]
    fn exit_feed_line_has_the_id_and_the_exit_notification_the_name() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // focus = 2
        let msg = app.on_pty_exit(1); // unfocused pane exits ⇒ notification
        assert_eq!(msg.as_deref(), Some("shell · tmp exited"));
        assert_eq!(app.feed().back().unwrap().text, "1 shell · tmp exited");
    }

    #[test]
    fn reopened_flash_and_feed_line_name_the_restored_pane() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::ClosePane); // quiet pane closes instantly
        app.apply(Action::Undo);
        let restored = app.focused;
        assert_eq!(app.flash(), Some("reopened shell · tmp"));
        assert_eq!(app.feed().back().unwrap().text, format!("reopened {restored} shell · tmp"));
    }

    #[test]
    fn busy_close_confirm_flash_names_the_pane() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        let id = app.focused;
        app.on_pty_output(id, b"x"); // Working
        app.apply(Action::ClosePane); // armed
        assert_eq!(app.flash(), Some("shell · tmp busy — Alt+w again to close"));
    }

    #[test]
    fn spawn_pushes_a_feed_line_with_id_and_display_name() {
        // U2: `spawned {id} {display_name}` — the untitled display name
        // (`shell · tmp`) already ends in the adapter/cwd tag, so no
        // `(shell)` suffix (C4's no-dup rule); titled spawns keep it (see
        // the float's `spawned N scratch (shell)` test).
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        let id = app.focused;
        let last = app.feed().back().expect("spawn should push a feed line");
        assert_eq!(last.text, format!("spawned {id} shell · tmp"));
        assert!(!last.needs_input);
    }

    #[test]
    fn close_pane_pushes_closed_name_when_the_tab_survives() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::ClosePane);
        let last = app.feed().back().unwrap();
        assert!(last.text.starts_with("closed "), "{}", last.text);
        assert!(!last.text.starts_with("closed tab"), "{}", last.text);
    }

    #[test]
    fn close_last_pane_of_a_tab_pushes_closed_tab_name_not_pane_name() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewTab);
        let ti = app.ws.active_tab;
        app.ws.tabs[ti].name = "scratch".into();
        app.apply(Action::ClosePane); // last pane of this tab → the tab is removed
        let last = app.feed().back().unwrap();
        assert_eq!(last.text, "closed tab scratch");
    }

    #[test]
    fn undo_pushes_reopened_lines_for_pane_and_tab() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::ClosePane);
        app.apply(Action::Undo);
        let last = app.feed().back().unwrap();
        assert!(last.text.starts_with("reopened "), "{}", last.text);
        assert!(!last.text.starts_with("reopened tab"), "{}", last.text);

        app.apply(Action::NewTab);
        let ti = app.ws.active_tab;
        app.ws.tabs[ti].name = "scratch".into();
        app.apply(Action::ClosePane); // removes the tab
        app.apply(Action::Undo);
        let last = app.feed().back().unwrap();
        assert_eq!(last.text, "reopened tab scratch");
    }

    #[test]
    fn on_pty_exit_pushes_a_feed_line_even_for_the_focused_pane() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused; // the sole pane starts focused
        app.on_pty_exit(id);
        let last = app.feed().back().unwrap();
        assert!(last.text.ends_with("exited"), "{}", last.text);
    }

    #[test]
    fn on_pty_exit_of_an_already_closed_pane_pushes_nothing() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1 & 2, focus = 2
        app.apply(Action::ClosePane); // deliberately closes pane 2
        let before = app.feed().len();
        app.on_pty_exit(2); // its late EOF
        assert_eq!(app.feed().len(), before, "an already-closed pane's EOF logs nothing new");
    }

    #[test]
    fn audited_control_call_pushes_exactly_one_ctl_feed_line_even_without_a_socket_dir() {
        use crate::core::control::{Method, Request};
        let (mut app, _) = mk_app(shell_ws()); // mk_app passes sock_path = None
        let ct = app.control_token().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        app.handle_control_msg(
            Request {
                token: ct,
                method: Method::Spawn { adapter: "shell".into(), cwd: None, initial_input: None },
            },
            tx,
        );
        let _ = rx.recv().unwrap();
        let ctl_lines: Vec<&FeedEntry> = app.feed().iter().filter(|e| e.text.starts_with("ctl ")).collect();
        assert_eq!(ctl_lines.len(), 1, "exactly one ctl feed line per audited call: {:?}", app.feed());
        assert!(
            ctl_lines[0].text.contains("fleet: spawn adapter=shell → ok"),
            "{}",
            ctl_lines[0].text
        );
    }

    #[test]
    fn toggle_feed_opens_from_normal_and_alt_e_closes_it_again() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleFeed);
        assert!(matches!(app.mode, Mode::Feed { offset: 0 }));

        // The same chord, while the feed is open, closes it (handled
        // specially in handle_mode_key — see its doc comment).
        let consumed = app.handle_mode_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT));
        assert!(consumed);
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn feed_esc_and_q_close_it() {
        use crossterm::event::{KeyCode, KeyEvent};
        for closer in [KeyCode::Esc, KeyCode::Char('q')] {
            let (mut app, _) = mk_app(shell_ws());
            app.apply(Action::ToggleFeed);
            app.handle_mode_key(KeyEvent::from(closer));
            assert!(matches!(app.mode, Mode::Normal));
        }
    }

    #[test]
    fn other_alt_chords_close_the_feed_and_still_apply_globally() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // 2 panes, so Focus(Left) actually moves
        app.apply(Action::ToggleFeed);
        assert!(matches!(app.mode, Mode::Feed { .. }));
        let consumed = app.handle_mode_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        assert!(!consumed, "falls through to the global Focus binding");
        assert!(matches!(app.mode, Mode::Normal), "any other Alt chord exits feed mode too");
    }

    #[test]
    fn feed_scroll_offset_clamps_at_both_ends() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, _) = mk_app(shell_ws());
        for i in 0..3 {
            app.push_feed(format!("entry {i}"), false);
        }
        app.apply(Action::ToggleFeed);
        assert!(matches!(app.mode, Mode::Feed { offset: 0 }));

        // Down (toward the live tail) at offset 0 stays clamped at 0.
        app.handle_mode_key(KeyEvent::from(KeyCode::Down));
        assert!(matches!(app.mode, Mode::Feed { offset: 0 }));

        // Up scrolls back one entry at a time, clamped at len-1 (3 entries
        // pushed by this test, plus the spawn line from mk_app's setup —
        // whatever the real count is, it must never exceed len-1).
        let cap = app.feed().len() - 1;
        for _ in 0..(cap + 5) {
            app.handle_mode_key(KeyEvent::from(KeyCode::Up));
        }
        assert!(matches!(app.mode, Mode::Feed { offset } if offset == cap));

        app.handle_mode_key(KeyEvent::from(KeyCode::Down));
        assert!(matches!(app.mode, Mode::Feed { offset } if offset == cap - 1));
    }

    #[test]
    fn feed_overlay_size_is_72x16_at_the_80x24_floor() {
        // C20: "At the 80×24 floor: 72×16, fits."
        let (mut app, _) = mk_app(shell_ws());
        app.on_resize(Size::new(80, 24), (0, 0));
        assert_eq!(feed_overlay_size(app.body_area()), (72, 16));
    }

    #[test]
    fn feed_overlay_size_clamps_on_a_tiny_body() {
        assert_eq!(feed_overlay_size(Rect::new(0, 1, 20, 6)), (16, 2));
    }

    // -- C22 floating scratch pane --------------------------------------------

    #[test]
    fn float_first_toggle_spawns_shows_and_focuses_a_shell_titled_scratch() {
        let (mut app, _) = mk_app(shell_ws());
        let real_pane = app.focused;
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        assert_ne!(float_id, real_pane);
        assert!(app.float.as_ref().unwrap().shown);
        assert_eq!(app.float.as_ref().unwrap().prev_focus, real_pane);
        let spec = app.find_spec(float_id).expect("float spec");
        assert_eq!(spec.adapter, "shell");
        assert_eq!(spec.title.as_deref(), Some("scratch"));
        assert!(app.runtimes.contains_key(&float_id), "must actually spawn");
        // Free correctness via the shared spawn_pane hook (no float-specific
        // feed code needed). U2: titled spawn = `spawned {id} {title} ({adapter})`.
        assert_eq!(app.feed().back().unwrap().text, format!("spawned {float_id} scratch (shell)"));
    }

    #[test]
    fn float_second_toggle_hides_it_without_killing_the_process() {
        let (mut app, _) = mk_app(shell_ws());
        let real_pane = app.focused;
        app.apply(Action::ToggleFloat); // spawn + show
        let float_id = app.focused;
        app.apply(Action::ToggleFloat); // hide
        assert_eq!(app.focused, real_pane);
        assert!(!app.float.as_ref().unwrap().shown);
        assert!(app.runtimes.contains_key(&float_id), "process stays alive while hidden");
    }

    #[test]
    fn float_third_toggle_reshows_the_same_pane_not_a_fresh_spawn() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        app.apply(Action::ToggleFloat); // hide
        app.apply(Action::ToggleFloat); // show again
        assert_eq!(app.focused, float_id);
        assert!(app.float.as_ref().unwrap().shown);
        assert_eq!(app.runtimes.len(), 2, "no second float spawned (real pane + float only)");
    }

    #[test]
    fn float_geometry_matches_the_c22_worked_example() {
        let (mut app, _) = mk_app(shell_ws());
        app.on_resize(Size::new(80, 24), (0, 0)); // body -> 80x22 -> float 48x13, per C22
        app.apply(Action::ToggleFloat);
        let rect = app.display_rects()[0].rect;
        assert_eq!((rect.width, rect.height), (48, 13));
    }

    #[test]
    fn float_toggle_refuses_when_the_body_is_too_small() {
        let (mut app, _) = mk_app(shell_ws());
        app.on_resize(Size::new(39, 20), (0, 0)); // body width 39 < the 40-col floor
        app.apply(Action::ToggleFloat);
        assert!(app.float.is_none(), "must not spawn below the refusal floor");
        assert_eq!(app.flash(), Some("no room for float"));
    }

    #[test]
    fn alloc_pane_id_accounts_for_the_float() {
        let (mut app, _) = mk_app(shell_ws()); // pane 1
        app.apply(Action::ToggleFloat); // float takes id 2
        let float_id = app.focused;
        assert_eq!(float_id, 2);
        app.apply(Action::ToggleFloat); // hide it, focus back to pane 1
        app.apply(Action::NewPane);
        assert_eq!(
            app.focused, 3,
            "ws.next_pane_id() alone would say 2 here — the float's own id, a collision"
        );
    }

    #[test]
    fn float_rule1_shown_routes_normally_rename_works_like_any_pane() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        app.apply(Action::RenamePane);
        assert!(matches!(app.mode, Mode::Rename { target: RenameTarget::Pane, .. }));
        // The buffer starts prefilled with the current title ("scratch",
        // like any titled pane) — clear it before typing the replacement.
        for _ in 0.."scratch".len() {
            app.handle_mode_key(KeyEvent::from(KeyCode::Backspace));
        }
        for c in "notes".chars() {
            app.handle_mode_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_mode_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.find_spec(float_id).unwrap().title.as_deref(), Some("notes"));
    }

    #[test]
    fn float_rule2_focus_dir_hides_it_and_returns_to_prev_focus() {
        let (mut app, _) = mk_app(shell_ws()); // single real pane
        let real_pane = app.focused;
        app.apply(Action::ToggleFloat);
        app.apply(Action::Focus(crate::core::layout::Dir::Right));
        assert_eq!(app.focused, real_pane);
        assert!(!app.float.as_ref().unwrap().shown);
    }

    #[test]
    fn float_rule2_jump_attention_hides_it_and_returns_to_prev_focus_without_also_jumping() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1|2, focus 2
        let real_pane = app.focused;
        app.runtimes.get_mut(&1).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.apply(Action::ToggleFloat); // float focused, prev_focus = 2
        app.apply(Action::JumpAttention);
        assert_eq!(app.focused, real_pane, "Alt+a from the float returns to prev_focus, not a ring jump");
        assert!(!app.float.as_ref().unwrap().shown);
    }

    #[test]
    fn float_rule2_tab_change_hides_it() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewTab);
        app.apply(Action::ToggleFloat);
        assert!(app.float.as_ref().unwrap().shown);
        app.apply(Action::GoToTab(0));
        assert!(!app.float.as_ref().unwrap().shown);
    }

    #[test]
    fn float_rule2_mouse_click_outside_hides_it_and_lands_on_what_it_hit() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1|2
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        assert_ne!(float_id, 1);
        app.on_click(1);
        assert_eq!(app.focused, 1);
        assert!(!app.float.as_ref().unwrap().shown);
    }

    #[test]
    fn float_rule2_mouse_click_on_the_float_itself_is_not_outside() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        app.on_click(float_id);
        assert_eq!(app.focused, float_id);
        assert!(app.float.as_ref().unwrap().shown);
    }

    #[test]
    fn float_rule3_structural_action_hides_float_first_and_does_not_wipe_the_tab_layout() {
        // The regression under test: without hiding the float first,
        // spawn_child would try to split off the float's id (not in the
        // tree), trip split_pane's empty-tab fallback, and replace the
        // whole tab's layout with just the new pane — losing panes 1 and 2.
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // tab0: panes 1,2
        let real_panes_before = app.ws.tabs[0].panes.len();
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        assert_ne!(float_id, 1);
        assert_ne!(float_id, 2);

        app.apply(Action::NewPane); // Alt+n while the float is focused

        assert_eq!(app.ws.tabs[0].panes.len(), real_panes_before + 1);
        let order = app.pane_order();
        assert!(order.contains(&1) && order.contains(&2), "original panes must stay reachable: {order:?}");
        assert!(!app.float.as_ref().unwrap().shown);
    }

    #[test]
    fn float_hides_before_cycle_layout_too() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::ToggleFloat);
        assert!(app.float.as_ref().unwrap().shown);
        app.apply(Action::CycleLayout);
        assert!(!app.float.as_ref().unwrap().shown);
        assert_eq!(app.pane_order().len(), 2, "the real 2-pane tab is untouched");
    }

    #[test]
    fn float_hides_before_picker_launch_but_not_when_merely_opening_it() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1,2
        app.apply(Action::ToggleFloat);
        assert!(app.float.as_ref().unwrap().shown);
        app.apply(Action::QuickLaunch);
        assert!(app.float.as_ref().unwrap().shown, "opening the picker alone must not hide the float");
        app.handle_mode_key(KeyEvent::from(KeyCode::Enter)); // launches the first item
        assert!(!app.float.as_ref().unwrap().shown, "picker launch (C22 rule 3) hides it");
        let order = app.pane_order();
        assert!(order.contains(&1) && order.contains(&2), "original panes survive: {order:?}");
    }

    #[test]
    fn control_spawn_while_float_focused_does_not_wipe_the_tab_layout() {
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // tab0: panes 1,2
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        let ct = app.control_token().to_string();
        let reply = app.handle_control(Request {
            token: ct,
            method: Method::Spawn { adapter: "shell".into(), cwd: None, initial_input: None },
        });
        assert!(matches!(reply, Reply::Ok { .. }));
        let order = app.pane_order();
        assert!(order.contains(&1) && order.contains(&2), "control spawn must not wipe the tab layout: {order:?}");
        assert_eq!(app.focused, float_id, "the human's focus/float must be undisturbed by a control spawn");
    }

    #[test]
    fn float_rule3_alt_z_refuses_instead_of_hiding_and_zooming_behind_it() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        app.apply(Action::ToggleZoom);
        assert!(!app.zoomed(), "must not zoom");
        assert!(app.float.as_ref().unwrap().shown, "must not hide the float either");
        assert_eq!(app.focused, float_id);
        assert_eq!(app.flash(), Some("can't zoom the float"));
    }

    #[test]
    fn float_rule4_alt_w_kills_it_for_real_with_no_undo_entry() {
        let (mut app, _) = mk_app(shell_ws());
        let real_pane = app.focused;
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        let undo_depth_before = app.undo.len();
        app.apply(Action::ClosePane);
        assert!(app.float.is_none());
        assert!(!app.runtimes.contains_key(&float_id));
        assert_eq!(app.focused, real_pane);
        assert_eq!(app.undo.len(), undo_depth_before, "scratch is not precious — no undo entry");
        assert_eq!(app.flash(), Some("scratch closed"));
        app.apply(Action::Undo); // must not somehow resurrect it
        assert!(app.float.is_none());
    }

    #[test]
    fn float_close_confirm_is_skipped_even_while_busy() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        app.runtimes.get_mut(&float_id).unwrap().set_extension_status(AgentStatus::Working);
        app.apply(Action::ClosePane); // must close immediately, no confirm arm
        assert!(app.float.is_none());
        assert!(!app.runtimes.contains_key(&float_id));
    }

    #[test]
    fn float_is_first_in_the_display_list_when_shown() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        let rects = app.display_rects();
        assert_eq!(rects[0].id, float_id, "float must be first — topmost priority for hit_test");
        assert_eq!(rects.len(), 3, "float + both real panes");
    }

    #[test]
    fn display_rects_omits_the_float_when_hidden() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleFloat);
        app.apply(Action::ToggleFloat); // hide
        let rects = app.display_rects();
        let float_id = app.float.as_ref().unwrap().id;
        assert!(rects.iter().all(|pr| pr.id != float_id));
    }

    #[test]
    fn float_shown_above_a_still_active_zoom_keeps_the_zoomed_pane_in_the_display_list() {
        // C21 "keeps zoom" + C22 rule 1: showing the float on top of an
        // already-zoomed pane doesn't disturb the zoom underneath.
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1|2, focus 2
        app.apply(Action::ToggleZoom); // zoom on pane 2
        assert!(app.zoomed());
        app.apply(Action::ToggleFloat); // float shows on top
        let float_id = app.focused;
        assert!(app.zoomed(), "zoom must still be active underneath");
        let rects = app.display_rects();
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].id, float_id);
        assert_eq!(rects[1].id, 2, "the zoom target stays pane 2, not the float");
        assert_eq!(rects[1].rect, app.body_area());
    }

    #[test]
    fn attention_ring_includes_the_float_last_when_needy() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1,2, focus 2
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        app.apply(Action::ToggleFloat); // hide it, focus back to pane 2
        app.runtimes.get_mut(&1).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.runtimes.get_mut(&float_id).unwrap().set_extension_status(AgentStatus::NeedsInput);
        assert_eq!(app.attention_ring(), vec![1, float_id]);
        assert_eq!(app.attention_ring().len(), app.needs_input_count());
    }

    #[test]
    fn jump_to_a_needy_float_shows_it_and_records_where_focus_came_from() {
        let (mut app, _) = mk_app(shell_ws());
        let real_pane = app.focused;
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        app.apply(Action::ToggleFloat); // hide, focus back to real_pane
        app.runtimes.get_mut(&float_id).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.apply(Action::JumpAttention);
        assert_eq!(app.focused, float_id);
        assert!(app.float.as_ref().unwrap().shown);
        assert_eq!(app.float.as_ref().unwrap().prev_focus, real_pane);
    }

    #[test]
    fn control_close_of_the_float_is_refused() {
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        let ct = app.control_token().to_string();
        let reply =
            app.handle_control(Request { token: ct, method: Method::Close { pane: float_id, force: true } });
        match reply {
            Reply::Err { err } => assert_eq!(err, "cannot close the scratch pane"),
            other => panic!("expected refusal, got {other:?}"),
        }
        assert!(app.float.as_ref().unwrap().shown, "the float must survive the refused close");
    }

    #[test]
    fn find_spec_learns_the_float_send_and_read_work_by_id() {
        use crate::core::control::{Method, ReadMode, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        assert!(app.find_spec(float_id).is_some());
        let ct = app.control_token().to_string();
        let reply = app.handle_control(Request {
            token: ct.clone(),
            method: Method::Send { pane: float_id, text: "hi".into(), submit: false },
        });
        assert!(matches!(reply, Reply::Ok { .. }));
        assert_eq!(app.runtimes.get(&float_id).unwrap().input, b"hi");
        let reply =
            app.handle_control(Request { token: ct, method: Method::Read { pane: float_id, mode: ReadMode::Screen } });
        assert!(matches!(reply, Reply::Ok { .. }));
    }

    #[test]
    fn float_never_appears_in_ctl_list() {
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        let ct = app.control_token().to_string();
        let reply = app.handle_control(Request { token: ct, method: Method::List });
        let Reply::Ok { ok } = reply else { panic!("expected ok") };
        let ids: Vec<u64> = ok.as_array().unwrap().iter().map(|v| v["pane"].as_u64().unwrap()).collect();
        assert!(!ids.contains(&float_id));
    }

    #[test]
    fn float_never_receives_a_fleet_broadcast() {
        // LOW-1: the float is the human's private interactive scratch shell
        // (Alt+f), never a fleet member — `send --all` must not type/submit
        // into it, and it must not inflate the reported count.
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        let ct = app.control_token().to_string();
        let p1 = app.focused;
        app.apply(Action::ToggleFloat); // spawns + shows the float
        let float_id = app.focused; // C22: toggling on focuses the float
        assert_ne!(float_id, p1);

        let ok = match app.handle_control(Request {
            token: ct,
            method: Method::Broadcast { text: "hi".into(), submit: true },
        }) {
            Reply::Ok { ok } => ok,
            Reply::Err { err } => panic!("{err}"),
        };
        let sent: Vec<u64> =
            ok["sent"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
        assert_eq!(sent, vec![p1], "the float must never be a broadcast target");
        assert_eq!(ok["count"], 1);
        assert!(
            app.runtimes.get(&float_id).unwrap().input.is_empty(),
            "the human's private scratch shell must never receive broadcast input"
        );
    }

    #[test]
    fn float_is_never_persisted_to_the_saved_workspace() {
        let (mut app, store) = mk_app(shell_ws());
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        let saved = store.0.lock().unwrap().clone().unwrap();
        assert!(saved.tabs.iter().all(|t| !t.panes.contains_key(&float_id)));
    }

    #[test]
    fn dead_float_relaunches_via_respawn_focused() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        app.runtimes.get_mut(&float_id).unwrap().kill();
        assert!(app.focused_dead());
        app.respawn_focused(false);
        assert!(!app.focused_dead());
        assert_eq!(app.focused, float_id);
    }

    // -- C23 raw pass-through --------------------------------------------------

    #[test]
    fn toggle_raw_action_flips_membership_on_the_focused_pane() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        assert!(!app.is_raw(id));
        app.apply(Action::ToggleRaw);
        assert!(app.is_raw(id));
        app.apply(Action::ToggleRaw);
        assert!(!app.is_raw(id));
    }

    #[test]
    fn raw_routing_active_requires_normal_mode_raw_and_alive() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        assert!(!app.raw_routing_active(), "not raw yet");
        app.apply(Action::ToggleRaw);
        assert!(app.raw_routing_active());

        app.mode = Mode::Help;
        assert!(!app.raw_routing_active(), "only applies in Normal mode");
        app.mode = Mode::Normal;
        assert!(app.raw_routing_active());

        app.runtimes.get_mut(&id).unwrap().kill();
        assert!(!app.raw_routing_active(), "a dead pane never raw-routes");
    }

    #[test]
    fn close_pane_id_cleans_up_stale_raw_membership() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        let id = app.focused;
        app.apply(Action::ToggleRaw);
        assert!(app.is_raw(id));
        app.apply(Action::ClosePane);
        assert!(!app.is_raw(id));
    }

    #[test]
    fn control_send_still_delivers_bytes_to_a_raw_pane() {
        // C23's routing predicate only gates the *keyboard* path
        // (main.rs's handle_key) — the control socket writes straight to
        // the runtime via ctl_send and must keep working while the focused
        // pane is flagged raw; raw is not consulted anywhere on that path.
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.apply(Action::ToggleRaw);
        assert!(app.is_raw(id));

        let ct = app.control_token().to_string();
        let reply = app.handle_control(Request {
            token: ct,
            method: Method::Send { pane: id, text: "hello".into(), submit: true },
        });
        assert!(matches!(reply, Reply::Ok { .. }));
        assert!(app.runtimes.get(&id).unwrap().input.ends_with(b"hello\r"));
        assert!(app.is_raw(id), "control send must not disturb the raw flag");
    }

    // -- C24 keyboard copy mode -------------------------------------------------

    /// Extract the C24 cursor from `Mode::Copy`, panicking with a clear
    /// message otherwise — every test in this section drives copy mode via
    /// `Action::CopyMode` first, so this should always match.
    fn copy_cursor(app: &App<FakePane>) -> (u16, u16) {
        match &app.mode {
            Mode::Copy { cursor } => *cursor,
            _ => panic!("expected Mode::Copy"),
        }
    }

    #[test]
    fn copy_mode_cursor_starts_bottom_left_of_the_focused_pane() {
        // 100x30 term, single pane fills the body (100x28) -> inner 98x26.
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::CopyMode);
        assert_eq!(copy_cursor(&app), (25, 0));
    }

    #[test]
    fn copy_cursor_motions_clamp_to_the_inner_grid() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::CopyMode); // cursor (25, 0), grid 26 rows x 98 cols
        for _ in 0..30 {
            app.handle_mode_key(KeyEvent::from(KeyCode::Char('k'))); // up, clamps at row 0
        }
        assert_eq!(copy_cursor(&app), (0, 0));
        for _ in 0..120 {
            app.handle_mode_key(KeyEvent::from(KeyCode::Char('l'))); // right, clamps at col 97
        }
        assert_eq!(copy_cursor(&app), (0, 97));
        for _ in 0..40 {
            app.handle_mode_key(KeyEvent::from(KeyCode::Down)); // clamps at row 25
        }
        assert_eq!(copy_cursor(&app), (25, 97));
        for _ in 0..120 {
            app.handle_mode_key(KeyEvent::from(KeyCode::Left)); // clamps at col 0
        }
        assert_eq!(copy_cursor(&app), (25, 0));
    }

    #[test]
    fn copy_cursor_0_and_dollar_jump_to_column_bounds() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::CopyMode);
        app.handle_mode_key(KeyEvent::from(KeyCode::Char('$')));
        assert_eq!(copy_cursor(&app).1, 97);
        app.handle_mode_key(KeyEvent::from(KeyCode::Char('0')));
        assert_eq!(copy_cursor(&app).1, 0);
    }

    #[test]
    fn copy_v_toggles_anchor_and_movement_extends_the_selection() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::CopyMode); // cursor (25, 0)
        app.handle_mode_key(KeyEvent::from(KeyCode::Char('v'))); // anchor at (25, 0)
        let sel = app.selection.expect("v sets an anchor");
        assert_eq!(sel.anchor, (25, 0));
        assert_eq!(sel.cursor, (25, 0));

        app.handle_mode_key(KeyEvent::from(KeyCode::Char('k')));
        app.handle_mode_key(KeyEvent::from(KeyCode::Char('k')));
        let sel = app.selection.expect("still selecting");
        assert_eq!(sel.anchor, (25, 0), "anchor stays put");
        assert_eq!(sel.cursor, (23, 0), "movement extends to the new cursor");
        assert_eq!(copy_cursor(&app), (23, 0));

        app.handle_mode_key(KeyEvent::from(KeyCode::Char('v'))); // toggle off
        assert!(app.selection.is_none());
    }

    #[test]
    fn copy_y_yanks_with_a_selection_and_stashes_it_for_the_clipboard() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, _) = mk_app(shell_ws());
        app.runtimes.get_mut(&1).unwrap().grab = "yanked text".into();
        app.apply(Action::CopyMode);
        app.handle_mode_key(KeyEvent::from(KeyCode::Char('v')));
        app.handle_mode_key(KeyEvent::from(KeyCode::Char('y')));
        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.selection.is_none());
        assert_eq!(app.take_pending_yank().as_deref(), Some("yanked text"));
        // U14: same for the keyboard path — the yank is stashed for the
        // composition root, which flashes the clipboard's real answer.
        assert!(app.flash().is_none());
    }

    #[test]
    fn copy_enter_also_yanks_same_as_y() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, _) = mk_app(shell_ws());
        app.runtimes.get_mut(&1).unwrap().grab = "abc".into();
        app.apply(Action::CopyMode);
        app.handle_mode_key(KeyEvent::from(KeyCode::Char('v')));
        app.handle_mode_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.take_pending_yank().as_deref(), Some("abc"));
    }

    #[test]
    fn copy_y_without_a_selection_flashes_and_stays_in_copy_mode() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::CopyMode);
        app.handle_mode_key(KeyEvent::from(KeyCode::Char('y')));
        assert!(matches!(app.mode, Mode::Copy { .. }), "must stay in copy mode");
        assert_eq!(app.flash(), Some("nothing selected"));
        assert!(app.take_pending_yank().is_none());
    }

    #[test]
    fn copy_esc_and_q_clear_the_selection_and_exit() {
        use crossterm::event::{KeyCode, KeyEvent};
        for closer in [KeyCode::Esc, KeyCode::Char('q')] {
            let (mut app, _) = mk_app(shell_ws());
            app.apply(Action::CopyMode);
            app.handle_mode_key(KeyEvent::from(KeyCode::Char('v')));
            assert!(app.selection.is_some());
            app.handle_mode_key(KeyEvent::from(closer));
            assert!(matches!(app.mode, Mode::Normal));
            assert!(app.selection.is_none());
        }
    }

    #[test]
    fn copy_mouse_drag_replaces_the_keyboard_selection_and_moves_the_cursor() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::CopyMode);
        app.handle_mode_key(KeyEvent::from(KeyCode::Char('v'))); // keyboard anchor at (25, 0)
        assert_eq!(app.selection.unwrap().anchor, (25, 0));

        // A fresh mouse drag starts an entirely new selection, overwriting
        // the keyboard one, and also moves the Mode::Copy cursor (C24: "a
        // drag also moves the cursor to the drag point").
        app.begin_selection(1, 3, 4);
        app.extend_selection(5, 6);
        let sel = app.selection.unwrap();
        assert_eq!(sel.anchor, (3, 4));
        assert_eq!(sel.cursor, (5, 6));
        assert_eq!(copy_cursor(&app), (5, 6));
    }
}
