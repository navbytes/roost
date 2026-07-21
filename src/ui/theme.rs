//! Chrome design tokens (DESIGN-ui.md §2, contract C1).
//!
//! Thesis: chrome is ink · paper · one red; program output keeps its own
//! colors. Everything roost draws itself (tab bar, borders, badges, stack
//! chrome, hint bar, modals) is built from exactly the items in this file —
//! one accent hue plus a warm-gray ramp. `ok`/`warn`/`info` are deliberately
//! **not** defined here: those are program-output-only hues (§2); defining
//! them would invite casual reuse and dilute the one-red rule.
//!
//! Every token here has a chrome call site except `BG`, which carries its
//! own narrow `#[allow(dead_code)]` — no module-wide allow (see its doc).

use std::time::Duration;

use ratatui::style::Color;

use crate::core::app::TabSummary;
use crate::core::status::AgentStatus;

// ---- Chrome color tokens (§2 token table; exact values, not re-derived) ----

/// "Paper". Explicit fill only when a surface needs it (§2 background policy)
/// — roost otherwise leaves the terminal's own background showing through.
#[allow(dead_code)] // reserved: §2 says BG may never gain a call site (roost never repaints the bg)
pub const BG: Color = Color::Rgb(21, 18, 15);
/// Primary ink.
pub const FG: Color = Color::Rgb(234, 229, 223);
/// Secondary ink.
pub const MUTED: Color = Color::Rgb(143, 137, 131);
/// Tertiary ink.
pub const DIM: Color = Color::Rgb(87, 82, 75);
/// Structural rules: unfocused borders, separators, focused-row/flash bg.
pub const RULE: Color = Color::Rgb(51, 47, 42);
/// The one red: focus, needs-input, working (pulse phase A), key hints.
pub const ACCENT: Color = Color::Rgb(255, 86, 60);
/// Dimmed red: exited glyph, working pulse phase B, dead/alt-warning bars.
pub const ACCENT_DIM: Color = Color::Rgb(168, 58, 40);
/// Tab bar row background.
pub const TAB_STRIP: Color = Color::Rgb(27, 23, 19);
/// Hint bar row background.
pub const BAR: Color = Color::Rgb(33, 29, 25);

/// Active tab's label cell background: `Color::Reset`, not one of the nine
/// `Rgb` tokens above — a deliberate sentinel (§2 background policy) so the
/// active tab visually fuses with whatever the terminal's own background
/// is, rather than painting `TAB_STRIP` under it or assuming roost owns the
/// terminal bg.
pub const ACTIVE_TAB_BG: Color = Color::Reset;

// ---- Chrome glyphs (§2 glyph inventory; all single-width) ----

// Status glyphs (C5 table).
pub const GLYPH_WORKING: char = '●'; // U+25CF
pub const GLYPH_NEEDS_INPUT: char = '◆'; // U+25C6
pub const GLYPH_WAITING: char = '○'; // U+25CB
pub const GLYPH_IDLE: char = '·'; // U+00B7
pub const GLYPH_EXITED: char = '✕'; // U+2715

// Structural chrome glyphs.
/// Active-tab / focused-collapsed-row marker.
pub const MARKER_ACTIVE: char = '▎'; // U+258E
/// Expanded-stack member's overpainted left edge.
pub const MARKER_EXPANDED_EDGE: char = '▌'; // U+258C
/// Tab bar separator, drawn after every tab.
pub const TAB_SEPARATOR: char = '│'; // U+2502
/// Rename-dialog input cursor (pre-existing glyph, now a named token).
pub const RENAME_CURSOR: char = '▏'; // U+258F
/// Picker selected-row marker.
pub const PICKER_SELECTED: char = '❯'; // U+276F
/// Save-indicator "saved" glyph.
pub const SAVED: char = '✓'; // U+2713
/// Tab-bar overflow clip marker.
pub const TAB_OVERFLOW: char = '…'; // U+2026

