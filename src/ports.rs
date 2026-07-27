//! Ports — the trait boundaries between roost's core and the outside world.
//!
//! The core (`core::app`) is generic over these traits and never touches a
//! real PTY, filesystem, or terminal. Production adapters live in `infra/`;
//! test fakes live in `ports::fakes` and let every core behavior run in a
//! plain unit test.
//!
//! One conscious shim: `PaneBackend::screen()` exposes the `vt100::Screen`
//! grid directly instead of wrapping it. The renderer needs the whole cell
//! grid; re-wrapping ~15 accessor methods would be ceremony without safety.
//! Fakes return `None` and the renderer must tolerate that.

use anyhow::Result;
use std::path::PathBuf;
use std::sync::mpsc::SyncSender;

use crate::agents::CommandSpec;
use crate::core::event::AppEvent;
use crate::core::status::AgentStatus;
use crate::core::workspace::{PaneId, Workspace};

/// What a pane is actually running, read from the OS — its live working
/// directory and any known agent CLI in its process subtree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observation {
    pub cwd: Option<PathBuf>,
    /// Adapter id of a known agent running in the pane, if any.
    pub agent: Option<String>,
}

/// U14: which clipboard channel actually took a copy. roost writes to both
/// (a native helper *and* OSC 52) and used to discard both results, so the
/// hint bar said "copied N chars" whether or not anything landed. The two
/// channels are not equally knowable, and the variants say exactly that:
/// a native helper reports an exit status, while OSC 52 is fire-and-forget
/// — the terminal may honor it, ignore it, or not be listening at all, and
/// there is no reply. Lives here (not in `infra`) because it is the
/// vocabulary the core's flash text is written against; `infra::clipboard`
/// produces it, `core::app::copy_flash_text` consumes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardOutcome {
    /// A native helper (pbcopy / wl-copy / xclip / xsel) exited successfully
    /// — the system clipboard really holds the text.
    Native,
    /// No native helper succeeded, but the OSC 52 sequence went out. It is
    /// the only channel that works over SSH/tmux, and the honest report is
    /// "sent, unacknowledged".
    Osc52,
    /// Neither channel got the text anywhere.
    Failed,
}

/// W3: what a pane's escape traffic asked roost to do on its behalf, drained
/// after each chunk of output. The pane's terminal state machine can't carry
/// these out itself — they need the thing that owns a host terminal, a
/// notification daemon, a clipboard.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PaneEffects {
    /// P2: OSC 9 / OSC 777 notification bodies, in stream order. The pane's
    /// attention state (the bell heuristic → `◆`) is already updated by the
    /// backend; these are the *texts*, for the notifier.
    pub notifications: Vec<String>,
    /// Bytes roost must forward verbatim to the HOST terminal's own output —
    /// P2's re-emitted OSC 9 (so native desktop notifications fire) and P3's
    /// OSC 52 clipboard writes. Already rate-limited, capped and sanitized
    /// by the backend; the composition root only has to write them between
    /// frames.
    pub host_writes: Vec<u8>,
}

/// What the pane's inner application asked for, mouse-wise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseProto {
    /// App doesn't listen for mouse events → roost handles the wheel itself.
    None,
    /// App enabled SGR mouse reporting → forward encoded events to the PTY.
    Sgr,
}

/// A running pane: process + terminal state machine. Implemented by
/// `infra::pty::PtyPane` in production.
pub trait PaneBackend: Sized {
    /// `pixels` is the pane's (width, height) pixel geometry, derived by the
    /// core from the host window's proportionally; (0, 0) = unknown — kept 0
    /// so downstream consumers (PTY winsize, XTWINOPS pixel reports) stay
    /// honest rather than inventing a geometry.
    fn spawn(
        id: PaneId,
        cmd: &CommandSpec,
        rows: u16,
        cols: u16,
        pixels: (u16, u16),
        tx: SyncSender<AppEvent>,
    ) -> Result<Self>;

