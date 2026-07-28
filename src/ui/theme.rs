//! Chrome design tokens (DESIGN-ui.md §2, contract C1).
//!
//! Thesis: chrome is *the user's own* ink on *the user's own* paper, plus
//! their red; program output keeps its own colors. Everything roost draws
//! itself (tab bar, borders, badges, stack chrome, hint bar, modals) is built
//! from exactly the accessors in this file. `ok`/`warn`/`info` are
//! deliberately **not** defined here: those are program-output-only hues
//! (§2); defining them would invite casual reuse and dilute the one-red rule.
//!
//! **Legibility principle (§2, 2026-07-27).** roost is chrome *around* other
//! programs' output, so it recedes into whatever theme the terminal already
//! wears. Theme variance is concentrated in the ANSI 8 slot, so variance is
//! allowed only where degradation is graceful:
//!
//! * **Text always derives from `Color::Reset`** — the terminal's own
//!   foreground on its own background, the one contrast pair the user has
//!   already validated (it is what every line of their shell output uses).
//!   Legible by construction, in light, dark and tinted themes alike.
//! * **The quieter text rung is `Reset` + `DIM`** — worst case a terminal
//!   ignores DIM and it renders as primary ink. It can never become invisible.
//! * **ANSI 8 (`Color::DarkGray`) is structure only** — borders, separators,
//!   rules. If a theme makes it faint you lose a hairline, not a word. It may
//!   never carry text (`render.rs`'s `structure_colour_never_carries_text`).
//! * **Attention surfaces use `Modifier::REVERSED`, never a colour fill** —
//!   reversing the terminal's own fg/bg is guaranteed contrasty everywhere.
//! * **The one red is the user's red**: ANSI 1, with ANSI 9 for the bright
//!   half of the pulse. The pulse must never lean on DIM: a terminal that
//!   ignores DIM would kill the animation outright, so the two phases are two
//!   guaranteed-visible reds.
//!
//! Consequence, intended: the old `MUTED`/`DIM` pair collapses into one quiet
//! rung. Text gets two levels (`ink`, `quiet`); structure gets its own slot.
//! There is deliberately no third text rung — inventing one out of ANSI 8
//! would put words in the one colour a theme is allowed to swallow.

use std::time::Duration;

use ratatui::style::{Color, Modifier, Style};

use crate::core::app::TabSummary;
use crate::core::status::AgentStatus;

// ---- Chrome style tokens (§2 token table) ----
//
// Accessors, not consts: a token is a whole `Style` now (colour *and*
// modifier), and `Style`'s builders aren't const fns. C1's "chrome styling
// comes only from theme.rs" gate is unchanged — this is still the one file.

/// Primary ink: the terminal's own foreground, on its own background.
pub fn ink() -> Style {
    Style::default().fg(Color::Reset)
}

/// Quiet ink — the same guaranteed-legible pair, one step back. The single
/// secondary text rung (it absorbed both of the old `MUTED` and `DIM`).
pub fn quiet() -> Style {
    Style::default().fg(Color::Reset).add_modifier(Modifier::DIM)
}

/// Structure only: unfocused pane borders, tab separators, rules. **Never
/// text** — this is the one theme-variant slot roost spends, and it is spent
/// where losing it costs a hairline rather than a word.
pub fn rule() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// The one red (ANSI 1): focus, needs-input, key hints, the working pulse's
/// quiet half.
pub fn accent() -> Style {
    Style::default().fg(Color::Red)
}

/// Quiet red: the exited glyph, the expanded-stack edge, the `raw`/`↑N` badge
/// tokens.
pub fn accent_quiet() -> Style {
    Style::default().fg(Color::Red).add_modifier(Modifier::DIM)
}

/// The working pulse's bright half (ANSI 9). Deliberately a *second red*
/// rather than `accent()` plus a modifier: a terminal that ignores DIM would
/// otherwise flatten the pulse into a steady dot.
pub fn pulse_bright() -> Style {
    Style::default().fg(Color::LightRed)
}

