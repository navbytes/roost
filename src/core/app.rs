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
use crate::core::control::{Actor, Method, ReadMode, Reply, Request, TokenTable, UNAUTHORIZED_MSG};
use crate::core::event::AppEvent;
use crate::core::layout::{self, LayoutNode, PaneId, PaneRect, SplitDir};
use crate::core::session_resolver;
use crate::core::status::AgentStatus;
use crate::core::workspace::{PaneSpec, Tab, Workspace};
use crate::ports::{ClipboardOutcome, Observation, PaneBackend, StateStore};
use crate::ui::input::{Action, Keymap};
use crate::ui::render::state_word;

const DETECT_INTERVAL: Duration = Duration::from_secs(2);

/// Slack subtracted from a promoted pane's "last seen as a shell" bound.
///
/// `observe_panes` reads the process tree on a `DETECT_INTERVAL` tick, so
/// its view can lag the truth by up to one tick: an agent that started at T
/// may still be observed as a shell a moment later, and its session file —
/// written at T — would then fall just outside a strict bound and never be
/// found. A few seconds of slack absorbs that while still excluding
/// everything a previous run left behind, which is the whole point.
const PROMOTION_GRACE: Duration = Duration::from_secs(10);

/// How long the filesystem fallback keeps looking for a pane's session file
/// before giving up.
///
/// `pending_detect` was previously only cleared on success or when the pane
/// vanished, so a pane whose agent never wrote a file roost could attribute
/// stayed pending for the life of the process — and since no adapter
/// overrides `detect_session`, each tick meant a full recursive walk of the
/// whole session root (`~/.pi/agent/sessions`: every project, all history)
/// stat-ing every file, twice a minute, forever.
///
/// A minute is far longer than any agent takes to create its session file,
/// so giving up costs nothing real — and when it is wrong the failure is
/// the safe one: the pane simply starts fresh next launch instead of
/// resuming. The exact channel (the agent-side extension, design doc §6.1)
/// does not go through `pending_detect` at all and keeps working after
/// this expires.
const DETECT_GIVE_UP: Duration = Duration::from_secs(60);

/// F1: how long the "Alt keys aren't reaching roost" hint stays up after its
/// most recent evidence (`App::alt_swallow_at`) — not since launch.
/// `is_alt_swallow_char`'s evidence is real but not unambiguous (every
/// character in it is also a directly-typed letter on some non-US macOS
/// layout), so this bounds a false positive to a few seconds instead of
/// latching for the session; a user who keeps producing the evidence keeps
/// re-arming the window.
const ALT_HINT_WINDOW: Duration = Duration::from_secs(8);

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

/// P0-3: how much of a spawn failure's full cause chain (`dead_reason`) is
/// kept for the dead-pane bar (`spawn failed: {reason}`, C16) and the feed —
/// one line, bounded so a pathological program path or a long chain can't
/// blow past either. Comfortably wider than any real ENOENT/EACCES message;
/// same shape as `pty::HOST_NOTIFY_CAP`.
const DEAD_REASON_CAP: usize = 200;

/// What a rename overlay is editing. Pane renames moved into the combined
/// C32 editor (`Mode::PaneEdit`) — `Tab` is the one target left, and the
/// enum stays only so the mode's shape doesn't churn twice if a third
/// single-line target ever appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameTarget {
    Tab,
}

/// C32: the note editor's line cap. A parking note is a headline plus a
/// few lines of "what's next" — eight is comfortably past the 2–3 the
/// surface is for, while keeping the dialog a dialog (it must fit inside
/// an 80×24 body with the C12 frame). At the cap, a newline is swallowed,
/// not split; a paste's overflow joins the last line instead of vanishing.
pub const NOTE_MAX_LINES: usize = 8;

/// UI mode: non-Normal modes capture all keys (see handle_mode_key).
pub enum Mode {
    Normal,
    /// U16: a one-line text field. `cursor` is the insertion point as a
    /// **char** index into `buffer` (0 = before the first char,
    /// `buffer.chars().count()` = at the end) — chars, not bytes, because
    /// every motion, edit and the `▏` caret's position are all counted in
    /// what the user sees, and a byte index would split a multi-byte name.
    Rename {
        buffer: String,
        cursor: usize,
        target: RenameTarget,
    },
    /// C32 (amended): the combined pane editor (Alt+r) — the pane's whole
    /// text surface in one dialog. `name` is the single-line title field;
    /// `lines` is the parking note split on newlines, never empty (an
    /// empty note edits as one empty line). `row` 0 is the name field,
    /// `row` i>0 is `lines[i-1]`; `col` is a **char** index into the
    /// current row for the same reason Rename's cursor is. Motion crosses
    /// the name/note seam freely, **edits never do** — Backspace/Delete
    /// at the boundary are walls, so the two fields can't merge by typo.
    /// `pane` pins the editor's owner at open time, so the commit can
    /// never land on a different pane than the dialog opened on.
    PaneEdit {
        name: String,
        lines: Vec<String>,
        row: usize,
        col: usize,
        pane: PaneId,
    },
    /// C36: the broadcast composer (`Alt+'`). `lines` is the message being
    /// written, split on newlines and never empty; `row` indexes it and
    /// `col` is a **char** index into that row, for the same reason
    /// Rename's cursor is. `status_filter` narrows the targets — the same
    /// `Option<AgentStatus>` C27's roster cycles with `Tab`, and the reason
    /// the dialog can show a live count of who is about to receive this.
    ///
    /// Deliberately **not** a sticky sync-input mode. A persistent "every
    /// keystroke goes to five panes" state is the unguarded-destructive
    /// shape U1 exists to prevent: you would have to notice a mode
    /// indicator to avoid typing into a fleet. One composed message, one
    /// deliberate send.
    Broadcast {
        lines: Vec<String>,
        row: usize,
        col: usize,
        status_filter: Option<AgentStatus>,
    },
    /// U20: the quick-launch picker. `selection` indexes the **filtered**
    /// adapter list (`filter` is the type-ahead query, empty = everything);
    /// `cwd` indexes the recent-directory column; `on_cwd` says which of the
    /// two columns `↑`/`↓` are steering.
    Picker {
        selection: usize,
        filter: String,
        cwd: usize,
        on_cwd: bool,
    },
    /// [P14] Scroll mode carries **no offset of its own** — a cached mirror
    /// of the view's position would be a second answer to a question with
    /// exactly one, since the vt100 grid auto-advances its offset as new
    /// rows bank while the view is scrolled. Every reader and every step
    /// asks `PaneBackend::scroll_offset` — the grid — instead.
    Scroll,
    /// Text selection — mouse drag, or the C24 keyboard cursor. `cursor` is
    /// (row, col) in the focused pane's inner cell space; both input
    /// methods write the same `App::selection`.
    Copy {
        cursor: (u16, u16),
    },
    /// C15: the full-keymap overlay (Alt+?). `top` is the first visible
    /// content row — 0 whenever the keymap fits, which is the common case;
    /// the scroll keys only do anything on a terminal too short to show the
    /// whole table at once.
    ///
    /// **[F9] `filter` is the type-ahead query, and `None` is not the same
    /// as `Some("")`.** `None` means the overlay is in its original state:
    /// every key that is not a scroll key dismisses it. `Some(q)` means `/`
    /// has opened the filter, and from then on printables type — including
    /// `q`, which is why the two states have to be distinguishable rather
    /// than an empty string standing in for "not filtering". An empty
    /// `Some` is a real state: `/` then Backspace, still filtering, showing
    /// everything.
    Help {
        top: usize,
        filter: Option<String>,
    },
    /// C20 activity-feed overlay; `offset` counts entries back from the
    /// newest (0 = live tail).
    Feed {
        offset: usize,
    },
    /// C27: the fleet roster — every pane in the workspace, grouped by tab.
    ///
    /// The cursor is a **`PaneId`, not a row index**: the roster is a live
    /// monitoring surface (panes spawn and exit while it is open), and an
    /// index would silently re-point at a different pane the moment the list
    /// under it changed. `filter` is the type-ahead query (empty = every
    /// pane); `top` is the first visible row of the scrolled window, the one
    /// piece of view state the renderer and the mouse hit-test share.
    ///
    /// `status_filter` narrows the list to one severity tier (`None` = every
    /// tier), cycled with `Tab`/`Shift+Tab` — the two non-`Char` keys the
    /// type-ahead (every printable, C27's own rule) cannot claim. Composes
    /// with `filter` by AND: a row must satisfy both.
    Roster {
        cursor: PaneId,
        filter: String,
        top: usize,
        status_filter: Option<AgentStatus>,
    },
    /// P21: the incremental scrollback-search prompt, opened with `/` from
    /// Scroll or Copy mode. The search itself lives on `App::search` (it
    /// outlives the prompt — `n`/`N` keep working after Enter); this only
    /// carries what Enter and Esc need to hand the mode back: the copy
    /// cursor to restore, or `None` for "came from Scroll mode".
    Search {
        copy_cursor: Option<(u16, u16)>,
    },
}

/// P21: one incremental search over a pane's scrollback + screen.
///
/// The haystack is captured **once**, when the search opens, so every
/// keystroke re-filters a frozen buffer instead of racing the pane's live
/// output — and so the line indices below mean the same thing from the
/// first character typed to the last `n`. Line `i` of the haystack is the
/// `i`-th row of the pane's whole history (`grab_all_text`, one line per
/// grid row), which sits `total - i` rows above the live tail.
///
/// Bounded staleness, deliberately accepted: if the pane banks enough new
/// output during the search to evict rows off the *front* of a full
/// scrollback ring, capture-time indices drift by however many rows were
/// dropped, so a jump can land a few rows off. What it can never do is lie
/// about where the view ended up — every jump writes through
/// `set_scrollback` and re-reads the grid-clamped result (U9), so the `↑N`
/// badge and Scroll mode's `↑N/M` stay true whatever the arithmetic asked
/// for. A **resize** is the one drift roost does not accept: P5's reflow
/// rewraps the grid wholesale, so `App::refresh_search` re-captures rather
/// than letting the indices rot (see there).
#[derive(Debug, Clone)]
pub struct Search {
    /// The pane this search is about. Its offsets are meaningless anywhere
    /// else, so leaving the pane ends the search.
    pub pane: PaneId,
    /// What has been typed so far. Empty = nothing to match yet.
    pub query: String,
    /// The pane's whole history + screen at open time, one entry per row.
    lines: Vec<String>,
    /// Rows banked at open time — the anchor that turns a line index into a
    /// scrollback offset.
    total: usize,
    /// Every hit in reading order (oldest first): (line index, char column).
    pub matches: Vec<(usize, usize)>,
    /// Index into `matches` of the hit the view is parked on. Meaningless
    /// while `matches` is empty.
    pub current: usize,
    /// The view offset the search opened on — what Esc puts back.
    origin: usize,
}

impl Search {
    /// The current hit's (line index, char column), or None with no matches.
    pub fn current_match(&self) -> Option<(usize, usize)> {
        self.matches.get(self.current).copied()
    }

    /// Width of a highlight run, in characters — the query's own length.
    pub fn width(&self) -> usize {
        self.query.chars().count()
    }

    /// A search over a literal haystack, for tests that need to *render* one
    /// (the chrome and highlight units) without driving a whole app. The
    /// hit list is derived, never hand-written, so a render test can't pin a
    /// match set the finder would never produce.
    #[cfg(test)]
    pub fn over(lines: Vec<String>, query: &str, current: usize) -> Self {
        let matches = find_matches(&lines, query);
        Self { pane: 1, query: query.to_string(), lines, total: 0, matches, current, origin: 0 }
    }
}

/// S4: (text, fire-at) for a copy staged from a double-click's own release
/// — see `App::release_native_selection`. The text is grabbed once, at
/// release time, and staged as itself rather than as pane/anchor/cursor to
/// re-extract later: a streaming pane can scroll or overwrite those cells
/// before the deadline fires, so a re-read could silently copy whatever now
/// occupies them instead of the word actually double-clicked.
type PendingCopy = (String, Instant);

/// An in-progress / completed text selection within one pane. Coordinates are
/// (row, col) in the pane's inner (border-excluded) cell space, 0-based.
///
/// C29: the same struct now backs two gestures that never run at once — the
/// C24 keyboard/mouse selection inside `Mode::Copy`, and a **native**
/// selection made directly in `Mode::Normal` over a pane whose app never
/// asked for the mouse (`MouseProto::None`). Both paint through the same C17
/// `highlight_selection` pass and extract through the same `grab_text` path;
/// only how each is started/extended/cleared differs — compare
/// `begin_selection`/`extend_selection`/`finish_selection` (shared by both)
/// with `click_count`/`select_word_at`/`select_line_at`/
/// `extend_selection_to`/`finish_native_selection` (native-only).
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
    /// U25: the pane this line is *about*, when there is one — what Enter
    /// on the entry focuses. `None` for lines about no single live pane: a
    /// closed pane or tab (gone; `Alt+u` is that line's recovery path, not
    /// a jump) and control-plane calls (about a request, not a pane). The
    /// id is a jump target, not a guarantee: pane ids are recycled, so
    /// Enter re-checks the pane still exists before moving focus.
    pub pane: Option<PaneId>,
}

/// How long the "copied" flash stays in the hint bar.
const FLASH_WINDOW: Duration = Duration::from_secs(2);

/// How long an armed destructive-close confirmation stays live: press the same
/// key again within this window to actually close/quit. A confirm's *flash*
/// carries this same window (U22): the prompt must be visible for exactly as
/// long as the second press would fire — the old FLASH_WINDOW-sized prompt
/// left a silent final second where Alt+w/Alt+q still confirmed.
const CONFIRM_WINDOW: Duration = Duration::from_secs(3);

/// C14 / F8: what `picker_filtered` appends to an adapter that is not on
/// `$PATH`. Public because `render.rs`'s `ADAPTER_COL` is sized for exactly
/// this suffix on the longest registry id, and the gate that checks the
/// column has to build the same worst case — two spellings of one suffix is
/// how a column ends up sized for a row the picker no longer draws.
pub const PICKER_MISSING_SUFFIX: &str = "not found";

/// C29: the standard macOS double-click interval. Crossterm reports no click
/// count at all, so roost derives double/triple-click itself, from the gap
/// between two presses (`click_count`).
const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(500);
/// C29: a repeat click within this many *cells* of the last one still counts
/// as "the same spot". A terminal cell is already coarser than a mouse
/// pixel, so even a tolerance of one is generous — but zero would fail a
/// double-click exactly when a real hand drifts a hair between two presses.
const CLICK_TOLERANCE: u16 = 1;

/// How many closed panes/tabs the undo stack keeps.
const UNDO_DEPTH: usize = 20;

/// Cap on concurrently parked `wait` requests. Each parked wait holds a
/// socket connection slot for up to its whole timeout, and one-shot calls
/// like status/list share that same pool (`MAX_CONN` in sock.rs) — without a
/// cap, enough parked waits could starve those of a slot to even report in
/// on. Left well below that pool size so plenty always stay free.
const MAX_WAITS: usize = 16;

/// Parked `wait`s any single principal may hold at once.
///
/// The global cap alone let one caller take every slot: waiters are freed
/// only by firing or timing out, so sixteen fire-and-forget waits with long
/// timeouts denied the verb fleet-wide for up to 24 hours. A per-actor
/// share means a pane can only ever starve itself.
const MAX_WAITS_PER_ACTOR: usize = 4;

/// Panes one `wait` may name. `poll_waiters` walks every id of every waiter
/// on the UI thread each event-loop iteration, and the list arrives
/// unbounded from the socket — see `register_waiter`.
const MAX_WAIT_PANES: usize = 64;

/// C20: activity-feed ring buffer capacity — oldest evicted first.
const FEED_CAP: usize = 200;

/// U20: how many recent working directories the picker's second column
/// keeps. Nine, so every one of them is reachable by the `1..9` row
/// accelerators' worth of glancing — and so the dialog stays a thing you
/// read in one look rather than scroll.
const MAX_RECENT_CWDS: usize = 9;

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
/// never masquerades as idle. `Exited` (U13) is the same honesty one step
/// further: without it, a tab whose agents are *dead* renders the same
/// Quiet blank as a background tab with nothing to say. `render` maps each
/// to a glyph + colour.
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
    /// The pane keystrokes go to. Never assign this directly — go through
    /// `set_focus`, which is also where P10's `CSI I`/`CSI O` reports are
    /// produced; a bare write would move focus without telling the panes.
    pub focused: PaneId,
    /// C35: the pane focus was on immediately before the current one — the
    /// target of "go back". Maintained in `set_focus`, which every focus
    /// move funnels through, so a click, an arrow, a tab switch and a
    /// roster jump all leave the same trail.
    ///
    /// `None` until focus has moved once, and never equal to `focused`
    /// (`set_focus` only records a real transition), so a toggle always
    /// ping-pongs: A→B leaves A, going back to A leaves B.
    alternate: Option<PaneId>,
    /// C40: the pane `Alt+Shift+x` marked, waiting for `Alt+Shift+v` to
    /// pull it into whichever tab is active by then. `None` almost always —
    /// a mark is a gesture in progress, not a state a workspace lives in,
    /// so it is session-only and never persisted.
    ///
    /// Cleared when the marked pane closes: pane ids are recycled
    /// (`next_pane_id` is max+1), so a stale mark would not merely be dead —
    /// it would eventually name a stranger. Same hygiene as `raw` /
    /// `tab_focus`, and for the same reason.
    marked: Option<PaneId>,
    /// P10: does roost's own window hold the host terminal's focus? Starts
    /// **true** — a host that never reports focus (no mode 1004 support, a
    /// bare PTY, another multiplexer) must not leave every pane believing it
    /// is permanently blurred. While false, roost withholds pane-level focus
    /// reports entirely: nothing inside roost is focused when roost isn't,
    /// and the pane that ends up focused collects its `CSI I` the moment the
    /// window comes back.
    host_focused: bool,
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
    /// When each pane was last observed running **no** agent — a plain
    /// shell. The lower bound for a *promoted* pane's session detection.
    ///
    /// A pane promoted by the user typing `pi` at a shell prompt cannot own
    /// a session file older than the last moment it was still a shell, so
    /// this is the honest window. It used to be `SystemTime::UNIX_EPOCH` —
    /// no bound at all — which let the scan claim the newest *unclaimed*
    /// file in the project whenever it was written, i.e. a conversation
    /// from days ago. See `PROMOTION_GRACE`.
    last_shell_seen: HashMap<PaneId, SystemTime>,
    last_detect: Instant,
    sock_path: Option<PathBuf>,
    /// `$HOME`, resolved once at startup — `focused_cwd()`'s `~`-abbreviation
    /// reads this instead of asking the environment on every frame (D4).
    home: Option<PathBuf>,
    started: Instant,
    /// Set the first time an Alt-modified key event arrives, so the
    /// "Alt keys aren't reaching roost" startup hint can stop once we know
    /// they are.
    alt_seen: bool,
    /// F1: when a key last arrived that looks like a *swallowed* Alt chord —
    /// the character the standard US macOS keyboard layout emits for
    /// `Option+<letter>` with Option-as-Meta off (`˜` for Option+n, `∑` for
    /// Option+w, …), carrying no Alt modifier at all. See
    /// `is_alt_swallow_char`.
    ///
    /// Every one of `is_alt_swallow_char`'s characters is also a
    /// directly-typed letter on some non-US macOS layout (`ç` on
    /// Portuguese/French/Turkish, `å`/`ø` on Nordic, `ß` on German, …), so a
    /// user typing their own language can trip a false positive with Alt
    /// never involved — hence a timestamp, not a latch: `show_alt_hint`
    /// only reads this within `ALT_HINT_WINDOW` of the *most recent* match,
    /// bounding a false positive to a few seconds while a user who keeps
    /// hitting real evidence keeps re-arming it.
    alt_swallow_at: Option<Instant>,
    /// Active/last text selection (copy mode).
    pub selection: Option<Selection>,
    /// P21: the live scrollback search, if one is running. Outlives the
    /// `Mode::Search` prompt on purpose — `n`/`N` keep walking the hits back
    /// in Scroll/Copy mode, and the renderer keeps highlighting them.
    pub search: Option<Search>,
    /// U20 / DESIGN.md §7: recent working directories, most recent first —
    /// the picker's second column, so launching into a project doesn't need
    /// a shell round-trip (`Alt+n`, `cd`, then the agent).
    ///
    /// **Session-only, deliberately.** Persisting it would mean a
    /// `workspace.json` schema field, i.e. a migration, for a list that is
    /// fully reconstructible: it is seeded at startup from the workspace's
    /// own pane cwds (which *are* persisted) and grows as panes are
    /// launched and `observe_panes` notices a pane `cd` somewhere new. A
    /// restart therefore comes back with every directory the workspace
    /// actually lives in — just not the ones a since-closed pane visited,
    /// which is a fair price for not touching the precious state's schema.
    recent_cwds: Vec<PathBuf>,
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
    /// The one storage for both the fleet control token and every pane's own
    /// `ROOST_TOKEN` (promotion-auth-gate) — replaces the former separate
    /// `tokens: HashMap<PaneId, String>` and `control_token: String` fields.
    /// `App` is the sole writer (mint on spawn, remove on close); sock.rs's
    /// accept-loop threads hold a `TokenReader` onto the same underlying
    /// snapshot so the door can judge a token synchronously without a
    /// main-loop round trip. A pane's own token authenticates its
    /// status/session reports — so a process in one pane can't spoof
    /// another's (they share the socket path via `ROOST_SOCK`).
    tokens: TokenTable,
    /// [F1] Open status-socket links per pane, refcounted rather than a bare
    /// bool: a pane can have more than one open reporting connection at
    /// once — a nested `claude` inside a pi pane inherits the same
    /// ROOST_PANE/ROOST_TOKEN and fires its own one-shot `roost __status`
    /// hooks (`cli.rs`'s `status_hook` has no isTTY guard, unlike
    /// `roost.ts`) alongside the pane's own long-lived pi extension
    /// connection. A bool would have the first of two connections to close
    /// clear the link while the other is still live. Sound because
    /// `sock.rs`'s `ConnGuard` dedups per *connection* (`link_panes`): each
    /// connection contributes at most one up and one down per pane over its
    /// lifetime, so this always nets to the count of connections actually
    /// open. `StatusTracker::ext_link` stays the simple bool `count > 0`
    /// collapses to.
    ext_link_counts: HashMap<PaneId, u32>,
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
    /// The question behind a pane's current NeedsInput, when its extension
    /// reported one (the ask-tool's prompt text) — consumed by the feed's
    /// transition line and the desktop notification so they can say *what*
    /// the agent is asking, not just that it asks. Inserted/cleared by
    /// `on_status` on every report (a report without a message clears it, so
    /// a stale question never outlives the ask it belongs to); pruned
    /// alongside `last_status` on the same cadence.
    needy_msgs: HashMap<PaneId, String>,
    /// F3: panes `attention_ring`'s ○ fallback
    /// must skip because focus already landed on them *during their current
    /// Waiting spell* — unlike ◆, which self-clears the moment you act on a
    /// pane (its status itself moves on), a finished-and-quiet agent's
    /// `Waiting` doesn't change just because you looked at it, so without
    /// this `Alt+a` round-robins the same panes forever and "nothing else
    /// needs you" only ever fires with exactly one waiting. Added by
    /// `set_focus` (every focus move funnels through there — Alt+a, a
    /// click, arrows, the roster all count); removed by `diff_statuses` the
    /// moment a pane's status leaves `Waiting`, so a *new* finished turn is
    /// never silently swallowed by a mark the previous turn left behind.
    /// Pruned alongside `last_status` on the same cadence.
    visited_waiting: HashSet<PaneId>,
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
    /// U19: a URL copy mode's `o` chord asked to open, waiting for the
    /// composition root to hand it to the browser — same core-does-no-I/O
    /// split as `pending_yank` above (the Alt+click path opens directly in
    /// `main.rs`, because it already runs there).
    pending_open: Option<String>,
    /// P20: the pane a mouse gesture latched onto at button-down, held
    /// until the matching release — so a drag that crosses a pane border
    /// doesn't switch target mid-gesture (which would leave the origin app
    /// stuck mid-selection, never seeing its release, and hand the
    /// neighbour orphan drag/up events for a press it never received).
    mouse_latch: Option<PaneId>,
    /// U21: the pane seam a left-drag is currently resizing, latched at
    /// button-down like `mouse_latch` and for the same reason — the pointer
    /// leaves the two-cell border almost immediately once the drag starts.
    resize_drag: Option<crate::ui::mouse::Seam>,
    /// C29: the last left click's (pane, row, col, count, at), for native
    /// selection's double/triple-click detection. Crossterm reports no
    /// click count — see `click_count`.
    last_click: Option<(PaneId, u16, u16, u8, Instant)>,
    /// S4: a copy staged from a double-click's own release
    /// — (text, fire-at), grabbed once at release time (see `PendingCopy`).
    /// Firing it immediately made every triple-click copy the word (2nd
    /// click's release) and then the line (3rd click's) a heartbeat later:
    /// two clipboard writes, two flashes, for one gesture. A double-click's
    /// release is the only one that has to wait, since it is the only one a
    /// further click can still supersede within the same window — see
    /// `release_native_selection` and `due_copy`.
    pending_copy: Option<PendingCopy>,
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
    /// config.json's parsed overrides (the key-bindings escape hatch —
    /// design doc), consulted by every Alt-chord translation from here on
    /// (`handle_mode_key` below; `main.rs`'s `handle_key` via `keymap()`).
    /// Defaults empty, i.e. today's unconfigured behavior — `new` never
    /// touches this itself; only `main.rs` installs the real one, via
    /// `set_keymap`, once it's actually loaded config.json. Every test
    /// (`mk_app` helpers included) keeps the default.
    keymap: Keymap,
}

impl<B: PaneBackend> App<B> {
    /// Restore the workspace (design doc §5): rebuild the tree and spawn
    /// every pane via its adapter — resume when a session id is known,
    /// fresh launch otherwise. A failed spawn degrades to a dead pane, it
    /// never aborts restore.
    // This was already at clippy's default threshold (7) before
    // promotion-auth-gate's `tokens: TokenTable` became the 8th — a
    // constructor wiring up every port/adapter this type is generic over is
    // exactly the shape the lint tends to overfire on, and a
    // builder/args-struct split is out of scope for this change.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ws: Workspace,
        registry: Registry,
        store: Box<dyn StateStore>,
        tx: SyncSender<AppEvent>,
        term_size: Size,
        host_pixels: (u16, u16),
        sock_path: Option<PathBuf>,
        tokens: TokenTable,
    ) -> Result<Self> {
        // A loaded workspace.json may be hand-edited, partially migrated, or
        // otherwise inconsistent — repair layout ↔ panes before spawning.
        let mut ws = ws;
        ws.validate_and_repair();
        // The fleet control token's CSPRNG refusal now happens in main.rs,
        // before `TokenTable::new` is even called (promotion-auth-gate
        // decision 9) — `spawn_listener` needs a `TokenReader` built from
        // this same table before `App::new` runs at all, so the table (and
        // its fatal-if-unavailable mint) has to exist first.
        let mut app = Self {
            focused: 0,
            alternate: None,
            marked: None,
            host_focused: true,
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
            last_shell_seen: HashMap::new(),
            last_detect: Instant::now(),
            sock_path,
            home: dirs::home_dir(),
            started: Instant::now(),
            alt_seen: false,
            alt_swallow_at: None,
            selection: None,
            search: None,
            recent_cwds: Vec::new(),
            flash: None,
            undo: Vec::new(),
            confirm_close: None,
            confirm_quit: None,
            tokens,
            ext_link_counts: HashMap::new(),
            waiters: Vec::new(),
            pending_input: HashMap::new(),
            last_status: HashMap::new(),
            needy_msgs: HashMap::new(),
            visited_waiting: HashSet::new(),
            feed: VecDeque::new(),
            float: None,
            raw: HashSet::new(),
            tab_focus: HashSet::new(),
            pending_yank: None,
            pending_open: None,
            mouse_latch: None,
            resize_drag: None,
            last_click: None,
            pending_copy: None,
            host_out: Vec::new(),
            host_title: String::new(),
            last_host_title: None,
            keymap: Keymap::default(),
        };
        app.spawn_active_tab();
        // Through `set_focus` like every other focus move, so that method
        // really is the only writer of `focused` (P10). Nothing is reported
        // here: the panes were spawned microseconds ago and none has had a
        // chance to send `?1004h` yet.
        let first = app.pane_order().first().copied().unwrap_or(0);
        app.set_focus(first);
        // U20: seed the picker's recent-cwd column from the workspace that
        // was just loaded — the directories this fleet already lives in.
        // Reverse order so the *first* pane's cwd ends up most-recent,
        // matching where focus just landed.
        let seed: Vec<PathBuf> =
            app.ws.tabs.iter().flat_map(|t| t.panes.values().map(|s| s.cwd.clone())).collect();
        for cwd in seed.into_iter().rev() {
            app.note_cwd(cwd);
        }
        Ok(app)
    }

    /// The config.json-derived keybinding overrides currently in effect —
    /// read by `main.rs`'s `handle_key` so every Alt chord it translates
    /// respects them, the same way `handle_mode_key` below does internally.
    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    /// Install the config.json-derived keybinding overrides (design doc:
    /// the key-bindings escape hatch). Called once by `main.rs`, right
    /// where it already installs the other startup-only pieces
    /// (`relayout`, the extension/socket flashes) — never by a test, which
    /// keeps `new`'s empty default: today's unconfigured behavior.
    /// C34: the chord an action is currently on, for the C10 flashes that
    /// name a key in a sentence. `None` when config.json disabled it —
    /// callers drop the clause rather than instruct a key that does
    /// nothing. The confirm flashes matter most here: "press X again to
    /// close" naming a dead X is a guard that cannot be satisfied, which is
    /// U1's hazard wearing a helpful face.
    fn chord_label(&self, action: Action) -> Option<String> {
        crate::ui::input::chord_for(&self.keymap, action)
    }

    /// The same, with the clause pre-formatted: `" · Alt+s"`-style inserts
    /// collapse to nothing when the chord is unbound.
    fn chord_clause(&self, action: Action, fmt: impl Fn(&str) -> String) -> String {
        self.chord_label(action).map(|c| fmt(&c)).unwrap_or_default()
    }

    pub fn set_keymap(&mut self, keymap: Keymap) {
        self.keymap = keymap;
    }

    /// Persist the workspace, and **say so out loud the moment it starts
    /// failing**.
    ///
    /// The failure used to reach exactly one surface: the tab bar's
    /// right-aligned `saved ✓` / `save failed ✗` indicator. C2's yield
    /// ladder drops that area *whole* when the tabs need the room, so on an
    /// ordinary 80-column terminal with five tabs roost could fail every
    /// single write — read-only state dir, full disk, wrong ownership — and
    /// show nothing at all, until the user relaunched into a fleet hours
    /// stale. The error text was discarded by `.is_ok()` on the way, so
    /// even the indicator could not say *why*.
    ///
    /// A C10 flash takes the whole hint bar and cannot be crowded out.
    /// Raised on the **transition** into failure, not on every save: a
    /// persistent failure fires on every action, and re-flashing each time
    /// would bury whatever else the bar was saying and make roost unusable
    /// exactly when the user needs to read it. The indicator remains the
    /// standing signal; this is the one that cannot be missed.
    fn save(&mut self) {
        let result = self.store.save(&self.ws);
        let ok = result.is_ok();
        if self.last_save_ok && !ok {
            // Innermost cause: the outer layers are `anyhow` context naming
            // the path, which the flash has no room for and the user
            // already knows. "Permission denied" is the actionable half.
            let why = result
                .err()
                .map(|e| e.chain().last().map(|c| c.to_string()).unwrap_or_else(|| e.to_string()))
                .unwrap_or_default();
            self.set_flash(format!("workspace NOT saved — {why}"));
        }
        self.last_save_ok = ok;
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

    /// C40: the pane waiting to be pulled, if any — the C9 hint bar's
    /// standing "pull marked pane here" pair is the only thing that makes a
    /// mark visible from the tab you marked it *from*.
    pub fn marked(&self) -> Option<PaneId> {
        self.marked
    }

    /// C20: the activity-feed ring, oldest first — read by the renderer
    /// while `Mode::Feed` is open.
    pub fn feed(&self) -> &VecDeque<FeedEntry> {
        &self.feed
    }

    /// C20: append one preformatted line to the activity feed, evicting the
    /// oldest entry once the ring is at capacity. The single entry point
    /// every hook (spawn/close/exit/status-diff/ctl) pushes through.
    fn push_feed(&mut self, text: String, needs_input: bool, pane: Option<PaneId>) {
        self.feed.push_back(FeedEntry { at: SystemTime::now(), text, needs_input, pane });
        if self.feed.len() > FEED_CAP {
            self.feed.pop_front();
        }
    }

    /// config.json diagnostics (the key-bindings escape hatch — design
    /// doc): a non-fatal problem found while loading it, one feed line
    /// each so none are lost even when the startup toast (`set_flash`) only
    /// has room to show the first. See `main.rs`'s config-loading block.
    pub fn note_config_issue(&mut self, msg: String) {
        self.push_feed(msg, false, None);
    }

    /// Record that an Alt-modified key actually arrived, so the startup hint
    /// (below) knows it doesn't need to warn.
    pub fn note_alt_seen(&mut self) {
        self.alt_seen = true;
    }

    /// F1: record a key event as possible evidence the Alt layer isn't
    /// reaching roost. Only counts when the key carries no Alt modifier AND
    /// its character is one the standard US macOS layout produces for
    /// `Option+<letter>` with Option-as-Meta off — "a key arrived" is not
    /// evidence (nearly all typing satisfies that, including the shell
    /// prompt a healthy user types first); "the specific character a
    /// swallowed Option chord produces" is.
    ///
    /// Stamps the time rather than latching a bool — every fresh match
    /// re-arms `ALT_HINT_WINDOW` (see `show_alt_hint`), so a user who keeps
    /// producing the evidence (a genuinely broken Alt layer) keeps being
    /// told, while one unlucky keystroke (a non-US layout's own letter, see
    /// `alt_swallow_at`) only costs a few seconds, not the session.
    pub fn note_key_seen(&mut self, key: crossterm::event::KeyEvent) {
        if key.modifiers.contains(crossterm::event::KeyModifiers::ALT) {
            return;
        }
        if let crossterm::event::KeyCode::Char(c) = key.code {
            if is_alt_swallow_char(c) {
                self.alt_swallow_at = Some(Instant::now());
            }
        }
    }

    /// C11/U4: should the "Alt keys aren't reaching roost" bar be up? Many
    /// terminals don't send Alt as a modifier until told to (stock
    /// Terminal.app's "Use Option as Meta Key", iTerm2's Left Option =
    /// `Esc+`) — with it off, every Alt chord roost relies on silently does
    /// nothing and there is no other signal saying why. Gating this on
    /// `TERM_PROGRAM == "Apple_Terminal"` only meant the README's own
    /// recommended terminal (iTerm2) never warned; the trigger is the
    /// evidence itself, on any terminal (see `wants_alt_hint`).
    ///
    /// The evidence is real but not unambiguous — deliberately, and not
    /// merely unnoticed (see `alt_swallow_at`'s doc): every
    /// `is_alt_swallow_char` match is also a
    /// directly-typed letter on some non-US macOS layout, so this *can*
    /// false-fire on ordinary typing in those languages. What bounds the
    /// cost is `ALT_HINT_WINDOW`, checked against the *most recent* match,
    /// not against launch — a false positive clears itself within the
    /// window instead of owning the hint row for the rest of the session,
    /// and a real Alt chord (`alt_seen`) dismisses it outright, for good,
    /// regardless of any evidence that arrives after.
    pub fn show_alt_hint(&self) -> bool {
        wants_alt_hint(self.alt_seen, self.alt_swallow_at.map(|t| t.elapsed()))
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
    /// Working-glyph spinner (`theme::spinner_frame`, C5), so every animating
    /// glyph on screen shows the same frame.
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
        // `next_pane_id` scans **live tabs**. Two other places own ids that
        // are not in a live tab, and both must be counted or the id gets
        // handed out twice.
        let base = self.ws.next_pane_id();
        let base = match &self.float {
            Some(f) => base.max(f.id + 1),
            None => base,
        };
        // The undo stack. A parked `Closed::Tab` is re-inserted **verbatim,
        // with its original pane ids** (`undo_close`), and `spawn_active_tab`
        // then builds runtimes for them — so if one of those ids has since
        // been reissued, reopening the tab silently clobbers the live pane's
        // entry in `runtimes`, `tokens`, `dead`, `pending_input` and `raw`,
        // all of which are keyed by `PaneId` across every tab. Two panes end
        // up on one PTY and one auth token.
        //
        // `Closed::Pane` needs no such guard: it stores only a `PaneSpec`
        // (which carries no id) and `restore_pane` allocates a fresh one.
        let parked = self
            .undo
            .iter()
            .filter_map(|c| match c {
                Closed::Tab { tab, .. } => tab.panes.keys().max().map(|id| id + 1),
                Closed::Pane { .. } => None,
            })
            .max()
            .unwrap_or(0);
        base.max(parked)
    }

    /// Is `id` the float's pane?
    pub fn is_float(&self, id: PaneId) -> bool {
        self.float.as_ref().is_some_and(|f| f.id == id)
    }

    /// C22/M1: the one guard every control-plane verb that resolves a
    /// specific pane id shares — the float is the human's private
    /// interactive scratch shell, never a control-plane target, for
    /// *any* verb, not just the ones an earlier pass happened to touch
    /// (close/send/read only — status/wait/fork still reached it: a
    /// same-uid pane reading `control.token` off disk *is* Fleet, and
    /// `spawn_float`'s `spawned_by: None` puts the float outside every
    /// subtree, so Fleet was never the only actor that needed refusing).
    /// `verb` slots into "cannot {verb} the scratch pane" — ctl_close's own
    /// wording — so every caller reads naturally without each inventing
    /// its own phrasing. Callers check this before authz: an actor doesn't
    /// get told "forbidden" for a pane it could never legitimately reach
    /// anyway, it gets told the truth, unconditionally.
    fn float_refusal(&self, id: PaneId, verb: &str) -> Option<String> {
        self.is_float(id).then(|| format!("cannot {verb} the scratch pane"))
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
        matches!(self.mode, Mode::Normal)
            && self.raw.contains(&self.focused)
            && !self.focused_dead()
    }

    /// C24: take (clearing) the pending keyboard-copy yank, if any — see
    /// `pending_yank`. Polled once per tick from the composition root.
    pub fn take_pending_yank(&mut self) -> Option<String> {
        self.pending_yank.take()
    }

    /// U19: take (clearing) the URL copy mode's `o` asked to open — see
    /// `pending_open`. Polled beside the yank, once per tick.
    pub fn take_pending_open(&mut self) -> Option<String> {
        self.pending_open.take()
    }

    /// P20: the pane a gesture in progress is latched to, if any.
    pub fn mouse_latch(&self) -> Option<PaneId> {
        self.mouse_latch
    }

    /// P20: latch a gesture to `id` (button-down), or clear it (release).
    pub fn set_mouse_latch(&mut self, id: Option<PaneId>) {
        self.mouse_latch = id;
    }

    /// Selection-freeze amendment (C29, design-audit D1): clear the latch
    /// AND drop whatever pane it held frozen, atomically — the freeze's
    /// lifetime is the latch's lifetime, so releasing one without the other
    /// is exactly the leak this closes. A no-op when nothing is latched, so
    /// it is safe to call from anywhere a gesture might be ending, whether
    /// or not one is actually in flight: copy mode or a modal taking over
    /// the mouse mid-drag (`handle_mouse`'s two early returns), or an
    /// orphan release whose latched pane no longer resolves in `rects`
    /// (tab switch, C31 cross-tab focus). Deliberately NOT used for the
    /// ordinary in-pane release — that one must unfreeze *after*
    /// `release_native_selection` has extracted the selection through the
    /// still-frozen view, not before (`handle_native_selection`'s own Up
    /// arm, and `handle_mouse`'s unconditional post-dispatch unfreeze,
    /// cover that ordering).
    pub fn release_mouse_gesture(&mut self) {
        if let Some(id) = self.mouse_latch.take() {
            if let Some(rt) = self.runtimes.get_mut(&id) {
                rt.unfreeze_view();
            }
        }
    }

    /// U21: a left press landed on the seam between two panes — start a
    /// resize gesture instead of the usual focus-and-forward. Returns
    /// whether one started, so the caller can stop routing this press.
    ///
    /// Deliberately does **not** move focus. Grabbing a border is an act on
    /// the layout, not a choice of which pane to type into, and stealing
    /// focus from the pane you were working in to the one you happened to
    /// drag past would be a side effect nobody asked for.
    pub fn begin_seam_drag(&mut self, col: u16, row: u16) -> bool {
        // C21: a zoomed pane fills the body and has no seam; C22's float
        // lives outside the layout tree, so its edges resize nothing.
        if self.zoomed {
            return false;
        }
        let rects = self.display_rects();
        let seam = crate::ui::mouse::seam_at(&rects, col, row);
        let started = seam.is_some();
        self.resize_drag = seam;
        started
    }

    /// U21: the pointer moved during a seam drag — move the border to it.
    ///
    /// A **closed loop on the drawn geometry**, not an accumulated offset:
    /// every event measures where the border actually is now and asks for
    /// the ratio change that would put it under the cursor. `resize_pane`
    /// works in ratios of a split whose extent this layer cannot see (the
    /// pair may be nested two splits deep), so any open-loop conversion
    /// would drift — under-move on nested splits, over-move on wide ones,
    /// and never recover. Measuring each time means an imperfect estimate
    /// is corrected by the next event a cell later, which is what makes the
    /// border track the pointer instead of lagging it.
    pub fn drag_seam(&mut self, col: u16, row: u16) {
        let Some(seam) = self.resize_drag else { return };
        let rects = self.display_rects();
        let (Some(a), Some(b)) = (
            rects.iter().find(|p| p.id == seam.a).map(|p| p.rect),
            rects.iter().find(|p| p.id == seam.b).map(|p| p.rect),
        ) else {
            self.resize_drag = None; // a pane went away mid-drag
            return;
        };
        let (border, want, extent, axis) = if seam.vertical {
            (b.x as i32, col as i32, (a.width + b.width).max(1), SplitDir::Vertical)
        } else {
            (b.y as i32, row as i32, (a.height + b.height).max(1), SplitDir::Horizontal)
        };
        let moved = want - border;
        if moved == 0 {
            return;
        }
        let delta = moved as f32 / extent as f32;
        let target = seam.a;
        layout::resize_pane(&mut self.ws.active_tab_mut().layout, target, axis, delta);
        self.relayout();
    }

    /// U21: the drag ended. Saving here rather than on every drag event —
    /// a resize writes the workspace, and a drag emits one event per cell.
    pub fn end_seam_drag(&mut self) {
        if self.resize_drag.take().is_some() {
            self.save();
        }
    }

    /// U21: is a seam drag in progress? The mouse path checks this to keep
    /// routing drag/release to the resize rather than to a pane.
    pub fn seam_dragging(&self) -> bool {
        self.resize_drag.is_some()
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

        // Validate a stored session id via SessionResolver (design doc
        // §6.1): only launch fresh + clear it when the session is
        // *definitively* gone. All adapter queries happen here, before we
        // borrow self mut.
        let resolution =
            session_resolver::resolve(adapter.as_ref(), &spec.cwd, spec.session.as_deref());
        let mut cmd = match &resolution.session {
            Some(s) => adapter.resume(&spec.cwd, s),
            None => adapter.launch(&spec.cwd),
        };
        if let Some(sock) = &self.sock_path {
            cmd.env.push(("ROOST_SOCK".into(), sock.to_string_lossy().into_owned()));
            // Fresh per-spawn token: the pane authenticates its socket messages
            // with it, and no other pane knows it. Reissued on every (re)spawn.
            let token = crate::core::control::gen_token();
            cmd.env.push(("ROOST_TOKEN".into(), token.clone()));
            self.tokens.set_pane_token(id, token);
            // [F1 residual] A respawn's old connections (if any survive the
            // kill) can only ever carry the OLD token from here on, so
            // their eventual link-down is unauthenticated by construction
            // (`socket_authorized` rejects it) and the refcount would never
            // see the matching decrement — stranding `ext_link_counts` at
            // whatever it was mid-respawn, permanently, for this pane.
            // Token-mint already means "fresh world for this pane" for
            // `tokens` itself; zero the link count here too rather than
            // trust connections that can no longer prove themselves.
            self.ext_link_counts.remove(&id);
        }
        // adapter / registry borrow ends here.

        if resolution.stale {
            // Persist the correction so the dead id isn't retried next launch.
            if let Some(s) = self.find_spec_mut(id) {
                s.session = None;
            }
            self.save();
        }

        let (rows, cols) = inner_dims(rect);
        let pixels = self.pane_pixels(rows, cols);
        match B::spawn(id, &cmd, rows, cols, pixels, self.tx.clone()) {
            Ok(mut rt) => {
                // D5: the title-status channel is live only while an agent
                // runs here — on at birth for agent panes, and kept current
                // by `observe_panes` for shells that launch one by hand.
                rt.set_title_signal(spec.adapter != "shell");
                self.runtimes.insert(id, rt);
                self.dead.remove(&id);
                // Owe this pane a session id? Watch for one (socket reports
                // it exactly; the filesystem scan in tick() is the fallback).
                if resolution.wants_detect {
                    self.pending_detect.insert(id, SystemTime::now());
                }
                // C20: spawn owns the pane's "birth" line — diff_statuses
                // deliberately stays silent on a pane's first observation.
                // U2: led by the pane id; the `({adapter})` suffix only for
                // titled panes (an untitled display name already ends in the
                // adapter/cwd tag — C4's no-dup rule).
                let label = format!("{id} {}", display_name_live(spec, None));
                let line = match &spec.title {
                    Some(_) => format!("spawned {label} ({})", spec.adapter),
                    None => format!("spawned {label}"),
                };
                self.push_feed(line, false, Some(id));
            }
            Err(e) => {
                // P0-3: `e.to_string()` (anyhow's default Display) shows only
                // the outermost `.context()` pty.rs adds ("spawning pi") —
                // the real cause (ENOENT/EACCES) is computed, carried, and
                // then dropped right here, one call before the dead-pane bar
                // would show it. `dead_reason` walks the whole chain instead.
                let reason = dead_reason(&e);
                // P0-2: same reason, on the activity feed — the spawn success
                // arm above already logs a "spawned" line; a failure deserves
                // the symmetric "spawn failed" one, not silence.
                self.push_feed(format!("spawn failed: {reason}"), false, Some(id));
                self.dead.insert(id, reason);
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
        pending.sort_by_key(|(_, since)| std::cmp::Reverse(*since));
        // Drop anything past the give-up horizon before scanning for it.
        // Checked against the pane's own `since` rather than a separate
        // clock so the window means the same thing here as it does in the
        // scan: how long we have been looking for *this* pane's file.
        let now = SystemTime::now();
        self.pending_detect.retain(|_, since| {
            now.duration_since(*since).map(|age| age < DETECT_GIVE_UP).unwrap_or(true)
        });
        pending.retain(|(id, _)| self.pending_detect.contains_key(id));
        for (id, since) in pending.clone() {
            let Some((spec, adapter)) = self
                .find_spec(id)
                .and_then(|s| self.registry.get(s.adapter.as_str()).map(|a| (s.clone(), a)))
            else {
                self.pending_detect.remove(&id);
                continue;
            };
            // Two panes in *different projects* whose adapter gives them the
            // same session root cannot be told apart by this scan at all:
            // the root carries no cwd signal (codex buckets rollouts by
            // date, `~/.codex/sessions` for every project), so the only
            // thing separating the candidates is mtime order, which says
            // nothing about whose they are. Decline rather than guess.
            //
            // **A wrong session is far worse than no session.** Losing a
            // resume pointer costs the user a `--continue`; attaching a
            // pane to another project's conversation corrupts work in it.
            // Declining leaves the pane pending, so the exact channel (the
            // agent-side extension) can still report it, and the scan
            // retries the moment the ambiguity clears.
            //
            // Same-cwd concurrency is *not* ambiguous in this sense and is
            // deliberately still allowed: both panes really are in that
            // project, and the newest-first ordering above mirrors file
            // creation order so each takes its own.
            let root = adapter.session_root(&spec.cwd);
            let ambiguous = root.is_some()
                && pending.iter().any(|(other, _)| {
                    *other != id
                        && self.find_spec(*other).is_some_and(|o| {
                            o.adapter == spec.adapter
                                && o.cwd != spec.cwd
                                && self
                                    .registry
                                    .get(o.adapter.as_str())
                                    .and_then(|a| a.session_root(&o.cwd))
                                    == root
                        })
                });
            if ambiguous {
                continue;
            }
            // Session ids already owned by other panes — never re-assign one
            // (concurrent same-cwd launches otherwise cross-wire onto it).
            let taken = session_resolver::claimed_sessions(&self.ws);
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
    ///
    /// Reads `display_status`, not the raw runtime status — same reasoning
    /// as D1's `tab_summary`: the feed is chrome the user reads at a
    /// glance, and every other surface (badge, roster, collapsed rows, tab
    /// bar) already reads a quiet shell's `Waiting` as `Idle`. Without this
    /// the feed would log `1 shell: working → your turn` on every command
    /// any shell pane ran, while the rest of the chrome said `idle` in the
    /// same frame.
    fn diff_statuses(&mut self) {
        let ids: Vec<PaneId> = self.runtimes.keys().copied().collect();
        let current: Vec<(PaneId, AgentStatus)> = ids
            .into_iter()
            .map(|id| (id, self.display_status(id).unwrap_or(AgentStatus::Exited)))
            .collect();
        for (id, status) in current {
            let prev = self.last_status.insert(id, status);
            match prev {
                None => {} // first observation: spawn owns birth, no line
                Some(old) if old == status || status == AgentStatus::Exited => {}
                Some(old) => {
                    // F3: leaving `Waiting` starts a fresh attention cycle —
                    // any "you already looked at this" mark belongs to the
                    // turn that just ended, not the one that just started,
                    // so it must not silently swallow a real new ○.
                    if old == AgentStatus::Waiting {
                        self.visited_waiting.remove(&id);
                    }
                    // U2: `{id} {display_name}`, so four identical shells'
                    // transitions stop being indistinguishable in the feed.
                    let mut text = format!(
                        "{}: {} → {}",
                        self.feed_label(id),
                        state_word(old),
                        state_word(status)
                    );
                    // A ◆ landing with a known question says what it is —
                    // "needs input — pick a database" reads, "needs input"
                    // sends you to the pane to find out.
                    if status == AgentStatus::NeedsInput {
                        if let Some(q) = self.needy_msgs.get(&id) {
                            text.push_str(" — ");
                            text.push_str(q);
                        }
                    }
                    self.push_feed(text, status == AgentStatus::NeedsInput, Some(id));
                }
            }
        }
        // Backstop for panes that lost their runtime without going through
        // `close_pane_id` (a tab removed with its panes, a workspace
        // reload). The close path prunes all three itself — it has to, since
        // ids ARE recycled (C23) and this pass only runs every
        // `DETECT_INTERVAL`.
        self.last_status.retain(|id, _| self.runtimes.contains_key(id));
        self.needy_msgs.retain(|id, _| self.runtimes.contains_key(id));
        self.visited_waiting.retain(|id| self.runtimes.contains_key(id));
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
        // Panes observed running no agent this tick — `last_shell_seen`
        // (collected here and written after the loop, since `find_spec_mut`
        // holds `self`).
        let mut still_shell: Vec<PaneId> = Vec::new();
        // D5: adapter flips collected here and pushed into the runtimes'
        // title-signal gate after the loop (`find_spec_mut` holds `self`).
        let mut retitle: Vec<(PaneId, bool)> = Vec::new();
        // U20: directories a pane has *moved into* since we last looked —
        // the user typing `cd ~/project` is the clearest possible statement
        // that it is a directory they are working in, which is exactly what
        // the picker's recent-cwd column offers back. Collected here and
        // recorded after the loop, since `find_spec_mut` holds `self`.
        let mut visited: Vec<PathBuf> = Vec::new();
        for (id, o) in observations {
            let Some(spec) = self.find_spec_mut(id) else { continue };
            if let Some(cwd) = o.cwd {
                if spec.cwd != cwd {
                    spec.cwd = cwd.clone();
                    visited.push(cwd);
                    dirty = true;
                }
            }
            // Reflect the running agent: promote a shell that's now running pi
            // to the pi adapter; demote back to shell when the agent exits.
            let want = o.agent.unwrap_or_else(|| "shell".to_string());
            // Not gated on a *transition*: a pane sitting at a shell prompt
            // for an hour must keep moving this bound forward, or the
            // window it eventually gets on promotion would reach back to
            // whenever it last changed state.
            if want == "shell" {
                still_shell.push(id);
            }
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
                retitle.push((id, !demoting));
                dirty = true;
            }
        }
        // D5: promote/demote flips the title-status gate with the adapter —
        // an exited agent's leftover `✳`/spinner title must stop counting
        // the moment the pane is a plain shell again (demotion clears the
        // stored signal too, so a relaunch starts clean).
        for (id, enabled) in retitle {
            if let Some(rt) = self.runtimes.get_mut(&id) {
                rt.set_title_signal(enabled);
            }
        }
        let now = SystemTime::now();
        for id in still_shell {
            self.last_shell_seen.insert(id, now);
        }
        // A newly-recognized agent needs its already-created session file
        // located — it was written moments before roost noticed, so `now()`
        // would miss it. The bound is **the last tick this pane was still a
        // shell**, minus `PROMOTION_GRACE` for observation lag.
        //
        // This used to be `SystemTime::UNIX_EPOCH`, on the reasoning that a
        // wide window "plus the taken-set finds it without cross-wiring".
        // The taken-set does not carry that weight: `claimed_sessions` only
        // knows ids stored on *live* panes, so a conversation from a closed
        // pane or an earlier run is unclaimed and therefore eligible. With
        // no lower bound the scan took the newest such file whenever it was
        // written, `set_session` committed it, and the pane was dropped from
        // `pending_detect` — so the mistake was permanent, and the next
        // relaunch resumed a conversation from days ago. Reported as "it
        // loads the wrong session"; pinned by
        // `a_promoted_pane_never_claims_a_session_older_than_its_shell`.
        for id in promoted {
            let floor = self
                .last_shell_seen
                .get(&id)
                .copied()
                .unwrap_or(now)
                .checked_sub(PROMOTION_GRACE)
                .unwrap_or(SystemTime::UNIX_EPOCH);
            // `max`, not `or_insert`: the window may only ever **tighten**.
            // A pane that promotes, finds nothing (so stays pending),
            // demotes when the agent exits, then promotes again hours later
            // would otherwise keep its first floor — by then old enough to
            // reach conversations from earlier in the same session.
            self.pending_detect.entry(id).and_modify(|f| *f = (*f).max(floor)).or_insert(floor);
        }
        for cwd in visited {
            self.note_cwd(cwd);
        }
        if dirty {
            self.save();
        }
    }

    /// Is anything on screen moving with no event behind it?
    ///
    /// The event loop repaints on demand and, failing that, on a budget
    /// (`main.rs`'s `should_repaint`). This picks which budget: the C5
    /// spinner is the only thing roost draws that moves faster than half a
    /// second unprompted, and it moves for exactly one reason — a pane whose
    /// **displayed** status is Working. Everything else time-driven changes
    /// at a boundary a few hundred milliseconds either side of which nobody
    /// can tell: a flash expiring, the F1 hint window closing, a badge's age
    /// tag ticking over a minute.
    ///
    /// `display_status`, not the raw runtime status, for the same reason
    /// `diff_statuses` reads it: the chrome that draws the spinner reads it
    /// too, so this cannot say "still" about a frame that is about to move.
    ///
    /// Workspace-wide (`runtimes`), not the active tab: the tab strip's
    /// per-tab summary and C27's roster both draw spinners for panes that
    /// are not on screen.
    ///
    /// Being wrong here is deliberately cheap. `should_repaint`'s outer
    /// budget fires whatever this answers, so a missed animation is a
    /// spinner that steps twice a second for as long as the miss lasts —
    /// never a screen that stops updating.
    pub fn animating(&self) -> bool {
        self.runtimes.keys().any(|id| self.display_status(*id) == Some(AgentStatus::Working))
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

    /// C22: learns the float — badges/rename/respawn by id all route
    /// through this, so they work on the scratch pane exactly like any
    /// other, with zero extra code at those call sites. `send`/`read`/
    /// `close` resolve it here too (so "no such pane" is still right for
    /// them), but each then adds its own is-float refusal (M1) — the float
    /// is the human's private scratch shell, never a control-plane target.
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
    /// Returns the summary **and how many of the tab's panes are in that
    /// summarized state** — the number the tab bar renders beside the glyph
    /// (`◆3`), so a tab holding three blocked agents stops reading exactly
    /// like a tab holding one.
    ///
    /// The count is of the *summarized* state only, never a pane total: the
    /// glyph says what the tab is reporting, and the count says how much of it
    /// there is. `Quiet` counts 0 — it reports nothing by construction. The
    /// ranking is U13's order, verbatim, which is why the count rides along
    /// here rather than in a second function that could disagree with it
    /// about which state won.
    ///
    /// Reads `display_status`, not the raw runtime status: the badge and the
    /// roster already do, and C5 is one table for every chrome surface that
    /// shows status, tab bar included — a mismatch here would draw `○` on
    /// the tab bar and `·` on the badge in the same frame. `None` (not
    /// spawned) still buckets as `unknown`, `Some(Exited)` still as
    /// `exited` — `display_status` only ever touches the `Waiting` rung, so
    /// both map onto today's rungs 1:1.
    pub fn tab_summary(&self, tab_index: usize) -> (TabSummary, usize) {
        let Some(tab) = self.ws.tabs.get(tab_index) else { return (TabSummary::Quiet, 0) };
        let (mut unknown, mut needs, mut working, mut waiting, mut exited) = (0, 0, 0, 0, 0);
        for id in tab.panes.keys() {
            match self.display_status(*id) {
                Some(AgentStatus::NeedsInput) => needs += 1,
                Some(AgentStatus::Working) => working += 1,
                Some(AgentStatus::Waiting) => waiting += 1,
                Some(AgentStatus::Exited) => exited += 1, // U13
                Some(AgentStatus::Idle) => {}
                None => unknown += 1, // not spawned yet
            }
        }
        if needs > 0 {
            (TabSummary::NeedsInput, needs) // a real, actionable signal wins outright
        } else if unknown > 0 {
            (TabSummary::Unknown, unknown) // honest: we haven't run these panes
        } else if working > 0 {
            (TabSummary::Working, working)
        } else if waiting > 0 {
            (TabSummary::Waiting, waiting)
        } else if exited > 0 {
            // U13: ranked below Waiting (a live agent outranks a corpse) and
            // above Quiet (a dead pane is news; an idle one isn't).
            (TabSummary::Exited, exited)
        } else {
            (TabSummary::Quiet, 0)
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
    /// Delegates to the snapshot so this and sock.rs's door check (which
    /// consults the exact same `TokenSnapshot::pane_authorized`) can never
    /// drift apart.
    pub fn socket_authorized(&self, id: PaneId, token: &str) -> bool {
        self.tokens.load().pane_authorized(id, token)
    }

    // -- control interface -------------------------------------------------

    /// The fleet control token (written to `<state>/control.token` by
    /// startup). Owned `String` (not `&str`): it now reads through a
    /// snapshot `Arc`, not a field living directly on `self`.
    pub fn control_token(&self) -> String {
        self.tokens.load().control().to_string()
    }

    /// Resolve a token to the caller it represents: the fleet control token, or
    /// a pane acting via its own `ROOST_TOKEN`. Fails closed on empty/unknown.
    /// Delegates to the snapshot — the same `TokenSnapshot::resolve_actor`
    /// sock.rs's door uses for a control request's admission check.
    fn resolve_actor(&self, token: &str) -> Option<Actor> {
        self.tokens.load().resolve_actor(token)
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
            // C36: the human at the keyboard reaches every pane, same as
            // the fleet token. They are separate variants so the audit can
            // tell them apart, not because their reach differs.
            Actor::Fleet | Actor::Local => true,
            Actor::Pane(a) => self.in_subtree(a, target),
        }
    }

    /// C35: does this id name a pane that exists right now — tiled or the
    /// float? Used at both ends of the alternate pointer: recording (so
    /// `focused`'s `0` initializer never becomes a phantom "previous pane"
    /// on the first focus move) and using it (a pane can close while you are
    /// away). One predicate rather than two, so the two ends cannot disagree
    /// about what "exists" means.
    fn pane_exists(&self, id: PaneId) -> bool {
        self.tab_of(id).is_some() || self.float.as_ref().is_some_and(|f| f.id == id)
    }

    fn tab_of(&self, id: PaneId) -> Option<usize> {
        self.ws.tabs.iter().position(|t| t.panes.contains_key(&id))
    }

    /// What any surface — chrome or control plane — should say about pane
    /// `id`'s state. `Some` once the pane has a runtime (or is known dead),
    /// `None` while it has none: a pane in a tab this session has never
    /// visited, which the lazy spawn has not started yet.
    ///
    /// One source for that three-rung distinction, because two surfaces read
    /// it in the same frame — without it, C27's roster could call a
    /// runtime-less pane `Exited` while `tab_summary` calls the same tab
    /// `Unknown`, showing a freshly restored workspace's entire fleet as
    /// corpses under a tab bar of quiet dots.
    pub fn row_status(&self, id: PaneId) -> Option<AgentStatus> {
        if let Some(rt) = self.runtimes.get(&id) {
            return Some(rt.status());
        }
        if self.dead.contains_key(&id) || self.find_spec(id).is_none() {
            // Spawn failed, or the pane is closed/never existed — either way
            // nothing further will happen to it.
            return Some(AgentStatus::Exited);
        }
        None // a background pane not spawned yet (lazy)
    }

    /// [ux P2-10] `row_status`'s raw answer, with one presentation-layer
    /// adjustment: a plain `shell` pane has no turn to hand back (`is_shell`'s
    /// own words — "the one roost ships that has no turns to be mid-way
    /// through"), so a heuristic `Waiting` reads as `Idle` here. Left as
    /// `Waiting`, every shell sitting untouched at its prompt for more than
    /// `ACTIVE_WINDOW` reads `○` "your turn" forever — wrong for a pane
    /// nobody asked anything of, and it buries a real agent's ○ in a fleet of
    /// quiet shells. Every other status, and every non-shell pane, passes
    /// through unchanged.
    ///
    /// This is the one place that downgrade happens; the corner badge,
    /// collapsed rows, the roster (C27) and the C19 ring all resolve a
    /// pane's status through here rather than `row_status` directly, so the
    /// three can't drift from each other. The control plane deliberately
    /// does *not* route through here — `row_status`/`status_str`/
    /// `wait --until` keep answering with the raw signal, because a script's
    /// status contract must not shift out from under it over a chrome-only
    /// judgment call.
    pub fn display_status(&self, id: PaneId) -> Option<AgentStatus> {
        match self.row_status(id) {
            Some(AgentStatus::Waiting) if self.is_shell(id) => Some(AgentStatus::Idle),
            other => other,
        }
    }

    fn status_str(&self, id: PaneId) -> &'static str {
        match self.row_status(id) {
            Some(AgentStatus::Working) => "working",
            Some(AgentStatus::NeedsInput) => "needs_input",
            Some(AgentStatus::Waiting) => "waiting",
            Some(AgentStatus::Idle) => "idle",
            Some(AgentStatus::Exited) => "exited",
            None => "unknown",
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
            None => Reply::err(UNAUTHORIZED_MSG),
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
                let _ = reply.send(Reply::err(UNAUTHORIZED_MSG));
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
            Some(Actor::Local) => "local".to_string(),
            Some(Actor::Pane(id)) => format!("pane:{id}"),
            None => "?".to_string(),
        };
        let outcome = if ok { "ok" } else { "err" };
        self.push_feed(format!("ctl {principal}: {} → {outcome}", sanitize(summary)), false, None);

        let Some(dir) = self.sock_path.as_ref().and_then(|p| p.parent()) else { return };
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let line =
            format!("{ts} {principal} {} -> {outcome} {}\n", sanitize(summary), sanitize(detail));
        use std::io::Write;
        let path = dir.join("control.log");
        rotate_audit_log(&path, AUDIT_LOG_MAX);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
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
            // M1: `wait <float> --until idle` would be a typing oracle on
            // the human's private scratch shell — same refusal as every
            // other verb that resolves a specific pane id.
            if let Some(msg) = self.float_refusal(p, "wait on") {
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
        // A wait over more panes than any fleet has is not a wait, it is a
        // load generator. `poll_waiters` runs on the **UI thread** every
        // event-loop iteration and does `is_float` + `find_spec` +
        // `may_target` + `pane_matches` for every id of every waiter; the
        // list was unbounded and un-deduplicated, and ~32 000 ids fit in one
        // 64 KiB control line. Sixteen such waits measured a 4.5x hit to
        // control round-trip latency (21ms -> 94ms) sustained for the wait's
        // whole timeout — the frame budget blown every frame, from sixteen
        // requests. Bounded well above any real fleet.
        if panes.len() > MAX_WAIT_PANES {
            let msg = "too many panes in one wait";
            let _ = reply.send(Reply::err(msg));
            return Err(msg.into());
        }
        // Cap concurrently parked waiters well below the socket's global
        // connection limit: rejecting here closes the connection right away,
        // freeing its slot back to the pool instead of holding it for the
        // full timeout (see MAX_WAITS).
        //
        // **Per principal as well as globally.** A parked waiter is only
        // ever freed by firing or timing out — nothing notices that the
        // client hung up — so sixteen fire-and-forget waits with long
        // timeouts held every global slot for up to 24 hours while holding
        // no connection and no process, denying the verb to the entire
        // fleet. A pane can do that with its own `$ROOST_TOKEN`, which puts
        // it squarely outside its authority. With a per-actor share the
        // worst a pane can do is starve itself.
        let mine = self.waiters.iter().filter(|w| w.actor == actor).count();
        if self.waiters.len() >= MAX_WAITS || mine >= MAX_WAITS_PER_ACTOR {
            let msg = "too many concurrent waits";
            let _ = reply.send(Reply::err(msg));
            return Err(msg.into());
        }
        // Default 5 min, capped at 24 h, so a parked reply can't live forever.
        // Both numbers live in `core::control` — the client sizes its own
        // read deadline from them (`cli::reply_timeout`).
        let ms = timeout_ms
            .unwrap_or(crate::core::control::WAIT_DEFAULT_TIMEOUT_MS)
            .min(crate::core::control::WAIT_MAX_TIMEOUT_MS);
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
            //
            // M1: the same recycling can hand `p` to the float instead of
            // to an unrelated real pane — and Fleet's `may_target` is
            // unconditionally `true`, so the check above alone would not
            // catch it. An explicit is_float re-check is the only thing
            // that closes that gap for a wait registered before the float
            // existed at this id.
            let hit = w.panes.iter().copied().find(|&p| {
                if self.is_float(p) {
                    return false;
                }
                if self.find_spec(p).is_some() && !self.may_target(w.actor, p) {
                    return false;
                }
                self.pane_matches(p, w.until)
            });
            let timed_out = now >= w.deadline;
            if let Some(id) = hit {
                let status = self.status_str(id);
                let w = self.waiters.remove(i);
                let _ =
                    w.reply.send(Reply::ok(serde_json::json!({ "pane": id, "status": status })));
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
            // P0-2: null unless the pane is dead *and* roost knows why —
            // `list`/`status` are what a `spawn` caller polls afterward, so
            // this is the same reason `spawn_reply` folds into the spawn
            // reply itself when it's already known by then.
            "error": self.dead.get(&id),
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
                // M1: `None` (the fleet-wide list) already excludes the
                // float structurally — it lives outside `ws.tabs`, which
                // `ctl_list` is the only thing that iterates — but a
                // specific id resolves it via `find_spec` same as any
                // other verb, so it needs the same explicit refusal.
                if let Some(msg) = self.float_refusal(p, "get the status of") {
                    return Reply::err(msg);
                }
                if !self.may_target(actor, p) {
                    return Reply::err("forbidden: pane not in your subtree");
                }
                // P0-2: `status` is what an orchestrator polls after `spawn`
                // — a dead pane's reason (`self.dead`) has to reach it here,
                // not just the bare "exited" `status_str` already gave, or a
                // caller that only checks `status` still retries forever.
                Reply::ok(serde_json::json!({
                    "pane": p,
                    "status": self.status_str(p),
                    "error": self.dead.get(&p),
                }))
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
        if !self.registry.contains_key(adapter) {
            return Reply::err(format!("unknown adapter: {adapter}"));
        }
        // QA-2: an explicit --cwd is the one path a caller can name a
        // directory roost has never seen — validate it before anything is
        // created. `None` (inherit the split target's cwd) needs no check:
        // that path was already validated when ITS pane spawned.
        let cwd = match cwd {
            Some(c) => {
                let path = PathBuf::from(c);
                if let Err(e) = validate_spawn_cwd(&path) {
                    return Reply::err(e);
                }
                Some(path)
            }
            None => None,
        };
        self.ctl_spawn_child(actor, adapter, cwd, initial_input, "spawn")
    }

    /// Shared tail of `ctl_spawn`/`ctl_fork`: resolve `actor`'s owner id,
    /// split off a new `adapter` pane, and reply. `verb` names the action in
    /// the refusal message ("spawn"/"fork").
    ///
    /// `spawn_child` (shared with the interactive Alt+n path) splits off
    /// `self.focused` and moves focus to the new pane — fine for a human
    /// keystroke, but the control API must never steal the human's focus or
    /// jump their active tab out from under them (DESIGN-control §5.2).
    /// Save + restore around the call; the new pane is still created,
    /// spawned, and its id returned either way.
    fn ctl_spawn_child(
        &mut self,
        actor: Actor,
        adapter: &str,
        cwd: Option<PathBuf>,
        initial_input: Option<String>,
        verb: &str,
    ) -> Reply {
        let owner = match actor {
            Actor::Fleet | Actor::Local => None,
            Actor::Pane(a) => Some(a),
        };
        // A control spawn must leave the human's view exactly as it found
        // it — including the float. `spawn_child` focuses the pane it
        // creates, and C22 rule 1 (enforced in `set_focus`) takes the float
        // down when focus leaves it, so restoring `focused` alone put focus
        // back on a float that was now hidden: a state the model does not
        // have, and one `cycle_layout` then planted into the tab's layout
        // tree as a pane with no spec. Save and restore both halves.
        let (focused, active_tab) = (self.focused, self.ws.active_tab);
        let float_shown = self.float.as_ref().is_some_and(|f| f.shown);
        let id = self.spawn_child(adapter, cwd, owner);
        if let Some(f) = &mut self.float {
            f.shown = float_shown;
        }
        self.set_focus(focused);
        self.ws.active_tab = active_tab;
        let Some(id) = id else {
            // C38: the same refusal the TUI flashes, phrased for a caller
            // rather than a reader — an API client cannot widen a terminal,
            // so the actionable advice here is to stack rather than the
            // pane's shortfall. The divergence is deliberate and contracted;
            // what both surfaces must share is that the refusal is *said*.
            let hint = self
                .chord_clause(Action::ToggleStack, |c| format!("; stack a pane with {c} first"));
            return Reply::err(format!("{verb} refused: not enough room to split{hint}"));
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
        self.spawn_reply(id)
    }

    /// P0-2: the `{"pane": id}` reply `ctl_spawn`/`ctl_fork` both end on,
    /// folding in whatever `spawn_child` → `spawn_pane` already learned
    /// *synchronously* (`self.dead`) — the ENOENT/EACCES class of failure
    /// finding #1 is about, fully known by the time this reply goes out, at
    /// no extra latency. A pane that starts fine and dies moments later is a
    /// different, later event with no reason string to attach here yet; that
    /// one reaches the caller through `status`/`list` instead (`pane_json`,
    /// `ctl_status`), never by delaying THIS reply to wait and see — a
    /// spawn call must answer promptly, not hang on a race it can't win.
    fn spawn_reply(&self, id: PaneId) -> Reply {
        Reply::ok(serde_json::json!({ "pane": id, "error": self.dead.get(&id) }))
    }

    fn ctl_fork(&mut self, actor: Actor, pane: Option<PaneId>) -> Reply {
        let target = match (pane, actor) {
            (Some(p), _) => p,
            (None, Actor::Pane(a)) => a,
            (None, Actor::Fleet | Actor::Local) => {
                return Reply::err("fork requires a pane id for a fleet caller")
            }
        };
        // M1: forking the float would clone its spec into a brand-new real
        // pane (and surface its cwd in `list`) — refused before authz, same
        // as every other verb that resolves a specific pane id.
        if let Some(msg) = self.float_refusal(target, "fork") {
            return Reply::err(msg);
        }
        if !self.may_target(actor, target) {
            return Reply::err("forbidden: pane not in your subtree");
        }
        let Some(spec) = self.find_spec(target).cloned() else {
            return Reply::err("no such pane");
        };
        // Same adapter + cwd. Session-branching (a true fork of the agent's
        // conversation) lands with the bidirectional pi extension; for now this
        // opens a fresh sibling in the same context.
        self.ctl_spawn_child(actor, &spec.adapter, Some(spec.cwd), None, "fork")
    }

    fn ctl_send(&mut self, actor: Actor, pane: PaneId, text: &str, submit: bool) -> Reply {
        if self.find_spec(pane).is_none() {
            return Reply::err("no such pane");
        }
        if let Some(msg) = self.float_refusal(pane, "send to") {
            return Reply::err(msg);
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
    /// C36: every pane a broadcast by `actor` would reach — alive, not the
    /// float, within the actor's authority — sorted for determinism.
    ///
    /// Factored out of `ctl_broadcast` so the TUI composer can *count* the
    /// targets before sending (the visible blast radius that is C36's whole
    /// safety affordance) using the identical predicate the send then uses.
    /// A count that disagreed with the send would be worse than no count:
    /// it would be a guard that lies.
    ///
    /// `status` narrows it further when the composer's filter is set — the
    /// same `Option<AgentStatus>` idiom C27's roster cycles with `Tab`.
    pub fn broadcast_targets(&self, actor: Actor, status: Option<AgentStatus>) -> Vec<PaneId> {
        let mut targets: Vec<PaneId> = self
            .runtimes
            .iter()
            .filter(|(&id, rt)| {
                rt.status() != AgentStatus::Exited
                    && !self.is_float(id)
                    && self.may_target(actor, id)
                    && status.is_none_or(|want| self.display_status(id) == Some(want))
            })
            .map(|(&id, _)| id)
            .collect();
        targets.sort_unstable();
        targets
    }

    /// C36: write `bytes` to each of `targets`, returning the ones that took
    /// it. The one place a broadcast actually delivers, shared by the
    /// control verb and the TUI composer.
    fn deliver_broadcast(&mut self, targets: &[PaneId], bytes: &[u8]) -> Vec<PaneId> {
        targets
            .iter()
            .copied()
            .filter(|id| self.runtimes.get_mut(id).is_some_and(|rt| rt.write_input(bytes)))
            .collect()
    }

    fn ctl_broadcast(&mut self, actor: Actor, text: &str, submit: bool) -> Reply {
        let mut bytes = text.as_bytes().to_vec();
        if submit {
            bytes.push(b'\r');
        }
        let targets = self.broadcast_targets(actor, None);
        let sent = self.deliver_broadcast(&targets, &bytes);
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
        if let Some(msg) = self.float_refusal(pane, "read") {
            return Reply::err(msg);
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
            // Selection-freeze amendment (SPEC-parity P1 gap): `read_screen_text`,
            // not `grab_text` — a control client is a different consumer
            // than the human dragging a mouse over the TUI, and must not
            // see that human's frozen frame. Still honors the P1
            // sync-output veneer (a torn mid-redraw frame is wrong for
            // both consumers alike); it bypasses only the gesture freeze.
            ReadMode::Screen => rt.read_screen_text((0, 0), (u16::MAX, u16::MAX)),
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
        if let Some(msg) = self.float_refusal(pane, "close") {
            return Reply::err(msg);
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
        //
        // C22: the pane that is zoomed is not always the focused one. While
        // the float is shown focus belongs to it — never to a tiled pane —
        // so asking `id == self.focused` could not fire, and closing the
        // pane actually being shown full-screen left `zoomed` set pointing
        // at a pane that no longer exists. `display_rects` resolves the real
        // target the same way; this must agree with it.
        let zoom_target = match &self.float {
            Some(f) if f.shown => f.prev_focus,
            _ => self.focused,
        };
        if self.zoomed && id == zoom_target {
            self.exit_zoom();
        }
        let spec = self.ws.tabs[ti].panes.get(&id).cloned();
        let tab_snapshot = self.ws.tabs[ti].clone();
        if let Some(mut rt) = self.runtimes.remove(&id) {
            rt.kill();
        }
        self.tokens.remove_pane_token(id);
        // [F1] Same bound-map hygiene as `tokens` — pane ids are recycled
        // (see the C23 comment a few lines below), so an unremoved entry
        // isn't just inert growth: it could be misread as a fresh pane's
        // own refcount the moment this id is reused.
        self.ext_link_counts.remove(&id);
        // Drop any spawn-error record for this pane too; otherwise a pane that
        // failed to spawn and is then closed leaves a stale `dead` entry that
        // a later pane reusing this id would inherit.
        self.dead.remove(&id);
        // A pane closed before its first output never got to flush its
        // buffered initial_input (see on_pty_output) — drop it too, rather
        // than leaking an entry keyed on an id that's gone for good.
        self.pending_input.remove(&id);
        // C23: pane ids ARE recycled (`next_pane_id` is max(current ids)+1,
        // not a monotonic counter — see U11's comment below, and
        // `wait_does_not_leak_a_recycled_pane_ids_status_cross_subtree`),
        // so this isn't just cheap hygiene: an unremoved entry could
        // otherwise be inherited by an unrelated later pane that reuses
        // this id, silently switching it into raw mode.
        self.raw.remove(&id);
        // Same reason again, for the three attention maps. These were pruned
        // only by `diff_statuses` on the tick, under a comment asserting the
        // opposite of the C23 note above — so for up to a `DETECT_INTERVAL`
        // a pane taking this id inherited: F3's "you already looked at this"
        // mark (silently absent from Alt+a though nobody ever saw it), a
        // last status that makes its first observation read as a transition
        // and prints a C20 feed line for a life it did not live, and the
        // dead pane's pending question, attributed to it.
        self.last_status.remove(&id);
        self.needy_msgs.remove(&id);
        self.visited_waiting.remove(&id);
        // And once more, for the two session-detection maps — the last
        // `PaneId`-keyed state in `App` that close was not pruning, and the
        // one where inheriting a dead pane's entry is not cosmetic.
        //
        // `last_shell_seen` is the floor `observe_panes` gives a promoted
        // pane's session scan ("the last tick this pane was still a
        // shell"). A pane that sat at a prompt for two hours leaves a
        // two-hour-old stamp behind; a later pane taking that id and
        // promoting before its own first shell observation — which is the
        // normal case for a pane spawned with initial input, since it is
        // running the agent within milliseconds — inherits it and scans two
        // hours back. That is exactly the window
        // `a_promoted_pane_never_claims_a_session_older_than_its_shell`
        // exists to close, reopened through the recycled id: the scan takes
        // the newest unclaimed file in that window, `set_session` commits
        // it permanently, and the next relaunch resumes a stranger's
        // conversation.
        //
        // `pending_detect` is the same hazard one step later: a pane closed
        // mid-detection leaves its window keyed on the id, and `tick` only
        // discards it if the id resolves to no spec at all — so an id
        // recycled within the tick keeps hunting on the dead pane's clock,
        // and can overwrite a session the new pane was resuming with an
        // unrelated one.
        self.last_shell_seen.remove(&id);
        self.pending_detect.remove(&id);
        // C40: and the pull mark, for the same recycled-id reason — a mark
        // left pointing at a closed pane would be pulled by whichever pane
        // inherited the number next.
        if self.marked == Some(id) {
            self.marked = None;
        }
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
            self.push_feed(format!("closed tab {}", tab_snapshot.name), false, None);
            self.remember_closed(Closed::Tab { index: ti, tab: tab_snapshot });
            self.spawn_active_tab();
        } else if !empty {
            if let Some(spec) = spec {
                // U2: the spec is already out of the tree, so the label is
                // built from the captured spec (same `{id} {name}` shape as
                // `feed_label`).
                self.push_feed(
                    format!("closed {id} {}", display_name_live(&spec, None)),
                    false,
                    None,
                );
                self.remember_closed(Closed::Pane { tab_index: ti, spec });
            }
        }
        // The active tab's membership may have changed out from under
        // `focused` (its pane closed, or its tab shifted/removed above) —
        // keep focus inside whatever tab is now on screen rather than
        // routing keystrokes to a pane in a tab nobody's looking at. U11:
        // being moved onto a tab is a tab switch like any other, so honor
        // that tab's focus memory before falling back to its first pane.
        // C22 rule 1 first: the float lives outside every tab's map by
        // construction, so this test's answer for a focused float is always
        // "not in this tab" — without the guard the fallback fired on every
        // close while the float was up, pulling focus onto a tiled pane
        // while the float stayed on screen, and the user's keystrokes went
        // somewhere they could not see.
        if !self.float_focused() && !self.ws.active_tab().panes.contains_key(&self.focused) {
            let active = self.ws.active_tab;
            let target = self
                .tab_focus_target(active)
                .or_else(|| self.pane_order().first().copied())
                .unwrap_or(0);
            self.set_focus(target);
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

    /// Queue one of **roost's own** notifications for the host terminal.
    ///
    /// The single door for every desktop notification roost raises — a pane
    /// needing input, a status transition, a pane exiting. `infra::notify`
    /// builds the bytes (a bell plus a bounded, sanitized `OSC 9`) and owns
    /// the process-wide budget; this puts them on the same `host_out` queue
    /// as P2's relays and P6's title, so they land between frames like
    /// everything else roost writes to the host (W3).
    pub fn notify_host(&mut self, text: &str) {
        if let Some(bytes) = crate::infra::notify::host_bytes(text) {
            self.host_out.extend_from_slice(&bytes);
        }
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

    /// P6: keep the host terminal's title at `roost · {id} {focused pane}`.
    /// Queues an OSC 2 only when the text actually changes, and no more often
    /// than `HOST_TITLE_INTERVAL` — an agent that republishes its OSC title on
    /// every spinner frame would otherwise repaint the host title bar ~30x a
    /// second. A change skipped by the throttle is picked up on the next
    /// call, since the comparison is against what was last *published*.
    ///
    /// The pane id leads the name, exactly as it does on the badge and in
    /// collapsed rows (C4/C8): a fleet of untitled panes in one project all
    /// render the same `shell · {cwd-tag}`, so without the id the window
    /// title cannot say *which* of them is focused — and the id is the join
    /// key to `roost send <id>` besides. `feed_label` builds the same pair.
    fn sync_host_title(&mut self) {
        if self.last_host_title.is_some_and(|t| t.elapsed() < HOST_TITLE_INTERVAL) {
            return;
        }
        // H1: this is the one write path in roost that bypasses ratatui's
        // buffer (W3) and lands raw bytes on the host terminal's own stdout
        // — so, unlike everything else `display_name` feeds (badge,
        // collapsed rows, feed, notifications), the label here needs its
        // own sanitization pass, not a borrowed guarantee from upstream.
        // `live_title` sanitizes the OSC-set title, but `display_name_live`
        // also returns `spec.title` (an Alt+r rename, but also a
        // hand-editable `title` field in workspace.json) and a `shell`
        // pane's `cwd.file_name()` (attacker-controlled via `roost spawn
        // --cwd`) verbatim — either can carry ESC/BEL/OSC bytes straight
        // into this escape sequence. Same cap as the live title, so a
        // hostile name can't grow the sequence unbounded either.
        let label = sanitize_title(&self.feed_label(self.focused), LIVE_TITLE_CAP);
        let want = format!("roost · {label}");
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
        self.push_feed(format!("{} exited", self.feed_label(id)), false, Some(id));
        if id == self.focused {
            return None;
        }
        Some(format!("{} exited", self.display_name(id)))
    }

    /// Session id reported exactly by an agent-side extension.
    pub fn on_session(&mut self, id: PaneId, session: String) {
        self.set_session(id, session);
    }

    /// The pane's status-socket connection went up or down (D2) — see
    /// `infra::sock`'s per-connection pane tracking and `StatusTracker::
    /// set_ext_link`. No notification: link state alone never means "needs
    /// you", only how much roost trusts a resting/Working report over PTY
    /// output while deciding what does.
    ///
    /// [F1] Refcounted via `ext_link_counts` — see its doc comment for why a
    /// bool undercounts (two connections reporting for one pane, e.g. a
    /// nested claude's one-shot hooks alongside the pane's own pi
    /// extension). `StatusTracker::set_ext_link` only ever sees the
    /// collapsed `count > 0`.
    pub fn on_status_link(&mut self, id: PaneId, up: bool) {
        let count = self.ext_link_counts.entry(id).or_insert(0);
        if up {
            *count += 1;
        } else {
            *count = count.saturating_sub(1);
        }
        let live = *count > 0;
        if let Some(rt) = self.runtimes.get_mut(&id) {
            rt.set_ext_link(live);
        }
    }

    /// Exact status from an agent-side extension. Returns a notification
    /// message when a *non-focused* pane starts needing the user.
    ///
    /// `detail` is the optional question text riding a `needs_input` report
    /// (the ask-tool's prompt). Every report updates `needy_msgs` — set on a
    /// NeedsInput that carries one, cleared otherwise — so the question can
    /// never outlive the ask it belongs to and reappear on a later ◆.
    pub fn on_status(
        &mut self,
        id: PaneId,
        status: AgentStatus,
        detail: Option<String>,
    ) -> Option<String> {
        let prev = self.runtimes.get(&id).map(|rt| rt.status());
        let prev_msg = self.needy_msgs.get(&id).cloned();
        if let Some(rt) = self.runtimes.get_mut(&id) {
            rt.set_extension_status(status);
        }
        // Socket-sourced text crossing into UI surfaces (feed, notification):
        // untrusted chrome-bound text — P6-class sanitization via
        // `sanitize_needy_msg` (controls including C1 become spaces, char
        // cap), see its doc for why neither existing sanitizer fits.
        let detail = match status {
            AgentStatus::NeedsInput => {
                detail.map(|m| sanitize_needy_msg(&m)).filter(|m| !m.is_empty())
            }
            _ => None,
        };
        match &detail {
            Some(m) => {
                self.needy_msgs.insert(id, m.clone());
            }
            None => {
                self.needy_msgs.remove(&id);
            }
        }
        // NeedsInput is an explicit "I need you" and always pulls attention
        // — but only once per ask: an extension's reconnect replay re-sends
        // its last needs_input verbatim (same status, same question), and a
        // ◆ can now rest for minutes on a question-ending turn, so a roost
        // restart or a transient socket drop in that window would re-ring
        // the desktop for an ask the user has already been told about. A
        // repeat is silent unless the question itself changed (a genuinely
        // new ask while already ◆ still notifies). Waiting is softer (turn
        // ended) — only notify when it follows active work, so a resume
        // that lands straight on Waiting doesn't nag.
        let became_needy = match status {
            AgentStatus::NeedsInput => prev != Some(AgentStatus::NeedsInput) || detail != prev_msg,
            AgentStatus::Waiting => prev == Some(AgentStatus::Working),
            _ => false,
        };
        if became_needy && id != self.focused {
            // U2: the shared display name — "shell · roost is waiting for
            // you", not an anonymous "shell is waiting for you". With a
            // question attached, say what's actually being asked.
            Some(match detail {
                Some(q) => format!("{} asks: {q}", self.display_name(id)),
                None => format!("{} is waiting for you", self.display_name(id)),
            })
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
        // U8(b) for C32: the pane editor owns the paste. On the name row
        // it takes Rename's filter verbatim (printables only — a name is
        // single-line, and no control byte may reach a title). On a note
        // row, newlines are the one control character that survives —
        // pasting a small checklist is the multi-line field's best entry
        // path. CR/CRLF normalize to LF first; every other control char
        // drops. Overflow past the line cap folds into the last line
        // (space-joined) rather than vanishing: a clipped paste must
        // still read as the text you pasted. The point lands after the
        // insertion, ahead of whatever tail it split off.
        if let Mode::PaneEdit { name, lines, row, col, .. } = &mut self.mode {
            if *row == 0 {
                let clean: String = text.chars().filter(|c| !c.is_control()).collect();
                let at = (*col).min(name.chars().count());
                let byte = byte_at(name, at);
                name.insert_str(byte, &clean);
                *col = at + clean.chars().count();
                return;
            }
            let clean = text.replace("\r\n", "\n").replace('\r', "\n");
            let clean: String = clean.chars().filter(|c| !c.is_control() || *c == '\n').collect();
            let mut nrow = (*row - 1).min(lines.len().saturating_sub(1));
            let at = byte_at(&lines[nrow], (*col).min(lines[nrow].chars().count()));
            let tail = lines[nrow][at..].to_string();
            lines[nrow].truncate(at);
            let mut parts = clean.split('\n');
            if let Some(first) = parts.next() {
                lines[nrow].push_str(first);
            }
            for part in parts {
                if lines.len() < NOTE_MAX_LINES {
                    nrow += 1;
                    lines.insert(nrow, part.to_string());
                } else {
                    if !lines[nrow].is_empty() && !part.is_empty() {
                        lines[nrow].push(' ');
                    }
                    lines[nrow].push_str(part);
                }
            }
            *col = lines[nrow].chars().count();
            lines[nrow].push_str(&tail);
            *row = nrow + 1;
            return;
        }
        if let Mode::Rename { buffer, cursor, .. } = &mut self.mode {
            // U16: a paste lands at the point, like typing does, and leaves
            // the point after what it inserted — a paste that always
            // appended would be the one edit in the dialog that ignored the
            // cursor.
            let clean: String = text.chars().filter(|c| !c.is_control()).collect();
            let at = (*cursor).min(buffer.chars().count());
            let byte = byte_at(buffer, at);
            buffer.insert_str(byte, &clean);
            *cursor = at + clean.chars().count();
            return;
        }
        if self.modal_active() {
            // Picker / Help / Feed / Roster: nothing to type into. The
            // roster's type-ahead is deliberately keystroke-only — a pasted
            // blob is not a filter anybody meant to type, and it must never
            // reach the pane hidden underneath (U8).
            return;
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
        // P21 × P5: a resize reflows the live grid, so every row index a
        // running search captured now points somewhere else. Re-capture
        // rather than paint hits at stale coordinates.
        self.refresh_search();
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
            (u32::from(px) * u32::from(cells) / u32::from(host_cells)).min(u32::from(u16::MAX))
                as u16
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
        self.pane_inner_dims(self.focused)
    }

    /// The same measure for any displayed pane — U19's wrapped-row join
    /// needs the *drawn* column count of the pane a click landed in.
    fn pane_inner_dims(&self, id: PaneId) -> (u16, u16) {
        self.display_rects()
            .iter()
            .find(|pr| pr.id == id)
            .map(|pr| inner_dims(pr.rect))
            .unwrap_or((1, 1))
    }

    // -- selection: Copy mode (C24) + native Normal-mode (C29) -------------

    pub fn in_copy_mode(&self) -> bool {
        matches!(self.mode, Mode::Copy { .. })
    }

    /// Start a selection in pane `id` at inner cell (row, col). C24: also
    /// moves the keyboard cursor there, so a fresh mouse drag and the
    /// keyboard cursor never disagree about where the selection is.
    pub fn begin_selection(&mut self, id: PaneId, row: u16, col: u16) {
        self.selection =
            Some(Selection { pane: id, anchor: (row, col), cursor: (row, col), dragging: true });
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

    /// C29: classify a left click at (pane, row, col) as the 1st/2nd/3rd of a
    /// run — crossterm hands roost no click count, so this derives one from
    /// timing (`DOUBLE_CLICK_INTERVAL`) and position (`CLICK_TOLERANCE`). A
    /// 4th click within the window wraps back to 1st rather than climbing
    /// past triple, since nothing above triple-click is bound.
    pub fn click_count(&mut self, pane: PaneId, row: u16, col: u16) -> u8 {
        let now = Instant::now();
        let count = match self.last_click {
            Some((p, r, c, n, at))
                if p == pane
                    && row.abs_diff(r) <= CLICK_TOLERANCE
                    && col.abs_diff(c) <= CLICK_TOLERANCE
                    && now.saturating_duration_since(at) < DOUBLE_CLICK_INTERVAL =>
            {
                if n >= 3 {
                    1
                } else {
                    n + 1
                }
            }
            _ => 1,
        };
        self.last_click = Some((pane, row, col, count, now));
        count
    }

    /// C29: double-click — select the whitespace-delimited word under (row,
    /// col), the same tokenizer `find_url_at` uses (`word_bounds_at`,
    /// cell-aware per B1). No word there (whitespace, or past the row's
    /// trimmed content) **clears the selection outright** (D3) rather than
    /// degrading to a single-point (anchor == cursor) selection: a
    /// single-point selection is indistinguishable from a genuine
    /// one-character *word* (also anchor == cursor, by construction), so
    /// double-clicking `a`, `$`, or any other 1-char token would silently
    /// copy nothing. Tracking "found nothing" explicitly, instead of
    /// inferring it from the resulting range, keeps the two distinguishable.
    pub fn select_word_at(&mut self, pane: PaneId, row: u16, col: u16) {
        use unicode_width::UnicodeWidthChar;
        let line = self.row_text(pane, row);
        match word_bounds_at(&line, col as usize) {
            Some((start, end)) => {
                let c0 = char_to_cell(&line, start) as u16;
                // B1: the *last* cell of the end char's own span — a
                // trailing wide glyph must stay fully inside the
                // selection, not just its first column.
                let end_w = line.chars().nth(end).and_then(|c| c.width()).unwrap_or(1).max(1);
                let c1 = (char_to_cell(&line, end) + end_w - 1) as u16;
                self.selection =
                    Some(Selection { pane, anchor: (row, c0), cursor: (row, c1), dragging: false });
            }
            None => self.selection = None,
        }
    }

    /// C29: triple-click — select the whole row, mirroring Copy mode's `V`
    /// (same anchor/cursor shape, `dragging: false` so a further drag does
    /// not silently widen it word-by-word — out of scope here, see C29).
    pub fn select_line_at(&mut self, pane: PaneId, row: u16) {
        let last = self.pane_inner_dims(pane).1.saturating_sub(1);
        self.selection =
            Some(Selection { pane, anchor: (row, 0), cursor: (row, last), dragging: false });
    }

    /// C29: shift-click — extend the live selection to the pointer, keeping
    /// its anchor. With nothing to extend (no selection yet, or one that
    /// belongs to a different pane) it starts a fresh one at the click point
    /// instead, exactly like a plain click — there is no persistent caret to
    /// extend from otherwise. Also arms `dragging`, so a shift-click
    /// followed by a held drag keeps extending through the ordinary drag
    /// path rather than freezing at the shift-click's own point.
    pub fn extend_selection_to(&mut self, pane: PaneId, row: u16, col: u16) {
        match &mut self.selection {
            Some(sel) if sel.pane == pane => {
                sel.cursor = (row, col);
                sel.dragging = true;
            }
            _ => self.begin_selection(pane, row, col),
        }
    }

    /// U14: the hint-bar text for a finished copy of `chars` characters,
    /// given which clipboard channel actually took it — fired only once the
    /// channel is known, not at extraction time, so a clipboard write that
    /// silently failed can't still report a successful copy. Pure, so the
    /// wording is pinned without touching a clipboard.
    pub fn copy_flash_text(chars: usize, outcome: ClipboardOutcome) -> String {
        match outcome {
            ClipboardOutcome::Native => format!("copied {chars} chars"),
            // Sent, unacknowledged: OSC 52 has no reply, and the terminal
            // may not be listening. The qualifier is the whole point — it
            // tells you where to look when the paste comes up empty.
            ClipboardOutcome::Osc52 => format!("copied {chars} chars (OSC 52)"),
            // Both channels tried and lost (ClipboardOutcome::Failed's own
            // contract: "neither channel got the text anywhere") — name that
            // plainly, and point at the one thing actually worth checking:
            // the terminal's own clipboard support, since a missing native
            // helper is the far more common half of "both".
            ClipboardOutcome::Failed => {
                "copy failed: no clipboard channel worked; check your terminal's clipboard support"
                    .to_string()
            }
        }
    }

    /// U14: flash the result of a copy the composition root just performed
    /// (core has no clipboard I/O of its own — same division as
    /// `pending_yank`). Both copy paths, mouse and keyboard, end here.
    pub fn flash_copy(&mut self, chars: usize, outcome: ClipboardOutcome) {
        self.set_flash(Self::copy_flash_text(chars, outcome));
    }

    /// Shared by `finish_selection`/`finish_native_selection`: clear
    /// `dragging` on the live selection and grab its text — `None` for an
    /// empty grab or no live selection.
    fn grab_selection_text(&mut self) -> Option<String> {
        let sel = self.selection.as_mut()?;
        sel.dragging = false;
        let (pane, anchor, cursor) = (sel.pane, sel.anchor, sel.cursor);
        let text =
            self.runtimes.get(&pane).map(|rt| rt.grab_text(anchor, cursor)).unwrap_or_default();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }

    /// Finish the drag: extract the selected text and leave copy mode.
    /// Returns the text to hand to the clipboard (None when the selection
    /// is empty). U14: the "copied" flash is *not* set here — it is set by
    /// the caller once the clipboard has actually answered (`flash_copy`).
    pub fn finish_selection(&mut self) -> Option<String> {
        let text = self.grab_selection_text();
        if self.selection.is_some() {
            self.mode = Mode::Normal;
            self.selection = None;
        }
        text
    }

    /// C29: end a native (Normal-mode) selection gesture *without* discarding
    /// it. Unlike `finish_selection` above — which always exits Copy mode,
    /// dropping the highlight with it — a native selection stays lit until
    /// the next click (`on_click`) or keypress (`handle_mode_key`), so this
    /// only clears `dragging` and returns the grabbed text, leaving
    /// `self.selection` (and `self.mode`, already `Normal`) untouched.
    pub fn finish_native_selection(&mut self) -> Option<String> {
        self.grab_selection_text()
    }

    /// C29: handle a release over a live native selection — the single path
    /// `main.rs`'s `Up` arm calls, so the click-count-aware timing rule
    /// below lives in one place rather than split across the two layers.
    ///
    /// D3: a gesture that never moved (`dragging` still true — a plain
    /// click or shift-click, never promoted by a drag or a click-count
    /// upgrade — *and* `anchor == cursor`) selects nothing and is cleared
    /// outright. Anything else is a real selection, **even one cell wide**
    /// — `select_word_at` already clears `self.selection` itself when it
    /// truly finds nothing, so by the time this runs `Some` means "there
    /// is something here".
    ///
    /// S4: a double-click's own release is *staged*, not committed
    /// immediately — it is the one release a still-possible 3rd click can
    /// supersede within the same double-click window. Every other release
    /// (a plain drag, or a triple-click's own release) commits straight
    /// through `finish_native_selection`.
    ///
    /// Every release — staged or immediate — starts by clearing any earlier
    /// stage outright: a real mouse release always supersedes one, most
    /// commonly the 3rd click of a triple-click, whose own (wider)
    /// selection is the one that should win. A keypress does NOT cancel a
    /// stage (F5): it has nothing to do with whether the word actually
    /// double-clicked should still land on the clipboard, so `due_copy`
    /// fires it regardless.
    ///
    /// Returns the text to copy *now*, if any — `None` covers "nothing was
    /// selected", "staged for later" (see `due_copy`) and "nothing
    /// extractable" alike; the caller doesn't need to tell them apart.
    pub fn release_native_selection(&mut self) -> Option<String> {
        self.pending_copy = None;
        let sel = self.selection?;
        if sel.dragging && sel.anchor == sel.cursor {
            self.selection = None;
            return None;
        }
        if self.last_click.is_some_and(|(.., n, _)| n == 2) {
            // Grab the text now — not a promise to re-read the grid later,
            // which a streaming pane could scroll or overwrite by the time
            // the deadline passes (see `PendingCopy`).
            if let Some(text) = self.finish_native_selection() {
                self.pending_copy = Some((text, Instant::now() + DOUBLE_CLICK_INTERVAL));
            }
            return None;
        }
        let text = self.finish_native_selection();
        if text.is_none() {
            self.selection = None; // dragged/clicked over nothing extractable
        }
        text
    }

    /// S4: the staged double-click copy's text, once its deadline has
    /// passed — `None` otherwise, whether because it's still waiting on a
    /// possible 3rd click, or because there was never one staged.
    ///
    /// Real supersession is handled at stage-time (see
    /// `release_native_selection`); an unrelated keypress clearing the
    /// highlight does NOT cancel a staged copy (F5) — the text is already
    /// fixed at release time, so once the deadline passes there is nothing
    /// left to check, it fires. The composition root calls this once per
    /// event-loop tick and performs the actual clipboard write when it
    /// answers (core has no I/O of its own).
    pub fn due_copy(&mut self) -> Option<String> {
        let at = self.pending_copy.as_ref()?.1;
        if Instant::now() < at {
            return None;
        }
        self.pending_copy.take().map(|(text, _)| text)
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

    /// The URL under inner cell (row, col) of pane `id`, if any — for
    /// Alt+click-to-open and copy mode's `o`.
    ///
    /// U19: reads the whole *wrapped run* the row belongs to, not one grid
    /// row. Agents print long CI/GitHub links into narrow panes constantly;
    /// reading only the clicked row would open a wrapped link's first-row
    /// fragment and dead-click everywhere below the break.
    pub fn url_at(&self, id: PaneId, row: u16, col: u16) -> Option<String> {
        let rt = self.runtimes.get(&id)?;
        let (rows, width) = self.pane_inner_dims(id);
        // Walk back to the run's first row, then collect forward through it.
        let mut first = row;
        while first > 0 && rt.row_wrapped(first - 1) {
            first -= 1;
        }
        let mut run: Vec<String> = Vec::new();
        let mut r = first;
        loop {
            run.push(rt.grab_text((r, 0), (r, u16::MAX)));
            // A run can't outlast the grid, whatever the flags claim.
            if !rt.row_wrapped(r) || r >= rows.saturating_sub(1) {
                break;
            }
            r += 1;
        }
        url_in_wrapped_rows(&run, width as usize, (row - first) as usize, col as usize)
    }

    // -- mouse -------------------------------------------------------------

    /// Left click: focus the pane under the cursor (expanding stack members).
    /// C22 rule 2: a click outside the float's rect hides it first — the
    /// click itself still lands normally on whatever it hit (below).
    /// Clicking the float's own rect (id == the already-focused float) is
    /// not "outside" and leaves it shown.
    pub fn on_click(&mut self, id: PaneId) {
        // C29: a plain click elsewhere dismisses a lingering native
        // selection — its highlight stays lit only until "the next click or
        // keypress" (the native-selection contract, C29), and a click on a
        // *different* pane is exactly that. A click back inside the same
        // pane is left alone here: the caller starts a fresh gesture right
        // after this (`begin_selection`/`select_word_at`/…), which already
        // supersedes whatever was there.
        if self.selection.is_some_and(|s| s.pane != id) {
            self.selection = None;
        }
        if self.float_focused() && id != self.focused {
            self.hide_float();
        }
        self.set_focus(id);
        layout::expand_in_stacks(&mut self.ws.active_tab_mut().layout, id);
        self.relayout();
        self.save();
    }

    /// U8: is a modal overlay up? These six modes (the roster, C27, is a
    /// C12 modal like the rest) own the whole non-keyboard input surface —
    /// mouse and paste alike — while they are active. Scroll/Copy are
    /// deliberately excluded: they draw no dialog, nothing is hidden
    /// underneath them, and their mouse behavior (wheel scrolls, drag
    /// selects) is the point.
    pub fn modal_active(&self) -> bool {
        matches!(
            self.mode,
            Mode::Rename { .. }
                | Mode::PaneEdit { .. }
                | Mode::Picker { .. }
                | Mode::Help { .. }
                | Mode::Feed { .. }
                | Mode::Roster { .. }
                // C36: omitting the composer here was U8(b) verbatim — a
                // paste landed in the pane *behind* the dialog and a click
                // moved focus under it. Found by the C36 design audit.
                | Mode::Broadcast { .. }
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
        let page = self.overlay_page();
        let cap = self.feed.len().saturating_sub(1);
        if let Mode::Feed { offset } = &mut self.mode {
            match me.kind {
                MouseEventKind::ScrollUp => *offset = (*offset + page).min(cap),
                MouseEventKind::ScrollDown => *offset = offset.saturating_sub(page),
                _ => {}
            }
        }
        // C27: the roster's wheel moves its CURSOR, not a detached view —
        // Enter acts on the cursor, so a wheel that scrolled the list out
        // from under it would leave the overlay pointing at a row nobody can
        // see. One row per notch, the arrow keys' own step.
        if matches!(self.mode, Mode::Roster { .. }) {
            match me.kind {
                MouseEventKind::ScrollUp => self.roster_move(-1),
                MouseEventKind::ScrollDown => self.roster_move(1),
                _ => {}
            }
        }
        // C15: the wheel reads the keymap on when there is more of it than
        // fits (its own PgUp/PgDn step, like the feed's), and any *click*
        // dismisses it — "any key closes it" in mouse form. Handled here,
        // above the left-click-only gate below, or the one gesture that
        // means "show me more" would fall through to the dismissal. Over a
        // keymap that fits entirely on screen the wheel is simply inert,
        // exactly as the arrow keys are.
        if matches!(self.mode, Mode::Help { .. }) {
            let page = self.help_page();
            match me.kind {
                MouseEventKind::ScrollUp => {
                    self.help_scroll(-page);
                }
                MouseEventKind::ScrollDown => {
                    self.help_scroll(page);
                }
                MouseEventKind::Down(MouseButton::Left) => self.mode = Mode::Normal,
                _ => {}
            }
            return;
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
        // dialog opened on). Esc/Enter stay the ways out. The note editor
        // (C32) rides the same rule for the same reason, with more unsaved
        // work at stake.
        if matches!(self.mode, Mode::Rename { .. } | Mode::PaneEdit { .. }) {
            return;
        }
        if matches!(self.mode, Mode::Picker { .. }) {
            // U20: hit-test against the FILTERED list — the rows that are
            // actually drawn. A click on row 2 must launch what row 2 says,
            // whatever the type-ahead has narrowed away. Row *count* only,
            // so this skips `picker_filtered`'s PATH checks — same number of
            // rows either way, they just don't need re-proving per click.
            let rows = self.picker_filtered_ids().len();
            match dialog.and_then(|r| crate::ui::mouse::picker_row_at(r, rows, me.column, me.row)) {
                Some(i) => self.picker_launch(i),
                None if !inside => self.mode = Mode::Normal,
                None => {}
            }
            return;
        }
        // C27: a click on a pane row goes to that pane (one press — the
        // roster is a navigator, and "select, then confirm" would be a second
        // click for nothing, the same call C14's picker made); a click on a
        // group header does nothing, because a header is a label; a click
        // outside dismisses, like the feed.
        if matches!(self.mode, Mode::Roster { .. }) {
            let (rows, top) = self.roster_view();
            let visible = rows.len().saturating_sub(top).min(self.roster_view_rows());
            match dialog
                .and_then(|r| crate::ui::mouse::picker_row_at(r, visible, me.column, me.row))
            {
                Some(i) => {
                    if let Some(RosterRow::Pane { id }) = rows.get(top + i) {
                        let id = *id;
                        self.roster_jump(id);
                    }
                }
                None if !inside => self.mode = Mode::Normal,
                None => {}
            }
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
    /// the Working spinner must stop asserting liveness over (N1). 0 for a
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

    // -- P21: scrollback search --------------------------------------------

    /// `/` from Scroll or Copy mode: capture the focused pane's whole
    /// history + screen and open the incremental prompt over it. Re-pressing
    /// `/` with a search already up restarts it (a fresh haystack — the
    /// pane may have moved on), which is also what makes `/` idempotent
    /// rather than a way to stack searches.
    fn open_search(&mut self, copy_cursor: Option<(u16, u16)>) {
        let pane = self.focused;
        let Some(rt) = self.runtimes.get(&pane) else { return };
        let lines: Vec<String> = rt.grab_all_text().lines().map(str::to_string).collect();
        let (origin, total) = (rt.scroll_offset(), rt.scroll_total());
        self.search = Some(Search {
            pane,
            query: String::new(),
            lines,
            total,
            matches: Vec::new(),
            current: 0,
            origin,
        });
        self.mode = Mode::Search { copy_cursor };
    }

    /// Re-run the search against the (frozen) haystack after the query
    /// changed, and park the view on the first hit — reading order, so
    /// typing more characters walks *forward* from the oldest match rather
    /// than teleporting around the buffer. An empty query puts the view back
    /// where the search opened: backspacing to nothing is a cancel of the
    /// filtering, not a jump to row zero.
    fn refilter_search(&mut self) {
        let Some(s) = &mut self.search else { return };
        s.matches = find_matches(&s.lines, &s.query);
        s.current = 0;
        if s.matches.is_empty() {
            let (pane, origin) = (s.pane, s.origin);
            self.scroll_view_to(pane, origin);
            return;
        }
        self.jump_to_match(0);
    }

    /// Park the view on hit `idx` (wrapping, so `n` past the last match
    /// lands back on the first). The haystack's line `i` sits `total - i`
    /// rows above the live tail; that offset goes through the U9-vetted
    /// path — write it, then read the grid's clamp back — so the `↑N` badge
    /// and Scroll mode's `↑N/M` describe where the view actually is, never
    /// what the arithmetic asked for.
    fn jump_to_match(&mut self, idx: usize) {
        let Some(s) = &mut self.search else { return };
        if s.matches.is_empty() {
            return;
        }
        let idx = idx % s.matches.len();
        s.current = idx;
        let (line, _) = s.matches[idx];
        let (pane, want) = (s.pane, s.total.saturating_sub(line));
        self.scroll_view_to(pane, want);
    }

    /// `n` / `N`: step to the next (newer) or previous (older) hit, wrapping
    /// at both ends. Silent with no search running or no hits — the mode's
    /// other keys still work, and a flash per keypress would be noise.
    fn step_match(&mut self, forward: bool) {
        let Some(s) = &self.search else { return };
        let n = s.matches.len();
        if n == 0 {
            return;
        }
        let next = if forward { s.current + 1 } else { s.current + n - 1 };
        self.jump_to_match(next % n);
    }

    /// The one place a search moves a view: write the offset and let the
    /// grid clamp it. [P14] There is nothing to mirror afterwards — Scroll
    /// mode keeps no counter, and every surface that reports the position
    /// asks the backend — so a jump past the banked history can no longer
    /// leave a phantom number behind the honest one.
    fn scroll_view_to(&mut self, pane: PaneId, offset: usize) {
        let Some(rt) = self.runtimes.get_mut(&pane) else { return };
        rt.set_scrollback(offset);
    }

    /// P21 × P5 (reflow): re-capture a running search's haystack against the
    /// grid as it is *now*. A resize rewraps the live grid, so both the row
    /// count and which text sits on which row change — capture-time indices
    /// would place highlights on the wrong lines and send `n` to the wrong
    /// place. The view is deliberately **not** re-jumped: the grid already
    /// re-clamped the offset during the reflow, and a resize teleporting you
    /// to a match you had scrolled away from would be its own surprise.
    /// `current` is clamped into the new hit list, and a query that no
    /// longer matches anything simply reports `0/0`.
    fn refresh_search(&mut self) {
        let Some(s) = &self.search else { return };
        let pane = s.pane;
        let Some(rt) = self.runtimes.get(&pane) else { return };
        let lines: Vec<String> = rt.grab_all_text().lines().map(str::to_string).collect();
        let total = rt.scroll_total();
        let Some(s) = &mut self.search else { return };
        s.matches = find_matches(&lines, &s.query);
        s.current = s.current.min(s.matches.len().saturating_sub(1));
        s.lines = lines;
        s.total = total;
    }

    /// Esc: throw the search away and put the view back exactly where it was
    /// when `/` was pressed. A cancelled search leaves no trace — not the
    /// highlights, not the position it wandered to.
    fn cancel_search(&mut self) {
        let Some(s) = self.search.take() else { return };
        self.scroll_view_to(s.pane, s.origin);
    }

    /// P21: the mode the search prompt hands back to. [P14] Scroll mode
    /// carries nothing to re-seed: the view is wherever the search left it,
    /// and that is read from the grid by whoever asks.
    fn mode_after_search(&self, copy_cursor: Option<(u16, u16)>) -> Mode {
        match copy_cursor {
            Some(cursor) => Mode::Copy { cursor },
            None => Mode::Scroll,
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

    /// The pasteable resume command for a pane — `cd <cwd> && pi --session
    /// <id>` — the same adapter `resume()` Enter runs, rendered for a plain
    /// terminal (the `y` key on the dead-pane bar copies it). None when the
    /// pane has no session pointer (shell panes, agents that died before
    /// session detection): nothing to resume, so the bars don't advertise
    /// `y`. Deliberately skips SessionResolver's on-disk liveness check —
    /// a copied command for a since-deleted session fails visibly when
    /// pasted, which is honest enough for an escape hatch. What it must NOT
    /// skip is `valid_session_id`: `set_session` stores whatever the pane's
    /// token-authenticated socket sent, and this is the one consumer that
    /// renders the id into a *shell* line — a flag-shaped id
    /// (`--config=/tmp/evil`) is all shell_word-safe chars, so without the
    /// check it would paste unquoted into the user's trusted terminal. The
    /// spawn path gets the same guard from SessionResolver.
    pub fn resume_command_line(&self, id: PaneId) -> Option<String> {
        let spec = self.find_spec(id)?;
        let session = spec.session.as_deref().filter(|s| crate::agents::valid_session_id(s))?;
        let adapter = self.registry.get(spec.adapter.as_str())?;
        Some(adapter.resume(&spec.cwd, session).shell_line())
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
        //
        // **[C38] `MovePane` left this list**, joining Alt+z for the same
        // reason and by the same mechanism: it now refuses the float itself
        // (`move_pane_dir`) rather than being retargeted onto `prev_focus`.
        // While it was here the C38 float row could never fire — `hide_float`
        // ran first, so `float_focused()` was always false by the time the
        // check saw it — and worse, the behaviour it was written to replace
        // was not silence but a *surprise*: the float vanished and the swap
        // landed on whichever pane it had been covering. Found by the C38
        // design audit, which probed the row instead of reading it. Zoom is
        // still left, from inside `move_pane_dir` — under zoom `rects()`
        // yields only the zoomed pane, so `neighbor` must run after.
        if matches!(
            action,
            Action::NewPane
                | Action::ToggleStack
                | Action::FlipSplit
                | Action::Resize { .. }
                | Action::CycleLayout { .. }
        ) {
            self.exit_zoom();
            self.hide_float();
        }
        match action {
            Action::Quit => self.quit_guarded(),
            Action::NewPane => {
                if self.spawn_child("shell", None, None).is_none() {
                    self.flash_no_room();
                }
            }
            Action::ClosePane => self.close_pane(),
            Action::Focus(dir) => self.focus_dir(dir),
            Action::MovePane(dir) => self.move_pane_dir(dir),
            Action::FocusAlternate => self.focus_alternate(),
            Action::ToggleBroadcast => self.toggle_broadcast(),
            Action::NewTab => self.new_tab(),
            Action::GoToTab(i) => self.go_to_tab(i),
            Action::NextTab => self.step_tab(1),
            Action::PrevTab => self.step_tab(-1),
            Action::LastTab => self.go_to_tab(self.ws.tabs.len().saturating_sub(1)),
            Action::MovePaneToTab { forward } => {
                self.move_pane_to_tab(if forward { 1 } else { -1 })
            }
            Action::MarkPane => self.mark_pane(),
            Action::PullPane => self.pull_pane(),
            // C38: each of these three declines silently on a tab the shape
            // cannot apply to — overwhelmingly "this tab has one pane", and
            // `Alt+s` is one of C9's advertised seven, so its silence is the
            // most visible of the three. The refusal is detected by
            // comparing the tree rather than by the mutator's `bool`,
            // because the bool does not mean "changed": `resize_pane`
            // returns `true` at the 0.1/0.9 clamp ("handled, no change"),
            // which is a no-op the user has every reason to hear about.
            // One rule, one mechanism, no per-site special cases.
            Action::ToggleStack => {
                let focused = self.focused;
                let before = self.ws.active_tab().layout.clone();
                layout::toggle_stack(&mut self.ws.active_tab_mut().layout, focused);
                self.flash_if_layout_unchanged(&before, "stack");
            }
            Action::FlipSplit => {
                let focused = self.focused;
                let before = self.ws.active_tab().layout.clone();
                layout::flip_split(&mut self.ws.active_tab_mut().layout, focused);
                self.flash_if_layout_unchanged(&before, "flip");
            }
            Action::Resize { horizontal, grow } => {
                let delta = if grow { 0.04 } else { -0.04 };
                let axis = if horizontal { SplitDir::Vertical } else { SplitDir::Horizontal };
                let focused = self.focused;
                let before = self.ws.active_tab().layout.clone();
                layout::resize_pane(&mut self.ws.active_tab_mut().layout, focused, axis, delta);
                self.flash_if_layout_unchanged(&before, "resize");
            }
            Action::EditPane => {
                // C32 (combined): opens prefilled — the editor IS the
                // note's reader (peek, edit and clear are one surface), so
                // both fields must arrive intact. The point opens at the
                // end of the *name* row: renames are the muscle memory
                // Alt+r inherits, and one Shift+↵/↓ reaches the note. A
                // specless pane (dead mid-frame) has nowhere to keep
                // either field; the chord just does nothing there.
                let focused = self.focused;
                if let Some(spec) = self.find_spec(focused) {
                    let name = spec.title.clone().unwrap_or_default();
                    let lines: Vec<String> = match &spec.note {
                        Some(n) => n.lines().map(str::to_string).collect(),
                        None => vec![String::new()],
                    };
                    // `str::lines` on "" yields no lines; the editor's
                    // invariant is at least one note row.
                    let lines = if lines.is_empty() { vec![String::new()] } else { lines };
                    let col = name.chars().count();
                    self.mode = Mode::PaneEdit { name, lines, row: 0, col, pane: focused };
                }
            }
            Action::RenameTab => {
                let current = self.ws.active_tab().name.clone();
                let cursor = current.chars().count();
                self.mode = Mode::Rename { buffer: current, cursor, target: RenameTarget::Tab };
            }
            Action::QuickLaunch => {
                // U20: a fresh picker every time — no sticky filter from a
                // previous visit, and the cwd column parked on its most
                // recent entry (which is the pane you are splitting off, so
                // the zero-keystroke launch matches the pre-U20 behavior).
                self.mode =
                    Mode::Picker { selection: 0, filter: String::new(), cwd: 0, on_cwd: false };
            }
            Action::ScrollMode => {
                // U9: entering Scroll mode after wheeling continues from the
                // wheeled position — a zero seed made the first keypress snap
                // the view back toward the live tail. [P14] It continues from
                // there by *not carrying a seed at all*: the view is already
                // where the wheel left it, and the first key reads it from
                // the grid.
                self.mode = Mode::Scroll;
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
            Action::Help => self.mode = Mode::Help { top: 0, filter: None },
            Action::JumpAttention => self.jump_attention(),
            Action::ToggleRoster => self.toggle_roster(),
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
            Action::CycleLayout { forward } => self.cycle_layout(forward),
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
                let first = self.pane_order().first().copied().unwrap_or(0);
                self.set_focus(first);
                self.push_feed(format!("reopened tab {name}"), false, None);
                // U2: flashes name what they acted on.
                self.set_flash(format!("reopened tab {name}"));
            }
            Closed::Pane { tab_index, spec } => {
                // Restore into its original tab if it still exists, else the
                // active one; split the focused pane and reuse the saved spec
                // (session id preserved ⇒ the agent resumes).
                self.ws.active_tab = tab_index.min(self.ws.tabs.len().saturating_sub(1));
                let first = self.pane_order().first().copied().unwrap_or(0);
                self.set_focus(first);
                if !self.restore_pane(spec.clone()) {
                    // Refused for want of room. The close is NOT consumed:
                    // put it back so the pane is still reopenable once the
                    // user makes room, and say why nothing happened (C38).
                    self.remember_closed(Closed::Pane { tab_index, spec });
                    self.flash_no_room();
                    return;
                }
                // U2: `restore_pane` allocated the pane's NEW id and left it
                // focused — label with that id, not the closed one's.
                let restored = self.focused;
                self.push_feed(
                    format!("reopened {}", self.feed_label(restored)),
                    false,
                    Some(restored),
                );
                self.set_flash(format!("reopened {}", self.display_name(restored)));
            }
        }
    }

    /// Insert `spec` as a new pane split off the focused pane, spawning it.
    /// Shared by undo (reuses a saved spec, session and all). Returns false
    /// if the split was refused for want of room — nothing was mutated.
    ///
    /// C38: the fit test is `split_fit`, the same one `spawn_child` uses,
    /// not a hand-rolled copy. This used to pick the axis from the rect's
    /// aspect and then split unconditionally, so `Alt+u` would land a pane
    /// in a slot `Alt+n` had just refused to open — under
    /// `MIN_SPLIT_COLS`×`MIN_SPLIT_ROWS`, which is also the vt100 underflow
    /// trigger, and it took the pane being split down there with it. Same
    /// rect and the same keypress-level decision must give the same answer.
    #[must_use]
    fn restore_pane(&mut self, spec: PaneSpec) -> bool {
        let focused = self.focused;
        let target_rect = self.rects().iter().find(|pr| pr.id == focused).map(|pr| pr.rect);
        let Some(dir) = split_fit(target_rect) else { return false };
        let id = self.alloc_pane_id();
        let tab = self.ws.active_tab_mut();
        tab.panes.insert(id, spec.clone());
        if !layout::split_pane(&mut tab.layout, focused, id, dir) {
            tab.layout = LayoutNode::Pane(id);
        }
        self.set_focus(id);
        if let Some(pr) = self.rects().iter().find(|pr| pr.id == id).copied() {
            self.spawn_pane(id, &spec, pr.rect);
        }
        true
    }

    /// P10: move roost's focus to `id`, telling the panes involved.
    ///
    /// THE single writer of `self.focused` — every focus move in roost
    /// (arrows, click, tab switch, spawn, undo, float show/hide, the
    /// attention ring) funnels through here, so a pane that subscribed to
    /// focus reporting can never miss a transition because some new code
    /// path assigned the field directly. A no-op move sends nothing: `CSI O`
    /// followed by `CSI I` for "focus stayed put" is a lie an editor would
    /// act on (nvim's `FocusGained` autoread re-stats every buffer).
    ///
    /// Nothing is sent while roost's own window is blurred — see
    /// `host_focused`. The reports are written with `write_input_raw`, not
    /// `write_input`: they are roost speaking, not the user typing, and
    /// yanking a scrolled-back pane to its live tail because focus moved
    /// would undo exactly what U9 fixed.
    fn set_focus(&mut self, id: PaneId) {
        let old = self.focused;
        // C35: the trail "go back" follows. Recorded here rather than at each
        // call site precisely because this is the one chokepoint — the
        // comment below already says every focus move funnels through it, and
        // a per-caller version would have to be remembered by the next
        // feature that moves focus.
        if old != id && self.pane_exists(old) {
            self.alternate = Some(old);
        }
        // C22 rule 1: a shown float IS the focused pane, so focus landing on
        // anything else means the float is no longer up. Enforced here, in
        // the single writer of `self.focused`, rather than at each door: the
        // rule was previously re-applied per call site, and the ones that
        // forgot (C35's Alt+`` `` "go back" among them) left the float drawn
        // over the body with the user's keystrokes going to a tiled pane
        // underneath it. `shown` is cleared directly rather than through
        // `hide_float`, which would re-route focus the caller has already
        // decided.
        if old != id && self.float_focused() && !self.is_float(id) {
            if let Some(f) = &mut self.float {
                f.shown = false;
            }
        }
        self.focused = id;
        // F3: landing focus on a pane that's currently in the ○ fallback
        // takes it out of `attention_ring`'s rotation — see
        // `visited_waiting`. Every focus move funnels through here, so
        // Alt+a, a click, arrow-focus and the roster all count the same way
        // (the finding named "visiting", not "Alt+a specifically").
        if self.display_status(id) == Some(AgentStatus::Waiting) {
            self.visited_waiting.insert(id);
        }
        // Nothing inside roost is focused while roost isn't: the old pane
        // was already told it lost focus when the window blurred, and the
        // new one collects its `CSI I` when the window comes back.
        if old == id || !self.host_focused {
            return;
        }
        self.report_focus(old, false);
        self.report_focus(id, true);
    }

    /// P10: roost's own window gained/lost the host terminal's focus. The
    /// host event is about roost as a whole; inside roost exactly one pane
    /// holds focus, so it is that pane's event too. Idempotent — terminals
    /// repeat focus events freely (a window manager can re-assert focus on
    /// every raise), and a duplicate `CSI I` is a spurious wakeup.
    pub fn on_host_focus(&mut self, focused: bool) {
        if self.host_focused == focused {
            return;
        }
        self.host_focused = focused;
        self.report_focus(self.focused, focused);
    }

    /// P10: send one focus report to `id` — `CSI I` (gained) or `CSI O`
    /// (lost) — but only if that pane's app asked for them with `?1004h`.
    /// A pane that never subscribed gets nothing: to it these bytes are
    /// input, and `\x1b[I` typed into a shell is a live command edit.
    /// Whether a *transition* is worth reporting at all is the callers'
    /// call (`set_focus` / `on_host_focus`); this only addresses it.
    fn report_focus(&mut self, id: PaneId, gained: bool) {
        let bytes: &[u8] = if gained { b"\x1b[I" } else { b"\x1b[O" };
        if let Some(rt) = self.runtimes.get_mut(&id) {
            if rt.focus_events() {
                rt.write_input_raw(bytes);
            }
        }
    }

    /// Move focus spatially to the nearest pane in `dir`; stay put if none
    /// — except `Left`/`Right` off a tab's edge, which continue into the
    /// next/previous tab (`focus_dir_cross_tab`, C31).
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
            self.set_focus(id);
            layout::expand_in_stacks(&mut self.ws.active_tab_mut().layout, id);
            return;
        }
        self.focus_dir_cross_tab(dir, &rects);
    }

    /// C33 (`Alt+Shift+hjkl`): swap the focused pane with its spatial
    /// neighbour in `dir`, inside the active tab.
    ///
    /// Deliberately the same first two steps as `focus_dir` above — same
    /// `rects()`, same `layout::neighbor` — so "which pane is that way?"
    /// has exactly one answer in roost and the move can never disagree with
    /// the focus key that taught the user the direction. Stacks need no
    /// special case for the same reason: every member already carries its
    /// own `PaneRect` (collapsed ones a single row), so `Down` inside a
    /// stack finds the next member the way it finds the next split.
    ///
    /// **No cross-tab handoff at the edge.** `focus_dir` falls through to
    /// `focus_dir_cross_tab`; this deliberately does not. Moving a pane out
    /// of its tab is a structural edit rather than a look, `Alt+Shift+i`/
    /// `Alt+Shift+m` (C28) already do exactly that and say so, and a no-op
    /// is the recoverable failure — see DESIGN-ui.md C33 for the full
    /// argument.
    ///
    /// The float is excluded on C22 rule 2's grounds, exactly as
    /// `focus_dir` excludes it: it has no position in the tiled tree, so no
    /// direction applies to it. Unlike `focus_dir` this does not *hide* the
    /// float — `apply`'s structural-action guard above already did, so by
    /// the time this runs a float-focused user has been returned to
    /// `prev_focus` and the chord acts on that pane instead.
    fn move_pane_dir(&mut self, dir: layout::Dir) {
        // C38: every refusal here says so. C33 argued only that a swap must
        // not *cross tabs* ("the recoverable failure is doing nothing") —
        // never that it should do so wordlessly, and C35 had already
        // written the opposite rule for the sibling gesture: "a navigation
        // key that silently does nothing reads as broken". Found by a
        // simulation agent comparing the two, which is the comparison that
        // makes the inconsistency visible.
        if self.float_focused() {
            self.set_flash("the scratch pane sits outside the layout");
            return;
        }
        // C21, from here rather than `apply`'s structural guard: the float
        // check above has to see a float that is still shown, and `rects()`
        // below has to see the whole tab rather than just the zoomed pane.
        self.exit_zoom();
        let rects = self.rects();
        let Some(target) = layout::neighbor(&rects, self.focused, dir) else {
            self.flash_move_edge(dir);
            return;
        };
        let focused = self.focused;
        let layout = &mut self.ws.active_tab_mut().layout;
        if !layout::swap_panes(layout, focused, target) {
            return;
        }
        // The focused id never changed — it just sits in a different slot —
        // so focus follows the pane with no `set_focus` call (and no
        // spurious CSI O/I pair, per `focus_dir_cross_tab`'s note). What it
        // may need is expanding: swapping *into* a stack can land it in a
        // collapsed slot, the same condition `focus_dir` calls this for.
        layout::expand_in_stacks(layout, focused);
        // No save/relayout here: `apply` does both on the way out, which is
        // also what resizes each pane's PTY to the rect it just moved into
        // (same as `FlipSplit`/`Resize`/`ToggleStack` — none of them persist
        // by hand either).
    }

    /// C38: the shared no-op detector for the three structural chords that
    /// reshape a tab (`Alt+s`, `Alt+o`, `Alt+Shift+arrows`).
    ///
    /// Compares the tree, not the mutator's return value. `resize_pane`
    /// returns `true` at the 0.1/0.9 ratio clamp — "handled, no change" —
    /// so trusting the bool would keep exactly the case a user is most
    /// likely to lean on the key for. Comparing what actually happened is
    /// also the rule that cannot drift as those mutators grow.
    ///
    /// The message distinguishes the two reasons, because they need
    /// different things from the user: a single-pane tab needs a split
    /// (`Alt+n`), while a shape that simply cannot take this verb — a
    /// resize across an axis with no split on it, `Alt+o` on an unsplit
    /// tab — needs a different key.
    fn flash_if_layout_unchanged(&mut self, before: &LayoutNode, verb: &str) {
        if format!("{before:?}") != format!("{:?}", self.ws.active_tab().layout) {
            return;
        }
        if self.ws.active_tab().panes.len() < 2 {
            let split = self.chord_clause(Action::NewPane, |c| format!(" — {c} splits it"));
            self.set_flash(format!("nothing to {verb}: this tab has one pane{split}"));
        } else {
            self.set_flash(format!("nothing to {verb} here"));
        }
    }

    /// C38: what a refused split says. `split_fit` returns `None` when the
    /// pane being split is under `MIN_SPLIT_COLS`×`MIN_SPLIT_ROWS`, which
    /// is also the trigger for the vt100 underflow crash — so the refusal
    /// is right and only the silence was wrong. The message names the
    /// threshold because that is the one fact that makes the next attempt
    /// succeed: "widen it" is advice, "36×10" is an instruction.
    ///
    /// Deliberately raised at the two *keypress* sites (`Alt+n` and a
    /// picker launch), not inside `spawn_child`. The control CLI reaches
    /// the same refusal and already reports it in its own reply; setting a
    /// TUI flash from an API call would put a message on screen that no one
    /// at the keyboard caused.
    fn flash_no_room(&mut self) {
        // `split_fit` picks the axis from the rect's aspect and then tests
        // **that axis only**, so "a pane needs 36×10" was not the guard's
        // condition — a 30×30 pane splits happily into two 30-wide halves.
        // Naming the axis that actually failed, with the number it missed
        // by, is both true and the more useful sentence. Found by the C38
        // design audit; the first draft asserted a rule the code does not
        // have, which is the worst kind of helpful message.
        let rect = self
            .split_target()
            .and_then(|id| self.rects().iter().find(|pr| pr.id == id).map(|pr| pr.rect));
        let detail = match rect {
            Some(r) if r.width >= r.height * 3 => {
                format!(
                    "{} needs {} columns, has {}",
                    "side by side",
                    layout::MIN_SPLIT_COLS,
                    r.width
                )
            }
            Some(r) => {
                format!("stacked needs {} rows, has {}", layout::MIN_SPLIT_ROWS, r.height)
            }
            None => "the pane is too small".to_string(),
        };
        self.set_flash(format!("no room to split — {detail}"));
    }

    /// C38: what `Alt+Shift+hjkl` says when there is nothing that way.
    ///
    /// Horizontally it names the chord that *does* cross tabs, so the dead
    /// end teaches the way out rather than just reporting one — C33
    /// declined the cross-tab handoff precisely because `Alt+Shift+i` /
    /// `Alt+Shift+m` (C28) "already do exactly that job and name it", and
    /// this is where the user is holding the question. Vertically there is
    /// no such chord (tabs are roost's horizontal axis, C31), so the flash
    /// only says what happened.
    ///
    /// The chord comes from `chord_clause`, never a literal: the C34 gate
    /// bans spelling one in `src/`, and a flash that taught a remapped-away
    /// chord would be the exact drift C34 exists to prevent.
    fn flash_move_edge(&mut self, dir: layout::Dir) {
        let (forward, whither) = match dir {
            layout::Dir::Right => (true, "next"),
            layout::Dir::Left => (false, "previous"),
            _ => {
                let whence = if matches!(dir, layout::Dir::Up) { "above" } else { "below" };
                self.set_flash(format!("nothing {whence} to swap with"));
                return;
            }
        };
        // Named only when it would actually work. The clause was guarded on
        // *bound*, which on the default single-tab workspace pointed at a
        // chord that immediately refuses with "only one tab" — a dead end
        // that hands you a second dead end. Found by the C38 design audit.
        let carry = if self.ws.tabs.len() > 1 {
            self.chord_clause(Action::MovePaneToTab { forward }, |c| {
                format!(" — {c} moves it to the {whither} tab")
            })
        } else {
            String::new()
        };
        self.set_flash(format!("at the tab's edge{carry}"));
    }

    /// C31: `Left`/`Right` off the edge of the active tab continue into the
    /// next/previous tab instead of stopping
    /// dead — "keep going right" should behave like it sounds. `Up`/`Down`
    /// are untouched: tabs are roost's own horizontal axis (the strip,
    /// `Alt+i`/`Alt+m`), so only the two keys that already share that axis
    /// pick up tab-switching semantics; giving all four directions that
    /// meaning would surprise a vertical split's Up/Down.
    ///
    /// The landing pane is geometric, read from the *destination* tab's own
    /// computed rects — never `tab_focus` (contrast U11 rule 2, C2): `Right`
    /// lands on the destination's leftmost pane, `Left` on its rightmost
    /// (`edge_pane`) — the edge nearest the one just crossed, matching "keep
    /// going right" rather than wherever that tab was last left. Wraps at
    /// both ends, exactly like `Alt+i`/`Alt+m` (`step_tab`); a no-op below
    /// two tabs.
    ///
    /// Same skeleton as `go_to_tab` — C21 zoom exit, C22 float hide, U11
    /// bookkeeping, lazy spawn — with the target swapped for `edge_pane`'s
    /// pick. `go_to_tab` itself is deliberately not called here: it would
    /// set focus once (to `tab_focus_target`) and this function a second
    /// time right after, and `set_focus` reports a real focus transition on
    /// every call that changes the id — a spurious CSI O/I pair to a pane
    /// that was never truly focused.
    ///
    /// `rects` is the active tab's rects, already computed by the caller —
    /// reused here, not recomputed, to check whether `self.focused` is
    /// *genuinely* at that edge. `neighbor == None` alone isn't enough: a
    /// pane spanning the tab's full width has no Left/Right neighbour
    /// purely because nothing can sit beside a full-width rect
    /// (`layout::neighbor`'s gap check excludes every other pane on both
    /// sides at once, regardless of row) — that's an artifact of its shape,
    /// not evidence it's "the last or first pane" the client asked about. A
    /// pane that owns a whole row above/below a split row is exactly this:
    /// reachable as `Alt+n`, `Alt+o`, `Alt+n` from a single pane. Excluded:
    /// the tab's *only* pane, which necessarily spans the full width (and
    /// height) and is unambiguously both ends at once — the base case every
    /// other C31 test relies on.
    fn focus_dir_cross_tab(&mut self, dir: layout::Dir, rects: &[PaneRect]) {
        let delta: isize = match dir {
            layout::Dir::Right => 1,
            layout::Dir::Left => -1,
            layout::Dir::Up | layout::Dir::Down => return,
        };
        let spans_full_width = rects.len() > 1
            && rects
                .iter()
                .find(|p| p.id == self.focused)
                .is_some_and(|p| p.rect.width == self.body_area().width);
        if spans_full_width {
            // F7: this same key teleports across tabs at a real edge — a
            // silent refusal here reads as broken, not deliberate. roost's
            // own rule is every no-op flashes.
            self.set_flash("full-width pane — nothing to cross into");
            return;
        }
        let n = self.ws.tabs.len();
        if n < 2 {
            // F7: same reasoning as `move_pane_to_tab`'s identical refusal —
            // reuse its wording so the two read as one rule, not two.
            self.set_flash("only one tab");
            return; // C31: only when more than one tab exists
        }
        let next = (self.ws.active_tab as isize + delta).rem_euclid(n as isize) as usize;
        let mut drects = Vec::new();
        layout::compute_rects(&self.ws.tabs[next].layout, self.body_area(), &mut drects);
        // Right enters the destination from its left edge (leftmost pane);
        // Left enters from its right edge (rightmost).
        let Some(target) = edge_pane(&drects, dir == layout::Dir::Left) else { return };

        self.exit_zoom(); // C21: any (real) tab change exits zoom
        self.hide_float(); // C22 rule 2: the float can't actually be shown here (focus_dir's float_focused() guard already returned above if it were) — kept for parity with every other tab switch
        self.remember_tab_focus(); // U11 bookkeeping, before active_tab moves
        self.ws.active_tab = next;
        self.spawn_active_tab();
        self.set_focus(target);
        layout::expand_in_stacks(&mut self.ws.active_tab_mut().layout, target);
    }

    /// C22: the pane a split would actually be taken off — the focused one
    /// when it is in the active tab's tree, else that tab's first pane.
    ///
    /// `self.focused` can be the float, which lives outside the tree, and
    /// splitting "off" an id the tree does not contain is what trips
    /// `split_pane`'s empty-tab fallback and wipes the whole tab's layout.
    /// Shared by `spawn_child` and C38's refusal flash so the message can
    /// never describe a different pane than the one that was refused.
    fn split_target(&self) -> Option<PaneId> {
        if self.ws.active_tab().panes.contains_key(&self.focused) {
            Some(self.focused)
        } else {
            self.pane_order().first().copied()
        }
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
        let split_target = self.split_target()?;
        let cwd = cwd.unwrap_or_else(|| {
            self.ws
                .active_tab()
                .panes
                .get(&split_target)
                .map(|s| s.cwd.clone())
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
        });
        let spec = PaneSpec {
            adapter: adapter.into(),
            cwd,
            session: None,
            title: None,
            spawned_by,
            note: None,
            noted_at: None,
        };

        // Split in the widest direction of the split target's rect. Refuses
        // (returns None) a split that would produce unusably tiny panes
        // (also the trigger for the vt100 underflow crash), leaving the
        // layout untouched. **Not silent any more** (C38): the refusal is
        // reported by whoever asked — `flash_no_room` at the two keypress
        // sites, `ctl_spawn_child`'s own `Reply` for the control plane —
        // and deliberately not from here, so an API call cannot put a
        // message on a screen nobody was typing at.
        let target_rect = self.rects().iter().find(|pr| pr.id == split_target).map(|pr| pr.rect);
        let dir = split_fit(target_rect)?;

        let tab = self.ws.active_tab_mut();
        tab.panes.insert(id, spec.clone());
        if !layout::split_pane(&mut tab.layout, split_target, id, dir) {
            tab.layout = LayoutNode::Pane(id); // empty tab fallback (now only for a genuinely-empty tab)
        }
        self.set_focus(id);
        if let Some(pr) = self.rects().iter().find(|pr| pr.id == id).copied() {
            self.spawn_pane(id, &spec, pr.rect);
        }
        Some(id)
    }

    /// U12 × U22: is pane `id` **mid-turn** — the predicate the destructive
    /// guards (`Alt+w`'s confirm, `Alt+q`'s) arm on?
    ///
    /// `Working || NeedsInput` is U12's answer and still is, but *how roost
    /// learned it* now matters, because the two sources are not equally
    /// strong:
    ///
    /// - **An extension/hook reported it** (`status_reported`) — a real turn
    ///   is in flight, whatever the pane is running. Always guarded.
    /// - **roost inferred it** from PTY traffic — `ACTIVE_WINDOW` (2s) of
    ///   bytes. For an agent that is the best evidence available and roost
    ///   guards on it: a pi/claude pane with no hook installed still deserves
    ///   the confirm. For a **shell** it is not evidence of anything: the
    ///   bytes are usually the echo of the command you just ran, so closing a
    ///   shell within two seconds of your own `ls` armed a confirm and made
    ///   you press `Alt+w` twice. That is U22's report, and it is why the
    ///   guard cost more than it bought there.
    ///
    /// Recorded trade-off: a `shell` running something long (`cargo build`)
    /// now closes on the first press. roost cannot tell that from `ls` —
    /// both are "recent output" — so the choice is between a guard that
    /// fires on every close and one that fires on none, and a guard that
    /// fires constantly is one people learn to double-tap through. The
    /// pane-level undo (`Alt+u`) reopens it, and the *last-pane* and quit
    /// guards are untouched.
    fn mid_turn(&self, id: PaneId) -> bool {
        let Some(rt) = self.runtimes.get(&id) else { return false };
        if !is_busy(rt.status()) {
            return false;
        }
        rt.status_reported() || !self.is_shell(id)
    }

    /// Is pane `id` a plain shell rather than an agent? The `shell` adapter
    /// is the one roost ships that has no turns to be mid-way through.
    fn is_shell(&self, id: PaneId) -> bool {
        self.find_spec(id).is_some_and(|s| s.adapter == "shell")
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
        let is_busy = self.mid_turn(id);
        let would_quit = self.ws.tabs.len() == 1 && self.ws.active_tab().panes.len() == 1;
        let armed = self.confirm_close.is_some_and(|t| t.elapsed() < CONFIRM_WINDOW);
        if (is_busy || would_quit) && !armed {
            self.confirm_close = Some(Instant::now());
            // C34: the chord you are being told to press again is resolved.
            // Unbound (config.json disabled it) drops to "again", which is
            // still true however the repeat arrives — the confirm is armed
            // on the action, not on one key.
            let again = self
                .chord_label(Action::ClosePane)
                .map(|c| format!("{c} again"))
                .unwrap_or_else(|| "again".to_string());
            let msg = if would_quit {
                format!("last pane — {again} to quit roost")
            } else {
                // U2: name which agent the close would interrupt.
                format!("{} busy — {again} to close", self.display_name(id))
            };
            self.set_confirm_flash(msg);
            return;
        }
        self.confirm_close = None;
        self.clear_confirm_flash(); // the arm was consumed; its prompt dies with it (U22)

        // Closing the very last pane is a **quit**, not a close — its own
        // confirm says so ("last pane — {again} to quit roost"), and the
        // control plane refuses the identical call outright rather than
        // quitting for a caller (`ctl_close`). So leave the pane exactly
        // where it is and take Alt+q's door: `shutdown` saves the workspace,
        // `kill_fleet` takes the processes.
        //
        // Removing it first was silent data loss on the one file the whole
        // tool is. `close_pane_id` drops the spec; `layout::remove_pane`
        // cannot drop a bare `Pane` root; and with one tab left there is no
        // tab to drop either — so `shutdown`'s save wrote a tab whose layout
        // still names a pane its `panes` map no longer has. On the next
        // launch `validate_and_repair` mints that id a **fresh** spec:
        // adapter `shell`, `cwd` wherever roost happened to be launched
        // from, and no session, title or note. Quitting through this door
        // therefore threw away the last pane's resumable conversation, its
        // directory and its name; quitting with Alt+q kept all of them. Two
        // doors to "quit roost", one of them losing the thing roost exists
        // to preserve — and losing it quietly, since the repair is silent
        // and the file it repairs looks intact.
        //
        // (`kill_fleet` still kills the pane's process on the way out. What
        // survives is what always survives a quit: the spec.)
        if would_quit {
            self.quit = true;
            return;
        }
        // The actual removal (kill the runtime, capture undo, fix up
        // tab/focus bookkeeping) is close_pane_id's job — shared with the
        // control interface. This wrapper only adds the confirm guard above.
        self.close_pane_id(id);
    }

    /// Alt+q. U1: quitting kills every pane's process at once, so it gets
    /// the same second-press machinery as `close_pane`'s busy guard — while
    /// any pane is busy (`Working || NeedsInput`, the shared U12 predicate),
    /// the first press arms and prompts; the second within the window quits.
    /// A quiet fleet still quits instantly: sessions resume on relaunch, so
    /// there's nothing in flight to protect.
    fn quit_guarded(&mut self) {
        // U22's shell exemption is deliberately **not** applied here, and
        // the asymmetry is the point. `Alt+w` closes one pane you are
        // looking at, having just typed in it — the heuristic fires on your
        // own echo, and `Alt+u` reopens what it closed. `Alt+q` kills the
        // whole fleet, including panes you are not looking at and may have
        // forgotten are running, and nothing reopens that. U1's "a quiet
        // fleet quits instantly, sessions resume on relaunch" is an
        // *agent* argument: pi and claude resume, a shell halfway through a
        // build does not. So a busy shell is exactly the pane a quit should
        // still ask about. One spurious keypress against the session's live
        // work is not a close call.
        let busy = self.runtimes.values().filter(|rt| is_busy(rt.status())).count();
        let armed = self.confirm_quit.is_some_and(|t| t.elapsed() < CONFIRM_WINDOW);
        if busy > 0 && !armed {
            self.confirm_quit = Some(Instant::now());
            let noun = if busy == 1 { "agent" } else { "agents" };
            let again = self
                .chord_label(Action::Quit)
                .map(|c| format!("{c} again"))
                .unwrap_or_else(|| "again".to_string());
            self.set_confirm_flash(format!("{busy} {noun} busy — {again} to quit"));
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
        panes.insert(
            id,
            PaneSpec {
                adapter: "shell".into(),
                cwd,
                session: None,
                title: None,
                spawned_by: None,
                note: None,
                noted_at: None,
            },
        );
        self.ws.tabs.push(Tab {
            name: format!("tab{}", self.ws.tabs.len() + 1),
            layout: LayoutNode::Pane(id),
            panes,
        });
        self.ws.active_tab = self.ws.tabs.len() - 1;
        self.spawn_active_tab();
        self.set_focus(id);
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
            // C38: `Alt+5` on a two-tab workspace is literally C35's case —
            // "a navigation key that silently does nothing reads as broken"
            // — and the digit keys are the most guessed-at chords roost
            // has. The count is the useful half: it says both that the key
            // works and what the legal range is.
            let n = self.ws.tabs.len();
            let plural = if n == 1 { "tab" } else { "tabs" };
            self.set_flash(format!("no tab {}: this workspace has {n} {plural}", i + 1));
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
        let target = self
            .tab_focus_target(i)
            .or_else(|| self.pane_order().first().copied())
            .unwrap_or(self.focused);
        self.set_focus(target);
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

    /// C28: carry the focused pane `delta` tabs (wrapping), and go with it.
    ///
    /// The process is never touched — `runtimes` is keyed by `PaneId` across
    /// the whole workspace, not per tab — so a running agent keeps its PTY,
    /// its scrollback and its session through the move. Only the layout
    /// trees and the spec change hands.
    ///
    /// Refusals, both flashed rather than silent (a no-op you can see beats
    /// one you can't): the C22 float belongs to no tab, and a lone tab has
    /// nowhere to send anything.
    fn move_pane_to_tab(&mut self, delta: isize) {
        if self.float_focused() {
            self.set_flash("the scratch pane belongs to no tab");
            return;
        }
        let n = self.ws.tabs.len();
        if n < 2 {
            self.set_flash("only one tab");
            return;
        }
        let id = self.focused;
        let si = self.ws.active_tab;
        if !self.ws.tabs[si].panes.contains_key(&id) {
            return; // focus is somewhere this action has no business moving
        }
        let di = (si as isize + delta).rem_euclid(n as isize) as usize;
        self.carry_pane_to_tab(id, si, di);
    }

    /// The carry itself, shared by C28's `Alt+Shift+i`/`Alt+Shift+m` and
    /// C40's pull: take `id` out of tab `si` and land it in tab `di`, with
    /// focus. Split out rather than duplicated because the interesting half
    /// is the *refusal* — the fit check, the emptied-source rule and the
    /// index fixup below are the contract, and a second copy of them would
    /// be a second set of edge cases to keep in step.
    ///
    /// Caller's job: `si` really holds `id`, and `si != di`. Answers
    /// whether the pane actually moved — the destination can refuse for
    /// want of room, and C40's mark stands when it does.
    fn carry_pane_to_tab(&mut self, id: PaneId, si: usize, mut di: usize) -> bool {
        // Where the pane lands, and whether it fits — decided *before*
        // anything is removed, so a refusal leaves the workspace untouched
        // rather than stranding the pane between two tabs. The destination
        // is not the active tab, but `compute_rects` is pure and the body
        // area is the same whichever tab is on screen, so this is the real
        // geometry rather than a guess.
        let body = self.body_area();
        let mut drects = Vec::new();
        layout::compute_rects(&self.ws.tabs[di].layout, body, &mut drects);
        let host = self
            .tab_focus_target(di)
            .filter(|h| drects.iter().any(|pr| pr.id == *h))
            .or_else(|| drects.first().map(|pr| pr.id));
        let Some(host) = host else { return false };
        // Same split rule as `spawn_child`: cut the widest way, and refuse
        // below the C25 floor rather than make a pane nothing can render in.
        let host_rect = drects.iter().find(|pr| pr.id == host).map(|pr| pr.rect);
        let Some(dir) = split_fit(host_rect) else {
            self.set_flash(format!("{} has no room", self.ws.tabs[di].name));
            return false;
        };

        // C21/C22: this is a tab change, so it exits zoom and hides the
        // float exactly like `go_to_tab` — done here rather than inherited,
        // since the switch below is hand-rolled (the pane has to leave one
        // tree and join another between the leave and the arrive).
        self.exit_zoom();
        self.hide_float();

        let label = self.feed_label(id);
        let Some(spec) = self.ws.tabs[si].panes.remove(&id) else { return false };
        // U11: the pane that is leaving cannot also be what this tab
        // remembers — coming back falls through to the tab's first pane.
        // (`remember_tab_focus` is deliberately *not* called for the source:
        // there is nothing honest to store when the focused pane departs.)
        self.tab_focus.remove(&id);
        let source_emptied = layout::remove_pane(&mut self.ws.tabs[si].layout, id);
        if source_emptied {
            // Moving a tab's last pane away removes the tab — the same rule
            // `close_pane_id` follows, and for the same reason: a tab with
            // no panes is not a thing roost can draw. No undo record: the
            // pane is alive in the destination, and resurrecting the tab
            // around it would duplicate it.
            let name = self.ws.tabs[si].name.clone();
            self.ws.tabs.remove(si);
            if si < di {
                di -= 1; // every later tab shifted down one
            }
            self.push_feed(format!("closed tab {name}"), false, None);
        }

        let dest = &mut self.ws.tabs[di];
        let dest_name = dest.name.clone();
        dest.panes.insert(id, spec);
        if !layout::split_pane(&mut dest.layout, host, id, dir) {
            dest.layout = LayoutNode::Pane(id); // the empty-tab fallback `spawn_child` keeps
        }
        self.ws.active_tab = di;
        // The destination may never have been visited (lazy spawn), so its
        // *other* panes come alive here — the moved one already has its
        // runtime and `spawn_active_tab` skips it.
        self.spawn_active_tab();
        self.set_focus(id);
        self.push_feed(format!("moved {label} to {dest_name}"), false, None);
        self.relayout();
        self.save();
        true
    }

    /// C40: mark the focused pane for a later pull, or unmark it if it is
    /// already the marked one — one chord both ways, like every other
    /// toggle roost binds.
    ///
    /// Nothing moves here, and the flash says so by naming the chord that
    /// finishes the gesture (resolved, never spelled — C34). The float
    /// refuses on C28's exact wording: it belongs to no tab, so there is no
    /// tab to pull it out of.
    fn mark_pane(&mut self) {
        if self.float_focused() {
            self.set_flash("the scratch pane belongs to no tab");
            return;
        }
        let id = self.focused;
        if self.marked == Some(id) {
            self.marked = None;
            self.set_flash(format!("unmarked {}", self.feed_label(id)));
            return;
        }
        self.marked = Some(id);
        let clause = self.chord_clause(Action::PullPane, |c| format!(" — {c} pulls it here"));
        self.set_flash(format!("marked {}{clause}", self.feed_label(id)));
    }

    /// C40: bring the marked pane into the active tab.
    ///
    /// The mark is cleared on every outcome that resolves it — the pull,
    /// "already here", and a pane that has since gone — so the hint bar's
    /// standing "pull marked pane" pair never outlives the gesture. It is
    /// *kept* when the destination refuses for want of room, which is the
    /// one outcome the user can fix and retry.
    fn pull_pane(&mut self) {
        let Some(id) = self.marked else {
            let clause = self.chord_clause(Action::MarkPane, |c| format!(" — {c} marks one"));
            self.set_flash(format!("nothing marked{clause}"));
            return;
        };
        // Gone, or moved into the float — either way there is nothing in a
        // tab to carry, and the mark has outlived its pane.
        let Some(si) = self.tab_of(id) else {
            self.marked = None;
            self.set_flash("the marked pane is gone");
            return;
        };
        let di = self.ws.active_tab;
        if si == di {
            self.marked = None;
            self.set_flash(format!("{} is already in this tab", self.feed_label(id)));
            return;
        }
        // The destination is the tab we are *standing on*, and
        // `carry_pane_to_tab` picks its host through `tab_focus_target` —
        // the remembered set, which `remember_tab_focus` only stamps on the
        // way *out* of a tab. Stale here, so the pull would split whatever
        // pane the tree happens to list first instead of the one the user
        // is looking at. Stamp it first: the pane you are on is where a
        // pull lands, exactly like `Alt+n`.
        self.remember_tab_focus();
        if self.carry_pane_to_tab(id, si, di) {
            self.marked = None;
        }
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
    /// predicate `needs_input_count` uses, so the ring's ◆ pass can never
    /// disagree with the hint bar's advertised N whenever N > 0. Ordered by
    /// (tab index ascending, position within that tab's `pane_order()`); the
    /// float (C22), if needy, is last — `needs_input_count` counts `runtimes`
    /// directly (float included), so it must be too.
    ///
    /// [ux P1-5] `Alt+a` promises "one key jumps there", but a ◆-only ring
    /// keeps that promise only for extension-instrumented panes: without a
    /// hook, `NeedsInput` is reachable solely via the BEL heuristic, so a
    /// quiet turn-end lands on `Waiting`. When the ◆ pass comes back
    /// **empty** (nothing anywhere needs input), the ring falls through to
    /// every pane reading `Waiting` instead — same tab/pane_order/float
    /// shape, so nothing else about a jump changes. An instrumented fleet
    /// is untouched by construction: the fallback only runs once the ◆
    /// pass is empty, so a real ◆ is never skipped in favor of a ○. The
    /// predicate is `display_status`, not the raw runtime status, so a
    /// plain shell sitting at its prompt (P2-10: it has no turn to hand
    /// back) can never pull the ring — only a pane whose `Waiting` actually
    /// means something does.
    ///
    /// F2: `attention_segment` (below) reads this same ring, so the hint
    /// bar's right segment matches what `Alt+a` will actually do in every
    /// case, not just when the ◆ pass is non-empty.
    ///
    /// F3: the fallback pass also drops any pane in `visited_waiting` —
    /// panes focus has already landed on during their current `Waiting`
    /// spell. Without this the fallback never empties: a real ◆ clears
    /// itself the moment you act on it (its own status moves on), but a
    /// quiet `Waiting` pane's status doesn't change just because you looked
    /// at it, so a fleet with nothing but finished turns would turn
    /// `Alt+a` into an endless round-robin. The ◆ pass is untouched — a
    /// real ◆ must never be hidden by a stale visit.
    fn attention_ring(&self) -> Vec<PaneId> {
        let needy = self.status_ring(AgentStatus::NeedsInput);
        if !needy.is_empty() {
            return needy;
        }
        self.status_ring(AgentStatus::Waiting)
            .into_iter()
            .filter(|id| !self.visited_waiting.contains(id))
            .collect()
    }

    /// C9/F2: what the hint bar's right segment
    /// should announce about `Alt+a` — `Some((n, true))` for a real ◆ count
    /// (`needs_input_count`, unchanged wording/style), `Some((n, false))`
    /// for `attention_ring`'s ○ fallback count once none do, `None` only
    /// when `Alt+a` has nowhere to go at all. Reads `attention_ring` itself
    /// rather than recomputing the two passes, so the segment is
    /// structurally the ring's size — the two can never drift apart.
    pub fn attention_segment(&self) -> Option<(usize, bool)> {
        let ring = self.attention_ring();
        if ring.is_empty() {
            return None;
        }
        Some((ring.len(), self.needs_input_count() > 0))
    }

    /// Every pane — tab order, then that tab's `pane_order()`, the float
    /// (C22) last — whose `display_status` is exactly `status`. The shared
    /// shape behind `attention_ring`'s two passes (◆, then the ○ fallback).
    fn status_ring(&self, status: AgentStatus) -> Vec<PaneId> {
        let mut ring = Vec::new();
        for tab in &self.ws.tabs {
            let mut order = Vec::new();
            layout::pane_order(&tab.layout, &mut order);
            ring.extend(order.into_iter().filter(|&id| self.display_status(id) == Some(status)));
        }
        if let Some(f) = &self.float {
            if self.display_status(f.id) == Some(status) {
                ring.push(f.id);
            }
        }
        ring
    }

    /// C19: the ring member `Alt+a` would move to from where focus is now —
    /// the first one strictly after the focused pane, wrapping past the end
    /// back to `R[0]`; a focused pane that isn't in the ring gets `R[0]`
    /// itself. `None` only when nothing needs attention.
    ///
    /// Split out of `jump_attention` for C27: the roster's opening cursor is
    /// *defined* as "the pane Alt+a would pick", so both must read one
    /// function or the two chords would eventually disagree about it.
    fn attention_next(&self) -> Option<PaneId> {
        let ring = self.attention_ring();
        if ring.is_empty() {
            return None;
        }
        Some(match ring.iter().position(|&id| id == self.focused) {
            Some(k) => ring[(k + 1) % ring.len()],
            None => ring[0],
        })
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
        let Some(next) = self.attention_next() else {
            self.set_flash("nothing needs you");
            return;
        };
        if next == self.focused {
            // The only ring member is the pane we're already on.
            self.set_flash("nothing else needs you");
            return;
        }
        self.focus_attention_target(next);
    }

    /// C36 (`Alt+'`): open the broadcast composer, or close it if it is
    /// already open (C24b's uniform mode-entry toggle).
    fn toggle_broadcast(&mut self) {
        self.mode = match self.mode {
            Mode::Broadcast { .. } => Mode::Normal,
            _ => {
                Mode::Broadcast { lines: vec![String::new()], row: 0, col: 0, status_filter: None }
            }
        };
    }

    /// C36: deliver the composed message to every pane the filter selects.
    ///
    /// `--enter` semantics deliberately: a broadcast that only *types* into
    /// five agents and leaves each prompt sitting there un-submitted has
    /// done the tedious half of the job and left the half you opened it for.
    /// `roost send --all` makes submitting opt-in because a script may want
    /// to stage text; a human who just typed a message and pressed Enter has
    /// already expressed the intent.
    ///
    /// Audited as `Actor::Local`, so `control.log` and the C20 feed record a
    /// keyboard broadcast as its own thing rather than as a token holder's.
    fn send_broadcast(&mut self, text: &str, status: Option<AgentStatus>) {
        if text.is_empty() {
            self.set_flash("nothing to broadcast");
            return;
        }
        let targets = self.broadcast_targets(Actor::Local, status);
        if targets.is_empty() {
            self.set_flash("no panes to broadcast to");
            return;
        }
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(b'\r');
        let sent = self.deliver_broadcast(&targets, &bytes);
        let count = sent.len();
        let summary = method_summary(&Method::Broadcast { text: text.to_string(), submit: true });
        self.audit(Some(Actor::Local), &summary, true, &format!("count={count}"));
        let noun = if count == 1 { "pane" } else { "panes" };
        self.set_flash(format!("broadcast to {count} {noun}"));
    }

    /// C35 (`Alt+;`): return to the pane focus was on before this one.
    ///
    /// The motion `Alt+a` cannot make. Every other navigation chord is
    /// absolute (`Alt+1..9`) or forward-directional (`Alt+a`, `Alt+m`,
    /// `Alt+hjkl`); at fleet scale the dominant move is "check on B, come
    /// back to A", and roost had no key for the second half. tmux has had
    /// `prefix ;` for last-pane since forever, vim has `Ctrl-^`.
    ///
    /// Reuses `focus_attention_target`, so a cross-tab return switches tabs,
    /// expands a collapsed stack member and handles a float exactly as
    /// `Alt+a`'s jump does — "which pane is that" and "how do I get there"
    /// stay one answer each.
    ///
    /// A closed alternate flashes rather than no-oping silently: the whole
    /// value of the chord is confidence about where it lands, so "the pane
    /// you'd go back to is gone" is information, not noise.
    fn focus_alternate(&mut self) {
        let Some(target) = self.alternate else {
            self.set_flash("no pane to go back to yet");
            return;
        };
        if !self.pane_exists(target) {
            self.alternate = None;
            self.set_flash("the pane you were on has closed");
            return;
        }
        if target == self.focused {
            return;
        }
        self.focus_attention_target(target);
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
        self.set_focus(target);
        layout::expand_in_stacks(&mut self.ws.active_tab_mut().layout, target);
    }

    /// Alt+g: step the active tab through the three canned arrangements,
    /// skipping ones that don't fit the current body area. No-ops (without
    /// advancing the cycle counter) when there's nothing to arrange or
    /// nothing fits.
    /// **[C37, 2026-08-19]** `forward` is false for `Alt+Shift+g`, which
    /// walks the same cycle backwards.
    ///
    /// `layout_cycle` is the arrangement to try *next going forward*, so
    /// what is showing is `layout_cycle - 1` and stepping back means
    /// starting two behind it. The `1 - step` below is exactly that:
    /// candidates `lc+1, lc, lc-1` — the forward order reversed, with the
    /// same skip-what-doesn't-fit behaviour, so a shape the terminal is too
    /// small for is passed over in both directions alike.
    fn cycle_layout(&mut self, forward: bool) {
        let order = self.pane_order();
        if order.len() < 2 {
            self.set_flash("one pane — nothing to arrange");
            return;
        }
        let focused = self.focused;
        let area = self.body_area();
        for step in 0..3 {
            let offset = if forward { step } else { 1 - step };
            let idx = (self.layout_cycle as isize + offset).rem_euclid(3) as usize;
            let node = arrangement_for(idx, &order, focused);
            if layout::arrangement_fits(&node, area) {
                self.ws.active_tab_mut().layout = node;
                self.layout_cycle = (idx + 1) % 3;
                return;
            }
        }
        let hint =
            self.chord_clause(Action::ToggleStack, |c| format!("; stack a pane with {c} first"));
        self.set_flash(format!("no room to rearrange{hint}"));
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

    /// C20/C27: the feed and roster overlays' shared paging step — half the
    /// (shared) overlay height, at least one row/entry. The single source
    /// for both keyboards' PgUp/PgDn and the feed's wheel notch.
    fn overlay_page(&self) -> usize {
        (feed_overlay_size(self.body_area()).1 / 2).max(1) as usize
    }

    // ---- C27: the fleet roster ------------------------------------------

    /// C27: open the roster on the pane `Alt+a` would jump to, or close it if
    /// it is already up (U18's rule: a mode's entry chord exits it — the
    /// generic path in `handle_mode_key` handles the close, this stays
    /// honest either way).
    ///
    /// The opening cursor is load-bearing: landing it on `attention_next`
    /// makes `Alt+Shift+a` `Enter` land on the pane `Alt+a` would *jump* to,
    /// so the roster is a superset of the chord users already know rather
    /// than a competing command. With nothing needing attention it lands on
    /// the focused pane — the roster then opens where you already are.
    ///
    /// One case is deliberately not equivalent: with the float focused,
    /// `Alt+a` dismisses the float instead of jumping (C22 rule 2), while
    /// the roster still opens on `attention_next`. The roster's job is to
    /// show you the fleet, and there is no row that means "hide this".
    fn toggle_roster(&mut self) {
        if matches!(self.mode, Mode::Roster { .. }) {
            self.mode = Mode::Normal;
            return;
        }
        let cursor = self.attention_next().unwrap_or(self.focused);
        self.mode = Mode::Roster { cursor, filter: String::new(), top: 0, status_filter: None };
        self.roster_reveal();
    }

    /// C27: every row the roster shows right now — each tab's header
    /// followed by that tab's panes, the float last. The float's group is
    /// never reordered (C19's ring puts it last regardless of urgency too —
    /// C22's own placement, unchanged); the float is listed whether or not it
    /// is *shown*, because C19's ring carries a hidden float that needs you,
    /// so a roster that hid it could open with its cursor on a row that isn't
    /// there (no `❯` drawn anywhere, Enter acting on something invisible).
    /// Hidden is a display state, not an absence; Enter on the row shows it,
    /// exactly as a ring jump does.
    ///
    /// Tab order is not the sort key. Both tiers sort worst-first
    /// (`roster_rank`, C5's severity order: ◆→○→⠋→·→exited — ⠋ standing for
    /// the Working spinner): panes within a tab's group, and then the
    /// groups themselves by their
    /// own worst pane — so the tab holding the one ◆ in a twenty-pane fleet
    /// is the *first* group, and that pane is its *first* row, not something
    /// you scroll a screenful to find. Both sorts are stable, so panes/tabs
    /// tied on rank keep the ring's own (tab index, `pane_order()`) order —
    /// C19 and the roster still agree on tie-order, just not on row order
    /// once severities mix. `display_status`, not the raw status (P2-10): a
    /// quiet shell ranks with Idle, not Waiting.
    ///
    /// The type-ahead filter and the status filter both apply to panes, AND
    /// composed (a row must satisfy both); a group whose panes all filter out
    /// (by either) drops its header too (an empty group is a row that says
    /// nothing).
    pub fn roster_rows(&self) -> Vec<RosterRow> {
        let (filter, status_filter) = match &self.mode {
            Mode::Roster { filter, status_filter, .. } => {
                (filter.to_ascii_lowercase(), *status_filter)
            }
            _ => (String::new(), None),
        };
        // [design-supervisor D2] Compared by `roster_rank`, not equality: a
        // never-started pane (`None`) ranks with `Idle` for the *sort*, and
        // the filter has to agree — otherwise a pane that draws `·` and
        // sorts in the `·` band could vanish under the `·` filter, which is
        // exactly the never-spawned-tab case (C27's headline scenario) that
        // a restored workspace hits on its very first roster open.
        let shows = |id: PaneId| {
            self.roster_matches(id, &filter)
                && status_filter
                    .is_none_or(|s| roster_rank(self.display_status(id)) == roster_rank(Some(s)))
        };

        let mut groups: Vec<(u8, usize, Vec<PaneId>)> = Vec::new();
        for (i, tab) in self.ws.tabs.iter().enumerate() {
            let mut order = Vec::new();
            layout::pane_order(&tab.layout, &mut order);
            let mut panes: Vec<PaneId> = order.into_iter().filter(|&id| shows(id)).collect();
            if panes.is_empty() {
                continue;
            }
            panes.sort_by_key(|&id| roster_rank(self.display_status(id)));
            let rank = roster_rank(self.display_status(panes[0])); // panes[0] is worst after the sort
            groups.push((rank, i, panes));
        }
        groups.sort_by_key(|&(rank, i, _)| (rank, i));

        let mut rows = Vec::new();
        for (_, i, panes) in groups {
            let name = &self.ws.tabs[i].name;
            rows.push(RosterRow::Group { label: roster_group_label(i, name, panes.len()) });
            rows.extend(panes.into_iter().map(|id| RosterRow::Pane { id }));
        }
        if let Some(f) = &self.float {
            if shows(f.id) {
                rows.push(RosterRow::Group { label: roster_float_label() });
                rows.push(RosterRow::Pane { id: f.id });
            }
        }
        rows
    }

    /// C27: does pane `id` match the (already lowercased) type-ahead query?
    /// Matched against the three things a row shows — the pane id, its
    /// display name and its adapter — as an ASCII-case-insensitive
    /// substring, the same matching rule U20 gave the picker. An empty query
    /// matches everything, so the un-filtered roster is the zero-keystroke
    /// case of this one.
    fn roster_matches(&self, id: PaneId, filter: &str) -> bool {
        if filter.is_empty() {
            return true;
        }
        let adapter = self.find_spec(id).map(|s| s.adapter.clone()).unwrap_or_default();
        let hay = format!("{id} {} {adapter}", self.display_name(id)).to_ascii_lowercase();
        hay.contains(filter)
    }

    /// C27/U18: cycle the roster's status filter — `Tab` forward, `Shift+Tab`
    /// back — through `ROSTER_STATUS_CYCLE` (worst-first, `None` = every
    /// tier, at both ends so cycling never dead-ends). Reuses
    /// `roster_clamp_cursor` (U20's own picker-filter idiom): a cursor
    /// filtered away lands on the new list's first row, and the view scrolls
    /// to keep it visible.
    fn roster_cycle_status(&mut self, delta: isize) {
        let Mode::Roster { status_filter, .. } = &mut self.mode else { return };
        *status_filter = cycle_status_filter(*status_filter, delta < 0);
        self.roster_clamp_cursor();
    }

    /// C27: the roster overlay's visible row window — `top` clamped against
    /// the rows there actually are and the height there actually is. The one
    /// computation the renderer and the mouse hit-test share, so a click can
    /// never land on a different row than the one under the pointer
    /// (§4/§5 lockstep).
    pub fn roster_view(&self) -> (Vec<RosterRow>, usize) {
        let rows = self.roster_rows();
        let top = match &self.mode {
            Mode::Roster { top, .. } => *top,
            _ => 0,
        };
        let top = roster_top_clamped(top, rows.len(), self.roster_view_rows());
        (rows, top)
    }

    /// How many rows the overlay can show at once (its inner height).
    fn roster_view_rows(&self) -> usize {
        feed_overlay_size(self.body_area()).1.saturating_sub(2) as usize
    }

    /// C15: the keymap's paging step — half its visible height, at least one
    /// row. Same rule as C20/C27's shared `overlay_page`.
    fn help_page(&self) -> isize {
        let q = self.help_filter().map(str::to_string);
        (crate::ui::render::help_scroll_extent(self.body_area(), &self.keymap, q.as_deref()).0 / 2)
            .max(1) as isize
    }

    /// C15: move the keymap's window `delta` rows, clamped to the table.
    /// `isize::MIN`/`MAX` mean Home/End.
    ///
    /// Returns **whether it moved** — that is the whole contract with
    /// `handle_mode_key`: a scroll key that has nowhere to go is not a
    /// scroll key, so it falls through to "any key closes it" and the
    /// amendment stays invisible on any terminal that shows the whole table.
    /// [F9] The live type-ahead query, or `None` while the filter is closed.
    /// The distinction matters to callers too: `None` is "the overlay in its
    /// original state", which is what keeps the amendment invisible.
    pub fn help_filter(&self) -> Option<&str> {
        match &self.mode {
            Mode::Help { filter, .. } => filter.as_deref(),
            _ => None,
        }
    }

    fn help_scroll(&mut self, delta: isize) -> bool {
        let q = self.help_filter().map(str::to_string);
        let (visible, total) =
            crate::ui::render::help_scroll_extent(self.body_area(), &self.keymap, q.as_deref());
        let max = total.saturating_sub(visible) as isize;
        let Mode::Help { top, .. } = &mut self.mode else { return false };
        let next = match delta {
            isize::MIN => 0,
            isize::MAX => max,
            d => (*top as isize + d).clamp(0, max),
        } as usize;
        let moved = next != *top;
        *top = next;
        moved
    }

    /// The cursor's index among the rows currently shown, if it is still one
    /// of them (a pane can exit, close, or be filtered away underneath it).
    fn roster_cursor_index(&self, rows: &[RosterRow]) -> Option<usize> {
        let Mode::Roster { cursor, .. } = &self.mode else { return None };
        rows.iter().position(|r| matches!(r, RosterRow::Pane { id } if id == cursor))
    }

    /// Move the cursor `delta` pane rows (negative = up), skipping group
    /// headers — they are labels, not destinations. Clamped at both ends
    /// rather than wrapping: the roster is a map of the fleet, and a cursor
    /// that teleported from the last pane to the first on an overshoot would
    /// make a held key a hazard (same rule C13's caret follows).
    fn roster_move(&mut self, delta: isize) {
        let rows = self.roster_rows();
        let panes: Vec<PaneId> = rows
            .iter()
            .filter_map(|r| match r {
                RosterRow::Pane { id } => Some(*id),
                RosterRow::Group { .. } => None,
            })
            .collect();
        if panes.is_empty() {
            return;
        }
        let Mode::Roster { cursor, .. } = &self.mode else { return };
        let at = panes.iter().position(|id| id == cursor).unwrap_or(0) as isize;
        let next = (at + delta).clamp(0, panes.len() as isize - 1) as usize;
        let id = panes[next];
        if let Mode::Roster { cursor, .. } = &mut self.mode {
            *cursor = id;
        }
        self.roster_reveal();
    }

    /// Scroll the window the least it can to keep the cursor on screen (and
    /// clamp it against a list that shrank while the roster was open).
    fn roster_reveal(&mut self) {
        let rows = self.roster_rows();
        let height = self.roster_view_rows();
        let len = rows.len();
        let at = self.roster_cursor_index(&rows);
        // Scrolling up onto the first pane of a group takes that group's
        // header with it: a row whose tab you cannot see is a pane you cannot
        // place, and the header costs one row exactly when there is one to
        // spare (the cursor is at the top of the window either way).
        let reveal = match at {
            Some(at) if at > 0 && matches!(rows[at - 1], RosterRow::Group { .. }) => Some(at - 1),
            other => other,
        };
        if let Mode::Roster { top, .. } = &mut self.mode {
            let mut t = roster_top_clamped(*top, len, height);
            if let (Some(at), Some(reveal)) = (at, reveal) {
                if reveal < t {
                    t = reveal;
                } else if height > 0 && at >= t + height {
                    t = at + 1 - height;
                }
            }
            *top = t;
        }
    }

    /// Put the cursor back on a real row after the filter changed under it —
    /// the first pane still shown. A filter that narrowed the list to
    /// nothing leaves the cursor alone (there is nothing to point at, and
    /// widening the query brings it back).
    fn roster_clamp_cursor(&mut self) {
        let rows = self.roster_rows();
        if self.roster_cursor_index(&rows).is_none() {
            let first = rows.iter().find_map(|r| match r {
                RosterRow::Pane { id } => Some(*id),
                RosterRow::Group { .. } => None,
            });
            if let (Some(id), Mode::Roster { cursor, .. }) = (first, &mut self.mode) {
                *cursor = id;
            }
        }
        self.roster_reveal();
    }

    /// C27's only action: go to a pane. Through `focus_attention_target` —
    /// the helper C19's ring and C20's Enter already share — so a roster jump
    /// switches tabs, expands collapsed stacks and shows the float exactly
    /// like every other jump in roost. Ids are recycled, so a pane that
    /// closed while the roster was open flashes instead of moving focus.
    fn roster_jump(&mut self, id: PaneId) {
        if self.find_spec(id).is_none() {
            self.set_flash("that pane is gone");
            return;
        }
        self.mode = Mode::Normal;
        self.focus_attention_target(id);
    }

    /// C14: launch the picker's item `index` — leave the modal, then spawn
    /// it with the same C21/C22 pre-steps a picker launch has always had.
    /// Shared by the keyboard's Enter and U8's click-a-row path so the two
    /// can't drift.
    fn picker_launch(&mut self, index: usize) {
        // U20: `index` addresses the rows the user can actually see — the
        // *filtered* list — so the accelerators, the click and Enter all
        // agree with what is drawn even mid-type-ahead. An out-of-range
        // index (a digit past the end of a narrowed list) launches nothing
        // and leaves the picker up: the rows carry their own numbers, so it
        // is self-evidently out of range.
        //
        // P2-13: addressed through the *raw* ids, not `picker_filtered`'s
        // annotated display text — a not-installed row still reads as
        // "codex not found" in the list, and launching has to spawn `codex`,
        // not a string with a marker glued onto it.
        let ids = self.picker_filtered_ids();
        let Some(&adapter) = ids.get(index) else { return };
        let cwd = self.picker_cwd();
        self.mode = Mode::Normal;
        self.exit_zoom(); // C21: "picker launch" is a structural action
        self.hide_float(); // C22 rule 3: ditto
        if self.spawn_child(adapter, cwd.clone(), None).is_none() {
            self.flash_no_room();
        }
        // Launching into a directory is the strongest evidence it is one you
        // are working in — float it to the top of the column for next time.
        if let Some(cwd) = cwd {
            self.note_cwd(cwd);
        }
        self.relayout();
        self.save();
    }

    /// U20: the RAW adapter ids the picker is currently showing — every
    /// adapter whose id contains the type-ahead query, in registry order. An
    /// empty query shows everything, so the pre-U20 picker is the
    /// zero-keystroke case of this one. Matching is ASCII-case-insensitive
    /// substring, not prefix: `laud` should find `claude`, because a picker
    /// that only honors prefixes makes you know the list you came to read.
    ///
    /// What `picker_launch` addresses by index; `picker_filtered` is the
    /// annotated *display* text derived from this exact same list, so the
    /// two can never disagree about which row is which adapter.
    fn picker_filtered_ids(&self) -> Vec<&'static str> {
        let filter = match &self.mode {
            Mode::Picker { filter, .. } => filter.to_ascii_lowercase(),
            _ => String::new(),
        };
        crate::agents::picker_ids()
            .into_iter()
            .filter(|id| filter.is_empty() || id.to_ascii_lowercase().contains(&filter))
            .collect()
    }

    /// U20 / P2-13: the picker rows as display text — `picker_filtered_ids`,
    /// each annotated when its launch program isn't actually on `$PATH`.
    /// Annotated rather than hidden: this registry has six adapters, and on
    /// a single-agent machine hiding the other five would leave one or two
    /// rows under a title that still reads "pick agent" — confusingly
    /// short, and indistinguishable from a picker that lost its adapters
    /// (the exact failure U20's own type-ahead title already guards
    /// against).
    ///
    /// F8: the suffix is `" not found"` (the familiar shell idiom for "no
    /// such program on `$PATH`"), not "gone" — "gone" implies something
    /// that *was* here and disappeared, which is false for an adapter never
    /// installed on this machine at all.
    ///
    /// It is `PICKER_MISSING_SUFFIX` rather than a literal because C14's
    /// `ADAPTER_COL` is sized for *this string on the longest registry id*,
    /// and the gate that checks that has to build the same worst case. Two
    /// spellings of one suffix is how the column gets sized for a row the
    /// picker no longer draws — the ADAPTER_COL bug one level up.
    pub fn picker_filtered(&self) -> Vec<String> {
        self.picker_filtered_ids()
            .into_iter()
            .map(|id| {
                if adapter_installed(id, &self.registry) {
                    id.to_string()
                } else {
                    format!("{id} {PICKER_MISSING_SUFFIX}")
                }
            })
            .collect()
    }

    /// U20 / DESIGN.md §7: the recent-directory column, most recent first.
    pub fn picker_cwds(&self) -> &[PathBuf] {
        &self.recent_cwds
    }

    /// The directory the picker would launch into right now — the selected
    /// row of the cwd column, or `None` (inherit the focused pane's, the
    /// pre-U20 behavior) when there is nothing to select.
    fn picker_cwd(&self) -> Option<PathBuf> {
        let Mode::Picker { cwd, .. } = &self.mode else { return None };
        self.recent_cwds.get(*cwd).cloned()
    }

    /// U20: record `cwd` as the most recently used directory — move-to-front,
    /// deduplicated, capped. The cap is `MAX_RECENT_CWDS` because the column
    /// is a *list you read at a glance* inside a modal, not an archive; past
    /// that a directory is faster to reach by typing it in a shell.
    fn note_cwd(&mut self, cwd: PathBuf) {
        self.recent_cwds.retain(|c| c != &cwd);
        self.recent_cwds.insert(0, cwd);
        self.recent_cwds.truncate(MAX_RECENT_CWDS);
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
        // The float's id comes out of the borrow before `set_focus` runs:
        // that call needs all of `self` (the report it may send goes to the
        // runtimes map), which `&mut self.float` would still be holding.
        let prev = self.focused;
        let show = self.float.as_mut().map(|f| {
            f.shown = true;
            f.prev_focus = prev;
            f.id
        });
        match show {
            Some(id) => self.set_focus(id),
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
            note: None,
            noted_at: None,
        };
        self.float = Some(Float { id, spec: spec.clone(), shown: true, prev_focus });
        self.set_focus(id);
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
        let target = if self.ws.active_tab().panes.contains_key(&back) {
            back
        } else {
            // prev_focus may have been closed via the control plane while
            // the float was up — fall back to whatever's on screen now,
            // same recovery `close_pane_id` uses.
            self.pane_order().first().copied().unwrap_or(0)
        };
        self.set_focus(target);
    }

    /// C22 rule 4: Alt+w on the float kills it for real and clears the slot
    /// — unlike hiding, no undo entry (scratch is not precious).
    fn close_float(&mut self) {
        let Some(f) = self.float.take() else { return };
        if let Some(mut rt) = self.runtimes.remove(&f.id) {
            rt.kill();
        }
        self.tokens.remove_pane_token(f.id);
        self.ext_link_counts.remove(&f.id); // [F1]
        self.dead.remove(&f.id);
        self.pending_input.remove(&f.id);
        self.raw.remove(&f.id);
        let target = if self.ws.active_tab().panes.contains_key(&f.prev_focus) {
            f.prev_focus
        } else {
            self.pane_order().first().copied().unwrap_or(0)
        };
        self.set_focus(target);
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
        // C29: any keypress dismisses a lingering native selection — its
        // highlight stays lit only until "the next click or keypress" (C29).
        // Gated on `Mode::Normal` specifically (not just "not Copy") so this
        // can never fire *while* Copy mode is managing `self.selection` for
        // its own cursor (by the time that mode is active, `self.mode` is
        // already `Mode::Copy`, not `Normal`) — including the Alt+c keypress
        // that enters it: this runs first, so Copy mode always starts from
        // a clean `self.selection` rather than inheriting a stale highlight.
        if matches!(self.mode, Mode::Normal) && self.selection.is_some() {
            self.selection = None;
        }
        // Alt-chords always reach the global bindings (Alt+q must quit from
        // anywhere), so any mode yields to them. If we were scrolling, snap the
        // pane back to its live tail first — otherwise moving focus (Alt+arrow)
        // would leave the pane you were reading frozen mid-history while scroll
        // keys silently drive a different pane.
        if key.modifiers.contains(crossterm::event::KeyModifiers::ALT) {
            // C32: Alt+Enter *inside the pane editor* is a line break, not
            // a cancel into the quick-launch picker. Terminal.app reports
            // Shift+Enter as exactly these bytes (ESC CR — the README's
            // documented collapse), so on that terminal this is the only
            // spelling of "newline" the dialog can ever receive; on CSI-u
            // terminals it's a synonym for Shift+Enter. No other Alt chord
            // is claimed — the rest still cancel out to the global
            // bindings below, Alt+r's own toggle-off included.
            if key.code == KeyCode::Enter {
                match &mut self.mode {
                    Mode::PaneEdit { lines, row, col, .. } => {
                        pane_edit_break(lines, row, col);
                        return true;
                    }
                    // C36: the composer needs the same carve-out. On
                    // Terminal.app, Shift+Enter arrives as Alt+Enter
                    // (README's CSI-u note), so without this the one chord
                    // that should break a line instead opened the picker
                    // and discarded the composed message. Found by the C36
                    // design audit; C36's "keys are C32's, exactly" was an
                    // overclaim until now.
                    Mode::Broadcast { lines, row, col, .. } => {
                        broadcast_break(lines, row, col);
                        return true;
                    }
                    _ => {}
                }
            }
            // U18: a mode's own entry chord toggles it OFF. C20's Alt+e had
            // this rule special-cased for the feed; every other mode fell
            // through to the global binding and *re-entered* — Alt+c in copy
            // mode threw the cursor and selection away for a fresh one, and
            // Alt+PageUp in scroll mode snapped the view to the live tail and
            // called that "scroll mode". Both read as "the chord did
            // nothing", which is worse than either exiting or staying.
            // Generalized through `translate_with` (config.json-aware; see
            // `self.keymap`) rather than a second chord table, so the
            // toggle can never drift from the binding — including a
            // remapped one.
            if let crate::ui::input::InputResult::Action(a) =
                crate::ui::input::translate_with(key, &self.keymap)
            {
                if Some(a) == mode_entry_action(&self.mode) {
                    self.exit_mode();
                    return true;
                }
            }
            // U9: Alt+c is exempt from the snap below — Scroll→Copy hands
            // the frozen view over intact, because scroll→select→yank is
            // THE keyboard path for copying history (the wheel→Alt+c route
            // already preserved the view; the keyboard route snapped to the
            // live tail at the handoff, so history could never be yanked by
            // keyboard). Same special-case shape as Alt+e above; the chord
            // still falls through to the global binding, which enters Copy.
            //
            // [P21] The search prompt is a Scroll-mode surface and rides the
            // same two rules: its view snaps back on the way out, and the
            // Alt+c handoff keeps it — a search *is* how you find the text
            // you are about to select, so throwing the hits away at the
            // handoff would break the flow the search exists to serve.
            let looking_back = matches!(self.mode, Mode::Scroll | Mode::Search { .. });
            let scroll_to_copy = looking_back && key.code == KeyCode::Char('c');
            if looking_back && !scroll_to_copy {
                let focused = self.focused;
                if let Some(rt) = self.runtimes.get_mut(&focused) {
                    rt.set_scrollback(0);
                }
            }
            if !scroll_to_copy {
                self.search = None;
            }
            self.mode = Mode::Normal;
            return false;
        }
        // C20: half the feed overlay's own height, for PgUp/PgDn — computed
        // here (needs a whole `&self` via `body_area()`) rather than inside
        // the match below, where `Mode::Feed`'s arm already holds
        // `self.mode` mutably borrowed.
        let feed_page = self.overlay_page();
        // C24: the focused pane's current inner grid, for clamping the copy
        // cursor — same borrow-ordering reason as `feed_page` above.
        let (copy_h, copy_w) = self.focused_inner_dims();
        // U17: the text of the row the copy cursor sits on, for the w/b/e
        // word motions. Same borrow-ordering reason again — and computed
        // only for the three keys that need it, so an ordinary hjkl press
        // still costs nothing.
        let copy_row = match (&self.mode, key.code) {
            (Mode::Copy { cursor }, KeyCode::Char('w' | 'b' | 'e')) => {
                self.row_text(self.focused, cursor.0)
            }
            _ => String::new(),
        };
        // P21: the two look-back modes share three search keys. They live
        // ahead of the `match &mut self.mode` below because each one needs
        // the whole `&mut self` — the pane's history, its runtime, the
        // mode itself — which the arm-local borrows down there rule out.
        // Copy mode hands its cursor along so Enter/Esc can put it back;
        // Scroll mode passes None. Neither `n`, `N` nor `/` was bound in
        // either mode before, so nothing is displaced.
        let look_back = match self.mode {
            Mode::Scroll => Some(None),
            Mode::Copy { cursor } => Some(Some(cursor)),
            _ => None,
        };
        if let Some(copy_cursor) = look_back {
            match key.code {
                KeyCode::Char('/') => {
                    self.open_search(copy_cursor);
                    return true;
                }
                KeyCode::Char('n') => {
                    self.step_match(true);
                    return true;
                }
                KeyCode::Char('N') => {
                    self.step_match(false);
                    return true;
                }
                _ => {}
            }
        }
        match &mut self.mode {
            Mode::Normal => false,
            Mode::Rename { buffer, cursor, target } => {
                let target = *target;
                // U16: only Ctrl+W/Ctrl+U are real edits here — the two
                // every line editor on the platform binds — every *other*
                // modified char is discarded: a chord roost doesn't
                // implement must never leave its letter behind in a name.
                // Every edit is relative to `cursor`, the point, which is
                // what makes Ctrl+W's readline name ("word behind point")
                // honest.
                let ctrl = key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL);
                let len = buffer.chars().count();
                *cursor = (*cursor).min(len);
                match key.code {
                    // Ctrl+U — readline's unix-line-discard: kill from the
                    // point back to the start, keeping the tail. (It cleared
                    // the whole buffer before, which was the same thing when
                    // the point was always the end.)
                    KeyCode::Char('u') if ctrl => {
                        *buffer = buffer[byte_at(buffer, *cursor)..].to_string();
                        *cursor = 0;
                    }
                    // Ctrl+W — readline's unix-word-rubout, over the text
                    // behind the point only; whatever follows it survives.
                    KeyCode::Char('w') if ctrl => {
                        let at = byte_at(buffer, *cursor);
                        let head = erase_word(&buffer[..at]);
                        *cursor = head.chars().count();
                        *buffer = head + &buffer[at..];
                    }
                    // Shift is how an uppercase letter arrives (kitty CSI-u
                    // spells `A` as Shift+`a`), so it is the one modifier a
                    // plain insert may carry.
                    KeyCode::Char(c)
                        if key
                            .modifiers
                            .difference(crossterm::event::KeyModifiers::SHIFT)
                            .is_empty() =>
                    {
                        buffer.insert(byte_at(buffer, *cursor), c);
                        *cursor += 1;
                    }
                    KeyCode::Char(_) => {} // any other chord: swallowed, not typed
                    // Backspace eats the char *behind* the point, Delete the
                    // one under it — the split every text field makes, and
                    // impossible to offer before there was a point.
                    KeyCode::Backspace => {
                        if *cursor > 0 {
                            *cursor -= 1;
                            buffer.remove(byte_at(buffer, *cursor));
                        }
                    }
                    KeyCode::Delete => {
                        if *cursor < len {
                            buffer.remove(byte_at(buffer, *cursor));
                        }
                    }
                    KeyCode::Left => *cursor = cursor.saturating_sub(1),
                    KeyCode::Right => *cursor = (*cursor + 1).min(len),
                    KeyCode::Home => *cursor = 0,
                    KeyCode::End => *cursor = len,
                    KeyCode::Enter => {
                        let text = buffer.trim().to_string();
                        // Tab is the one Rename target left (C32 absorbed
                        // pane renames). A tab always needs a name; ignore
                        // an empty one.
                        let RenameTarget::Tab = target;
                        if !text.is_empty() {
                            self.ws.active_tab_mut().name = text;
                        }
                        self.save();
                        self.mode = Mode::Normal;
                    }
                    KeyCode::Esc => self.mode = Mode::Normal,
                    _ => {}
                }
                true
            }
            Mode::Broadcast { lines, row, col, status_filter } => {
                // C36: C32's editing vocabulary over a plain line buffer —
                // no name row, so `row` indexes `lines` directly and the
                // seam rules PaneEdit needs don't apply. Enter *sends*
                // (Shift/Ctrl+Enter breaks a line, C32's convention), Tab
                // cycles the target filter, Esc walks away having sent
                // nothing.
                let ctrl = key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL);
                let shift = key.modifiers.contains(crossterm::event::KeyModifiers::SHIFT);
                *row = (*row).min(lines.len().saturating_sub(1));
                let len = lines[*row].chars().count();
                *col = (*col).min(len);
                match key.code {
                    KeyCode::Enter if shift || ctrl => {
                        broadcast_break(lines, row, col);
                    }
                    KeyCode::Enter => {
                        let text = lines.join("\n").trim().to_string();
                        let filter = *status_filter;
                        self.mode = Mode::Normal;
                        self.send_broadcast(&text, filter);
                    }
                    // C27's idiom: Tab cycles which statuses are targeted,
                    // and the title's count moves with it — so the filter
                    // is legible as "who gets this", not as a setting.
                    KeyCode::Tab | KeyCode::BackTab => {
                        let back = matches!(key.code, KeyCode::BackTab);
                        // `deliverable_only`: the composer skips `Exited`,
                        // which `broadcast_targets` filters out anyway — see
                        // `step_status_filter`. The roster keeps it.
                        *status_filter = step_status_filter(*status_filter, back, true);
                    }
                    KeyCode::Char('u') if ctrl => {
                        let cur = &mut lines[*row];
                        *cur = cur[byte_at(cur, *col)..].to_string();
                        *col = 0;
                    }
                    KeyCode::Char('w') if ctrl => {
                        let cur = &mut lines[*row];
                        let at = byte_at(cur, *col);
                        let head = erase_word(&cur[..at]);
                        *col = head.chars().count();
                        *cur = head + &cur[at..];
                    }
                    KeyCode::Char(c)
                        if key
                            .modifiers
                            .difference(crossterm::event::KeyModifiers::SHIFT)
                            .is_empty() =>
                    {
                        let cur = &mut lines[*row];
                        let at = byte_at(cur, *col);
                        cur.insert(at, c);
                        *col += 1;
                    }
                    KeyCode::Char(_) => {} // any other chord: swallowed, not typed
                    KeyCode::Backspace => {
                        if *col > 0 {
                            *col -= 1;
                            let cur = &mut lines[*row];
                            let at = byte_at(cur, *col);
                            cur.remove(at);
                        } else if *row > 0 {
                            let cur = lines.remove(*row);
                            *row -= 1;
                            *col = lines[*row].chars().count();
                            lines[*row].push_str(&cur);
                        }
                    }
                    KeyCode::Delete => {
                        if *col < len {
                            let cur = &mut lines[*row];
                            let at = byte_at(cur, *col);
                            cur.remove(at);
                        } else if *row + 1 < lines.len() {
                            let next = lines.remove(*row + 1);
                            lines[*row].push_str(&next);
                        }
                    }
                    KeyCode::Left => {
                        if *col > 0 {
                            *col -= 1;
                        } else if *row > 0 {
                            *row -= 1;
                            *col = lines[*row].chars().count();
                        }
                    }
                    KeyCode::Right => {
                        if *col < len {
                            *col += 1;
                        } else if *row + 1 < lines.len() {
                            *row += 1;
                            *col = 0;
                        }
                    }
                    KeyCode::Up => {
                        if *row > 0 {
                            *row -= 1;
                            *col = (*col).min(lines[*row].chars().count());
                        }
                    }
                    KeyCode::Down => {
                        if *row + 1 < lines.len() {
                            *row += 1;
                            *col = (*col).min(lines[*row].chars().count());
                        }
                    }
                    KeyCode::Home => *col = 0,
                    KeyCode::End => *col = len,
                    KeyCode::Esc => self.mode = Mode::Normal,
                    _ => {}
                }
                true
            }
            Mode::PaneEdit { name, lines, row, col, pane } => {
                // C32 (combined): Rename's editing vocabulary on the
                // current row — Ctrl+U/W, insert-at-point, Backspace/
                // Delete, Home/End — where row 0 is the name and rows
                // 1.. are the note; plus the vertical half: ↑↓ move rows,
                // Shift+Enter (or Ctrl+Enter; Alt+Enter is caught in the
                // Alt branch above) breaks — descend from the name, split
                // inside the note — and ←→ flow across note line ends.
                // Motion crosses the name/note seam; **edits never do**:
                // Backspace at the note's top-left and Delete at the
                // name's end are walls, so a typo can't merge status text
                // into identity. Plain Enter commits both fields, Esc
                // walks away.
                let pane = *pane;
                let ctrl = key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL);
                let shift = key.modifiers.contains(crossterm::event::KeyModifiers::SHIFT);
                // Same stale-point hygiene as Rename: clamp, never slice
                // bad. Unified rows: 0 = name, 1..=lines.len() = note.
                *row = (*row).min(lines.len());
                let len =
                    if *row == 0 { name.chars().count() } else { lines[*row - 1].chars().count() };
                *col = (*col).min(len);
                match key.code {
                    KeyCode::Enter if shift || ctrl => {
                        pane_edit_break(lines, row, col);
                    }
                    KeyCode::Enter => {
                        // Whole-text trims: an empty name clears back to
                        // the adapter fallback (Rename's rule); stray
                        // blank note lines at either end die (a headline
                        // of "" would make the badge segment a lie),
                        // interior blanks are the author's call.
                        let new_title = name.trim().to_string();
                        let new_title = if new_title.is_empty() { None } else { Some(new_title) };
                        let text = lines.join("\n").trim().to_string();
                        let new_note = if text.is_empty() { None } else { Some(text) };
                        if let Some(spec) = self.find_spec_mut(pane) {
                            spec.title = new_title;
                            // The age stamp answers "when did the *status*
                            // last change" — a pure rename must not
                            // refresh it, an unchanged note keeps its
                            // honest age, and an emptied note clears the
                            // stamp with itself (the acknowledgment
                            // gesture).
                            if spec.note != new_note {
                                spec.noted_at = new_note.as_ref().map(|_| now_unix_secs());
                                spec.note = new_note;
                            }
                        }
                        self.save();
                        self.mode = Mode::Normal;
                    }
                    KeyCode::Char('u') if ctrl => {
                        let cur = if *row == 0 { &mut *name } else { &mut lines[*row - 1] };
                        *cur = cur[byte_at(cur, *col)..].to_string();
                        *col = 0;
                    }
                    KeyCode::Char('w') if ctrl => {
                        let cur = if *row == 0 { &mut *name } else { &mut lines[*row - 1] };
                        let at = byte_at(cur, *col);
                        let head = erase_word(&cur[..at]);
                        *col = head.chars().count();
                        *cur = head + &cur[at..];
                    }
                    KeyCode::Char(c)
                        if key
                            .modifiers
                            .difference(crossterm::event::KeyModifiers::SHIFT)
                            .is_empty() =>
                    {
                        let cur = if *row == 0 { &mut *name } else { &mut lines[*row - 1] };
                        let at = byte_at(cur, *col);
                        cur.insert(at, c);
                        *col += 1;
                    }
                    KeyCode::Char(_) => {} // any other chord: swallowed, not typed
                    KeyCode::Backspace => {
                        if *col > 0 {
                            *col -= 1;
                            let cur = if *row == 0 { &mut *name } else { &mut lines[*row - 1] };
                            let at = byte_at(cur, *col);
                            cur.remove(at);
                        } else if *row > 1 {
                            // Note-internal join; row 1's left edge is the
                            // wall — the note never merges into the name.
                            let cur = lines.remove(*row - 1);
                            *row -= 1;
                            *col = lines[*row - 1].chars().count();
                            lines[*row - 1].push_str(&cur);
                        }
                    }
                    KeyCode::Delete => {
                        if *col < len {
                            let cur = if *row == 0 { &mut *name } else { &mut lines[*row - 1] };
                            let at = byte_at(cur, *col);
                            cur.remove(at);
                        } else if *row >= 1 && *row < lines.len() {
                            // Note-internal join; the name's end is the
                            // wall in the other direction.
                            let next = lines.remove(*row);
                            lines[*row - 1].push_str(&next);
                        }
                    }
                    // Motion crosses the seam — ←→ flow across row ends,
                    // ↑↓ keep the column where they can.
                    KeyCode::Left => {
                        if *col > 0 {
                            *col -= 1;
                        } else if *row > 0 {
                            *row -= 1;
                            *col = if *row == 0 {
                                name.chars().count()
                            } else {
                                lines[*row - 1].chars().count()
                            };
                        }
                    }
                    KeyCode::Right => {
                        if *col < len {
                            *col += 1;
                        } else if *row < lines.len() {
                            *row += 1;
                            *col = 0;
                        }
                    }
                    KeyCode::Up => {
                        if *row > 0 {
                            *row -= 1;
                            let l = if *row == 0 {
                                name.chars().count()
                            } else {
                                lines[*row - 1].chars().count()
                            };
                            *col = (*col).min(l);
                        }
                    }
                    KeyCode::Down => {
                        if *row < lines.len() {
                            *row += 1;
                            *col = (*col).min(lines[*row - 1].chars().count());
                        }
                    }
                    KeyCode::Home => *col = 0,
                    KeyCode::End => *col = len,
                    KeyCode::Esc => self.mode = Mode::Normal,
                    _ => {}
                }
                true
            }
            Mode::Picker { .. } => {
                // U20: both column lengths, read before the mode is borrowed
                // mutably below — the adapter list depends on the live
                // type-ahead filter, so it can't be a constant. Row count
                // only, so this is `picker_filtered_ids` (no PATH checks).
                let rows = self.picker_filtered_ids().len();
                let cwds = self.recent_cwds.len();
                let Mode::Picker { selection, filter, cwd, on_cwd } = &mut self.mode else {
                    return true;
                };
                match key.code {
                    // U20: digits are always accelerators, never filter
                    // text. No adapter id or path column ever needs a digit
                    // typed to reach it, and the accelerator is the fastest
                    // thing in the dialog — giving it up to type-ahead
                    // would trade the picker's best key for its rarest.
                    // Out of range is ignored and the picker stays up: the
                    // rows carry their own numbers (C14).
                    KeyCode::Char(c @ '1'..='9') => {
                        let i = c as usize - '1' as usize;
                        if i < rows {
                            self.picker_launch(i);
                        }
                    }
                    // U20 type-ahead: every other printable narrows the
                    // adapter list. This is why `j`/`k` are no longer
                    // motions — a list you filter by typing cannot reserve
                    // letters, and those two were never advertised anywhere
                    // (the unhinted-`j/k` complaint U20 opened with). The
                    // arrows, which the hint bar does advertise, are
                    // untouched.
                    KeyCode::Char(c)
                        if key
                            .modifiers
                            .difference(crossterm::event::KeyModifiers::SHIFT)
                            .is_empty() =>
                    {
                        filter.push(c);
                        *selection = 0; // the old row may not exist now
                    }
                    KeyCode::Char(_) => {} // any other chord: swallowed
                    KeyCode::Backspace => {
                        filter.pop();
                        *selection = 0;
                    }
                    // ← / → move between the two columns; ↑ / ↓ move within
                    // whichever one has them. Wrapping is per column, as
                    // before.
                    KeyCode::Left => *on_cwd = false,
                    KeyCode::Right => *on_cwd = cwds > 0,
                    KeyCode::Tab => *on_cwd = !*on_cwd && cwds > 0,
                    KeyCode::Up => {
                        let (sel, len) =
                            if *on_cwd { (&mut *cwd, cwds) } else { (&mut *selection, rows) };
                        if len > 0 {
                            *sel = sel.checked_sub(1).unwrap_or(len - 1);
                        }
                    }
                    KeyCode::Down => {
                        let (sel, len) =
                            if *on_cwd { (&mut *cwd, cwds) } else { (&mut *selection, rows) };
                        if len > 0 {
                            *sel = (*sel + 1) % len;
                        }
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
            Mode::Scroll => {
                let page = (self.term_size.height / 2).max(1) as usize;
                let focused = self.focused;
                // U9/P14: every step reads the view's CURRENT offset off the
                // grid and writes the result straight back to it. The grid
                // auto-advances that offset as new rows bank while scrolled,
                // so anything cached between two presses is stale by the
                // second one — which is what walked the view 4 lines toward
                // the tail on the next key, and what let ~240 banked Down
                // presses pile up before the screen moved. There is now no
                // "between": a pane with no runtime has no view to scroll,
                // and this does nothing rather than counting into the void.
                let Some(cur) = self.runtimes.get(&focused).map(|rt| rt.scroll_offset()) else {
                    return true;
                };
                let want = match key.code {
                    KeyCode::Up | KeyCode::Char('k') => Some(cur + 1),
                    KeyCode::Down | KeyCode::Char('j') => Some(cur.saturating_sub(1)),
                    KeyCode::PageUp => Some(cur + page),
                    KeyCode::PageDown => Some(cur.saturating_sub(page)),
                    KeyCode::Esc | KeyCode::Char('q') => None,
                    _ => return true,
                };
                match want {
                    Some(n) => {
                        if let Some(rt) = self.runtimes.get_mut(&focused) {
                            rt.set_scrollback(n);
                        }
                    }
                    None => self.exit_mode(), // U18: one definition of leaving
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
                    // U17: vim's word motions, over the row the cursor is on
                    // — whitespace-delimited, and clamped to the row's ends
                    // rather than wrapping to a neighbouring line (the
                    // cursor lives in visible-grid cell space, C24).
                    KeyCode::Char('w') => {
                        cursor.1 = word_forward(&copy_row, cursor.1, copy_w.saturating_sub(1))
                    }
                    KeyCode::Char('b') => cursor.1 = word_back(&copy_row, cursor.1),
                    KeyCode::Char('e') => {
                        cursor.1 = word_end(&copy_row, cursor.1, copy_w.saturating_sub(1))
                    }
                    _ => {}
                }
                // A motion above extends an active selection to the new
                // cursor (v's "movement extends the selection" rule) — kept
                // out of the motion arms themselves so it applies uniformly
                // without repeating it six times.
                if matches!(
                    key.code,
                    KeyCode::Char('h' | 'l' | 'k' | 'j' | '0' | '$' | 'w' | 'b' | 'e')
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
                            self.selection = Some(Selection {
                                pane: focused,
                                anchor,
                                cursor: anchor,
                                dragging: false,
                            });
                        }
                    }
                    // U17: `V` selects the cursor's whole row — the "grab
                    // what the agent just said" gesture, and the one people
                    // reach for first. Absolute, not a toggle like `v`: it
                    // always (re)selects the current line, so pressing it
                    // again is idempotent and `j`/`k` then extend by rows.
                    // `v` and Esc are the ways to clear.
                    KeyCode::Char('V') => {
                        let focused = self.focused;
                        let last = copy_w.saturating_sub(1);
                        let row = cursor.0;
                        cursor.1 = last;
                        self.selection = Some(Selection {
                            pane: focused,
                            anchor: (row, 0),
                            cursor: (row, last),
                            dragging: false,
                        });
                    }
                    // U19: `o` opens the URL under the cursor — keyboard
                    // parity for a verb that was mouse-only (Alt+click), so
                    // a link an agent printed is reachable without ever
                    // leaving the keyboard. Wrapped links included: `url_at`
                    // joins the run either way.
                    KeyCode::Char('o') => {
                        let focused = self.focused;
                        let (r, c) = *cursor;
                        match self.url_at(focused, r, c) {
                            Some(url) => self.pending_open = Some(url),
                            None => self.set_flash("no URL under the cursor"),
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
                    KeyCode::Esc | KeyCode::Char('q') => self.exit_mode(), // U18
                    _ => {}
                }
                true
            }
            Mode::Help { .. } => {
                // C15 (amended): any key dismisses the keymap — except the
                // scroll keys, and only when there is something to scroll
                // to. On a terminal that shows the whole table (the common
                // case) `help_scroll` reports no movement, so ↓ closes the
                // overlay exactly as it always did: the amendment costs
                // nothing anywhere it isn't needed, and a reader on a short
                // terminal is not dismissed for trying to read on.
                //
                // [F9] ...and except the filter, once `/` has opened it.
                // The two carve-outs are deliberately different shapes and
                // C15 says so: scrolling is *conditional* (a key that moved
                // nothing falls through and closes), while the filter, once
                // open, holds every printable. That is C27's roster rule
                // ("a letter is filter text"), not a widening of the scroll
                // rule, and it is confined to a state the title declares.
                let filtering = matches!(&self.mode, Mode::Help { filter: Some(_), .. });

                // `/` opens the filter — and only from the un-filtered
                // state. Once filtering, `/` is a character like any other:
                // a query can contain a slash, and a key that means "open"
                // in one state and "type" in another is the ambiguity U20
                // resolved the same way for the picker.
                if !filtering && key.code == KeyCode::Char('/') {
                    if let Mode::Help { filter, .. } = &mut self.mode {
                        *filter = Some(String::new());
                    }
                    return true;
                }

                if filtering {
                    // Delete is an *edit* key, and a reader reaching for it
                    // is erasing a typo — closing the overlay instead is the
                    // sharpest way there is to lose a query. It has nothing
                    // to delete (the caret is always at the end of an
                    // append-only query), so it does nothing, which is what
                    // Delete does at the end of any text field. Found by the
                    // C39 design audit, which called it the sharp one.
                    if key.code == KeyCode::Delete {
                        return true;
                    }
                    // Scrolling still works while filtering — none of these
                    // are printable, so nothing is displaced. Handled below
                    // by falling through to the shared `delta` match.
                    let typed = match key.code {
                        KeyCode::Char(c)
                            if key
                                .modifiers
                                .difference(crossterm::event::KeyModifiers::SHIFT)
                                .is_empty() =>
                        {
                            Some(Some(c))
                        }
                        KeyCode::Backspace => Some(None),
                        _ => None,
                    };
                    if let Some(edit) = typed {
                        if let Mode::Help { filter: Some(q), top } = &mut self.mode {
                            match edit {
                                Some(c) => q.push(c),
                                None => {
                                    q.pop();
                                }
                            }
                            // The list under the query just changed length,
                            // so a `top` from the old list can point past
                            // the new one's end. Reset rather than clamp:
                            // a filtered list is a new list, and C14/C27
                            // both put the cursor at its start.
                            *top = 0;
                        }
                        return true;
                    }
                    // Esc clears a query, then closes — two presses, the
                    // roster's own way out. Clearing first means a typo
                    // costs one key, not the whole overlay and the place
                    // you had scrolled to.
                    if key.code == KeyCode::Esc {
                        let empty = matches!(&self.mode, Mode::Help { filter: Some(q), .. } if q.is_empty());
                        if empty {
                            self.mode = Mode::Normal;
                        } else if let Mode::Help { filter, top } = &mut self.mode {
                            *filter = Some(String::new());
                            *top = 0;
                        }
                        return true;
                    }
                }

                let step = self.help_page();
                let delta = match key.code {
                    KeyCode::Down => 1,
                    KeyCode::Up => -1,
                    // `j`/`k` scroll only while the filter is closed. Once
                    // it is open they are query text — the same trade C27's
                    // roster makes for every letter, and the reason its
                    // hint bar drops `q`.
                    KeyCode::Char('j') if !filtering => 1,
                    KeyCode::Char('k') if !filtering => -1,
                    // Space is deliberately NOT a page key. In a modal whose
                    // contract is "any key closes it", Space is the key a
                    // reader hits to make it go away — pressing it and
                    // getting another screenful is the opposite of what they
                    // asked for.
                    KeyCode::PageDown => step,
                    KeyCode::PageUp => -step,
                    KeyCode::Home => isize::MIN,
                    KeyCode::End => isize::MAX,
                    _ => {
                        self.mode = Mode::Normal;
                        return true;
                    }
                };
                if !self.help_scroll(delta) && !filtering {
                    // While filtering, a scroll key that hit the end must
                    // NOT close: the reader is mid-query, and losing it to
                    // an over-pressed ↓ is the "modal you open when you are
                    // lost is the one with a surprising way out" failure
                    // C15 rejected scrolling over in the first place.
                    self.mode = Mode::Normal;
                }
                true
            }
            Mode::Feed { offset } => {
                let cap = self.feed.len().saturating_sub(1);
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => *offset = (*offset + 1).min(cap),
                    KeyCode::Down | KeyCode::Char('j') => *offset = offset.saturating_sub(1),
                    KeyCode::PageUp => *offset = (*offset + feed_page).min(cap),
                    KeyCode::PageDown => *offset = offset.saturating_sub(feed_page),
                    // U25: the feed listed what happened and left you to go
                    // find it by hand. Enter jumps to the selected entry's
                    // pane, through the same helper Alt+a's ring uses — so a
                    // jump out of the feed switches tabs, expands stacks and
                    // shows the float exactly like every other jump.
                    KeyCode::Enter => {
                        let target = feed_selected(&self.feed, *offset).and_then(|e| e.pane);
                        match target {
                            // Ids are recycled, and a line can outlive its
                            // pane: re-check before moving anywhere.
                            Some(id) if self.find_spec(id).is_some() => {
                                self.mode = Mode::Normal;
                                self.focus_attention_target(id);
                            }
                            Some(_) => self.set_flash("that pane is gone"),
                            None => self.set_flash("no pane on that line"),
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
                    _ => {}
                }
                true
            }
            // C27: the fleet roster. A list you filter by typing cannot
            // reserve letters (U20's finding, paid for by the picker), so the
            // motions here are the arrows and the paging keys; every
            // printable — `j`, `k` and `q` included — is filter text, and
            // `Esc` (with Alt+Shift+a) is the way out. `Tab`/`Shift+Tab`
            // cycle the status filter — the two keys left over once every
            // `Char` is spoken for.
            Mode::Roster { .. } => {
                let page = self.overlay_page() as isize;
                match key.code {
                    KeyCode::Up => self.roster_move(-1),
                    KeyCode::Down => self.roster_move(1),
                    KeyCode::PageUp => self.roster_move(-page),
                    KeyCode::PageDown => self.roster_move(page),
                    KeyCode::Tab => self.roster_cycle_status(1),
                    KeyCode::BackTab => self.roster_cycle_status(-1),
                    KeyCode::Enter => {
                        if let Mode::Roster { cursor, .. } = &self.mode {
                            let target = *cursor;
                            self.roster_jump(target);
                        }
                    }
                    KeyCode::Esc => self.mode = Mode::Normal,
                    // Shift is how an uppercase letter arrives under the
                    // kitty encoding, so it is the one modifier a filter
                    // keystroke may carry — same rule the rename field (U16)
                    // and the search prompt (P21) follow.
                    KeyCode::Char(c)
                        if key
                            .modifiers
                            .difference(crossterm::event::KeyModifiers::SHIFT)
                            .is_empty() =>
                    {
                        if let Mode::Roster { filter, .. } = &mut self.mode {
                            filter.push(c);
                        }
                        self.roster_clamp_cursor();
                    }
                    KeyCode::Backspace => {
                        if let Mode::Roster { filter, .. } = &mut self.mode {
                            filter.pop();
                        }
                        self.roster_clamp_cursor();
                    }
                    _ => {}
                }
                true
            }
            // P21: the incremental search prompt. Every printable key is
            // query text — including `n`, `N` and `/`, which only mean
            // "step" once the prompt is closed; a prompt that stole letters
            // from what you are typing would be unusable.
            Mode::Search { copy_cursor } => {
                let copy_cursor = *copy_cursor;
                match key.code {
                    // Shift is how an uppercase letter arrives under the
                    // kitty encoding, so it is the one modifier a plain
                    // insert may carry — same rule as the rename dialog
                    // (U16). Any other chord is swallowed, never typed.
                    KeyCode::Char(c)
                        if key
                            .modifiers
                            .difference(crossterm::event::KeyModifiers::SHIFT)
                            .is_empty() =>
                    {
                        if let Some(s) = &mut self.search {
                            s.query.push(c);
                        }
                        self.refilter_search();
                    }
                    KeyCode::Char(_) => {}
                    KeyCode::Backspace => {
                        if let Some(s) = &mut self.search {
                            s.query.pop();
                        }
                        self.refilter_search();
                    }
                    // Enter accepts: the prompt closes, the view stays on
                    // the hit it walked to, and the search itself lives on
                    // so `n`/`N` and the highlights keep working in the
                    // mode it hands back to.
                    KeyCode::Enter => self.mode = self.mode_after_search(copy_cursor),
                    // Esc cancels: the view goes back where `/` was pressed
                    // and the search leaves no trace.
                    KeyCode::Esc => {
                        self.cancel_search();
                        self.mode = self.mode_after_search(copy_cursor);
                    }
                    _ => {}
                }
                true
            }
        }
    }

    /// U18: leave the current mode exactly the way its own `Esc` does — the
    /// one definition both that key and the mode's entry chord (pressed a
    /// second time) go through, so a toggle-off can never leave state a
    /// cancel would have cleaned up. `Esc`'s arms in `handle_mode_key`
    /// mirror this; `esc_and_the_entry_chord_leave_identical_state` pins
    /// them together.
    fn exit_mode(&mut self) {
        match self.mode {
            // Snap the view back to the live tail, same as Esc/q.
            Mode::Scroll => {
                let focused = self.focused;
                if let Some(rt) = self.runtimes.get_mut(&focused) {
                    rt.set_scrollback(0);
                }
            }
            // A cancelled copy takes its selection with it.
            Mode::Copy { .. } => self.selection = None,
            _ => {}
        }
        // P21: and its search — the hits are line offsets into one pane's
        // history, meaningless the moment you are no longer looking at it.
        self.search = None;
        self.mode = Mode::Normal;
    }

    /// The visible text of one row of pane `id` as the copy cursor sees it
    /// (one char per cell, trailing spaces trimmed) — the string U17's word
    /// motions walk.
    fn row_text(&self, id: PaneId, row: u16) -> String {
        self.runtimes.get(&id).map(|rt| rt.grab_text((row, 0), (row, u16::MAX))).unwrap_or_default()
    }

    /// Clean shutdown: save workspace, kill children (their sessions live on).
    pub fn shutdown(&mut self) {
        self.save();
        self.kill_fleet();
    }

    /// The teardown half of `shutdown`, without the save.
    ///
    /// Split out for the panic path (`main.rs`'s `run`): a panic unwinds
    /// straight *past* `shutdown`, and roost dying while every agent keeps
    /// running detached is the exact failure `tests/signal_shutdown.rs`
    /// exists to prevent — reached by a bug instead of a signal.
    ///
    /// The save is deliberately not part of it. A panic can have left the
    /// workspace mid-mutation, and roost writes on every mutating action, so
    /// the copy on disk is at most one action old: writing again is all
    /// downside.
    pub fn kill_fleet(&mut self) {
        // Graceful stop: SIGHUP everything (agents flush their final turn like
        // a closed terminal would allow), a short grace window, then the
        // guaranteed SIGKILL + reap for anything that ignored the hangup.
        if self.runtimes.is_empty() {
            return;
        }
        for rt in self.runtimes.values_mut() {
            rt.hangup();
        }
        // Once, for the whole fleet — `hangup` marks each pane so `kill`
        // does not spend it again per pane (`infra::pty::HANGUP_GRACE`).
        std::thread::sleep(crate::infra::pty::HANGUP_GRACE);
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
/// Per-field cap on what a caller can push into one audit line. The log is
/// byte-bounded (`AUDIT_LOG_MAX`, one rotated generation), and audit lines
/// interpolate caller-controlled strings — an adapter name, a `wait --until`
/// value — so without a cap the *bytes per call* are attacker-chosen up to
/// the socket's 64 KiB line limit. That made rolling the log twice, which
/// erases every record of what a compromised pane did, a ~140-request job:
/// seconds, not hours. Capping the field is what makes the rotation attack
/// take a length of time the rate limiter can then make impractical; the
/// buckets alone never could, because they bound calls and the log bounds
/// bytes. 512 is far past any legitimate adapter name or status word.
///
/// This is a **byte** budget, not a char count: every number in this
/// comment is about bytes on disk against `AUDIT_LOG_MAX`, and a char cap
/// would let an adapter-name field stuffed with 4-byte astral-plane
/// codepoints buy up to ~2 KiB instead.
const AUDIT_FIELD_CAP: usize = 512;

/// Cleans control chars, then caps at `AUDIT_FIELD_CAP` **bytes** (not
/// chars), never splitting a multi-byte character: each candidate char is
/// checked against the byte budget before it's pushed, so truncation
/// always lands on a char boundary.
fn sanitize(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        let c = if c.is_ascii_control() { ' ' } else { c };
        if out.len() + c.len_utf8() > AUDIT_FIELD_CAP {
            break;
        }
        out.push(c);
    }
    out
}

/// B1: the char index the grid CELL column `cell` falls inside.
/// `row_text`/`grab_text` (P15) drop wide glyphs' continuation cells, so
/// the extracted string's char positions and the row's cell positions
/// diverge the moment a row holds one — indexing the string directly with
/// a cell column would walk off by however many wide glyphs sit before the
/// target, returning the wrong word entirely. Clamps to the string's
/// length past the end.
fn cell_to_char(line: &str, cell: usize) -> usize {
    use unicode_width::UnicodeWidthChar;
    let mut acc = 0usize;
    for (i, c) in line.chars().enumerate() {
        let w = c.width().unwrap_or(1).max(1);
        if cell < acc + w {
            return i;
        }
        acc += w;
    }
    line.chars().count()
}

/// The inverse of `cell_to_char`: the grid CELL column char index `idx`
/// starts at.
fn char_to_cell(line: &str, idx: usize) -> usize {
    use unicode_width::UnicodeWidthChar;
    line.chars().take(idx).map(|c| c.width().unwrap_or(1).max(1)).sum()
}

/// The whitespace-delimited run containing grid CELL column `cell` of
/// `line` — the shared tokenizer `find_url_at` (URL validation layered on
/// top) and double-click word selection (`App::select_word_at`) both walk,
/// so the two can never disagree about where a token starts and ends.
/// Converts to char space via `cell_to_char` before walking (B1) and
/// returns **char** indices — `find_url_at`'s own char-based slicing wants
/// exactly that, and `select_word_at` converts back with `char_to_cell`.
/// `None` when `cell` sits on whitespace or past the line's
/// (trailing-space-trimmed) end — there is no token there to grab.
fn word_bounds_at(line: &str, cell: usize) -> Option<(usize, usize)> {
    let chars: Vec<char> = line.chars().collect();
    let col = cell_to_char(line, cell);
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
    Some((start, end))
}

/// Find an http(s) URL that covers grid CELL column `col` in `line` (B1: a
/// cell column, not a char index — see `cell_to_char`). The URL is the
/// surrounding non-whitespace run, with wrapping/trailing punctuation
/// stripped. Pure, so it's unit-tested.
pub fn find_url_at(line: &str, col: usize) -> Option<String> {
    let (start, end) = word_bounds_at(line, col)?;
    let token: String = line.chars().skip(start).take(end - start + 1).collect();
    // Strip wrapping brackets/quotes and trailing sentence punctuation.
    let trimmed = token.trim_matches(|c: char| "()[]{}<>\"'`.,;:!?".contains(c));
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        Some(trimmed.to_string())
    } else {
        None
    }
}

/// U25: the feed entry the overlay's cursor is on — `offset` counts back
/// from the newest, and the renderer draws that entry on the window's last
/// row (`render::feed_window`'s `end - 1`), so "selected" here and the
/// marked row on screen are the same line by construction.
pub fn feed_selected(feed: &VecDeque<FeedEntry>, offset: usize) -> Option<&FeedEntry> {
    let last = feed.len().checked_sub(1)?;
    feed.get(last - offset.min(last))
}

/// U19: stitch one wrapped run of grid rows back into the single logical
/// line the program printed, and return the URL under a cell of it. `run`
/// holds the run's rows in order as `grab_text` returns them (trailing
/// spaces trimmed), `width` is the pane's inner column count, and
/// `(row, col)` is the clicked cell — `row` indexed *within the run*.
///
/// Every row but the last is padded back out to `width` before the join.
/// A wrapped row is full by definition, so that padding is normally empty
/// and only keeps the column arithmetic exact; where a tail really was
/// blank, the restored spaces correctly keep the two rows' tokens apart
/// instead of fusing them into one nonsense word.
pub fn url_in_wrapped_rows(run: &[String], width: usize, row: usize, col: usize) -> Option<String> {
    let mut line = String::new();
    let mut at = None;
    for (i, text) in run.iter().enumerate() {
        if i == row {
            at = Some(line.chars().count() + col);
        }
        line.push_str(text);
        if i + 1 < run.len() {
            for _ in text.chars().count()..width {
                line.push(' ');
            }
        }
    }
    find_url_at(&line, at?)
}

/// U18: the global `Action` that *enters* each mode — press that chord while
/// the mode is already up and it exits instead of re-entering. Derived
/// against `input::translate`'s own output rather than a second chord table,
/// so rebinding a chord moves its toggle with it. `Normal` has no entry
/// chord (nothing to toggle off).
fn mode_entry_action(mode: &Mode) -> Option<Action> {
    match mode {
        Mode::Normal => None,
        Mode::Rename { .. } => Some(Action::RenameTab),
        Mode::PaneEdit { .. } => Some(Action::EditPane),
        Mode::Picker { .. } => Some(Action::QuickLaunch),
        Mode::Scroll => Some(Action::ScrollMode),
        Mode::Copy { .. } => Some(Action::CopyMode),
        Mode::Help { .. } => Some(Action::Help),
        Mode::Feed { .. } => Some(Action::ToggleFeed),
        Mode::Roster { .. } => Some(Action::ToggleRoster),
        Mode::Broadcast { .. } => Some(Action::ToggleBroadcast),
        // P21: the search prompt is entered with `/`, not an Alt chord, so
        // there is no chord for it to toggle off. Alt+PgUp while searching
        // therefore falls through to the global binding, exactly as it
        // would from any other non-Scroll mode.
        Mode::Search { .. } => None,
    }
}

/// P21: every occurrence of `needle` in `lines`, in reading order (oldest
/// line first), as (line index, **char** column). Non-overlapping within a
/// line, the way every search UI counts them. An empty needle matches
/// nothing — an incremental search with nothing typed has no results, not
/// every position in the buffer.
///
/// Matching is ASCII-case-insensitive: `to_ascii_lowercase` is 1:1 on chars,
/// so the column of a hit in the folded text is also its column in the real
/// text. Full Unicode case folding is not — `İ` lowercases to two chars —
/// and a column that doesn't survive the fold would highlight the wrong
/// cells, which is worse than not folding Turkish dotted I.
///
/// Columns are char offsets, not grid cells: a row containing wide (CJK /
/// emoji) glyphs ahead of a hit highlights that many cells to the left of
/// it. Same bounded, documented drift `find_url_at` carries (SPEC-ux U19);
/// the fix for both is one cell→char map per row, and it belongs with U24's
/// wide-char work rather than here.
pub fn find_matches(lines: &[String], needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    let needle = needle.to_ascii_lowercase();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let hay = line.to_ascii_lowercase();
        for (byte, _) in hay.match_indices(&needle) {
            out.push((i, hay[..byte].chars().count()));
        }
    }
    out
}

/// U16: byte offset of char index `at` in `s` (clamped to the end) — the
/// bridge between the rename cursor's char-space arithmetic and Rust's
/// byte-indexed slicing. A name can hold multi-byte chars (an emoji, an
/// accented word), and slicing one down the middle panics.
fn byte_at(s: &str, at: usize) -> usize {
    s.char_indices().nth(at).map_or(s.len(), |(b, _)| b)
}

/// C32: Shift/Ctrl/Alt+Enter in the combined editor, shared by the mode
/// arm and the Alt-branch carve-out so the spellings can never drift. On
/// the name row (`row` 0) it *descends* into the note — the name is
/// single-line by contract, so "line break" there means "start writing
/// the note", which makes name → note one continuous typing flow. On a
/// note row it splits at the point via `note_split_line`.
/// C27/C36: one step through `ROSTER_STATUS_CYCLE` (worst-first, with
/// `None` = every tier at both ends so cycling never dead-ends). Shared by
/// the roster's `Tab` and the broadcast composer's, so the two surfaces
/// cannot drift about what the tiers are or what order they come in — the
/// composer is showing a *target count* for the filter it names, and a
/// filter that meant something different in each place would make that
/// count unreadable.
fn cycle_status_filter(current: Option<AgentStatus>, back: bool) -> Option<AgentStatus> {
    step_status_filter(current, back, false)
}

/// The same step, with `deliverable_only` dropping the one tier the
/// broadcast composer can never act on.
///
/// `broadcast_targets` filters `Exited` unconditionally — a pane that is
/// gone cannot receive anything — so on that surface the cycle's last stop
/// was **structurally inert**: `Tab` landed on "exited", the title's live
/// count read 0 whatever the workspace held, and Enter sent nothing. A stop
/// that can only ever mean "nobody" is not a filter, it is a hole in the
/// cycle. Found by the C36 design audit.
///
/// The roster keeps it, because the roster is a *monitoring* surface and
/// "which of my panes died" is exactly what it is for. So the two surfaces
/// still share one definition of what the tiers are and what order they
/// come in — the property the shared function exists for — and differ by a
/// single omission stated here rather than by a second const free to drift.
///
/// Skipping is a **second step in the same direction**, never a fallback to
/// `None`: `Tab` and `Shift+Tab` have to stay inverses across the gap, and
/// landing both on the same tier would break that at exactly one point in
/// the lap.
///
/// **The double step assumes exactly one skipped tier.** Two adjacent
/// skips would land on a skipped tier, and this would have to loop instead.
/// One is all there is — `broadcast_targets` has one unconditional
/// exclusion — and the assertion below fails loudly rather than returning a
/// tier the caller cannot use, so a second exclusion added later cannot
/// pass silently. Named by the design audit as an undocumented invariant.
fn step_status_filter(
    current: Option<AgentStatus>,
    back: bool,
    deliverable_only: bool,
) -> Option<AgentStatus> {
    let n = ROSTER_STATUS_CYCLE.len() as isize;
    let at = ROSTER_STATUS_CYCLE.iter().position(|s| *s == current).unwrap_or(0) as isize;
    let step = if back { -1 } else { 1 };
    let next = ROSTER_STATUS_CYCLE[(at + step).rem_euclid(n) as usize];
    if deliverable_only && next == Some(AgentStatus::Exited) {
        let past = ROSTER_STATUS_CYCLE[(at + step * 2).rem_euclid(n) as usize];
        debug_assert_ne!(
            past,
            Some(AgentStatus::Exited),
            "the single-gap assumption broke: two skipped tiers need a loop, not a double step",
        );
        return past;
    }
    next
}

/// C36: split the composer's current line at the point.
///
/// **Capped at `BROADCAST_MAX_LINES`.** The dialog is `lines + 2` rows tall
/// and `centered_near` clamps to the body, so without a cap a long enough
/// message pushes its own rows — and the caret — off the bottom silently,
/// with nothing to scroll them back. C32 caps its note for the same reason;
/// this is that rule applied to the surface that borrowed its keys. At the
/// cap the break is refused rather than partially applied: losing the split
/// is recoverable, losing the text under it is not.
fn broadcast_break(lines: &mut Vec<String>, row: &mut usize, col: &mut usize) {
    if lines.len() >= BROADCAST_MAX_LINES {
        return;
    }
    let at = byte_at(&lines[*row], *col);
    let tail = lines[*row].split_off(at);
    lines.insert(*row + 1, tail);
    *row += 1;
    *col = 0;
}

fn pane_edit_break(lines: &mut Vec<String>, row: &mut usize, col: &mut usize) {
    if *row == 0 {
        // Land at the headline's END, not at the name's column carried
        // over: descending means "write the note now", and the writing
        // point of an existing line is after its last character.
        *row = 1;
        *col = lines[0].chars().count();
    } else {
        let mut nrow = *row - 1;
        note_split_line(lines, &mut nrow, col);
        *row = nrow + 1;
    }
}

/// C32: split the note line at the point. At `NOTE_MAX_LINES` the split
/// is swallowed whole (no new line, nothing scrolled off the dialog);
/// the clamps are the Rename arm's stale-point hygiene. `row`/`col` are
/// **note-relative** (0 = the headline) — `pane_edit_break` converts from
/// the editor's unified rows.
fn note_split_line(lines: &mut Vec<String>, row: &mut usize, col: &mut usize) {
    if lines.len() >= NOTE_MAX_LINES {
        return;
    }
    *row = (*row).min(lines.len() - 1);
    let line = &mut lines[*row];
    let at = byte_at(line, (*col).min(line.chars().count()));
    let tail = line[at..].to_string();
    line.truncate(at);
    lines.insert(*row + 1, tail);
    *row += 1;
    *col = 0;
}

/// C32: wall-clock seconds for `noted_at`. A pre-1970 system clock yields
/// 0 rather than a panic — the age tag then reads absurdly old, which is
/// at least honest about the clock being broken. `pub` because the badge's
/// age tag reads the same clock, once per frame (C5's shared-read idiom).
pub fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// U17 word motions, shared shape: is cell `i` of `line` whitespace? Past the
/// end of the (trailing-space-trimmed) row counts as whitespace — the grid's
/// blank tail is not part of any word.
fn ws_at(chars: &[char], i: u16) -> bool {
    chars.get(i as usize).is_none_or(|c| c.is_whitespace())
}

/// U17 `w`: the start of the next whitespace-delimited word to the right,
/// clamped to `last` (the row's final column — copy-mode motions never leave
/// the row they are on).
pub fn word_forward(line: &str, col: u16, last: u16) -> u16 {
    let chars: Vec<char> = line.chars().collect();
    let mut i = col;
    while i <= last && !ws_at(&chars, i) {
        i += 1;
    }
    while i <= last && ws_at(&chars, i) {
        i += 1;
    }
    i.min(last)
}

/// U17 `b`: the start of the word at or before the cursor, clamped to 0.
pub fn word_back(line: &str, col: u16) -> u16 {
    let chars: Vec<char> = line.chars().collect();
    let Some(mut i) = col.checked_sub(1) else { return 0 };
    while i > 0 && ws_at(&chars, i) {
        i -= 1;
    }
    while i > 0 && !ws_at(&chars, i - 1) {
        i -= 1;
    }
    i
}

/// U17 `e`: the last cell of the next word to the right, clamped to `last`.
pub fn word_end(line: &str, col: u16, last: u16) -> u16 {
    let chars: Vec<char> = line.chars().collect();
    if col >= last {
        return last;
    }
    let mut i = col + 1;
    while i < last && ws_at(&chars, i) {
        i += 1;
    }
    while i < last && !ws_at(&chars, i + 1) {
        i += 1;
    }
    i
}

/// U16: readline's `unix-word-rubout` over a text buffer — drop any trailing
/// whitespace, then the whitespace-delimited word in front of it. Pure, so
/// the boundary rule is pinned without a dialog. (The rename buffer has no
/// cursor of its own yet, so "word behind point" is always "word at the
/// end"; cursor motion inside the buffer stays out of scope — SPEC-ux U16.)
pub fn erase_word(buffer: &str) -> String {
    let mut chars: Vec<char> = buffer.chars().collect();
    while chars.last().is_some_and(|c| c.is_whitespace()) {
        chars.pop();
    }
    while chars.last().is_some_and(|c| !c.is_whitespace()) {
        chars.pop();
    }
    chars.into_iter().collect()
}

fn inner_dims(rect: Rect) -> (u16, u16) {
    (rect.height.saturating_sub(2).max(1), rect.width.saturating_sub(2).max(1))
}

/// Shared by `spawn_child` and `move_pane_to_tab`: cut `r` the widest way,
/// refusing (`None`) below the C25 fit floor (`layout::MIN_SPLIT_*`) — the
/// vt100 underflow crash's trigger. A `None` rect (split target not found in
/// the caller's own rect list) is treated as always fitting, matching both
/// callers' pre-existing fallback.
fn split_fit(rect: Option<Rect>) -> Option<SplitDir> {
    let Some(r) = rect else { return Some(SplitDir::Vertical) };
    let dir = if r.width >= r.height * 3 { SplitDir::Vertical } else { SplitDir::Horizontal };
    let too_small = match dir {
        SplitDir::Vertical => r.width < layout::MIN_SPLIT_COLS,
        SplitDir::Horizontal => r.height < layout::MIN_SPLIT_ROWS,
    };
    if too_small {
        None
    } else {
        Some(dir)
    }
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

/// Cap for a needs-input question crossing the socket into chrome —
/// **chars**, unlike `AUDIT_FIELD_CAP`'s bytes (that one budgets disk).
/// Room for a real multi-sentence ask; still ~30× under the socket's
/// per-line limit even at 4 bytes a char.
const NEEDY_MSG_CAP: usize = 512;

/// P6-class sanitize for the needs-input question (`on_status`'s `detail`).
/// Neither existing helper fits: `sanitize` (audit log) passes C1 controls
/// through, `sanitize_title` *drops* controls, which would glue a multi-line
/// ask's words together ("line one\nline two" → "line oneline two"). Here
/// every control char (C0 and C1) becomes a space, then trim + char-cap on
/// char boundaries.
fn sanitize_needy_msg(raw: &str) -> String {
    raw.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(NEEDY_MSG_CAP)
        .collect::<String>()
        .trim()
        .to_string()
}

/// U12's busy predicate, shared by the Alt+w and Alt+q destructive guards so
/// the two can never disagree about what "busy" means: an agent mid-turn is
/// one actively producing (`Working`) *or* blocked on your approval
/// (`NeedsInput`) — closing either loses the in-flight turn.
fn is_busy(status: AgentStatus) -> bool {
    matches!(status, AgentStatus::Working | AgentStatus::NeedsInput)
}

/// Rotate `<state>/control.log` past this size. One generation is kept, so
/// the audit trail costs at most `2 × AUDIT_LOG_MAX` on disk.
///
/// 4 MiB is roughly a hundred thousand control calls — far more than any
/// session produces, so an interactive user never sees a rotation, while a
/// scripted orchestrator hammering `send`/`read` in a loop can no longer
/// grow the file without bound (the ROADMAP's `[perf] Audit-log rotation`).
const AUDIT_LOG_MAX: u64 = 4 * 1024 * 1024;

/// Size-based rotation for the audit log: `control.log` → `control.log.1`,
/// replacing any previous `.1`, once the live file passes `max`.
///
/// Rename rather than truncate-in-place: a rename is atomic, so a reader
/// tailing the log never observes a half-empty file, and the most recent
/// generation survives for exactly as long as it takes the next one to fill.
/// Deliberately best-effort and silent — an audit line that cannot be
/// written must never take a control call down with it, which is the same
/// stance the append below already takes.
fn rotate_audit_log(path: &std::path::Path, max: u64) {
    let too_big = std::fs::metadata(path).map(|m| m.len() >= max).unwrap_or(false);
    if too_big {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
}

/// C20: the feed overlay's `(width, height)` at the given body area —
/// `w = min(72, body.width − 4)`, `h = min(16, body.height − 4)`. Shared by
/// the renderer (geometry) and `handle_mode_key` (PgUp/PgDn page size) so
/// both agree on what's actually on screen — and by the roster overlay
/// (C27), deliberately the same function so toggling between the two never
/// resizes the frame. Pure so the 72×16 formula is unit-tested without a
/// `Frame`.
///
/// **Floored at one cell in each axis.** The `saturating_sub(4)` reached 0
/// on a body four rows tall — a terminal six rows tall, since the tab bar
/// and hint bar take two — and a zero-height dialog draws *nothing*: no
/// border, no title, no rows. The chord still flipped the mode word to
/// `FEED`/`ROSTER` and the hint bar still offered `↑↓ select`, so pressing
/// Alt+e or Alt+Shift+a at a small terminal changed one word on screen and
/// left everything else alone — indistinguishable from a binding that does
/// not work. The picker already had the answer, in `dialog_rect`: "an empty
/// result still needs a frame to say so". These two never adopted it.
/// `centered_near` clamps the ask to the body, so the floor can never
/// overflow a body smaller than the frame it asks for.
pub fn feed_overlay_size(body: Rect) -> (u16, u16) {
    let w = body.width.saturating_sub(4).clamp(1, 72);
    let h = body.height.saturating_sub(4).clamp(1, 16);
    (w, h)
}

/// C27: one row of the fleet roster — a tab's group header, or a pane.
/// Headers are labels, never destinations: the cursor skips them, and a
/// click on one does nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RosterRow {
    Group { label: String },
    Pane { id: PaneId },
}

/// [ux P2-11] Worst-first severity rank for `roster_rows`' sort — lower
/// sorts earlier, mirroring C5's glyph-severity order (◆→○→⠋→·→exited, ⠋
/// standing for the Working spinner) and
/// the roster's own status-filter cycle (`ROSTER_STATUS_CYCLE`) below it. A
/// never-started pane (`None` — C27's own "not started" rung, `display_status`
/// returns it for a lazy tab's pane) ranks with `Idle`: the tab bar already
/// draws it as the same idle dot (C27's `Unknown` rule), and it is "nothing
/// to report" exactly the way an idle pane is. A corpse still sorts last —
/// dead news beats no news, but every live or pending pane beats a corpse.
fn roster_rank(status: Option<AgentStatus>) -> u8 {
    match status {
        Some(AgentStatus::NeedsInput) => 0,
        Some(AgentStatus::Waiting) => 1,
        Some(AgentStatus::Working) => 2,
        Some(AgentStatus::Idle) | None => 3,
        Some(AgentStatus::Exited) => 4,
    }
}

/// [ux P2-11] The roster's status filter cycle — `Tab` steps forward through
/// this list, `Shift+Tab` back, wrapping both ways. Worst-first, same as
/// `roster_rank`, with `None` ("every tier") at the front so the first press
/// narrows from "everything" to the severity you'd actually go looking for.
/// C36: the composer's line ceiling — the dialog grows a row per break and
/// never scrolls, so this is what keeps it inside the body. Matches C32's
/// note cap in spirit and in number.
const BROADCAST_MAX_LINES: usize = NOTE_MAX_LINES;

const ROSTER_STATUS_CYCLE: [Option<AgentStatus>; 6] = [
    None,
    Some(AgentStatus::NeedsInput),
    Some(AgentStatus::Waiting),
    Some(AgentStatus::Working),
    Some(AgentStatus::Idle),
    Some(AgentStatus::Exited),
];

/// C27: a tab group's header text, in C6's uppercase-label idiom
/// (`" STACK · 3 PANES"`) — the tab's own bar label, then how many of its
/// panes the roster is showing.
fn roster_group_label(index: usize, name: &str, panes: usize) -> String {
    format!(" {} {} · {}", index + 1, name.to_uppercase(), roster_pane_count(panes))
}

/// C22's float is not a tab, so it gets its own group rather than being
/// smuggled into the last one.
fn roster_float_label() -> String {
    format!(" FLOAT · {}", roster_pane_count(1))
}

fn roster_pane_count(n: usize) -> String {
    if n == 1 {
        "1 PANE".to_string()
    } else {
        format!("{n} PANES")
    }
}

/// C27: the roster window's first visible row — `top` clamped so the window
/// never runs off the end of a list that shrank while the overlay was open.
/// Pure so the renderer, the mouse hit-test and the cursor-reveal all clamp
/// identically.
pub fn roster_top_clamped(top: usize, len: usize, height: usize) -> usize {
    top.min(len.saturating_sub(height))
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

/// C31: the geometric edge pane of a destination tab's rects — leftmost
/// when `rightmost` is false, rightmost when true. Ties on x — every member
/// of a stack shares its column's x — are broken by `collapsed` (false
/// first): the currently-*expanded* member wins over a 1-row collapsed
/// title bar, so a navigation key lands on what the tab was actually left
/// showing rather than rearranging it. Only below that does position decide
/// (smallest y), then smallest id — `layout::neighbor`'s own tie-break,
/// reused for the same reason (deterministic, not dependent on rect order)
/// rather than invented fresh. `None` only if `rects` is empty, which a
/// real tab's computed rects never are.
fn edge_pane(rects: &[PaneRect], rightmost: bool) -> Option<PaneId> {
    let mut best: Option<(i32, bool, u16, PaneId)> = None;
    for p in rects {
        let x = p.rect.x as i32;
        let key = (if rightmost { -x } else { x }, p.collapsed, p.rect.y, p.id);
        if best.is_none_or(|b| key < b) {
            best = Some(key);
        }
    }
    best.map(|(_, _, _, id)| id)
}

/// Pure decision behind `App::show_alt_hint`, split out so it's testable
/// without depending on process env vars or wall-clock time.
///
/// U4/F1: the trigger is evidence, not an allowlist. "Keys are arriving and
/// not one of them has carried Alt" is true of nearly all typing, so that
/// can't be the evidence — it would fire on a healthy terminal the moment
/// the user typed a shell prompt. The real signature of a swallowed Option
/// chord is narrower — `is_alt_swallow_char` — but (SG1) still not
/// unambiguous: every character in that set is also a directly-typed
/// letter on some non-US macOS layout. `since_evidence` is time elapsed
/// since the *most recent* match (`App::alt_swallow_at`), not since
/// launch — checked against `ALT_HINT_WINDOW` so a false positive clears
/// itself in a few seconds rather than latching for the session, while
/// fresh evidence keeps re-arming the window for a genuinely broken Alt
/// layer. One real Alt key ever ends it outright, for the rest of the
/// session, regardless of any evidence timestamp.
fn wants_alt_hint(alt_seen: bool, since_evidence: Option<Duration>) -> bool {
    !alt_seen && since_evidence.is_some_and(|d| d < ALT_HINT_WINDOW)
}

/// F1: the complete set of characters the standard **US** macOS keyboard
/// layout emits for `Option+<letter>`, `letter` in `a..=z`, when Option
/// isn't configured as Meta — one entry per letter, 26 total (this
/// `matches!` arm list *is* the definition; nothing is abbreviated or
/// approximated here). One of these arriving with no Alt modifier at all is
/// evidence that *some* Alt chord was just swallowed, unlike "a key
/// arrived" (true of nearly every keystroke) — see SG1 for the
/// false-positive this still carries on non-US layouts, and
/// `wants_alt_hint` for how that's bounded.
///
/// Scoped to the US layout only (SG2): a non-US layout's own Option+letter
/// table is a different 26 characters (mostly overlapping, not identical),
/// and this does not attempt to cover them.
///
/// Assumes terminals apply the OS keyboard layout without dead-key
/// composition — i.e. `Option+n` arrives immediately as `˜` rather than
/// held pending a following vowel. That assumption is believed true of
/// Terminal.app/iTerm2/Ghostty/kitty (none composes dead keys itself; a
/// pty is not the `NSTextInputClient` a composing app would need) but is
/// **not verified by anything in this suite** (SG3) — it would need a real
/// terminal emulator and a real OS keyboard driver, neither of which a unit
/// or integration test here drives.
fn is_alt_swallow_char(c: char) -> bool {
    matches!(
        c,
        'å' | '∫'
            | 'ç'
            | '∂'
            | '´'
            | 'ƒ'
            | '©'
            | '˙'
            | 'ˆ'
            | '∆'
            | '˚'
            | '¬'
            | 'µ'
            | '˜'
            | 'ø'
            | 'π'
            | 'œ'
            | '®'
            | 'ß'
            | '†'
            | '¨'
            | '√'
            | '∑'
            | '≈'
            | '¥'
            | 'Ω'
    )
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

/// P0-3: what `spawn_pane` records for a dead pane, and what the dead-pane
/// bar and feed line show — anyhow's default `Display` (`{}`) keeps only the
/// outermost `.context()` pty.rs adds ("spawning pi"); the alternate format
/// (`{:#}`) walks the whole chain on one line ("spawning pi: No such file or
/// directory (os error 2)"), which is the difference between a message and
/// an answer. Capped and truncated on a char boundary — same shape as
/// `pty::sanitize_for_host` — so a pathological chain still reads as one
/// line in the bar instead of spilling past it.
fn dead_reason(e: &anyhow::Error) -> String {
    let full = format!("{e:#}");
    if full.chars().count() <= DEAD_REASON_CAP {
        return full;
    }
    let mut truncated: String = full.chars().take(DEAD_REASON_CAP.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

/// QA-2: validate a spawn's cwd BEFORE a pane is created for it, so a
/// mistyped or just-deleted path fails loudly here instead of the PTY child
/// silently landing somewhere else (confirmed: `$HOME`) while `list`/`status`
/// keep reporting the *requested* path. Two distinct failures, on purpose:
/// a path that doesn't resolve at all (also covers a directory this process
/// can't even see, e.g. a blocked parent) gets the raw OS error; a path that
/// resolves but names a file, not a directory, gets its own clearer message
/// — both are "you can't spawn there," but naming which is which saves a
/// round trip. A directory that exists and is *listable* is treated as
/// enterable — the same permission `chdir` itself needs — so this doubles as
/// the "unreadable directory" case: `read_dir` opens it far enough to prove
/// that without walking its contents.
fn validate_spawn_cwd(cwd: &Path) -> Result<(), String> {
    let meta = std::fs::metadata(cwd).map_err(|e| format!("cwd {}: {e}", cwd.display()))?;
    if !meta.is_dir() {
        return Err(format!("cwd {} is not a directory", cwd.display()));
    }
    if let Err(e) = std::fs::read_dir(cwd) {
        return Err(format!("cwd {}: {e}", cwd.display()));
    }
    Ok(())
}

/// P2-13: does `program` resolve to a real, executable file the way a shell
/// would find it? An absolute/relative path (contains `/`) is checked
/// directly; a bare name is looked up across `path_dirs`, in order. Pure —
/// the directories are a parameter, not read from the environment here — so
/// a test can prove the search itself without depending on what happens to
/// be installed on the machine that runs it.
fn resolves_on_path<P: AsRef<Path>>(program: &str, mut path_dirs: impl Iterator<Item = P>) -> bool {
    if program.contains('/') {
        return is_executable_file(Path::new(program));
    }
    path_dirs.any(|dir| is_executable_file(&dir.as_ref().join(program)))
}

/// A regular file with at least one executable bit set — what actually lets
/// a shell (or `spawn_command`) run it, not just see it.
fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// P2-13: is `id`'s launch program actually installed? Derived from the same
/// `CommandSpec` `spawn_pane` would build (`adapter.launch`), not a
/// hardcoded program name, so a new adapter's launch command is PATH-checked
/// automatically without touching `agents/**`. An id the registry doesn't
/// know (never happens today — `agents::picker_ids()` *is* the registry) is
/// treated as installed: nothing here to warn about.
fn adapter_installed(id: &str, registry: &Registry) -> bool {
    let Some(adapter) = registry.get(id) else { return true };
    let program = adapter.launch(Path::new(".")).program;
    let path = std::env::var_os("PATH").unwrap_or_default();
    resolves_on_path(&program, std::env::split_paths(&path))
}

// ---------------------------------------------------------------------------
// Unit tests — the whole app core runs against fakes, no PTYs involved.
// ---------------------------------------------------------------------------

// `pub(crate)` for `inv_check` alone: `main.rs`'s mouse fuzzer checks the
// same structural invariants after every gesture, and a second copy of them
// over there would drift from this one the first time either is amended.
#[cfg(test)]
pub(crate) mod tests {

    /// Security M2: the audit log is byte-bounded and rotates keeping one
    /// generation, and audit lines interpolate caller-controlled strings.
    /// Uncapped, a caller chose the bytes-per-call (up to the socket's 64 KiB
    /// line limit) and could roll the log twice — erasing every record of
    /// what it did — in ~140 requests. The cap is what turns that from
    /// seconds into a duration the rate limiter can make impractical.
    #[test]
    fn an_audit_field_cannot_be_grown_without_bound_by_its_caller() {
        let hostile = "A".repeat(64 * 1024);
        let out = super::sanitize(&hostile);
        assert_eq!(
            out.chars().count(),
            super::AUDIT_FIELD_CAP,
            "a caller-controlled audit field must be capped, not echoed at its own length"
        );

        // The cap must not disturb what sanitize already existed to do:
        // control bytes still become spaces, and ordinary values pass whole.
        let ctrl = super::sanitize("spawn\u{1b}]2;evil\u{7} adapter=pi");
        assert!(!ctrl.chars().any(|c| c.is_ascii_control()), "control bytes still neutralised");
        let ordinary = "adapter=claude until=needs_input";
        assert_eq!(super::sanitize(ordinary), ordinary, "a legitimate field is untouched");
    }

    /// The cap bounds bytes written to the byte-bounded log
    /// (`AUDIT_LOG_MAX`), not chars — a char cap would let a field of 4-byte
    /// codepoints buy several times the intended budget. The test above
    /// uses pure ASCII, where chars and bytes coincide and can't catch this.
    #[test]
    fn an_audit_field_caps_bytes_not_chars_and_never_splits_one() {
        // U+1F600, 4 bytes in UTF-8: 512 / 4 = 128 whole emoji exactly fill
        // the byte budget; capping by chars would have let all 1000 through
        // at 4000 bytes.
        let hostile = "😀".repeat(1000);
        let out = super::sanitize(&hostile);
        assert_eq!(out, "😀".repeat(128), "must cap bytes (512 / 4), not chars (1000)");
        assert_eq!(out.len(), super::AUDIT_FIELD_CAP);
    }
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
            TokenTable::new().unwrap(),
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
    fn a_pane_notification_names_the_pane_and_queues_one_host_emission() {
        let (mut app, _s) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1 | 2, focus 2
        assert_eq!(app.focused, 2);

        let name = app.display_name(1);
        app.runtimes.get_mut(&1).unwrap().effects = crate::ports::PaneEffects {
            notifications: vec!["NEEDS-YOU".to_string()],
            ..Default::default()
        };
        let msg = app.on_pty_output(1, b"whatever").expect("unfocused pane notifies");
        assert_eq!(msg, format!("{name}: NEEDS-YOU"));
        app.notify_host(&msg);
        // One emission, carrying the name — P2's 2026-08-21 amendment. The
        // verbatim relay that used to ride alongside it is gone; two banners
        // for one event was only invisible while one of them was an
        // `osascript` fork. `contains`, not equality, because the same queue
        // also carries P6's host title.
        let out = app.take_host_output();
        assert_eq!(count(&out, b"\x1b]9;"), 1, "exactly one OSC 9 per notification");
        assert!(window_contains(&out, format!("\x1b]9;{name}: NEEDS-YOU\x07").as_bytes()));

        // The focused pane's notification raises nothing at all: you are
        // already looking at it, and there is no second emitter left to
        // announce it behind your back.
        app.runtimes.get_mut(&2).unwrap().effects = crate::ports::PaneEffects {
            notifications: vec!["also me".to_string()],
            ..Default::default()
        };
        assert!(app.on_pty_output(2, b"x").is_none());
        assert!(!host_contains(&mut app, b"\x1b]9;"));
    }

    /// How many times `needle` occurs in `hay`.
    fn count(hay: &[u8], needle: &[u8]) -> usize {
        hay.windows(needle.len()).filter(|w| *w == needle).count()
    }

    fn window_contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
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
            TokenTable::new().unwrap(),
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
        assert_eq!(app.tab_summary(0).0, TabSummary::Quiet);
        // A pane needing input dominates the summary.
        app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::NeedsInput);
        assert_eq!(app.tab_summary(0).0, TabSummary::NeedsInput);
        // Not spawned (no runtime, no recorded failure) → Unknown, never idle.
        app.runtimes.remove(&id);
        assert_eq!(app.tab_summary(0).0, TabSummary::Unknown);
    }

    /// C2: the summary carries **how many** panes are in the state it
    /// reports — without this, a tab of three needy agents would draw the
    /// same single `◆` as a tab of one. The count is of the summarized
    /// state alone, never a pane total, and it rides on the same call as
    /// the ranking so the two can never disagree about which state won.
    #[test]
    fn tab_summary_counts_the_panes_in_the_state_it_reports() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::NewPane); // one tab, three panes

        assert_eq!(app.tab_summary(0), (TabSummary::Quiet, 0), "Quiet reports nothing");

        for id in [1u64, 2, 3] {
            app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::Working);
        }
        assert_eq!(app.tab_summary(0), (TabSummary::Working, 3));

        // The count follows the *summarized* state, not the pane total: one
        // needy pane outranks two working ones, and counts as one.
        app.runtimes.get_mut(&1).unwrap().set_extension_status(AgentStatus::NeedsInput);
        assert_eq!(app.tab_summary(0), (TabSummary::NeedsInput, 1));
        app.runtimes.get_mut(&2).unwrap().set_extension_status(AgentStatus::NeedsInput);
        assert_eq!(app.tab_summary(0), (TabSummary::NeedsInput, 2));

        // Unknown counts the unspawned panes; Exited counts the corpses.
        app.runtimes.remove(&3);
        assert_eq!(app.tab_summary(0), (TabSummary::NeedsInput, 2), "needs-you still wins");
        app.runtimes.get_mut(&1).unwrap().set_extension_status(AgentStatus::Idle);
        app.runtimes.get_mut(&2).unwrap().set_extension_status(AgentStatus::Idle);
        assert_eq!(app.tab_summary(0), (TabSummary::Unknown, 1));
        app.dead.insert(3, "boom".into()); // a recorded spawn failure is a corpse (U13)
        app.runtimes.get_mut(&1).unwrap().kill();
        assert_eq!(app.tab_summary(0), (TabSummary::Exited, 2));

        // A tab index that doesn't exist reports the blank, counting nothing.
        assert_eq!(app.tab_summary(9), (TabSummary::Quiet, 0));
    }

    /// U13 (closes SPEC-GAP-2): a tab whose agents are dead says so instead
    /// of rendering the Quiet blank — otherwise a background tab full of
    /// corpses gets only one transient bell, then silence, as its entire
    /// signal.
    #[test]
    fn tab_summary_reports_exited_and_ranks_it_between_waiting_and_quiet() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1 | 2 in one tab
                                    // Pane 2 is a real agent, not the default shell: this test is U13's
                                    // "a live agent outranks a corpse" ranking, and U13 is silent on
                                    // shells — a shell's Waiting would read Idle (P2-10) and never
                                    // exercise the Waiting rung this test means to pin.
        app.find_spec_mut(2).unwrap().adapter = "pi".into();
        assert_eq!(app.tab_summary(0).0, TabSummary::Quiet);

        app.runtimes.get_mut(&1).unwrap().kill(); // pane 1 dies
        assert_eq!(app.tab_summary(0).0, TabSummary::Exited, "a dead pane is news");

        // A live pane with something to say outranks a corpse...
        app.runtimes.get_mut(&2).unwrap().set_extension_status(AgentStatus::Waiting);
        assert_eq!(app.tab_summary(0).0, TabSummary::Waiting);
        app.runtimes.get_mut(&2).unwrap().set_extension_status(AgentStatus::Working);
        assert_eq!(app.tab_summary(0).0, TabSummary::Working);
        app.runtimes.get_mut(&2).unwrap().set_extension_status(AgentStatus::NeedsInput);
        assert_eq!(app.tab_summary(0).0, TabSummary::NeedsInput);
        // ...and an idle one does not: Idle is "nothing to report", the
        // corpse is the only news on the tab.
        app.runtimes.get_mut(&2).unwrap().set_extension_status(AgentStatus::Idle);
        assert_eq!(app.tab_summary(0).0, TabSummary::Exited);
        // Every pane dead is still Exited, never Quiet.
        app.runtimes.get_mut(&2).unwrap().kill();
        assert_eq!(app.tab_summary(0).0, TabSummary::Exited);
    }

    /// A pane whose *spawn* failed has no runtime at all — it is dead, not
    /// unspawned, and must not read as `Unknown` (nor drag the tab to it).
    #[test]
    fn tab_summary_counts_a_failed_spawn_as_exited_not_unknown() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.pane_order()[0];
        app.runtimes.remove(&id);
        app.dead.insert(id, "spawn-fail requested".into());
        assert_eq!(app.tab_summary(0).0, TabSummary::Exited);
    }

    /// [design-supervisor D1] The default one-shell workspace must not draw
    /// `○` on the tab bar while the badge/roster (both `display_status`)
    /// draw `·` for the very same pane in the very same frame — C5 is one
    /// table for every chrome surface that shows status, tab bar included.
    #[test]
    fn tab_summary_reads_a_quiet_shells_waiting_as_quiet_not_waiting() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.pane_order()[0];
        app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::Waiting);
        assert_eq!(
            app.tab_summary(0).0,
            TabSummary::Quiet,
            "a shell's Waiting is not tab-bar news"
        );
        assert_eq!(
            app.display_status(id),
            Some(AgentStatus::Idle),
            "same answer the badge/roster read"
        );
    }

    /// [ux P2-10] A quiet shell's heuristic `Waiting` reads `Idle` through
    /// `display_status` — "your turn" is false for a pane nobody asked
    /// anything of. `row_status` (the control plane's own ground truth) is
    /// untouched: a script polling `roost status`/`wait --until waiting`
    /// against a shell must not have its contract rewritten under it.
    #[test]
    fn display_status_downgrades_a_quiet_shells_waiting_to_idle() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.pane_order()[0];
        app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::Waiting);
        assert_eq!(app.row_status(id), Some(AgentStatus::Waiting), "raw ground truth is unchanged");
        assert_eq!(
            app.display_status(id),
            Some(AgentStatus::Idle),
            "a shell has no turn to hand back"
        );

        // A real agent pane's Waiting is untouched — it really might be
        // "your turn".
        app.find_spec_mut(id).unwrap().adapter = "pi".into();
        assert_eq!(app.display_status(id), Some(AgentStatus::Waiting));

        // Every other status, shell or not, passes through unchanged.
        app.find_spec_mut(id).unwrap().adapter = "shell".into();
        for s in [AgentStatus::Working, AgentStatus::NeedsInput, AgentStatus::Idle] {
            app.runtimes.get_mut(&id).unwrap().set_extension_status(s);
            assert_eq!(app.display_status(id), Some(s));
        }
        app.runtimes.get_mut(&id).unwrap().kill();
        assert_eq!(app.display_status(id), Some(AgentStatus::Exited));
    }

    /// U14: the flash says what actually happened. A native helper that
    /// exited clean is an unqualified copy; OSC 52 alone is flagged (sent,
    /// unacknowledged — the one channel that works over SSH, and the one
    /// that can silently do nothing); neither is an honest failure.
    #[test]
    fn copy_flash_text_names_the_channel_that_took_it() {
        assert_eq!(
            App::<FakePane>::copy_flash_text(13, ClipboardOutcome::Native),
            "copied 13 chars"
        );
        assert_eq!(
            App::<FakePane>::copy_flash_text(13, ClipboardOutcome::Osc52),
            "copied 13 chars (OSC 52)"
        );
        let failed = App::<FakePane>::copy_flash_text(13, ClipboardOutcome::Failed);
        assert_eq!(
            failed,
            "copy failed: no clipboard channel worked; check your terminal's clipboard support"
        );
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
        assert_eq!(
            app.flash(),
            Some(
                "copy failed: no clipboard channel worked; check your terminal's clipboard support"
            )
        );
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
        // U22: a *reported* Working — the hook saying a turn is in flight,
        // which is what the guard is for. (Heuristic output on a shell no
        // longer arms it; that case has its own test below.)
        app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::Working);
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

    /// U22: **a shell echoing your own command is not mid-turn.** Any PTY
    /// output in the last two seconds reads as Working, so without this a
    /// shell closed right after `ls` would arm the confirm and cost a
    /// second `Alt+w` — a guard that fires on ordinary use is one people
    /// learn to double-tap through, which is exactly what it must not
    /// become.
    #[test]
    fn a_shell_with_only_heuristic_output_closes_on_the_first_press() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        let id = app.focused;
        app.on_pty_output(id, b"x"); // FakePane: output ⇒ heuristic Working
        assert_eq!(app.runtimes.get(&id).unwrap().status(), AgentStatus::Working);
        assert!(!app.runtimes.get(&id).unwrap().status_reported(), "nothing reported it");
        assert_eq!(app.runtimes.len(), 2);
        app.apply(Action::ClosePane);
        assert_eq!(app.runtimes.len(), 1, "closed on the first press");
        assert_eq!(app.flash(), None, "and armed nothing");
    }

    /// …but the guard is kept everywhere it earns its keep. An **agent**
    /// pane with the same heuristic evidence still arms — a pi/claude pane
    /// with no hook installed has a real turn to lose, and PTY traffic is
    /// the best evidence roost has for it.
    #[test]
    fn an_agent_pane_still_arms_on_heuristic_output_alone() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.spawn_child("pi", None, None).expect("a pi pane");
        app.set_focus(id);
        app.on_pty_output(id, b"x"); // heuristic Working, no extension report
        assert!(!app.runtimes.get(&id).unwrap().status_reported());
        let before = app.runtimes.len();
        app.apply(Action::ClosePane);
        assert_eq!(app.runtimes.len(), before, "armed, not closed");
        assert!(app.flash().is_some_and(|m| m.contains("busy")), "{:?}", app.flash());
        app.apply(Action::ClosePane);
        assert_eq!(app.runtimes.len(), before - 1, "confirmed");
    }

    /// …and a *reported* Working arms whatever the adapter is: a hook saying
    /// a turn is in flight outranks any guess about what the pane runs.
    #[test]
    fn a_reported_working_arms_even_on_a_shell() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::Working);
        app.apply(Action::ClosePane);
        assert_eq!(app.runtimes.len(), 2, "armed, not closed");
    }

    /// U22's shell exemption stops at `Alt+w`, and the asymmetry is the
    /// point: `Alt+q` kills the whole fleet with no undo, including panes
    /// you are not looking at. U1's "sessions resume on relaunch" is an
    /// *agent* argument — a shell halfway through a build resumes nothing —
    /// so a busy shell is exactly what a quit should still ask about.
    #[test]
    fn quit_still_arms_on_a_shell_that_is_merely_producing_output() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.on_pty_output(id, b"x"); // heuristic Working on a shell
                                     // …which `Alt+w` would now close on the first press:
        assert!(!app.mid_turn(id));
        // …but `Alt+q` still arms, because it takes the fleet with it.
        app.apply(Action::Quit);
        assert!(!app.quit, "the quit armed instead");
        assert_eq!(app.flash(), Some("1 agent busy — Alt+q again to quit"));
        app.apply(Action::Quit);
        assert!(app.quit);
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
        app.runtimes.get_mut(&1).unwrap().set_extension_status(AgentStatus::Working);
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
        app.runtimes.get_mut(&1).unwrap().set_extension_status(AgentStatus::Working);
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
        app.runtimes.get_mut(&1).unwrap().set_extension_status(AgentStatus::Working);
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
        app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::Working);
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
        // U22: FLASH_WINDOW (2s) < CONFIRM_WINDOW (3s), so a confirm prompt
        // carries the confirm window itself rather than FLASH_WINDOW — else
        // there'd be a silent final second where the second press still
        // fires with no prompt showing. Ordinary flashes keep FLASH_WINDOW;
        // a consumed arm takes its prompt down with it.
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::Working);
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
        assert!(matches!(app.mode, Mode::Scroll), "still in scroll mode");
        // [P14] The view itself is the assertion now — scroll mode holds no
        // mirror to check, which is the point: the ask (30) never became
        // state anywhere, so the next key cannot resume from it.
        assert_eq!(app.scroll_offset(id), 10, "the grid's clamp, not the ask");
        // One Down moves the view immediately.
        app.handle_mode_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.runtimes.get(&id).unwrap().scrollback, 9);
        assert_eq!(app.scroll_offset(id), 9);
    }

    // -- P21: scrollback search ---------------------------------------------

    fn key(c: char) -> crossterm::event::KeyEvent {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn press(app: &mut App<FakePane>, code: crossterm::event::KeyCode) {
        use crossterm::event::{KeyEvent, KeyModifiers};
        app.handle_mode_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn type_query(app: &mut App<FakePane>, s: &str) {
        for c in s.chars() {
            app.handle_mode_key(key(c));
        }
    }

    /// A pane whose history is `n` numbered lines, all of them banked — the
    /// same shape the e2e drives with `seq`, so the offset arithmetic can be
    /// checked exactly.
    fn searchable_app(lines: usize) -> App<FakePane> {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        let text: Vec<String> = (1..=lines).map(|i| format!("line {i}")).collect();
        let rt = app.runtimes.get_mut(&id).unwrap();
        rt.all_text = text.join("\n");
        rt.scroll_total = lines;
        app
    }

    #[test]
    fn find_matches_reports_every_hit_in_reading_order_by_char_column() {
        let lines: Vec<String> =
            ["alpha beta", "no hits here", "beta beta", "BETA shouting"].map(String::from).to_vec();
        assert_eq!(
            find_matches(&lines, "beta"),
            vec![(0, 6), (2, 0), (2, 5), (3, 0)],
            "reading order, non-overlapping, ASCII-case-insensitive"
        );
        // An empty needle matches nothing — an incremental search with
        // nothing typed has no results, not every position in the buffer.
        assert!(find_matches(&lines, "").is_empty());
        assert!(find_matches(&lines, "gamma").is_empty());
        // Columns are char offsets, not byte offsets.
        let wide = vec!["héllo beta".to_string()];
        assert_eq!(find_matches(&wide, "beta"), vec![(0, 6)]);
    }

    /// P21: typing filters incrementally and parks the view on the first
    /// hit; the offset is the one that puts that line on the view's top row.
    #[test]
    fn typing_a_query_jumps_the_view_to_the_first_hit() {
        let mut app = searchable_app(300);
        let id = app.focused;
        app.apply(Action::ScrollMode);
        app.handle_mode_key(key('/'));
        assert!(matches!(app.mode, Mode::Search { .. }), "`/` opens the prompt");
        type_query(&mut app, "line 42");
        let s = app.search.as_ref().expect("a search is running");
        assert_eq!(s.query, "line 42");
        // "line 42" is on haystack line 41 (0-based); "line 42x" doesn't
        // exist in 1..=300, so there is exactly one hit.
        assert_eq!(s.matches, vec![(41, 0)]);
        assert_eq!(app.scroll_offset(id), 300 - 41, "the hit sits on the view's top row");
    }

    /// P21: `n` past the last hit wraps to the first, `N` before the first
    /// wraps to the last — a search that dead-ends at the edge of the buffer
    /// makes you retype the query to keep looking.
    #[test]
    fn n_and_capital_n_wrap_around_the_match_list() {
        let mut app = searchable_app(300);
        app.apply(Action::ScrollMode);
        app.handle_mode_key(key('/'));
        type_query(&mut app, "line 1"); // 1, 1x, 1xx → many hits
        press(&mut app, crossterm::event::KeyCode::Enter);
        let n = app.search.as_ref().unwrap().matches.len();
        assert!(n > 3, "the fixture must have several hits, got {n}");
        assert_eq!(app.search.as_ref().unwrap().current, 0);
        // Walk forward off the end.
        for _ in 0..n {
            app.handle_mode_key(key('n'));
        }
        assert_eq!(app.search.as_ref().unwrap().current, 0, "`n` past the last hit wraps");
        // ...and backward off the front.
        app.handle_mode_key(key('N'));
        assert_eq!(app.search.as_ref().unwrap().current, n - 1, "`N` before the first wraps");
    }

    /// P21: every jump writes through `set_scrollback` and re-reads the
    /// grid's clamp — so a hit older than the banked history parks the view
    /// at the top of what exists, and the `↑N/M` the bar reads says so.
    /// Never a cached counter (U9).
    #[test]
    fn a_search_jump_goes_through_the_grid_clamped_path() {
        let mut app = searchable_app(300);
        let id = app.focused;
        app.apply(Action::ScrollMode);
        app.handle_mode_key(key('/')); // captures a 300-row haystack
                                       // Between the capture and the jump the pane floods and the ring
                                       // evicts almost all of that history — the documented drift case.
                                       // The arithmetic still asks for a row that is no longer banked.
        app.runtimes.get_mut(&id).unwrap().scroll_total = 10;
        type_query(&mut app, "line 42"); // wants offset 300 - 41 = 259
        assert_eq!(app.scroll_offset(id), 10, "the grid's clamp wins over the arithmetic");
        assert_eq!(app.scroll_position(), (10, 10), "and the ↑N/M hint reports the clamp");
        press(&mut app, crossterm::event::KeyCode::Enter);
        assert!(matches!(app.mode, Mode::Scroll), "Enter hands back to scroll mode");
        assert_eq!(app.scroll_offset(id), 10, "…on the view the search left behind");
    }

    /// P21: Esc restores the view the search opened on and leaves nothing
    /// behind; Enter keeps both the position and the search (so `n`/`N` and
    /// the highlights survive the prompt closing).
    #[test]
    fn esc_restores_the_pre_search_view_and_enter_keeps_it() {
        let mut app = searchable_app(300);
        let id = app.focused;
        app.wheel_scroll(id, 5);
        app.apply(Action::ScrollMode);
        app.handle_mode_key(key('/'));
        type_query(&mut app, "line 42");
        assert_eq!(app.scroll_offset(id), 259);
        press(&mut app, crossterm::event::KeyCode::Esc);
        assert_eq!(app.scroll_offset(id), 5, "Esc puts the view back");
        assert!(app.search.is_none(), "a cancelled search leaves no trace");
        assert!(matches!(app.mode, Mode::Scroll), "and hands scroll mode back");

        // Enter, by contrast, keeps both.
        app.handle_mode_key(key('/'));
        type_query(&mut app, "line 42");
        press(&mut app, crossterm::event::KeyCode::Enter);
        assert_eq!(app.scroll_offset(id), 259);
        assert!(app.search.is_some(), "the hits outlive the prompt");
    }

    /// P21: backspacing the query back to empty is a cancel of the
    /// filtering, not a jump to row zero — the view returns to where `/`
    /// was pressed and the hit list empties.
    #[test]
    fn backspacing_to_an_empty_query_returns_the_view_to_where_search_opened() {
        let mut app = searchable_app(300);
        let id = app.focused;
        app.wheel_scroll(id, 7);
        app.apply(Action::ScrollMode);
        app.handle_mode_key(key('/'));
        type_query(&mut app, "line 42");
        assert_eq!(app.scroll_offset(id), 259);
        for _ in 0.."line 42".len() {
            press(&mut app, crossterm::event::KeyCode::Backspace);
        }
        assert_eq!(app.search.as_ref().unwrap().query, "");
        assert!(app.search.as_ref().unwrap().matches.is_empty());
        assert_eq!(app.scroll_offset(id), 7, "back to the pre-search view");
    }

    /// P21: the prompt swallows the keys that mean "step" once it closes —
    /// `n`, `N` and `/` are query text while you are typing one.
    #[test]
    fn the_prompt_types_the_letters_that_are_bindings_outside_it() {
        let mut app = searchable_app(300);
        app.apply(Action::ScrollMode);
        app.handle_mode_key(key('/'));
        type_query(&mut app, "n/N");
        assert_eq!(app.search.as_ref().unwrap().query, "n/N");
        assert!(matches!(app.mode, Mode::Search { .. }), "none of them closed the prompt");
    }

    /// P21: `/` works from copy mode too, and Enter/Esc hand the copy cursor
    /// back untouched — a search is how you find the text you are about to
    /// select, so it must not cost you your place.
    #[test]
    fn search_from_copy_mode_returns_the_cursor_it_borrowed() {
        let mut app = searchable_app(300);
        app.apply(Action::CopyMode);
        let Mode::Copy { cursor } = app.mode else { panic!("in copy mode") };
        app.handle_mode_key(key('/'));
        type_query(&mut app, "line 42");
        press(&mut app, crossterm::event::KeyCode::Enter);
        let Mode::Copy { cursor: back } = app.mode else { panic!("copy mode handed back") };
        assert_eq!(back, cursor);
        assert!(app.search.is_some(), "and the hits survive for `n`/`N` in copy mode");
    }

    /// P21 × P5: a resize reflows the live grid, so a search's captured row
    /// indices stop meaning anything — they are re-captured against the grid
    /// as it is now, rather than left to paint highlights on the wrong
    /// lines. The view is not re-jumped (the reflow already re-clamped it).
    #[test]
    fn a_resize_recaptures_the_searchs_haystack_instead_of_trusting_stale_rows() {
        let mut app = searchable_app(300);
        let id = app.focused;
        app.apply(Action::ScrollMode);
        app.handle_mode_key(key('/'));
        type_query(&mut app, "line 42");
        assert_eq!(app.search.as_ref().unwrap().matches, vec![(41, 0)]);
        let after_jump = app.scroll_offset(id);

        // The "reflow": the same content now occupies different rows.
        let rt = app.runtimes.get_mut(&id).unwrap();
        let rewrapped: Vec<String> = (1..=300).map(|i| format!("... line {i}")).collect();
        rt.all_text =
            std::iter::once("banner".to_string()).chain(rewrapped).collect::<Vec<_>>().join("\n");
        rt.scroll_total = 301;
        app.on_resize(Size::new(80, 24), (0, 0));

        let s = app.search.as_ref().expect("the search survives a resize");
        assert_eq!(s.matches, vec![(42, 4)], "hits are re-found on the reflowed grid");
        assert_eq!(s.current, 0);
        assert_eq!(app.scroll_offset(id), after_jump, "a resize does not re-jump the view");
    }

    /// P21: leaving the look-back modes ends the search — its hits are line
    /// offsets into one pane's history and mean nothing once you are not
    /// looking at it. The Scroll→Copy handoff is the documented exception
    /// (U9's exemption: that path exists so history can be found *and* then
    /// selected).
    #[test]
    fn leaving_scroll_mode_ends_the_search_except_across_the_copy_handoff() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = searchable_app(300);
        app.apply(Action::ScrollMode);
        app.handle_mode_key(key('/'));
        type_query(&mut app, "line 42");
        press(&mut app, KeyCode::Enter);
        app.handle_mode_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.search.is_none(), "Esc out of scroll mode drops the search");

        // Alt+c: the frozen view AND the hits carry over into copy mode.
        app.apply(Action::ScrollMode);
        app.handle_mode_key(key('/'));
        type_query(&mut app, "line 42");
        press(&mut app, KeyCode::Enter);
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::ALT));
        app.apply(Action::CopyMode);
        assert!(app.search.is_some(), "the Scroll→Copy handoff keeps the search");
        assert_eq!(app.scroll_offset(app.focused), 259, "and its view");

        // Any other Alt chord ends it (and snaps the view back).
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT));
        assert!(app.search.is_none());
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
        assert!(matches!(app.mode, Mode::Scroll), "scroll mode");
        assert_eq!(app.scroll_offset(id), 24, "entering the mode moved nothing");
        app.handle_mode_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.runtimes.get(&id).unwrap().scrollback, 25);
    }

    /// P14, the invariant made structural: **every scroll step reads the
    /// view's current offset off the grid, never a number roost stashed
    /// between two keypresses.**
    ///
    /// The grid auto-compensates its own offset as new rows bank while the
    /// view is scrolled — that is what keeps the view pinned to the content
    /// you were reading instead of letting it slide. Anything roost cached
    /// before that happened would go stale the moment it does, walking a
    /// step seeded from it toward the tail instead (measured at 4 lines on
    /// one Up) — `Mode::Scroll` holds no such cache, and this pins the
    /// behaviour that fact guarantees.
    #[test]
    fn a_scroll_step_reads_the_grid_after_it_auto_advanced_under_new_output() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.wheel_scroll(id, 5);
        app.apply(Action::ScrollMode);
        assert_eq!(app.scroll_offset(id), 5);

        // The pane emits; the grid banks 7 rows and carries the view with
        // them, exactly as the vendored parser does. (`FakePane` has no real
        // grid, so the compensation is applied directly — it is the
        // *backend's* behaviour being modelled, not roost's.)
        app.runtimes.get_mut(&id).unwrap().scrollback = 12;

        app.handle_mode_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(
            app.scroll_offset(id),
            13,
            "one row further back than where the view actually is (12), not than \
             where it was when the mode opened (5)",
        );
        app.handle_mode_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.scroll_offset(id), 12, "and back down from the same truth");
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

    /// U17: the word motions' exact boundaries, including both clamps —
    /// copy-mode motions live in visible-grid cell space and never wrap onto
    /// a neighbouring row, so running off either end parks on that end.
    #[test]
    fn word_motions_walk_whole_words_and_clamp_at_both_row_ends() {
        use super::{word_back, word_end, word_forward};
        let line = "hello world  foo";
        let last = 97; // a 98-column row: the text's blank tail is not a word

        assert_eq!(word_forward(line, 0, last), 6, "start of the next word");
        assert_eq!(word_forward(line, 6, last), 13, "a two-space gap is still one gap");
        assert_eq!(word_forward(line, 13, last), last, "past the last word: clamped");
        assert_eq!(word_forward("", 0, last), last, "an empty row has nowhere to go");

        assert_eq!(word_back(line, last), 13, "back from the blank tail: the last word");
        assert_eq!(word_back(line, 13), 6);
        assert_eq!(word_back(line, 6), 0);
        assert_eq!(word_back(line, 0), 0, "before the first word: clamped");
        assert_eq!(word_back(line, 3), 0, "from mid-word, to that word's start");

        assert_eq!(word_end(line, 0, last), 4, "end of the word we are in");
        assert_eq!(word_end(line, 4, last), 10, "then the end of the next");
        assert_eq!(word_end(line, 10, last), 15);
        assert_eq!(word_end(line, 15, last), last, "no word left: clamped");
        assert_eq!(word_end(line, last, last), last);
    }

    /// U17: `w`/`b`/`e` on the copy cursor, driven through the real key
    /// path — they were unbound no-ops before, so a keyboard selection had
    /// to be walked out one column at a time.
    #[test]
    fn copy_mode_w_b_e_move_the_cursor_by_words() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().grab = "hello world  foo".into();
        let (_, w) = inner_dims(app.rects()[0].rect);
        let last = w - 1;
        app.apply(Action::CopyMode);
        assert_eq!(copy_cursor(&app).1, 0);

        let press = |app: &mut App<FakePane>, c: char| {
            app.handle_mode_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        };
        press(&mut app, 'w');
        assert_eq!(copy_cursor(&app).1, 6);
        press(&mut app, 'e');
        assert_eq!(copy_cursor(&app).1, 10, "e lands on the last cell of that word");
        press(&mut app, 'w');
        assert_eq!(copy_cursor(&app).1, 13);
        press(&mut app, 'w');
        assert_eq!(copy_cursor(&app).1, last, "past the last word, clamped to the row end");
        press(&mut app, 'b');
        assert_eq!(copy_cursor(&app).1, 13);
        for _ in 0..4 {
            press(&mut app, 'b');
        }
        assert_eq!(copy_cursor(&app).1, 0, "clamped at column 0, never wrapping up a row");
        assert!(matches!(app.mode, Mode::Copy { .. }), "motions never leave the mode");
    }

    /// U17: `V` grabs the cursor's whole line — the "copy what the agent
    /// just said" gesture — and, like every other motion, a following
    /// `j`/`k` extends the selection from there.
    #[test]
    fn copy_mode_capital_v_selects_the_whole_row_and_extends_by_lines() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        let (_, w) = inner_dims(app.rects()[0].rect);
        let last = w - 1;
        app.apply(Action::CopyMode);
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        let row = copy_cursor(&app).0;

        app.handle_mode_key(KeyEvent::new(KeyCode::Char('V'), KeyModifiers::NONE));
        let sel = app.selection.expect("V marks a selection");
        assert_eq!((sel.anchor, sel.cursor), ((row, 0), (row, last)), "column 0 through the end");
        assert!(!sel.dragging);
        assert_eq!(copy_cursor(&app), (row, last), "the cursor rides the line's end");

        // Movement extends it, exactly as it extends a `v` selection.
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(app.selection.unwrap().cursor, (row - 1, last));
        assert_eq!(app.selection.unwrap().anchor, (row, 0), "the anchor stays put");

        // Idempotent: V again re-takes just the row the cursor is on now.
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('V'), KeyModifiers::NONE));
        let sel = app.selection.unwrap();
        assert_eq!((sel.anchor, sel.cursor), ((row - 1, 0), (row - 1, last)));
    }

    /// U18: every mode's own entry chord toggles it off. Only Alt+e had this
    /// rule; Alt+c re-entered copy mode with a fresh cursor and Alt+PageUp
    /// re-entered scroll mode at the live tail — both indistinguishable from
    /// "the chord did nothing".
    #[test]
    fn a_modes_own_entry_chord_exits_it() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        const ALT: KeyModifiers = KeyModifiers::ALT;
        let cases: [(Action, KeyEvent); 7] = [
            (Action::EditPane, KeyEvent::new(KeyCode::Char('r'), ALT)),
            (Action::RenameTab, KeyEvent::new(KeyCode::Char('r'), ALT.union(KeyModifiers::SHIFT))),
            (Action::QuickLaunch, KeyEvent::new(KeyCode::Enter, ALT)),
            (Action::ScrollMode, KeyEvent::new(KeyCode::PageUp, ALT)),
            (Action::CopyMode, KeyEvent::new(KeyCode::Char('c'), ALT)),
            (Action::Help, KeyEvent::new(KeyCode::Char('?'), ALT)),
            (Action::ToggleFeed, KeyEvent::new(KeyCode::Char('e'), ALT)),
        ];
        for (enter, chord) in cases {
            let (mut app, _) = mk_app(shell_ws());
            app.apply(enter);
            assert!(!matches!(app.mode, Mode::Normal), "{enter:?} must open a mode");
            let consumed = app.handle_mode_key(chord);
            assert!(consumed, "{enter:?}: the entry chord is consumed, not re-dispatched");
            assert!(matches!(app.mode, Mode::Normal), "{enter:?}: the chord must exit the mode");
        }
    }

    /// U18: exiting by entry chord is exiting — the mode's own cleanup runs,
    /// so a toggled-off scroll snaps back to the live tail and a toggled-off
    /// copy drops its selection, exactly as Esc does.
    #[test]
    fn esc_and_the_entry_chord_leave_identical_state() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        for exit in [
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::ALT),
        ] {
            let (mut app, _) = mk_app(shell_ws());
            let id = app.focused;
            app.apply(Action::ScrollMode);
            app.handle_mode_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
            assert_eq!(app.runtimes.get(&id).unwrap().scrollback, 1);
            app.handle_mode_key(exit);
            assert!(matches!(app.mode, Mode::Normal));
            assert_eq!(app.runtimes.get(&id).unwrap().scrollback, 0, "{exit:?} snaps to live");
        }
        for exit in [
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::ALT),
        ] {
            let (mut app, _) = mk_app(shell_ws());
            app.apply(Action::CopyMode);
            app.handle_mode_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
            assert!(app.selection.is_some());
            app.handle_mode_key(exit);
            assert!(matches!(app.mode, Mode::Normal));
            assert!(app.selection.is_none(), "{exit:?} drops the selection");
        }
    }

    /// U18's generalization must not swallow chords that are NOT the current
    /// mode's own: they still break out to their global binding, mode reset
    /// and scroll-snap included.
    #[test]
    fn another_modes_entry_chord_still_falls_through_to_its_global_binding() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleFeed);
        // Alt+? is Help's chord, not the feed's: not consumed here, so the
        // dispatcher opens Help next.
        let consumed = app.handle_mode_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::ALT));
        assert!(!consumed);
        assert!(matches!(app.mode, Mode::Normal));
        app.apply(Action::Help);
        assert!(matches!(app.mode, Mode::Help { .. }));
    }

    // ---- C15 (amended): the keymap scrolls only when it must -------------

    /// On a terminal too short for the whole table, the scroll keys read on
    /// instead of dismissing — and every other key still closes it, so
    /// C15's "any key closes it" is narrowed by exactly the keys that have
    /// somewhere to go and by nothing else.
    #[test]
    fn the_keymap_scrolls_on_a_short_terminal_and_still_closes_on_any_other_key() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, _) = mk_app(shell_ws());
        let (visible, total) =
            crate::ui::render::help_scroll_extent(app.body_area(), app.keymap(), None);
        assert!(visible < total, "this fixture's body is too short for the whole keymap");

        app.apply(Action::Help);
        assert!(matches!(app.mode, Mode::Help { top: 0, .. }));
        app.handle_mode_key(KeyEvent::from(KeyCode::Down));
        assert!(matches!(app.mode, Mode::Help { top: 1, .. }), "↓ read on");
        app.handle_mode_key(KeyEvent::from(KeyCode::PageDown));
        let Mode::Help { top, .. } = app.mode else { panic!("PgDn kept it open") };
        assert!(top > 1, "PgDn paged further than ↓ did");
        app.handle_mode_key(KeyEvent::from(KeyCode::Home));
        assert!(matches!(app.mode, Mode::Help { top: 0, .. }), "Home went back to the first row");
        app.handle_mode_key(KeyEvent::from(KeyCode::End));
        assert!(
            matches!(app.mode, Mode::Help { top: t, .. } if t == total - visible),
            "End went to the last screenful",
        );
        // …and at the bottom, ↓ has nowhere to go, so it closes — the same
        // key, dismissing exactly when it stops being a scroll key.
        app.handle_mode_key(KeyEvent::from(KeyCode::Down));
        assert!(matches!(app.mode, Mode::Normal), "↓ at the end closes");

        app.apply(Action::Help);
        app.handle_mode_key(KeyEvent::from(KeyCode::Char('x')));
        assert!(matches!(app.mode, Mode::Normal), "an ordinary key still closes it");
    }

    /// …and on a terminal that shows the whole table, the amendment is
    /// invisible: every key closes it, arrows included, exactly as before.
    #[test]
    fn the_keymap_closes_on_every_key_when_the_whole_table_fits() {
        use crossterm::event::{KeyCode, KeyEvent};
        let mut ws = shell_ws();
        ws.tabs[0].name = "main".into();
        let (mut app, _) = mk_app(ws);
        app.on_resize(Size::new(120, 60), (0, 0));
        let (visible, total) =
            crate::ui::render::help_scroll_extent(app.body_area(), app.keymap(), None);
        assert_eq!(visible, total, "a tall body shows the whole keymap at once");
        for key in [KeyCode::Down, KeyCode::PageDown, KeyCode::End, KeyCode::Char('j')] {
            app.apply(Action::Help);
            app.handle_mode_key(KeyEvent::from(key));
            assert!(matches!(app.mode, Mode::Normal), "{key:?} closes when nothing scrolls");
        }
    }

    #[test]
    fn scrolling_then_a_global_chord_snaps_the_pane_back_to_live() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.apply(Action::ScrollMode);
        app.handle_mode_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)); // scroll back 1
        assert!(matches!(app.mode, Mode::Scroll));
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

    /// A registered adapter whose `launch`/`resume` always name FakePane's
    /// dedicated spawn-failure hook (`program == "spawn-fail"`) — deterministic
    /// and hermetic, unlike coercing this through ShellAdapter's real `$SHELL`
    /// (every other test's `mk_app(shell_ws())` reads that same process-global
    /// var while constructing its own app, which is exactly the kind of
    /// cross-test race a mutated env var risks).
    struct AlwaysFailsAdapter;
    impl agents::AgentAdapter for AlwaysFailsAdapter {
        fn id(&self) -> &'static str {
            "always-fails"
        }
        fn launch(&self, cwd: &Path) -> agents::CommandSpec {
            agents::CommandSpec::new("spawn-fail", cwd)
        }
        fn resume(&self, cwd: &Path, _session: &str) -> agents::CommandSpec {
            self.launch(cwd)
        }
        fn resume_flag(&self) -> &'static str {
            "--resume" // unused: resume() overridden above
        }
    }

    /// P0-2 / P0-3 end-to-end: a synchronous PTY spawn failure must be
    /// visible over the socket — in the `spawn` reply itself (not just
    /// `{"pane": id}`), and again on every later `status`/`list` poll, and
    /// on the feed. All from the one place `spawn_pane`'s `Err` arm runs.
    #[test]
    fn ctl_spawn_status_list_and_feed_all_surface_a_synchronous_spawn_failure() {
        use crate::core::control::{Method, Reply, Request};
        let mut registry = agents::registry();
        registry.insert("always-fails", Box::new(AlwaysFailsAdapter));
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut app = App::<FakePane>::new(
            shell_ws(),
            registry,
            Box::new(MemStore::default()),
            tx,
            Size::new(100, 30),
            (0, 0),
            None,
            TokenTable::new().unwrap(),
        )
        .unwrap();
        let ct = app.control_token().to_string();
        let ok = |r: Reply| match r {
            Reply::Ok { ok } => ok,
            Reply::Err { err } => panic!("expected ok, got err: {err}"),
        };

        // P0-2: the spawn reply already knows — spawn_pane ran synchronously
        // inside spawn_child, before this reply was ever built.
        let spawned = ok(app.handle_control(Request {
            token: ct.clone(),
            method: Method::Spawn {
                adapter: "always-fails".into(),
                cwd: None,
                initial_input: None,
            },
        }));
        let id = spawned["pane"].as_u64().unwrap();
        // FakePane::spawn's hook is a bare `anyhow::bail!` — no chain to
        // walk, so this also pins that a plain, unchained message still
        // survives `dead_reason` unchanged (the chained case is
        // `dead_reason_keeps_the_real_cause_reachable`, and the real chain
        // pty.rs actually produces is `spawn_failure_keeps_the_real_cause_
        // reachable_via_alternate_format` in infra::pty).
        assert_eq!(spawned["error"].as_str(), Some("spawn-fail requested"));

        // And a caller that only polls afterward gets the same reason.
        let status = ok(app.handle_control(Request {
            token: ct.clone(),
            method: Method::Status { pane: Some(id) },
        }));
        assert_eq!(status["status"], "exited");
        assert_eq!(status["error"].as_str(), Some("spawn-fail requested"));

        let list = ok(app.handle_control(Request { token: ct, method: Method::List }));
        let entry =
            list.as_array().unwrap().iter().find(|p| p["pane"].as_u64() == Some(id)).unwrap();
        assert_eq!(entry["error"].as_str(), Some("spawn-fail requested"));

        // P0-2's "ideally the activity feed" half.
        assert!(
            app.feed().iter().any(|e| e.text == "spawn failed: spawn-fail requested"),
            "{:?}",
            app.feed()
        );
    }

    /// The non-regression this all rides on: a healthy pane's `error` is
    /// `null`, not just absent-when-convenient — `status`/`list` must not
    /// start crying wolf for panes that spawned fine.
    #[test]
    fn pane_json_and_status_report_a_null_error_for_a_healthy_pane() {
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        let ct = app.control_token().to_string();
        let ok = |r: Reply| match r {
            Reply::Ok { ok } => ok,
            Reply::Err { err } => panic!("expected ok, got err: {err}"),
        };
        let id = app.focused;
        let status = ok(app.handle_control(Request {
            token: ct.clone(),
            method: Method::Status { pane: Some(id) },
        }));
        assert!(status["error"].is_null());
        let list = ok(app.handle_control(Request { token: ct, method: Method::List }));
        assert!(list.as_array().unwrap()[0]["error"].is_null());
    }

    /// QA-2 through the real control path: `roost spawn --cwd <nonexistent>`
    /// must refuse loudly, naming the path, and leave no pane behind for the
    /// orchestrator to (wrongly) think is running there.
    #[test]
    fn ctl_spawn_refuses_a_nonexistent_cwd_and_creates_no_pane() {
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        let ct = app.control_token().to_string();
        let before = app.runtimes.len();
        let missing =
            std::env::temp_dir().join(format!("roost-ctl-cwd-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&missing);

        let reply = app.handle_control(Request {
            token: ct,
            method: Method::Spawn {
                adapter: "shell".into(),
                cwd: Some(missing.to_string_lossy().into_owned()),
                initial_input: None,
            },
        });

        match reply {
            Reply::Err { err } => assert!(err.contains(&missing.display().to_string()), "{err:?}"),
            Reply::Ok { ok } => panic!("expected a refusal, got ok: {ok}"),
        }
        assert_eq!(app.runtimes.len(), before, "no pane left behind for a rejected cwd");
    }

    /// The happy path around the QA-2 fix: a `--cwd` that IS a real
    /// directory spawns exactly as before.
    #[test]
    fn ctl_spawn_accepts_a_real_cwd() {
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        let ct = app.control_token().to_string();
        let reply = app.handle_control(Request {
            token: ct,
            method: Method::Spawn {
                adapter: "shell".into(),
                cwd: Some(std::env::temp_dir().to_string_lossy().into_owned()),
                initial_input: None,
            },
        });
        let Reply::Ok { ok } = reply else { panic!("a real cwd must not be refused") };
        assert!(ok["pane"].as_u64().is_some());
    }

    #[test]
    fn control_read_full_and_tail_reach_scrollback_not_just_the_screen() {
        // M1: Full/Tail must reach past the visible grid (`grab_text` alone
        // only sees the screen), or an orchestrator's read(tail:20) misses
        // anything already scrolled off. FakePane's `grab` (screen) and
        // `all_text` (full history) are deliberately different here so a
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
        app.tokens.set_pane_token(1, "tok1".into());
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
        app.tokens.set_pane_token(1, "tok1".into());
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
            app.handle_control(Request {
                token: ct,
                method: Method::Close { pane: 1, force: true }
            }),
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

        match app
            .handle_control(Request { token: ct, method: Method::Fork { pane: Some(spawned) } })
        {
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
            TokenTable::new().unwrap(),
        )
        .unwrap();
        let ct = app.control_token().to_string();

        // A denied wait (unknown pane) must be audited as a denial (M3), not
        // an unconditional "ok ... parked" logged regardless of outcome.
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

    /// The audit log was append-only and unbounded — fine for a human, but a
    /// scripted orchestrator looping `send`/`read` grows it forever.
    /// Rotation keeps one generation, so the trail costs at most twice the
    /// cap on disk and the *most recent* history is always the part kept.
    #[test]
    fn the_audit_log_rotates_once_it_passes_its_cap_and_keeps_one_generation() {
        let dir = std::env::temp_dir().join(format!("roost-audit-rot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("control.log");
        let old = dir.join("control.log.1");

        // Under the cap: nothing moves, however many times it is checked.
        std::fs::write(&log, b"a line\n").unwrap();
        super::rotate_audit_log(&log, 64);
        assert!(log.exists() && !old.exists(), "a small log is left alone");

        // Past it: the live file becomes the kept generation and the next
        // write starts a fresh one.
        std::fs::write(&log, vec![b'x'; 64]).unwrap();
        super::rotate_audit_log(&log, 64);
        assert!(!log.exists(), "the live file was rotated away");
        assert_eq!(std::fs::metadata(&old).unwrap().len(), 64, "…into .1, intact");

        // Rotating again replaces the previous generation rather than
        // accumulating .2, .3, … — the bound is two files, not a series.
        std::fs::write(&log, vec![b'y'; 64]).unwrap();
        super::rotate_audit_log(&log, 64);
        assert_eq!(std::fs::read(&old).unwrap(), vec![b'y'; 64], ".1 is the newer generation");
        assert!(!dir.join("control.log.2").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn broadcast_audit_line_has_the_contracted_shape_and_omits_the_text() {
        use crate::core::control::{Method, Reply, Request};
        let dir =
            std::env::temp_dir().join(format!("roost-broadcast-audit-{}", std::process::id()));
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
            TokenTable::new().unwrap(),
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
        assert!(
            lines[0].contains("fleet broadcast len=14 submit=true -> ok count=1"),
            "{}",
            lines[0]
        );
        assert!(!log.contains("secret payload"), "broadcast text must never be logged: {log:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn broadcast_pushes_exactly_one_ctl_feed_line() {
        // Pinned on the C20 feed side: a broadcast must call `audit()`
        // exactly once — `audit()` also feeds C20, so a second call would
        // show up as two near-identical `ctl` rows in the activity overlay.
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        let ct = app.control_token().to_string();
        let (rtx, rrx) = std::sync::mpsc::channel();
        app.handle_control_msg(
            Request { token: ct, method: Method::Broadcast { text: "hi".into(), submit: false } },
            rtx,
        );
        assert!(matches!(rrx.recv().unwrap(), Reply::Ok { .. }));
        let ctl_lines: Vec<&FeedEntry> =
            app.feed().iter().filter(|e| e.text.starts_with("ctl ")).collect();
        assert_eq!(ctl_lines.len(), 1, "exactly one ctl feed line per broadcast: {:?}", app.feed());
        assert!(
            ctl_lines[0].text.contains("fleet: broadcast len=2 submit=false → ok"),
            "{}",
            ctl_lines[0].text
        );
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
        let alpha =
            transitions.iter().find(|e| e.text.contains("alpha:")).expect("alpha's own line");
        assert_eq!(alpha.text, format!("{a} alpha: idle → working"));
        assert!(!alpha.needs_input);
        let charlie =
            transitions.iter().find(|e| e.text.contains("charlie:")).expect("charlie's own line");
        assert_eq!(charlie.text, format!("{c} charlie: idle → needs you"));
        assert!(charlie.needs_input);
        assert!(
            !transitions.iter().any(|e| e.text.contains("bravo:")),
            "unchanged pane must stay silent"
        );

        // The broadcast's one ctl line must still be exactly one — untouched by the tick.
        assert_eq!(app.feed().iter().filter(|e| e.text.starts_with("ctl ")).count(), 1);
    }

    /// [design-supervisor D5] The feed reads `display_status`, not the raw
    /// runtime status — the same fix D1 gave `tab_summary`. Left alone, a
    /// shell's heuristic `Waiting` logged `{name}: working → your turn`
    /// (P2-10's wrongness, on the one chrome surface the original pass
    /// didn't reach) while every other surface, same frame, said `idle`.
    #[test]
    fn diff_statuses_logs_a_quiet_shells_waiting_as_idle_not_your_turn() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.pane_order()[0];
        let is_transition = |e: &&FeedEntry| e.text.contains('→') && !e.text.starts_with("ctl ");

        // Baseline tick: first observation is silently seeded.
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();
        assert_eq!(app.feed().iter().filter(is_transition).count(), 0);

        app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::Working);
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();

        app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::Waiting);
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();

        let transitions: Vec<&FeedEntry> = app.feed().iter().filter(is_transition).collect();
        let last = transitions.last().expect("a transition line");
        assert_eq!(
            last.text,
            format!("{}: working → idle", app.feed_label(id)),
            "not \"your turn\""
        );
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
        app.tokens.set_pane_token(1, "tok1".into());
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
        app.tokens.set_pane_token(1, "secret-1".into());
        app.tokens.set_pane_token(2, "secret-2".into());
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

    // -- MC-ordering (I2: mint-before-child) --------------------------------
    //
    // `B::spawn` is a plain associated function — it gets no `&App`, so the
    // only way to observe "was this pane's token already published when I
    // was called" from inside it is a channel set up before the call. A
    // thread-local is exactly that: this test drives everything (App::new,
    // spawn_active_tab, B::spawn) synchronously on its own thread, so
    // there's no cross-thread visibility question to worry about.
    use crate::core::control::TokenReader;
    thread_local! {
        static ORDERING_READER: std::cell::RefCell<Option<TokenReader>> = const { std::cell::RefCell::new(None) };
    }

    /// A minimal `PaneBackend` whose only interesting behavior is `spawn`:
    /// it asserts the pane's `ROOST_TOKEN` (read off the real `CommandSpec`
    /// `spawn_pane` built, same as a real child process would receive it)
    /// already authorizes that pane in the table `ORDERING_READER` points
    /// at. Every other method is an inert stand-in — this fixture exists to
    /// pin one invariant, not to run a session.
    struct OrderingCheckPane;

    impl PaneBackend for OrderingCheckPane {
        fn spawn(
            id: PaneId,
            cmd: &crate::agents::CommandSpec,
            _rows: u16,
            _cols: u16,
            _pixels: (u16, u16),
            _tx: SyncSender<AppEvent>,
        ) -> Result<Self> {
            if let Some((_, token)) = cmd.env.iter().find(|(k, _)| k == "ROOST_TOKEN") {
                ORDERING_READER.with(|cell| {
                    if let Some(reader) = cell.borrow().as_ref() {
                        assert!(
                            reader.load().pane_authorized(id, token),
                            "I2 violated: pane {id}'s token must be published in the \
                             TokenTable strictly before B::spawn runs — the mint \
                             (spawn_pane) must never be reordered after the spawn, \
                             or deferred"
                        );
                    }
                });
            }
            Ok(OrderingCheckPane)
        }
        fn process_output(&mut self, _bytes: &[u8]) {}
        fn write_input(&mut self, _bytes: &[u8]) -> bool {
            true
        }
        fn write_input_raw(&mut self, _bytes: &[u8]) -> bool {
            true
        }
        fn resize(&mut self, _rows: u16, _cols: u16, _pixels: (u16, u16)) {}
        fn kill(&mut self) {}
        fn status(&self) -> AgentStatus {
            AgentStatus::Idle
        }
        fn set_extension_status(&mut self, _s: AgentStatus) {}
        fn on_exit(&mut self) {}
        fn screen(&self) -> Option<&vt100::Screen> {
            None
        }
        fn set_scrollback(&mut self, _lines: usize) {}
        fn scroll_by(&mut self, _delta: i32) {}
        fn scroll_offset(&self) -> usize {
            0
        }
        fn scroll_total(&self) -> usize {
            0
        }
        fn mouse_proto(&self) -> crate::ports::MouseProto {
            crate::ports::MouseProto::None
        }
    }

    /// MC-ordering: reds if anyone reorders the mint (app.rs's `spawn_pane`,
    /// currently before `B::spawn`) to run after it, or adds a deferred
    /// publish — `OrderingCheckPane::spawn`'s assertion would then fire on
    /// the very first pane `App::new` spawns.
    #[test]
    fn mc_ordering_pane_token_is_published_in_the_table_before_the_child_spawns() {
        let table = TokenTable::new().unwrap();
        ORDERING_READER.with(|cell| *cell.borrow_mut() = Some(table.reader()));

        let store = MemStore::default();
        let (tx, _rx) = mpsc::sync_channel(64);
        // Token-minting is gated on a real control socket (`sock_path`) —
        // see `spawn_pane` — so this must be `Some` for the mint (the
        // invariant under test) to run at all.
        let app = App::<OrderingCheckPane>::new(
            shell_ws(),
            agents::registry(),
            Box::new(store),
            tx,
            Size::new(100, 30),
            (0, 0),
            Some(std::path::PathBuf::from("/tmp/roost-mc-ordering-test.sock")),
            table,
        )
        .unwrap();
        // If execution reaches here, `OrderingCheckPane::spawn`'s assertion
        // never fired for any pane `App::new` spawned — the real proof this
        // test exists for. This just confirms the setup actually exercised
        // that path at all.
        assert!(!app.runtimes.is_empty(), "setup: at least one pane must have spawned");

        ORDERING_READER.with(|cell| *cell.borrow_mut() = None);
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

    /// [F1] The bug a bare bool had: two connections reporting for one pane
    /// (a nested claude's one-shot `roost __status` hooks alongside the
    /// pane's own long-lived pi extension link — both inherit the same
    /// ROOST_PANE/ROOST_TOKEN). The first to close must not clear the link
    /// while the second is still open.
    #[test]
    fn two_overlapping_links_for_one_pane_the_first_to_close_leaves_it_live() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.on_status_link(id, true); // the long-lived pi extension connects
        app.on_status_link(id, true); // a nested claude's one-shot connects
        assert!(app.runtimes.get(&id).unwrap().ext_link);
        app.on_status_link(id, false); // the one-shot disconnects
        assert!(
            app.runtimes.get(&id).unwrap().ext_link,
            "the pane's link must stay live while the other connection is still open"
        );
        app.on_status_link(id, false); // the long-lived connection finally closes too
        assert!(!app.runtimes.get(&id).unwrap().ext_link, "down once every connection has closed");
    }

    /// [F1] The reviewer's probe, pinned permanently: a one-shot connection's
    /// up/down landing *during* an already-live long-lived connection must
    /// never revert the pane's link to down — the exact silent revert that
    /// would have brought the D1 phantom-pulse bug back.
    #[test]
    fn a_one_shot_link_during_a_live_connection_never_reverts_it_down() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.on_status_link(id, true); // long-lived connection: up, and stays up
        assert!(app.runtimes.get(&id).unwrap().ext_link);
        app.on_status_link(id, true); // nested one-shot: up
        app.on_status_link(id, false); // nested one-shot: down, same tick
        assert!(
            app.runtimes.get(&id).unwrap().ext_link,
            "a one-shot's up/down must not revert the long-lived connection's link to down"
        );
    }

    /// [F1 residual, probe-confirmed] A respawn re-mints the pane's token
    /// synchronously (`spawn_pane`), so an old connection's eventual
    /// link-down — arriving after the respawn, carrying the OLD token —
    /// can never authenticate again: `socket_authorized` rejects it exactly
    /// like `main.rs`'s real event-loop gate does, so `on_status_link`
    /// never even sees it and the decrement never lands. Without clearing
    /// `ext_link_counts` in `spawn_pane` itself, that stranded count (and
    /// so `ext_link`) stays stuck on for the rest of the pane's life —
    /// byte-promotion permanently disabled, and the destructive-close guard
    /// (`vouched_live`) permanently trusting a report nothing backs anymore.
    #[test]
    fn respawn_clears_a_refcount_the_old_tokens_link_down_can_no_longer_reach() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        // Token-minting is gated on a real control socket (`sock_path`),
        // which `mk_app` leaves `None` — set one so `spawn_pane`'s real
        // token-reissue path (the one under test) actually runs on respawn.
        app.sock_path = Some(std::path::PathBuf::from("/tmp/roost-test.sock"));
        let old_token = "old-token".to_string();
        app.tokens.set_pane_token(id, old_token.clone());

        app.on_status_link(id, true); // the pane's extension connects
        assert!(app.runtimes.get(&id).unwrap().ext_link);

        app.respawn_focused(false); // kills the old process, mints a new token
                                    // TokenTable exposes no by-id getter (sock.rs's door only ever needs
                                    // "does this token authorize this pane", never "what is pane N's
                                    // token") — read the freshly minted one the same way a real child
                                    // process would receive it: off the spawned command's own env.
        let new_token = app
            .runtimes
            .get(&id)
            .unwrap()
            .cmd
            .env
            .iter()
            .find(|(k, _)| k == "ROOST_TOKEN")
            .expect("respawn must reissue ROOST_TOKEN")
            .1
            .clone();
        assert_ne!(old_token, new_token, "setup: respawn must rotate the token");

        // The old connection's Drop link-down finally arrives, carrying the
        // token it was actually admitted under — rejected exactly like
        // main.rs's real gate would reject it.
        assert!(!app.socket_authorized(id, &old_token), "the old token must not survive a respawn");

        assert!(
            !app.ext_link_counts.contains_key(&id),
            "the refcount must not be stranded by the respawn: {:?}",
            app.ext_link_counts.get(&id)
        );
        assert!(
            !app.runtimes.get(&id).unwrap().ext_link,
            "must not be stuck live with zero connections actually able to prove themselves"
        );

        // A fresh connection under the NEW token still works normally.
        assert!(app.socket_authorized(id, &new_token));
        app.on_status_link(id, true);
        assert!(app.runtimes.get(&id).unwrap().ext_link);
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
        let saved_session =
            store.0.lock().unwrap().clone().unwrap().tabs[0].panes[&1].session.clone();

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

    /// `y` on the dead-pane bar copies the adapter's own resume command with
    /// a `cd` prefix — the same `resume()` Enter runs, rendered pasteable.
    /// Panes without a session pointer (shells, agents that died before
    /// detection) return None, which is what hides `y` on both bars.
    /// Session is planted via `set_session` (post-spawn), not the workspace,
    /// so SessionResolver's on-disk check can't clear it on this machine.
    #[test]
    fn resume_command_line_is_cd_plus_adapter_resume_or_none() {
        let (mut app, _) = mk_app(shell_ws());
        assert_eq!(app.resume_command_line(1), None, "shell pane: no session, no y");
        app.spawn_child("pi", None, None);
        let id = app.focused;
        assert_eq!(app.resume_command_line(id), None, "agent without a learned session: no y");
        app.set_session(id, "019fe044-8236".into());
        assert_eq!(
            app.resume_command_line(id).as_deref(),
            Some("cd -- /tmp && pi --session 019fe044-8236")
        );
        // A socket-planted, flag-shaped "session" must never reach a shell
        // line — the same valid_session_id gate the spawn path has.
        app.set_session(id, "--config=/tmp/evil".into());
        assert_eq!(app.resume_command_line(id), None, "hostile id: no copy, no y");
    }

    // -- characterisation: spawn_pane's session-state decision table -------
    // Pins the private logic ROADMAP's "[health] Extract SessionResolver"
    // targets, across every branch it can take, BEFORE that logic moves.
    // `stale_session_falls_back_to_fresh_launch` above already covers Gone
    // end-to-end through the real pi adapter; these use a lighter, fully
    // hermetic fixture to cover the rest (and restate Gone for symmetry).

    /// Test-only adapter whose session root is caller-controlled: `None` (no
    /// root at all) or any path, readable or not — so `spawn_pane`'s
    /// resume/stale/detect branches can be driven deterministically without
    /// touching a real `~/.pi`/`~/.claude`. Mirrors `agents::tests::RootAdapter`,
    /// which app.rs's tests can't reach (private to that module).
    struct RootedAdapter(Option<PathBuf>);
    impl crate::agents::AgentAdapter for RootedAdapter {
        fn id(&self) -> &'static str {
            "rooted"
        }
        fn launch(&self, cwd: &Path) -> crate::agents::CommandSpec {
            crate::agents::CommandSpec::new("true", cwd)
        }
        fn resume(&self, cwd: &Path, session: &str) -> crate::agents::CommandSpec {
            crate::agents::CommandSpec::new("true", cwd).arg(session)
        }
        fn resume_flag(&self) -> &'static str {
            "--resume" // unused: resume() overridden above
        }
        fn session_root(&self, _cwd: &Path) -> Option<PathBuf> {
            self.0.clone()
        }
    }

    /// A single shell-shaped pane running `RootedAdapter(root)`, starting
    /// with `stored` as its persisted session id.
    fn mk_rooted_app(root: Option<PathBuf>, stored: Option<&str>) -> (App<FakePane>, MemStore) {
        let mut ws = shell_ws();
        let spec = ws.tabs[0].panes.get_mut(&1).unwrap();
        spec.adapter = "rooted".into();
        spec.session = stored.map(String::from);
        let mut registry = agents::registry();
        registry.insert("rooted", Box::new(RootedAdapter(root)));
        let store = MemStore::default();
        let (tx, _rx) = mpsc::sync_channel(64);
        let app = App::<FakePane>::new(
            ws,
            registry,
            Box::new(store.clone()),
            tx,
            Size::new(100, 30),
            (0, 0),
            None,
            TokenTable::new().unwrap(),
        )
        .unwrap();
        (app, store)
    }

    #[test]
    fn spawn_with_no_session_root_and_nothing_stored_never_queues_detection() {
        // The common shell-pane shape: an adapter with nowhere to look for a
        // session file must never arm the fs-scan fallback.
        let (app, _) = mk_rooted_app(None, None);
        assert!(app.pending_detect.is_empty());
        assert!(app.runtimes.get(&1).unwrap().cmd.args.is_empty(), "expected a fresh launch");
    }

    #[test]
    fn spawn_with_no_session_root_but_a_stored_id_still_attempts_resume() {
        // No root to check ⇒ SessionState::Unknown, not Gone: resume is
        // attempted and the stored id is left alone, not cleared.
        let (app, _) = mk_rooted_app(None, Some("kept"));
        assert_eq!(app.runtimes.get(&1).unwrap().cmd.args, vec!["kept"]);
        assert_eq!(app.find_spec(1).unwrap().session.as_deref(), Some("kept"));
    }

    #[test]
    fn spawn_with_an_unreadable_session_root_also_attempts_resume_and_keeps_the_id() {
        // A root that exists but can't be read (permission denied) must be
        // treated exactly like a missing one: Unknown, not Gone — same
        // reasoning `stale_session_falls_back_to_fresh_launch` documents for
        // the missing-root case.
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("roost-resolve-unreadable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();

        let (app, _) = mk_rooted_app(Some(dir.clone()), Some("kept"));

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap(); // restore for cleanup
        assert_eq!(app.runtimes.get(&1).unwrap().cmd.args, vec!["kept"]);
        assert_eq!(app.find_spec(1).unwrap().session.as_deref(), Some("kept"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spawn_with_an_existing_session_file_resumes_without_clearing_it() {
        let dir = std::env::temp_dir().join(format!("roost-resolve-exists-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("kept-id.txt"), "").unwrap(); // default session_id_from_path = file stem

        let (app, _) = mk_rooted_app(Some(dir.clone()), Some("kept-id"));

        assert_eq!(app.runtimes.get(&1).unwrap().cmd.args, vec!["kept-id"]);
        assert_eq!(app.find_spec(1).unwrap().session.as_deref(), Some("kept-id"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spawn_with_a_gone_session_clears_it_and_launches_fresh_via_a_hermetic_fixture() {
        // Same Gone contract as `stale_session_falls_back_to_fresh_launch`,
        // through a fixture root instead of a redirected $HOME: a readable,
        // empty root is "definitely gone" — launch fresh AND persist the
        // correction so it isn't retried.
        let dir = std::env::temp_dir().join(format!("roost-resolve-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let (app, store) = mk_rooted_app(Some(dir.clone()), Some("ghost"));

        assert!(app.runtimes.get(&1).unwrap().cmd.args.is_empty(), "expected a fresh launch");
        assert!(app.find_spec(1).unwrap().session.is_none());
        let saved = store.0.lock().unwrap().clone().unwrap();
        assert!(
            saved.tabs[0].panes[&1].session.is_none(),
            "the dead id must be persisted as cleared"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn notification_only_for_unfocused_working_to_waiting() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // focus = 2
        app.on_status(1, AgentStatus::Working, None);
        assert!(app.on_status(1, AgentStatus::Waiting, None).is_some());
        // Focused pane never notifies.
        app.on_status(2, AgentStatus::Working, None);
        assert!(app.on_status(2, AgentStatus::NeedsInput, None).is_none());
        // Idle → waiting (no working phase) doesn't notify.
        assert!(app.on_status(1, AgentStatus::Waiting, None).is_none());
    }

    #[test]
    fn needs_input_notifies_even_without_a_prior_working_phase() {
        // An agent that asks for you immediately on resume (straight to
        // NeedsInput, no Working) must still pull attention when unfocused.
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // focus = 2, pane 1 unfocused & idle
        assert!(app.on_status(1, AgentStatus::NeedsInput, None).is_some());
        // ...but still never for the focused pane.
        assert!(app.on_status(2, AgentStatus::NeedsInput, None).is_none());
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
        app.on_status(id, AgentStatus::Working, None);
        // The nested pi finishes its work: session_shutdown → "exited".
        assert!(app.on_status(id, AgentStatus::Exited, None).is_none());
        assert!(!app.focused_dead(), "socket 'exited' must not enter the dead-pane key path");
        assert_eq!(app.runtimes.get(&id).unwrap().status(), AgentStatus::Waiting);
        // The pane's own agent keeps going — status recovers, nothing sticky.
        app.on_status(id, AgentStatus::Working, None);
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
        // C32: the pane editor's NAME row keeps Rename's filter verbatim —
        // a pasted newline must not reach a title (nor descend into the
        // note: a name paste is a name paste).
        app.apply(Action::EditPane);
        app.handle_paste("api\n\x1b[0mbox\t");
        match &app.mode {
            Mode::PaneEdit { name, row, .. } => {
                assert_eq!(name, "api[0mbox");
                assert_eq!(*row, 0, "a name paste stays on the name row");
            }
            _ => panic!("the editor must still be up, holding the pasted text"),
        }
        assert!(app.runtimes.get(&id).unwrap().input.is_empty(), "the pane must see nothing");
        app.handle_mode_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.find_spec(id).and_then(|s| s.title.clone()), Some("api[0mbox".into()));
    }

    // -- C32: the combined pane editor (name + parking note) --------------

    /// The editor's fields and point, for the tests below. `row` is
    /// unified: 0 = name, i>0 = note line i-1.
    fn edit_state(app: &App<FakePane>) -> (String, Vec<String>, usize, usize) {
        match &app.mode {
            Mode::PaneEdit { name, lines, row, col, .. } => {
                (name.clone(), lines.clone(), *row, *col)
            }
            _ => panic!("the pane editor must be up"),
        }
    }

    fn shift_enter() -> crossterm::event::KeyEvent {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)
    }

    /// C32, the whole write path as one typing flow: name, Shift+Enter
    /// (descends into the note), headline, Shift+Enter (splits), body,
    /// Enter — both fields land on the spec, the note stamped, and the
    /// workspace saved (the overnight story is a disk write, not a mode).
    #[test]
    fn edit_pane_types_name_and_note_in_one_flow() {
        use crossterm::event::KeyCode;
        let (mut app, store) = mk_app(shell_ws());
        let id = app.focused;
        app.apply(Action::EditPane);
        type_query(&mut app, "auth-fix");
        app.handle_mode_key(shift_enter());
        type_query(&mut app, "tests green");
        app.handle_mode_key(shift_enter());
        type_query(&mut app, "next: merge");
        let (name, lines, ..) = edit_state(&app);
        assert_eq!(name, "auth-fix");
        assert_eq!(lines, vec!["tests green".to_string(), "next: merge".to_string()]);
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.mode, Mode::Normal));
        let spec = app.find_spec(id).unwrap();
        assert_eq!(spec.title.as_deref(), Some("auth-fix"));
        assert_eq!(spec.note.as_deref(), Some("tests green\nnext: merge"));
        assert!(spec.noted_at.is_some(), "a saved note carries its write time");
        let saved = store.load().unwrap().unwrap();
        assert_eq!(saved.tabs[0].panes[&id].note.as_deref(), Some("tests green\nnext: merge"));
        assert_eq!(saved.tabs[0].panes[&id].title.as_deref(), Some("auth-fix"));
    }

    /// C32: the editor opens prefilled — both fields intact, point at the
    /// end of the NAME row (renames are Alt+r's muscle memory) — and an
    /// emptied note clears note + timestamp while the name survives: the
    /// clear gesture acknowledges status without touching identity.
    #[test]
    fn edit_pane_opens_prefilled_and_an_emptied_note_clears_only_the_note() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        let spec = app.ws.tabs[0].panes.get_mut(&id).unwrap();
        spec.title = Some("web".into());
        spec.note = Some("old line\nsecond".into());
        spec.noted_at = Some(5);
        app.apply(Action::EditPane);
        let (name, lines, row, col) = edit_state(&app);
        assert_eq!(name, "web");
        assert_eq!(lines, vec!["old line".to_string(), "second".to_string()]);
        assert_eq!((row, col), (0, 3), "the point opens at the end of the name");
        // Walk down and empty both note lines (End first — Ctrl+U kills
        // behind the point); the final join leaves one blank line.
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::End);
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::End);
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Enter);
        let spec = app.find_spec(id).unwrap();
        assert_eq!(spec.title.as_deref(), Some("web"), "identity survives the clear");
        assert!(spec.note.is_none(), "an empty note save clears the note");
        assert!(spec.noted_at.is_none(), "…and its timestamp with it");
    }

    /// C32: the age stamp answers "when did the STATUS last change" — a
    /// pure rename re-stamps nothing, an unchanged note keeps its honest
    /// age, and only a real note edit refreshes it.
    #[test]
    fn edit_pane_restamps_the_note_age_only_when_the_note_changed() {
        use crossterm::event::KeyCode;
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        let spec = app.ws.tabs[0].panes.get_mut(&id).unwrap();
        spec.note = Some("waiting on CI".into());
        spec.noted_at = Some(5);
        // Open + save with no edits: the age must not refresh.
        app.apply(Action::EditPane);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.find_spec(id).unwrap().noted_at, Some(5));
        // Rename only: still no refresh.
        app.apply(Action::EditPane);
        type_query(&mut app, "x");
        press(&mut app, KeyCode::Enter);
        let spec = app.find_spec(id).unwrap();
        assert_eq!(spec.title.as_deref(), Some("x"));
        assert_eq!(spec.noted_at, Some(5), "a rename must not make a stale note look fresh");
        // A real note edit: refreshed.
        app.apply(Action::EditPane);
        press(&mut app, KeyCode::Down);
        type_query(&mut app, "!");
        press(&mut app, KeyCode::Enter);
        assert_ne!(app.find_spec(id).unwrap().noted_at, Some(5));
    }

    /// C32: Esc and the entry chord's toggle-off (U18, now Alt+r itself)
    /// both cancel — typed edits die with the dialog, both stored fields
    /// stay exactly as they were.
    #[test]
    fn edit_pane_esc_and_toggle_off_both_cancel_without_touching_either_field() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        let spec = app.ws.tabs[0].panes.get_mut(&id).unwrap();
        spec.title = Some("web".into());
        spec.note = Some("keep me".into());
        app.apply(Action::EditPane);
        type_query(&mut app, " junk");
        press(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
        let spec = app.find_spec(id).unwrap();
        assert_eq!(spec.title.as_deref(), Some("web"));
        assert_eq!(spec.note.as_deref(), Some("keep me"));
        app.apply(Action::EditPane);
        type_query(&mut app, " junk");
        let consumed = app.handle_mode_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::ALT));
        assert!(consumed, "the toggle-off is handled, not forwarded");
        assert!(matches!(app.mode, Mode::Normal));
        let spec = app.find_spec(id).unwrap();
        assert_eq!(spec.title.as_deref(), Some("web"));
        assert_eq!(spec.note.as_deref(), Some("keep me"));
    }

    /// C32: Alt+Enter inside the dialog is a break, never a cancel into
    /// the quick-launch picker — on Terminal.app it is the only bytes
    /// Shift+Enter can arrive as. On the name row it descends; on a note
    /// row it splits.
    #[test]
    fn edit_pane_alt_enter_breaks_instead_of_opening_the_picker() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::EditPane);
        type_query(&mut app, "ab");
        let consumed = app.handle_mode_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert!(consumed);
        let (name, lines, row, col) = edit_state(&app);
        assert_eq!((name.as_str(), row, col), ("ab", 1, 0), "descended into the note");
        // The descend opens an empty note rather than carrying the name into
        // it — the half this test used to bind and then shadow unread.
        assert_eq!(lines, vec![String::new()], "the note starts empty");
        type_query(&mut app, "cd");
        let consumed = app.handle_mode_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert!(consumed);
        let (_, lines, row, col) = edit_state(&app);
        assert_eq!(lines, vec!["cd".to_string(), String::new()]);
        assert_eq!((row, col), (2, 0), "split the note line");
    }

    /// C32: the note stops growing at NOTE_MAX_LINES — the split is
    /// swallowed whole, nothing scrolls off and nothing is half-split.
    #[test]
    fn edit_pane_line_cap_swallows_the_split() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::EditPane);
        for _ in 0..NOTE_MAX_LINES + 3 {
            app.handle_mode_key(shift_enter());
        }
        assert_eq!(edit_state(&app).1.len(), NOTE_MAX_LINES);
    }

    /// C32 + U8(b): a note-row paste keeps its newlines as line breaks
    /// (CR/CRLF normalized), other control bytes drop, and overflow past
    /// the cap folds into the last line instead of vanishing.
    #[test]
    fn edit_pane_note_paste_weaves_lines_and_folds_overflow_at_the_cap() {
        use crossterm::event::KeyCode;
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::EditPane);
        press(&mut app, KeyCode::Down); // onto the note
        app.handle_paste("a\r\nb\rc\x1b[0m");
        let (_, lines, row, col) = edit_state(&app);
        assert_eq!(lines, vec!["a".to_string(), "b".to_string(), "c[0m".to_string()]);
        assert_eq!((row, col), (3, 4), "the point lands after the insertion");

        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::EditPane);
        press(&mut app, KeyCode::Down);
        app.handle_paste("1\n2\n3\n4\n5\n6\n7\n8\n9\n10");
        let (_, lines, ..) = edit_state(&app);
        assert_eq!(lines.len(), NOTE_MAX_LINES);
        assert_eq!(lines[NOTE_MAX_LINES - 1], "8 9 10", "overflow folds, space-joined");
    }

    /// C32: motion crosses the name/note seam, edits never do — Backspace
    /// at the note's top-left and Delete at the name's end are walls, so
    /// a typo can't merge status text into identity (in either direction).
    #[test]
    fn edit_pane_edits_never_cross_the_name_note_wall() {
        use crossterm::event::KeyCode;
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        let spec = app.ws.tabs[0].panes.get_mut(&id).unwrap();
        spec.title = Some("web".into());
        spec.note = Some("note".into());
        app.apply(Action::EditPane);
        // Delete at the name's end: wall.
        press(&mut app, KeyCode::Delete);
        // Backspace at the note's top-left: wall.
        press(&mut app, KeyCode::Right); // seam-cross to (1, 0) — motion is allowed
        let (.., row, col) = edit_state(&app);
        assert_eq!((row, col), (1, 0));
        press(&mut app, KeyCode::Backspace);
        let (name, lines, ..) = edit_state(&app);
        assert_eq!(name, "web");
        assert_eq!(lines, vec!["note".to_string()]);
        // …while inside the note, Backspace at a line edge still joins.
        app.handle_mode_key(shift_enter());
        press(&mut app, KeyCode::Backspace);
        let (_, lines, ..) = edit_state(&app);
        assert_eq!(lines, vec!["note".to_string()], "note-internal join still works");
    }

    /// C32: the note rides `PaneSpec`, so `Alt+u` restores it with the
    /// pane — same clause as the session id, no separate machinery.
    #[test]
    fn undo_reopens_a_closed_pane_with_its_note() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        let id = app.focused;
        let spec = app.ws.tabs[0].panes.get_mut(&id).unwrap();
        spec.note = Some("parked: retry flaky e2e".into());
        spec.noted_at = Some(7);
        app.apply(Action::ClosePane);
        app.apply(Action::Undo);
        let restored = app.focused;
        let spec = app.find_spec(restored).unwrap();
        assert_eq!(spec.note.as_deref(), Some("parked: retry flaky e2e"));
        assert_eq!(spec.noted_at, Some(7));
    }

    /// U16: `unix-word-rubout`'s exact boundary rule — trailing whitespace
    /// goes first, then one whole word; a buffer of only whitespace, or an
    /// empty one, empties without panicking.
    #[test]
    fn erase_word_drops_trailing_space_then_one_word() {
        use super::erase_word;
        assert_eq!(erase_word("abc def"), "abc ");
        assert_eq!(erase_word("abc def  "), "abc ");
        assert_eq!(erase_word("abc"), "");
        assert_eq!(erase_word("   "), "");
        assert_eq!(erase_word(""), "");
        // Repeated application walks back word by word, never wrapping past
        // the start.
        assert_eq!(erase_word(&erase_word("one two three")), "one ");
        assert_eq!(erase_word(&erase_word(&erase_word("one two three"))), "");
    }

    /// U16: typing `abc` then Ctrl+W then Ctrl+U must commit the empty
    /// string, not `abcwu` — Ctrl+W erases the word it just typed, Ctrl+U
    /// clears the line, and no `w`/`u` is ever inserted.
    #[test]
    fn rename_honors_ctrl_w_and_ctrl_u_instead_of_typing_their_letters() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::RenameTab);
        // Wipe the prefilled tab name first (same reason as rename_typing).
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        for c in "abc def".chars() {
            app.handle_mode_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        match &app.mode {
            Mode::Rename { buffer, .. } => assert_eq!(buffer, "abc ", "Ctrl+W erases one word"),
            _ => panic!("still renaming"),
        }
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        match &app.mode {
            Mode::Rename { buffer, .. } => assert_eq!(buffer, "", "Ctrl+U clears the line"),
            _ => panic!("still renaming"),
        }
    }

    // -- U16: rename cursor motion (the deferred half) ----------------------

    /// The rename buffer and its cursor, for the motion tests below.
    fn rename_state(app: &App<FakePane>) -> (String, usize) {
        match &app.mode {
            Mode::Rename { buffer, cursor, .. } => (buffer.clone(), *cursor),
            _ => panic!("still renaming"),
        }
    }

    fn rename_typing(text: &str) -> App<FakePane> {
        let (mut app, _) = mk_app(shell_ws());
        // U16's single-line vocabulary lives on in the tab dialog (C32
        // moved pane renames into the combined editor, which has its own
        // tests below). The dialog prefills the current tab name ("main");
        // Ctrl+U from the end wipes it so each test operates on exactly
        // the text it types.
        app.apply(Action::RenameTab);
        app.handle_mode_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('u'),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        for c in text.chars() {
            app.handle_mode_key(key(c));
        }
        app
    }

    /// U16: Left/Right/Home/End move a real insertion point, and typing
    /// happens *at* it — the dialog was append-only, so a typo in a long
    /// name meant deleting back to it.
    #[test]
    fn rename_cursor_moves_and_insertion_happens_at_the_point() {
        use crossterm::event::KeyCode;
        let mut app = rename_typing("abcd");
        assert_eq!(rename_state(&app), ("abcd".into(), 4), "the point starts at the end");
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Left);
        assert_eq!(rename_state(&app), ("abcd".into(), 2));
        app.handle_mode_key(key('X'));
        assert_eq!(rename_state(&app), ("abXcd".into(), 3), "inserted at the point");
        press(&mut app, KeyCode::Home);
        app.handle_mode_key(key('_'));
        assert_eq!(rename_state(&app), ("_abXcd".into(), 1));
        press(&mut app, KeyCode::End);
        app.handle_mode_key(key('!'));
        assert_eq!(rename_state(&app), ("_abXcd!".into(), 7));
        // Motion clamps at both ends rather than wrapping.
        for _ in 0..20 {
            press(&mut app, KeyCode::Left);
        }
        assert_eq!(rename_state(&app).1, 0);
        for _ in 0..20 {
            press(&mut app, KeyCode::Right);
        }
        assert_eq!(rename_state(&app).1, 7);
    }

    /// U16: Backspace eats the char behind the point and Delete the one
    /// under it — the split every text field makes, and one that could not
    /// exist before there was a point.
    #[test]
    fn rename_backspace_and_delete_are_relative_to_the_point() {
        use crossterm::event::KeyCode;
        let mut app = rename_typing("abcd");
        press(&mut app, KeyCode::Left); // between c and d
        press(&mut app, KeyCode::Backspace);
        assert_eq!(rename_state(&app), ("abd".into(), 2), "Backspace takes the char behind");
        press(&mut app, KeyCode::Delete);
        assert_eq!(rename_state(&app), ("ab".into(), 2), "Delete takes the char under");
        // Both are no-ops at their respective ends.
        press(&mut app, KeyCode::Delete);
        assert_eq!(rename_state(&app), ("ab".into(), 2));
        press(&mut app, KeyCode::Home);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(rename_state(&app), ("ab".into(), 0));
    }

    /// U16: with a point, "word behind point" finally means something —
    /// Ctrl+W rubs out the word before the cursor and leaves the tail
    /// alone, and Ctrl+U kills back to the start rather than the whole
    /// buffer. Both are readline's actual definitions.
    #[test]
    fn rename_ctrl_w_and_ctrl_u_respect_the_cursor() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = rename_typing("alpha beta gamma");
        // Park the point right after `beta`.
        for _ in 0.." gamma".len() {
            press(&mut app, KeyCode::Left);
        }
        assert_eq!(rename_state(&app), ("alpha beta gamma".into(), 10));
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(
            rename_state(&app),
            ("alpha  gamma".into(), 6),
            "the word behind the point goes; the tail stays"
        );
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(
            rename_state(&app),
            (" gamma".into(), 0),
            "Ctrl+U kills back to the start, keeping what follows the point"
        );
    }

    /// U16: a paste lands at the point too — it would be the one edit in
    /// the dialog that ignored the cursor otherwise. Multi-byte text keeps
    /// the char/byte bookkeeping honest.
    #[test]
    fn rename_paste_inserts_at_the_point_including_multibyte() {
        use crossterm::event::KeyCode;
        let mut app = rename_typing("héllo");
        press(&mut app, KeyCode::Left);
        app.handle_paste("~ê~");
        assert_eq!(rename_state(&app), ("héll~ê~o".into(), 7));
        // And typing after a multi-byte insert still slices on a char
        // boundary rather than panicking.
        app.handle_mode_key(key('z'));
        assert_eq!(rename_state(&app), ("héll~ê~zo".into(), 8));
    }

    /// U16: everything else modified is swallowed, and Shift — how an
    /// uppercase letter arrives under kitty's CSI-u encoding — still types.
    #[test]
    fn rename_swallows_other_modified_chars_but_still_types_shifted_ones() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.apply(Action::EditPane);
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
        for c in ['k', 'x', 'd'] {
            app.handle_mode_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
        }
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        app.handle_mode_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            app.find_spec(id).and_then(|s| s.title.clone()),
            Some("Ab".into()),
            "unimplemented Ctrl chords may not leave their letters in the name"
        );
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

    // -- P10: focus reporting (DECSET 1004) ---------------------------------

    /// Two panes, both spawned, with `input` cleared so only what the focus
    /// machinery writes is left. `subscribed` says which of them ran
    /// `?1004h`. Returns the app with pane 2 focused (the split's new pane).
    fn focus_fixture(sub1: bool, sub2: bool) -> App<FakePane> {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // pane 2, now focused
        for (id, sub) in [(1, sub1), (2, sub2)] {
            let rt = app.runtimes.get_mut(&id).expect("pane spawned");
            rt.focus_events = sub;
            rt.input.clear();
        }
        app
    }

    fn pane_input(app: &App<FakePane>, id: PaneId) -> Vec<u8> {
        app.runtimes[&id].input.clone()
    }

    /// P10: the whole point — moving roost's focus tells the pane that lost
    /// it `CSI O` and the pane that gained it `CSI I`. Before this, a pane
    /// running `?1004h` saw nothing across a focus round-trip, so vim's
    /// autoread and every dim-on-blur TUI stayed asleep inside roost.
    #[test]
    fn a_focus_move_sends_o_to_the_pane_leaving_and_i_to_the_pane_arriving() {
        let mut app = focus_fixture(true, true);
        app.apply(Action::Focus(layout::Dir::Left)); // 2 → 1
        assert_eq!(app.focused, 1);
        assert_eq!(pane_input(&app, 2), b"\x1b[O".to_vec(), "the pane losing focus is told");
        assert_eq!(pane_input(&app, 1), b"\x1b[I".to_vec(), "the pane gaining focus is told");
        // ...and back the other way, symmetrically.
        app.apply(Action::Focus(layout::Dir::Right)); // 1 → 2
        assert_eq!(pane_input(&app, 1), b"\x1b[I\x1b[O".to_vec());
        assert_eq!(pane_input(&app, 2), b"\x1b[O\x1b[I".to_vec());
    }

    /// P10: a pane that never sent `?1004h` gets *nothing*. To it these
    /// bytes are keystrokes — `\x1b[I` typed at a shell prompt is a live
    /// command edit, which is why "just send it to everyone" is not an
    /// option.
    #[test]
    fn a_pane_that_never_subscribed_is_sent_no_focus_reports() {
        let mut app = focus_fixture(false, false);
        app.apply(Action::Focus(layout::Dir::Left));
        assert!(pane_input(&app, 1).is_empty());
        assert!(pane_input(&app, 2).is_empty());
        // Mixed fleet: only the subscriber hears about the same move.
        let mut app = focus_fixture(true, false);
        app.apply(Action::Focus(layout::Dir::Left));
        assert_eq!(pane_input(&app, 1), b"\x1b[I".to_vec());
        assert!(pane_input(&app, 2).is_empty(), "an unsubscribed pane hears nothing");
    }

    /// P10: focus that doesn't actually move reports nothing. `CSI O` then
    /// `CSI I` for "you still have focus" is a lie an editor acts on
    /// (nvim re-stats every buffer on FocusGained).
    #[test]
    fn a_focus_move_that_goes_nowhere_reports_nothing() {
        let mut app = focus_fixture(true, true);
        // Pane 2 is the right half of the split: there is nothing to its
        // right, so the move is refused and focus stays put.
        app.apply(Action::Focus(layout::Dir::Right));
        assert_eq!(app.focused, 2);
        assert!(pane_input(&app, 1).is_empty());
        assert!(pane_input(&app, 2).is_empty());
    }

    /// P10: the host's own focus event belongs to whichever pane is focused
    /// inside roost — and only to it. Repeats are swallowed: window managers
    /// re-assert focus freely and a duplicate `CSI I` is a spurious wakeup.
    #[test]
    fn host_focus_events_go_to_the_focused_pane_and_never_repeat() {
        let mut app = focus_fixture(true, true);
        app.on_host_focus(false);
        assert_eq!(pane_input(&app, 2), b"\x1b[O".to_vec());
        assert!(pane_input(&app, 1).is_empty(), "an unfocused pane isn't the window");
        app.on_host_focus(false); // duplicate
        assert_eq!(pane_input(&app, 2), b"\x1b[O".to_vec());
        app.on_host_focus(true);
        assert_eq!(pane_input(&app, 2), b"\x1b[O\x1b[I".to_vec());
    }

    /// P10: while roost's window is blurred nothing inside it is focused, so
    /// pane-level moves stay silent — and the pane that ends up focused
    /// collects its `CSI I` when the window comes back, not before.
    #[test]
    fn pane_focus_moves_stay_silent_while_the_window_is_blurred() {
        let mut app = focus_fixture(true, true);
        app.on_host_focus(false);
        assert_eq!(pane_input(&app, 2), b"\x1b[O".to_vec());
        app.apply(Action::Focus(layout::Dir::Left)); // 2 → 1, window blurred
        assert_eq!(app.focused, 1, "focus still moves; only the reporting waits");
        assert!(pane_input(&app, 1).is_empty());
        assert_eq!(pane_input(&app, 2), b"\x1b[O".to_vec(), "no second O for the old pane");
        app.on_host_focus(true);
        assert_eq!(pane_input(&app, 1), b"\x1b[I".to_vec(), "the now-focused pane is told");
    }

    /// P10: the reports are roost speaking, not the user typing — they must
    /// not snap a scrolled-back pane to its live tail (`write_input` does,
    /// `write_input_raw` doesn't; U9's frozen view depends on the
    /// difference).
    #[test]
    fn a_focus_report_does_not_yank_a_scrolled_pane_back_to_the_tail() {
        let mut app = focus_fixture(true, true);
        app.wheel_scroll(1, 7);
        assert_eq!(app.runtimes[&1].scroll_offset(), 7);
        app.apply(Action::Focus(layout::Dir::Left)); // pane 1 gains focus
        assert_eq!(pane_input(&app, 1), b"\x1b[I".to_vec());
        assert_eq!(app.runtimes[&1].scroll_offset(), 7, "the frozen view survives");
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

    // ---- C29: native (Normal-mode) selection -------------------------------

    /// Crossterm hands roost no click count, so `click_count` derives
    /// 1st/2nd/3rd from timing + position and wraps a 4th click back to 1st
    /// (nothing above triple-click is bound). A different pane, or one that
    /// arrives cold, always restarts the count.
    #[test]
    fn click_count_cycles_through_1_2_3_and_wraps_on_a_4th() {
        let (mut app, _) = mk_app(shell_ws());
        assert_eq!(app.click_count(1, 5, 5), 1);
        assert_eq!(app.click_count(1, 5, 5), 2);
        assert_eq!(app.click_count(1, 5, 5), 3);
        assert_eq!(app.click_count(1, 5, 5), 1, "a 4th click restarts the cycle");
        // A one-cell drift is still "the same spot" (CLICK_TOLERANCE).
        assert_eq!(app.click_count(1, 5, 5), 2);
        assert_eq!(app.click_count(1, 6, 5), 3);
        // A different pane never continues another pane's run.
        assert_eq!(app.click_count(2, 6, 5), 1);
    }

    /// Double-click selects the whitespace-delimited word under the
    /// pointer — the same tokenizer `find_url_at` uses (`word_bounds_at`).
    #[test]
    fn select_word_at_grabs_the_whitespace_delimited_word() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().rows = vec!["hello world".into()];

        app.select_word_at(id, 0, 2); // inside "hello"
        let sel = app.selection.expect("double-click marks a selection");
        assert_eq!((sel.anchor, sel.cursor), ((0, 0), (0, 4)));
        assert!(!sel.dragging);

        app.select_word_at(id, 0, 8); // inside "world"
        assert_eq!(
            (app.selection.unwrap().anchor, app.selection.unwrap().cursor),
            ((0, 6), (0, 10))
        );

        // D3: the gap between the words has nothing to grab, and must clear
        // `self.selection` outright rather than degrade to a same-point
        // selection — a same-point selection is indistinguishable from a
        // genuine one-character *word* (also anchor == cursor, by
        // construction), which would silently drop double-clicks on `a`,
        // `$`, and the like on release.
        app.select_word_at(id, 0, 5);
        assert!(app.selection.is_none());
    }

    /// D3: a one-character word is a real selection, not "nothing found" —
    /// both share the anchor == cursor shape, so the two must be told apart
    /// some other way (see `select_word_at`'s doc).
    #[test]
    fn select_word_at_a_one_char_word_is_a_real_selection_not_nothing_found() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().rows = vec!["a bc".into()];
        app.select_word_at(id, 0, 0);
        let sel = app.selection.expect("a 1-char word still marks a selection");
        assert_eq!((sel.anchor, sel.cursor), ((0, 0), (0, 0)));
    }

    /// B1: `select_word_at` must index by grid CELL, not by char — a row
    /// holding a wide (2-cell) glyph before the target column would
    /// otherwise walk off by however many extra cells the glyph ate,
    /// selecting (and, on release, copying) the wrong word. `word_bounds_at`
    /// converts cell → char before finding the word and `select_word_at`
    /// converts the result back — both ends, pinned together.
    #[test]
    fn select_word_at_accounts_for_a_wide_glyph_earlier_in_the_row() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        // "日" is 2 cells wide: cells 0-1, then a space at cell 2, then
        // "hello" at cells 3..=7.
        app.runtimes.get_mut(&id).unwrap().rows = vec!["日 hello world".into()];
        app.select_word_at(id, 0, 3); // cell 3 == 'h'
        let sel = app.selection.expect("marks the word after the wide glyph");
        assert_eq!((sel.anchor, sel.cursor), ((0, 3), (0, 7)), "cells 3..=7 is \"hello\"");
    }

    /// Triple-click selects the whole row — the same shape Copy mode's `V`
    /// produces (`copy_mode_capital_v_selects_the_whole_row_and_extends_by_lines`).
    #[test]
    fn select_line_at_grabs_the_whole_row() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        let (_, w) = app.pane_inner_dims(id);
        app.select_line_at(id, 3);
        let sel = app.selection.expect("triple-click marks a selection");
        assert_eq!((sel.anchor, sel.cursor), ((3, 0), (3, w - 1)));
        assert!(!sel.dragging);
    }

    /// Shift-click extends the live selection's cursor to the pointer,
    /// keeping the anchor, and arms `dragging` so a held drag after the
    /// shift-click keeps extending. With nothing to extend — no selection,
    /// or one that belongs to a different pane — it starts a fresh one at
    /// the click point instead.
    #[test]
    fn extend_selection_to_extends_the_live_selection_or_starts_fresh() {
        let (mut app, _) = mk_app(shell_ws());
        app.begin_selection(1, 2, 2);
        app.extend_selection_to(1, 2, 9);
        let sel = app.selection.unwrap();
        assert_eq!((sel.anchor, sel.cursor), ((2, 2), (2, 9)), "anchor stays, cursor moves");
        assert!(sel.dragging, "a held drag after shift-click should keep extending");

        // A different pane: nothing of its own to extend, so a fresh start.
        app.extend_selection_to(2, 4, 4);
        let sel = app.selection.unwrap();
        assert_eq!((sel.pane, sel.anchor, sel.cursor), (2, (4, 4), (4, 4)));

        // No selection at all: same fresh start.
        app.selection = None;
        app.extend_selection_to(1, 1, 1);
        assert_eq!(app.selection.unwrap().anchor, (1, 1));
    }

    /// The defining difference from `finish_selection`: a native release
    /// copies but does NOT drop the highlight (Copy mode's version always
    /// exits the mode, taking the selection with it). It stays lit until
    /// the next click or keypress (`on_click` / `handle_mode_key`, below).
    #[test]
    fn finish_native_selection_extracts_text_but_leaves_the_highlight_lit() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().grab = "selected text".into();
        app.begin_selection(id, 0, 0);
        app.extend_selection(0, 5);
        assert_eq!(app.finish_native_selection().as_deref(), Some("selected text"));
        assert!(app.selection.is_some(), "the highlight survives the release");
        assert!(!app.selection.unwrap().dragging);
        assert!(matches!(app.mode, Mode::Normal), "native selection never touches mode");

        // Nothing to copy: reports None. Clearing the now-pointless
        // highlight is the caller's job (main.rs), not this function's.
        app.runtimes.get_mut(&id).unwrap().grab = String::new();
        assert_eq!(app.finish_native_selection(), None);
    }

    /// S4: a double-click's own release is *staged*,
    /// not committed immediately — it's the one release a still-possible
    /// 3rd click can supersede within the same double-click window.
    /// `due_copy` reports the staged text once the deadline passes —
    /// backdated here rather than slept for, so the test costs nothing.
    #[test]
    fn release_native_selection_stages_a_double_click_and_due_copy_fires_it_later() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().rows = vec!["hello world".into()];
        app.click_count(id, 0, 8); // 1st
        assert_eq!(app.click_count(id, 0, 8), 2, "2nd click at the same spot: a double-click");
        app.select_word_at(id, 0, 8); // "world"

        assert_eq!(app.release_native_selection(), None, "staged, not committed yet");
        assert!(app.selection.is_some(), "the highlight stays lit while staged");
        assert_eq!(app.due_copy(), None, "the window hasn't passed yet");

        app.pending_copy =
            app.pending_copy.map(|(text, _)| (text, Instant::now() - Duration::from_millis(1)));
        assert_eq!(app.due_copy().as_deref(), Some("world"));
        assert_eq!(app.due_copy(), None, "consumed exactly once");
    }

    /// A 3rd click (triple-click) supersedes a staged double-click copy
    /// entirely — the word is never copied, only the line the 3rd click
    /// selects instead. Supersession is detected by the 3rd click's own
    /// release, which cancels the stage directly (every release clears
    /// `pending_copy` unconditionally — see `release_native_selection`).
    #[test]
    fn a_third_click_cancels_the_staged_double_click_copy() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().rows = vec!["hello world".into()];
        app.click_count(id, 0, 8);
        assert_eq!(app.click_count(id, 0, 8), 2);
        app.select_word_at(id, 0, 8);
        assert_eq!(
            app.release_native_selection(),
            None,
            "the word's release stages, doesn't commit"
        );
        assert!(app.pending_copy.is_some());

        assert_eq!(app.click_count(id, 0, 8), 3, "the 3rd click");
        app.select_line_at(id, 0); // overwrites self.selection with the line
        assert_eq!(
            app.release_native_selection().as_deref(),
            Some("hello world"),
            "the 3rd click's own release commits the line immediately"
        );
        assert!(
            app.pending_copy.is_none(),
            "the word's stage was cancelled outright, not just outrun"
        );
        assert_eq!(app.due_copy(), None, "nothing left to fire, staged or otherwise");
    }

    /// F5: a keypress clearing the highlight (C29's "any click or keypress
    /// clears" rule) must NOT cancel a staged double-click copy — a
    /// keypress has nothing to do with whether the word actually
    /// double-clicked should still be copied, so it never touches the
    /// stage at all.
    #[test]
    fn an_unrelated_keypress_does_not_cancel_the_staged_double_click_copy() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().rows = vec!["hello world".into()];
        app.click_count(id, 0, 8);
        app.click_count(id, 0, 8);
        app.select_word_at(id, 0, 8);
        app.release_native_selection();
        assert!(app.pending_copy.is_some());

        // Not a click: e.g. the pane forwarding a typed key, which clears
        // the native-selection highlight per C29.
        app.selection = None;

        app.pending_copy =
            app.pending_copy.map(|(text, _)| (text, Instant::now() - Duration::from_millis(1)));
        assert_eq!(app.due_copy().as_deref(), Some("world"), "the copy still lands");
    }

    /// The staged copy is the string grabbed at release time
    /// (`PendingCopy`), not a promise to re-read the grid later — `due_copy`
    /// must not re-extract from the *live* grid at fire time, or a
    /// streaming pane that scrolls or overwrites the row before the 500ms
    /// window closes would silently copy whatever now occupies those cells
    /// instead of the word actually double-clicked.
    #[test]
    fn the_staged_copy_is_fixed_at_release_time_not_re_read_from_a_later_grid() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().rows = vec!["hello world".into()];
        app.click_count(id, 0, 8);
        app.click_count(id, 0, 8);
        app.select_word_at(id, 0, 8);
        app.release_native_selection();

        // The pane's content changes before the deadline — a streaming
        // agent printing new output, or the view scrolling.
        app.runtimes.get_mut(&id).unwrap().rows = vec!["totally different text".into()];

        app.pending_copy =
            app.pending_copy.map(|(text, _)| (text, Instant::now() - Duration::from_millis(1)));
        assert_eq!(
            app.due_copy().as_deref(),
            Some("world"),
            "must copy what was actually double-clicked"
        );
    }

    /// `on_click` drops a lingering native selection when the click lands on
    /// a *different* pane — the highlight must not survive once focus moves
    /// elsewhere. A click back on the SAME pane leaves it for the caller's
    /// fresh gesture (begin_selection/select_word_at/…) to supersede.
    #[test]
    fn on_click_clears_a_selection_that_belongs_to_a_different_pane_only() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1 | 2, focus 2
        app.begin_selection(1, 0, 0);
        app.extend_selection(0, 3);
        assert!(app.selection.is_some());

        app.on_click(2); // clicking the OTHER pane
        assert!(app.selection.is_none(), "the other pane's click dismissed it");

        app.begin_selection(2, 1, 1);
        app.on_click(2); // clicking the pane it belongs to
        assert!(app.selection.is_some(), "on_click doesn't clear its own pane's selection");
    }

    /// Any keypress dismisses a lingering native selection while in Normal
    /// mode — but the guard must never fire once Copy mode owns
    /// `self.selection` for its own cursor, or every hjkl press there would
    /// erase the very selection it just extended.
    #[test]
    fn any_keypress_clears_a_normal_mode_selection_but_copy_mode_is_exempt() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        app.begin_selection(1, 0, 0);
        app.extend_selection(0, 3);
        assert!(app.selection.is_some());
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(app.selection.is_none(), "a plain keypress in Normal mode clears it");

        // Copy mode manages `self.selection` itself — its own motions must
        // survive its own keypresses.
        app.apply(Action::CopyMode);
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        assert!(app.selection.is_some(), "v marked a Copy-mode selection");
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        assert!(app.selection.is_some(), "hjkl inside copy mode must not self-clear");
    }

    /// Entering Copy mode (Alt+c) clears any lingering native selection
    /// first, in the same order main.rs's event loop calls things:
    /// `handle_mode_key` (which does the clearing, then falls through
    /// unhandled) followed by the global binding it fell through to.
    #[test]
    fn entering_copy_mode_clears_a_lingering_native_selection_first() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.begin_selection(id, 0, 0);
        app.extend_selection(0, 3);
        assert!(app.selection.is_some());
        assert!(!app.handle_mode_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::ALT)));
        assert!(app.selection.is_none(), "cleared before Copy mode is even entered");
        app.apply(Action::CopyMode);
        assert!(matches!(app.mode, Mode::Copy { .. }));
        assert!(app.selection.is_none(), "Copy mode starts with no inherited selection");
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

    /// U19: a URL that ran off the end of one row and continued on the next
    /// is ONE token again — clicking either row returns the whole link. The
    /// old single-row read opened the first row's fragment from above the
    /// break and dead-clicked everything below it.
    #[test]
    fn url_at_joins_a_link_that_wrapped_across_two_rows() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        let (_, width) = app.pane_inner_dims(id);
        let w = width as usize;
        // Row 3 is filled to its last column and flagged wrapped; row 4
        // carries the rest.
        let head = "https://ci.example.com/runs/";
        let row3 = format!("{head}{}", "1".repeat(w - head.chars().count()));
        let row4 = "234/logs and then some prose".to_string();
        let whole = format!("{row3}234/logs");
        {
            let rt = app.runtimes.get_mut(&id).unwrap();
            rt.rows = vec![String::new(), String::new(), String::new(), row3, row4];
            rt.wrapped = vec![false, false, false, true, false];
        }
        // From the first row...
        assert_eq!(app.url_at(id, 3, 5).as_deref(), Some(whole.as_str()));
        // ...and from the continuation row, which must not be a dead click.
        assert_eq!(app.url_at(id, 4, 2).as_deref(), Some(whole.as_str()));
        // A token past the URL on the continuation row is still not a URL.
        assert_eq!(app.url_at(id, 4, 12), None); // "and"
    }

    /// U19: an unwrapped row is still read alone — the join must not fuse
    /// two lines that merely sit next to each other, which would invent
    /// links out of adjacent prose.
    #[test]
    fn url_at_never_joins_rows_the_grid_did_not_wrap() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        {
            let rt = app.runtimes.get_mut(&id).unwrap();
            rt.rows = vec!["https://a.co/x".into(), "trailing".into()];
            rt.wrapped = vec![false, false];
        }
        assert_eq!(app.url_at(id, 0, 3).as_deref(), Some("https://a.co/x"));
        assert_eq!(app.url_at(id, 1, 0), None);
    }

    /// U19: the join's column arithmetic and its padding rule, in isolation
    /// — a row whose tail really was blank keeps its tokens apart instead of
    /// fusing them into one nonsense word.
    #[test]
    fn url_in_wrapped_rows_maps_columns_and_restores_trimmed_tails() {
        use super::url_in_wrapped_rows;
        // A real wrap: the first row is full to the pane's last column
        // (17 of 17), so nothing was trimmed and nothing is padded back.
        let run = vec!["https://a.co/very".to_string(), "long/path tail".to_string()];
        let joined = "https://a.co/verylong/path";
        assert_eq!(url_in_wrapped_rows(&run, 17, 0, 0).as_deref(), Some(joined));
        assert_eq!(url_in_wrapped_rows(&run, 17, 1, 0).as_deref(), Some(joined));
        // Column arithmetic: the second row's column 4 is index 21 of the
        // join — still inside the link, and past it lands on "tail".
        assert_eq!(url_in_wrapped_rows(&run, 17, 1, 4).as_deref(), Some(joined));
        assert_eq!(url_in_wrapped_rows(&run, 17, 1, 10), None); // "tail"
                                                                // A row whose tail really was blank keeps the tokens apart: the
                                                                // padding restores exactly what `grab_text` trimmed, so the two
                                                                // rows never fuse into one nonsense word.
        let run = vec!["https://a.co".to_string(), "notpartofit".to_string()];
        assert_eq!(url_in_wrapped_rows(&run, 40, 0, 0).as_deref(), Some("https://a.co"));
        assert_eq!(url_in_wrapped_rows(&run, 40, 1, 0), None);
        // A click past the end of the run's rows finds nothing, no panic.
        assert_eq!(url_in_wrapped_rows(&run, 40, 9, 0), None);
    }

    /// U19: `o` in copy mode opens the URL under the cursor — keyboard
    /// parity for a verb that was mouse-only. Core stashes it; the
    /// composition root does the I/O.
    #[test]
    fn copy_mode_o_stashes_the_url_under_the_cursor() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().grab = "go to https://a.co/x now".into();
        app.apply(Action::CopyMode);
        // Cursor is bottom-left; walk it onto the link with U17's `w`.
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
        assert_eq!(app.take_pending_open().as_deref(), Some("https://a.co/x"));
        assert!(app.take_pending_open().is_none(), "taking it clears it");
        assert!(matches!(app.mode, Mode::Copy { .. }), "opening a URL never exits the mode");

        // Off the link: an honest flash rather than a silent no-op.
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('0'), KeyModifiers::NONE));
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
        assert!(app.take_pending_open().is_none());
        assert_eq!(app.flash(), Some("no URL under the cursor"));
    }

    /// U20: `1..9` launches that row directly — the picker was arrows-and-
    /// Enter only, two or three keystrokes for a list you can already see.
    #[test]
    fn picker_number_accelerators_launch_that_row() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let items = crate::agents::picker_ids();
        assert!(items.len() >= 2, "this test needs a picker with a second row");
        let (mut app, _) = mk_app(shell_ws());
        let before = app.runtimes.len();
        app.apply(Action::QuickLaunch);
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal), "launching closes the picker");
        assert_eq!(app.runtimes.len(), before + 1);
        assert_eq!(
            app.find_spec(app.focused).map(|s| s.adapter.clone()),
            Some(items[1].to_string()),
            "the digit picks the row it labels, not the highlighted one"
        );

        // A digit past the end of the list is ignored; the picker stays up.
        let (mut app, _) = mk_app(shell_ws());
        let before = app.runtimes.len();
        app.apply(Action::QuickLaunch);
        app.handle_mode_key(KeyEvent::new(KeyCode::Char('9'), KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Picker { .. }));
        assert_eq!(app.runtimes.len(), before);
    }

    /// U20's other half: a click on a picker row launches it, through the
    /// same `picker_launch` the keyboard uses and the same drawn rect U8's
    /// modal gate hit-tests against.
    #[test]
    fn picker_click_launches_the_row_it_landed_on() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let items = crate::agents::picker_ids();
        let (mut app, _) = mk_app(shell_ws());
        let before = app.runtimes.len();
        app.apply(Action::QuickLaunch);
        let rect = crate::ui::render::modal_rect(&app).expect("the picker draws a dialog");
        let row = rect.y + 1 + (items.len() - 1) as u16; // the last item
        app.handle_modal_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x + 1,
                row,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
            Some(rect),
        );
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.runtimes.len(), before + 1);
        assert_eq!(
            app.find_spec(app.focused).map(|s| s.adapter.clone()),
            Some(items[items.len() - 1].to_string()),
        );
    }

    // -- U20: picker type-ahead + the recent-cwd column ---------------------

    /// U20: typing filters the adapter list, Backspace widens it back. The
    /// picker was a fixed list you could only walk; with three adapters that
    /// is already two keystrokes more than naming one.
    ///
    /// Asserts against `picker_filtered_ids` (raw ids), not `picker_filtered`
    /// (P2-13's *installed* annotation) — this test is about the substring
    /// filter, and pinning it to which adapters this machine happens to have
    /// on `$PATH` would make it pass or fail by host, not by logic.
    #[test]
    fn picker_type_ahead_filters_the_adapter_list_and_backspace_widens_it() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::QuickLaunch);
        assert_eq!(
            app.picker_filtered_ids().len(),
            crate::agents::picker_ids().len(),
            "no filter shows all"
        );
        app.handle_mode_key(key('l'));
        // Substring, not prefix: `l` finds both `claude` and `shell`.
        assert_eq!(app.picker_filtered_ids(), vec!["claude", "shell"]);
        app.handle_mode_key(key('a'));
        assert_eq!(app.picker_filtered_ids(), vec!["claude"]);
        app.handle_mode_key(key('z'));
        assert!(app.picker_filtered_ids().is_empty(), "a query matching nothing shows nothing");
        assert!(matches!(app.mode, Mode::Picker { .. }), "and the picker stays up");
        press(&mut app, crossterm::event::KeyCode::Backspace);
        assert_eq!(app.picker_filtered_ids(), vec!["claude"]);
        press(&mut app, crossterm::event::KeyCode::Backspace);
        assert_eq!(app.picker_filtered_ids(), vec!["claude", "shell"]);
    }

    /// U20: the `1..9` accelerators keep working *through* a filter — they
    /// address the rows on screen, not the unfiltered registry, so a digit
    /// always launches what the row it labels says.
    #[test]
    fn picker_accelerators_address_the_filtered_rows() {
        let (mut app, _) = mk_app(shell_ws());
        let before = app.runtimes.len();
        app.apply(Action::QuickLaunch);
        app.handle_mode_key(key('l')); // → [claude, shell]
        app.handle_mode_key(key('2')); // row 2 of the *filtered* list
        assert!(matches!(app.mode, Mode::Normal), "launching closes the picker");
        assert_eq!(app.runtimes.len(), before + 1);
        assert_eq!(
            app.find_spec(app.focused).map(|s| s.adapter.clone()),
            Some("shell".to_string()),
        );

        // A digit past the end of the *narrowed* list is ignored, and the
        // picker stays up — the rows carry their own numbers.
        let (mut app, _) = mk_app(shell_ws());
        let before = app.runtimes.len();
        app.apply(Action::QuickLaunch);
        app.handle_mode_key(key('l'));
        app.handle_mode_key(key('3'));
        assert!(matches!(app.mode, Mode::Picker { .. }));
        assert_eq!(app.runtimes.len(), before);
    }

    /// U20: a click still launches the row it landed on after a filter —
    /// the hit-test addresses the drawn rows, so keyboard and mouse can't
    /// disagree about what row 1 is.
    #[test]
    fn picker_click_still_lands_on_the_right_row_after_filtering() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::QuickLaunch);
        app.handle_mode_key(key('l')); // → [claude, shell]
        let rect = crate::ui::render::modal_rect(&app).expect("the picker draws a dialog");
        app.handle_modal_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x + 1,
                row: rect.y + 1, // first filtered row
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
            Some(rect),
        );
        assert_eq!(
            app.find_spec(app.focused).map(|s| s.adapter.clone()),
            Some("claude".to_string()),
        );
    }

    /// U20 / DESIGN.md §7: the recent-cwd column is seeded from the
    /// workspace, grows when a pane `cd`s somewhere new, and is what the
    /// picker launches into — so opening an agent in another project costs
    /// no shell round-trip.
    #[test]
    fn picker_launches_into_the_selected_recent_directory() {
        use crossterm::event::KeyCode;
        let (mut app, _) = mk_app(shell_ws());
        assert_eq!(app.picker_cwds(), [PathBuf::from("/tmp")], "seeded from the workspace");

        // A pane cd's into a project; the next observation records it.
        let id = app.focused;
        app.runtimes.get_mut(&id).unwrap().observation =
            Some(Observation { cwd: Some(PathBuf::from("/work/proj")), agent: None });
        app.last_detect = Instant::now() - Duration::from_secs(60);
        app.tick();
        assert_eq!(
            app.picker_cwds(),
            [PathBuf::from("/work/proj"), PathBuf::from("/tmp")],
            "most recent first, deduplicated"
        );

        // The picker opens on the most recent, and `→` + `↓` walks to the
        // older one; Enter launches there.
        app.apply(Action::QuickLaunch);
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.find_spec(app.focused).map(|s| s.cwd.clone()),
            Some(PathBuf::from("/tmp")),
            "the launch uses the cwd column's selection"
        );
        // ...and using a directory floats it back to the top.
        assert_eq!(app.picker_cwds()[0], PathBuf::from("/tmp"));
    }

    /// A registry substitute for "pi" whose `launch()` names a program that
    /// will never exist on any real `$PATH` — deterministic proof that an
    /// adapter is "not installed" without mutating the real `$PATH` (every
    /// picker test — including render.rs's own — reads the SAME process-
    /// global var through `App::registry`, which would make `$PATH`
    /// mutation here exactly as unsafe as `$SHELL` is for `mk_app`).
    struct NeverInstalledAdapter;
    impl agents::AgentAdapter for NeverInstalledAdapter {
        fn id(&self) -> &'static str {
            "pi"
        }
        fn launch(&self, cwd: &Path) -> agents::CommandSpec {
            agents::CommandSpec::new("roost-test-definitely-does-not-exist-anywhere-xyz123", cwd)
        }
        fn resume(&self, cwd: &Path, _session: &str) -> agents::CommandSpec {
            self.launch(cwd)
        }
        fn resume_flag(&self) -> &'static str {
            "--resume" // unused: resume() overridden above
        }
    }

    /// P2-13 end-to-end: a not-installed adapter's picker row is annotated
    /// (not hidden — six adapters is short enough that hiding five would
    /// read as a picker that lost its list), and — the property that
    /// actually matters — launching that row still spawns the RAW id, never
    /// the annotated display text. `agents::picker_ids()` is the static, always-6
    /// registry list (`agents::adapter_specs()`), so the substitution has to
    /// happen in `App::registry` (what `adapter_installed` actually
    /// consults), not in what rows get drawn.
    #[test]
    fn picker_filtered_annotates_uninstalled_adapters_but_launch_still_spawns_the_raw_id() {
        let mut registry = agents::registry();
        registry.insert("pi", Box::new(NeverInstalledAdapter));
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut app = App::<FakePane>::new(
            shell_ws(),
            registry,
            Box::new(MemStore::default()),
            tx,
            Size::new(100, 30),
            (0, 0),
            None,
            TokenTable::new().unwrap(),
        )
        .unwrap();

        app.apply(Action::QuickLaunch);
        let rows = app.picker_filtered();
        app.handle_mode_key(key('1')); // row 1 = index 0 = "pi" (agents::picker_ids() order)
        let spawned = app.find_spec(app.focused).map(|s| s.adapter.clone());

        assert_eq!(rows[0], "pi not found", "pi's substituted launch program can never resolve");
        // ShellAdapter resolves an absolute $SHELL (or /bin/bash fallback)
        // path directly, not via a $PATH scan — real, unmodified, and
        // guaranteed to exist on any machine this suite already assumes one
        // on (the harness's `pgrep -P`, every shell-spawning test, ...).
        let shell_at = crate::agents::picker_ids().iter().position(|&id| id == "shell").unwrap();
        assert_eq!(rows[shell_at], "shell", "resolved via an absolute path, not $PATH");
        assert_eq!(
            spawned,
            Some("pi".to_string()),
            "launch used the raw id, not the annotated text"
        );
    }

    /// U20: `↑`/`↓` steer whichever column has the keyboard, and `←`/`→`
    /// hand it over. Without this the second column would be unreachable.
    #[test]
    fn picker_arrows_steer_the_focused_column_only() {
        use crossterm::event::KeyCode;
        let (mut app, _) = mk_app(shell_ws());
        app.note_cwd(PathBuf::from("/a"));
        app.note_cwd(PathBuf::from("/b")); // → [/b, /a, /tmp]
        app.apply(Action::QuickLaunch);
        press(&mut app, KeyCode::Down);
        let Mode::Picker { selection, cwd, on_cwd, .. } = &app.mode else { panic!("picker") };
        assert_eq!((*selection, *cwd, *on_cwd), (1, 0, false), "↓ moves the adapter column");
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        let Mode::Picker { selection, cwd, on_cwd, .. } = &app.mode else { panic!("picker") };
        assert_eq!((*selection, *cwd, *on_cwd), (1, 2, true), "→ hands ↑↓ to the cwd column");
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Up);
        let Mode::Picker { selection, cwd, on_cwd, .. } = &app.mode else { panic!("picker") };
        assert_eq!((*selection, *cwd, *on_cwd), (0, 2, false), "← hands them back");
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
        assert!(matches!(app.ws.tabs[0].layout, LayoutNode::Split { dir: SplitDir::Vertical, .. }));
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
        // C32: Alt+r opens the combined editor with the point on the name
        // row, so the old rename flow — chord, type, Enter — is unchanged
        // keystroke for keystroke.
        app.apply(Action::EditPane);
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
        fn resume_flag(&self) -> &'static str {
            "--resume" // unused: resume() overridden above
        }
        fn session_root(&self, cwd: &std::path::Path) -> Option<PathBuf> {
            Some(cwd.to_path_buf())
        }
    }

    /// A pane promoted to an agent must not claim a session from *before*
    /// it was a shell.
    ///
    /// The reported workflow: open a shell pane, type `pi`. That is the
    /// **promotion** path, and it seeds detection with `SystemTime::
    /// UNIX_EPOCH` — no lower bound at all — so the scan takes the newest
    /// unclaimed file in that cwd whenever it was written. Agent CLIs write
    /// their session file *after* starting, so if the 2s tick lands in that
    /// gap the newest unclaimed file is a **previous conversation**, and
    /// `set_session` commits it and drops the pane from `pending_detect` —
    /// permanently. The next quit/relaunch then resumes last week's
    /// session, which is what "it loads the wrong session" looks like.
    ///
    /// The taken-set does not save this: `claimed_sessions` only knows ids
    /// stored on live panes, so any conversation from a closed pane or an
    /// earlier run is eligible.
    #[test]
    fn a_promoted_pane_never_claims_a_session_older_than_its_shell() {
        let dir = std::env::temp_dir().join(format!("roost-promote-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Last week's conversation in this project, sitting in the session
        // root — unclaimed, because the pane that made it is long gone.
        let old = dir.join("old.jsonl");
        std::fs::write(&old, "").unwrap();
        let long_ago = SystemTime::now() - Duration::from_secs(7 * 24 * 3600);
        std::fs::File::open(&old).unwrap().set_modified(long_ago).unwrap();

        let mut panes = HashMap::new();
        // Starts as a *shell* — the reported workflow is opening a pane and
        // typing `pi` into it, which is what makes this the promotion path.
        panes.insert(
            1,
            PaneSpec {
                adapter: "shell".into(),
                cwd: dir.clone(),
                session: None,
                title: None,
                spawned_by: None,
                note: None,
                noted_at: None,
            },
        );
        let ws = Workspace {
            version: 1,
            active_tab: 0,
            tabs: vec![Tab { name: "main".into(), layout: LayoutNode::Pane(1), panes }],
        };
        let store = MemStore::default();
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut registry = agents::registry();
        registry.insert("detect", Box::new(DetectAdapter));
        let mut app = App::<FakePane>::new(
            ws,
            registry,
            Box::new(store),
            tx,
            Size::new(100, 30),
            (0, 0),
            None,
            TokenTable::new().unwrap(),
        )
        .unwrap();

        // Drive the **real** promotion path rather than hand-seeding the
        // window: the first draft did the latter, and reverting the fix
        // then left it green — it proved the scan honours a bound, not that
        // promotion supplies one. Tick once seeing a plain shell (which
        // records `last_shell_seen`), then again seeing the agent.
        app.pending_detect.clear();
        app.runtimes.get_mut(&1).unwrap().observation =
            Some(crate::ports::Observation { cwd: None, agent: None });
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();

        app.runtimes.get_mut(&1).unwrap().observation =
            Some(crate::ports::Observation { cwd: None, agent: Some("detect".into()) });
        // `tick` self-throttles on `last_detect`, which `App::new` set to
        // now — without this the call returns instantly and the assertion
        // below passes having exercised nothing. (It did, on the first
        // draft of this test.)
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();

        assert_eq!(
            app.find_spec(1).unwrap().session,
            None,
            "claimed a conversation that predates the shell this pane was",
        );

        // ...and once the agent *does* write its file, the same pane picks
        // it up. A bound that excluded everything would pass the assertion
        // above while breaking detection outright.
        let fresh = dir.join("fresh.jsonl");
        std::fs::write(&fresh, "").unwrap();
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();
        assert_eq!(
            app.find_spec(1).unwrap().session.as_deref(),
            Some("fresh"),
            "the session this pane actually started must still be found",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ...and the same must hold for a pane that *inherits* an id, which is
    /// how the bound above was reopened.
    ///
    /// Pane ids are recycled (`alloc_pane_id` is max-live+1, C23), and
    /// `last_shell_seen` — the map that supplies the promotion floor — was
    /// the one `PaneId`-keyed map `close_pane_id` did not prune. So a pane
    /// that sat at a shell prompt for hours left its stamp behind, and the
    /// next pane to take that id promoted against *that* clock instead of
    /// its own birth. Promoting before ever being seen as a shell is not
    /// exotic: a pane spawned with initial input (the control plane's
    /// `spawn --input`, the quick-launch path) is running its agent within
    /// milliseconds, so the first `observe_panes` tick that sees it already
    /// sees the agent.
    ///
    /// The consequence is identical to the original defect — the scan takes
    /// the newest unclaimed file in the window, `set_session` commits it and
    /// drops the pane from `pending_detect`, so the next relaunch resumes an
    /// unrelated conversation, permanently.
    #[test]
    fn a_recycled_pane_id_does_not_inherit_the_dead_panes_promotion_floor() {
        let dir = std::env::temp_dir().join(format!("roost-recycle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // An unrelated conversation from an hour ago, unclaimed because the
        // pane that made it is long closed.
        let old = dir.join("old.jsonl");
        std::fs::write(&old, "").unwrap();
        std::fs::File::open(&old)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(3600))
            .unwrap();

        let mut registry = agents::registry();
        registry.insert("detect", Box::new(DetectAdapter));
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut app = App::<FakePane>::new(
            Workspace::default_in(dir.clone()),
            registry,
            Box::new(MemStore::default()),
            tx,
            Size::new(100, 30),
            (0, 0),
            None,
            TokenTable::new().unwrap(),
        )
        .unwrap();

        // A shell pane that has been sitting at its prompt for hours.
        // `observe_panes` stamps `SystemTime::now()`, so the passage of time
        // is seeded rather than waited for; what is under test is what
        // `close_pane_id` does with the stamp, not how it got there.
        let dead = app.spawn_child("shell", Some(dir.clone()), None).expect("room to split");
        app.last_shell_seen.insert(dead, SystemTime::now() - Duration::from_secs(2 * 3600));
        app.close_pane_id(dead);
        assert!(
            !app.last_shell_seen.contains_key(&dead),
            "a closed pane's shell stamp outlived it",
        );

        // A new pane takes the freed id and is running its agent by the
        // first tick — never observed as a shell, so its floor can only come
        // from whatever `last_shell_seen` still holds for this id.
        let id = app.spawn_child("shell", Some(dir.clone()), None).expect("room to split");
        assert_eq!(id, dead, "the id must actually be recycled for this to test anything");
        app.pending_detect.clear();
        app.runtimes.get_mut(&id).unwrap().observation =
            Some(crate::ports::Observation { cwd: None, agent: Some("detect".into()) });
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();

        assert_eq!(
            app.find_spec(id).unwrap().session,
            None,
            "the recycled id claimed a conversation from before it existed",
        );

        // ...and the floor is still a floor, not a wall: a file this pane
        // could plausibly have written is picked up on the next tick.
        std::fs::write(dir.join("fresh.jsonl"), "").unwrap();
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();
        assert_eq!(
            app.find_spec(id).unwrap().session.as_deref(),
            Some("fresh"),
            "the session this pane actually started must still be found",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// C22: the float lives outside every tab's layout tree — including
    /// when its id comes from the same pool a parked tab's panes do.
    ///
    /// `alloc_pane_id` guards the float and the undo stack separately; this
    /// is the crossing case. Close a tab (parking its ids), open the float,
    /// then reopen the tab: if the float took an id the tab is about to
    /// bring back, `is_float` becomes true for a real tiled pane and both
    /// share one entry in `runtimes`.
    #[test]
    fn the_float_never_takes_an_id_a_parked_tab_will_bring_back() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewTab);
        let parked: Vec<PaneId> = app.ws.tabs[app.ws.active_tab].panes.keys().copied().collect();
        for id in &parked {
            app.close_pane_id(*id);
        }
        app.apply(Action::ToggleFloat);
        let float_id = app.float.as_ref().expect("the float is up").id;
        assert!(
            !parked.contains(&float_id),
            "the float took id {float_id}, which the parked tab still owns",
        );

        app.apply(Action::Undo);
        let tree: Vec<PaneId> = app.ws.tabs.iter().flat_map(|t| t.panes.keys().copied()).collect();
        assert!(
            !tree.contains(&float_id),
            "C22: the float's id {float_id} is now a tiled pane too — tree {tree:?}",
        );
    }

    /// F3/C20: a closed pane must not bequeath its attention state to
    /// whatever pane next takes its id.
    ///
    /// `last_status`, `needy_msgs` and `visited_waiting` were pruned only on
    /// the tick, under a comment claiming "ids are never reused" — which the
    /// close path itself contradicts a thousand lines away, where `raw`,
    /// `dead`, `tab_focus` and the token table are all pruned *at close*
    /// precisely because ids ARE recycled (`alloc_pane_id` is a max+1, and a
    /// closed pane's id is not parked). Close the highest-id pane and open
    /// one inside the 2s `DETECT_INTERVAL` and the newborn inherits all
    /// three: it is silently absent from Alt+a's fallback though nobody ever
    /// looked at it, its first status observation reads as a *transition*
    /// and prints a feed line for a life it did not live, and the dead
    /// pane's question gets attributed to it.
    #[test]
    fn a_closed_panes_attention_state_does_not_outlive_it() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1|2, focus=2
        app.find_spec_mut(2).unwrap().adapter = "pi".into();
        app.runtimes.get_mut(&2).unwrap().set_extension_status(AgentStatus::Waiting);
        app.set_focus(1);
        app.apply(Action::JumpAttention); // visits pane 2 — marks it seen
        app.needy_msgs.insert(2, "pick a database".into());
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick(); // `last_status[2]` = Waiting
        assert!(app.visited_waiting.contains(&2), "setup: pane 2 was visited");
        assert!(app.last_status.contains_key(&2), "setup: pane 2 has a status");

        app.close_pane_id(2);

        assert!(
            !app.visited_waiting.contains(&2),
            "F3: pane 2's `already looked at this` mark outlived the pane",
        );
        assert!(
            !app.last_status.contains_key(&2),
            "C20: pane 2's last status outlived the pane — the next pane to \
             take id 2 would have its birth read as a transition",
        );
        assert!(!app.needy_msgs.contains_key(&2), "pane 2's question outlived the pane",);
    }

    /// C38: Alt+u must respect the same floor Alt+n does.
    ///
    /// `restore_pane` hand-rolled the aspect test and never checked
    /// `MIN_SPLIT_COLS`/`MIN_SPLIT_ROWS`, so undo would split a pane that
    /// `spawn_child` had just refused to split — landing the reopened pane
    /// in a slot too small to render in, and driving the *other* half under
    /// the floor too. Same rect, same keypress-level decision, opposite
    /// answer.
    #[test]
    fn undo_refuses_the_split_alt_n_just_refused() {
        let (mut app, _) = mk_app(shell_ws());
        // Something on the undo stack to reopen, taken while there is room.
        app.apply(Action::NewPane);
        let victim = app.focused;
        app.close_pane_id(victim);

        // Undo splits `pane_order().first()`, so shrink *that* pane until
        // it is under the floor — Alt+n itself refuses on it here.
        for _ in 0..12 {
            let first = app.pane_order()[0];
            app.set_focus(first);
            if app.spawn_child("shell", None, None).is_none() {
                break;
            }
        }
        let first = app.pane_order()[0];
        app.set_focus(first);
        assert!(
            app.spawn_child("shell", None, None).is_none(),
            "setup: the first pane should be too small to split",
        );
        let before: Vec<PaneId> = app.pane_order();

        app.apply(Action::Undo);

        assert_eq!(app.pane_order(), before, "Alt+u split a pane Alt+n had just refused to split",);
        assert!(
            app.flash().is_some_and(|f| f.contains("no room")),
            "the refusal is spoken, not silent: {:?}",
            app.flash(),
        );
    }

    /// C21/C22: closing the pane that is actually zoomed must end the zoom,
    /// even when the float is covering it.
    ///
    /// `close_pane_id` asked `id == self.focused`, but while the float is
    /// shown focus belongs to the float — never to a tiled pane — so the
    /// test could not fire. `display_rects` already knows the real target is
    /// `float.prev_focus`; the close path did not, so it left `zoomed` set
    /// pointing at a pane that no longer exists, and the renderer was handed
    /// a rect for it.
    #[test]
    fn closing_the_zoomed_pane_under_the_float_ends_the_zoom() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1|2, focus=2
        app.apply(Action::ToggleZoom);
        assert!(app.zoomed(), "setup: pane 2 is zoomed");
        let zoomed = app.focused;
        app.apply(Action::ToggleFloat);
        assert!(app.float_focused(), "setup: the float has focus");

        app.close_pane_id(zoomed);

        let live: Vec<PaneId> = app.ws.active_tab().panes.keys().copied().collect();
        for pr in app.display_rects() {
            assert!(
                app.is_float(pr.id) || live.contains(&pr.id),
                "the renderer was handed pane {} — gone from the tab {live:?}",
                pr.id,
            );
        }
    }

    /// C22 rule 1: a shown float IS the focused pane. Closing some other
    /// pane must not quietly take that away.
    ///
    /// The close path's "keep focus inside the tab on screen" fallback asked
    /// whether `self.focused` is in the active tab's map. The float lives
    /// outside every tab's map by construction, so while it was up the
    /// answer was always no and the fallback fired on every close, yanking
    /// focus off the float onto a tiled pane while the float stayed on
    /// screen — keystrokes going to a pane the user cannot see.
    #[test]
    fn closing_a_pane_does_not_steal_focus_from_a_shown_float() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::NewPane); // three panes
        let victim = app.focused;
        app.apply(Action::ToggleFloat);
        assert!(app.float_focused(), "setup: the float has focus");

        app.close_pane_id(victim);

        if app.float.as_ref().is_some_and(|f| f.shown) {
            assert!(
                app.float_focused(),
                "C22: the float is on screen but focus is on {}",
                app.focused,
            );
        }
    }

    /// C22 rule 1, at the one chokepoint: every focus move off the float
    /// takes the float down with it.
    ///
    /// The rule was enforced per call site, so each new focus path had to
    /// remember it — and C35's "go back" did not. The float stayed drawn
    /// over the body while keystrokes went to a tiled pane underneath.
    #[test]
    fn every_focus_move_off_the_float_hides_it() {
        for (name, action) in [
            ("FocusAlternate", Action::FocusAlternate),
            ("Focus(Left)", Action::Focus(layout::Dir::Left)),
            ("NextTab", Action::NextTab),
        ] {
            let (mut app, _) = mk_app(shell_ws());
            app.apply(Action::NewPane);
            app.apply(Action::NewTab);
            app.go_to_tab(0);
            app.apply(Action::Focus(layout::Dir::Right));
            app.apply(Action::Focus(layout::Dir::Left)); // give C35 an alternate
            app.apply(Action::ToggleFloat);
            assert!(app.float_focused(), "{name}: setup — the float has focus");

            app.apply(action);

            if !app.float_focused() {
                assert!(
                    !app.float.as_ref().is_some_and(|f| f.shown),
                    "{name}: the float is still drawn but focus is on {}",
                    app.focused,
                );
            }
        }
    }

    /// A control spawn borrows focus and gives it back. It must give back
    /// the float's visibility with it, or the pair ends up in a state the
    /// model does not have: hidden and focused at the same time.
    ///
    /// From there `cycle_layout` — which arranges `pane_order()` around
    /// whatever is focused — planted the float's id into the tab's layout
    /// tree as a pane with no spec.
    #[test]
    fn a_control_spawn_gives_back_the_float_it_borrowed_focus_from() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        assert!(app.float_focused(), "setup: the float has focus");

        let ct = app.control_token().to_string();
        app.handle_control(crate::core::control::Request {
            token: ct,
            method: crate::core::control::Method::Spawn {
                adapter: "shell".into(),
                cwd: None,
                initial_input: None,
            },
        });

        assert_eq!(app.focused, float_id, "focus was borrowed, not taken");
        assert!(
            app.float.as_ref().is_some_and(|f| f.shown),
            "the float is focused but no longer drawn",
        );

        // The state that used to leak: a hidden-but-focused float becomes a
        // ghost pane in the tree the moment anything arranges by focus.
        app.apply(Action::CycleLayout { forward: true });
        let tree: Vec<PaneId> = {
            let mut v = Vec::new();
            layout::pane_order(&app.ws.active_tab().layout, &mut v);
            v
        };
        for id in &tree {
            assert!(
                app.ws.active_tab().panes.contains_key(id),
                "pane {id} is in the tree with no spec — tree {tree:?}",
            );
        }
    }

    // -- the layout invariant fuzzer -----------------------------------------
    //
    // roost's structural state is three things that must agree: each tab's
    // layout tree, its `panes` map, and the workspace-wide `runtimes` map —
    // with the float sitting deliberately outside all three. Nearly every
    // action moves at least two of them, and the crossings (zoom under a
    // float, a control-plane close of a pane in another tab, undo after a
    // tab was removed) are where they drift apart. Each such drift is
    // invisible until it isn't: a pane in the tree with no spec, a runtime
    // nothing can reach, a rect handed to the renderer for a pane that is
    // gone.
    //
    // Hand-written tests cover the crossings someone thought of. This walks
    // long random sequences and re-checks every invariant after every single
    // operation, so the failure arrives with the exact trail that produced
    // it. It has earned its runtime: the one-member stack, both float
    // close-path crossings, C35's go-back, and the control-spawn float
    // restore were all found here, none of them by reading the code.

    fn inv_tree_ids(node: &LayoutNode) -> Vec<PaneId> {
        let mut v = Vec::new();
        layout::pane_order(node, &mut v);
        v
    }

    fn inv_check_node(node: &LayoutNode, ctx: &str, tab: usize) {
        match node {
            LayoutNode::Pane(_) => {}
            LayoutNode::Stack { children, expanded } => {
                // >= 2, not merely non-empty (C6, clarified 2026-08-20): a
                // stack of one is a pane, and `layout.rs` normalizes it into
                // one wherever it can arise. The weaker assertion let the
                // one-member stack that `toggle_stack` turns into a
                // single-child `Split` walk right past this checker — the
                // design audit had to find it by reading. Nothing guards a
                // future construction path unless this does.
                assert!(
                    children.len() >= 2,
                    "{ctx}: tab {tab} has a Stack with {} member(s) — a stack of one is a pane",
                    children.len()
                );
                assert!(
                    *expanded < children.len(),
                    "{ctx}: tab {tab} stack expanded={expanded} out of range ({} members)",
                    children.len()
                );
            }
            LayoutNode::Split { children, ratios, .. } => {
                assert!(
                    children.len() >= 2,
                    "{ctx}: tab {tab} has a Split with {} child(ren) — layout.rs says it never builds one",
                    children.len()
                );
                assert_eq!(
                    ratios.len(),
                    children.len(),
                    "{ctx}: tab {tab} split ratios {ratios:?} vs {} children",
                    children.len()
                );
                for c in children {
                    inv_check_node(c, ctx, tab);
                }
            }
        }
    }

    /// THE invariant: per tab, the set of PaneIds in `layout` == the keys of
    /// `panes`; ids are unique within a tab AND across tabs; the float is in
    /// no tree.
    pub(crate) fn inv_check(app: &App<FakePane>, ctx: &str) {
        use std::collections::HashSet;
        let mut global: HashMap<PaneId, usize> = HashMap::new();
        for (i, tab) in app.ws.tabs.iter().enumerate() {
            let ids = inv_tree_ids(&tab.layout);
            let set: HashSet<PaneId> = ids.iter().copied().collect();
            assert_eq!(set.len(), ids.len(), "{ctx}: tab {i} tree lists a pane twice: {ids:?}");
            let keys: HashSet<PaneId> = tab.panes.keys().copied().collect();
            assert_eq!(set, keys, "{ctx}: tab {i} tree ids {set:?} != panes map keys {keys:?}");
            if let Some(f) = &app.float {
                assert!(
                    !set.contains(&f.id),
                    "{ctx}: tab {i} tree contains the float's id {}",
                    f.id
                );
            }
            for id in ids {
                if let Some(prev) = global.insert(id, i) {
                    panic!("{ctx}: pane {id} is in tab {prev} AND tab {i}");
                }
            }
            inv_check_node(&tab.layout, ctx, i);
        }
        if app.float.as_ref().is_some_and(|f| !f.shown && f.id == app.focused) {
            panic!("{ctx}: the float is HIDDEN but still focused");
        }
        // C22 rule 1: a shown float is the focused pane.
        if app.float.as_ref().is_some_and(|f| f.shown) {
            assert!(
                app.float_focused(),
                "{ctx}: the float is shown but focus is on {}",
                app.focused
            );
        }
        // Focus is always a real pane: the float, or a pane in the tab on screen.
        assert!(
            app.is_float(app.focused) || app.ws.active_tab().panes.contains_key(&app.focused),
            "{ctx}: focused pane {} is in neither the active tab nor the float",
            app.focused
        );
        // Nothing is drawn that does not exist.
        let live: HashSet<PaneId> = app.ws.active_tab().panes.keys().copied().collect();
        for pr in app.display_rects() {
            assert!(
                app.is_float(pr.id) || live.contains(&pr.id),
                "{ctx}: display_rects hands the renderer pane {} — not in the active tab {live:?}",
                pr.id
            );
        }
        // (A pane that exists but is allocated no area is a real hazard,
        // but at these densities — 20 panes in 58 rows — it is honest
        // overcrowding rather than a bug. The one case that ISN'T is
        // covered by `a_split_never_allocates_its_last_child_nothing`.)
        // No runtime outlives its pane: `runtimes` is keyed by PaneId
        // workspace-wide, and a leftover entry is a live child process
        // nothing can reach.
        let all_panes: HashSet<PaneId> =
            app.ws.tabs.iter().flat_map(|t| t.panes.keys().copied()).collect();
        let orphan_runtimes: Vec<PaneId> = app
            .runtimes
            .keys()
            .copied()
            .filter(|id| !all_panes.contains(id) && !app.is_float(*id))
            .collect();
        assert!(
            orphan_runtimes.is_empty(),
            "{ctx}: runtimes {orphan_runtimes:?} belong to no pane and no float"
        );
        // Split ratios describe a whole: they are renormalised on removal
        // and traded on resize, so they always sum to 1.
        fn inv_ratio_sums(node: &LayoutNode, ctx: &str) {
            if let LayoutNode::Split { children, ratios, .. } = node {
                let sum: f32 = ratios.iter().sum();
                assert!(
                    (sum - 1.0).abs() < 0.01,
                    "{ctx}: split ratios {ratios:?} sum to {sum}, not 1"
                );
                for c in children {
                    inv_ratio_sums(c, ctx);
                }
            }
        }
        for tab in &app.ws.tabs {
            inv_ratio_sums(&tab.layout, ctx);
        }
    }

    fn inv_app(cols: u16, rows: u16) -> App<FakePane> {
        let store = MemStore::default();
        let (tx, _rx) = mpsc::sync_channel(4096);
        App::<FakePane>::new(
            shell_ws(),
            agents::registry(),
            Box::new(store),
            tx,
            Size::new(cols, rows),
            (0, 0),
            None,
            TokenTable::new().unwrap(),
        )
        .unwrap()
    }

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0 >> 16
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    fn inv_has_stack(node: &LayoutNode) -> bool {
        match node {
            LayoutNode::Pane(_) => false,
            LayoutNode::Stack { .. } => true,
            LayoutNode::Split { children, .. } => children.iter().any(inv_has_stack),
        }
    }

    /// Long random operation sequences must never break the tree↔map
    /// invariant.
    #[test]
    fn layout_tree_and_panes_map_never_diverge_under_random_operations() {
        use crate::core::layout::Dir;
        let dirs = [Dir::Left, Dir::Right, Dir::Up, Dir::Down];
        let mut coverage: HashMap<&'static str, usize> = HashMap::new();
        let mut stacks_seen = 0usize;
        let mut zoom_seen = 0usize;
        let mut float_seen = 0usize;
        let mut float_and_zoom = 0usize;
        let mut multi_tab = 0usize;
        let mut ops_total = 0usize;
        for seed in 0..200u64 {
            let mut rng = Lcg(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(12345));
            let mut app = inv_app(200, 60);
            let mut trail: Vec<String> = Vec::new();
            'step: for step in 0..400 {
                let would_quit = app.ws.tabs.len() == 1 && app.ws.active_tab().panes.len() == 1;
                let (name, action) = loop {
                    let pick = rng.below(42);
                    let d = dirs[rng.below(4) as usize];
                    let a = match pick {
                        0..=3 => ("NewPane", Action::NewPane),
                        4 | 5 => {
                            if would_quit {
                                continue;
                            }
                            ("ClosePane", Action::ClosePane)
                        }
                        6..=8 => ("Focus", Action::Focus(d)),
                        9..=11 => ("MovePane", Action::MovePane(d)),
                        12 => ("NewTab", Action::NewTab),
                        13 => ("MovePaneToTab+", Action::MovePaneToTab { forward: true }),
                        14 => ("MovePaneToTab-", Action::MovePaneToTab { forward: false }),
                        15 => ("NextTab", Action::NextTab),
                        16 => ("PrevTab", Action::PrevTab),
                        17 | 18 => ("ToggleStack", Action::ToggleStack),
                        19 => ("FlipSplit", Action::FlipSplit),
                        20 => (
                            "Resize",
                            Action::Resize {
                                horizontal: rng.below(2) == 0,
                                grow: rng.below(2) == 0,
                            },
                        ),
                        21 => ("Undo", Action::Undo),
                        22 => ("ToggleZoom", Action::ToggleZoom),
                        23 => ("CycleLayout+", Action::CycleLayout { forward: true }),
                        24 => ("CycleLayout-", Action::CycleLayout { forward: false }),
                        25 => ("ToggleFloat", Action::ToggleFloat),
                        26 => ("GoToTab", Action::GoToTab(rng.below(4) as usize)),
                        27 => {
                            // Control-plane close of an arbitrary live pane
                            // (the other close_pane_id caller). Refuses the
                            // last pane itself, so nothing to guard here.
                            let all: Vec<PaneId> =
                                app.ws.tabs.iter().flat_map(|t| t.panes.keys().copied()).collect();
                            if all.is_empty() {
                                continue;
                            }
                            let victim = all[rng.below(all.len() as u64) as usize];
                            let ct = app.control_token().to_string();
                            app.handle_control(crate::core::control::Request {
                                token: ct,
                                method: crate::core::control::Method::Close {
                                    pane: victim,
                                    force: true,
                                },
                            });
                            *coverage.entry("CtlClose").or_default() += 1;
                            ops_total += 1;
                            trail.push(format!("{step}:CtlClose({victim})"));
                            inv_check(&app, &format!("seed {seed} after [{}]", trail.join(",")));
                            continue 'step;
                        }
                        28 => {
                            let (w, h) = match rng.below(4) {
                                0 => (200u16, 60u16),
                                1 => (120, 40),
                                2 => (80, 24),
                                _ => (60, 18),
                            };
                            app.on_resize(Size::new(w, h), (0, 0));
                            *coverage.entry("Resize(term)").or_default() += 1;
                            ops_total += 1;
                            trail.push(format!("{step}:Term({w}x{h})"));
                            inv_check(&app, &format!("seed {seed} after [{}]", trail.join(",")));
                            continue 'step;
                        }
                        29 => ("LastTab", Action::LastTab),
                        30 => ("JumpAttention", Action::JumpAttention),
                        31 => {
                            // C27: jump to an arbitrary pane through the
                            // roster's own path (tab switch + stack expand).
                            let all: Vec<PaneId> =
                                app.ws.tabs.iter().flat_map(|t| t.panes.keys().copied()).collect();
                            if all.is_empty() {
                                continue;
                            }
                            let dest = all[rng.below(all.len() as u64) as usize];
                            app.apply(Action::ToggleRoster);
                            app.roster_jump(dest);
                            *coverage.entry("RosterJump").or_default() += 1;
                            ops_total += 1;
                            trail.push(format!("{step}:RosterJump({dest})"));
                            inv_check(&app, &format!("seed {seed} after [{}]", trail.join(",")));
                            continue 'step;
                        }
                        32 => {
                            // Control-plane spawn: the other pane-creating
                            // path, which allocates its own id.
                            let ct = app.control_token().to_string();
                            app.handle_control(crate::core::control::Request {
                                token: ct,
                                method: crate::core::control::Method::Spawn {
                                    adapter: "shell".into(),
                                    cwd: None,
                                    initial_input: None,
                                },
                            });
                            *coverage.entry("CtlSpawn").or_default() += 1;
                            ops_total += 1;
                            trail.push(format!("{step}:CtlSpawn"));
                            inv_check(&app, &format!("seed {seed} after [{}]", trail.join(",")));
                            continue 'step;
                        }
                        33 => {
                            // U25: jump out of the activity feed (the third
                            // `focus_attention_target` caller).
                            let all: Vec<PaneId> =
                                app.ws.tabs.iter().flat_map(|t| t.panes.keys().copied()).collect();
                            if all.is_empty() {
                                continue;
                            }
                            let dest = all[rng.below(all.len() as u64) as usize];
                            app.push_feed("fuzz".into(), false, Some(dest));
                            app.apply(Action::ToggleFeed);
                            app.handle_mode_key(crossterm::event::KeyEvent::from(
                                crossterm::event::KeyCode::Enter,
                            ));
                            app.mode = Mode::Normal;
                            *coverage.entry("FeedJump").or_default() += 1;
                            ops_total += 1;
                            trail.push(format!("{step}:FeedJump({dest})"));
                            inv_check(&app, &format!("seed {seed} after [{}]", trail.join(",")));
                            continue 'step;
                        }
                        34 => {
                            // The rest of the control plane, with arguments a
                            // machine caller can really send: pane ids that
                            // do not exist, an empty `text`, a `tail` past
                            // any scrollback, a `wait` naming panes in bulk.
                            // `CtlClose`/`CtlSpawn` above are the two verbs
                            // that change structure; these are the ones that
                            // read it, and they reach the same maps and the
                            // same `find_spec`/`tab_of` lookups. A panic or a
                            // drift here is a control call taking the fleet
                            // down (`tests/panic_shutdown.rs`), which is why
                            // they are walked and not merely unit-tested.
                            use crate::core::control::{Method, ReadMode, Request};
                            let all: Vec<PaneId> =
                                app.ws.tabs.iter().flat_map(|t| t.panes.keys().copied()).collect();
                            let any = |rng: &mut Lcg| -> PaneId {
                                // Half real ids, half ids that name nothing.
                                if all.is_empty() || rng.below(2) == 0 {
                                    rng.below(64)
                                } else {
                                    all[rng.below(all.len() as u64) as usize]
                                }
                            };
                            let method = match rng.below(7) {
                                0 => Method::List,
                                1 => Method::Status {
                                    pane: (rng.below(2) == 0).then(|| any(&mut rng)),
                                },
                                2 => Method::Fork {
                                    pane: (rng.below(2) == 0).then(|| any(&mut rng)),
                                },
                                3 => Method::Send {
                                    pane: any(&mut rng),
                                    text: ["", "hi", "\u{4e2d}\u{1f600}", "\x1b[2J"]
                                        [rng.below(4) as usize]
                                        .into(),
                                    submit: rng.below(2) == 0,
                                },
                                4 => Method::Broadcast {
                                    text: ["", "go"][rng.below(2) as usize].into(),
                                    submit: rng.below(2) == 0,
                                },
                                5 => Method::Read {
                                    pane: any(&mut rng),
                                    mode: match rng.below(3) {
                                        0 => ReadMode::Screen,
                                        1 => ReadMode::Tail(rng.below(100_000) as usize),
                                        _ => ReadMode::Full,
                                    },
                                },
                                _ => Method::Wait {
                                    panes: (0..rng.below(80)).map(|_| any(&mut rng)).collect(),
                                    until: ["waiting", "idle", "nonsense", ""]
                                        [rng.below(4) as usize]
                                        .into(),
                                    timeout_ms: match rng.below(3) {
                                        0 => None,
                                        1 => Some(0),
                                        _ => Some(u64::MAX),
                                    },
                                },
                            };
                            let name = method_summary(&method);
                            let ct = app.control_token().to_string();
                            app.handle_control(Request { token: ct, method });
                            *coverage.entry("CtlVerb").or_default() += 1;
                            ops_total += 1;
                            trail.push(format!("{step}:Ctl({name})"));
                            inv_check(&app, &format!("seed {seed} after [{}]", trail.join(",")));
                            continue 'step;
                        }
                        35 => ("ToggleFloat", Action::ToggleFloat),
                        36 | 37 => ("ToggleZoom", Action::ToggleZoom),
                        // C40: the mark is the one pane-carrying verb whose
                        // source tab is arbitrary rather than the active
                        // one, so it reaches tree/map states the walk-based
                        // move cannot — worth the two picks.
                        38 => ("MarkPane", Action::MarkPane),
                        39 => ("PullPane", Action::PullPane),
                        _ => ("FocusAlternate", Action::FocusAlternate),
                    };
                    break a;
                };
                trail.push(format!("{step}:{name}"));
                *coverage.entry(name).or_default() += 1;
                ops_total += 1;
                app.apply(action);
                if app.zoomed() {
                    zoom_seen += 1;
                }
                if app.float.as_ref().is_some_and(|f| f.shown) {
                    float_seen += 1;
                    if app.zoomed() {
                        float_and_zoom += 1;
                    }
                }
                if app.ws.tabs.len() > 1 {
                    multi_tab += 1;
                }
                if app.ws.tabs.iter().any(|t| inv_has_stack(&t.layout)) {
                    stacks_seen += 1;
                }
                inv_check(&app, &format!("seed {seed} after [{}]", trail.join(",")));
                assert!(!app.quit, "seed {seed}: quit unexpectedly at [{}]", trail.join(","));
            }
        }
        let mut cov: Vec<_> = coverage.iter().collect();
        cov.sort();
        println!("ops={ops_total} coverage={cov:?}");
        println!(
            "states: stack-present={stacks_seen} zoomed={zoom_seen} float-shown={float_seen} float+zoom={float_and_zoom} multi-tab={multi_tab}"
        );
        assert!(stacks_seen > 1000, "generator never reached stacks");
        assert!(zoom_seen > 1000, "generator never reached zoom");
        assert!(float_seen > 1000, "generator never reached the float");
        assert!(float_and_zoom > 100, "generator never crossed the float with zoom");
        assert!(multi_tab > 1000, "generator never reached multiple tabs");
    }

    /// One principal must not be able to deny `wait` to the whole fleet.
    ///
    /// A parked waiter is freed only by firing or timing out — nothing
    /// notices the client hung up — so 16 fire-and-forget waits held every
    /// global slot for up to 24 hours while holding no connection and no
    /// process. A pane can send them with its own `$ROOST_TOKEN`, which is
    /// squarely outside its authority.
    #[test]
    fn one_principal_cannot_park_every_wait_slot() {
        let (mut app, _) = mk_app(shell_ws());
        let pane = app.focused;
        let mut keep = Vec::new();

        // One pane spams waits with its own token's authority.
        let mut parked = 0;
        for _ in 0..MAX_WAITS * 2 {
            let (tx, rx) = mpsc::channel();
            if app
                .register_waiter(Actor::Pane(pane), vec![pane], "exited", Some(60_000), tx)
                .is_ok_and(|why| why == "parked")
            {
                parked += 1;
            }
            keep.push(rx);
        }
        assert!(
            parked <= MAX_WAITS_PER_ACTOR,
            "one principal parked {parked} waits — it can hold every global slot",
        );

        // ...and the fleet can still use the verb.
        let (tx, _rx) = mpsc::channel();
        let got = app.register_waiter(Actor::Fleet, vec![pane], "exited", Some(60_000), tx);
        assert_eq!(got.as_deref(), Ok("parked"), "the verb is denied to everyone else");
    }

    /// A `wait` naming more panes than any fleet has is a load generator:
    /// `poll_waiters` walks every id of every waiter on the **UI thread**,
    /// every event-loop iteration, for the wait's whole timeout.
    #[test]
    fn a_wait_cannot_name_an_unbounded_pane_list() {
        let (mut app, _) = mk_app(shell_ws());
        let pane = app.focused;
        let (tx, _rx) = mpsc::channel();
        let huge: Vec<PaneId> = vec![pane; 20_000];
        assert!(
            app.register_waiter(Actor::Fleet, huge, "exited", Some(60_000), tx).is_err(),
            "a 20,000-id wait was accepted; it is re-walked on the UI thread every frame",
        );
    }

    /// Detection must give up eventually.
    ///
    /// `pending_detect` is only ever removed from on success
    /// (`set_session`) or when the pane disappears. A pane whose agent
    /// never writes a session file roost can attribute — it exited early,
    /// it is an adapter with no session root reachable, or the scan keeps
    /// declining as ambiguous — stays pending **forever**, and every 2s
    /// tick runs `session_files_since` over the whole session root for it.
    ///
    /// No adapter overrides `detect_session`, so that is a full recursive
    /// walk stat-ing every file under e.g. `~/.pi/agent/sessions` — every
    /// project, all history — twice a minute, for the life of the process.
    #[test]
    fn detection_stops_retrying_a_pane_that_will_never_resolve() {
        let dir = std::env::temp_dir().join(format!("roost-giveup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut panes = HashMap::new();
        panes.insert(
            1,
            PaneSpec {
                adapter: "detect".into(),
                cwd: dir.clone(),
                session: None,
                title: None,
                spawned_by: None,
                note: None,
                noted_at: None,
            },
        );
        let ws = Workspace {
            version: 1,
            active_tab: 0,
            tabs: vec![Tab { name: "main".into(), layout: LayoutNode::Pane(1), panes }],
        };
        let mut registry = agents::registry();
        registry.insert("detect", Box::new(DetectAdapter));
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut app = App::<FakePane>::new(
            ws,
            registry,
            Box::new(MemStore::default()),
            tx,
            Size::new(100, 30),
            (0, 0),
            None,
            TokenTable::new().unwrap(),
        )
        .unwrap();

        // Pending since well past any plausible agent startup, and the
        // session root is empty, so no scan will ever succeed.
        app.pending_detect.clear();
        app.pending_detect.insert(1, SystemTime::now() - Duration::from_secs(3600));
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();

        assert!(
            !app.pending_detect.contains_key(&1),
            "still pending an hour on — roost will rescan the whole session root \
             every 2 seconds for the rest of the process's life",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pane id parked on the undo stack must not be handed out again.
    ///
    /// `Workspace::next_pane_id` takes the max over **live tabs only**, and
    /// `alloc_pane_id` additionally guards the float — but a tab sitting on
    /// the undo stack still owns its pane ids, and nothing accounts for
    /// them. Close a tab, open enough panes to reach its ids, then `Alt+u`:
    /// the restored tab's panes collide with live ones.
    ///
    /// That is not cosmetic. `App::runtimes` is keyed by `PaneId` **across
    /// all tabs**, as are `tokens`, `dead`, `pending_input` and `raw` — so
    /// two panes sharing an id share one PTY, one auth token and one
    /// status. Keystrokes meant for one reach the other.
    #[test]
    fn a_pane_id_parked_on_the_undo_stack_is_never_reused() {
        let (mut app, _) = mk_app(shell_ws());
        // A second tab, so closing it parks a whole Tab on the undo stack.
        app.apply(Action::NewTab);
        let parked: Vec<PaneId> = app.ws.tabs[app.ws.active_tab].panes.keys().copied().collect();
        assert!(!parked.is_empty(), "the new tab has a pane");

        // Close it: its last pane going means the tab is parked whole.
        for id in &parked {
            app.close_pane_id(*id);
        }
        assert_eq!(app.ws.tabs.len(), 1, "the tab is gone");

        // Now open panes until we would reach the parked ids.
        for _ in 0..parked.len() + 2 {
            app.apply(Action::NewPane);
        }
        let live: Vec<PaneId> = app.ws.tabs.iter().flat_map(|t| t.panes.keys().copied()).collect();
        for id in &parked {
            assert!(
                !live.contains(id),
                "pane id {id} is parked on the undo stack and was handed out again — \
                 restoring that tab would put two panes on one runtime. live={live:?}",
            );
        }

        // And the consequence, so the claim above is demonstrated rather
        // than asserted: reopening puts the tab back with its original ids.
        app.apply(Action::Undo);
        let mut seen: HashMap<PaneId, usize> = HashMap::new();
        for t in &app.ws.tabs {
            for id in t.panes.keys() {
                *seen.entry(*id).or_insert(0) += 1;
            }
        }
        let dupes: Vec<PaneId> = seen.iter().filter(|(_, n)| **n > 1).map(|(id, _)| *id).collect();
        assert!(
            dupes.is_empty(),
            "after reopening, pane id(s) {dupes:?} exist in two tabs at once — \
             `runtimes`, `tokens` and `dead` are all keyed by PaneId across tabs, \
             so those panes share one PTY and one auth token",
        );
    }

    /// The promotion floor must never be *loosened* by a later promotion.
    ///
    /// `pending_detect.entry(id).or_insert(floor)` keeps whatever bound is
    /// already there. A pane that promotes, fails to find a file (so stays
    /// pending), demotes when the agent exits, and promotes again hours
    /// later would keep the **first** floor — by then old enough to reach
    /// conversations from earlier in the session. The window must only ever
    /// tighten.
    #[test]
    fn a_second_promotion_tightens_the_detection_floor_it_never_loosens() {
        let dir = std::env::temp_dir().join(format!("roost-refloor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut panes = HashMap::new();
        panes.insert(
            1,
            PaneSpec {
                adapter: "shell".into(),
                cwd: dir.clone(),
                session: None,
                title: None,
                spawned_by: None,
                note: None,
                noted_at: None,
            },
        );
        let ws = Workspace {
            version: 1,
            active_tab: 0,
            tabs: vec![Tab { name: "main".into(), layout: LayoutNode::Pane(1), panes }],
        };
        let mut registry = agents::registry();
        registry.insert("detect", Box::new(DetectAdapter));
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut app = App::<FakePane>::new(
            ws,
            registry,
            Box::new(MemStore::default()),
            tx,
            Size::new(100, 30),
            (0, 0),
            None,
            TokenTable::new().unwrap(),
        )
        .unwrap();

        let tick = |app: &mut App<FakePane>, agent: Option<&str>| {
            app.runtimes.get_mut(&1).unwrap().observation =
                Some(crate::ports::Observation { cwd: None, agent: agent.map(String::from) });
            app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
            app.tick();
        };

        // Shell → agent: promoted, nothing on disk, so it stays pending.
        tick(&mut app, None);
        tick(&mut app, Some("detect"));
        let first_floor = *app.pending_detect.get(&1).expect("pending after promotion");

        // The agent exits (demote), the pane sits as a shell, then the user
        // launches it again much later.
        tick(&mut app, None);
        app.last_shell_seen.insert(1, SystemTime::now() + Duration::from_secs(3600));
        tick(&mut app, Some("detect"));

        let second_floor = *app.pending_detect.get(&1).expect("pending after re-promotion");
        assert!(
            second_floor > first_floor,
            "the second promotion kept the first floor ({first_floor:?}), so its window \
             still reaches back to conversations from before the agent was relaunched",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same guarantee when the pane **changes directory first** — open
    /// a pane, `cd` into a project, then launch the agent.
    ///
    /// This is the sharper version of the reported workflow: the directory
    /// you move into is one you have worked in before, so its session root
    /// is full of previous conversations. `observe_panes` follows the live
    /// cwd, so the scan is correctly scoped to the *new* project — which is
    /// exactly why an unbounded window would hand the pane that project's
    /// most recent old conversation.
    #[test]
    fn a_pane_that_cds_before_launching_still_gets_its_own_session() {
        let old_dir = std::env::temp_dir().join(format!("roost-cd-a-{}", std::process::id()));
        let new_dir = std::env::temp_dir().join(format!("roost-cd-b-{}", std::process::id()));
        for d in [&old_dir, &new_dir] {
            let _ = std::fs::remove_dir_all(d);
            std::fs::create_dir_all(d).unwrap();
        }
        // A conversation from last week in the project we are about to
        // `cd` into.
        let stale = new_dir.join("lastweek.jsonl");
        std::fs::write(&stale, "").unwrap();
        std::fs::File::open(&stale)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(7 * 24 * 3600))
            .unwrap();

        let mut panes = HashMap::new();
        panes.insert(
            1,
            PaneSpec {
                adapter: "shell".into(),
                cwd: old_dir.clone(),
                session: None,
                title: None,
                spawned_by: None,
                note: None,
                noted_at: None,
            },
        );
        let ws = Workspace {
            version: 1,
            active_tab: 0,
            tabs: vec![Tab { name: "main".into(), layout: LayoutNode::Pane(1), panes }],
        };
        let mut registry = agents::registry();
        registry.insert("detect", Box::new(DetectAdapter));
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut app = App::<FakePane>::new(
            ws,
            registry,
            Box::new(MemStore::default()),
            tx,
            Size::new(100, 30),
            (0, 0),
            None,
            TokenTable::new().unwrap(),
        )
        .unwrap();

        // Tick 1: still a shell, but now in the new directory.
        app.runtimes.get_mut(&1).unwrap().observation =
            Some(crate::ports::Observation { cwd: Some(new_dir.clone()), agent: None });
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();
        assert_eq!(app.find_spec(1).unwrap().cwd, new_dir, "the pane followed the cd");

        // Tick 2: the agent is up, its session file not yet written.
        app.runtimes.get_mut(&1).unwrap().observation = Some(crate::ports::Observation {
            cwd: Some(new_dir.clone()),
            agent: Some("detect".into()),
        });
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();
        assert_eq!(
            app.find_spec(1).unwrap().session,
            None,
            "claimed last week's conversation from the directory it cd'd into",
        );

        // Tick 3: the agent writes its file, and the pane claims that.
        std::fs::write(new_dir.join("mine.jsonl"), "").unwrap();
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();
        assert_eq!(app.find_spec(1).unwrap().session.as_deref(), Some("mine"));

        for d in [&old_dir, &new_dir] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    /// Adapter with a **global** session root that claims every file for
    /// every cwd — codex's shape (`~/.codex/sessions`, bucketed by date,
    /// `owns_session_file` true by design because the path carries no cwd
    /// signal at all).
    struct UnscopedAdapter(PathBuf);
    impl crate::agents::AgentAdapter for UnscopedAdapter {
        fn id(&self) -> &'static str {
            "unscoped"
        }
        fn launch(&self, cwd: &std::path::Path) -> crate::agents::CommandSpec {
            crate::agents::CommandSpec::new("true", cwd)
        }
        fn resume(&self, cwd: &std::path::Path, session: &str) -> crate::agents::CommandSpec {
            crate::agents::CommandSpec::new("true", cwd).arg(session)
        }
        fn resume_flag(&self) -> &'static str {
            "--resume"
        }
        fn session_root(&self, _cwd: &std::path::Path) -> Option<PathBuf> {
            Some(self.0.clone()) // global: the same root for every project
        }
    }

    /// Two panes in **different projects** must never be handed each
    /// other's conversation.
    ///
    /// An adapter whose session root is global (codex) gives the scan no
    /// cwd signal, so when two panes are mid-detection at once the only
    /// thing separating them is arrival order — and a session file's mtime
    /// says nothing about which project it belongs to. The taken-set stops
    /// one id reaching two panes; it does nothing about the *wrong* id
    /// reaching a pane.
    ///
    /// Guiding principle for this tool: **a wrong session is far worse than
    /// no session.** Losing a resume pointer costs the user `--continue`;
    /// attaching a pane to another project's conversation corrupts work.
    #[test]
    fn an_unscoped_adapter_never_hands_a_pane_another_projects_session() {
        let root = std::env::temp_dir().join(format!("roost-unscoped-{}", std::process::id()));
        let a_dir = root.join("proj-a");
        let b_dir = root.join("proj-b");
        let _ = std::fs::remove_dir_all(&root);
        for d in [&root, &a_dir, &b_dir] {
            std::fs::create_dir_all(d).unwrap();
        }
        // Two sessions land in the one global root, moments apart. Nothing
        // in either path says which project it came from.
        let base = SystemTime::now();
        for (name, offset) in [("first", 10u64), ("second", 20)] {
            let f = root.join(format!("{name}.jsonl"));
            std::fs::write(&f, "").unwrap();
            std::fs::File::open(&f)
                .unwrap()
                .set_modified(base + Duration::from_millis(offset))
                .unwrap();
        }

        let mut panes = HashMap::new();
        let seeds: [(PaneId, &PathBuf); 2] = [(1, &a_dir), (2, &b_dir)];
        for (id, dir) in seeds {
            panes.insert(
                id,
                PaneSpec {
                    adapter: "unscoped".into(),
                    cwd: dir.clone(),
                    session: None,
                    title: None,
                    spawned_by: None,
                    note: None,
                    noted_at: None,
                },
            );
        }
        let layout = LayoutNode::Split {
            dir: SplitDir::Vertical,
            ratios: vec![0.5, 0.5],
            children: vec![LayoutNode::Pane(1), LayoutNode::Pane(2)],
        };
        let ws = Workspace {
            version: 1,
            active_tab: 0,
            tabs: vec![Tab { name: "main".into(), layout, panes }],
        };
        let mut registry = agents::registry();
        registry.insert("unscoped", Box::new(UnscopedAdapter(root.clone())));
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut app = App::<FakePane>::new(
            ws,
            registry,
            Box::new(MemStore::default()),
            tx,
            Size::new(100, 30),
            (0, 0),
            None,
            TokenTable::new().unwrap(),
        )
        .unwrap();

        app.pending_detect.clear();
        app.pending_detect.insert(1, base);
        app.pending_detect.insert(2, base);
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();

        let a = app.find_spec(1).unwrap().session.clone();
        let b = app.find_spec(2).unwrap().session.clone();
        assert!(
            a.is_none() && b.is_none(),
            "two panes in different projects were each handed a session from a \
             global root that cannot say which project it belongs to: \
             pane1(proj-a)={a:?} pane2(proj-b)={b:?}",
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tick_lets_each_concurrently_launched_pane_claim_its_own_session_file() {
        // Two panes launched into the same cwd around the same time share
        // one session root; `tick()` must process them newest-spawned-first
        // (not HashMap/arbitrary order), or whichever pane got processed
        // first could steal the *other* pane's newer, not-yet-claimed
        // session file, leaving that other pane with none at all — it would
        // then relaunch fresh instead of resuming on the next run.
        let dir = std::env::temp_dir().join(format!("roost-detect-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let base = SystemTime::now();
        let file_a = dir.join("a.jsonl");
        let file_b = dir.join("b.jsonl");
        std::fs::write(&file_a, "").unwrap();
        std::fs::write(&file_b, "").unwrap();
        std::fs::File::open(&file_a)
            .unwrap()
            .set_modified(base + Duration::from_millis(10))
            .unwrap();
        std::fs::File::open(&file_b)
            .unwrap()
            .set_modified(base + Duration::from_millis(20))
            .unwrap();

        let mut panes = HashMap::new();
        panes.insert(
            1,
            PaneSpec {
                adapter: "detect".into(),
                cwd: dir.clone(),
                session: None,
                title: None,
                spawned_by: None,
                note: None,
                noted_at: None,
            },
        );
        panes.insert(
            2,
            PaneSpec {
                adapter: "detect".into(),
                cwd: dir.clone(),
                session: None,
                title: None,
                spawned_by: None,
                note: None,
                noted_at: None,
            },
        );
        let layout = LayoutNode::Split {
            dir: SplitDir::Vertical,
            ratios: vec![0.5, 0.5],
            children: vec![LayoutNode::Pane(1), LayoutNode::Pane(2)],
        };
        let ws = Workspace {
            version: 1,
            active_tab: 0,
            tabs: vec![Tab { name: "main".into(), layout, panes }],
        };

        let mut registry = agents::registry();
        registry.insert("detect", Box::new(DetectAdapter));
        let store = MemStore::default();
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut app = App::<FakePane>::new(
            ws,
            registry,
            Box::new(store),
            tx,
            Size::new(100, 30),
            (0, 0),
            None,
            TokenTable::new().unwrap(),
        )
        .unwrap();

        // Pane 1 "spawned" before either file existed (widest window); pane 2
        // "spawned" after file_a but before file_b — an ordering that
        // starves whichever pane is processed second if order isn't honored.
        app.pending_detect.clear();
        app.pending_detect.insert(1, base);
        app.pending_detect.insert(2, base + Duration::from_millis(15));
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);

        app.tick();

        assert_eq!(app.find_spec(1).unwrap().session.as_deref(), Some("a"));
        assert_eq!(app.find_spec(2).unwrap().session.as_deref(), Some("b"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Characterisation: pane 1 already owns "b" (the newer of two session
    /// files); pane 2 is mid-detection in the same cwd and must not steal
    /// it — even though "b" is the newest candidate — falling through to
    /// the older, still-unclaimed file instead. A session id must never be
    /// handed to two panes at once.
    #[test]
    fn tick_skips_a_session_id_already_claimed_by_another_pane() {
        let dir = std::env::temp_dir().join(format!("roost-detect-taken-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let base = SystemTime::now();
        let file_a = dir.join("a.jsonl");
        let file_b = dir.join("b.jsonl");
        std::fs::write(&file_a, "").unwrap();
        std::fs::write(&file_b, "").unwrap();
        std::fs::File::open(&file_a)
            .unwrap()
            .set_modified(base + Duration::from_millis(10))
            .unwrap();
        std::fs::File::open(&file_b)
            .unwrap()
            .set_modified(base + Duration::from_millis(20))
            .unwrap();

        let mut panes = HashMap::new();
        panes.insert(
            1,
            PaneSpec {
                adapter: "detect".into(),
                cwd: dir.clone(),
                session: Some("b".into()), // already claimed before this tick
                title: None,
                spawned_by: None,
                note: None,
                noted_at: None,
            },
        );
        panes.insert(
            2,
            PaneSpec {
                adapter: "detect".into(),
                cwd: dir.clone(),
                session: None,
                title: None,
                spawned_by: None,
                note: None,
                noted_at: None,
            },
        );
        let layout = LayoutNode::Split {
            dir: SplitDir::Vertical,
            ratios: vec![0.5, 0.5],
            children: vec![LayoutNode::Pane(1), LayoutNode::Pane(2)],
        };
        let ws = Workspace {
            version: 1,
            active_tab: 0,
            tabs: vec![Tab { name: "main".into(), layout, panes }],
        };

        let mut registry = agents::registry();
        registry.insert("detect", Box::new(DetectAdapter));
        let store = MemStore::default();
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut app = App::<FakePane>::new(
            ws,
            registry,
            Box::new(store),
            tx,
            Size::new(100, 30),
            (0, 0),
            None,
            TokenTable::new().unwrap(),
        )
        .unwrap();

        // Pane 1 resumed cleanly at spawn (its "b" file exists), so only
        // pane 2 is left pending; `since` predates both files.
        assert!(!app.pending_detect.contains_key(&1));
        app.pending_detect.insert(2, base);
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);

        app.tick();

        assert_eq!(app.find_spec(2).unwrap().session.as_deref(), Some("a"));
        assert!(!app.pending_detect.contains_key(&2));

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
        // D5: a plain shell pane spawns with the title channel disarmed.
        assert_eq!(app.runtimes[&1].title_signal, Some(false));
        // pane 1: user cd'd to /work/proj and typed `pi`
        app.runtimes.get_mut(&1).unwrap().observation =
            Some(Observation { cwd: Some(PathBuf::from("/work/proj")), agent: Some("pi".into()) });
        app.observe_panes();
        let spec = app.find_spec(1).unwrap();
        assert_eq!(spec.adapter, "pi");
        assert_eq!(spec.cwd, PathBuf::from("/work/proj"));
        // D5: the promotion arms the title channel for the running agent.
        assert_eq!(app.runtimes[&1].title_signal, Some(true));
        let saved = store.0.lock().unwrap().clone().unwrap();
        assert_eq!(saved.tabs[0].panes[&1].adapter, "pi"); // persisted
        assert!(app.pending_detect.contains_key(&1)); // queued for session detection
    }

    #[test]
    fn observe_demotes_to_shell_when_agent_exits() {
        let mut ws = shell_ws();
        ws.tabs[0].panes.get_mut(&1).unwrap().adapter = "pi".into();
        let (mut app, _) = mk_app(ws);
        // D5: an agent-adapter pane's title channel is armed at spawn.
        assert_eq!(app.runtimes[&1].title_signal, Some(true));
        // pi exited; the pane is a plain shell again
        app.runtimes.get_mut(&1).unwrap().observation =
            Some(Observation { cwd: None, agent: None });
        app.observe_panes();
        assert_eq!(app.find_spec(1).unwrap().adapter, "shell");
        // D5: the demotion disarms the title channel — an exited agent's
        // leftover `✳` must not veto Working for later shell commands.
        assert_eq!(app.runtimes[&1].title_signal, Some(false));
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
        // M6: `initial_input` must not be written the instant the pane
        // spawns — the agent's stdin reader isn't necessarily up yet, which
        // would silently drop it. It must sit buffered until the pane's
        // first output (a reliable "it's alive and reading" signal) and be
        // flushed exactly once then.
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
    fn alt_hint_gates_on_evidence_within_the_window_since_it_arrived() {
        // F1: evidence, not "a key arrived" — and the window is keyed to
        // the evidence's own timestamp, not launch: is_alt_swallow_char's
        // evidence is real but not unambiguous (every character is also a
        // directly-typed letter on some non-US layout — see its own doc),
        // so it must not latch for the whole session. Bounded to
        // ALT_HINT_WINDOW since the *most recent* match instead.
        assert!(wants_alt_hint(false, Some(Duration::from_secs(1))));
        assert!(wants_alt_hint(false, Some(ALT_HINT_WINDOW - Duration::from_millis(1))));
        assert!(!wants_alt_hint(false, Some(ALT_HINT_WINDOW)), "at the boundary, already expired");
        assert!(
            !wants_alt_hint(false, Some(ALT_HINT_WINDOW + Duration::from_secs(60))),
            "long expired"
        );
        // One Alt key ever ends it, and it never fires with no evidence at
        // all: an untouched roost — or one that's only seen ordinary keys —
        // has nothing to warn about.
        assert!(!wants_alt_hint(true, Some(Duration::from_secs(1))));
        assert!(!wants_alt_hint(false, None));
    }

    #[test]
    fn is_alt_swallow_char_matches_the_examples_the_audit_named() {
        assert!(is_alt_swallow_char('˜')); // Option+n with Option-as-Meta off
        assert!(is_alt_swallow_char('∑')); // Option+w
                                           // Plain letters — what a healthy terminal sends when Alt DOES get
                                           // through (roost sees the ALT modifier bit instead) and what an
                                           // unrelated keystroke looks like either way — are not evidence.
        for c in 'a'..='z' {
            assert!(!is_alt_swallow_char(c), "{c:?} is not swallowed-Alt evidence");
        }
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

    /// The trigger end-to-end on a real `App`: silent until a swallowed-Alt
    /// key arrives, unmoved by an ordinary keystroke, gone for good once a
    /// real Alt chord gets through.
    #[test]
    fn the_alt_warning_appears_on_swallowed_alt_evidence_and_dies_on_the_first_real_alt() {
        let (mut app, _) = mk_app(shell_ws());
        assert!(!app.show_alt_hint(), "nothing typed yet — nothing to warn about");
        app.note_key_seen(key('n')); // plain 'n' — Alt was never involved
        assert!(!app.show_alt_hint(), "an ordinary key is not evidence");
        app.note_key_seen(key('˜')); // Option+n, swallowed — this is evidence
        assert!(app.show_alt_hint());
        app.note_alt_seen();
        assert!(!app.show_alt_hint(), "Alt got through — the warning is wrong now");
        app.note_key_seen(key('˜'));
        assert!(!app.show_alt_hint(), "and it stays gone for the session");
    }

    /// F1 hard requirement 1: a correctly configured terminal must never
    /// see the warning just because the user typed something — typing a
    /// shell prompt is the single most likely first action on a fresh
    /// launch.
    #[test]
    fn healthy_terminal_typing_a_shell_prompt_never_fires_the_alt_hint() {
        let (mut app, _) = mk_app(shell_ws());
        for c in "ls -la && git status\r".chars() {
            app.note_key_seen(key(c));
        }
        assert!(!app.show_alt_hint(), "ordinary typing must never look like a swallowed Alt chord");
    }

    /// F1 hard requirement 2: a read-first user who studies the screen for
    /// 40s before ever touching Alt must still get the warning the moment
    /// their first (swallowed) Alt press arrives — the window
    /// (`ALT_HINT_WINDOW`, SG1) is keyed to that press's own timestamp, not
    /// to launch, so `started` plays no part in this decision at all,
    /// however long it's been.
    #[test]
    fn read_first_user_still_gets_the_hint_however_late_the_first_alt_press_is() {
        let (mut app, _) = mk_app(shell_ws());
        app.started -= Duration::from_secs(40);
        assert!(!app.show_alt_hint(), "no evidence yet, however long it's been");
        app.note_key_seen(key('˜')); // their first Alt press: Option+n, swallowed
        assert!(app.show_alt_hint());
    }

    /// Design audit SG1's own repro: every `is_alt_swallow_char` character
    /// is also a directly-typed letter on some non-US macOS layout (`ç` on
    /// Portuguese/French/Turkish, among others) — a user typing their own
    /// language must not get a bar that owns the hint row for the rest of
    /// the session. It still fires (that's the acknowledged tradeoff,
    /// `is_alt_swallow_char`'s own doc), but it must self-clear.
    #[test]
    fn a_non_us_layouts_own_letter_self_clears_instead_of_latching_for_the_session() {
        let (mut app, _) = mk_app(shell_ws());
        app.note_key_seen(key('ç')); // e.g. Portuguese "ação", typed directly, no Alt involved
        assert!(app.show_alt_hint(), "the char alone still reads as evidence — the known tradeoff");
        app.alt_swallow_at = app.alt_swallow_at.map(|t| t - ALT_HINT_WINDOW);
        assert!(
            !app.show_alt_hint(),
            "must self-clear, not own the row for the rest of the session"
        );
    }

    /// The other half of SG1's fix: a still-broken Alt layer must keep being
    /// reported, not just once. Each fresh match resets the window rather
    /// than merely extending the first one's original deadline.
    #[test]
    fn fresh_evidence_re_arms_the_window() {
        let (mut app, _) = mk_app(shell_ws());
        app.note_key_seen(key('˜'));
        app.alt_swallow_at = app.alt_swallow_at.map(|t| t - ALT_HINT_WINDOW);
        assert!(!app.show_alt_hint(), "sanity: the first match alone has expired");
        app.note_key_seen(key('∑')); // tried Alt again — still broken
        assert!(app.show_alt_hint(), "a fresh match re-arms the window");
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
        assert_eq!(
            abbreviate_home(&PathBuf::from("/home/navvy/work"), Some(&home)),
            "/home/navvy/work"
        );
    }

    /// P0-3: the exact bug — a chained error's default `Display` drops
    /// everything past the outermost `.context()`. `dead_reason` must not:
    /// the cause has to actually reach the string the dead-pane bar (and
    /// now the feed/status `error` field) renders.
    #[test]
    fn dead_reason_keeps_the_real_cause_reachable() {
        let err = anyhow::anyhow!("No such file or directory (os error 2)").context("spawning pi");
        let reason = dead_reason(&err);
        assert!(reason.contains("spawning pi"), "{reason:?}");
        assert!(reason.contains("No such file or directory"), "{reason:?}");
        assert!(!reason.contains('\n'), "must stay on one line: {reason:?}");
        // `{}` alone is exactly the outer context — the bug this replaces.
        assert_eq!(reason, format!("{err:#}"));
        assert_ne!(reason, format!("{err}"));
    }

    /// P0-3: a pathological chain is capped, not left to blow past the
    /// dead-pane bar — truncated on a char boundary so a multi-byte glyph
    /// is never split.
    #[test]
    fn dead_reason_caps_a_long_chain_on_a_char_boundary() {
        let err =
            anyhow::anyhow!("{}", "x".repeat(DEAD_REASON_CAP * 2)).context("spawning wide 日");
        let reason = dead_reason(&err);
        assert_eq!(reason.chars().count(), DEAD_REASON_CAP);
        assert!(reason.ends_with('…'));
        assert!(std::str::from_utf8(reason.as_bytes()).is_ok(), "never split a glyph");
    }

    #[test]
    fn validate_spawn_cwd_accepts_a_real_directory() {
        assert!(validate_spawn_cwd(&std::env::temp_dir()).is_ok());
    }

    /// QA-2: the confirmed repro — `roost spawn --cwd <nonexistent>` must
    /// fail loudly, naming the path, with the real OS reason rather than a
    /// made-up one.
    #[test]
    fn validate_spawn_cwd_names_the_path_when_it_does_not_exist() {
        let missing =
            std::env::temp_dir().join(format!("roost-cwd-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&missing);
        let err = validate_spawn_cwd(&missing).unwrap_err();
        assert!(err.contains(&missing.display().to_string()), "{err:?}");
        assert!(err.to_lowercase().contains("no such file"), "{err:?}");
    }

    /// QA-2's deliberate design call: a path that resolves but names a file,
    /// not a directory, is a DIFFERENT, more specific failure than "doesn't
    /// exist" — worth its own message instead of a raw (and here, absent)
    /// OS error.
    #[test]
    fn validate_spawn_cwd_distinguishes_a_file_from_a_missing_directory() {
        let dir = std::env::temp_dir().join(format!("roost-cwd-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("not-a-directory");
        std::fs::write(&file, "x").unwrap();

        let err = validate_spawn_cwd(&file).unwrap_err();

        let _ = std::fs::remove_dir_all(&dir);
        assert!(err.contains("is not a directory"), "{err:?}");
        assert!(err.contains(&file.display().to_string()), "{err:?}");
    }

    /// P2-13: the PATH-scan logic itself, against a synthetic directory
    /// instead of whatever happens to be installed on the machine running
    /// this — deterministic on any host.
    #[test]
    fn resolves_on_path_finds_an_executable_and_rejects_a_non_executable_or_missing_name() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("roost-path-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let exe = dir.join("real-program");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();

        let not_exe = dir.join("not-executable");
        std::fs::write(&not_exe, "just text").unwrap();
        std::fs::set_permissions(&not_exe, std::fs::Permissions::from_mode(0o644)).unwrap();

        let dirs = [dir.clone()];
        assert!(resolves_on_path("real-program", dirs.iter()), "an executable on PATH is found");
        assert!(
            !resolves_on_path("not-executable", dirs.iter()),
            "present but not executable ⇒ not found, same as absent"
        );
        assert!(!resolves_on_path("does-not-exist-at-all", dirs.iter()), "absent ⇒ not found");
        // An absolute/relative path (contains '/') is checked directly, the
        // way ShellAdapter's $SHELL-derived program always is — never
        // scanned across `dirs`, so an empty iterator still finds it.
        assert!(resolves_on_path(exe.to_str().unwrap(), std::iter::empty::<PathBuf>()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn adapter_installed_checks_the_real_path_and_treats_an_unknown_id_as_installed() {
        // ShellAdapter falls back to /bin/bash when $SHELL is unset; either
        // way it's an absolute path, so this is the one adapter guaranteed
        // not to depend on which agent CLIs happen to be on the machine
        // running this test.
        assert!(adapter_installed("shell", &agents::registry()));
        // Nothing to warn about for an id the registry doesn't even have —
        // `agents::picker_ids()` IS the registry, so this never happens through
        // the picker, but the function must still answer something sane.
        assert!(adapter_installed("not-a-real-adapter-id", &agents::registry()));
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
            TokenTable::new().unwrap(),
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

    /// [ux P1-5] With **no** ◆ anywhere, the ring falls through to every
    /// `Waiting` pane — the fallback that makes an uninstrumented agent's
    /// finished turn reachable by `Alt+a` at all, not just extension panes'
    /// explicit `NeedsInput`.
    #[test]
    fn attention_ring_falls_through_to_waiting_when_nothing_needs_input() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1|2, focus=2
        app.find_spec_mut(1).unwrap().adapter = "pi".into();
        app.runtimes.get_mut(&1).unwrap().set_extension_status(AgentStatus::Waiting);
        assert_eq!(app.attention_ring(), vec![1]);
        app.apply(Action::JumpAttention);
        assert_eq!(app.focused, 1, "Alt+a reaches a finished, uninstrumented turn");
    }

    /// A real ◆ anywhere in the fleet wins outright — the fallback only ever
    /// runs once the ◆ pass comes back empty, so an instrumented fleet's ring
    /// behaves exactly as it did before this amendment: no ◆ is ever skipped
    /// in favor of a ○.
    #[test]
    fn attention_ring_never_skips_a_real_needs_input_for_the_waiting_fallback() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::NewPane); // panes 1, 2, 3
        app.find_spec_mut(1).unwrap().adapter = "pi".into();
        app.find_spec_mut(2).unwrap().adapter = "pi".into();
        app.runtimes.get_mut(&1).unwrap().set_extension_status(AgentStatus::Waiting);
        app.runtimes.get_mut(&2).unwrap().set_extension_status(AgentStatus::Waiting);
        app.runtimes.get_mut(&3).unwrap().set_extension_status(AgentStatus::NeedsInput);
        assert_eq!(app.attention_ring(), vec![3], "the one real ◆ wins outright, no ○ mixed in");
    }

    /// F2: the hint bar's right segment must match what `Alt+a` actually
    /// does in every case — nothing, a real ◆, or the ○ fallback — not just
    /// when a ◆ exists.
    #[test]
    fn attention_segment_matches_the_ring_in_every_case() {
        let (mut app, _) = mk_app(shell_ws());
        assert_eq!(app.attention_segment(), None, "a fresh shell pane pulls neither pass");

        app.apply(Action::NewPane); // panes 1|2, focus=2
        app.find_spec_mut(1).unwrap().adapter = "pi".into();
        app.runtimes.get_mut(&1).unwrap().set_extension_status(AgentStatus::Waiting);
        assert_eq!(app.attention_segment(), Some((1, false)), "○ fallback: real, but not a ◆");

        app.runtimes.get_mut(&1).unwrap().set_extension_status(AgentStatus::NeedsInput);
        assert_eq!(app.attention_segment(), Some((1, true)), "a real ◆ takes over");
    }

    /// Interaction (items 1 + 4): a quiet shell's `Waiting` reads `Idle`
    /// (P2-10) through `display_status`, so it must not pull the fallback
    /// ring — only a pane whose `Waiting` means something (a real agent)
    /// does. The case that matters: a fleet mixing instrumented/uninstrumented
    /// agents with plain shells, and only the agent counts.
    #[test]
    fn attention_ring_waiting_fallback_excludes_quiet_shells() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1 (shell), 2 (shell, promoted below)
        app.find_spec_mut(2).unwrap().adapter = "pi".into();
        app.runtimes.get_mut(&1).unwrap().set_extension_status(AgentStatus::Waiting); // shell: no turn
        app.runtimes.get_mut(&2).unwrap().set_extension_status(AgentStatus::Waiting); // agent: your turn
        assert_eq!(app.attention_ring(), vec![2], "the shell's Waiting never pulls the ring");

        // A fleet of shells only, none of them ever needing a jump.
        app.find_spec_mut(2).unwrap().adapter = "shell".into();
        assert!(app.attention_ring().is_empty(), "an all-shell fleet has nothing to jump to");
        app.apply(Action::JumpAttention);
        assert_eq!(app.flash(), Some("nothing needs you"));
    }

    /// F3: unlike a real ◆, which clears itself the moment you act on it, a
    /// quiet `Waiting` pane's status never changes just because you looked
    /// at it — without retiring a visited pane, three finished agents would
    /// make `Alt+a` an endless 1→2→3→1→2→3… round-robin and "nothing else
    /// needs you" would never fire (only ever with exactly one pane
    /// waiting). Each visit must permanently retire that pane from the
    /// rotation, so the ring shrinks to empty exactly once every pane has
    /// been seen.
    #[test]
    fn jump_attention_waiting_fallback_shrinks_as_each_pane_is_visited() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::NewPane); // panes 1, 2, 3 — all real agents below
        for id in [1u64, 2, 3] {
            app.find_spec_mut(id).unwrap().adapter = "pi".into();
            app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::Waiting);
        }
        assert_eq!(app.attention_ring(), vec![1, 2, 3]);

        app.apply(Action::JumpAttention);
        assert_eq!(app.focused, 1);
        assert_eq!(app.attention_ring(), vec![2, 3], "visiting 1 retires it");

        app.apply(Action::JumpAttention);
        assert_eq!(app.focused, 2);
        assert_eq!(app.attention_ring(), vec![3], "visiting 2 retires it too");

        app.apply(Action::JumpAttention);
        assert_eq!(app.focused, 3);
        assert!(app.attention_ring().is_empty(), "all three visited — the tier is finally empty");

        // A fourth press must say so, not silently wrap back to pane 1.
        app.apply(Action::JumpAttention);
        assert_eq!(app.focused, 3, "nothing to jump to — focus does not move");
        assert_eq!(app.flash(), Some("nothing needs you"));
    }

    /// The finding named "visiting a waiting pane", not "Alt+a-ing to it" —
    /// `set_focus` is the one funnel every focus move shares (P10), so a
    /// plain spatial focus move must retire a pane exactly as a jump does.
    #[test]
    fn any_focus_move_not_just_alt_a_retires_a_visited_waiting_pane() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1|2, focus=2
        app.find_spec_mut(1).unwrap().adapter = "pi".into();
        app.find_spec_mut(2).unwrap().adapter = "pi".into();
        app.runtimes.get_mut(&1).unwrap().set_extension_status(AgentStatus::Waiting);
        app.runtimes.get_mut(&2).unwrap().set_extension_status(AgentStatus::Waiting);
        assert_eq!(app.attention_ring(), vec![1, 2]);

        app.apply(Action::Focus(layout::Dir::Left)); // 2 -> 1, no Alt+a involved
        assert_eq!(app.focused, 1);
        assert_eq!(app.attention_ring(), vec![2], "a plain focus move counts as a visit too");
    }

    /// The other half of the fix, and the one that makes it safe: a pane
    /// that starts a genuinely new turn after being visited must not stay
    /// silently excluded forever — the stale mark belongs to the finished
    /// turn that was already seen, not the fresh one.
    #[test]
    fn a_revisited_pane_re_enters_the_fallback_after_a_fresh_finished_turn() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1|2, focus=2
        app.find_spec_mut(1).unwrap().adapter = "pi".into();
        app.runtimes.get_mut(&1).unwrap().set_extension_status(AgentStatus::Waiting);

        app.apply(Action::JumpAttention); // visits and retires pane 1
        assert_eq!(app.focused, 1);
        assert!(app.attention_ring().is_empty());

        // Baseline tick: `last_status[1]` learns it's Waiting (first
        // observation — no transition yet).
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();

        // A new turn starts — `diff_statuses` must see the pane leave
        // Waiting and clear its stale visited mark right then, not later.
        app.runtimes.get_mut(&1).unwrap().set_extension_status(AgentStatus::Working);
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();

        // ...and finishes again: this is a brand new ○, not the one already seen.
        app.runtimes.get_mut(&1).unwrap().set_extension_status(AgentStatus::Waiting);
        assert_eq!(app.attention_ring(), vec![1], "a fresh finished turn must not be swallowed");
    }

    // -- C27 fleet roster -----------------------------------------------------

    /// A two-tab fixture with a needy pane parked in the *second* tab — the
    /// case the roster exists for (a pane you cannot see from where you are).
    /// Returns (app, tab0 order, tab1 order).
    fn roster_fixture() -> (App<FakePane>, Vec<PaneId>, Vec<PaneId>) {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // tab0: two panes
        let tab0 = app.pane_order();
        app.apply(Action::NewTab);
        app.apply(Action::NewPane); // tab1: two panes
        let tab1 = app.pane_order();
        app.go_to_tab(0);
        (app, tab0, tab1)
    }

    fn roster_panes<B: PaneBackend>(app: &App<B>) -> Vec<PaneId> {
        app.roster_rows()
            .into_iter()
            .filter_map(|r| match r {
                RosterRow::Pane { id } => Some(id),
                RosterRow::Group { .. } => None,
            })
            .collect()
    }

    fn roster_cursor<B: PaneBackend>(app: &App<B>) -> PaneId {
        match &app.mode {
            Mode::Roster { cursor, .. } => *cursor,
            _ => panic!("the roster is not open"),
        }
    }

    /// C27: the roster lists **every** pane in the workspace grouped by tab.
    /// With every pane tied on severity (the common case), the sort's tie-
    /// break is C19's ring enumeration verbatim — tabs ascending,
    /// `pane_order()` within each — so an all-quiet or all-needy fleet reads
    /// in plain ring order. The worst-first reorder itself is pinned
    /// separately below
    /// (`roster_rows_sort_worst_first_reorders_both_panes_and_groups`).
    #[test]
    fn roster_rows_group_every_pane_by_tab_in_ring_order() {
        let (mut app, tab0, tab1) = roster_fixture();
        app.apply(Action::ToggleRoster);
        let rows = app.roster_rows();
        assert!(matches!(&rows[0], RosterRow::Group { label } if label.contains("MAIN")));
        assert_eq!(rows[1], RosterRow::Pane { id: tab0[0] });
        assert_eq!(rows[2], RosterRow::Pane { id: tab0[1] });
        assert!(matches!(&rows[3], RosterRow::Group { label } if label.contains("TAB2")));
        assert_eq!(rows[4], RosterRow::Pane { id: tab1[0] });
        assert_eq!(rows[5], RosterRow::Pane { id: tab1[1] });
        assert_eq!(rows.len(), 6);
        // The pane sequence is the ring's own order, with nothing dropped:
        // make every pane needy (still tied, all ◆) and the two lists must
        // coincide.
        for id in tab0.iter().chain(tab1.iter()) {
            app.runtimes.get_mut(id).unwrap().set_extension_status(AgentStatus::NeedsInput);
        }
        assert_eq!(roster_panes(&app), app.attention_ring());
    }

    /// [ux P2-11] Sort worst-first (◆→○→⠋→·→exited, ⠋ = Working spinner), both within a tab's
    /// group and across groups by their own worst pane — the fix for
    /// "past one screenful you scroll hunting for the ◆s": the tab holding
    /// the one ◆ becomes the *first* group, and that pane its *first* row,
    /// however late the tab sits in tab order.
    #[test]
    fn roster_rows_sort_worst_first_reorders_both_panes_and_groups() {
        let (mut app, tab0, tab1) = roster_fixture(); // tab0=[a,b], tab1=[c,d]
                                                      // tab0 (earlier in tab order) is entirely quiet; tab1's *second*
                                                      // pane is the fleet's one ◆. Worst-first must promote tab1 to the
                                                      // first group, and that pane to the first row inside it.
        app.find_spec_mut(tab1[1]).unwrap().adapter = "pi".into();
        app.runtimes.get_mut(&tab1[1]).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.apply(Action::ToggleRoster);
        let rows = app.roster_rows();
        assert!(
            matches!(&rows[0], RosterRow::Group { label } if label.contains("TAB2")),
            "the tab with the ◆ leads: {rows:?}"
        );
        assert_eq!(rows[1], RosterRow::Pane { id: tab1[1] }, "and its ◆ leads inside it: {rows:?}");
        assert_eq!(rows[2], RosterRow::Pane { id: tab1[0] }, "tab1's quiet pane trails its own ◆");
        assert!(matches!(&rows[3], RosterRow::Group { label } if label.contains("MAIN")));
        assert_eq!(roster_panes(&app), vec![tab1[1], tab1[0], tab0[0], tab0[1]]);
    }

    /// Interaction (items 1 + 3 + 4), the case that matters: a fleet mixing
    /// an instrumented agent (explicit ◆), an uninstrumented agent resting
    /// on a heuristic `Waiting`, and plain shells. The roster sorts the
    /// agent's ○ ahead of every shell (P2-10's downgrade keeps a shell out of
    /// the ○ band entirely), and `Alt+a`'s fallback ring (P1-5) opens the
    /// roster's cursor on that same agent, not a shell.
    #[test]
    fn roster_and_ring_agree_on_a_mixed_instrumented_and_shell_fleet() {
        let (mut app, tab0, tab1) = roster_fixture(); // tab0=[a,b], tab1=[c,d], all shell
        app.find_spec_mut(tab0[1]).unwrap().adapter = "claude".into(); // instrumented, ◆
        app.runtimes.get_mut(&tab0[1]).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.find_spec_mut(tab1[0]).unwrap().adapter = "pi".into(); // uninstrumented, resting
        app.runtimes.get_mut(&tab1[0]).unwrap().set_extension_status(AgentStatus::Waiting);
        // tab1[1] and tab0[0] stay plain, untouched shells.

        // Item 1: the ring has the real ◆ only — the ○ fallback never runs
        // while a ◆ exists, and no shell is ever in it regardless.
        assert_eq!(app.attention_ring(), vec![tab0[1]]);

        // Item 3: MAIN (tab0) leads — its own worst pane is the fleet's ◆ —
        // with that ◆ first inside it, its quiet shell trailing; TAB2 (tab1)
        // follows, its resting agent's ○ leading its own quiet shell. A
        // shell never out-sorts a real ○, but it never leaves its own tab's
        // group either — sort reorders groups and reorders within them, it
        // does not flatten the fleet into one cross-tab severity list.
        app.apply(Action::ToggleRoster);
        assert_eq!(roster_panes(&app), vec![tab0[1], tab0[0], tab1[0], tab1[1]]);
        // The opening cursor is the ring's own pick (C27's contract) — the
        // real ◆, never the resting agent or a shell.
        assert_eq!(roster_cursor(&app), tab0[1]);
    }

    /// C22's float is not a tab, so it rides last under its own header —
    /// same placement it has in the C19 ring.
    #[test]
    fn roster_lists_the_float_last_under_its_own_group() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        app.apply(Action::ToggleRoster);
        let rows = app.roster_rows();
        assert_eq!(rows.last(), Some(&RosterRow::Pane { id: float_id }));
        assert!(
            matches!(&rows[rows.len() - 2], RosterRow::Group { label } if label.contains("FLOAT"))
        );
    }

    /// C27: a *hidden* float is still fleet, so it is still a row. C19's
    /// ring carries a hidden float that needs you, and the opening cursor is
    /// defined as the ring's next member — so a roster that listed the float
    /// only while it was shown could open with its cursor on a row that does
    /// not exist: no `❯` drawn anywhere, and Enter acting on something the
    /// user cannot see. Every cursor the roster can open on must be listed.
    #[test]
    fn roster_lists_a_hidden_float_so_the_opening_cursor_is_always_a_real_row() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleFloat); // spawn + show + focus
        let float_id = app.focused;
        app.apply(Action::ToggleFloat); // hide it again
        assert!(!app.float.as_ref().unwrap().shown, "the float is hidden");
        app.runtimes.get_mut(&float_id).unwrap().set_extension_status(AgentStatus::NeedsInput);
        assert!(app.attention_ring().contains(&float_id), "C19 rings a hidden needy float");

        app.apply(Action::ToggleRoster);
        assert_eq!(roster_cursor(&app), float_id, "the cursor opens on it");
        assert!(
            roster_panes(&app).contains(&float_id),
            "…and it is one of the rows: {:?}",
            roster_panes(&app)
        );
    }

    /// The load-bearing one: the opening cursor is the pane `Alt+a` would
    /// pick, so `Alt+Shift+a` `Enter` lands where `Alt+a` jumps. Pinned
    /// against `attention_ring` itself rather than a hand-written id.
    #[test]
    fn roster_opens_on_the_pane_alt_a_would_jump_to() {
        let (mut app, tab0, tab1) = roster_fixture();
        app.runtimes.get_mut(&tab1[1]).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.runtimes.get_mut(&tab0[1]).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.focused = tab0[1]; // sitting ON a ring member: Alt+a goes to the next
        app.apply(Action::ToggleRoster);
        assert_eq!(roster_cursor(&app), tab1[1]);
        assert_eq!(roster_cursor(&app), app.attention_ring()[1]);
    }

    /// ...and with nothing needing attention it opens where you already are.
    #[test]
    fn roster_opens_on_the_focused_pane_when_nothing_needs_attention() {
        let (mut app, tab0, _) = roster_fixture();
        app.focused = tab0[1];
        app.apply(Action::ToggleRoster);
        assert_eq!(roster_cursor(&app), tab0[1]);
    }

    /// C27: `Alt+Shift+a` `Enter` and `Alt+a` land on the same pane, from the
    /// same starting state — the equivalence the opening cursor exists for.
    #[test]
    fn roster_enter_lands_exactly_where_alt_a_would_have() {
        use crossterm::event::{KeyCode, KeyEvent};
        let needy = |app: &mut App<FakePane>, ids: &[PaneId]| {
            for id in ids {
                app.runtimes.get_mut(id).unwrap().set_extension_status(AgentStatus::NeedsInput);
            }
        };

        let (mut jump, tab0, tab1) = roster_fixture();
        needy(&mut jump, &[tab0[1], tab1[0]]);
        jump.apply(Action::JumpAttention);

        let (mut roster, ..) = roster_fixture();
        needy(&mut roster, &[tab0[1], tab1[0]]);
        roster.apply(Action::ToggleRoster);
        roster.handle_mode_key(KeyEvent::from(KeyCode::Enter));

        assert_eq!(roster.focused, jump.focused);
        assert_eq!(roster.ws.active_tab, jump.ws.active_tab);
        assert!(matches!(roster.mode, Mode::Normal), "the jump closes the roster behind it");
    }

    /// C27: Enter goes through `focus_attention_target`, so a jump into
    /// another tab switches tabs exactly like `Alt+a` and the feed do.
    #[test]
    fn roster_enter_jumps_across_tabs_through_the_shared_helper() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, _, tab1) = roster_fixture();
        assert_eq!(app.ws.active_tab, 0);
        app.apply(Action::ToggleRoster);
        // Walk the cursor onto the second tab's last pane: 3 pane rows down
        // from tab0's first pane, headers skipped by construction.
        app.focused = app.pane_order()[0];
        if let Mode::Roster { cursor, .. } = &mut app.mode {
            *cursor = app.ws.tabs[0].panes.keys().copied().min().unwrap();
        }
        for _ in 0..3 {
            app.handle_mode_key(KeyEvent::from(KeyCode::Down));
        }
        assert_eq!(roster_cursor(&app), tab1[1]);
        app.handle_mode_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.focused, tab1[1]);
        assert_eq!(app.ws.active_tab, 1, "the jump switched tabs");
        assert!(matches!(app.mode, Mode::Normal));
    }

    /// C27: `↑`/`↓` move by *pane*, never landing on a group header, and
    /// clamp at both ends instead of wrapping.
    #[test]
    fn roster_cursor_skips_group_headers_and_clamps_at_both_ends() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, tab0, tab1) = roster_fixture();
        app.focused = tab0[0];
        app.apply(Action::ToggleRoster);
        assert_eq!(roster_cursor(&app), tab0[0]);

        let mut seen = vec![roster_cursor(&app)];
        for _ in 0..5 {
            app.handle_mode_key(KeyEvent::from(KeyCode::Down));
            seen.push(roster_cursor(&app));
        }
        // Four panes, walked in order, then clamped on the last one — never a
        // header (a header is not a `PaneId` at all) and never wrapped.
        assert_eq!(seen, vec![tab0[0], tab0[1], tab1[0], tab1[1], tab1[1], tab1[1]]);

        for _ in 0..9 {
            app.handle_mode_key(KeyEvent::from(KeyCode::Up));
        }
        assert_eq!(roster_cursor(&app), tab0[0], "clamps at the top too");
    }

    /// C27: type-ahead, U20's idiom — printables filter on id / name /
    /// adapter, Backspace widens, and a group whose panes all filter out
    /// takes its header with it.
    #[test]
    fn roster_type_ahead_filters_on_id_name_and_adapter() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, tab0, tab1) = roster_fixture();
        app.find_spec_mut(tab1[0]).unwrap().title = Some("backend".into());
        app.apply(Action::ToggleRoster);
        assert_eq!(roster_panes(&app).len(), 4);

        for c in "backend".chars() {
            app.handle_mode_key(KeyEvent::from(KeyCode::Char(c)));
        }
        assert_eq!(roster_panes(&app), vec![tab1[0]], "filtered to the renamed pane");
        assert_eq!(app.roster_rows().len(), 2, "only its own group's header survives");
        assert_eq!(roster_cursor(&app), tab1[0], "the cursor follows the filter");

        for _ in 0..7 {
            app.handle_mode_key(KeyEvent::from(KeyCode::Backspace));
        }
        assert_eq!(roster_panes(&app).len(), 4, "Backspace widens it back");

        // The id is matchable on its own — it is the `roost send <id>` key.
        for c in tab0[1].to_string().chars() {
            app.handle_mode_key(KeyEvent::from(KeyCode::Char(c)));
        }
        assert!(roster_panes(&app).contains(&tab0[1]));
        // ...and so is the adapter every pane in the fixture shares.
        for _ in 0..4 {
            app.handle_mode_key(KeyEvent::from(KeyCode::Backspace));
        }
        for c in "shell".chars() {
            app.handle_mode_key(KeyEvent::from(KeyCode::Char(c)));
        }
        assert_eq!(roster_panes(&app).len(), 4);
    }

    /// [ux P2-11] `Tab`/`Shift+Tab` cycle the roster's status filter — worst-
    /// first, `None` ("every tier") at both ends so cycling never dead-ends
    /// — and each step narrows the visible rows to that tier alone.
    #[test]
    fn roster_tab_key_cycles_the_status_filter() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, tab0, tab1) = roster_fixture();
        app.runtimes.get_mut(&tab0[0]).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.runtimes.get_mut(&tab0[1]).unwrap().set_extension_status(AgentStatus::Working);
        // tab1[0] stays the default Idle; tab1[1] dies outright.
        app.runtimes.get_mut(&tab1[1]).unwrap().kill();
        app.apply(Action::ToggleRoster);
        assert_eq!(roster_panes(&app).len(), 4, "no filter: every pane");

        app.handle_mode_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(roster_panes(&app), vec![tab0[0]], "◆ only");
        app.handle_mode_key(KeyEvent::from(KeyCode::Tab));
        assert!(roster_panes(&app).is_empty(), "○ only — nothing is Waiting");
        app.handle_mode_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(roster_panes(&app), vec![tab0[1]], "⠋ (working) only");
        app.handle_mode_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(roster_panes(&app), vec![tab1[0]], "· only");
        app.handle_mode_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(roster_panes(&app), vec![tab1[1]], "exited only");
        app.handle_mode_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(roster_panes(&app).len(), 4, "wraps back to every tier");

        app.handle_mode_key(KeyEvent::from(KeyCode::BackTab));
        assert_eq!(roster_panes(&app), vec![tab1[1]], "Shift+Tab steps back, to exited");
    }

    /// [ux P2-11] The status filter composes with the text filter by AND — a
    /// row must satisfy both, exactly like a multi-facet search. A group
    /// whose panes all fail *either* filter drops its header too, the same
    /// rule the type-ahead alone already has.
    #[test]
    fn roster_status_filter_composes_with_text_filter_by_and() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, tab0, tab1) = roster_fixture();
        app.find_spec_mut(tab1[0]).unwrap().title = Some("backend".into());
        app.runtimes.get_mut(&tab0[0]).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.runtimes.get_mut(&tab1[0]).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.apply(Action::ToggleRoster);

        // Status alone: both ◆ panes, across both tabs.
        app.handle_mode_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(roster_panes(&app), vec![tab0[0], tab1[0]]);

        // AND the text filter in: only the one satisfying both survives.
        for c in "backend".chars() {
            app.handle_mode_key(KeyEvent::from(KeyCode::Char(c)));
        }
        assert_eq!(roster_panes(&app), vec![tab1[0]], "must satisfy both filters");
        assert_eq!(
            app.roster_rows().len(),
            2,
            "tab0's group — its ◆ fails the text filter — drops whole"
        );

        // Widen the text filter back: the status filter alone still narrows.
        for _ in 0..7 {
            app.handle_mode_key(KeyEvent::from(KeyCode::Backspace));
        }
        assert_eq!(roster_panes(&app), vec![tab0[0], tab1[0]]);
    }

    /// [design-supervisor D2] A never-started pane draws `·` and sorts in
    /// the `·` band (`roster_rank` puts `None` with `Idle`) — the `·` filter
    /// must show it too, or it draws idle yet vanishes under its own tier's
    /// filter. Exactly C27's headline case: a restored workspace with an
    /// unspawned background tab.
    #[test]
    fn roster_idle_filter_includes_a_never_started_pane() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, tab0, tab1) = roster_fixture();
        app.runtimes.remove(&tab1[0]); // never spawned: lazy background tab
        assert_eq!(app.display_status(tab1[0]), None);
        app.apply(Action::ToggleRoster);

        for _ in 0..4 {
            app.handle_mode_key(KeyEvent::from(KeyCode::Tab)); // cycle to Idle
        }
        assert!(roster_panes(&app).contains(&tab1[0]), "not-started sorts and filters with idle");
        // The rest of the fixture is untouched shell, also Idle.
        assert_eq!(roster_panes(&app), vec![tab0[0], tab0[1], tab1[0], tab1[1]]);
    }

    /// U18: a mode's entry chord exits it — `Alt+Shift+a` closes the roster,
    /// leaving exactly what `Esc` leaves.
    #[test]
    fn roster_entry_chord_and_esc_both_close_it() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let chord = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT | KeyModifiers::SHIFT);

        let (mut app, ..) = roster_fixture();
        app.apply(Action::ToggleRoster);
        assert!(app.handle_mode_key(chord), "the toggle is consumed, never re-dispatched");
        assert!(matches!(app.mode, Mode::Normal));

        let (mut app, ..) = roster_fixture();
        app.apply(Action::ToggleRoster);
        app.handle_mode_key(KeyEvent::from(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Normal));
    }

    /// C27: the roster is a C12 modal, so U8's rules cover it — and Enter on
    /// a pane that closed while it was open flashes instead of moving focus.
    #[test]
    fn roster_is_a_modal_and_a_stale_cursor_flashes_instead_of_jumping() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, tab0, _) = roster_fixture();
        app.apply(Action::ToggleRoster);
        assert!(app.modal_active(), "the roster owns the mouse and paste surface (U8)");
        if let Mode::Roster { cursor, .. } = &mut app.mode {
            *cursor = 999; // an id no pane has
        }
        let focused = app.focused;
        app.handle_mode_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.focused, focused, "no focus move");
        assert!(matches!(app.mode, Mode::Roster { .. }), "and the roster stays up");
        assert_eq!(app.flash(), Some("that pane is gone"));
        assert!(tab0.contains(&focused));
    }

    /// C27 "live": the list reflects the workspace while it is open. Close a
    /// pane under the roster and its row goes with it.
    #[test]
    fn roster_rows_follow_the_workspace_while_it_is_open() {
        let (mut app, tab0, _) = roster_fixture();
        app.apply(Action::ToggleRoster);
        assert_eq!(roster_panes(&app).len(), 4);
        app.close_pane_id(tab0[1]);
        let panes = roster_panes(&app);
        assert_eq!(panes.len(), 3);
        assert!(!panes.contains(&tab0[1]));
    }

    /// C27: the window scrolls to keep the cursor visible, and clamps rather
    /// than running off the end of a list that shrank underneath it.
    #[test]
    fn roster_window_follows_the_cursor_and_clamps() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, _) = mk_app(shell_ws());
        for _ in 0..7 {
            app.apply(Action::NewPane);
        }
        // A short terminal: the overlay shows fewer rows than the fleet has.
        app.on_resize(Size::new(100, 12), (0, 0));
        app.apply(Action::ToggleRoster);
        let height = app.roster_view_rows();
        let visible = |app: &App<FakePane>| {
            let (rows, top) = app.roster_view();
            let at = rows
                .iter()
                .position(|r| matches!(r, RosterRow::Pane { id } if *id == roster_cursor(app)))
                .expect("the cursor is on a row");
            assert!(at >= top && at < top + height, "the cursor is inside the window");
            // "the window never runs past the end" used to be asserted
            // here as `top + height <= rows.len()` — which is
            // `roster_top_clamped`'s own postcondition (`top.min(len -
            // height)`), applied unconditionally by `roster_view` on the
            // line above. It could not fail. The property is real, so it
            // moved to `roster_top_clamped`'s own test, where the clamp is
            // the thing under test rather than the thing supplying the
            // answer. Sixth instance of the shape; see DESIGN-ui.md §7.
            top
        };
        let (rows, _) = app.roster_view();
        assert!(rows.len() > height, "fixture must overflow the window");
        // It opens scrolled to wherever the cursor landed — here the focused
        // pane, which is the last one spawned, i.e. off the first screenful.
        let opened_at = visible(&app);
        assert!(opened_at > 0, "the window scrolled to reveal the opening cursor");

        for _ in 0..8 {
            app.handle_mode_key(KeyEvent::from(KeyCode::Up));
        }
        assert_eq!(visible(&app), 0, "walking back up scrolls the window home");
        for _ in 0..8 {
            app.handle_mode_key(KeyEvent::from(KeyCode::Down));
        }
        visible(&app);
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

    /// Quitting by closing the last pane must keep the pane it quits on.
    ///
    /// `Alt+w` on the last pane is a quit — its own confirm says "last pane
    /// — Alt+w again to quit roost", and the control plane refuses the same
    /// call rather than quitting for a caller. It used to remove the pane
    /// first anyway, and that removal reached disk: `layout::remove_pane`
    /// cannot drop a bare `Pane` root and one tab cannot be dropped either,
    /// so `shutdown`'s save wrote a tab whose layout named a pane its
    /// `panes` map no longer had. `validate_and_repair` then minted that id
    /// a fresh `shell` spec in roost's launch directory on the next start —
    /// so the session id, cwd, title and note of the pane you quit on were
    /// gone, silently, while `Alt+q` on the same pane kept every one of
    /// them. Found by the mouse fuzzer, which — unlike the layout fuzzer —
    /// does not skip `ClosePane` on the last pane.
    #[test]
    fn quitting_by_closing_the_last_pane_keeps_the_pane_it_quits_on() {
        let mut panes = HashMap::new();
        panes.insert(
            1,
            PaneSpec {
                adapter: "claude".into(),
                cwd: PathBuf::from("/repos/important"),
                session: Some("11111111-2222-3333-4444-555555555555".into()),
                title: Some("the pane you quit on".into()),
                spawned_by: None,
                note: Some("what's next".into()),
                noted_at: None,
            },
        );
        let ws = Workspace {
            version: 1,
            active_tab: 0,
            tabs: vec![Tab { name: "main".into(), layout: LayoutNode::Pane(1), panes }],
        };
        let (mut app, store) = mk_app(ws);

        app.apply(Action::ClosePane); // arms the confirm
        assert!(!app.quit, "the first press only arms");
        app.apply(Action::ClosePane); // confirms
        assert!(app.quit, "the second press quits");

        let spec =
            app.ws.tabs[0].panes.get(&1).expect("the pane roost quit on is still in the workspace");
        assert_eq!(spec.adapter, "claude");
        assert_eq!(spec.cwd, PathBuf::from("/repos/important"));
        assert_eq!(spec.session.as_deref(), Some("11111111-2222-3333-4444-555555555555"));
        assert_eq!(spec.title.as_deref(), Some("the pane you quit on"));
        assert_eq!(spec.note.as_deref(), Some("what's next"));

        // ...and that is what reaches disk, satisfying the tree-vs-map
        // invariant *before* `validate_and_repair` has to rescue it — the
        // repair mints a blank spec, so a file that needs rescuing here has
        // already lost everything asserted above.
        app.save();
        let saved = store.0.lock().unwrap().clone().expect("shutdown saves");
        assert_eq!(saved.tabs[0].panes.len(), 1, "saved workspace still holds the pane");
        let mut ids = Vec::new();
        layout::pane_order(&saved.tabs[0].layout, &mut ids);
        assert_eq!(ids, vec![1], "the layout names exactly the pane the map holds");
        assert_eq!(
            saved.tabs[0].panes[&1].session.as_deref(),
            Some("11111111-2222-3333-4444-555555555555"),
            "the resumable conversation survived the quit",
        );
    }

    /// `animating` must answer for the whole fleet, not the active tab, and
    /// must agree with the surfaces that actually draw a spinner — a pane
    /// working quietly in another tab still spins in the tab strip's
    /// summary and in C27's roster.
    ///
    /// It picks the event loop's repaint budget (`main.rs`'s
    /// `should_repaint`): a `false` here that should have been `true` is a
    /// spinner stepping at the outer ceiling instead of at its own frame
    /// rate. Cheap, but only because that ceiling exists.
    #[test]
    fn animating_is_true_exactly_while_the_fleet_has_a_spinner_on_screen() {
        let (mut app, _) = mk_app(shell_ws());
        assert!(!app.animating(), "a quiet fleet draws no spinner");

        app.apply(Action::NewTab); // pane 1 in tab 0, a fresh pane in tab 1
        let elsewhere = app.ws.tabs[0].panes.keys().copied().next().expect("tab 0 has a pane");
        assert_ne!(app.ws.active_tab, 0, "the new tab is the one on screen");

        app.runtimes.get_mut(&elsewhere).unwrap().set_extension_status(AgentStatus::Working);
        assert_eq!(app.display_status(elsewhere), Some(AgentStatus::Working));
        assert!(
            app.animating(),
            "a pane working in another tab still spins in the tab strip and the roster",
        );

        app.runtimes.get_mut(&elsewhere).unwrap().set_extension_status(AgentStatus::Waiting);
        assert!(!app.animating(), "and stops when it does");

        // Exited is not animation either — a dead pane's glyph is static.
        let _ = app.on_pty_exit(elsewhere);
        assert!(!app.animating());
    }

    /// C34: the confirm flashes name the chord you are being told to press
    /// again, and before C34 they spelled it as a literal — so under a
    /// remap the guard instructed a key that does nothing. That is U1's
    /// hazard wearing a helpful face: a confirmation that cannot be
    /// satisfied by the key it names.
    #[test]
    fn the_close_confirm_names_the_live_chord() {
        use crate::ui::input::Keymap;
        let (keymap, diagnostics) =
            Keymap::parse(r#"{"keys": {"alt+v": "close_pane"}}"#, "config.json");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let (mut app, _) = mk_app(shell_ws());
        app.set_keymap(keymap);
        app.apply(Action::ClosePane); // last pane ⇒ arms the quit confirm
        let flash = app.flash().unwrap_or_default().to_string();
        assert!(flash.contains("Alt+v again"), "names the live chord: {flash}");
        assert!(!flash.contains("Alt+w"), "and not the default: {flash}");
    }

    /// And with the chord disabled entirely the sentence stays true rather
    /// than naming nothing: the confirm is armed on the *action*, so a
    /// repeat by any route satisfies it.
    #[test]
    fn the_close_confirm_drops_the_chord_when_it_is_unbound() {
        use crate::ui::input::Keymap;
        let (keymap, _) = Keymap::parse(r#"{"keys": {"alt+w": "disable"}}"#, "config.json");
        let (mut app, _) = mk_app(shell_ws());
        app.set_keymap(keymap);
        app.apply(Action::ClosePane);
        let flash = app.flash().unwrap_or_default().to_string();
        assert!(flash.contains("again to quit roost"), "still instructive: {flash}");
        assert!(!flash.contains("Alt+"), "but names no dead key: {flash}");
    }

    // ---- C37: reverse the layout cycle (Alt+Shift+g) ---------------------

    /// Which of C25's three arrangements a tab is showing.
    ///
    /// The cycle compares by *shape*, not by tree: `arrangement_for`
    /// re-derives from the live `pane_order`, which a previous step can
    /// have reordered, so two visits to "grid" are the same arrangement
    /// with the panes in different slots. That is pre-existing C25
    /// behaviour — the forward-only tests already assert shapes rather than
    /// trees — and C37 inherits it rather than introducing it.
    fn arrangement_kind(layout: &LayoutNode) -> &'static str {
        match layout {
            LayoutNode::Stack { .. } => "all-stack",
            LayoutNode::Split { ratios, .. } if ratios == &vec![0.6, 0.4] => "main+stack",
            LayoutNode::Split { .. } => "grid",
            LayoutNode::Pane(_) => "single",
        }
    }

    /// The property that makes a reverse cycle worth a chord at all: from
    /// inside the ring it is the *inverse*. Forward then back lands on the
    /// arrangement you started from, at every point — otherwise it is just
    /// a second way to go forward, and with three arrangements that buys
    /// nothing.
    ///
    /// "From inside the ring" is the contract, not a test convenience: a
    /// tab's layout before the first `Alt+g` is whatever splitting left it,
    /// which is **not** one of the three canned arrangements.
    /// `layout_cycle` is a position in a three-item ring, not an undo
    /// stack, so stepping back from the first wraps to the last rather than
    /// restoring a pre-cycle custom layout. Anything else would need the
    /// cycle to remember arbitrary layouts, which is what `Alt+u` and the
    /// split chords are for.
    #[test]
    fn reversing_the_layout_cycle_undoes_a_forward_step() {
        let (mut app, _) = mk_app(shell_ws()); // 100x30 fits all three shapes
        app.apply(Action::NewPane);
        app.apply(Action::NewPane);
        app.apply(Action::CycleLayout { forward: true }); // enter the ring

        for _ in 0..3 {
            let before = arrangement_kind(&app.ws.tabs[0].layout);
            app.apply(Action::CycleLayout { forward: true });
            assert_ne!(arrangement_kind(&app.ws.tabs[0].layout), before, "forward moved");
            app.apply(Action::CycleLayout { forward: false });
            assert_eq!(
                arrangement_kind(&app.ws.tabs[0].layout),
                before,
                "back returns to the arrangement forward left",
            );
        }
    }

    /// Walking backwards reaches all three arrangements and closes the
    /// ring, so the reverse direction is a full traversal rather than a
    /// one-step nudge that then sticks.
    #[test]
    fn the_reverse_cycle_visits_every_arrangement() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::NewPane);
        app.apply(Action::CycleLayout { forward: true }); // enter the ring

        let start = arrangement_kind(&app.ws.tabs[0].layout);
        let mut seen = vec![start];
        for _ in 0..2 {
            app.apply(Action::CycleLayout { forward: false });
            seen.push(arrangement_kind(&app.ws.tabs[0].layout));
        }
        let mut unique = seen.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 3, "three distinct arrangements going back: {seen:?}");

        app.apply(Action::CycleLayout { forward: false });
        assert_eq!(arrangement_kind(&app.ws.tabs[0].layout), start, "and the ring closes");
    }

    /// The skip-what-doesn't-fit rule (C25) applies in reverse too: a shape
    /// the terminal is too small for is passed over going back exactly as
    /// it is going forward, so the two directions can't disagree about
    /// which arrangements exist.
    #[test]
    fn the_reverse_cycle_skips_shapes_that_do_not_fit() {
        // Narrow enough that main+stack cannot fit, as the forward-only
        // sibling test already establishes for this geometry.
        let (mut app, _) = mk_app(shell_ws());
        // The same geometry the forward-only sibling test uses: body ~80x20,
        // where main+stack's 0.4 column falls under the split floor.
        app.on_resize(Size::new(80, 22), (0, 0));
        app.apply(Action::NewPane);
        app.apply(Action::CycleLayout { forward: false });
        // Whatever it landed on must be a shape that actually fits — the
        // reverse walk never installs an arrangement forward would refuse.
        assert!(
            layout::arrangement_fits(&app.ws.tabs[0].layout, app.body_area()),
            "the reverse walk only installs shapes that fit",
        );
    }

    // ---- C36: broadcast composer (Alt+') --------------------------------

    /// The verb roost was uniquely positioned for and you had to leave roost
    /// to use: one message, every reachable pane, submitted.
    #[test]
    fn broadcast_sends_the_composed_message_to_every_reachable_pane() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::NewPane); // three panes

        app.apply(Action::ToggleBroadcast);
        for c in "ship it".chars() {
            app.handle_mode_key(key(c));
        }
        press(&mut app, crossterm::event::KeyCode::Enter);

        assert!(matches!(app.mode, Mode::Normal), "sending closes the composer");
        for id in app.pane_order() {
            assert!(
                app.runtimes.get(&id).unwrap().input.ends_with(b"ship it\r"),
                "pane {id} got the message, submitted",
            );
        }
        assert!(
            app.flash().unwrap_or_default().contains("broadcast to 3 panes"),
            "{:?}",
            app.flash()
        );
    }

    /// C36's safety affordance is the *visible target count*, so the count
    /// the title shows and the set the send writes to must be the same
    /// predicate — a guard that lies is worse than no guard. Both go
    /// through `broadcast_targets`; this pins that they agree, including
    /// under a status filter.
    #[test]
    fn the_target_count_is_the_set_that_actually_receives() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::NewPane);
        let order = app.pane_order();
        // Put exactly one pane in NeedsInput.
        let marked = order[1];
        app.runtimes.get_mut(&marked).unwrap().set_extension_status(AgentStatus::NeedsInput);

        let filtered = app.broadcast_targets(Actor::Local, Some(AgentStatus::NeedsInput));
        assert_eq!(filtered, vec![marked], "the filter selects exactly the ◆ pane");

        app.apply(Action::ToggleBroadcast);
        if let Mode::Broadcast { status_filter, .. } = &mut app.mode {
            *status_filter = Some(AgentStatus::NeedsInput);
        }
        for c in "only you".chars() {
            app.handle_mode_key(key(c));
        }
        press(&mut app, crossterm::event::KeyCode::Enter);

        assert!(
            app.flash().unwrap_or_default().contains("broadcast to 1 pane"),
            "{:?}",
            app.flash()
        );
        for id in order {
            let got = app.runtimes.get(&id).unwrap().input.ends_with(b"only you\r");
            assert_eq!(got, id == marked, "pane {id} received iff it was targeted");
        }
    }

    /// Esc is a real abort: nothing is written. The composer exists so that
    /// a fleet-wide send is always a deliberate act.
    #[test]
    fn escaping_the_composer_sends_nothing() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::ToggleBroadcast);
        for c in "never".chars() {
            app.handle_mode_key(key(c));
        }
        press(&mut app, crossterm::event::KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
        for id in app.pane_order() {
            let got = String::from_utf8_lossy(&app.runtimes.get(&id).unwrap().input).into_owned();
            assert!(!got.contains("never"), "pane {id} got nothing");
        }
    }

    /// Shift+Enter breaks a line rather than sending — C32's convention,
    /// and the reason the composer can carry a multi-line agent prompt at
    /// all. Plain Enter after it still sends the whole thing.
    #[test]
    fn shift_enter_breaks_a_line_instead_of_sending() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleBroadcast);
        app.handle_mode_key(key('a'));
        app.handle_mode_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        app.handle_mode_key(key('b'));
        match &app.mode {
            Mode::Broadcast { lines, row, .. } => {
                assert_eq!(lines, &vec!["a".to_string(), "b".to_string()]);
                assert_eq!(*row, 1);
            }
            _ => panic!("still composing, not sent"),
        }
    }

    /// C36 audits as `Actor::Local`, not `Fleet`: attribution is the audit
    /// log's whole value, and a keyboard broadcast logged as a token
    /// holder's would be indistinguishable from one. The C20 feed comes
    /// along for free because it renders the same ctl lines.
    #[test]
    fn a_keyboard_broadcast_is_audited_as_local_and_reaches_the_feed() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::ToggleBroadcast);
        app.handle_mode_key(key('x'));
        press(&mut app, crossterm::event::KeyCode::Enter);

        let ctl: Vec<&FeedEntry> =
            app.feed().iter().filter(|e| e.text.starts_with("ctl ")).collect();
        assert_eq!(ctl.len(), 1, "exactly one ctl line, as the control path also guarantees");
        assert!(
            ctl[0].text.contains("local"),
            "attributed to the human, not the fleet: {:?}",
            ctl[0].text
        );
    }

    /// C12/U8, and the highest-severity thing the C36 audit found: the
    /// composer was not a modal to the mouse or to paste. `modal_active`
    /// omitted it, so a click moved focus under the open dialog and — the
    /// dangerous half — a paste landed in the **pane behind it**. Someone
    /// composing a fleet-wide message and pasting a snippet would have
    /// silently typed it into whichever agent happened to be focused.
    #[test]
    fn the_composer_is_a_modal_to_the_mouse_and_to_paste() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        let focused = app.focused;
        app.apply(Action::ToggleBroadcast);
        assert!(app.modal_active(), "the composer takes the mouse/paste path");

        app.handle_paste("pasted");
        for id in app.pane_order() {
            let got = String::from_utf8_lossy(&app.runtimes.get(&id).unwrap().input).into_owned();
            assert!(
                !got.contains("pasted"),
                "pane {id} must not receive a paste meant for the dialog"
            );
        }
        assert_eq!(app.focused, focused, "and focus did not move beneath it");
    }

    /// C36/D2: on Terminal.app, Shift+Enter arrives as Alt+Enter, so
    /// without the same carve-out C32 has, the one chord that should break
    /// a line instead opened the quick-launch picker and discarded the
    /// composed message. "Keys are C32's, exactly" was an overclaim until
    /// this passed.
    #[test]
    fn alt_enter_breaks_a_composer_line_rather_than_opening_the_picker() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleBroadcast);
        app.handle_mode_key(key('a'));
        app.handle_mode_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        app.handle_mode_key(key('b'));
        match &app.mode {
            Mode::Broadcast { lines, .. } => {
                assert_eq!(lines, &vec!["a".to_string(), "b".to_string()]);
            }
            _ => panic!("Alt+Enter left the composer — it opened the picker and ate the message"),
        }
    }

    /// C36: the dialog grows a row per break and never scrolls, so an
    /// uncapped message pushes its own rows and the caret off the body with
    /// no way back. The break is refused at the cap rather than partly
    /// applied — losing the split is recoverable, losing the text is not.
    #[test]
    fn the_composer_stops_growing_at_its_line_cap() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleBroadcast);
        for _ in 0..(BROADCAST_MAX_LINES + 5) {
            app.handle_mode_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        }
        match &app.mode {
            Mode::Broadcast { lines, .. } => assert_eq!(lines.len(), BROADCAST_MAX_LINES),
            _ => panic!("still composing"),
        }
    }

    // ---- C35: go back (Alt+;) -------------------------------------------

    /// The motion the fleet actually makes: check on B, come back to A. And
    /// it must *ping-pong* — pressing it twice returns you where you
    /// started — which falls out of recording the trail on every real focus
    /// transition rather than only on the toggle.
    #[test]
    fn back_returns_to_the_previous_pane_and_toggles() {
        use crate::core::layout::Dir;
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1|2, focus 2
        app.apply(Action::Focus(Dir::Left));
        assert_eq!(app.focused, 1);

        app.apply(Action::FocusAlternate);
        assert_eq!(app.focused, 2, "back goes to where focus came from");
        app.apply(Action::FocusAlternate);
        assert_eq!(app.focused, 1, "and again returns — it is a toggle, not a stack");
    }

    /// The case that motivates the chord over `Alt+a`: the pane you were on
    /// is in another tab. Reusing `focus_attention_target` means the tab
    /// switch comes for free and behaves exactly like the jump's.
    #[test]
    fn back_returns_across_a_tab_switch() {
        let (mut app, _) = mk_app(shell_ws());
        let first = app.focused;
        app.apply(Action::NewTab);
        let second_tab = app.ws.active_tab;
        assert_ne!(second_tab, 0, "the fixture really switched tabs");

        app.apply(Action::FocusAlternate);
        assert_eq!(app.focused, first, "focus is back on the original pane");
        assert_eq!(app.ws.active_tab, 0, "and the tab came with it");
    }

    /// Before focus has ever moved there is nothing to go back to, and the
    /// chord says so rather than doing nothing — a navigation key that
    /// silently no-ops reads as broken.
    #[test]
    fn back_with_nowhere_to_go_flashes() {
        let (mut app, _) = mk_app(shell_ws());
        let focused = app.focused;
        app.apply(Action::FocusAlternate);
        assert_eq!(app.focused, focused);
        assert!(
            app.flash().unwrap_or_default().contains("no pane to go back to"),
            "{:?}",
            app.flash()
        );
    }

    /// The alternate can die while you are away. Landing focus on a closed
    /// pane id would be a real bug, so the pointer is validated on use and
    /// cleared — the whole value of this chord is confidence about where it
    /// puts you.
    #[test]
    fn back_to_a_closed_pane_flashes_instead_of_landing_on_a_dead_id() {
        use crate::core::layout::Dir;
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // 1|2, focus 2
        app.apply(Action::Focus(Dir::Left)); // focus 1, alternate 2
        let doomed = 2;
        app.close_pane_id(doomed);

        app.apply(Action::FocusAlternate);
        assert_eq!(app.focused, 1, "focus did not move to the closed pane");
        assert!(app.flash().unwrap_or_default().contains("has closed"), "{:?}", app.flash());
    }

    // ---- C33: move a pane within its tab (Alt+Shift+hjkl) ---------------

    /// The core gesture: the focused pane trades places with its spatial
    /// neighbour, and focus follows it — for free, because the tree swaps
    /// two ids and the focused id never changes.
    #[test]
    fn moving_a_pane_swaps_it_with_its_neighbour_and_focus_follows() {
        use crate::core::layout::Dir;
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1|2 side by side, focus = 2 (right)

        let left_before = app.rects().iter().min_by_key(|p| p.rect.x).unwrap().id;
        assert_eq!(left_before, 1);

        app.apply(Action::MovePane(Dir::Left));

        let left_after = app.rects().iter().min_by_key(|p| p.rect.x).unwrap().id;
        assert_eq!(left_after, 2, "pane 2 took the left slot");
        assert_eq!(app.focused, 2, "focus rode along with the pane it moved");
    }

    /// C33's stated edge rule, and the one deliberate divergence from
    /// `focus_dir`: `Alt+←/→` continues into the next tab at an edge (C31),
    /// but moving a pane there does **not** — `Alt+Shift+i`/`Alt+Shift+m`
    /// (C28) are the chords that take a pane out of its tab, and they say
    /// so. A no-op is the recoverable failure.
    #[test]
    fn moving_a_pane_off_the_tabs_edge_is_a_no_op_not_a_cross_tab_move() {
        use crate::core::layout::Dir;
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewTab); // tab 2
        app.apply(Action::NewPane); // tab 2 holds panes 2|3, focus = 3 (right)
        let tab_before = app.ws.active_tab;
        let panes_before = app.ws.active_tab().panes.len();

        app.apply(Action::MovePane(Dir::Right)); // already at the right edge

        assert_eq!(app.ws.active_tab, tab_before, "the tab did not change");
        assert_eq!(
            app.ws.active_tab().panes.len(),
            panes_before,
            "and the pane did not leave the tab",
        );
        assert_eq!(app.focused, 3);
    }

    /// C38: `Alt+Shift+hjkl` on the float refuses and says why — and it can
    /// actually reach that refusal.
    ///
    /// It could not when C38 shipped: `apply`'s structural guard hid the
    /// float for `MovePane` *before* `move_pane_dir` ran, so
    /// `float_focused()` was always false and the contract's third row was
    /// dead code. What happened instead was worse than the silence C38 set
    /// out to fix — the float vanished and the swap landed on whichever
    /// pane it had been covering, so the gesture moved a pane the user was
    /// not looking at. Found by the C38 design audit, which probed the row
    /// rather than reading it.
    #[test]
    fn moving_the_float_refuses_instead_of_hiding_it_and_moving_something_else() {
        use crate::core::layout::Dir;
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1|2 in the tab
        let before = format!("{:?}", app.ws.active_tab().layout);
        app.apply(Action::ToggleFloat);
        assert!(app.float_focused(), "the float is up and focused");

        app.apply(Action::MovePane(Dir::Left));

        assert!(app.float_focused(), "the float is still up — the chord refused it");
        assert_eq!(
            format!("{:?}", app.ws.active_tab().layout),
            before,
            "and nothing behind it moved",
        );
        assert_eq!(app.flash(), Some("the scratch pane sits outside the layout"),);
    }

    /// C38: the edge no-op **says so**, and horizontally it names the chord
    /// that does cross tabs — the dead end teaches the way out.
    ///
    /// C33 defended the no-op ("the recoverable failure is doing nothing"),
    /// which is an argument about the *swap*, not about the silence. C35
    /// had already written the opposite rule for the sibling gesture: "a
    /// navigation key that silently does nothing reads as broken." Two
    /// contracts that shipped the same week disagreed, and a user pressing
    /// `Alt+Shift+h` at an edge could not tell "bound but nowhere to go"
    /// from "not bound".
    #[test]
    fn moving_a_pane_off_the_edge_names_the_chord_that_would_work() {
        use crate::core::layout::Dir;
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewTab); // a second tab, so the carry chord is usable
        app.apply(Action::NewPane); // panes 2|3 here, focus 3 (right)

        app.apply(Action::MovePane(Dir::Right)); // at the right edge
        let right = app.flash().expect("the edge speaks").to_string();
        assert!(right.contains("edge"), "{right}");
        let carry = app
            .chord_label(Action::MovePaneToTab { forward: true })
            .expect("the cross-tab chord is bound by default");
        assert!(right.contains(&carry), "the flash names {carry}: {right}");
        assert!(right.contains("next tab"), "and which way it goes: {right}");

        // Vertically there is no cross-tab chord to name (C31: tabs are the
        // horizontal axis), so the flash reports and offers nothing.
        app.apply(Action::MovePane(Dir::Up));
        let up = app.flash().expect("the edge speaks").to_string();
        assert_eq!(up, "nothing above to swap with");
    }

    /// C38: with nowhere to carry the pane *to*, the flash offers nothing.
    ///
    /// The clause was guarded on whether the chord was **bound**, not on
    /// whether it would work — so on the default single-tab workspace the
    /// edge flash named `Alt+Shift+i`, which then refuses with "only one
    /// tab": a dead end handing you a second dead end. Found by the C38
    /// design audit.
    #[test]
    fn the_edge_flash_offers_nothing_when_there_is_no_other_tab() {
        use crate::core::layout::Dir;
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // one tab, panes 1|2, focus 2

        app.apply(Action::MovePane(Dir::Right));
        assert_eq!(app.flash(), Some("at the tab's edge"));
    }

    /// C38: `Alt+5` on a two-tab workspace says so, and says what the range
    /// is. The digit keys are the most guessed-at chords roost has, and
    /// this is C35's own case verbatim.
    #[test]
    fn a_tab_digit_past_the_end_says_how_many_tabs_there_are() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewTab); // two tabs now

        app.apply(Action::GoToTab(4)); // "Alt+5"
        assert_eq!(app.flash(), Some("no tab 5: this workspace has 2 tabs"));
        assert_eq!(app.ws.active_tab, 1, "and it did not move");

        // Singular reads correctly too — the common case is one tab.
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::GoToTab(2));
        assert_eq!(app.flash(), Some("no tab 3: this workspace has 1 tab"));
    }

    /// C38: the three structural chords say so when the tab's shape has
    /// nothing for them, rather than looking unbound. Detected by comparing
    /// the tree — `resize_pane` returns `true` at the ratio clamp ("handled,
    /// no change"), so the mutator's bool is not the question.
    #[test]
    fn the_structural_chords_say_when_a_tab_has_nothing_for_them() {
        let (mut app, _) = mk_app(shell_ws()); // one pane

        for (action, verb) in [
            (Action::ToggleStack, "stack"),
            (Action::FlipSplit, "flip"),
            (Action::Resize { horizontal: true, grow: true }, "resize"),
        ] {
            app.set_flash("stale");
            app.apply(action);
            let flash = app.flash().expect("{verb} speaks").to_string();
            assert!(flash.starts_with(&format!("nothing to {verb}")), "{flash}");
            assert!(flash.contains("one pane"), "it says why: {flash}");
            let split = app.chord_label(Action::NewPane).expect("Alt+n is bound");
            assert!(flash.contains(&split), "and what to press: {flash}");
        }
    }

    /// ...and once the tab has a shape they apply to, they stay quiet.
    #[test]
    fn the_structural_chords_stay_quiet_when_they_do_something() {
        // A fresh tab per verb: run in sequence they interfere honestly —
        // `Alt+s` turns the split into a stack, and `Alt+o` then has
        // nothing to flip and correctly says so. That is the code being
        // right, so each verb gets the shape it applies to.
        for action in [
            Action::ToggleStack,
            Action::FlipSplit,
            Action::Resize { horizontal: true, grow: true },
        ] {
            let (mut app, _) = mk_app(shell_ws());
            app.apply(Action::NewPane); // a real split to work on
            app.flash = None;
            app.apply(action);
            assert!(app.flash().is_none(), "{:?}: {:?}", action, app.flash());
        }
    }

    /// The composer's `Tab` must never stop on a tier it cannot deliver to.
    ///
    /// `broadcast_targets` filters `Exited` unconditionally — a pane that is
    /// gone cannot receive anything — so the shared cycle's last stop was
    /// structurally inert on that surface: `Tab` landed on "exited", the
    /// title's live count read 0, and Enter sent nothing. Recorded from the
    /// C36 design audit; this is the check that it was real.
    #[test]
    fn the_composer_never_stops_on_a_tier_it_cannot_send_to() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::ToggleBroadcast);

        // A full lap of Tab, from every starting point.
        let mut seen = Vec::new();
        for _ in 0..ROSTER_STATUS_CYCLE.len() + 2 {
            app.handle_mode_key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Tab));
            let Mode::Broadcast { status_filter, .. } = app.mode else { panic!("still composing") };
            seen.push(status_filter);
        }
        assert!(
            !seen.contains(&Some(AgentStatus::Exited)),
            "Tab stopped on a tier with nothing to send to: {seen:?}",
        );
        // And it still reaches every tier it *can* send to, in both
        // directions — skipping a stop must not shorten the lap.
        for want in [None, Some(AgentStatus::NeedsInput), Some(AgentStatus::Idle)] {
            assert!(seen.contains(&want), "{want:?} is unreachable now: {seen:?}");
        }
    }

    /// `Tab` and `Shift+Tab` stay exact inverses **across the skipped
    /// tier**, which is the part a skip is most likely to break: a naive
    /// "if you land on Exited, go to None" would make the two directions
    /// agree at one point in the lap and diverge for the rest of it.
    #[test]
    fn the_composer_cycle_is_reversible_across_the_gap() {
        for &start in &ROSTER_STATUS_CYCLE {
            if start == Some(AgentStatus::Exited) {
                continue; // not a state the composer can be in
            }
            let fwd = step_status_filter(start, false, true);
            assert_eq!(
                step_status_filter(fwd, true, true),
                start,
                "forward then back from {start:?} landed on {:?}",
                step_status_filter(fwd, true, true),
            );
            let back = step_status_filter(start, true, true);
            assert_eq!(
                step_status_filter(back, false, true),
                start,
                "back then forward from {start:?} landed on {:?}",
                step_status_filter(back, false, true),
            );
        }
    }

    /// ...while the roster keeps it, because the roster is a monitoring
    /// surface: "which of my panes died" is exactly what it is for. The two
    /// cycles share one definition and differ by one documented omission.
    #[test]
    fn the_roster_still_filters_to_exited_panes() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleRoster);
        let mut seen = Vec::new();
        for _ in 0..ROSTER_STATUS_CYCLE.len() {
            app.handle_mode_key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Tab));
            let Mode::Roster { status_filter, .. } = app.mode else { panic!("still rostering") };
            seen.push(status_filter);
        }
        assert!(seen.contains(&Some(AgentStatus::Exited)), "{seen:?}");
    }

    // -- F9: the help overlay's type-ahead filter ---------------------------

    fn help_key(app: &mut App<FakePane>, code: crossterm::event::KeyCode) {
        app.handle_mode_key(crossterm::event::KeyEvent::from(code));
    }

    fn help_type(app: &mut App<FakePane>, s: &str) {
        for c in s.chars() {
            help_key(app, crossterm::event::KeyCode::Char(c));
        }
    }

    fn open_help(app: &mut App<FakePane>) {
        app.apply(Action::Help);
        assert!(matches!(app.mode, Mode::Help { filter: None, .. }), "opens unfiltered");
    }

    /// [F9] The whole point of `Option<String>` rather than a bare `String`:
    /// until `/` is pressed the overlay behaves exactly as it did before the
    /// amendment, so every key still closes it — `/`'s neighbours included.
    #[test]
    fn the_help_overlay_is_unchanged_until_slash_opens_the_filter() {
        use crossterm::event::KeyCode;
        for code in [KeyCode::Char('a'), KeyCode::Char('q'), KeyCode::Char('?'), KeyCode::Enter] {
            let (mut app, _) = mk_app(shell_ws());
            open_help(&mut app);
            help_key(&mut app, code);
            assert!(
                matches!(app.mode, Mode::Normal),
                "{code:?} must still close an unfiltered overlay",
            );
        }
    }

    /// `/` opens the filter and typing narrows it; `q` types rather than
    /// closing, which is the trade C27's roster already makes.
    #[test]
    fn slash_opens_the_filter_and_letters_type_into_it() {
        let (mut app, _) = mk_app(shell_ws());
        open_help(&mut app);
        help_key(&mut app, crossterm::event::KeyCode::Char('/'));
        assert_eq!(app.help_filter(), Some(""), "`/` opens an empty query, not a closed one");

        help_type(&mut app, "quit");
        assert_eq!(app.help_filter(), Some("quit"), "`q` typed instead of closing");
        assert!(matches!(app.mode, Mode::Help { .. }), "still open");

        help_key(&mut app, crossterm::event::KeyCode::Backspace);
        assert_eq!(app.help_filter(), Some("qui"));
    }

    /// A second `/` is *text*, not a second open. A key that means "open"
    /// in one state and "type" in another is the ambiguity U20 already
    /// resolved for the picker.
    #[test]
    fn a_slash_inside_the_query_is_a_character() {
        let (mut app, _) = mk_app(shell_ws());
        open_help(&mut app);
        help_type(&mut app, "//");
        assert_eq!(app.help_filter(), Some("/"), "the first opened, the second typed");
    }

    /// Esc clears a query, then closes — two presses. Clearing first means
    /// a typo costs one key rather than the overlay and the place you had
    /// scrolled to.
    #[test]
    fn esc_clears_the_query_before_it_closes_the_overlay() {
        let (mut app, _) = mk_app(shell_ws());
        open_help(&mut app);
        help_type(&mut app, "/layout");
        assert_eq!(app.help_filter(), Some("layout"));

        help_key(&mut app, crossterm::event::KeyCode::Esc);
        assert_eq!(app.help_filter(), Some(""), "the first Esc clears");
        assert!(matches!(app.mode, Mode::Help { .. }), "and does not close");

        help_key(&mut app, crossterm::event::KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal), "the second closes");
    }

    /// Editing the query resets the scroll. A filtered list is a *new* list,
    /// and a `top` from the old one can point past the new one's end — C14
    /// and C27 both put the cursor at the start for the same reason.
    #[test]
    fn editing_the_query_returns_to_the_top_of_the_new_list() {
        let (mut app, _) = mk_app(shell_ws());
        open_help(&mut app);
        app.mode = Mode::Help { top: 4, filter: Some(String::new()) };
        help_type(&mut app, "t");
        assert!(matches!(app.mode, Mode::Help { top: 0, .. }), "typing went back to the top");

        app.mode = Mode::Help { top: 4, filter: Some("tab".into()) };
        help_key(&mut app, crossterm::event::KeyCode::Backspace);
        assert!(matches!(app.mode, Mode::Help { top: 0, .. }), "so did backspacing");
    }

    /// A scroll key that hits the end must **not** close while filtering.
    /// Un-filtered it does — that is C15's conditional carve-out and it is
    /// unchanged — but losing a live query to an over-pressed ↓ is exactly
    /// the "the modal you open when you are lost is the one with a
    /// surprising way out" failure C15 rejected scrolling over.
    #[test]
    fn a_dead_end_scroll_key_closes_an_unfiltered_overlay_but_not_a_filtered_one() {
        // `End` first: at 100×30 the keymap *does* scroll, so a bare ↓
        // moves and proves nothing. The dead end is the bottom of the list,
        // where the next ↓ has nowhere to go — which is the case C15's
        // conditional carve-out is actually about.
        let (mut app, _) = mk_app(shell_ws());
        open_help(&mut app);
        help_key(&mut app, crossterm::event::KeyCode::End);
        assert!(matches!(app.mode, Mode::Help { .. }), "End scrolled rather than closing");
        help_key(&mut app, crossterm::event::KeyCode::Down);
        assert!(matches!(app.mode, Mode::Normal), "unfiltered: ↓ that moved nothing closes");

        let (mut app, _) = mk_app(shell_ws());
        open_help(&mut app);
        help_type(&mut app, "/tab");
        help_key(&mut app, crossterm::event::KeyCode::End);
        help_key(&mut app, crossterm::event::KeyCode::Down);
        assert!(
            matches!(app.mode, Mode::Help { filter: Some(_), .. }),
            "filtering: ↓ that moved nothing must keep the query",
        );
    }

    /// [C39] Delete must not close a live query — a reader reaching for it
    /// is erasing a typo, and losing the overlay is the sharpest possible
    /// answer. Enter and Tab *do* close, deliberately: neither has a
    /// meaning here (the filtered list has no cursor to act on), so they
    /// read as "done", and the alternative is a key that silently does
    /// nothing on a surface whose whole point is answering questions.
    #[test]
    fn delete_edits_the_query_while_enter_and_tab_deliberately_close() {
        use crossterm::event::KeyCode;
        let (mut app, _) = mk_app(shell_ws());
        open_help(&mut app);
        help_type(&mut app, "/tab");
        help_key(&mut app, KeyCode::Delete);
        assert_eq!(app.help_filter(), Some("tab"), "Delete left the query alone…");
        assert!(matches!(app.mode, Mode::Help { .. }), "…and did not close the overlay");

        for code in [KeyCode::Enter, KeyCode::Tab] {
            let (mut app, _) = mk_app(shell_ws());
            open_help(&mut app);
            help_type(&mut app, "/tab");
            help_key(&mut app, code);
            assert!(matches!(app.mode, Mode::Normal), "{code:?} closes, by contract");
        }
    }

    /// `j`/`k` scroll while the filter is closed and type while it is open.
    /// The same trade C27's roster makes for every letter.
    #[test]
    fn j_and_k_become_query_text_once_the_filter_is_open() {
        let (mut app, _) = mk_app(shell_ws());
        open_help(&mut app);
        help_type(&mut app, "/jk");
        assert_eq!(app.help_filter(), Some("jk"));
    }

    /// C27: the roster's scroll clamp, tested directly rather than through
    /// a caller that applies it and then asserts its postcondition.
    ///
    /// `roster_window_follows_the_cursor_and_clamps` used to carry
    /// `top + height <= rows.len()` after calling `roster_view`, which
    /// clamps — so the assertion restated the clamp it had just run. That
    /// left the clamp itself, including its `saturating_sub` edge, with no
    /// test at all: the tautology had been standing in for one.
    #[test]
    fn the_roster_scroll_clamp_keeps_the_window_inside_the_list() {
        // The ordinary case: a window smaller than the list.
        assert_eq!(roster_top_clamped(0, 20, 5), 0, "the top of a long list stays put");
        assert_eq!(roster_top_clamped(15, 20, 5), 15, "the last legal top is reachable");
        assert_eq!(roster_top_clamped(99, 20, 5), 15, "past the end clamps to it");
        for top in 0..30 {
            let t = roster_top_clamped(top, 20, 5);
            assert!(t + 5 <= 20, "top {top} clamped to {t}, which runs past the end");
        }

        // The edge the `saturating_sub` exists for: a window taller than
        // the list. There is nothing to scroll, so the only legal top is 0
        // — an underflow here would wrap to usize::MAX and scroll a short
        // roster off its own bottom.
        assert_eq!(roster_top_clamped(0, 3, 10), 0);
        assert_eq!(roster_top_clamped(7, 3, 10), 0, "a taller window pins the top at 0");
        assert_eq!(roster_top_clamped(1, 0, 10), 0, "and an empty list has nowhere to go");
    }

    /// The flash resolves its chord through the live keymap. A literal
    /// would survive a remap and teach a key that no longer exists — the
    /// drift C34 was built to end, and the reason `flash_move_edge` goes
    /// through `chord_clause`.
    #[test]
    fn the_edge_flash_follows_a_remapped_cross_tab_chord() {
        use crate::core::layout::Dir;
        let (mut app, _) = mk_app(shell_ws());
        let (keymap, diags) =
            Keymap::parse(r#"{"keys": {"alt+shift+m": "disable"}}"#, "config.json");
        assert!(diags.is_empty(), "{diags:?}");
        app.set_keymap(keymap);
        app.apply(Action::NewPane);

        app.apply(Action::MovePane(Dir::Right));
        let flash = app.flash().expect("the edge still speaks").to_string();
        assert_eq!(
            flash, "at the tab's edge",
            "with the chord unbound the clause collapses rather than naming a dead key",
        );
    }

    /// C38: a refused split says why, and names the threshold — the one
    /// fact that makes the next attempt succeed. `spawn_child` has always
    /// refused a split that would produce unusable panes (it is also the
    /// vt100 underflow trigger); until now the sixth `Alt+n` on a small
    /// terminal simply did nothing.
    #[test]
    fn a_refused_split_says_why_and_names_the_threshold() {
        let (mut app, _) = mk_app(shell_ws());
        // Split until the layout refuses, exactly as a user leaning on
        // Alt+n would.
        while app.spawn_child("shell", None, None).is_some() {}
        app.set_flash("stale");

        app.apply(Action::NewPane);
        let flash = app.flash().expect("the refusal speaks").to_string();
        assert!(flash.contains("no room"), "{flash}");
        // The axis `split_fit` actually tested, with the number it missed by
        // — not both dimensions, which is a rule the guard does not have (a
        // 30×30 pane splits fine into two 30-wide halves).
        let vertical = flash.contains("columns");
        let want = if vertical { layout::MIN_SPLIT_COLS } else { layout::MIN_SPLIT_ROWS };
        assert!(flash.contains(&want.to_string()), "the threshold is actionable: {flash}");
        assert!(
            flash.contains("columns") != flash.contains("rows"),
            "exactly one axis is named — the one that failed: {flash}",
        );
    }

    /// ...and a split that *works* says nothing. A flash on every Alt+n
    /// would be noise, and the pane appearing is its own feedback.
    #[test]
    fn a_split_that_succeeds_stays_quiet() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        assert!(app.flash().is_none(), "{:?}", app.flash());
    }

    /// Inside a stack the gesture reorders — no special case in the app, it
    /// falls out of every stack member owning its own `PaneRect`. The moved
    /// pane must still be the expanded one afterwards (layout's
    /// `expanded`-follows-the-swap rule), or "move down" would collapse the
    /// pane you are reading.
    #[test]
    fn moving_a_pane_inside_a_stack_reorders_it_and_keeps_it_expanded() {
        use crate::core::layout::Dir;
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::NewPane); // three panes
        app.apply(Action::ToggleStack); // ... collapsed into one stack
        let moved = app.focused;

        let order_before: Vec<_> = app.rects().iter().map(|p| p.id).collect();
        let i_before = order_before.iter().position(|&id| id == moved).unwrap();
        assert!(i_before > 0, "this fixture needs a member above the focused one");

        app.apply(Action::MovePane(Dir::Up));

        let order_after: Vec<_> = app.rects().iter().map(|p| p.id).collect();
        let i_after = order_after.iter().position(|&id| id == moved).unwrap();
        assert_eq!(i_after, i_before - 1, "the pane moved one slot up the stack");
        assert_eq!(app.focused, moved, "focus stayed on it");
        // It is also still the *expanded* member, so "move up" never
        // collapses the pane you are reading. Note this assertion cannot
        // fail on its own: `move_pane_dir`'s `expand_in_stacks` call would
        // re-expand the focused pane even if `swap_panes` had left the
        // index behind (verified by mutation — this test stays green with
        // layout's rule removed). The rule itself is pinned where it lives,
        // by `layout::tests::swapping_inside_one_stack_carries_the_expanded_
        // slot_with_the_pane`; this line pins the *user-visible* outcome
        // through whichever of the two produces it.
        assert!(
            !app.rects().iter().find(|p| p.id == moved).unwrap().collapsed,
            "the moved pane is the expanded member",
        );
    }

    /// C21/C22: a swap is a structural layout edit, so it leaves zoom the
    /// way `FlipSplit`/`Resize`/`CycleLayout` do — the tab must never change
    /// invisibly behind a full-screen pane.
    #[test]
    fn moving_a_pane_leaves_zoom_first() {
        use crate::core::layout::Dir;
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::ToggleZoom);
        assert!(app.zoomed());
        app.apply(Action::MovePane(Dir::Left));
        assert!(!app.zoomed(), "a structural edit exits zoom");
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
                PaneSpec {
                    adapter: "shell".into(),
                    cwd: "/tmp".into(),
                    session: None,
                    title: None,
                    spawned_by: None,
                    note: None,
                    noted_at: None,
                },
            );
        }
        let layout = LayoutNode::Stack { children: vec![1, 2, 3], expanded: 0 };
        let ws = Workspace {
            version: 1,
            active_tab: 0,
            tabs: vec![Tab { name: "main".into(), layout, panes }],
        };
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
            Action::CycleLayout { forward: true },
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

    // ---- C31: cross-tab directional focus at an edge ---------------------

    /// Two tabs, each a horizontal row of panes — tab 0 is `[1, 2]`, tab 1
    /// is `[10, 11, 12]`. Every id is DFS-first-vs-leftmost-vs-rightmost
    /// distinguishable, so a test can tell "landed on the geometric edge"
    /// apart from "landed on the tab's first/remembered pane" (U11).
    fn two_tab_row_ws() -> Workspace {
        fn row(ids: &[PaneId]) -> (LayoutNode, HashMap<PaneId, PaneSpec>) {
            let mut panes = HashMap::new();
            for &id in ids {
                panes.insert(
                    id,
                    PaneSpec {
                        adapter: "shell".into(),
                        cwd: "/tmp".into(),
                        session: None,
                        title: None,
                        spawned_by: None,
                        note: None,
                        noted_at: None,
                    },
                );
            }
            let children = ids.iter().map(|&id| LayoutNode::Pane(id)).collect::<Vec<_>>();
            let n = children.len() as f32;
            let layout = LayoutNode::Split {
                dir: SplitDir::Vertical,
                ratios: vec![1.0 / n; children.len()],
                children,
            };
            (layout, panes)
        }
        let (layout0, panes0) = row(&[1, 2]);
        let (layout1, panes1) = row(&[10, 11, 12]);
        Workspace {
            version: 1,
            active_tab: 0,
            tabs: vec![
                Tab { name: "t0".into(), layout: layout0, panes: panes0 },
                Tab { name: "t1".into(), layout: layout1, panes: panes1 },
            ],
        }
    }

    /// C31: `Alt+Right` at a tab's rightmost pane continues into the next
    /// tab, landing on *its* leftmost pane — not wherever that tab was last
    /// left (contrast U11 rule 2's `tab_focus`).
    #[test]
    fn cross_tab_right_edge_lands_on_next_tabs_leftmost_pane() {
        let (mut app, store) = mk_app(two_tab_row_ws());
        app.set_focus(2); // tab 0's rightmost pane — nothing further right in-tab
        app.apply(Action::Focus(layout::Dir::Right));
        assert_eq!(app.ws.active_tab, 1);
        assert_eq!(app.focused, 10, "lands on the destination's leftmost pane");
        let saved = store.0.lock().unwrap().clone().unwrap();
        assert_eq!(saved.active_tab, 1, "the tab switch persists, same as Alt+i/Alt+m");
    }

    /// The mirror: `Alt+Left` at a tab's leftmost pane continues into the
    /// *previous* tab, landing on *its* rightmost pane — "keep going left"
    /// arrives nearest the edge just crossed.
    #[test]
    fn cross_tab_left_edge_lands_on_previous_tabs_rightmost_pane() {
        let (mut app, _) = mk_app(two_tab_row_ws());
        app.apply(Action::GoToTab(1));
        assert_eq!(app.focused, 10, "tab 1's leftmost pane (also its DFS-first here)");
        app.apply(Action::Focus(layout::Dir::Left));
        assert_eq!(app.ws.active_tab, 0);
        assert_eq!(app.focused, 2, "lands on the previous tab's rightmost pane, not its first (1)");
    }

    /// Wraps at both ends, exactly like `Alt+i`/`Alt+m` — two ways of moving
    /// between tabs disagreeing about hitting an end would be its own bug.
    #[test]
    fn cross_tab_focus_wraps_at_both_ends() {
        let (mut app, _) = mk_app(shell_ws());
        for _ in 0..2 {
            app.apply(Action::NewTab); // three tabs: 0, 1, 2 — one pane each
        }
        assert_eq!(app.ws.active_tab, 2);
        let last_tabs_pane = app.focused;

        app.apply(Action::Focus(layout::Dir::Right)); // off the right edge of the last tab
        assert_eq!(app.ws.active_tab, 0, "wraps forward past the last tab");

        app.apply(Action::Focus(layout::Dir::Left)); // off the left edge of the first tab
        assert_eq!(app.ws.active_tab, 2, "wraps backward past the first tab");
        assert_eq!(
            app.focused, last_tabs_pane,
            "back on the tab it started from, and its only pane"
        );
    }

    /// Below two tabs this is exactly the pre-existing dead end: `neighbor`
    /// finds nothing and C31 declines to fire at all.
    ///
    /// F7: the same key teleports across tabs at a real edge, so a refusal
    /// here must be visible, not silent — roost's own rule is every no-op
    /// flashes. Reuses `move_pane_to_tab`'s identical "only one tab"
    /// wording (its `n < 2` refusal is the same shape) so the two read as
    /// one rule instead of two.
    #[test]
    fn cross_tab_focus_is_a_no_op_with_only_one_tab() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1|2, focus=2 (rightmost, one tab)
        app.apply(Action::Focus(layout::Dir::Right));
        assert_eq!(app.ws.tabs.len(), 1);
        assert_eq!(app.focused, 2, "a lone tab's edge stays a dead end");
        assert_eq!(app.flash(), Some("only one tab"), "the no-op must be visible");
    }

    /// Design decision: only `Left`/`Right` pick up tab-switching semantics.
    /// Tabs are roost's own horizontal axis, so `Up`/`Down` at an edge must
    /// stay the dead end they always were, even with other tabs to reach.
    #[test]
    fn cross_tab_focus_never_fires_for_up_or_down() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewTab); // two tabs, one pane each — every direction is an edge
        let (tab_before, pane_before) = (app.ws.active_tab, app.focused);
        app.apply(Action::Focus(layout::Dir::Up));
        assert_eq!(app.ws.active_tab, tab_before, "Up must never switch tabs, even at an edge");
        assert_eq!(app.focused, pane_before);
        // F7's flash is scoped to Left/Right's new cross-tab refusals only —
        // Up/Down's dead end predates C31 and is out of this fix's scope.
        assert_eq!(app.flash(), None);
        app.apply(Action::Focus(layout::Dir::Down));
        assert_eq!(app.ws.active_tab, tab_before, "…nor Down");
        assert_eq!(app.focused, pane_before);
        assert_eq!(app.flash(), None);
    }

    /// `neighbor == None` is not the same as "at the tab's edge": a pane
    /// spanning the tab's full width (a row above/below a split row) has no
    /// Left *or* Right neighbour, but is not meaningfully "the last or
    /// first pane" the client asked about. Neither key may leave the tab
    /// from it; a pane that genuinely owns the tab's edge, in that same
    /// layout, still does.
    ///
    /// F7: the refusal above must flash — a silent one would be
    /// indistinguishable from Alt+→ simply not being bound, on the one key
    /// that visibly does something everywhere else in the same tab.
    #[test]
    fn cross_tab_focus_ignores_a_full_width_pane_thats_not_the_tabs_only_pane() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewTab); // a second tab, so a wrong trigger has somewhere to go
        app.apply(Action::GoToTab(0));
        // The audit's repro: one pane spanning the row above a split row.
        app.apply(Action::NewPane);
        app.apply(Action::FlipSplit);
        app.apply(Action::NewPane);
        let bottom_right = app.focused; // genuinely at the right edge — the sanity check below
        let tab_before = app.ws.active_tab;

        app.set_focus(1); // the full-width top pane: touches both x edges, but shares the tab
        app.apply(Action::Focus(layout::Dir::Right));
        assert_eq!(
            app.ws.active_tab, tab_before,
            "full width but not the tab's only pane — not an edge"
        );
        assert_eq!(app.focused, 1);
        assert_eq!(app.flash(), Some("full-width pane — nothing to cross into"));
        app.apply(Action::Focus(layout::Dir::Left));
        assert_eq!(app.ws.active_tab, tab_before);
        assert_eq!(app.focused, 1);
        assert_eq!(app.flash(), Some("full-width pane — nothing to cross into"));

        // Sanity: a pane that genuinely owns the edge, in this same mixed
        // layout, is unaffected by the fix above.
        app.set_focus(bottom_right);
        app.apply(Action::Focus(layout::Dir::Right));
        assert_ne!(app.ws.active_tab, tab_before, "a real edge still crosses");
    }

    /// Landing on a stack's topmost member would collapse whatever the tab
    /// had expanded — a navigation
    /// key must not rearrange a tab you haven't looked at yet. `edge_pane`
    /// prefers the *expanded* member on an x-tie, so the jump lands on
    /// what was already visible and `expand_in_stacks` is a no-op: the
    /// stack's shape is untouched.
    #[test]
    fn cross_tab_focus_lands_on_the_already_expanded_stack_member_at_the_destination() {
        fn spec() -> PaneSpec {
            PaneSpec {
                adapter: "shell".into(),
                cwd: "/tmp".into(),
                session: None,
                title: None,
                spawned_by: None,
                note: None,
                noted_at: None,
            }
        }
        let mut panes0 = HashMap::new();
        panes0.insert(1, spec());
        let mut panes1 = HashMap::new();
        for id in [20u64, 21, 22, 23] {
            panes1.insert(id, spec());
        }
        let ws = Workspace {
            version: 1,
            active_tab: 0,
            tabs: vec![
                Tab { name: "t0".into(), layout: LayoutNode::Pane(1), panes: panes0 },
                Tab {
                    name: "t1".into(),
                    layout: LayoutNode::Split {
                        dir: SplitDir::Vertical,
                        ratios: vec![0.5, 0.5],
                        children: vec![
                            LayoutNode::Pane(20),
                            LayoutNode::Stack { children: vec![21, 22, 23], expanded: 1 }, // 22 expanded
                        ],
                    },
                    panes: panes1,
                },
            ],
        };
        let (mut app, _) = mk_app(ws);

        // Tab 0's only pane has no left neighbour — wraps to tab 1
        // (previous, n=2), landing on its rightmost pane: the stack column,
        // tie-broken to the *expanded* member (22), not the topmost (21).
        app.apply(Action::Focus(layout::Dir::Left));

        assert_eq!(app.ws.active_tab, 1);
        assert_eq!(app.focused, 22, "lands on the member already visible, not the topmost");
        match &app.ws.tabs[1].layout {
            LayoutNode::Split { children, .. } => match &children[1] {
                LayoutNode::Stack { expanded, children } => {
                    assert_eq!(
                        children[*expanded], 22,
                        "the stack's shape is untouched — still 22 expanded"
                    )
                }
                other => panic!("expected the stack, got {other:?}"),
            },
            other => panic!("expected the split, got {other:?}"),
        }
    }

    /// C21: a cross-tab jump is a tab change like any other, so it exits
    /// zoom — the same rule `go_to_tab`/C28/`Alt+a`'s cross-tab jump follow.
    #[test]
    fn cross_tab_focus_exits_zoom_like_any_other_tab_change() {
        let (mut app, _) = mk_app(two_tab_row_ws());
        app.set_focus(2); // tab 0's rightmost pane
        app.apply(Action::ToggleZoom);
        assert!(app.zoomed());
        app.apply(Action::Focus(layout::Dir::Right));
        assert_eq!(app.ws.active_tab, 1, "the jump happened");
        assert!(!app.zoomed(), "…and it must exit zoom, exactly like any other tab change");
    }

    /// C22 rule 2 is untouched: the float has no spatial position, so
    /// `focus_dir` returns before ever reaching C31 — the active tab must
    /// never change while what's being left is the float, even with more
    /// than one tab around to (wrongly) jump to.
    #[test]
    fn float_rule2_focus_dir_does_not_cross_tabs_even_with_more_than_one() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewTab); // two tabs, active = 1
        let real_pane = app.focused;
        let tab_before = app.ws.active_tab;
        app.apply(Action::ToggleFloat);
        app.apply(Action::Focus(layout::Dir::Right));
        assert_eq!(app.focused, real_pane, "rule 2: back to prev_focus, not a cross-tab jump");
        assert_eq!(app.ws.active_tab, tab_before, "C31 must never fire while leaving the float");
        assert!(!app.float.as_ref().unwrap().shown);
    }

    // ---- C28: move the focused pane between tabs -------------------------

    /// The core of it: the pane changes tabs, focus follows it there, and
    /// **the process is untouched** — same runtime object, so the agent keeps
    /// its PTY, its scrollback and its session across the move. That is the
    /// whole reason the chord exists rather than "close it and start another
    /// one over there".
    #[test]
    fn move_pane_to_tab_carries_the_pane_and_the_focus_without_respawning() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // tab 1: panes 1|2, focus 2
        app.apply(Action::NewTab); // tab 2: pane 3, focus 3
        app.apply(Action::GoToTab(0));
        app.set_focus(2);
        // Stamp the live runtime so "the same process" is provable rather
        // than assumed: a respawn would hand back a fresh FakePane with an
        // empty input log.
        app.runtimes.get_mut(&2).expect("pane 2 is running").write_input(b"mid-turn");

        app.apply(Action::MovePaneToTab { forward: true });

        assert_eq!(app.ws.active_tab, 1, "focus followed the pane to tab 2");
        assert_eq!(app.focused, 2);
        assert!(!app.ws.tabs[0].panes.contains_key(&2), "gone from the source tab");
        assert!(app.ws.tabs[1].panes.contains_key(&2), "and arrived in the destination");
        assert!(app.pane_order().contains(&2), "…in its layout tree, not just its map");
        assert_eq!(
            app.runtimes.get(&2).map(|rt| rt.input.clone()),
            Some(b"mid-turn".to_vec()),
            "the same runtime object — a move must never respawn the agent",
        );
        assert!(app.ws.tabs[0].panes.contains_key(&1), "the source tab's other pane stayed");
    }

    /// It wraps at both ends, exactly like the `Alt+i`/`Alt+m` pair it is
    /// the shifted sibling of.
    #[test]
    fn move_pane_to_tab_wraps_at_both_ends() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        for _ in 0..2 {
            app.apply(Action::NewTab); // three tabs
        }
        app.apply(Action::GoToTab(0));
        app.set_focus(2);
        app.apply(Action::MovePaneToTab { forward: false });
        assert_eq!(app.ws.active_tab, 2, "moving back from the first tab wraps to the last");
        assert!(app.ws.tabs[2].panes.contains_key(&2));
        app.apply(Action::MovePaneToTab { forward: true });
        assert_eq!(app.ws.active_tab, 0, "…and forward from the last wraps to the first");
        assert!(app.ws.tabs[0].panes.contains_key(&2));
    }

    /// Moving a tab's *last* pane away removes the tab — `close_pane_id`'s
    /// rule, since a tab with no panes is not a thing roost can draw. The
    /// destination index has to survive the tabs below it shifting down.
    #[test]
    fn moving_a_tabs_last_pane_away_removes_the_tab_and_still_lands_right() {
        let (mut app, _) = mk_app(shell_ws());
        for _ in 0..2 {
            app.apply(Action::NewTab); // tabs: [1] [2] [3], one pane each
        }
        app.apply(Action::GoToTab(0)); // sitting on the first tab's only pane
        let moved = app.focused;

        app.apply(Action::MovePaneToTab { forward: true });

        assert_eq!(app.ws.tabs.len(), 2, "the emptied source tab went with it");
        assert_eq!(app.ws.active_tab, 0, "the destination shifted down into index 0");
        assert_eq!(app.ws.tabs[0].name, "tab2", "…and it really is the tab we aimed at");
        assert!(app.ws.tabs[0].panes.contains_key(&moved));
        assert_eq!(app.focused, moved);
        // No undo record: the pane is alive over here, and resurrecting the
        // tab around it would duplicate it.
        assert!(!app.undo.iter().any(|c| matches!(c, Closed::Tab { .. })), "a move is not a close",);
    }

    /// Both refusals are flashed, not silent, and neither disturbs anything:
    /// the C22 float belongs to no tab, and a lone tab has nowhere to send.
    #[test]
    fn move_pane_to_tab_refuses_visibly_on_the_float_and_on_a_lone_tab() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::MovePaneToTab { forward: true });
        assert_eq!(app.ws.tabs.len(), 1, "nothing happened");
        assert!(app.flash.as_ref().is_some_and(|(m, ..)| m.contains("one tab")), "{:?}", app.flash);

        app.apply(Action::NewTab);
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        app.apply(Action::MovePaneToTab { forward: true });
        assert_eq!(app.focused, float_id, "the float is still where it was");
        assert!(app.is_float(float_id));
        assert!(app.flash.as_ref().is_some_and(|(m, ..)| m.contains("scratch")), "{:?}", app.flash);
    }

    /// C21: a move is a tab change, so it exits zoom — the invariant that
    /// keeps the layout from changing invisibly behind a full-screen pane.
    #[test]
    fn move_pane_to_tab_exits_zoom_like_any_other_tab_change() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::NewTab);
        app.apply(Action::GoToTab(0));
        app.set_focus(2);
        app.apply(Action::ToggleZoom);
        assert!(app.zoomed());
        app.apply(Action::MovePaneToTab { forward: true });
        assert!(!app.zoomed(), "the tab changed under the zoom");
    }

    /// A destination with no room refuses **before** anything moves: the
    /// pane must never be stranded between two tabs by a split the
    /// destination cannot legally take (C25's `MIN_SPLIT_*` floor).
    #[test]
    fn move_pane_to_tab_refuses_a_destination_with_no_room_without_moving_anything() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewTab);
        // Fill tab 2 until one more split would break the floor.
        while app.spawn_child("shell", None, None).is_some() {}
        let packed: Vec<PaneId> = app.pane_order();
        assert!(packed.len() > 1, "the destination is genuinely full: {packed:?}");
        app.apply(Action::GoToTab(0));
        let moved = app.focused;

        app.apply(Action::MovePaneToTab { forward: true });

        assert_eq!(app.ws.active_tab, 0, "we did not move");
        assert!(app.ws.tabs[0].panes.contains_key(&moved), "…and neither did the pane");
        assert_eq!(app.pane_order(), vec![moved]);
        assert_eq!(app.ws.tabs[1].panes.len(), packed.len(), "the destination is unchanged");
        assert!(app.flash.as_ref().is_some_and(|(m, ..)| m.contains("no room")), "{:?}", app.flash);
    }

    // ---- C40: mark and pull ---------------------------------------------

    /// The whole point of C40 over C28: the destination is arbitrary, so a
    /// pane four tabs away arrives in one gesture rather than four presses
    /// — and it arrives *alive*, same as a move (same runtime, no respawn).
    #[test]
    fn a_marked_pane_is_pulled_into_whatever_tab_is_active_however_far_away() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // tab 1: panes 1|2
        for _ in 0..4 {
            app.apply(Action::NewTab); // tabs 2..5, one pane each
        }
        app.apply(Action::GoToTab(0));
        app.set_focus(2);
        app.runtimes.get_mut(&2).expect("pane 2 is running").write_input(b"mid-turn");

        app.apply(Action::MarkPane);
        app.apply(Action::GoToTab(4)); // four tabs away, no walking
        app.apply(Action::PullPane);

        assert_eq!(app.ws.active_tab, 4, "the pull never leaves the tab you are on");
        assert_eq!(app.focused, 2, "focus follows the pane, exactly like a move");
        assert!(app.ws.tabs[4].panes.contains_key(&2), "it arrived");
        assert!(app.pane_order().contains(&2), "…in the layout tree, not just the map");
        assert!(!app.ws.tabs[0].panes.contains_key(&2), "and left the tab it was marked in");
        assert_eq!(
            app.runtimes.get(&2).map(|rt| rt.input.clone()),
            Some(b"mid-turn".to_vec()),
            "the same runtime object — a pull must never respawn the agent",
        );
        assert_eq!(app.marked(), None, "the gesture is over, so the mark is too");
    }

    /// One chord both ways, like every other toggle roost binds — and the
    /// unmark really disarms the pull rather than just clearing a label.
    #[test]
    fn marking_the_marked_pane_unmarks_it() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::NewTab);
        app.apply(Action::GoToTab(0));
        let id = app.focused;

        app.apply(Action::MarkPane);
        assert_eq!(app.marked(), Some(id));
        app.apply(Action::MarkPane);
        assert_eq!(app.marked(), None);

        app.apply(Action::GoToTab(1));
        app.apply(Action::PullPane);
        assert!(!app.pane_order().contains(&id), "an unmarked pane stays put");
        assert!(app.flash.as_ref().is_some_and(|(m, ..)| m.contains("nothing marked")));
    }

    /// Every outcome that resolves the gesture says so out loud (C10: a
    /// no-op you can see beats one you can't) and clears the mark, so the
    /// hint bar's standing pair never outlives it.
    #[test]
    fn pull_flashes_and_clears_when_the_mark_cannot_be_honoured() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        let id = app.focused;

        // Marked, then pulled into the tab it already lives in.
        app.apply(Action::MarkPane);
        app.apply(Action::PullPane);
        assert_eq!(app.ws.tabs.len(), 1, "nothing moved");
        assert!(app.flash.as_ref().is_some_and(|(m, ..)| m.contains("already in this tab")));
        assert_eq!(app.marked(), None);

        // The C22 float belongs to no tab, so there is no tab to pull it
        // out of — C28's refusal, verbatim.
        app.apply(Action::ToggleFloat);
        app.apply(Action::MarkPane);
        assert_eq!(app.marked(), None, "the float is never marked");
        assert!(app.flash.as_ref().is_some_and(|(m, ..)| m.contains("scratch")));
        app.apply(Action::ToggleFloat);
        assert_eq!(app.focused, id);
    }

    /// A destination with no room refuses the pull the way it refuses a
    /// move — and the mark **stands**, because that is the one refusal the
    /// user can fix (close something) and retry.
    #[test]
    fn a_refused_pull_keeps_the_mark() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewTab);
        app.apply(Action::GoToTab(0));
        let marked = app.focused;
        app.apply(Action::MarkPane);

        app.apply(Action::GoToTab(1));
        while app.spawn_child("shell", None, None).is_some() {} // fill it
        let packed = app.pane_order().len();
        app.apply(Action::PullPane);

        assert_eq!(app.pane_order().len(), packed, "the destination is unchanged");
        assert!(app.ws.tabs[0].panes.contains_key(&marked), "…and so is the source");
        assert!(app.flash.as_ref().is_some_and(|(m, ..)| m.contains("no room")), "{:?}", app.flash);
        assert_eq!(app.marked(), Some(marked), "still armed for a retry");
    }

    /// Pane ids are recycled (`next_pane_id` is max+1), so a mark left
    /// pointing at a closed pane would not merely be dead — the next pane
    /// to take that number would be pulled in its place.
    #[test]
    fn closing_the_marked_pane_clears_the_mark() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewTab); // tab 2, pane 2 — the tab survives the close
        app.apply(Action::NewPane); // pane 3, the highest id: closing it is
        let marked = app.focused; // what actually frees the number for reuse
        app.apply(Action::MarkPane);
        assert_eq!(app.marked(), Some(marked));

        app.apply(Action::ClosePane);

        assert_eq!(app.marked(), None);
        let recycled = app.spawn_child("shell", None, None).expect("room for one more");
        assert_eq!(recycled, marked, "the id really was recycled — this is the hazard");
        app.apply(Action::GoToTab(0));
        app.apply(Action::PullPane);
        assert!(!app.pane_order().contains(&recycled), "the stranger was not pulled");
        assert!(app.flash.as_ref().is_some_and(|(m, ..)| m.contains("nothing marked")));
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
        let reply = app
            .handle_control(Request { token: ct, method: Method::Close { pane: 1, force: false } });
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

        app.apply(Action::CycleLayout { forward: true });
        assert!(matches!(
            app.ws.tabs[0].layout,
            LayoutNode::Split { dir: SplitDir::Horizontal, .. }
        ));

        app.apply(Action::CycleLayout { forward: true });
        match &app.ws.tabs[0].layout {
            LayoutNode::Split { ratios, .. } => assert_eq!(ratios, &vec![0.6, 0.4]),
            other => panic!("expected a main+stack split, got {other:?}"),
        }

        app.apply(Action::CycleLayout { forward: true });
        assert!(matches!(app.ws.tabs[0].layout, LayoutNode::Stack { .. }));

        app.apply(Action::CycleLayout { forward: true }); // wraps back to grid
        assert!(matches!(
            app.ws.tabs[0].layout,
            LayoutNode::Split { dir: SplitDir::Horizontal, .. }
        ));
    }

    #[test]
    fn cycle_layout_skips_an_unfit_arrangement_and_applies_the_next() {
        let (mut app, _) = mk_app(shell_ws());
        app.on_resize(Size::new(80, 22), (0, 0)); // body ~80x20: main+stack's 0.4 column (32) < the 36 floor
        app.apply(Action::NewPane); // 2 panes

        app.apply(Action::CycleLayout { forward: true }); // 1st: grid (0.5/0.5) fits
        match &app.ws.tabs[0].layout {
            LayoutNode::Split { ratios, .. } => assert_eq!(ratios, &vec![0.5, 0.5]),
            other => panic!("expected grid (0.5/0.5 split), got {other:?}"),
        }

        app.apply(Action::CycleLayout { forward: true }); // 2nd: main+stack unfit here → skipped → all-stack applied
        assert!(matches!(app.ws.tabs[0].layout, LayoutNode::Stack { .. }));
    }

    #[test]
    fn cycle_layout_noop_when_fewer_than_two_panes() {
        let (mut app, _) = mk_app(shell_ws()); // 1 pane
        app.apply(Action::CycleLayout { forward: true });
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
        app.apply(Action::CycleLayout { forward: true });
        assert_eq!(format!("{:?}", app.ws.tabs[0].layout), before, "layout must be untouched");
        assert_eq!(app.flash(), Some("no room to rearrange; stack a pane with Alt+s first"));
    }

    #[test]
    fn cycle_layout_preserves_focus() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::NewPane);
        let focused = app.focused;
        for _ in 0..3 {
            app.apply(Action::CycleLayout { forward: true });
            assert_eq!(app.focused, focused);
        }
    }

    #[test]
    fn cycle_layout_persists_like_any_layout_edit() {
        let (mut app, store) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::CycleLayout { forward: true });
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
        let restored =
            app.ws.tabs.iter().find(|t| t.name == "fleet").expect("tab reopened by name");
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
            app.push_feed(format!("entry {i}"), false, None);
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
            note: None,
            noted_at: None,
        };
        assert_eq!(display_name_live(&spec, None), "pi · rqa-work");
        spec.title = Some("worker1".into());
        assert_eq!(display_name_live(&spec, None), "worker1");
        // A cwd with no final component (e.g. `/`) degrades to the bare
        // adapter, no dangling separator.
        spec.title = None;
        spec.cwd = PathBuf::from("/");
        assert_eq!(display_name_live(&spec, None), "pi");
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
            note: None,
            noted_at: None,
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
            note: None,
            noted_at: None,
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
    /// once per change, through the same queue as every other host write,
    /// and led by the pane id exactly like the badge.
    #[test]
    fn host_title_follows_focus_and_live_title() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // panes 1 | 2, focus 2

        let want = format!("\x1b]2;roost · {}\x07", app.feed_label(2));
        assert!(host_contains(&mut app, want.as_bytes()), "first frame publishes a title");
        // Unchanged ⇒ nothing republished (the throttle's companion rule).
        app.last_host_title = None;
        assert!(!host_contains(&mut app, b"\x1b]2;"));

        // The id is what makes the title usable: both fixture panes are
        // `shell · tmp`, so a name alone could not say which is focused —
        // and a focus move between them would publish nothing at all.
        app.focused = 1;
        app.last_host_title = None;
        assert!(
            host_contains(&mut app, b"\x1b]2;roost \xc2\xb7 1 shell \xc2\xb7 tmp\x07"),
            "a focus move between same-named panes still renames the window"
        );

        // A rename of the focused pane takes the same path — as does a live
        // OSC title, since both resolve through `display_name`.
        app.find_spec_mut(1).unwrap().title = Some("TASK-8".into());
        app.last_host_title = None;
        assert!(host_contains(&mut app, b"\x1b]2;roost \xc2\xb7 1 TASK-8\x07"));
    }

    /// H1: `spec.title` reaches `sync_host_title` verbatim from
    /// `display_name_live` (an Alt+r rename, but also a hand-edited
    /// `title` field in workspace.json) — a hostile one must not carry its
    /// own OSC/BEL out through roost's own escape sequence to the host's
    /// real terminal, since that write bypasses ratatui entirely (W3).
    #[test]
    fn host_title_strips_control_bytes_from_a_hostile_pane_title() {
        let (mut app, _) = mk_app(shell_ws());
        app.find_spec_mut(1).unwrap().title = Some("safe\x1b]52;c;cGduZWQ=\x07evil".into());
        app.last_host_title = None;
        let out = app.take_host_output();
        // Exactly roost's own introducer/terminator — the pane-supplied
        // ESC/BEL embedded in the title must not add a second pair.
        assert_eq!(out.iter().filter(|&&b| b == 0x1b).count(), 1);
        assert_eq!(out.iter().filter(|&&b| b == 0x07).count(), 1);
        assert_eq!(out, b"\x1b]2;roost \xc2\xb7 1 safe]52;c;cGduZWQ=evil\x07".to_vec());
    }

    /// H1: the same escape, via the *other* unsanitized fallback in
    /// `display_name_live` — an untitled shell pane's `cwd.file_name()`,
    /// which an in-pane agent fully controls via `roost spawn --cwd
    /// '<hostile dir name>'`.
    #[test]
    fn host_title_strips_control_bytes_from_a_hostile_cwd_tag() {
        let (mut app, _) = mk_app(shell_ws());
        app.find_spec_mut(1).unwrap().cwd = PathBuf::from("/tmp/evil\x1b]52;c;AA==\x07dir");
        app.last_host_title = None;
        let out = app.take_host_output();
        assert_eq!(out.iter().filter(|&&b| b == 0x1b).count(), 1);
        assert_eq!(out.iter().filter(|&&b| b == 0x07).count(), 1);
        assert_eq!(out, b"\x1b]2;roost \xc2\xb7 1 shell \xc2\xb7 evil]52;c;AA==dir\x07".to_vec());
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
        assert!(host_contains(&mut app, b"\x1b]2;roost \xc2\xb7 1 second\x07"));
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
        let msg = app.on_status(1, AgentStatus::NeedsInput, None);
        assert_eq!(msg.as_deref(), Some("shell · tmp is waiting for you"));
    }

    /// A replayed needs_input (same status, same question — what an
    /// extension's reconnect replay sends after a roost restart or a
    /// transient drop) must not re-ring the desktop; a genuinely new
    /// question while already ◆ still must.
    #[test]
    fn repeated_needs_input_notifies_once_unless_the_question_changes() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane); // focus = 2, pane 1 unfocused
        assert!(app.on_status(1, AgentStatus::NeedsInput, Some("pick a db".into())).is_some());
        // Verbatim replay: silent.
        assert!(app.on_status(1, AgentStatus::NeedsInput, Some("pick a db".into())).is_none());
        // Same status, new ask: notifies.
        assert!(app.on_status(1, AgentStatus::NeedsInput, Some("deploy too?".into())).is_some());
        // Messageless repeat while ◆ (message cleared = changed): notifies —
        // and a fresh ◆ after working always does.
        app.on_status(1, AgentStatus::Working, None);
        assert!(app.on_status(1, AgentStatus::NeedsInput, None).is_some());
        assert!(app.on_status(1, AgentStatus::NeedsInput, None).is_none(), "bare replay is silent");
    }

    #[test]
    fn needs_input_question_reaches_notification_and_feed_line() {
        let (mut app, _) = mk_app(shell_ws());
        let id = app.pane_order()[0];
        app.apply(Action::NewPane); // focus moves off pane `id`
                                    // Seed the diff baseline so the ◆ below logs as a transition.
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();

        let msg = app.on_status(id, AgentStatus::NeedsInput, Some("pick a database".into()));
        assert_eq!(msg.as_deref(), Some("shell · tmp asks: pick a database"));
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();
        let line = app.feed().back().unwrap();
        assert!(line.needs_input);
        assert_eq!(
            line.text,
            format!("{}: idle → needs you — pick a database", app.feed_label(id))
        );

        // Per-ask, not sticky: the next ◆ without a question is plain again —
        // a stale question must never resurrect on a later, unrelated ask.
        app.on_status(id, AgentStatus::Working, None);
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();
        let msg = app.on_status(id, AgentStatus::NeedsInput, None);
        assert_eq!(msg.as_deref(), Some("shell · tmp is waiting for you"));
        app.last_detect = Instant::now() - DETECT_INTERVAL - Duration::from_secs(1);
        app.tick();
        assert_eq!(
            app.feed().back().unwrap().text,
            format!("{}: working → needs you", app.feed_label(id))
        );
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
        app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::Working);
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
        let ctl_lines: Vec<&FeedEntry> =
            app.feed().iter().filter(|e| e.text.starts_with("ctl ")).collect();
        assert_eq!(
            ctl_lines.len(),
            1,
            "exactly one ctl feed line per audited call: {:?}",
            app.feed()
        );
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

    /// U25: Enter on a feed entry focuses the pane it is about — the feed
    /// listed what happened and then left you to go find it by hand. The
    /// jump goes through the same helper Alt+a's ring uses, so it crosses
    /// tabs and expands stacks like every other jump.
    #[test]
    fn feed_enter_focuses_the_selected_entrys_pane() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, _) = mk_app(shell_ws()); // tab0: pane 1
        app.apply(Action::NewTab); // tab1: pane 2, focused, active_tab=1
        let far = app.focused;
        assert_ne!(far, 1);
        app.go_to_tab(0);
        app.focused = 1;

        // The newest entry is tab1's spawn; Enter on it crosses the tab.
        app.apply(Action::ToggleFeed);
        assert_eq!(feed_selected(app.feed(), 0).and_then(|e| e.pane), Some(far));
        app.handle_mode_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(app.mode, Mode::Normal), "the jump closes the feed");
        assert_eq!(app.focused, far);
        assert_eq!(app.ws.active_tab, 1, "the jump switched tabs, like Alt+a's");
    }

    /// U25: scrolling back selects an older entry, and Enter follows *that*
    /// line — the marker the renderer draws and the entry Enter acts on are
    /// the same row by construction.
    #[test]
    fn feed_enter_follows_the_scrolled_back_selection() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _) = mk_app(shell_ws());
        let first = app.focused;
        app.apply(Action::NewPane);
        let second = app.focused;
        app.apply(Action::ToggleFeed);
        // Newest first: pane 2's spawn, then pane 1's below it.
        assert_eq!(feed_selected(app.feed(), 0).and_then(|e| e.pane), Some(second));
        app.handle_mode_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(feed_selected(app.feed(), 1).and_then(|e| e.pane), Some(first));
        app.handle_mode_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.focused, first);
    }

    /// U25: lines that are about no live pane say so instead of jumping
    /// somewhere arbitrary. A closed pane carries no id at all (`Alt+u` is
    /// that line's recovery path, not a jump), and ids are recycled, so an
    /// id that no longer resolves is re-checked before focus moves.
    #[test]
    fn feed_enter_on_a_line_with_no_live_pane_flashes_instead_of_jumping() {
        use crossterm::event::{KeyCode, KeyEvent};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::ClosePane); // newest line: "closed 2 …", pane None
        let here = app.focused;
        app.apply(Action::ToggleFeed);
        assert_eq!(feed_selected(app.feed(), 0).and_then(|e| e.pane), None);
        app.handle_mode_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.flash(), Some("no pane on that line"));
        assert_eq!(app.focused, here, "a dead line moves nothing");
        assert!(matches!(app.mode, Mode::Feed { .. }), "and leaves the feed open");

        // An id that no longer resolves to a pane is caught by the re-check.
        let (mut app, _) = mk_app(shell_ws());
        app.push_feed("ghost".into(), false, Some(999));
        app.apply(Action::ToggleFeed);
        app.handle_mode_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.flash(), Some("that pane is gone"));
        assert!(matches!(app.mode, Mode::Feed { .. }));
    }

    /// U25: an empty ring has nothing selected — Enter must not index into
    /// it. (`feed_selected` is the only reader; this pins its degenerate
    /// cases alongside the clamp `Up`/`PageUp` already rely on.)
    #[test]
    fn feed_selected_handles_an_empty_ring_and_clamps_past_the_oldest() {
        let (mut app, _) = mk_app(shell_ws());
        app.feed.clear();
        assert!(feed_selected(app.feed(), 0).is_none());
        app.push_feed("a".into(), false, None);
        app.push_feed("b".into(), false, None);
        assert_eq!(feed_selected(app.feed(), 0).map(|e| e.text.clone()), Some("b".into()));
        assert_eq!(feed_selected(app.feed(), 1).map(|e| e.text.clone()), Some("a".into()));
        assert_eq!(feed_selected(app.feed(), 999).map(|e| e.text.clone()), Some("a".into()));
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
            app.push_feed(format!("entry {i}"), false, None);
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

    /// C12/C27: the feed and roster never resolve to a **zero-sized** rect.
    /// `saturating_sub(4)` reached 0 at a body four rows tall — a terminal
    /// six rows tall, since the tab bar and hint bar take two — and a
    /// zero-height dialog draws nothing at all. The chord still flipped the
    /// mode word to `FEED`/`ROSTER` and the hint bar still offered
    /// `↑↓ select`, so pressing Alt+e or Alt+Shift+a at a small terminal
    /// changed one word on screen and nothing else: indistinguishable from
    /// a broken binding. The picker had the answer already ("an empty
    /// result still needs a frame to say so", `dialog_rect`); these two
    /// were the surfaces that never adopted it. Found by a simulation agent
    /// sweeping terminal sizes, which pinned the cutoff at height 6.
    #[test]
    fn the_fleet_overlays_always_have_a_frame_to_say_so_with() {
        // A body with no cells is C30's territory (`draw_too_small`
        // pre-empts the whole chrome below two rows), and nothing can be
        // drawn there by anyone — so the invariant starts at one cell.
        for h in 1..12u16 {
            for w in 1..12u16 {
                let (ow, oh) = feed_overlay_size(Rect::new(0, 0, w, h));
                assert!(
                    ow >= 1 && oh >= 1,
                    "a {w}x{h} body gives a {ow}x{oh} overlay — it would draw nothing",
                );
                // And it never asks for more than the body can hold, which
                // is what kept `centered_near`'s clamp arithmetic safe.
                assert!(ow <= w && oh <= h, "{w}x{h} → {ow}x{oh} overflows the body");
            }
        }
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
        app.apply(Action::EditPane);
        assert!(matches!(app.mode, Mode::PaneEdit { .. }));
        // The name row starts prefilled with the current title ("scratch",
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
        assert_eq!(
            app.focused, real_pane,
            "Alt+a from the float returns to prev_focus, not a ring jump"
        );
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
        assert!(
            order.contains(&1) && order.contains(&2),
            "original panes must stay reachable: {order:?}"
        );
        assert!(!app.float.as_ref().unwrap().shown);
    }

    #[test]
    fn float_hides_before_cycle_layout_too() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::NewPane);
        app.apply(Action::ToggleFloat);
        assert!(app.float.as_ref().unwrap().shown);
        app.apply(Action::CycleLayout { forward: true });
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
        assert!(
            app.float.as_ref().unwrap().shown,
            "opening the picker alone must not hide the float"
        );
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
        assert!(
            order.contains(&1) && order.contains(&2),
            "control spawn must not wipe the tab layout: {order:?}"
        );
        assert_eq!(
            app.focused, float_id,
            "the human's focus/float must be undisturbed by a control spawn"
        );
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

    /// Interaction (items 1 + 4): the float (C22) is always spawned as a
    /// `shell` pane (`spawn_float`) — roost's own scratch shell, never an
    /// agent — so its heuristic `Waiting` reads `Idle` (P2-10) the same as
    /// any other shell, and it can never join the Waiting fallback ring. A
    /// needy float still rings (NeedsInput is untouched by the P2-10
    /// downgrade); a merely-quiet one does not.
    #[test]
    fn attention_ring_waiting_fallback_never_includes_the_scratch_float() {
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        app.apply(Action::ToggleFloat); // hide it, focus back to pane 1
        app.runtimes.get_mut(&float_id).unwrap().set_extension_status(AgentStatus::Waiting);
        assert!(app.attention_ring().is_empty(), "a quiet scratch shell never rings");
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
        let reply = app.handle_control(Request {
            token: ct,
            method: Method::Close { pane: float_id, force: true },
        });
        match reply {
            Reply::Err { err } => assert_eq!(err, "cannot close the scratch pane"),
            other => panic!("expected refusal, got {other:?}"),
        }
        assert!(app.float.as_ref().unwrap().shown, "the float must survive the refused close");
    }

    /// M1: `find_spec` still learns the float (badges/rename/respawn need
    /// that), but `send`/`read` must refuse it over the control plane —
    /// otherwise any actor holding control.token could `read` the human's
    /// private scratch shell verbatim, or type into it.
    #[test]
    fn control_send_and_read_of_the_float_are_refused() {
        use crate::core::control::{Method, ReadMode, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        // Resolved, so the refusal below is is_float's, not "no such pane".
        assert!(app.find_spec(float_id).is_some());
        let ct = app.control_token().to_string();
        let reply = app.handle_control(Request {
            token: ct.clone(),
            method: Method::Send { pane: float_id, text: "hi".into(), submit: false },
        });
        match reply {
            Reply::Err { err } => assert_eq!(err, "cannot send to the scratch pane"),
            other => panic!("expected refusal, got {other:?}"),
        }
        assert!(
            app.runtimes.get(&float_id).unwrap().input.is_empty(),
            "nothing must reach the float"
        );
        let reply = app.handle_control(Request {
            token: ct,
            method: Method::Read { pane: float_id, mode: ReadMode::Screen },
        });
        match reply {
            Reply::Err { err } => assert_eq!(err, "cannot read the scratch pane"),
            other => panic!("expected refusal, got {other:?}"),
        }
    }

    /// M1 (review round 2): `ctl_status(Some(float_id))` would leak the
    /// live status of the human's private scratch shell — same refusal,
    /// through the same `float_refusal` helper, as close/send/read.
    #[test]
    fn control_status_of_the_float_is_refused() {
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        let ct = app.control_token().to_string();
        let reply = app
            .handle_control(Request { token: ct, method: Method::Status { pane: Some(float_id) } });
        match reply {
            Reply::Err { err } => assert_eq!(err, "cannot get the status of the scratch pane"),
            other => panic!("expected refusal, got {other:?}"),
        }
    }

    /// M1 (review round 2): forking the float would clone its spec into a
    /// brand-new real pane — and surface its cwd in `list` — so it gets the
    /// same refusal as close/send/read.
    #[test]
    fn control_fork_of_the_float_is_refused() {
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        let panes_before = app.runtimes.len();
        let ct = app.control_token().to_string();
        let reply = app
            .handle_control(Request { token: ct, method: Method::Fork { pane: Some(float_id) } });
        match reply {
            Reply::Err { err } => assert_eq!(err, "cannot fork the scratch pane"),
            other => panic!("expected refusal, got {other:?}"),
        }
        assert_eq!(app.runtimes.len(), panes_before, "no clone of the float must be spawned");
    }

    /// M1 (review round 2): `wait <float> --until idle` would be a typing
    /// oracle on the human's private scratch shell — refused at
    /// registration, so it never parks a waiter that could later fire.
    #[test]
    fn control_wait_on_the_float_is_refused() {
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        let ct = app.control_token().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        app.handle_control_msg(
            Request {
                token: ct,
                method: Method::Wait {
                    panes: vec![float_id],
                    until: "idle".into(),
                    timeout_ms: None,
                },
            },
            tx,
        );
        match rx.recv().unwrap() {
            Reply::Err { err } => assert_eq!(err, "cannot wait on the scratch pane"),
            other => panic!("expected refusal, got {other:?}"),
        }
        assert!(app.waiters.is_empty(), "must not park a wait on the float");
    }

    /// M1 (review round 2): `poll_waiters`' own LOW-3 re-check already
    /// re-verifies `may_target` at fire time because a recycled pane id can
    /// belong to someone new by the time a parked wait fires — the same
    /// recycling can hand that id to the float instead of another real
    /// pane, and Fleet's `may_target` is unconditionally `true`, so only an
    /// explicit `is_float` re-check (not just registration-time refusal)
    /// closes that gap. Engineered the same way LOW-3's own regression
    /// test is (`wait_does_not_leak_a_recycled_pane_ids_status_cross_subtree`):
    /// close the highest-numbered pane so its id is genuinely recycled,
    /// then let the float claim it — no reaching into `app.waiters`.
    #[test]
    fn poll_waiters_refuses_an_id_recycled_onto_the_float() {
        use crate::core::control::{Method, Reply, Request};
        let (mut app, _) = mk_app(shell_ws());
        let ct = app.control_token().to_string();
        app.apply(Action::NewPane); // panes 1 | 2
        app.runtimes.get_mut(&2).unwrap().set_extension_status(AgentStatus::Working);

        let (tx, rx) = std::sync::mpsc::channel();
        app.handle_control_msg(
            Request {
                token: ct.clone(),
                method: Method::Wait {
                    panes: vec![2],
                    until: "idle".into(),
                    timeout_ms: Some(60_000),
                },
            },
            tx,
        );
        assert!(rx.try_recv().is_err(), "working, not idle yet -> parked");
        assert_eq!(app.waiters.len(), 1);

        // Pane 2 (highest id) closes, freeing its id, and the float —
        // spawned fresh, so it starts idle same as any shell pane —
        // recycles it.
        let close = app
            .handle_control(Request { token: ct, method: Method::Close { pane: 2, force: true } });
        assert!(matches!(close, Reply::Ok { .. }), "{close:?}");
        app.apply(Action::ToggleFloat);
        let float_id = app.focused;
        assert_eq!(float_id, 2, "test assumes the float actually recycled the freed id");

        app.poll_waiters();
        assert!(rx.try_recv().is_err(), "must not disclose the float's status, even to Fleet");
        assert_eq!(app.waiters.len(), 1, "the waiter stays parked, not silently dropped");
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
        let ids: Vec<u64> =
            ok.as_array().unwrap().iter().map(|v| v["pane"].as_u64().unwrap()).collect();
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

        app.mode = Mode::Help { top: 0, filter: None };
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

    // -- U1/C12: the mode escape hatch --------------------------------------

    /// Every `Mode` this test must cover, one of each variant.
    ///
    /// The `match` below is the point, and its limit is worth stating
    /// exactly: it is exhaustive over `Mode`, so adding a variant fails to
    /// compile *here* — which lands the author in this function, holding
    /// the list they need to extend. It cannot force them to extend it.
    /// That is still the strongest guarantee available (Rust exposes no
    /// variant count to assert against), and it matters more than usual
    /// because the rule this feeds is a **negative** one — "no mode may
    /// swallow an Alt chord" — so a mode that skipped the list would break
    /// it in silence, the shape the C36 audit already caught once with
    /// `Alt+Enter`. The duplicate check below is the other half: a list
    /// that gained an entry by copy-paste covers one fewer variant than
    /// its length suggests.
    fn every_mode() -> Vec<(&'static str, Mode)> {
        let modes = vec![
            ("Normal", Mode::Normal),
            ("Rename", Mode::Rename { buffer: "x".into(), cursor: 1, target: RenameTarget::Tab }),
            (
                "PaneEdit",
                Mode::PaneEdit {
                    name: "x".into(),
                    lines: vec![String::new()],
                    row: 0,
                    col: 0,
                    pane: 1,
                },
            ),
            (
                "Broadcast",
                Mode::Broadcast { lines: vec![String::new()], row: 0, col: 0, status_filter: None },
            ),
            ("Picker", Mode::Picker { selection: 0, filter: String::new(), cwd: 0, on_cwd: false }),
            ("Scroll", Mode::Scroll),
            ("Copy", Mode::Copy { cursor: (0, 0) }),
            ("Help", Mode::Help { top: 0, filter: None }),
            ("Feed", Mode::Feed { offset: 0 }),
            (
                "Roster",
                Mode::Roster { cursor: 1, filter: String::new(), top: 0, status_filter: None },
            ),
            ("Search", Mode::Search { copy_cursor: None }),
        ];
        for (_, m) in &modes {
            // Exhaustiveness guard — add the new variant to the list above.
            match m {
                Mode::Normal
                | Mode::Rename { .. }
                | Mode::PaneEdit { .. }
                | Mode::Broadcast { .. }
                | Mode::Picker { .. }
                | Mode::Scroll
                | Mode::Copy { .. }
                | Mode::Help { .. }
                | Mode::Feed { .. }
                | Mode::Roster { .. }
                | Mode::Search { .. } => {}
            }
        }
        let mut seen = std::collections::HashSet::new();
        for (label, m) in &modes {
            assert!(
                seen.insert(std::mem::discriminant(m)),
                "{label}: listed twice — the list covers fewer variants than it looks",
            );
        }
        modes
    }

    /// States that are a **second key surface inside one variant**, and so
    /// have to be swept even though `every_mode()` already lists the
    /// variant once.
    ///
    /// Kept separate rather than added to `every_mode()`, whose duplicate
    /// check is the mechanism that makes its exhaustive `match` mean
    /// something — a second `Mode::Help` there fails that check, correctly.
    /// One list answers "is every variant covered", this one answers "is
    /// every distinct key surface covered", and they are different
    /// questions.
    ///
    /// C39's filtering overlay is the first entry and the reason this
    /// exists: it holds every printable key, so it is precisely where
    /// C24b's rule is under real pressure, and the sweeps were proving
    /// nothing about it. Named by the C39 design audit.
    fn extra_mode_states() -> Vec<(&'static str, Mode)> {
        vec![("Help (filtering)", Mode::Help { top: 0, filter: Some("pane".into()) })]
    }

    /// Every mode surface the C24b sweeps must cover: one of each variant,
    /// plus the second surfaces above.
    fn every_key_surface() -> Vec<(&'static str, Mode)> {
        let mut all = every_mode();
        all.extend(extra_mode_states());
        all
    }

    /// `handle_mode_key`'s central promise: an Alt chord is never consumed
    /// by a mode, it drops the mode and yields to the global bindings —
    /// "Alt+q must quit from anywhere". Enforced by a single `if` at the
    /// top of that function, and until now asserted nowhere.
    ///
    /// C24b's "nothing else changes" bullet, at the one instance that
    /// matters most. Written after a simulation agent reported Alt+q
    /// hanging inside every modal. It does not — its harness pressed the
    /// chord 100 ms after spawn, before roost had drawn anything, so no
    /// modal was ever open, and the 1 s deadline was under the real quit
    /// time. The report was wrong; that the rule had no test was not.
    #[test]
    fn no_mode_swallows_an_alt_chord() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        for (label, m) in every_key_surface() {
            let (mut app, _) = mk_app(shell_ws());
            app.mode = m;
            let consumed =
                app.handle_mode_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT));
            assert!(
                !consumed,
                "{label}: swallowed Alt+q instead of yielding to the global bindings",
            );
            assert!(
                matches!(app.mode, Mode::Normal),
                "{label}: yielded the chord but stayed in the mode",
            );
        }
    }

    /// The contract in full, not just its most important instance: **every**
    /// Alt chord leaves every mode. C24b names the complete list of chords
    /// a mode may consume, and it is two — the mode's own entry chord
    /// (which also *exits*) and `Alt+Enter` in the two multi-line editors
    /// (which does **not**: it breaks a line and the dialog stays open, the
    /// tree's one retained Alt chord, because Terminal.app spells
    /// Shift+Enter that way). Anything else a mode holds on to fails here.
    ///
    /// Sweeping the whole chord space rather than listing chords is what
    /// makes this a gate: a mode that claims some future `Alt+x` is caught
    /// without anyone remembering to test `Alt+x`.
    #[test]
    fn the_only_alt_chords_a_mode_keeps_are_the_two_c24b_names() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut codes: Vec<KeyCode> = (0x20u8..=0x7e).map(|b| KeyCode::Char(b as char)).collect();
        codes.extend([
            KeyCode::Enter,
            KeyCode::PageUp,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
        ]);
        let mut swept = 0;
        // `Mode` is deliberately not `Clone` (a mode owns buffers), so each
        // probe rebuilds the list and takes its own copy of the variant.
        for i in 0..every_key_surface().len() {
            let (label, m) = every_key_surface().swap_remove(i);
            let entry = mode_entry_action(&m);
            let editor = matches!(m, Mode::PaneEdit { .. } | Mode::Broadcast { .. });
            for &code in &codes {
                for shift in [false, true] {
                    let mut mods = KeyModifiers::ALT;
                    if shift {
                        mods |= KeyModifiers::SHIFT;
                    }
                    let key = KeyEvent::new(code, mods);
                    let (mut app, _) = mk_app(shell_ws());
                    let Some(action) = bound_action(key, &app) else { continue };
                    swept += 1;
                    app.mode = every_key_surface().swap_remove(i).1;
                    if !app.handle_mode_key(key) {
                        assert!(
                            matches!(app.mode, Mode::Normal),
                            "{label}: yielded {action:?} but stayed in the mode",
                        );
                        continue;
                    }
                    // Consumed. Only C12's two are allowed to.
                    let is_entry = Some(&action) == entry.as_ref();
                    let is_editor_newline = editor && code == KeyCode::Enter;
                    assert!(
                        is_entry || is_editor_newline,
                        "{label}: consumed the Alt chord for {action:?} — C24b lets a \
                         mode keep only its own entry chord and, in the two multi-line \
                         editors, Alt+Enter",
                    );
                    // The two consumptions differ, and C24b turns on the
                    // difference: an entry chord is a *way out*, so it must
                    // leave the mode; a retained chord is *content*, so it
                    // must not. Asserting only the first would let the
                    // editors' Alt+Enter start closing the dialog it exists
                    // to type into, with nothing to say so.
                    if is_entry {
                        assert!(
                            matches!(app.mode, Mode::Normal),
                            "{label}: its entry chord must toggle the mode off (U18)",
                        );
                    } else {
                        assert!(
                            !matches!(app.mode, Mode::Normal),
                            "{label}: Alt+Enter is a line break, not a way out — the \
                             editor must still be open",
                        );
                    }
                }
            }
        }
        // The sweep is worthless if `bound_action` stopped resolving
        // anything — it would pass by testing nothing at all.
        assert!(swept > 200, "the chord sweep found almost nothing ({swept}) — check it");
    }

    /// What `translate` makes of `key` against the default keymap, or None
    /// if nothing is bound to it.
    fn bound_action(key: crossterm::event::KeyEvent, app: &App<FakePane>) -> Option<Action> {
        match crate::ui::input::translate_with(key, app.keymap()) {
            crate::ui::input::InputResult::Action(a) => Some(a),
            _ => None,
        }
    }

    /// And the end of that path: the yielded chord really does quit. A
    /// quiet fleet quits on the first press (U1), so one `apply` is the
    /// whole story — the busy-fleet confirm has its own tests.
    #[test]
    fn alt_q_quits_from_every_mode() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        for (label, m) in every_key_surface() {
            let (mut app, _) = mk_app(shell_ws());
            app.mode = m;
            let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT);
            if !app.handle_mode_key(key) {
                if let crate::ui::input::InputResult::Action(a) =
                    crate::ui::input::translate_with(key, app.keymap())
                {
                    app.apply(a);
                }
            }
            assert!(app.quit, "{label}: Alt+q did not quit");
        }
    }
}
