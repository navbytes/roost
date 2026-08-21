//! Per-pane status: Working / NeedsInput / Waiting / Idle / Exited.
//!
//! Two signal sources (design doc §6.3–6.4):
//! 1. Extension events (exact) — pi's roost.ts extension / Claude Code hooks
//!    reporting over the unix socket. TODO(M3): socket listener.
//! 2. Output heuristics (fallback) — recent PTY bytes ⇒ Working, silence ⇒
//!    Waiting/Idle. Prompt-pattern detection for NeedsInput is TODO(M3).
//!
//! A third input, `ext_link`, doesn't carry a status of its own — it's
//! whether source 1's socket connection is currently open (`infra::sock`
//! tracks this per pane and pushes it in via `set_ext_link`). It decides how
//! much source 2 is allowed to override source 1: while the link is live, an
//! extension/hook report is trusted over output heuristics almost
//! unconditionally (see `current()`); once it's down, output heuristics fall
//! back to today's behavior.

use std::time::{Duration, Instant};

/// A `Working` report decays to `Waiting` after this much silence, so a
/// badge doesn't stick forever if the hook that would report "done" dies.
/// Generous, to not misread a legitimately thinking agent. Only in effect
/// while `ext_link` is down — a live connection outranks the clock (see
/// `current()`'s `Working` arm).
const STUCK_WORKING: Duration = Duration::from_secs(45);
/// Output within this window counts as "actively producing".
const ACTIVE_WINDOW: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    /// Agent is actively producing output / running tools.
    Working,
    /// Agent explicitly asked for the user (extension signal).
    NeedsInput,
    /// Turn ended; ball is probably in your court.
    Waiting,
    /// Nothing has happened yet.
    Idle,
    /// Child process exited.
    Exited,
}

pub struct StatusTracker {
    last_output: Option<Instant>,
    exited: bool,
    /// Exact status pushed by an extension/hook, plus when it arrived.
    extension_status: Option<AgentStatus>,
    ext_at: Option<Instant>,
    /// Last time the pane rang the terminal bell (0x07) — the tmux-style
    /// "program wants your attention" signal, used as a heuristic NeedsInput
    /// when no extension/hook is installed.
    bell_at: Option<Instant>,
    /// Is the extension/hook's status-socket connection for this pane
    /// currently open? (D2/D1.) Pushed from `infra::sock` via
    /// `set_ext_link`, which tracks real connection liveness — not a timer
    /// or heuristic. Starts `false`, today's fallback behavior.
    ext_link: bool,
    /// Screen-derived status parsed from the pane's terminal title (D5) —
    /// Claude Code publishes a spinner prefix while working, `✳ ` at rest,
    /// and its hook connections are one-shot, so between hooks this is the
    /// only exact-ish signal a claude pane has. Ranks *between* extension
    /// reports and byte heuristics: skipped while `ext_link` is live (same
    /// deference as output promotion), never touches NeedsInput or Exited,
    /// and consulted at all only when `title_enabled` says an agent is
    /// actually running. Pushed from `infra::pty` on every title change;
    /// `None` for a title that matches no known agent pattern.
    title_status: Option<AgentStatus>,
    /// When the title string last changed — refreshes every frame for an
    /// *animating* spinner (a liveness heartbeat, not a transition marker),
    /// so `title_working_live`'s `STUCK_WORKING` bound only ever fires for a
    /// title that froze (a hung agent).
    title_at: Option<Instant>,
    /// Gate on the whole title channel, pushed by the app layer (spawn +
    /// `observe_panes`): true only while the pane's effective adapter is an
    /// agent. A vt100 title outlives the process that set it (nothing ever
    /// clears it, not even RIS), so without this an agent that exited in a
    /// shell pane would leave a permanent `✳` vetoing Working for every
    /// later command there. Same gate/reason as `display_name_live`'s
    /// shell-title rule.
    title_enabled: bool,
}

impl StatusTracker {
    pub fn new() -> Self {
        Self {
            last_output: None,
            exited: false,
            extension_status: None,
            ext_at: None,
            bell_at: None,
            ext_link: false,
            title_status: None,
            title_at: None,
            title_enabled: false,
        }
    }

