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

impl AgentStatus {
    pub fn badge(self) -> &'static str {
        match self {
            AgentStatus::Working => "●",
            AgentStatus::NeedsInput => "◆",
            AgentStatus::Waiting => "○",
            AgentStatus::Idle => "·",
            AgentStatus::Exited => "✕",
        }
    }
}

pub struct StatusTracker {
    last_output: Option<Instant>,
    exited: bool,
    /// Exact status pushed by an extension/hook, plus when it arrived.
    extension_status: Option<AgentStatus>,
    ext_at: Option<Instant>,
}

impl StatusTracker {
    pub fn new() -> Self {
        Self { last_output: None, exited: false, extension_status: None, ext_at: None }
    }

    pub fn on_output(&mut self) {
        self.last_output = Some(Instant::now());
    }

    pub fn on_exit(&mut self) {
        self.exited = true;
    }

    pub fn set_extension_status(&mut self, s: AgentStatus) {
        if s == AgentStatus::Exited {
            self.exited = true;
        }
        self.extension_status = Some(s);
        self.ext_at = Some(Instant::now());
    }

    fn recent_output(&self) -> bool {
        self.last_output.is_some_and(|t| t.elapsed() < ACTIVE_WINDOW)
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
            // Explicit "needs you" is always honored.
            Some(AgentStatus::NeedsInput) => AgentStatus::NeedsInput,
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
            // turn started even if no "working" event arrived.
            Some(other) => {
                if self.recent_output() {
                    AgentStatus::Working
                } else {
                    other
                }
            }
            None => match self.last_output {
                Some(_) if self.recent_output() => AgentStatus::Working,
                Some(_) => AgentStatus::Waiting,
                None => AgentStatus::Idle,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_status_wins_and_exit_is_sticky() {
        let mut t = StatusTracker::new();
        assert_eq!(t.current(), AgentStatus::Idle);
        t.on_output();
        assert_eq!(t.current(), AgentStatus::Working);
        t.set_extension_status(AgentStatus::NeedsInput);
        assert_eq!(t.current(), AgentStatus::NeedsInput);
        t.set_extension_status(AgentStatus::Exited);
        t.set_extension_status(AgentStatus::Working);
        // exited is sticky even if a late event arrives
        t.on_exit();
        assert_eq!(t.current(), AgentStatus::Exited);
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
    fn fresh_output_overrides_stale_waiting() {
        let mut t = StatusTracker::new();
        t.set_extension_status(AgentStatus::Waiting);
        assert_eq!(t.current(), AgentStatus::Waiting);
        t.on_output(); // new turn started, no "working" event came
        assert_eq!(t.current(), AgentStatus::Working);
    }
}
