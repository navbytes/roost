//! Per-pane status: Working / NeedsInput / Waiting / Idle / Exited.
//!
//! Two signal sources (design doc §6.3–6.4):
//! 1. Extension events (exact) — pi's roost.ts extension / Claude Code hooks
//!    reporting over the unix socket. TODO(M3): socket listener.
//! 2. Output heuristics (fallback) — recent PTY bytes ⇒ Working, silence ⇒
//!    Waiting/Idle. Prompt-pattern detection for NeedsInput is TODO(M3).

use std::time::{Duration, Instant};

/// A `Working` reported by an extension/hook decays to `Waiting` after this
/// much silence, so a badge doesn't stick forever if the hook that would
/// report "done" dies mid-session. Generous, to not misread a legitimately
/// thinking agent that just isn't printing.
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
    /// Last time the pane rang the terminal bell (0x07). The decades-old
    /// "program wants your attention" signal (tmux's monitor-bell), used as a
    /// heuristic NeedsInput when no extension/hook is installed.
    bell_at: Option<Instant>,
}

impl StatusTracker {
    pub fn new() -> Self {
        Self {
            last_output: None,
            exited: false,
            extension_status: None,
            ext_at: None,
            bell_at: None,
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
    /// (`on_exit`) — and a socket "exited" is hearsay. The pane's env
    /// (ROOST_PANE/ROOST_TOKEN) is inherited by every descendant process, so
    /// a *nested* pi — a subagent, a one-shot `pi -p` run by the agent's own
    /// bash tool, a pi launched by hand inside a shell pane — loads the same
    /// global extension and reports its `session_shutdown` as this pane when
    /// it finishes its work, while the pane's real child is alive and well.
    /// Believing it would render a live pane dead (with Enter then killing
    /// the live agent to "relaunch" it). When the process really is exiting,
    /// the EOF lands moments later and marks the pane dead for real; a
    /// session that ended without death is exactly "at rest" — Waiting.
    pub fn set_extension_status(&mut self, s: AgentStatus) {
        let s = if s == AgentStatus::Exited { AgentStatus::Waiting } else { s };
        self.extension_status = Some(s);
        self.ext_at = Some(Instant::now());
    }

    fn recent_output(&self) -> bool {
        self.last_output.is_some_and(|t| t.elapsed() < ACTIVE_WINDOW)
    }

    /// [U22] Is the status `current()` reports one an extension/hook actually
    /// *reported*, or one roost inferred from PTY traffic?
    ///
    /// `current()` folds the two together on purpose — a badge should read
    /// the same however roost learned it — but the destructive-close guard
    /// cannot afford that: a hook saying "working" means a turn is in
    /// flight, while `recent_output()` means bytes arrived, which is equally
    /// true of `ls`. A decayed report counts as *not* reported: once the
    /// hook has gone quiet past `STUCK_WORKING`, `current()` has stopped
    /// believing it, and so does this.
    pub fn reported(&self) -> bool {
        let live = self.ext_at.is_some_and(|t| t.elapsed() <= STUCK_WORKING);
        match self.extension_status {
            Some(AgentStatus::Working | AgentStatus::NeedsInput) => live,
            // A resting report that `current()` promoted to Working/NeedsInput
            // was promoted by a heuristic (fresh output, or a bell), not by
            // the report itself.
            _ => false,
        }
    }

    /// A bell that arrived *after* the extension's last status report, and
    /// recently enough to still matter. This is the signal the extension can't
    /// give us: pi exposes no event for its built-in permission/approval prompt,
    /// but pi rings the bell for it — so a bell landing after a resting
    /// "waiting" means a fresh prompt the hook didn't (couldn't) report.
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
            // Explicit "needs you" is honored, but self-heals: if the clearing
            // event never arrives (an elicitation the agent cancelled or that
            // errored out), a long silence decays it to Waiting so ◆ doesn't
            // pull the user to a pane forever. Mirrors the Working decay below.
            Some(AgentStatus::NeedsInput) => {
                let stuck = self.ext_at.is_some_and(|t| t.elapsed() > STUCK_WORKING);
                if stuck && !self.recent_output() {
                    AgentStatus::Waiting
                } else {
                    AgentStatus::NeedsInput
                }
            }
            // Trust "working" while output flows; if it goes quiet for a long
            // time the reporting hook probably died — self-heal to Waiting.
            Some(AgentStatus::Working) => {
                let stuck = self.ext_at.is_some_and(|t| t.elapsed() > STUCK_WORKING);
                if stuck && !self.recent_output() {
                    AgentStatus::Waiting
                } else {
                    AgentStatus::Working
                }
            }
            // For a resting state (waiting/idle), fresh output means a new
            // turn started even if no "working" event arrived; and a bell that
            // landed after this resting report means the agent is now blocked on
            // a prompt the extension can't see (pi's permission gate) — promote
            // it to NeedsInput so it still pulls the user.
            Some(other) => {
                if self.recent_output() {
                    AgentStatus::Working
                } else if self.bell_after_ext() {
                    AgentStatus::NeedsInput
                } else {
                    other
                }
            }
            // No extension/hook: pure heuristics. A recent bell (0x07) is the
            // classic "pane wants you" signal (tmux monitor-bell) — surface it
            // as NeedsInput once the pane is quiet, decaying on the same window
            // as the extension path so a stray bell can't pin ◆ forever. Active
            // output still means Working; longer silence means Waiting.
            None => {
                let recent_bell = self.bell_at.is_some_and(|t| t.elapsed() < STUCK_WORKING);
                if self.recent_output() {
                    AgentStatus::Working
                } else if recent_bell {
                    AgentStatus::NeedsInput
                } else if self.last_output.is_some() {
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
        // The extension (pi hook) says the turn ended → Waiting.
        let mut t = StatusTracker::new();
        t.set_extension_status(AgentStatus::Waiting);
        assert_eq!(t.current(), AgentStatus::Waiting);
        // pi then rings the bell for a built-in permission prompt it can't
        // report as an event → promote the resting pane to NeedsInput.
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

    #[test]
    fn fresh_output_overrides_stale_waiting() {
        let mut t = StatusTracker::new();
        t.set_extension_status(AgentStatus::Waiting);
        assert_eq!(t.current(), AgentStatus::Waiting);
        t.on_output(); // new turn started, no "working" event came
        assert_eq!(t.current(), AgentStatus::Working);
    }
}