    pub fn on_output(&mut self) {
        self.last_output = Some(Instant::now());
    }

    /// The pane emitted a bell (0x07). Recorded as a heuristic attention
    /// signal; only consulted when no exact extension status is present.
    pub fn on_bell(&mut self) {
        self.bell_at = Some(Instant::now());
    }

    pub fn on_exit(&mut self) {
        self.exited = true;
    }

    /// An extension/hook reported `s`. An `Exited` report is demoted to
    /// `Waiting`: process death has exactly one ground truth — the PTY EOF
    /// (`on_exit`) — a socket "exited" is hearsay. The pane's env
    /// (ROOST_PANE/ROOST_TOKEN) is inherited by every descendant process, so
    /// a *nested* pi (a subagent, a one-shot `pi -p` tool call, a pi run by
    /// hand in a shell pane) reports its own `session_shutdown` as this pane
    /// while the real child is alive and well — believing it would render a
    /// live pane dead. When the process really is exiting, the EOF lands
    /// moments later and marks it dead for real.
    pub fn set_extension_status(&mut self, s: AgentStatus) {
        let s = if s == AgentStatus::Exited { AgentStatus::Waiting } else { s };
        self.extension_status = Some(s);
        self.ext_at = Some(Instant::now());
    }

    /// The pane's status-socket connection went up or down (D2) — pushed by
    /// `infra::sock`, the source of truth; never a timer or heuristic here.
    pub fn set_ext_link(&mut self, up: bool) {
        self.ext_link = up;
    }

    /// The pane's terminal title changed and parsed to `s` (D5) — `None`
    /// when the title matches no known agent pattern, clearing any previous
    /// signal. Called on every title-string change; an animating spinner
    /// lands here every frame, which is what makes `title_at` a heartbeat.
    pub fn set_title_status(&mut self, s: Option<AgentStatus>) {
        self.title_status = s;
        self.title_at = Some(Instant::now());
    }

    /// D5's gate: is an agent actually running in this pane right now?
    /// (spawn adapter, kept current by `observe_panes`). While off, the
    /// title channel is stored but never consulted — a leftover title from
    /// an exited agent would otherwise be trusted forever. Disabling also
    /// clears the stored signal, so the next run starts clean.
    pub fn set_title_signal(&mut self, enabled: bool) {
        self.title_enabled = enabled;
        if !enabled {
            self.title_status = None;
            self.title_at = None;
        }
    }

    /// D5: a Working title is trusted while output flows or the title is
    /// still fresh (within `STUCK_WORKING` of its last change — an
    /// *animating* spinner refreshes that every frame, so this only fires
    /// for a frozen title, i.e. a hung agent). Deferred to a live extension
    /// link, same rule byte promotion follows.
    fn title_working_live(&self) -> bool {
        self.title_enabled
            && !self.ext_link
            && self.title_status == Some(AgentStatus::Working)
            && (self.recent_output() || self.title_at.is_some_and(|t| t.elapsed() <= STUCK_WORKING))
    }

    /// D5: the title says the agent is at rest (`✳`). No time decay — it
    /// only suppresses byte-noise promotion while the gate says an agent is
    /// running, and the gate (not a clock) kills the signal on exit. Same
    /// live-link deference as `title_working_live`.
    fn title_resting(&self) -> bool {
        self.title_enabled && !self.ext_link && self.title_status == Some(AgentStatus::Waiting)
    }

    fn recent_output(&self) -> bool {
        self.last_output.is_some_and(|t| t.elapsed() < ACTIVE_WINDOW)
    }

    /// Shared core of `vouched_live`/`recently_reported`: is the current
    /// extension report Working/NeedsInput, and does `live` — the caller's
    /// own "still trustworthy" definition — hold? A resting report that
    /// `current()` promoted via heuristic (fresh output, a bell) never
    /// counts here either way.
    fn reported_while(&self, live: bool) -> bool {
        match self.extension_status {
            Some(AgentStatus::Working | AgentStatus::NeedsInput) => live,
            _ => false,
        }
    }

