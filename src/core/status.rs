//! Per-pane status: Working / NeedsInput / Waiting / Idle / Exited.
//!
//! Two signal sources (design doc §6.3–6.4):
//! 1. Extension events (exact) — pi's roost.ts extension / Claude Code hooks
//!    reporting over the unix socket. TODO(M3): socket listener.
//! 2. Output heuristics (fallback) — recent PTY bytes ⇒ Working, silence ⇒
//!    Waiting/Idle. Prompt-pattern detection for NeedsInput is TODO(M3).

use std::time::{Duration, Instant};

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
    /// Exact status pushed by an extension/hook; trumps heuristics until the
    /// next output burst invalidates it.
    extension_status: Option<AgentStatus>,
}

impl StatusTracker {
    pub fn new() -> Self {
        Self { last_output: None, exited: false, extension_status: None }
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
    }

    pub fn current(&self) -> AgentStatus {
        if self.exited {
            return AgentStatus::Exited;
        }
        if let Some(s) = self.extension_status {
            return s;
        }
        match self.last_output {
            Some(t) if t.elapsed() < Duration::from_secs(2) => AgentStatus::Working,
            Some(_) => AgentStatus::Waiting,
            None => AgentStatus::Idle,
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
}