/// `AgentStatus` → (glyph, color, pulses) — C5's table verbatim. `pulses` is
/// true only for `Working`; every other state is steady (in particular,
/// `NeedsInput` never pulses — steady red means "waiting on you", pulsing red
/// means "alive").
pub fn status_style(status: AgentStatus) -> (char, Color, bool) {
    match status {
        AgentStatus::Working => (GLYPH_WORKING, ACCENT, true),
        AgentStatus::NeedsInput => (GLYPH_NEEDS_INPUT, ACCENT, false),
        AgentStatus::Waiting => (GLYPH_WAITING, FG, false),
        AgentStatus::Idle => (GLYPH_IDLE, DIM, false),
        AgentStatus::Exited => (GLYPH_EXITED, ACCENT_DIM, false),
    }
}

/// `TabSummary` → (glyph, color) — C5's tab-bar variant. Same colors as the
/// `AgentStatus` table; `Unknown` reuses the idle dot, `Quiet` is a blank
/// space (its color is unused by callers).
pub fn tab_summary_style(summary: TabSummary) -> (char, Color) {
    match summary {
        TabSummary::NeedsInput => (GLYPH_NEEDS_INPUT, ACCENT),
        TabSummary::Working => (GLYPH_WORKING, ACCENT),
        TabSummary::Waiting => (GLYPH_WAITING, FG),
        TabSummary::Unknown => (GLYPH_IDLE, DIM),
        TabSummary::Quiet => (' ', DIM),
    }
}

/// Pulse phase for the `Working` glyph (C5): period 1100ms, 50% duty —
/// `[0, 550)` → `ACCENT`, `[550, 1100)` → `ACCENT_DIM`, repeating. `elapsed`
/// is time since app start: one shared clock so every pulsing glyph flips in
/// unison (no per-glyph timers, no extra redraw scheduling — re-evaluated
/// each draw tick).
pub fn pulse_phase(elapsed: Duration) -> Color {
    if elapsed.as_millis() % 1100 < 550 {
        ACCENT
    } else {
        ACCENT_DIM
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_phase_boundaries() {
        assert_eq!(pulse_phase(Duration::from_millis(0)), ACCENT);
        assert_eq!(pulse_phase(Duration::from_millis(549)), ACCENT);
        assert_eq!(pulse_phase(Duration::from_millis(550)), ACCENT_DIM);
        assert_eq!(pulse_phase(Duration::from_millis(1099)), ACCENT_DIM);
        assert_eq!(pulse_phase(Duration::from_millis(1100)), ACCENT); // wraps
    }

    #[test]
    fn status_mapping_matches_c5_table() {
        assert_eq!(status_style(AgentStatus::Working), (GLYPH_WORKING, ACCENT, true));
        assert_eq!(status_style(AgentStatus::NeedsInput), (GLYPH_NEEDS_INPUT, ACCENT, false));
        assert_eq!(status_style(AgentStatus::Waiting), (GLYPH_WAITING, FG, false));
        assert_eq!(status_style(AgentStatus::Idle), (GLYPH_IDLE, DIM, false));
        assert_eq!(status_style(AgentStatus::Exited), (GLYPH_EXITED, ACCENT_DIM, false));
    }

    #[test]
    fn only_working_pulses() {
        for s in [AgentStatus::NeedsInput, AgentStatus::Waiting, AgentStatus::Idle, AgentStatus::Exited] {
            assert!(!status_style(s).2, "{s:?} must not pulse");
        }
    }

    #[test]
    fn tab_summary_mapping_matches_c5_table() {
        assert_eq!(tab_summary_style(TabSummary::NeedsInput), (GLYPH_NEEDS_INPUT, ACCENT));
        assert_eq!(tab_summary_style(TabSummary::Working), (GLYPH_WORKING, ACCENT));
        assert_eq!(tab_summary_style(TabSummary::Waiting), (GLYPH_WAITING, FG));
        assert_eq!(tab_summary_style(TabSummary::Unknown), (GLYPH_IDLE, DIM));
        assert_eq!(tab_summary_style(TabSummary::Quiet), (' ', DIM));
    }
}