    /// [U22] Is the status `current()` reports one an extension/hook
    /// actually *reported*, vs one roost inferred from PTY traffic — for the
    /// destructive-close guard (`PaneBackend::status_reported`). `current()`
    /// folds both together for the badge; the guard can't, since a hook
    /// saying "working" means a turn is in flight, while `recent_output()`
    /// is equally true of `ls`.
    ///
    /// [F4] A report counts as live while its link is up (`ext_link`), or,
    /// absent that, within `STUCK_WORKING` of arriving — only ever *more*
    /// permissive than the plain elapsed check, so it can never call a truly
    /// dead hook's stale report "vouched for". Do not reuse this for the
    /// bell relay (`recently_reported` is that one): with the link live,
    /// `current()` can sit at Idle on a stale report (D2) while this still
    /// says "vouched for", which would wrongly swallow a real bell.
    pub fn vouched_live(&self) -> bool {
        self.reported_while(
            self.ext_link || self.ext_at.is_some_and(|t| t.elapsed() <= STUCK_WORKING),
        )
    }

    /// [F4] The bell-relay gate's version (`PtyPane::process_output`):
    /// strictly time-bounded, `ext_link` never consulted. `vouched_live`
    /// would wrongly treat a live-linked but long-stale Working report as
    /// "owned" here (it decays to Idle in `current()`, D2), swallowing a
    /// real bell with nothing else standing in for it — this asks the
    /// narrower question instead: did the hook report Working/NeedsInput
    /// *recently* (within `STUCK_WORKING`), full stop.
    pub fn recently_reported(&self) -> bool {
        self.reported_while(self.ext_at.is_some_and(|t| t.elapsed() <= STUCK_WORKING))
    }

    /// A bell that arrived after the extension's last status report, recently
    /// enough to still matter — a generic escape hatch for an adapter/TUI
    /// that rings the terminal bell (0x07) for a "needs you" moment its
    /// extension/hook protocol has no event for. Deliberately not
    /// pi-specific: pi has no per-tool approval dialog, and its one blocking
    /// prompt (project trust) is reported via the `project_trust` extension
    /// event instead (D3) — no bell involved. The mechanism stays for
    /// whatever adapter does ring one.
    fn bell_after_ext(&self) -> bool {
        match (self.bell_at, self.ext_at) {
            (Some(b), Some(e)) => b >= e && b.elapsed() < STUCK_WORKING,
            (Some(b), None) => b.elapsed() < STUCK_WORKING,
            _ => false,
        }
    }