    /// Feed process output into the terminal state machine.
    fn process_output(&mut self, bytes: &[u8]);
    /// User keystrokes → process stdin. Implementations should snap
    /// scrollback to the live tail (typing means "I'm back"). Returns
    /// whether the bytes actually reached the process — a caller that
    /// reports a delivery count (`ctl_send`/`ctl_broadcast`) must not
    /// count a failed write as sent.
    fn write_input(&mut self, bytes: &[u8]) -> bool;
    /// Bytes → process stdin without touching scrollback (forwarded mouse
    /// events must not yank the view to the live tail). Same success
    /// semantics as `write_input`.
    fn write_input_raw(&mut self, bytes: &[u8]) -> bool;
    /// `pixels` as in `spawn` — the pane's derived pixel geometry, refreshed
    /// on every host resize; (0, 0) = unknown, kept 0.
    fn resize(&mut self, rows: u16, cols: u16, pixels: (u16, u16));
    /// Ask the child to exit cleanly (SIGHUP, as if its terminal closed) so it
    /// can flush a final turn. Best-effort; `kill()` is the guaranteed stop.
    /// Default no-op for backends without a real process.
    fn hangup(&mut self) {}
    fn kill(&mut self);

    fn status(&self) -> AgentStatus;
    fn set_extension_status(&mut self, s: AgentStatus);
    fn on_exit(&mut self);

    /// Has the pane negotiated the kitty "disambiguate" keyboard flag? When
    /// true, roost forwards modified Enter (and friends) in the CSI-u encoding
    /// the app asked for; otherwise it uses a legacy fallback. Default false.
    fn kitty_disambiguate(&self) -> bool {
        false
    }

    /// Has the pane's app switched on DECCKM application cursor keys mode
    /// (`CSI ?1h` — what zsh's line editor via `smkx`, vim, and most
    /// full-screen TUIs do)? When true, roost sends unmodified cursor keys as
    /// SS3 (`ESC O A` …) the way a real terminal would; otherwise the normal
    /// CSI forms. Default false.
    fn app_cursor_keys(&self) -> bool {
        false
    }

    /// Has the pane's app switched on bracketed paste (mode 2004 — modern
    /// shells, editors, and agent TUIs all do)? When true, roost delivers a
    /// host paste wrapped in the `ESC[200~`/`ESC[201~` guards the app asked
    /// for, so pasted newlines insert instead of submitting. Default false.
    fn bracketed_paste(&self) -> bool {
        false
    }

    /// P9: is the pane's app drawing on the *alternate* screen (`?1049h` /
    /// `?47h` — `less`, `man`, vim, every full-screen TUI)? That grid has no
    /// scrollback at all, so a wheel event roost handles itself can only be a
    /// no-op there; the wheel has to reach the application instead. See
    /// `ui::mouse::route_mouse`. Default false.
    fn alternate_screen(&self) -> bool {
        false
    }

    /// W3: drain what the pane's escape traffic asked roost to do since the
    /// last call — see `PaneEffects`. Called right after `process_output`;
    /// nothing accumulates between calls. Default empty for backends with no
    /// escape stream to interpret.
    fn take_effects(&mut self) -> PaneEffects {
        PaneEffects::default()
    }

    /// P7: the cursor shape the pane's app asked for with DECSCUSR
    /// (`CSI Ps SP q`) — 1..=6 per xterm, `None` when it wants the
    /// terminal's default. Only the focused pane's shape is ever mirrored to
    /// the host: there is one real cursor. Default None.
    fn cursor_shape(&self) -> Option<u8> {
        None
    }

    /// Terminal grid for rendering. `None` for fakes (renderer must cope).
    fn screen(&self) -> Option<&vt100::Screen>;
    fn set_scrollback(&mut self, lines: usize);
    /// Wheel scrolling: positive = further into history.
    fn scroll_by(&mut self, delta: i32);
    /// The view's current scrollback offset in rows — 0 = live tail. This
    /// is the *grid-clamped* truth of what's on screen (U3: chrome honesty
    /// surfaces like the `↑N` badge token must reflect the view, never a
    /// caller's unclamped intent), read rather than reaching into backend
    /// internals.
    fn scroll_offset(&self) -> usize;
    /// How many scrollback rows are actually banked — `scroll_offset`'s
    /// honest upper bound, the M of the scroll-position hint `↑N/M` (U3).
    fn scroll_total(&self) -> usize;
    fn mouse_proto(&self) -> MouseProto;

    /// Observe the pane's live working directory and any known agent running
    /// in it (`known_agents` are adapter ids). None = not inspectable (dead
    /// process / unsupported platform); the caller then leaves persisted
    /// state untouched. Default None for backends that can't inspect.
    fn observe(&self, _known_agents: &[String]) -> Option<Observation> {
        None
    }