/// A neutral attention surface (the transient flash): reverse the terminal's
/// own fg/bg. No colour fill anywhere in chrome — a fill assumes a background
/// roost does not own (§2 background policy) — and reversing the user's own
/// pair is the one treatment guaranteed to be contrasty in any theme.
pub fn attention() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

/// A *problem* attention surface (dead-pane action bar, Alt-trap warning):
/// the same reversal, tinted by the one red. Reversing `accent()` paints the
/// row in the user's red with their background colour as the ink, so a
/// problem still reads as a problem — the distinction C10/C11 carried with
/// `RULE` vs `ACCENT_DIM` fills before chrome inherited the theme, rebuilt
/// from primitives that survive any palette. Red is a mid-tone in every
/// sane theme, so the reversed ink stays legible on light and dark alike.
pub fn attention_problem() -> Style {
    Style::default().fg(Color::Red).add_modifier(Modifier::REVERSED)
}

/// Active tab's label cell background — a deliberate sentinel (§2 background
/// policy) so the active tab fuses with whatever the terminal's own
/// background is. It is the *only* `bg` chrome sets, and it sets it to
/// "nothing".
pub const ACTIVE_TAB_BG: Color = Color::Reset;

/// The active tab's label: full-strength ink on the terminal's own paper.
/// Active vs inactive is carried by ink weight plus the `▎` marker (C2) —
/// there is no highlight fill to lose.
pub fn active_tab_label() -> Style {
    ink().bg(ACTIVE_TAB_BG)
}

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
/// Scrollback marker (U3): leads the badge's `↑N` token and the scroll
/// hint's `↑N/M` position whenever a pane's view is frozen in history.
pub const SCROLLED: char = '↑'; // U+2191

/// `AgentStatus` → (glyph, style, pulses) — C5's table verbatim. `pulses` is
/// true only for `Working`; every other state is steady (in particular,
/// `NeedsInput` never pulses — steady red means "waiting on you", pulsing red
/// means "alive").
pub fn status_style(status: AgentStatus) -> (char, Style, bool) {
    match status {
        AgentStatus::Working => (GLYPH_WORKING, accent(), true),
        AgentStatus::NeedsInput => (GLYPH_NEEDS_INPUT, accent(), false),
        AgentStatus::Waiting => (GLYPH_WAITING, ink(), false),
        AgentStatus::Idle => (GLYPH_IDLE, quiet(), false),
        AgentStatus::Exited => (GLYPH_EXITED, accent_quiet(), false),
    }
}

/// `TabSummary` → (glyph, style) — C5's tab-bar variant. Same styles as the
/// `AgentStatus` table; `Unknown` reuses the idle dot, `Exited` reuses the
/// quiet-red `✕` (U13), `Quiet` is a blank space (its style is unused by
/// callers).
pub fn tab_summary_style(summary: TabSummary) -> (char, Style) {
    match summary {
        TabSummary::NeedsInput => (GLYPH_NEEDS_INPUT, accent()),
        TabSummary::Working => (GLYPH_WORKING, accent()),
        TabSummary::Waiting => (GLYPH_WAITING, ink()),
        TabSummary::Unknown => (GLYPH_IDLE, quiet()),
        TabSummary::Exited => (GLYPH_EXITED, accent_quiet()),
        TabSummary::Quiet => (' ', quiet()),
    }
}

