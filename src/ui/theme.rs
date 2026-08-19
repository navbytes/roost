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
//! * **The one red is the user's red**: ANSI 1, unmodulated. Animation lives
//!   in the *glyph* now, not the colour — the Working spinner (C5) swaps
//!   braille frames on a shared clock, so `accent()` never needs a second red
//!   to survive a terminal that ignores DIM the way the old colour-pulse did.
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

/// The one red (ANSI 1): focus, needs-input, key hints, the working spinner.
pub fn accent() -> Style {
    Style::default().fg(Color::Red)
}

/// Quiet red: the exited glyph, the expanded-stack edge, the `raw`/`↑N` badge
/// tokens.
pub fn accent_quiet() -> Style {
    Style::default().fg(Color::Red).add_modifier(Modifier::DIM)
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

// ---- Chrome glyphs (§2 glyph inventory; all single-width) ----

// Status glyphs (C5 table).
//
// Working is animated (C5): `SPINNER_FRAMES` verbatim from pi-tui's
// `loader.js` `DEFAULT_FRAMES` (pi 0.81.1) — so a pi pane's own badge agrees
// with what pi draws inside the pane. Terminals without braille glyphs
// render tofu; accepted deliberately — pi itself (the flagship adapter)
// already requires braille support, and per-terminal fallback detection
// isn't knowable from inside a PTY.
pub const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
/// The Working glyph's steady frame — shown wherever animation is
/// suppressed (a scrolled pane's frozen corner badge, N1) or not wanted (the
/// roster's status-filter tag). Always `SPINNER_FRAMES[0]`, the one frame
/// with no time dependency.
pub const GLYPH_WORKING: char = SPINNER_FRAMES[0];
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

/// `AgentStatus` → (glyph, style, spins) — C5's table verbatim. `spins` is
/// true only for `Working`; every other state is steady (in particular,
/// `NeedsInput` never spins — steady red means "waiting on you", an
/// animating glyph means "alive"). The `char` returned is the steady frame
/// (`GLYPH_WORKING`); a caller that finds `spins` true substitutes
/// `spinner_frame(elapsed)` for it instead — `style` never varies with time.
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

/// One spinner frame's on-screen time (C5). pi's own loader runs this
/// nominal (80ms/frame, 800ms full rotation); roost's render loop redraws
/// roughly every 33ms even with nothing to read (`main.rs`'s crossterm poll
/// timeout is the effective tick), which is finer than 80ms, so the frame
/// length is used as-is rather than quantized up to match a coarser tick.
const SPINNER_FRAME_MS: u128 = 80;

/// `elapsed` (the shared clock — `App::elapsed`, time since app start) → the
/// `SPINNER_FRAMES` index on screen right now, wrapping every 800ms (10
/// frames × `SPINNER_FRAME_MS`). One shared clock so every Working glyph
/// advances in unison (no per-glyph timers, no extra redraw scheduling —
/// re-evaluated once per frame, C5/D3) — the same rationale the old colour
/// pulse used.
pub fn spinner_frame(elapsed: Duration) -> char {
    let i = (elapsed.as_millis() / SPINNER_FRAME_MS) as usize % SPINNER_FRAMES.len();
    SPINNER_FRAMES[i]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines carrying this marker are program-output plumbing, not chrome —
    /// the vt100 blit hands a *program's* colours through byte-faithfully and
    /// must keep the full palette (C18). Everything else in `src/` inherits.
    const PASSTHROUGH: &str = "chrome-gate-exempt";

    use crate::ui::srcscan::src_files;

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
        let tokens: [(&str, Style); 6] = [
            ("ink", ink()),
            ("quiet", quiet()),
            ("rule", rule()),
            ("accent", accent()),
            ("accent_quiet", accent_quiet()),
            ("attention", attention()),
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
                if line.contains(setter) && !line.contains("Reset") {
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

    /// (a) Working never shows the retired dot — every frame is a member of
    /// pi's braille set.
    #[test]
    fn working_status_glyph_is_a_braille_spinner_frame_never_the_old_dot() {
        let (glyph, ..) = status_style(AgentStatus::Working);
        assert!(SPINNER_FRAMES.contains(&glyph), "{glyph:?} must be a spinner frame");
        assert_ne!(glyph, '●');
        let (tab_glyph, _) = tab_summary_style(TabSummary::Working);
        assert!(SPINNER_FRAMES.contains(&tab_glyph));
        assert_ne!(tab_glyph, '●');
    }

    /// (b) the frame index advances with elapsed and wraps at 10 (one full
    /// rotation, `SPINNER_FRAME_MS * SPINNER_FRAMES.len()` = 800ms).
    #[test]
    fn spinner_frame_advances_and_wraps_at_ten() {
        assert_eq!(spinner_frame(Duration::from_millis(0)), SPINNER_FRAMES[0]);
        assert_eq!(spinner_frame(Duration::from_millis(79)), SPINNER_FRAMES[0]);
        assert_eq!(spinner_frame(Duration::from_millis(80)), SPINNER_FRAMES[1]);
        assert_eq!(spinner_frame(Duration::from_millis(795)), SPINNER_FRAMES[9]);
        assert_eq!(spinner_frame(Duration::from_millis(800)), SPINNER_FRAMES[0]); // wraps
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
    fn only_working_spins() {
        for s in [AgentStatus::NeedsInput, AgentStatus::Waiting, AgentStatus::Idle, AgentStatus::Exited] {
            assert!(!status_style(s).2, "{s:?} must not animate");
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
    /// stays steady: only Working ever animates.
    #[test]
    fn tab_exited_matches_the_pane_exited_row_and_never_spins() {
        let (pane_glyph, pane_style, spins) = status_style(AgentStatus::Exited);
        assert_eq!(tab_summary_style(TabSummary::Exited), (pane_glyph, pane_style));
        assert!(!spins);
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