    /// Extract the visible text between two inclusive cell coords (row, col),
    /// in pane-inner space, for copy mode. Reading order, trailing spaces
    /// trimmed per line, lines joined with '\n'. Default empty.
    fn grab_text(&self, _start: (u16, u16), _end: (u16, u16)) -> String {
        String::new()
    }

    /// The pane's full scrollback history plus its current screen, for the
    /// control interface's `read --full`/`--tail` (which must see output
    /// that's since scrolled off-screen, not just the visible grid). Default
    /// degrades to the visible grid, for backends with no history to offer.
    fn grab_all_text(&self) -> String {
        self.grab_text((0, 0), (u16::MAX, u16::MAX))
    }

    /// U19: does visible row `row` continue onto the next one? The terminal
    /// grid knows — a row is flagged wrapped when the cursor ran off its
    /// last column rather than a newline arriving — and that flag is the
    /// only way to tell a link that wrapped from two adjacent lines that
    /// merely look adjacent. False (never wraps) for backends with no grid.
    fn row_wrapped(&self, _row: u16) -> bool {
        false
    }
}

/// Workspace persistence. Implemented by `infra::store::FsStore`.
pub trait StateStore {
    fn load(&self) -> Result<Option<Workspace>>;
    fn save(&self, ws: &Workspace) -> Result<()>;
}

/// "A pane needs you" side-channel. Implemented by `infra::notify`.
pub trait Notifier {
    fn notify(&mut self, msg: &str);
}

#[cfg(test)]
pub mod fakes {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// In-memory pane: records the spawn command and all input; status is
    /// settable. `cmd.program == "spawn-fail"` simulates a spawn error.
    pub struct FakePane {
        pub cmd: CommandSpec,
        pub input: Vec<u8>,
        pub scrollback: i64,
        /// Test knob: how many history rows this fake "banks" — the bound
        /// `scroll_total()` reports. Defaults to `usize::MAX` (effectively
        /// unbounded) so tests that never think about history keep their
        /// exact pre-U3 behavior.
        pub scroll_total: usize,
        status: AgentStatus,
        ext: Option<AgentStatus>,
        exited: bool,
        pub proto: MouseProto,
        /// Test-settable observation returned by `observe`.
        pub observation: Option<Observation>,
        /// Test-settable text returned by `grab_text`.
        pub grab: String,
        /// Test-settable text returned by `grab_all_text` (distinct from
        /// `grab` so a test can tell a full/tail read apart from a screen
        /// read).
        pub all_text: String,
        /// Test knob (U19): a per-row grid. When non-empty, a single-row
        /// `grab_text` reads `rows[row]` instead of `grab`, and `row_wrapped`
        /// reports `wrapped[row]` — enough to model a link that ran off the
        /// end of one row and continued on the next.
        pub rows: Vec<String>,
        pub wrapped: Vec<bool>,
        /// Test knob: when true, `write_input`/`write_input_raw` report
        /// failure and drop the bytes — simulates a pane whose pipe died
        /// right after a status snapshot said it was still running.
        pub fail_write: bool,
        /// Test knob: simulates the pane's app having enabled DECCKM
        /// application cursor keys mode.
        pub app_cursor: bool,
        /// Test knob: simulates the pane's app having enabled bracketed
        /// paste (mode 2004).
        pub bracketed: bool,
        /// Test knob (P9): simulates the pane's app drawing on the alternate
        /// screen (`?1049h`).
        pub alternate_screen: bool,
        /// The pixel geometry most recently seen — at spawn, then updated by
        /// every resize — so tests can assert the P4 pixel plumbing.
        pub pixels: (u16, u16),
        /// Test knob (W3): the effects the next `take_effects` drains. Set
        /// it, drive the core, assert on what the core did with them; like
        /// the real backend, draining empties it.
        pub effects: PaneEffects,
        /// Test knob (P7): the DECSCUSR shape this pane reports.
        pub cursor_shape: Option<u8>,
    }