    /// Resolve the pane's status, reconciling exact extension signals with
    /// output activity so neither source can leave the badge permanently
    /// wrong (a dead hook stuck on "working", or a stale "waiting" while the
    /// agent is clearly producing output again).
    pub fn current(&self) -> AgentStatus {
        if self.exited {
            return AgentStatus::Exited;
        }
        match self.extension_status {
            // Explicit "needs you" self-heals: if the clearing event never
            // arrives (a cancelled/errored elicitation), a long silence
            // decays it to Waiting rather than pulling the user forever.
            // Deliberately time-based regardless of `ext_link` — unlike
            // Working, a live link only proves the connection is open, not
            // that the agent isn't hard-frozen, and there's no "just quiet"
            // reading to fall back to here.
            Some(AgentStatus::NeedsInput) => {
                let stuck = self.ext_at.is_some_and(|t| t.elapsed() > STUCK_WORKING);
                if stuck && !self.recent_output() {
                    AgentStatus::Waiting
                } else {
                    AgentStatus::NeedsInput
                }
            }
            // Trust "working" while output flows. Past STUCK_WORKING of
            // silence it always decays (D2), but *where* depends on the
            // link:
            // - Live: the hook isn't dead, so this is a quiet stretch, not
            //   an abandoned turn — Idle (·), not an eternal spinner.
            // - Down: no evidence the hook is still around — self-heal to
            //   Waiting (today's behavior).
            Some(AgentStatus::Working) => {
                // D5: the title flipping to rest (`✳`) while quiet settles a
                // Working report immediately — a lost/failed Stop hook
                // otherwise leaves a phantom ● for the full STUCK_WORKING.
                if self.title_resting() && !self.recent_output() {
                    return AgentStatus::Waiting;
                }
                // D5, the sustaining direction: a live spinner title carries
                // a quiet Working report past the decay (the gap between
                // one-shot hooks); a *frozen* spinner still decays below.
                let stuck = self.ext_at.is_some_and(|t| t.elapsed() > STUCK_WORKING);
                if stuck && !self.recent_output() && !self.title_working_live() {
                    if self.ext_link {
                        AgentStatus::Idle
                    } else {
                        AgentStatus::Waiting
                    }
                } else {
                    AgentStatus::Working
                }
            }
            // Resting (waiting/idle): while the link is live, resting means
            // resting (D1) — byte noise (composer echo, answer rendering, a
            // resize) must not repaint a phantom ●. Only promote on fresh
            // output once the link is down (or was never persistent — one-
            // shot hooks). A bell after a resting report is a different
            // signal — a blocked prompt the extension can't see — and
            // promotes to NeedsInput unconditionally, link or no link.
            Some(other) => {
                // D5: a resting title (`✳`) vetoes byte-noise promotion —
                // one-shot hook links are down between hooks, so without it
                // every keystroke echo painted a phantom ●. Ranked below the
                // bell: a blocked agent's bell must win that race.
                if self.recent_output() && !self.ext_link && !self.title_resting() {
                    AgentStatus::Working
                } else if self.bell_after_ext() {
                    AgentStatus::NeedsInput
                } else if self.title_working_live() {
                    AgentStatus::Working
                } else {
                    other
                }
            }
            // No extension/hook: pure heuristics. A recent bell is the
            // classic "pane wants you" signal, surfaced as NeedsInput once
            // quiet, decaying on the same window as the extension path.
            None => {
                let recent_bell = self.bell_at.is_some_and(|t| t.elapsed() < STUCK_WORKING);
                // D5: same title rules and bell-over-title order as the
                // resting-report arm, with no extension/hook installed.
                if self.recent_output() && !self.title_resting() {
                    AgentStatus::Working
                } else if recent_bell {
                    AgentStatus::NeedsInput
                } else if self.title_working_live() {
                    AgentStatus::Working
                } else if self.last_output.is_some() || self.title_resting() {
                    AgentStatus::Waiting
                } else {
                    AgentStatus::Idle
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_status_wins_and_pty_exit_is_sticky() {
        let mut t = StatusTracker::new();
        assert_eq!(t.current(), AgentStatus::Idle);
        t.on_output();
        assert_eq!(t.current(), AgentStatus::Working);
        t.set_extension_status(AgentStatus::NeedsInput);
        assert_eq!(t.current(), AgentStatus::NeedsInput);
        // The PTY EOF marks the pane dead — sticky even if a late extension
        // event (a slow hook's final report) arrives afterwards.
        t.on_exit();
        t.set_extension_status(AgentStatus::Working);
        assert_eq!(t.current(), AgentStatus::Exited);
    }

    /// A socket-reported "exited" must never kill a live pane. The pane's env
    /// is inherited by every descendant process, so a nested pi (a subagent,
    /// a one-shot `pi -p` tool call, a pi run inside a shell pane) reports
    /// *its* session_shutdown as this pane the moment it finishes its work —
    /// while the pane's real child is alive. The report settles the pane to
    /// Waiting; fresh output resurrects Working as usual. Only the PTY EOF
    /// (`on_exit`) is terminal.
    #[test]
    fn extension_exited_is_advisory_and_never_kills_a_live_pane() {
        let mut t = StatusTracker::new();
        t.set_extension_status(AgentStatus::Working);
        t.set_extension_status(AgentStatus::Exited);
        assert_eq!(t.current(), AgentStatus::Waiting);
        // The pane's real child prints again → Working, no dead relic.
        t.on_output();
        assert_eq!(t.current(), AgentStatus::Working);
        t.set_extension_status(AgentStatus::Working);
        assert_eq!(t.current(), AgentStatus::Working);
    }

    #[test]
    fn stale_working_decays_after_silence() {
        let mut t = StatusTracker::new();
        t.set_extension_status(AgentStatus::Working);
        assert_eq!(t.current(), AgentStatus::Working);
        // simulate a long-dead hook: ext_at far in the past, no recent output
        t.ext_at = Some(Instant::now() - STUCK_WORKING - Duration::from_secs(1));
        assert_eq!(t.current(), AgentStatus::Waiting);
        // fresh output resurrects Working
        t.on_output();
        assert_eq!(t.current(), AgentStatus::Working);
    }

    #[test]
    fn stale_needs_input_decays_after_silence() {
        let mut t = StatusTracker::new();
        t.set_extension_status(AgentStatus::NeedsInput);
        assert_eq!(t.current(), AgentStatus::NeedsInput);
        // A dead/cancelled elicitation: the clear never comes. After a long
        // silence, ◆ self-heals to Waiting instead of pulling the user forever.
        t.ext_at = Some(Instant::now() - STUCK_WORKING - Duration::from_secs(1));
        assert_eq!(t.current(), AgentStatus::Waiting);
        // ...but recent output means the agent is still interacting → keep ◆.
        t.on_output();
        assert_eq!(t.current(), AgentStatus::NeedsInput);
    }

    #[test]
    fn bell_is_heuristic_needs_input_only_without_an_extension() {
        let mut t = StatusTracker::new();
        assert_eq!(t.current(), AgentStatus::Idle);
        // A bell while the pane is quiet → heuristic "needs you" (tmux's ! flag).
        t.on_bell();
        assert_eq!(t.current(), AgentStatus::NeedsInput);
        // Active output supersedes it — the pane is clearly working.
        t.on_output();
        assert_eq!(t.current(), AgentStatus::Working);
        // Once an extension reports exact status, the heuristic bell is ignored.
        t.set_extension_status(AgentStatus::Working);
        assert_eq!(t.current(), AgentStatus::Working);
        // A long-stale bell decays away: old output (not recent) + expired bell
        // → Waiting, not a stuck ◆.
        let mut t2 = StatusTracker::new();
        t2.last_output = Some(Instant::now() - Duration::from_secs(10));
        t2.bell_at = Some(Instant::now() - STUCK_WORKING - Duration::from_secs(1));
        assert_eq!(t2.current(), AgentStatus::Waiting);
    }

    #[test]
    fn bell_after_a_waiting_report_promotes_to_needs_input() {
        // The extension/hook says the turn ended → Waiting.
        let mut t = StatusTracker::new();
        t.set_extension_status(AgentStatus::Waiting);
        assert_eq!(t.current(), AgentStatus::Waiting);
        // The adapter then rings the terminal bell for a "needs you" moment
        // its own extension/hook protocol has no event for (NOT pi — see
        // `bell_after_ext`'s doc) → promote the resting pane to NeedsInput.
        t.on_bell();
        assert_eq!(t.current(), AgentStatus::NeedsInput);

        // A bell that predates the extension's report belonged to the finished
        // turn and must NOT promote it. Pin the bell strictly before `ext_at`
        // explicitly: two back-to-back `Instant::now()` calls can read equal at
        // the clock's resolution, and `bell_after_ext` uses `b >= e`, so relying
        // on call order alone made this assertion flaky (~50% on a fast clock).
        let mut t2 = StatusTracker::new();
        t2.set_extension_status(AgentStatus::Waiting);
        t2.bell_at = Some(t2.ext_at.unwrap() - Duration::from_millis(1));
        assert_eq!(t2.current(), AgentStatus::Waiting);
    }

    /// D1, the phantom-pulse pin: with the extension's socket link LIVE, a
    /// resting report stays resting even when fresh bytes arrive — a
    /// keystroke's composer echo, an agent's post-turn answer still
    /// rendering, a resize repaint. This is the bug itself: byte noise used
    /// to repaint ● Working for 1-2s on every one of those, though the
    /// extension (whose agent_start/agent_settled are exact) reported nothing.
    #[test]
    fn fresh_output_does_not_override_resting_while_link_is_live() {
        let mut t = StatusTracker::new();
        t.set_ext_link(true);
        t.set_extension_status(AgentStatus::Waiting);
        assert_eq!(t.current(), AgentStatus::Waiting);
        t.on_output(); // composer echo / answer rendering / a resize repaint
        assert_eq!(
            t.current(),
            AgentStatus::Waiting,
            "byte noise must not repaint a phantom ● while the link is live"
        );
    }

    /// The fallback D1 preserves: once the link is down (or was never
    /// persistent — Claude Code's hooks are one-shot connections that flick
    /// it up and back down), a resting report is the last thing roost heard,
    /// not necessarily the truth right now — fresh output is the best
    /// evidence left that a new turn started.
    #[test]
    fn fresh_output_promotes_resting_when_link_is_down() {
        let mut t = StatusTracker::new();
        // ext_link defaults to false (down) — never set here.
        t.set_extension_status(AgentStatus::Waiting);
        assert_eq!(t.current(), AgentStatus::Waiting);
        t.on_output(); // new turn started, no "working" event came
        assert_eq!(t.current(), AgentStatus::Working);
    }

    /// D2: a `Working` report always decays after `STUCK_WORKING` of silence
    /// — a slow local model's silent prefill routinely runs past 45s, and
    /// that's why the constant is generous, not a reason to stop the clock —
    /// but *where* it decays to depends on whether the reporting link is up.
    #[test]
    fn working_decay_target_depends_on_ext_link() {
        // Live link: the socket itself proves the hook isn't dead, so a long
        // silence reads as a quiet stretch (e.g. local-model prefill), not
        // an abandoned turn — Idle (·), not an eternal ● spinner and not a
        // misleading ○ "your turn" either. The hook's own next event (or
        // output resuming) returns it to Working the moment there's news.
        let mut live = StatusTracker::new();
        live.set_ext_link(true);
        live.set_extension_status(AgentStatus::Working);
        live.ext_at = Some(Instant::now() - STUCK_WORKING - Duration::from_secs(1));
        assert_eq!(live.current(), AgentStatus::Idle);
        live.on_output();
        assert_eq!(live.current(), AgentStatus::Working, "output resuming returns it to Working");

        // Down link: no live evidence the hook is still around at all —
        // today's behavior, self-heal to Waiting.
        let mut down = StatusTracker::new();
        down.set_extension_status(AgentStatus::Working);
        down.ext_at = Some(Instant::now() - STUCK_WORKING - Duration::from_secs(1));
        assert_eq!(down.current(), AgentStatus::Waiting);
        down.on_output();
        assert_eq!(down.current(), AgentStatus::Working, "output resuming returns it to Working");
    }

    /// NeedsInput's decay stays time-based no matter what the link is doing
    /// — unlike Working, there's no "the hook's alive, just quiet" reading
    /// for a stuck ◆ to fall back to, so a live link buys it nothing: self-
    /// healing to Waiting beats pulling the user to the pane forever either
    /// way.
    #[test]
    fn needs_input_decay_is_unaffected_by_ext_link() {
        let mut t = StatusTracker::new();
        t.set_ext_link(true);
        t.set_extension_status(AgentStatus::NeedsInput);
        assert_eq!(t.current(), AgentStatus::NeedsInput);
        t.ext_at = Some(Instant::now() - STUCK_WORKING - Duration::from_secs(1));
        assert_eq!(
            t.current(),
            AgentStatus::Waiting,
            "NeedsInput must still decay on a long silence even with the link live"
        );
    }

    /// D5: a Working title (an agent's spinner frame in the terminal title)
    /// is active-work evidence — it promotes a resting report even with no
    /// output at all, and decays like a report once stale AND silent, so a
    /// hung agent's leftover spinner can't pin ● forever.
    #[test]
    fn title_working_promotes_a_resting_report_and_decays_when_stale() {
        let mut t = StatusTracker::new();
        t.set_title_signal(true);
        t.set_extension_status(AgentStatus::Waiting);
        t.set_title_status(Some(AgentStatus::Working));
        assert_eq!(t.current(), AgentStatus::Working, "spinner title needs no output");
        // Stale title + silence: fall back to the report — the same
        // self-heal as a stale Working report from a dead hook.
        t.title_at = Some(Instant::now() - STUCK_WORKING - Duration::from_secs(1));
        assert_eq!(t.current(), AgentStatus::Waiting);
        // Fresh output revives trust in the (unchanged) spinner title.
        t.on_output();
        assert_eq!(t.current(), AgentStatus::Working);
    }

    /// D5: the resting title (`✳`) vetoes the byte-noise promotion — with
    /// one-shot hook links (down between hooks), every keystroke echo on a
    /// resting claude pane painted a phantom ● before this.
    #[test]
    fn title_resting_vetoes_byte_noise_promotion() {
        let mut t = StatusTracker::new();
        t.set_title_signal(true);
        t.set_extension_status(AgentStatus::Waiting);
        t.set_title_status(Some(AgentStatus::Waiting));
        t.on_output();
        assert_eq!(t.current(), AgentStatus::Waiting);
        // Same veto on a pane with no extension/hook at all.
        let mut t2 = StatusTracker::new();
        t2.set_title_signal(true);
        t2.set_title_status(Some(AgentStatus::Waiting));
        t2.on_output();
        assert_eq!(t2.current(), AgentStatus::Waiting);
    }

    /// D5: a lost/failed Stop hook otherwise leaves Working dangling for the
    /// full STUCK_WORKING — the title flipping to rest settles it the moment
    /// the pane is quiet, while output still flowing keeps the turn alive.
    #[test]
    fn title_resting_settles_a_working_report_once_quiet() {
        let mut t = StatusTracker::new();
        t.set_title_signal(true);
        t.set_extension_status(AgentStatus::Working);
        t.set_title_status(Some(AgentStatus::Waiting));
        assert_eq!(t.current(), AgentStatus::Waiting);
        t.on_output();
        assert_eq!(t.current(), AgentStatus::Working, "output flowing = turn still live");
    }

    /// D5: the title is screen inference — a live exact link outranks it
    /// (same deference byte promotion shows, and the same lesson: gate a
    /// heuristic on the exact signal's liveness, not a clock). And no title
    /// state ever touches ◆: NeedsInput is an explicit ask only its own
    /// clearing event (or decay) resolves.
    #[test]
    fn title_defers_to_a_live_ext_link_and_never_touches_needs_input() {
        let mut t = StatusTracker::new();
        t.set_title_signal(true);
        t.set_ext_link(true);
        t.set_extension_status(AgentStatus::Waiting);
        t.set_title_status(Some(AgentStatus::Working));
        assert_eq!(t.current(), AgentStatus::Waiting);

        let mut t2 = StatusTracker::new();
        t2.set_title_signal(true);
        t2.set_extension_status(AgentStatus::NeedsInput);
        t2.set_title_status(Some(AgentStatus::Working));
        assert_eq!(t2.current(), AgentStatus::NeedsInput);
        t2.set_title_status(Some(AgentStatus::Waiting));
        assert_eq!(t2.current(), AgentStatus::NeedsInput);
    }

    /// D5: with no extension/hook installed at all, the title alone gives a
    /// pane exact-ish status — and a title that stops matching any agent
    /// pattern clears the signal rather than freezing the last reading.
    #[test]
    fn title_alone_reports_status_with_no_extension_installed() {
        let mut t = StatusTracker::new();
        t.set_title_signal(true);
        t.set_title_status(Some(AgentStatus::Working));
        assert_eq!(t.current(), AgentStatus::Working);
        t.set_title_status(Some(AgentStatus::Waiting));
        assert_eq!(t.current(), AgentStatus::Waiting, "✳ with no output yet is at rest, not Idle");
        t.set_title_status(None);
        assert_eq!(t.current(), AgentStatus::Idle);
    }

    /// D5's gate: the title channel is dead until the app layer says an
    /// agent is running. A vt100 title outlives its process (nothing clears
    /// it), so an exited claude's leftover `✳` must not veto Working for the
    /// shell commands that follow — `printf '\e]2;✳ x\a'; yes` in a plain
    /// shell pane must still read ● while it streams.
    #[test]
    fn title_is_ignored_until_enabled_and_cleared_on_disable() {
        let mut t = StatusTracker::new();
        t.set_title_status(Some(AgentStatus::Waiting)); // gate off (default)
        t.on_output();
        assert_eq!(t.current(), AgentStatus::Working, "disabled title must not veto Working");

        // The agent exits, the pane demotes: disabling clears the stored
        // signal, so a later re-promotion starts clean instead of
        // inheriting this run's last title.
        t.set_title_signal(true);
        assert_eq!(t.current(), AgentStatus::Waiting, "enabled: the ✳ veto applies");
        t.set_title_signal(false);
        t.set_title_signal(true);
        assert_eq!(t.current(), AgentStatus::Working, "re-enabled with no stored title");
    }

    /// D5 ranks below the bell: a blocked agent's bell is the one "needs
    /// you" signal it can still emit while a (possibly stale, possibly
    /// still-animating) spinner title claims work — ◆ must win that race,
    /// in both the resting-report arm and the no-extension arm.
    #[test]
    fn bell_outranks_a_working_title() {
        let mut t = StatusTracker::new();
        t.set_title_signal(true);
        t.set_extension_status(AgentStatus::Waiting);
        t.set_title_status(Some(AgentStatus::Working));
        t.on_bell();
        assert_eq!(t.current(), AgentStatus::NeedsInput);

        let mut t2 = StatusTracker::new();
        t2.set_title_signal(true);
        t2.set_title_status(Some(AgentStatus::Working));
        t2.on_bell();
        assert_eq!(t2.current(), AgentStatus::NeedsInput);
    }

    /// D5, the sustaining direction: a spinner title still updating carries
    /// a quiet Working report past `STUCK_WORKING` (a long silent tool call
    /// between one-shot hooks); once the title itself goes stale too — a
    /// frozen spinner, i.e. a hung agent — the ordinary decay applies.
    #[test]
    fn spinner_title_sustains_a_working_report_past_decay() {
        let mut t = StatusTracker::new();
        t.set_title_signal(true);
        t.set_extension_status(AgentStatus::Working);
        t.set_title_status(Some(AgentStatus::Working));
        t.ext_at = Some(Instant::now() - STUCK_WORKING - Duration::from_secs(1));
        assert_eq!(t.current(), AgentStatus::Working, "fresh spinner sustains the report");
        t.title_at = Some(Instant::now() - STUCK_WORKING - Duration::from_secs(1));
        assert_eq!(t.current(), AgentStatus::Waiting, "frozen spinner decays as before");
    }

    /// [F4] `vouched_live` and `recently_reported` must disagree exactly
    /// where the split exists to make them: a Working report gone stale
    /// while its link is live. `current()`'s own Working arm treats that as
    /// unowned (decays to Idle, D2) — so `pty.rs`'s bell-relay gate (which
    /// asks `recently_reported`) must see "not reported" and let a real
    /// bell through, while the destructive-close guard (which asks
    /// `vouched_live`, via `PaneBackend::status_reported`) still sees
    /// "vouched for", because the hook provably isn't dead.
    #[test]
    fn vouched_live_and_recently_reported_disagree_on_a_stale_working_report_with_a_live_link() {
        let mut t = StatusTracker::new();
        t.set_ext_link(true);
        t.set_extension_status(AgentStatus::Working);
        t.ext_at = Some(Instant::now() - STUCK_WORKING - Duration::from_secs(1));
        assert_eq!(t.current(), AgentStatus::Idle, "setup: this is D2's live-link Working decay");

        assert!(
            t.vouched_live(),
            "the close guard must still trust a report backed by a live link"
        );
        assert!(
            !t.recently_reported(),
            "the bell relay must not be gated by a live link on a stale report — nothing else is \
             compensating for a real bell here"
        );
    }
}