/// Pulse phase for the `Working` glyph (C5): period 1100ms, 50% duty —
/// `[0, 550)` → `pulse_bright()` (ANSI 9), `[550, 1100)` → `accent()`
/// (ANSI 1), repeating. `elapsed` is time since app start: one shared clock
/// so every pulsing glyph flips in unison (no per-glyph timers, no extra
/// redraw scheduling — re-evaluated each draw tick).
pub fn pulse_phase(elapsed: Duration) -> Style {
    if elapsed.as_millis() % 1100 < 550 {
        pulse_bright()
    } else {
        accent()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines carrying this marker are program-output plumbing, not chrome —
    /// the vt100 blit hands a *program's* colours through byte-faithfully and
    /// must keep the full palette (C18). Everything else in `src/` inherits.
    const PASSTHROUGH: &str = "chrome-gate-exempt";

    /// Every `.rs` file under `src/`, as (path, contents).
    fn src_files() -> Vec<(std::path::PathBuf, String)> {
        let mut stack = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
        let mut out = Vec::new();
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read a src/ directory") {
                let path = entry.expect("a src/ dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).expect("read a src/ file");
                    out.push((path, text));
                }
            }
        }
        assert!(out.len() > 5, "the source scan found almost nothing — check the walk");
        out
    }

    /// Gate 1 (§2 theme-inherited stance): chrome inherits or it doesn't
    /// ship. No truecolor and no palette-indexed colour may be constructed
    /// anywhere under `src/` — the one exemption is the vt100 blit's
    /// `conv_color`, which is *program* output and keeps the full palette.
    /// Source-scanning is the honest way to pin this: the rule is about what
    /// the code may say, not about one code path's return value.
    #[test]
    fn no_truecolor_or_indexed_colour_is_constructed_in_src() {
        // Split so this test's own source doesn't trip its own scan.
        let banned = [concat!("Color::", "Rgb"), concat!("Color::", "Indexed")];
        let mut offenders = Vec::new();
        for (path, text) in src_files() {
            for (i, line) in text.lines().enumerate() {
                if line.contains(PASSTHROUGH) {
                    continue;
                }
                if banned.iter().any(|b| line.contains(b)) {
                    offenders.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "chrome must inherit the terminal's palette (§2); found:\n{}",
            offenders.join("\n"),
        );
    }

    /// Gate 2 (§2 background policy): no chrome element pairs two
    /// theme-variant colours. Chrome sets no colour fill at all, so the pair
    /// can't arise: the only `bg` any token carries is the `Color::Reset`
    /// sentinel. Attention surfaces reverse instead (`attention`).
    #[test]
    fn no_chrome_token_pairs_two_theme_variant_colours() {
        let tokens: [(&str, Style); 8] = [
            ("ink", ink()),
            ("quiet", quiet()),
            ("rule", rule()),
            ("accent", accent()),
            ("accent_quiet", accent_quiet()),
            ("pulse_bright", pulse_bright()),
            ("attention", attention()),
            ("active_tab_label", active_tab_label()),
        ];
        for (name, style) in tokens {
            match style.bg {
                None | Some(Color::Reset) => {}
                Some(other) => panic!("{name} fills with {other:?}; chrome may not paint a bg"),
            }
        }
    }

    /// Gate 2, the call-site half: nothing under `src/ui/` may set a
    /// background other than the `Color::Reset` sentinel. (`cell_style`'s
    /// program-output bg is the marked exemption.)
    #[test]
    fn no_chrome_call_site_sets_a_background_fill() {
        // Split so this test's own source doesn't trip its own scan.
        let setter = concat!(".", "bg(");
        let mut offenders = Vec::new();
        for (path, text) in src_files() {
            if !path.to_string_lossy().contains("/ui/") {
                continue;
            }
            for (i, line) in text.lines().enumerate() {
                if line.contains(PASSTHROUGH) || line.trim_start().starts_with("//") {
                    continue;
                }
                if line.contains(setter)
                    && !line.contains("Reset")
                    && !line.contains("ACTIVE_TAB_BG")
                {
                    offenders.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "chrome carries no fills (§2 background policy); found:\n{}",
            offenders.join("\n"),
        );
    }

    /// The text rungs both derive from the terminal's own foreground: the one
    /// contrast pair the user has already validated. A future edit that gives
    /// either of them a literal hue is the whole regression this change
    /// exists to prevent.
    #[test]
    fn both_text_rungs_derive_from_reset() {
        assert_eq!(ink().fg, Some(Color::Reset));
        assert_eq!(quiet().fg, Some(Color::Reset));
        assert!(quiet().add_modifier.contains(Modifier::DIM));
        assert!(!ink().add_modifier.contains(Modifier::DIM));
    }

    /// The pulse leans on two real reds, never on DIM: a terminal that
    /// ignores the modifier must still see the dot flip.
    #[test]
    fn pulse_phases_are_two_visible_reds_not_a_modifier() {
        let a = pulse_phase(Duration::from_millis(0));
        let b = pulse_phase(Duration::from_millis(600));
        assert_ne!(a.fg, b.fg, "the phases must differ by colour, not by attribute");
        assert!(!a.add_modifier.contains(Modifier::DIM));
        assert!(!b.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn pulse_phase_boundaries() {
        assert_eq!(pulse_phase(Duration::from_millis(0)), pulse_bright());
        assert_eq!(pulse_phase(Duration::from_millis(549)), pulse_bright());
        assert_eq!(pulse_phase(Duration::from_millis(550)), accent());
        assert_eq!(pulse_phase(Duration::from_millis(1099)), accent());
        assert_eq!(pulse_phase(Duration::from_millis(1100)), pulse_bright()); // wraps
    }

    #[test]
    fn status_mapping_matches_c5_table() {
        assert_eq!(status_style(AgentStatus::Working), (GLYPH_WORKING, accent(), true));
        assert_eq!(status_style(AgentStatus::NeedsInput), (GLYPH_NEEDS_INPUT, accent(), false));
        assert_eq!(status_style(AgentStatus::Waiting), (GLYPH_WAITING, ink(), false));
        assert_eq!(status_style(AgentStatus::Idle), (GLYPH_IDLE, quiet(), false));
        assert_eq!(status_style(AgentStatus::Exited), (GLYPH_EXITED, accent_quiet(), false));
    }

    #[test]
    fn only_working_pulses() {
        for s in [AgentStatus::NeedsInput, AgentStatus::Waiting, AgentStatus::Idle, AgentStatus::Exited] {
            assert!(!status_style(s).2, "{s:?} must not pulse");
        }
    }

    #[test]
    fn tab_summary_mapping_matches_c5_table() {
        assert_eq!(tab_summary_style(TabSummary::NeedsInput), (GLYPH_NEEDS_INPUT, accent()));
        assert_eq!(tab_summary_style(TabSummary::Working), (GLYPH_WORKING, accent()));
        assert_eq!(tab_summary_style(TabSummary::Waiting), (GLYPH_WAITING, ink()));
        assert_eq!(tab_summary_style(TabSummary::Unknown), (GLYPH_IDLE, quiet()));
        assert_eq!(tab_summary_style(TabSummary::Exited), (GLYPH_EXITED, accent_quiet()));
        assert_eq!(tab_summary_style(TabSummary::Quiet), (' ', quiet()));
    }

    /// U13: the tab-bar Exited variant is the same glyph *and* the same quiet
    /// red as the per-pane `AgentStatus::Exited` — C5 is one table, so a
    /// tab of corpses can't read differently from a pane corpse. And it
    /// stays steady: only Working ever pulses.
    #[test]
    fn tab_exited_matches_the_pane_exited_row_and_never_pulses() {
        let (pane_glyph, pane_style, pulses) = status_style(AgentStatus::Exited);
        assert_eq!(tab_summary_style(TabSummary::Exited), (pane_glyph, pane_style));
        assert!(!pulses);
    }

    /// Every summary but Quiet draws a real glyph — Quiet's blank is the
    /// one deliberate "nothing to report" cell (U13 gave the tab that used
    /// to fall into it for being dead its own mark).
    #[test]
    fn only_quiet_renders_a_blank_tab_glyph() {
        for s in [
            TabSummary::NeedsInput,
            TabSummary::Working,
            TabSummary::Waiting,
            TabSummary::Unknown,
            TabSummary::Exited,
        ] {
            assert_ne!(tab_summary_style(s).0, ' ', "{s:?} must have a glyph");
        }
        assert_eq!(tab_summary_style(TabSummary::Quiet).0, ' ');
    }
}
