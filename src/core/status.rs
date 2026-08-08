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

/// A `Working` reported by an extension/hook decays to `Waiting` after this
/// much silence, so a badge doesn't stick forever if the hook that would
/// report "done" dies mid-session. Generous, to not misread a legitimately
/// thinking agent that just isn't printing. Only in effect while `ext_link`
/// is down — a live reporting connection is stronger evidence than a clock
/// (see `current()`'s `Working` arm): a slow local model's silent prefill
/// routinely runs past this on hardware the hook is still very much alive on.
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
    /// Is the extension/hook's status-socket connection for this pane
    /// currently open? (D2/D1.) Pushed in from `infra::sock` via
    /// `set_ext_link`, which tracks real connection liveness end to end —
    /// this is not itself a timer or a heuristic. Starts `false`: with no
    /// connection yet reported, there is nothing to trust over the
    /// heuristics, which is exactly today's fallback behavior.
    ext_link: bool,
    /// Screen-derived status parsed from the pane's terminal title (D5).
    /// Claude Code publishes its state there — a braille spinner frame
    /// prefix while working, `✳ ` at rest — and its hook connections are
    /// one-shot, so between hooks this is the only exact-ish signal a claude
    /// pane has. It ranks *between* extension reports and byte heuristics:
    /// never consulted while `ext_link` is live (a live reporting connection
    /// outranks screen inference — same rule as output promotion), never
    /// able to touch NeedsInput or Exited, and never consulted at all unless
    /// `title_enabled` says an agent is actually running (see below). Pushed
    /// from `infra::pty` on every title change; `None` for titles that match
    /// no known agent pattern.
    title_status: Option<AgentStatus>,
    /// When the title string last changed. For an *animating* spinner this
    /// refreshes every frame — it's a liveness heartbeat, not a transition
    /// marker — so the `STUCK_WORKING` bound in `title_working_live` only
    /// ever fires for a title that froze (a hung agent), which is exactly
    /// the case it exists to catch.
    title_at: Option<Instant>,
    /// Gate on the whole title channel, pushed by the app layer (spawn +
    /// `observe_panes`): true only while the pane's effective adapter is an
    /// agent, false for plain shells. A vt100 title outlives the process
    /// that set it (nothing ever clears it — not even RIS), so an agent that
    /// ran in a shell pane and exited would otherwise leave a permanent `✳`
    /// vetoing Working for every later command in that pane. Same gate, same
    /// reason, as `display_name_live`'s shell-title rule.
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

    /// The pane's status-socket connection went up or down (D2). `infra::sock`
    /// is the source of truth: up on that connection's first accepted
    /// status/session line, down on its close (EOF/error) — never a timer or
    /// a heuristic here.
    pub fn set_ext_link(&mut self, up: bool) {
        self.ext_link = up;
    }

    /// The pane's terminal title changed and parsed to `s` (D5) — `None`
    /// when the new title matches no known agent pattern, which clears any
    /// previous title signal (the agent stopped publishing state there).
    /// Called on title-string changes; an animating spinner lands here every
    /// frame, which is what makes `title_at` a liveness heartbeat.
    pub fn set_title_status(&mut self, s: Option<AgentStatus>) {
        self.title_status = s;
        self.title_at = Some(Instant::now());
    }

    /// D5's gate: the app layer's answer to "is an agent actually running in
    /// this pane right now?" (spawn adapter, kept current by
    /// `observe_panes`'s promote/demote). While off, the title channel is
    /// dead weight — stored but never consulted — because a leftover title
    /// from an exited agent is otherwise trusted forever. Disabling clears
    /// the stored signal too: the next agent run starts from a clean slate
    /// instead of inheriting the previous run's last title.
    pub fn set_title_signal(&mut self, enabled: bool) {
        self.title_enabled = enabled;
        if !enabled {
            self.title_status = None;
            self.title_at = None;
        }
    }

    /// D5: a Working title is trusted while output flows or the title itself
    /// is fresh (`STUCK_WORKING` since it last changed — an *animating*
    /// spinner refreshes that clock every frame, so this bound only fires
    /// for a frozen title, i.e. a hung agent, which then decays to the
    /// ordinary heuristics same as a dead hook's stale report). Never while
    /// the extension link is live: a live exact source outranks screen
    /// inference (same rule byte promotion follows).
    fn title_working_live(&self) -> bool {
        self.title_enabled
            && !self.ext_link
            && self.title_status == Some(AgentStatus::Working)
            && (self.recent_output()
                || self.title_at.is_some_and(|t| t.elapsed() <= STUCK_WORKING))
    }

    /// D5: the title says the agent is at rest (`✳`). No time decay — while
    /// the gate says the agent is running, a resting claim can't pin a wrong
    /// ● or ◆, it only suppresses byte-noise promotion, and the title flips
    /// to a spinner the instant a real turn starts; the moment the agent
    /// exits, the gate (not a clock) kills the signal. Same live-link
    /// deference as `title_working_live`.
    fn title_resting(&self) -> bool {
        self.title_enabled && !self.ext_link && self.title_status == Some(AgentStatus::Waiting)
    }

    fn recent_output(&self) -> bool {
        self.last_output.is_some_and(|t| t.elapsed() < ACTIVE_WINDOW)
    }

    /// Shared core of `vouched_live`/`recently_reported`: is the current
    /// extension report a Working/NeedsInput one at all, and if so does
    /// `live` — the caller's own definition of "still trustworthy" — hold?
    /// A resting report that `current()` promoted to Working/NeedsInput was
    /// promoted by a heuristic (fresh output, or a bell), not by the report
    /// itself, so it never counts here either way.
    fn reported_while(&self, live: bool) -> bool {
        match self.extension_status {
            Some(AgentStatus::Working | AgentStatus::NeedsInput) => live,
            _ => false,
        }
    }

    /// [U22] Is the status `current()` reports one an extension/hook
    /// actually *reported*, or one roost inferred from PTY traffic? — for
    /// the destructive-close guard (`PaneBackend::status_reported`)
    /// specifically. `current()` folds the two together on purpose — a
    /// badge should read the same however roost learned it — but the guard
    /// cannot afford that: a hook saying "working" means a turn is in
    /// flight, while `recent_output()` means bytes arrived, which is
    /// equally true of `ls`.
    ///
    /// [F4] Link-aware, and deliberately so *only* here: a report counts as
    /// live while its socket connection is up (`ext_link` — the strongest
    /// evidence there is that the hook isn't dead), OR, absent that, while
    /// it's within `STUCK_WORKING` of arriving. This can only ever be *more*
    /// permissive than the plain elapsed check (an OR of it), so it never
    /// disagrees with `current()` in the direction that matters for a
    /// destructive guard — it may still call a heuristic-driven Working
    /// "vouched for" while the link is live and the report itself is stale,
    /// but it can never call a truly dead hook's stale report "vouched for"
    /// once both conditions lapse. Do not reuse this for the bell relay
    /// (`recently_reported` is that one): with the link live, `current()`'s
    /// Working arm can sit at Idle on a long-stale report (D2) — this
    /// method still (correctly, for a close guard) says "vouched for" there,
    /// which would wrongly suppress an actual bell with nothing else
    /// standing in for it.
    pub fn vouched_live(&self) -> bool {
        self.reported_while(
            self.ext_link || self.ext_at.is_some_and(|t| t.elapsed() <= STUCK_WORKING),
        )
    }

    /// [F4] The bell-relay gate's version (`PtyPane::process_output`):
    /// strictly time-bounded, `ext_link` never consulted. That gate relays a
    /// pane's own bell to the host only when nothing else already owns the
    /// attention it would draw — and a live-linked but long-silent Working
    /// report is exactly the case `current()` itself no longer treats as
    /// "owned" (it decays to Idle, D2), so `vouched_live` would wrongly
    /// swallow a real bell there with no compensating signal. This asks the
    /// narrower, honest question instead: did an extension/hook report
    /// Working/NeedsInput *recently* (within `STUCK_WORKING`), full stop.
    pub fn recently_reported(&self) -> bool {
        self.reported_while(self.ext_at.is_some_and(|t| t.elapsed() <= STUCK_WORKING))
    }

    /// A bell that arrived *after* the extension's last status report, and
    /// recently enough to still matter. This is a generic escape hatch for
    /// an adapter/TUI that genuinely rings the terminal bell (0x07) for a
    /// "needs you" moment its extension/hook protocol has no event for —
    /// deliberately NOT pi-specific: verified against installed pi 0.81.1
    /// (and pi-tui), every `\x07` either emits is an OSC string terminator
    /// (OSC 8/133/52), never an audible/attention BEL. pi has no per-tool
    /// approval dialog at all; its one blocking prompt (project trust) is
    /// reported directly via the `project_trust` extension event roost.ts
    /// subscribes to (D3) — no bell involved. The mechanism stays for
    /// whatever adapter, present or future, *does* ring one.
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
            //
            // Deliberately time-based regardless of `ext_link`, unlike the
            // Working arm below: a live link only proves the *connection* is
            // open, not that the agent isn't hard-frozen mid-prompt, and
            // unlike Working there is no "hook's still there, just quiet"
            // reading of a stuck ◆ to fall back to — self-healing to Waiting
            // beats pulling the user to the pane forever either way.
            Some(AgentStatus::NeedsInput) => {
                let stuck = self.ext_at.is_some_and(|t| t.elapsed() > STUCK_WORKING);
                if stuck && !self.recent_output() {
                    AgentStatus::Waiting
                } else {
                    AgentStatus::NeedsInput
                }
            }
            // Trust "working" while output flows. Past STUCK_WORKING of
            // silence it always decays (D2) — a slow local model's silent
            // prefill is exactly why STUCK_WORKING is generous, not a reason
            // to stop the clock — but *where* it decays to depends on
            // whether the reporting link is still up:
            // - Live: the hook itself isn't dead (the socket says so), so
            //   this is a quiet stretch, not an abandoned turn. Idle (·) is
            //   the calm reading — not an eternal spinner, not a misleading
            //   "your turn" — and the hook's own next event (or output
            //   resuming) returns it to Working the moment there's news.
            // - Down: no live evidence the hook is still around at all —
            //   today's behavior, self-heal to Waiting.
            Some(AgentStatus::Working) => {
                // D5: the title flipping to rest (`✳`) while the pane is
                // quiet settles a Working report immediately — Claude's
                // hooks are one-shot connections, so a lost/failed Stop hook
                // otherwise leaves a phantom ● for the full STUCK_WORKING.
                if self.title_resting() && !self.recent_output() {
                    return AgentStatus::Waiting;
                }
                // D5, the sustaining direction: a live spinner title carries
                // a quiet Working report past the decay — a >45 s silent
                // tool call with the spinner still animating is exactly the
                // "between one-shot hooks" gap the title channel closes. A
                // *frozen* spinner doesn't qualify (`title_working_live`'s
                // own bound), so a hung agent still decays below.
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
            // For a resting state (waiting/idle): while the extension/hook's
            // link is live, resting means resting (D1) — the extension sends
            // agent_start within ms of a real turn, so byte noise (composer
            // echo, post-turn answer rendering, a resize repaint) must not
            // repaint a phantom ●. Only promote on fresh output once that
            // link is down (or was never persistent — Claude's hooks are
            // one-shot connections): the same fallback as always, for a
            // report source that genuinely under-reports turn starts. A bell
            // landing after this resting report is a different signal
            // entirely — the agent is now blocked on a prompt the extension
            // can't see (pi's permission gate) — so it promotes to
            // NeedsInput unconditionally, link or no link.
            Some(other) => {
                // D5: a resting title (`✳`) vetoes the byte-noise promotion
                // — Claude's one-shot hook links are down between hooks, so
                // without it every keystroke echo on a resting claude pane
                // painted a phantom ●. A Working title is active-work
                // evidence too, but ranked *below* the bell: a bell is the
                // one "needs you" signal a blocked agent can still emit
                // while its (possibly stale, possibly still-animating)
                // spinner title claims work — ◆ must not lose that race.
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
            // No extension/hook: pure heuristics. A recent bell (0x07) is the
            // classic "pane wants you" signal (tmux monitor-bell) — surface it
            // as NeedsInput once the pane is quiet, decaying on the same window
            // as the extension path so a stray bell can't pin ◆ forever. Active
            // output still means Working; longer silence means Waiting.
            None => {
                let recent_bell = self.bell_at.is_some_and(|t| t.elapsed() < STUCK_WORKING);
                // D5: same title rules (and the same bell-over-title order)
                // as the resting-report arm — an agent that publishes state
                // in its title gets exact-ish status even with no
                // extension/hook installed at all.
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
        assert_eq!(
            live.current(),
            AgentStatus::Working,
            "output resuming returns it to Working"
        );

        // Down link: no live evidence the hook is still around at all —
        // today's behavior, self-heal to Waiting.
        let mut down = StatusTracker::new();
        down.set_extension_status(AgentStatus::Working);
        down.ext_at = Some(Instant::now() - STUCK_WORKING - Duration::from_secs(1));
        assert_eq!(down.current(), AgentStatus::Waiting);
        down.on_output();
        assert_eq!(
            down.current(),
            AgentStatus::Working,
            "output resuming returns it to Working"
        );
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
        assert_eq!(
            t.current(),
            AgentStatus::Idle,
            "setup: this is D2's live-link Working decay"
        );

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