    impl PaneBackend for FakePane {
        fn spawn(
            _id: PaneId,
            cmd: &CommandSpec,
            _rows: u16,
            _cols: u16,
            pixels: (u16, u16),
            _tx: SyncSender<AppEvent>,
        ) -> Result<Self> {
            if cmd.program == "spawn-fail" {
                anyhow::bail!("spawn-fail requested");
            }
            Ok(Self {
                cmd: cmd.clone(),
                input: vec![],
                scrollback: 0,
                scroll_total: usize::MAX,
                status: AgentStatus::Idle,
                ext: None,
                exited: false,
                proto: MouseProto::None,
                observation: None,
                grab: String::new(),
                all_text: String::new(),
                rows: vec![],
                wrapped: vec![],
                fail_write: false,
                app_cursor: false,
                bracketed: false,
                alternate_screen: false,
                pixels,
                effects: PaneEffects::default(),
                cursor_shape: None,
            })
        }
        fn process_output(&mut self, _bytes: &[u8]) {
            self.status = AgentStatus::Working;
        }
        fn write_input(&mut self, bytes: &[u8]) -> bool {
            if self.fail_write {
                return false;
            }
            self.scrollback = 0;
            self.input.extend_from_slice(bytes);
            true
        }
        fn write_input_raw(&mut self, bytes: &[u8]) -> bool {
            if self.fail_write {
                return false;
            }
            self.input.extend_from_slice(bytes);
            true
        }
        fn resize(&mut self, _rows: u16, _cols: u16, pixels: (u16, u16)) {
            self.pixels = pixels;
        }
        fn kill(&mut self) {
            self.exited = true;
        }
        fn status(&self) -> AgentStatus {
            if self.exited {
                AgentStatus::Exited
            } else {
                self.ext.unwrap_or(self.status)
            }
        }
        /// Mirrors `StatusTracker`'s contract: an extension-reported `Exited`
        /// is demoted to `Waiting` — only `on_exit` (the PTY EOF) marks a
        /// pane dead.
        fn set_extension_status(&mut self, s: AgentStatus) {
            let s = if s == AgentStatus::Exited { AgentStatus::Waiting } else { s };
            self.ext = Some(s);
        }
        fn on_exit(&mut self) {
            self.exited = true;
        }
        fn app_cursor_keys(&self) -> bool {
            self.app_cursor
        }
        fn bracketed_paste(&self) -> bool {
            self.bracketed
        }
        fn alternate_screen(&self) -> bool {
            self.alternate_screen
        }
        fn take_effects(&mut self) -> PaneEffects {
            std::mem::take(&mut self.effects)
        }
        fn cursor_shape(&self) -> Option<u8> {
            self.cursor_shape
        }
        fn screen(&self) -> Option<&vt100::Screen> {
            None
        }
        /// U9: models the real backend's clamp-and-read-back — the stored
        /// offset is what the "grid" (`scroll_total` banked rows) accepted,
        /// never the caller's unclamped ask.
        fn set_scrollback(&mut self, lines: usize) {
            self.scrollback = lines.min(self.scroll_total) as i64;
        }
        fn scroll_by(&mut self, delta: i32) {
            let want = (self.scrollback + delta as i64).max(0) as usize;
            self.scrollback = want.min(self.scroll_total) as i64;
        }
        /// Like the real grid, the reported view offset never exceeds the
        /// banked history (`scroll_total`), whatever a caller stored.
        fn scroll_offset(&self) -> usize {
            (self.scrollback.max(0) as usize).min(self.scroll_total)
        }
        fn scroll_total(&self) -> usize {
            self.scroll_total
        }
        fn mouse_proto(&self) -> MouseProto {
            self.proto
        }
        fn observe(&self, _known: &[String]) -> Option<Observation> {
            self.observation.clone()
        }
        fn grab_text(&self, start: (u16, u16), end: (u16, u16)) -> String {
            if !self.rows.is_empty() && start.0 == end.0 {
                return self.rows.get(start.0 as usize).cloned().unwrap_or_default();
            }
            self.grab.clone()
        }
        fn grab_all_text(&self) -> String {
            self.all_text.clone()
        }
        fn row_wrapped(&self, row: u16) -> bool {
            self.wrapped.get(row as usize).copied().unwrap_or(false)
        }
    }

    /// Shared in-memory store; clone to keep a handle for assertions.
    #[derive(Clone, Default)]
    pub struct MemStore(pub Arc<Mutex<Option<Workspace>>>);

    impl StateStore for MemStore {
        fn load(&self) -> Result<Option<Workspace>> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn save(&self, ws: &Workspace) -> Result<()> {
            *self.0.lock().unwrap() = Some(ws.clone());
            Ok(())
        }
    }

    /// Records notifications for assertions.
    #[derive(Clone, Default)]
    pub struct RecordingNotifier(pub Arc<Mutex<Vec<String>>>);

    impl Notifier for RecordingNotifier {
        fn notify(&mut self, msg: &str) {
            self.0.lock().unwrap().push(msg.to_string());
        }
    }
}
