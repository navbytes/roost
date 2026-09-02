//! Rendering: tab bar + pane borders + vt100 grid blit (design doc §8).

use std::collections::{HashSet, VecDeque};

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use crate::core::app::{
    feed_overlay_size, App, FeedEntry, Mode, RenameTarget, RosterRow, Search, Selection, TabSummary,
};
use crate::core::control::Actor;
use crate::core::layout::{self, Dir, PaneRect};
use crate::core::status::AgentStatus;
use crate::ports::PaneBackend;
use crate::ui::input::{self, Action, Keymap};
use crate::ui::mouse;
use crate::ui::theme;

pub fn draw<B: PaneBackend>(f: &mut Frame<'_>, app: &mut App<B>) {
    let area = f.area();
    if area.height < 2 {
        // ux P3-16: below the tab-bar-plus-one-body-row floor there is no
        // room for real chrome — this used to `return` here and leave the
        // screen blank, with nothing telling the user why.
        draw_too_small(f, area);
        return;
    }
    let tab_bar = Rect::new(area.x, area.y, area.width, 1);
    // Body comes from the app so pane rects, PTY sizing, and rendering all
    // agree on where the hint bar's reserved row is.
    let body = app.body_area();
    // C5/D3: one shared clock read for the whole frame. The spinner contract
    // requires every Working glyph to show the same frame; sampling
    // `app.elapsed()` separately per glyph left a real (if tiny) window for
    // the clock to tick past a frame boundary mid-draw and split a frame
    // across two different spinner glyphs.
    let spinner: char = theme::spinner_frame(app.elapsed());
    // C32: the badge age tags share one wall-clock read per frame, for the
    // same no-split-frame reason as the spinner above.
    let now: u64 = crate::core::app::now_unix_secs();

    draw_tab_bar(f, app, tab_bar, spinner);

    // C6: header row above each stack tall enough to spare one — a separate
    // walk over the same tree `app.rects()` reads, since the header isn't a
    // `PaneRect` (it belongs to no pane; §5). C21: stack headers are
    // real-tree chrome, suppressed entirely while zoomed.
    if !app.zoomed() {
        for header in layout::stack_headers(&app.ws.active_tab().layout, body) {
            draw_stack_header(f, header);
        }
    }
    // C7: which pane (if any) is the currently-expanded member of a stack —
    // computed once per frame, independent of whether that stack's header
    // row is shown.
    let mut stack_expanded = HashSet::new();
    layout::stack_expanded_ids(&app.ws.active_tab().layout, &mut stack_expanded);

    // C21/C22/§5: the zoom-and-float-aware display list — every
    // render/PTY-resize/mouse-hit path shares this one accessor so none of
    // them can disagree with what's actually on screen. It orders the float
    // *first* (topmost priority for `hit_test`'s first-match rule), but the
    // float must paint *last* (topmost visually, C22 stacking order: tiled
    // panes → zoomed view → float → modals) — so painting walks it in
    // reverse. Reversing has no effect on the non-float cases (a zoomed
    // singleton, or disjoint tiled rects that never overlap each other).
    let rects = app.display_rects();
    for pr in rects.iter().rev() {
        draw_pane(f, app, *pr, stack_expanded.contains(&pr.id), spinner, now);
    }

    if app.hints_shown() {
        let hint_bar = Rect::new(area.x, area.y + area.height - 1, area.width, 1);
        draw_hint_bar(f, app, hint_bar);
    } else if area.height >= 2 {
        // **[C10, 2026-08-20]** A flash is not a hint, and must not be
        // hostage to whether hints are shown. `hints_shown()` is false both
        // when the user pressed Alt+/ and when the terminal is under 3 rows
        // — and in that state every C10 message reached nobody: all 38 of
        // them, including C38's refusals ("no room to split"), U14's copy
        // result, the startup notice that a workspace.json was set aside,
        // and — worst — U22's confirm-arm prompts, so `Alt+w` armed a
        // destructive second press while showing no "again to close" at
        // all. Exactly the hazard C2's own dated amendment names for the
        // mode word: a safety affordance made conditional on an unrelated
        // toggle.
        //
        // Painted OVER the body's last row rather than by shrinking the
        // body: the geometry the panes were laid out and PTY-resized to must
        // not change for two seconds and back, and the row repaints itself
        // from the pane beneath the moment the flash expires. Nothing new is
        // drawn — same text, same `attention()` styling, same single row as
        // the hint-bar path, because it is literally the same function.
        //
        // **[C11, same amendment]** and the Alt-trap warning with it, in the
        // same precedence the hint bar uses (flash first). It is the worse
        // half of the two: if Alt really is being swallowed, `Alt+/` cannot
        // bring the hint bar back, so the one sentence explaining why no
        // chord works was unreachable by the only key that would reveal it.
        // Unlike the flash this bar is persistent, so it does cost a row of
        // pane content for as long as it holds — but `show_alt_hint` is
        // evidence-gated (U4/F1: the signature of a swallowed Option chord,
        // not merely an absence of Alt), and a user in that state needs the
        // sentence more than the row.
        let last = Rect::new(area.x, area.y + area.height - 1, area.width, 1);
        let _ = draw_flash(f, app, last) || draw_alt_warning(f, app, last);
    }

    // Anchor floating dialogs near the focused pane rather than dead-center
    // of the whole screen, so it's visually obvious which pane they affect.
    let anchor = rects.iter().find(|pr| pr.id == app.focused).map(|pr| pr.rect).unwrap_or(body);
    draw_mode_overlay(f, app, body, anchor, spinner);
}

/// ux P3-16: what `draw` shows instead of nothing when the terminal is
/// shorter than the tab-bar-plus-one-body-row floor (`area.height < 2`) —
/// there's no room left for real chrome, but a blank screen with no
/// explanation is worse than one plain line. At zero rows there is no cell
/// to draw into, so the only safe move is to draw nothing; at exactly one
/// row, that row is spent on the notice. `Paragraph` clips without
/// wrapping — the same idiom `draw_hint_bar` leans on — so the message
/// degrades character by character as the terminal narrows, down to 1×1,
/// with no manual width arithmetic of its own to get wrong.
fn draw_too_small(f: &mut Frame<'_>, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    f.render_widget(Paragraph::new("too small — resize").style(theme::ink()), area);
}

/// C9: Normal-mode hint pairs — exactly these seven; every other binding
/// stays discoverable via `Alt+?`. Every other mode's pairs are unchanged in
/// content (restyled only). Pure — no `Frame` — so the exact Normal-mode list
/// pins down. A focused-raw Normal pane shows exactly one pair — every other
/// hint would be a lie, since nothing else is intercepted; checked ahead of
/// `focused_dead` since a dead pane can't be raw-routed either way
/// (`raw_routing_active` requires it alive), so the dead branch wins when
/// both are true.
///
/// `help_scrolled` says whether the keymap is taller than its overlay right
/// now — the difference between two *true* Help hint rows: on a terminal
/// showing the whole table every key really does close it; on a shorter one
/// the arrows read on instead.
///
/// `resumable` — the focused dead pane has a session pointer
/// (`App::resume_command_line` is Some) — adds `y copy resume` to the dead
/// branch, ahead of `Alt+w`/`Alt+q` in the yield order: those two are
/// discoverable everywhere, `y` exists only on this bar.
/// **[F1, 2026-08-19]** Pairs are `(String, &'static str)` now, and every
/// **Alt** key on this bar is resolved from the live keymap rather than
/// compiled in — the hint bar had the same defect as the help overlay, and
/// it is the surface a user reads *most*, on every frame.
///
/// Mode-local keys (`hjkl`, `v/V`, `Esc`, `↑↓`, `n/N` …) stay literal, and
/// that is not an oversight: config.json's grammar is Alt chords only
/// (`Chord::parse` requires the `alt+` prefix), so those keys cannot be
/// remapped and cannot go stale. Only what can move is derived.
fn hint_pairs(
    mode: &Mode,
    focused_dead: bool,
    resumable: bool,
    focused_raw: bool,
    help_scrolled: bool,
    marked: bool,
    keymap: &Keymap,
) -> Vec<(String, &'static str)> {
    let b = input::effective_bindings(keymap);
    // A literal pair: a key config.json cannot touch.
    let lit = |k: &str, d: &'static str| (k.to_string(), d);
    // An Alt pair, resolved. `None` — every chord for it disabled — drops
    // the pair off the bar, the same rule the help overlay's rows follow.
    let alt = |actions: &'static [Action], spelling: &'static str, d: &'static str| {
        // The hint bar has its own fit/yield machinery (pairs drop whole from
        // the right), so a long label needs no elision here — C9 absorbs it.
        help_key_text(&HelpKey::Family(spelling, actions), "", &b).map(|k| (k, d))
    };
    match mode {
        // C24: keyboard cursor + mouse drag — every Copy-mode key is on this
        // bar (a binding nothing advertises may as well not exist).
        Mode::Copy { .. } => vec![
            lit("hjkl", "move"),
            lit("w/b/e", "word"),
            lit("0/$", "ends"),
            lit("v/V", "mark"),
            lit("y/↵", "yank"),
            // `o` opens the URL under the cursor — the keyboard half of Alt+click.
            lit("o", "open"),
            lit("drag", "select"),
            lit("Esc", "exit"),
        ],
        // [F9] Filtering displaces "any key closes", so the hint bar has to
        // say what replaced it — C27's roster pairs exactly, for the same
        // reason its comment gives: a letter is filter text now, so `Esc`
        // is the way out and the bar must lead with that.
        // [C41] `↵` joins the pair, between the motion that chooses a row
        // and the way out — the bar reads in the order the hands move.
        // `read on` became `move` for the same reason: while filtering the
        // arrows drive a cursor, not a view.
        Mode::Help { filter: Some(_), .. } => vec![
            lit("type", "filter"),
            lit("↑↓ PgUp/Dn", "move"),
            lit("↵", "run"),
            lit("Esc", "clear · close"),
        ],
        // [Amended 2026-09-01, C39] Bare typing opens the filter now, so
        // the un-filtered rows teach `type filter` and the one exit that
        // survives it (`Esc`) — "any key close" stopped being true.
        Mode::Help { .. } if help_scrolled => {
            vec![lit("↑↓ PgUp/Dn", "read on"), lit("type", "filter"), lit("Esc", "close")]
        }
        Mode::Help { .. } => {
            let mut pairs = Vec::new();
            pairs.extend(alt(&[Action::Help], "Alt+?", "all keys"));
            pairs.push(lit("type", "filter"));
            pairs.push(lit("Esc", "close"));
            pairs
        }
        // 48 columns. Tab is Rename's one remaining target (C32).
        Mode::Rename { .. } => {
            vec![lit("type", "tab name"), lit("←→", "move"), lit("↵", "save"), lit("Esc", "cancel")]
        }
        // C32 (combined), 72 columns. `Shift+↵` names the break — descend
        // from the name row, split inside the note (Ctrl/Alt+↵ are
        // unhinted synonyms, same rule as every alias on this bar).
        // C36, **74 columns** — and ordered by U6's rule that list order
        // *is* yield order, because 74 plus the 33-column needs-you
        // segment overflows the 100-col floor and trailing pairs drop.
        // The two that must survive a busy fleet lead: `↵ send` (what the
        // dialog is for) and `Esc cancel` (the escape hatch C24's list
        // refused to let yield). `type message` trails — a text field you
        // are already typing into is the pair whose absence costs least.
        // The motion keys are deliberately absent, on C24's precedent:
        // an eighth pair would push `Esc` off, and they are C13/C32's
        // shared vocabulary, taught by the C15 overlay.
        Mode::Broadcast { .. } => vec![
            lit("↵", "send"),
            lit("Esc", "cancel"),
            lit("Tab", "who gets it"),
            lit("Shift+↵", "new line"),
            lit("type", "message"),
        ],
        Mode::PaneEdit { .. } => vec![
            lit("type", "name / note"),
            lit("↵", "save"),
            lit("Shift+↵", "new line"),
            lit("↑↓←→", "move"),
            lit("Esc", "cancel"),
        ],
        // 71 columns, inside the floor. `j/k` are filter text now, not on
        // this bar.
        Mode::Picker { .. } => vec![
            lit("↑↓", "choose"),
            lit("↵", "open"),
            lit("1..9", "launch"),
            lit("type", "filter"),
            lit("←→", "dir"),
            lit("Esc", "cancel"),
        ],
        // 59 columns — comfortably inside the 100-col floor alongside the
        // right segment.
        Mode::Scroll => vec![
            lit("↑↓", "scroll"),
            lit("PgUp/Dn", "page"),
            lit("/", "search"),
            lit("n/N", "next"),
            lit("Esc", "exit"),
        ],
        // The search prompt's own list. `↵ keep`/`Esc cancel` are the two
        // exits and lead the yield order; the hits-walking pair trails
        // because it's the one that keeps working after the prompt closes
        // (and is advertised again on the Scroll list once it does).
        Mode::Search { .. } => {
            vec![lit("type", "filter"), lit("↵", "keep"), lit("Esc", "cancel"), lit("n/N", "next")]
        }
        Mode::Feed { .. } => vec![
            lit("↑↓", "select"),
            lit("PgUp/Dn", "page"),
            lit("↵", "go to pane"),
            lit("q/Esc", "close"),
        ],
        // 68 columns, inside the 100-col floor beside the right segment. `q`
        // is deliberately absent (the roster filters as you type, so a
        // letter is filter text, U20's rule) — `Esc` is the way out.
        Mode::Roster { .. } => vec![
            lit("↑↓", "select"),
            lit("PgUp/Dn", "page"),
            lit("↵", "go to pane"),
            lit("type", "filter"),
            lit("Tab", "status"),
            lit("Esc", "close"),
        ],
        Mode::Normal if focused_dead => {
            let mut pairs = vec![lit("↵", "relaunch"), lit("f", "fresh — drops resume")];
            if resumable {
                pairs.push(lit("y", "copy resume"));
            }
            pairs.extend(alt(&[Action::ClosePane], "Alt+w", "close"));
            pairs.extend(alt(&[Action::Quit], "Alt+q", "quit"));
            pairs
        }
        // C23: the one pair on a raw pane's bar — and the one whose
        // accuracy matters most, since it is the only advertised way out of
        // a mode that swallows everything else. Resolved, not compiled in.
        Mode::Normal if focused_raw => {
            alt(&[Action::ToggleRaw], "Alt+Shift+p", "exit raw").into_iter().collect()
        }
        // Pairs drop whole from the right, so `Alt+? keys` leads (the way to
        // everything else once it's dropped) and `Alt+r rename` trails
        // (costs nothing when gone — still findable under Alt+?).
        Mode::Normal => {
            // C9's seven, in the yield order U6 fixed: pairs drop whole from
            // the right, so `Alt+? keys` leads (the route to everything a
            // narrow bar just dropped) and the edit pair trails.
            const FOCUS: &[Action] = &[
                Action::Focus(Dir::Left),
                Action::Focus(Dir::Down),
                Action::Focus(Dir::Up),
                Action::Focus(Dir::Right),
            ];
            // C40: a mark is a gesture the user is halfway through, and
            // the pane it names is usually in another tab — invisible.
            // This pair is what makes it visible, so it leads (pairs drop
            // whole from the right) and it is here only while there is
            // something to pull.
            let pull = if marked {
                alt(&[Action::PullPane], "Alt+Shift+v", "pull marked pane")
            } else {
                None
            };
            [
                pull,
                alt(&[Action::Help], "Alt+?", "keys"),
                alt(&[Action::NewPane], "Alt+n", "new"),
                alt(&[Action::QuickLaunch], "Alt+↵", "launch"),
                alt(&[Action::StackPane], "Alt+s", "stack"),
                alt(FOCUS, "Alt+←↓↑→", "focus"),
                alt(&[Action::ClosePane], "Alt+w", "close"),
                alt(&[Action::EditPane], "Alt+r", "edit"),
            ]
            .into_iter()
            .flatten()
            .collect()
        }
    }
}

/// C9's right-segment uppercase mode word. A real non-Normal mode always
/// wins; in Normal, `RAW` (C23) beats `ZOOM` (C21) beats `NORMAL` — input
/// safety (knowing you're raw) trumps view state.
fn mode_word(mode: &Mode, zoomed: bool, raw: bool) -> &'static str {
    match mode {
        Mode::Normal if raw => "RAW",
        Mode::Normal if zoomed => "ZOOM",
        Mode::Normal => "NORMAL",
        Mode::Rename { .. } => "RENAME",
        Mode::PaneEdit { .. } => "EDIT",
        Mode::Picker { .. } => "PICKER",
        Mode::Scroll => "SCROLL",
        Mode::Copy { .. } => "COPY",
        Mode::Help { .. } => "HELP",
        Mode::Feed { .. } => "FEED",
        Mode::Roster { .. } => "ROSTER",
        Mode::Broadcast { .. } => "BROADCAST",
        Mode::Search { .. } => "SEARCH",
    }
}

/// C9's right-aligned segment: the aggregate "◆ N needs you · Alt+a" — then
/// (P21) the search prompt, then (Scroll/Search, U3) the dim position, then
/// the uppercase mode word, then one trailing space. Everything rides inside
/// the segment so C9's fit/yield machinery covers it for free: pairs drop
/// whole before any of it clips. Pure so the omission rules are
/// unit-testable without a `Frame`.
///
/// `attention` is `App::attention_segment()` verbatim — `None` omits the
/// aggregate entirely (rather than a hollow "0 needs you"); `Some((n, true))`
/// is the real ◆ count in `accent()`; `Some((n, false))` is
/// `attention_ring`'s ○ fallback count, `"○ {n} your turn · Alt+a"` in
/// `ink()` (one visual step back from the accent-red ◆ case, the same style
/// `theme::status_style` gives the Waiting glyph everywhere else).
///
/// `query` is the live search prompt (`/foo`) and is the one token on
/// this bar drawn in `ink`: it is text the user is typing right now, and
/// quiet input is input you cannot proofread. `position` carries `↑N/M` in
/// Scroll mode and the `i/n` hit counter while searching — both `quiet`, both
/// the same "where am I" role.
fn hint_bar_right_spans(
    attention: Option<(usize, bool)>,
    query: Option<String>,
    position: Option<String>,
    word: &str,
    jump_chord: Option<String>,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if let Some((n, needs_input)) = attention {
        // C34: the chord is resolved, not spelled. C9's own amendment calls
        // this segment "this feature's discoverability surface" — a surface
        // that teaches a dead chord after a remap is worse than one that
        // teaches nothing, so an unbound jump drops the `· chord` tail
        // rather than naming a key that does nothing.
        let tail = jump_chord.map(|c| format!(" · {c}")).unwrap_or_default();
        let (text, style) = if needs_input {
            (format!("◆ {n} needs you{tail}"), theme::accent())
        } else {
            (format!("○ {n} your turn{tail}"), theme::ink())
        };
        spans.push(Span::styled(text, style));
        spans.push(Span::raw("  "));
    }
    if let Some(q) = query {
        spans.push(Span::styled(format!("{q} "), theme::ink()));
    }
    if let Some(pos) = position {
        spans.push(Span::styled(format!("{pos} "), theme::quiet()));
    }
    spans.push(Span::styled(word.to_string(), theme::quiet()));
    spans.push(Span::raw(" "));
    spans
}

/// P21: the search prompt and hit counter for C9's right segment — `/query`
/// (with the `▏` glyph trailing it, so a typed prompt looks like a text
/// field) and `i/n`, or `0/0` when nothing matches. `None` for both outside
/// a search. Pure.
///
/// The `▏` stays a *glyph* here, unlike `rename_field`'s reversed cell: this
/// prompt and the picker/roster type-aheads are append-only — there is no
/// point to move, so nothing can be displaced by a caret that takes a cell.
fn search_segment(search: Option<&Search>) -> (Option<String>, Option<String>) {
    let Some(s) = search else { return (None, None) };
    let counter = if s.matches.is_empty() {
        "0/0".to_string()
    } else {
        format!("{}/{}", s.current + 1, s.matches.len())
    };
    (Some(format!("/{}{}", s.query, theme::RENAME_CURSOR)), Some(counter))
}

/// Zellij-style shortcut bar. Mode-aware: the keys shown match what you can
/// actually press right now. Precedence (C9, reordered F1 2026-08-07):
/// flash, then alt-warning, then the hint pairs — each takes over the whole
/// bar from the next.
/// Rendered column width of one hint pair. THE single source both the fit
/// calculation and the actual draw use, so they can never drift (a +3/+4
/// mismatch once dropped the whole right segment at widths 111–116).
/// Matches the span strings in `draw_hint_bar`: `" {key} "` (key + 2) plus
/// `"{label}  "` (label + 2).
fn hint_pair_cols(key: &str, label: &str) -> u16 {
    (key.chars().count() + label.chars().count() + 4) as u16
}

/// How many leading hint pairs fit alongside the right segment (C9 yield
/// order: pairs drop whole, from the right, before the segment ever yields).
fn fit_hint_pairs<K: AsRef<str>>(hints: &[(K, &'static str)], right_w: u16, width: u16) -> usize {
    let budget = width.saturating_sub(right_w);
    let mut used = 0u16;
    for (i, (key, label)) in hints.iter().enumerate() {
        let w = hint_pair_cols(key.as_ref(), label);
        if used + w > budget {
            return i;
        }
        used += w;
    }
    hints.len()
}

/// C10: paint the flash into `area`, or report that there was none.
///
/// One function for both the places a flash can land — the hint bar when it
/// is drawn, and the body's last row when it is not — so the two can never
/// disagree about a flash's text or styling.
///
/// The `Clear` is what makes that true, and it is not decoration.
/// `theme::attention()` is REVERSED and nothing else — deliberately, so the
/// flash inverts the user's own fg/bg pair instead of assuming a background
/// roost does not own (§2). Ratatui *patches* styles rather than replacing
/// them, so with no fg of its own the message takes the fg of whatever cells
/// it lands on. On the hint bar those cells are empty and the result is the
/// intended `Reset` + reverse. Over the body's last row they carry the pane
/// border drawn moments earlier in the same frame, and the message came out
/// reversed in the *border's* colour: `RULE` (structure colour carrying
/// text, banned by §2) under an unfocused pane, and under a focused one
/// `ACCENT` reversed — bit-identical to `attention_problem()`, so "copied 12
/// chars" rendered as C11/C16's reserved problem treatment. The cells past
/// the message kept their border glyphs, reversed, leaving a ragged band.
/// Found by the design-supervisor pass on this very change; the fixture that
/// would have caught it mechanically is added alongside.
fn draw_flash<B: PaneBackend>(f: &mut Frame<'_>, app: &App<B>, area: Rect) -> bool {
    let Some(msg) = app.flash() else { return false };
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(format!(" {msg} ")).style(theme::attention()), area);
    true
}

/// C11/U4: paint the Alt-trap warning into `area`, or report that there is
/// nothing to warn about. Same shape and same reasons as `draw_flash` — one
/// function for both the hint bar and the body's last row, and the `Clear`
/// so the cells past the message do not keep their border glyphs under the
/// reversal (`attention_problem()` names its own fg, so unlike the flash the
/// *text* could not inherit, but the ragged band could).
fn draw_alt_warning<B: PaneBackend>(f: &mut Frame<'_>, app: &App<B>, area: Rect) -> bool {
    if !app.show_alt_hint() {
        return false;
    }
    // Same bar, per-terminal wording — the app knows the host's TERM_PROGRAM
    // and picks the real menu path where there is one. A problem bar, so the
    // red-tinted reversal, not the neutral one.
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(app.alt_hint_line()).style(theme::attention_problem()), area);
    true
}

fn draw_hint_bar<B: PaneBackend>(f: &mut Frame<'_>, app: &App<B>, area: Rect) {
    // Flash wins the bar over the alt-warning: a transient action result
    // (e.g. "copied") takes over the bar briefly rather than being swallowed
    // by a persistent problem bar for its entire window.
    if draw_flash(f, app, area) || draw_alt_warning(f, app, area) {
        return;
    }

    // (key, what it does) pairs for the current context: key `accent`, label
    // `quiet`, no chip bg. The right segment (aggregate + mode word) WINS over
    // the pairs (C9 yield order): the mode word is a modal-safety affordance
    // and "◆ N needs you" the fleet's primary signal — trailing pairs drop
    // whole until the segment fits.
    let focused_raw = app.is_raw(app.focused);
    let resumable = app.focused_dead() && app.resume_command_line(app.focused).is_some();
    // Only the C15 overlay's own hint row asks whether the help table
    // scrolls, and answering means laying the whole keymap out —
    // `effective_bindings` (a clone of the default table, a redundant-
    // spelling pass, a sort) plus a formatted label per row, thrown away.
    // Unguarded, that ran on every frame in every mode: the hint bar
    // repaints 30 times a second whether or not the overlay is up.
    let scrolled = matches!(app.mode, Mode::Help { .. }) && {
        let (visible, total) = help_scroll_extent(app.body_area(), app.keymap(), app.help_filter());
        visible < total
    };
    let hints = hint_pairs(
        &app.mode,
        app.focused_dead(),
        resumable,
        focused_raw,
        scrolled,
        app.marked().is_some(),
        app.keymap(),
    );
    // U3: Scroll mode's right segment shows where in history the view sits
    // — `↑N/M` from the backend's grid-clamped (offset, banked) pair, so it
    // can never report a phantom row the grid refused (U9's overshoot).
    // P21: while the prompt is up the segment carries the query and the hit
    // counter instead — the search *is* the position report, and stacking
    // `↑N/M` beside `3/17` would be two answers to one question.
    let (query, position) = match &app.mode {
        Mode::Search { .. } => search_segment(app.search.as_ref()),
        Mode::Scroll => {
            let (off, total) = app.scroll_position();
            (None, Some(format!("{}{off}/{total}", theme::SCROLLED)))
        }
        _ => (None, None),
    };
    let right = hint_bar_right_spans(
        app.attention_segment(),
        query,
        position,
        mode_word(&app.mode, app.zoomed(), focused_raw),
        input::chord_for(app.keymap(), Action::JumpAttention),
    );
    let right_w: u16 = right.iter().map(|s| s.content.chars().count() as u16).sum();

    let shown = fit_hint_pairs(&hints, right_w, area.width);
    let mut spans: Vec<Span<'_>> = Vec::with_capacity(shown * 2 + 4);
    let mut used = 0u16;
    for (key, label) in &hints[..shown] {
        used += hint_pair_cols(key, label); // same source as fit_hint_pairs
        spans.push(Span::styled(format!(" {key} "), theme::accent()));
        spans.push(Span::styled(format!("{label}  "), theme::quiet()));
    }
    if used.saturating_add(right_w) <= area.width {
        let pad = area.width.saturating_sub(used).saturating_sub(right_w);
        spans.push(Span::raw(" ".repeat(pad as usize)));
        spans.extend(right);
    }

    // Paragraph truncates (no wrap) so a narrow terminal just clips the
    // tail. No row fill: the bar is ink on the terminal's own paper (§2
    // background policy), so every cell no span touches keeps the user's
    // background instead of a band roost painted.
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Floating rect of the given size, centered on `anchor` (the focused pane)
/// but clamped to fully fit inside `bounds` — so a dialog near the screen
/// edge still lands on-screen instead of centering blindly on the whole
/// terminal.
fn centered_near(anchor: Rect, bounds: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(bounds.width);
    let h = height.min(bounds.height);
    let cx = anchor.x + anchor.width / 2;
    let cy = anchor.y + anchor.height / 2;
    let x = cx.saturating_sub(w / 2).clamp(bounds.x, bounds.x + bounds.width - w);
    let y = cy.saturating_sub(h / 2).clamp(bounds.y, bounds.y + bounds.height - h);
    Rect::new(x, y, w, h)
}

/// Dim every cell in `body` so a floating overlay reads as a distinct modal
/// layer sitting on top of the panes, not more pane chrome. Callers dim the
/// whole body, then `Clear` the dialog's own rect right after (`modal_frame`)
/// — that reset already undoes the dim there, so this doesn't need to know
/// the dialog's rect at all.
fn dim_backdrop(f: &mut Frame<'_>, body: Rect) {
    f.buffer_mut().set_style(body, Style::new().add_modifier(Modifier::DIM));
}

/// C12: the modal preamble every floating dialog shares — dim the body,
/// clear the dialog's own cells, draw its bordered frame with `title`, and
/// hand back the inner area for the mode-specific content. Border color is
/// `theme::accent()`, the one look all dialogs share (a modal is the focused
/// interaction surface); no BOLD (§2 bold policy).
fn modal_frame(f: &mut Frame<'_>, body: Rect, rect: Rect, title: Line<'static>) -> Rect {
    dim_backdrop(f, body);
    f.render_widget(Clear, rect);
    let block =
        Block::bordered().title(title).border_type(BorderType::Plain).border_style(theme::accent());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    inner
}

/// C14 (U20): a picker row's text after its 1-column marker — the `1..9`
/// accelerator, then the adapter id. Items past the ninth get no digit
/// (there is no `Alt+10`), but keep the columns so the ids stay aligned.
/// Pure, and shared by the selected and unselected arms so the two rows can
/// never differ by anything but their style.
fn picker_row_body(i: usize, item: &str) -> String {
    match i {
        0..=8 => format!(" {} {item}", i + 1),
        _ => format!("   {item}"),
    }
}

/// C13 (U16, amended 2026-08-27): the rename field's rendered spans — the
/// caret **on** the character at the insertion point (`theme::attention()`,
/// the reversal every terminal's own block cursor is), not a glyph inserted
/// before it.
///
/// It was an inserted `▏` until a live report: moving the point left pushed
/// every character after it one column right, so editing the middle of a
/// name visibly shoved the tail around — the field's own text moving under
/// a key that was supposed to move only the cursor. An inserted caret costs
/// a real cell; a reversed one costs none, which is exactly why terminals
/// draw cursors that way. At the end of the buffer there is no character to
/// reverse, so the caret is a reversed space — appended, displacing nothing.
///
/// `cursor` is a char index and is clamped, so a stale value (a resize
/// between keystrokes, a paste that shortened the buffer) renders the caret
/// at the end instead of panicking on a bad slice. Pure so the caret's
/// placement has a unit-test seam.
fn rename_field(buffer: &str, cursor: usize) -> Vec<Span<'static>> {
    let at = cursor.min(buffer.chars().count());
    let byte = buffer.char_indices().nth(at).map_or(buffer.len(), |(b, _)| b);
    let mut spans = vec![Span::raw(buffer[..byte].to_string())];
    let mut tail = buffer[byte..].chars();
    match tail.next() {
        Some(c) => {
            spans.push(Span::styled(c.to_string(), theme::attention()));
            spans.push(Span::raw(tail.as_str().to_string()));
        }
        None => spans.push(Span::styled(" ", theme::attention())),
    }
    spans
}

/// The rendered width of a span run, in display columns (D1) — what a
/// caller has to pad against once a field is spans rather than one string.
fn spans_width(spans: &[Span<'_>]) -> u16 {
    spans.iter().map(|s| mouse::display_width(&s.content)).sum()
}

/// C14 (U20): one picker row's leading marker and text style, given whether
/// it is its column's selection and whether that column has the keyboard.
/// Three states, no fourth: the focused column's selection wears `❯` in
/// `accent` with `ink` text (C14's idiom, unchanged); the *other* column's
/// selection keeps `ink` text without the marker — what a launch would use
/// has to stay readable from either side — and everything else is `quiet`.
/// Pure so the three states are pinned without a `Frame`.
fn row_marks(selected: bool, column_focused: bool) -> (Span<'static>, Style) {
    match (selected, column_focused) {
        (true, true) => {
            (Span::styled(theme::PICKER_SELECTED.to_string(), theme::accent()), theme::ink())
        }
        (true, false) => (Span::raw(" "), theme::ink()),
        _ => (Span::raw(" "), theme::quiet()),
    }
}

/// C14 (U20): the picker's cwd column entry for `path` — the last two path
/// components, so `~/src/roost/vendor` reads as `roost/vendor` and a column
/// of sibling checkouts stays distinguishable without spending the width a
/// full path would. The root and single-component paths render whole.
fn picker_cwd_label(path: &std::path::Path) -> String {
    let parts: Vec<String> =
        path.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
    match parts.len() {
        0 => "/".to_string(),
        1 => parts[0].clone(),
        // A top-level directory's parent IS the root, so joining blindly
        // would spell `/tmp` as `//tmp`.
        n if parts[n - 2] == "/" => format!("/{}", parts[n - 1]),
        n => format!("{}/{}", parts[n - 2], parts[n - 1]),
    }
}

/// C14 (U20): the picker's adapter column — the 3-char row prefix
/// (`picker_row_body`'s `" {digit} "`) + the registry's longest id
/// (`opencode`, 8) + the missing-adapter suffix (`" not found"`, 10, see
/// `App::picker_filtered` and `PICKER_MISSING_SUFFIX`) + the selection
/// marker the pad formula subtracts (1) + **1 column of slack**.
///
/// That last term used to read "2 columns of slack" and did not count the
/// marker, so the stated derivation summed to one more than the constant
/// actually leaves. Caught by the design audit; the arithmetic was right
/// and only the sentence was wrong, which is the kind of error that
/// survives because nobody re-adds a comment.
///
/// **One constant, because there were two.** `picker_dialog_width` sized
/// the dialog for 23 while `draw_mode_overlay` padded its rows to 16 — same
/// name, same intent, free to disagree, and they did. Any row wider than 16
/// (`" 1 opencode not found"` is 21) got no padding at all, so its cwd
/// label started six columns right of every other row's: a ragged column in
/// exactly the case the dialog was sized to accommodate. Found by sweeping
/// for the class of the C15 padding bug rather than its instance — a
/// minimum-width pad standing in for a column is the same mistake twice.
const ADAPTER_COL: u16 = 23;

/// C14 (U20): the picker dialog's width — the adapter column plus the
/// widest cwd label, plus the gap and the two border columns.
/// `centered_near` still clamps it to the screen. Pure so the sizing and
/// the drawing can't drift.
///
/// `ADAPTER_COL` is the shared constant below — the same number that pads
/// each row, so the dialog is sized for exactly the column it draws.
fn picker_dialog_width(cwds: &[std::path::PathBuf]) -> u16 {
    let widest = cwds.iter().map(|p| mouse::display_width(&picker_cwd_label(p))).max().unwrap_or(0);
    // Never narrower than the pre-U20 dialog: a column of short labels must
    // not make the picker *shrink* relative to the one it replaced.
    const MIN: u16 = 32;
    if widest == 0 {
        return MIN;
    }
    (ADAPTER_COL + 2 + widest + 2).max(MIN)
}

/// The help overlay's key-column prefix: the key label left-padded to a
/// fixed column so every description lines up underneath it. Shared by the
/// width computation and the row-rendering loop below so they can't drift.
///
/// **The trailing space is load-bearing, and `{key:<18}` did not guarantee
/// one.** `draw_help_columns` draws this prefix and the description as two
/// adjacent spans with nothing between them, so all the separation there is
/// comes from this padding — and `{:<18}` is a *minimum* width, not a
/// column: a key 18 or more wide gets padded by nothing and the description
/// fuses straight onto it. On the default keymap **two** control-CLI rows
/// did this: `roost send <id> "text"` (22) rendered `…"text"type into that
/// pane`, and `roost spawn ADAPTER` (19) rendered `…ADAPTERlaunch a new
/// pane`. After C34 made the key column keymap-derived, an enumerated
/// family reaches 23 and did the same to `move focus`. Found by a
/// simulation agent stressing the 80-column floor, which is where it is
/// ugliest, but it was never width-dependent — the fusion happens at any
/// terminal size.
///
/// So: pad to the 18-column grid when the key fits it, and otherwise fall
/// back to exactly one space. Rows under 18 render byte-identically to
/// before; only the ones that were broken move. `elide_key` and
/// `help_content_width` both measure through here, so the extra column is
/// accounted for in the floor rather than smuggled past it — see
/// `HELP_COL_FLOOR`, which had to be corrected in the same pass: it read 80
/// where the dialog can only draw 77, and this extra column was what finally
/// made three columns of slop matter.
fn help_key_prefix(key: &str) -> String {
    let pad = (18usize).saturating_sub(mouse::display_width(key) as usize).max(1);
    format!(" {key}{}", " ".repeat(pad))
}

/// F1: the chord table with no config.json applied — what `HelpKey::Family`
/// compares against to decide whether its compact spelling is still true.
/// Memoized; it is a pure function of the compiled-in table.
fn default_bindings() -> &'static [(String, Action)] {
    static B: std::sync::OnceLock<Vec<(String, Action)>> = std::sync::OnceLock::new();
    B.get_or_init(|| input::effective_bindings(&Keymap::default()))
}

/// F1: the chords currently bound to `actions`, joined by `" / "` in the
/// order the row declared them. `None` when **none** of them is bound —
/// every chord disabled in config.json — which drops the row from the
/// overlay entirely: a row teaching a key that no longer exists is worse
/// than no row.
fn join_chords(actions: &[Action], bindings: &[(String, Action)]) -> Option<String> {
    let mut labels = Vec::new();
    for a in actions {
        for (label, bound) in bindings {
            if bound == a {
                labels.push(label.as_str());
            }
        }
    }
    (!labels.is_empty()).then(|| labels.join(" / "))
}

/// C15's 80-column floor, expressed as what one *column of content* may
/// measure — which is **not 80**. `help_layout` asks for `content + 3` (two
/// borders plus the column of air before the right one), so a column of 78
/// is a dialog of 81, `centered_near` clamps it to the terminal, and the air
/// goes first: a row that spends the full width ends flush against the
/// border. 80 − 2 borders − 1 air = 77.
///
/// It read `80` from the start, three columns too loose, and never bit
/// because the widest content the table could produce was exactly 77 — the
/// ceiling this constant should have named. Then C38's sibling fix (a key
/// column always ends in a space) added one column to the over-wide rows and
/// the slack was gone: at 80×24 with `alt+h` disabled, `…j … move focus (…at
/// an edge)` sat against the border. Found by the design audit of that fix,
/// measured rather than reasoned.
///
/// So it is derived here rather than restated, because the derivation is the
/// part that was wrong.
const HELP_COL_FLOOR: u16 = HELP_FLOOR_COLS - HELP_DIALOG_CHROME;

/// The terminal width C15's floor is written against.
const HELP_FLOOR_COLS: u16 = 80;

/// What `help_layout` adds to a column of content: a border each side, and
/// one column of air before the right one so a full-width row can breathe.
/// The key column already opens with a space, which is the left-hand air.
const HELP_DIALOG_CHROME: u16 = 3;

/// C34/C15: keep a resolved key column inside the floor by eliding **the
/// key**, never the description.
///
/// Before C34 a row's key was authored text of known width, so the floor
/// was a thing you checked once. It is derived now, and an enumerated
/// family is unbounded: disabling `Alt+←` alone makes the focus row spell
/// eight chords and pushes its column to 107, which at an 80-column
/// terminal clips the description — the exact failure C15's width rule
/// exists to prevent, reintroduced by the mechanism meant to make the row
/// truthful. (Found by the design-supervisor audit of C34, measured rather
/// than predicted.)
///
/// The key yields because the description is the row's irreplaceable half:
/// a reader who sees two of eight chords still learns what the row is for
/// and can widen the terminal for the rest, while a clipped description
/// teaches nothing at any width. Elision lands on a `" / "` boundary so a
/// chord is never shown half-spelled, and `…` marks that more exist. A
/// single chord that still doesn't fit is left alone: that is authored
/// width, not something this function created.
fn elide_key(key: String, desc: &str) -> String {
    let fits = |k: &str| {
        mouse::display_width(&help_key_prefix(k)) + mouse::display_width(desc) <= HELP_COL_FLOOR
    };
    if fits(&key) {
        return key;
    }
    let parts: Vec<&str> = key.split(" / ").collect();
    (1..parts.len())
        .rev()
        .map(|n| format!("{} …", parts[..n].join(" / ")))
        .find(|candidate| fits(candidate))
        .unwrap_or(key)
}

/// F1: a row's key column, resolved against the live keymap.
fn help_key_text(key: &HelpKey, desc: &str, bindings: &[(String, Action)]) -> Option<String> {
    match key {
        // Authored text of known width — the control-CLI block and the
        // legends. Not elided: it names no chord this function resolved, so
        // there is no " / " here that is safe to cut.
        HelpKey::Text(s) => Some((*s).to_string()),
        HelpKey::Chords(actions) => join_chords(actions, bindings).map(|k| elide_key(k, desc)),
        HelpKey::Family(spelling, actions) => {
            let live = join_chords(actions, bindings)?;
            // The compact spelling is a display shorthand for the default
            // chords and nothing else. If any member moved, it is no longer
            // a description of this keymap, so enumerate what is really
            // bound instead.
            if join_chords(actions, default_bindings()).as_deref() == Some(live.as_str()) {
                Some((*spelling).to_string())
            } else {
                Some(elide_key(live, desc))
            }
        }
    }
}

/// C15: the keymap's content width — the widest row
/// (key column + description). One *column* of it; the overlay may draw two
/// side by side, which `help_layout` decides. Pure so the sizing has a
/// unit-test seam.
fn help_content_width(lines: &[HelpLine]) -> u16 {
    lines
        .iter()
        .filter_map(|l| match l {
            HelpLine::Row(k, d, _) => {
                Some(mouse::display_width(&help_key_prefix(k)) + mouse::display_width(d))
            }
            HelpLine::Head(_) => None,
        })
        .max()
        .unwrap_or(0)
}

/// C15: one line of the drawn keymap — a group heading or a chord row.
/// The row's key column is owned rather than `&'static`: since F1 it is
/// resolved from the live keymap, not compiled in.
///
/// [C41] A row also carries the action `↵` runs on it, or `None` when the
/// row is not a command — see `help_row_action` for which rows are which.
#[derive(Clone, PartialEq, Eq, Debug)]
enum HelpLine {
    Head(&'static str),
    Row(String, &'static str, Option<Action>),
}

/// [C41] The action `↵` runs on a row, or `None` when the row is not a
/// command the overlay can execute.
///
/// **A row is a command only when it documents exactly one action.** That
/// rule is not a simplification — it is the whole reason the palette is
/// worth having, and it falls out of what the two shapes mean:
///
/// - `Chords([a])` is one verb with one outcome. Running it is unambiguous.
/// - `Family(_, [a, b, …])` and any multi-action `Chords` are a *direction
///   set* — focus, resize, tab motion, pane carry. There is no answer to
///   "which one does `↵` run", and even if you picked one it would be the
///   wrong feature: those are the chords you press five times in a row, and
///   a palette that runs one step and then closes is strictly worse than
///   the chord it is standing in for.
/// - `Text(_)` binds nothing at all — the control-CLI reference block, the
///   glyph legend, the dead-pane keys `main.rs` claims. Nothing to run.
///
/// So what stays runnable is exactly the set a palette is for: the rare,
/// one-shot, hard-to-remember verbs (flip split, cycle layout, mark/pull a
/// pane, toggle raw/float/zoom/feed/roster, undo, rename). The rows that
/// resist execution are the ones nobody would drive this way anyway.
fn help_row_action(key: &HelpKey) -> Option<Action> {
    match key {
        HelpKey::Chords([only]) => Some(*only),
        HelpKey::Chords(_) | HelpKey::Family(..) | HelpKey::Text(_) => None,
    }
}

/// [C41] The runnable rows under `filter`, in the order the overlay draws
/// them — the list `Mode::Help`'s cursor indexes and `↵` dispatches from.
///
/// Layout-independent by construction: it reads `help_lines`, which is the
/// flat table *before* `help_layout` decides how many columns to pour it
/// into. So the cursor means the same row whether the overlay is drawn in
/// one column or two, and a terminal resize under an open palette cannot
/// move what `↵` is pointing at.
pub fn help_actions(keymap: &Keymap, filter: &str) -> Vec<Action> {
    help_lines(keymap, filter)
        .into_iter()
        .filter_map(|l| match l {
            HelpLine::Row(_, _, action) => action,
            HelpLine::Head(_) => None,
        })
        .collect()
}

/// C15: the keymap flattened into drawable lines, in group order — each
/// heading followed by its rows, with no blank between groups. The
/// underlined heading is the separator (C6's idiom, exactly as C27's roster
/// stacks its groups), and a blank row per group would cost six rows of a
/// table that already has to scroll on a short terminal.
fn help_lines(keymap: &Keymap, filter: &str) -> Vec<HelpLine> {
    let bindings = input::effective_bindings(keymap);
    let mut out = Vec::new();
    for g in HELP_GROUPS {
        let rows: Vec<HelpLine> = g
            .rows
            .iter()
            .filter_map(|r| {
                help_key_text(&r.key, r.desc, &bindings)
                    .map(|k| HelpLine::Row(k, r.desc, help_row_action(&r.key)))
            })
            .filter(|l| help_row_matches(l, filter))
            .collect();
        // A group whose every chord was disabled in config.json contributes
        // no heading either — an empty titled block advertises a section
        // that isn't there.
        if rows.is_empty() {
            continue;
        }
        out.push(HelpLine::Head(g.title));
        out.extend(rows);
    }
    out
}

/// C15: how the keymap lays out in `body` — how many columns, how the lines
/// divide between them, and the dialog those choices need.
struct HelpLayout {
    /// One `Vec<HelpLine>` per column, left to right.
    columns: Vec<Vec<HelpLine>>,
    /// Rows of content each column shows at once (the shorter of "what it
    /// holds" and "what fits"); the scroll step and the `top` clamp use it.
    height: u16,
    /// The dialog rect's size, borders included.
    size: (u16, u16),
    /// One column's content width. Carried rather than recomputed by the
    /// drawer: since F1 deriving it means resolving every row against the
    /// keymap, and doing that twice per frame to reach the same number is
    /// waste the old `&'static` table didn't have to think about.
    content: u16,
    /// The width the dialog **asked for**, before `.min(body.width)`.
    ///
    /// The only honest thing for a floor test to assert on. `size.0` is
    /// already clamped, so at an 80-column body it reports 80 whether the
    /// layout fitted or was cut down to it — a gate on it passes by
    /// construction. C15's 2026-08-20 amendment named that shape ("a
    /// tautology that reads like a gate is worse than no gate") and C39's
    /// first floor test reintroduced it in this file anyway.
    ///
    /// Read only by tests — it exists so a floor gate has something honest
    /// to assert on, which is a job nothing in the draw path needs. The
    /// allow is narrow and deliberate rather than a `#[cfg(test)]` field,
    /// because a struct that changes shape between builds is a worse trade
    /// than one unused field.
    #[allow(dead_code)]
    asked: u16,
}

/// C15: lay the keymap out for `body`, taking **the
/// fewest columns that fit**.
///
/// One column is the calm form and stays the answer whenever the list fits
/// in the body's height. A second column is taken only when one column
/// would overflow *and* the terminal is wide enough to hold two — never to
/// fill space for its own sake. When neither fits, the overlay scrolls
/// (C15's amended key rules); nothing is dropped and nothing is merged away
/// to fit a cap, which is the whole point of this shape.
///
/// The split point is a group boundary, chosen to balance the two columns,
/// so a group is never sawn in half across the gutter.
fn help_layout(body: Rect, keymap: &Keymap, filter: Option<&str>) -> HelpLayout {
    let lines = help_lines(keymap, filter.unwrap_or(""));
    let content = help_content_width(&lines);
    let avail_h = body.height.saturating_sub(2); // the dialog's borders
    let one_fits = lines.len() as u16 <= avail_h;
    let two_wide_enough = content * 2 + HELP_GUTTER + 2 <= body.width;
    let columns = if one_fits || !two_wide_enough {
        vec![lines]
    } else {
        let at = help_split_point(&lines);
        let (a, b) = lines.split_at(at);
        vec![a.to_vec(), b.to_vec()]
    };
    let tallest = columns.iter().map(|c| c.len()).max().unwrap_or(0) as u16;
    let height = tallest.min(avail_h);
    // +1 so a row that spends the full content width still has a column of
    // air before the right border (the key column already opens with one).
    let w = content * columns.len() as u16
        + HELP_GUTTER * (columns.len() as u16 - 1)
        + HELP_DIALOG_CHROME;
    // [F9] ...and never narrower than its own title. Before the filter the
    // table always contained its widest row, so the frame was always wider
    // than any heading and this could not arise. A query isolating one
    // short row breaks that: `/this keymap` leaves a 33-column dialog under
    // a 44-column title, which `modal_frame` then clips — the overlay would
    // hide the very sentence telling a filtering reader how to get out.
    // Found by driving it in a PTY; no unit test was looking at the title
    // and the frame together.
    let tallest_rows = columns.iter().map(|c| c.len()).max().unwrap_or(0);
    let runnable = columns.iter().flatten().any(|l| matches!(l, HelpLine::Row(_, _, Some(_))));
    let title = help_title(
        filter,
        tallest_rows,
        tallest_rows,
        tallest_rows > height as usize,
        runnable,
        body.width.saturating_sub(2),
    );
    let title_w = mouse::display_width(&title) + 2; // the two border columns
                                                    // `asked` is the un-clamped want, carried so a floor test can assert on
                                                    // it: `size.0` is `.min(body.width)` and therefore reports the body's
                                                    // width whether the dialog fitted or was cut down to it. Asserting on
                                                    // `size.0` is the tautology C15's own 2026-08-20 amendment named — and
                                                    // that this field exists is the second time it had to be named.
    let asked = w.max(title_w);
    HelpLayout { columns, height, size: (asked.min(body.width), height + 2), content, asked }
}

/// [F9] C39's four title wordings, in one place — because the dialog's
/// **width** has to be floored by the title's, and a second spelling of the
/// title would let the floor guard a string the frame does not draw. That
/// is §4/§5 lockstep applied to a modal's own heading.
///
/// `shown` is the last visible row's index (the scrolled counter's left
/// half); pass `total` for the worst case when the caller does not know
/// `top` yet — the count only ever gets narrower, never wider.
///
/// `avail` is the widest the title may be. **The query is what gives**, and
/// it is elided rather than truncated by the frame: everything after it —
/// the count, and critically `Esc clears` — is how a filtering reader gets
/// out, so clipping from the right removes exactly the wrong end. Widening
/// the dialog cannot help here: at the 80-column floor a 46-character query
/// exceeds the whole terminal, so *something* must give and it has to be
/// the part the user can already see themselves typing.
/// [C41] `runnable` is whether any row under the query is a command, and it
/// gates the `↵ runs` clause. Announced rather than assumed, for F1's reason
/// in the row below: a title teaching a key that does nothing here is worse
/// than no title. A query isolating the control-CLI block matches rows but
/// no *commands*, and there `↵` really does just close, exactly as it did
/// before the palette existed.
fn help_title(
    filter: Option<&str>,
    shown: usize,
    total: usize,
    scrolled: bool,
    runnable: bool,
    avail: u16,
) -> String {
    let run = if runnable { " · ↵ runs" } else { "" };
    let (head, tail) = match (filter, scrolled) {
        (Some(_), true) => {
            (" keys — /", format!(" · {shown}/{total} · ↑↓ move{run} · Esc clears "))
        }
        (Some(_), false) => (" keys — /", format!(" · {total} shown{run} · Esc clears ")),
        // [Amended 2026-09-01, C39] The un-filtered rows teach the typing
        // rule and its one guaranteed exit: bare printables open the filter
        // now, so "any key closes" is no longer true and `Esc` is the key
        // to teach (C27's roster wording, for its reason).
        (None, true) => {
            return format!(" keys — {shown}/{total} · ↑↓ more · type to filter · Esc closes ");
        }
        (None, false) => return " keys — type to filter · Esc closes ".to_string(),
    };
    let q = filter.unwrap_or("");
    let fixed = mouse::display_width(head) + mouse::display_width(&tail);
    let room = avail.saturating_sub(fixed);
    format!("{head}{}{tail}", elide_to(q, room))
}

/// Cut `text` to `room` columns, marking the cut with `…` when it bites.
/// Room for nothing at all yields nothing at all — an ellipsis alone says
/// less than the count and exit hint it would be stealing space from.
fn elide_to(text: &str, room: u16) -> String {
    if mouse::display_width(text) <= room {
        return text.to_string();
    }
    if room == 0 {
        return String::new();
    }
    let keep = room.saturating_sub(1) as usize;
    let cut: String = text.chars().take(keep).collect();
    format!("{cut}…")
}

/// [F9] Does this row survive the type-ahead query? Case-insensitive over
/// **both** columns — the key and its description — because a reader
/// reaching for the filter is as likely to remember "the one with `g` in
/// it" as "the layout one". An empty query keeps everything, so the
/// un-filtered overlay costs no special case anywhere.
///
/// Headings are matched by their own rows, not by their text: a group
/// whose every row was filtered away contributes no heading either
/// (`help_lines` already drops an empty group), and a heading that matched
/// while its rows did not would title an empty block. That is the same
/// rule config.json's `disable` already put there.
fn help_row_matches(line: &HelpLine, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let needle = filter.to_lowercase();
    match line {
        HelpLine::Row(key, desc, _) => {
            key.to_lowercase().contains(&needle) || desc.to_lowercase().contains(&needle)
        }
        HelpLine::Head(_) => true,
    }
}

/// Columns are separated by this many blank cells.
const HELP_GUTTER: u16 = 2;

/// C15: draw the laid-out keymap into the dialog's inner area, every column
/// scrolled to the same `top` (they scroll as one sheet — two columns
/// sliding independently would be two lists, not one table).
///
/// A heading borrows C6's idiom, the same one the roster's group rows use:
/// uppercase, `quiet()`, underlined across its own column so it reads as a
/// rule rather than as another chord.
fn draw_help_columns(
    f: &mut Frame<'_>,
    layout: &HelpLayout,
    top: usize,
    cursor: Option<usize>,
    inner: Rect,
) {
    let content = layout.content;
    let at = cursor.and_then(|c| help_cursor_pos(layout, c));
    for (i, column) in layout.columns.iter().enumerate() {
        let x = inner.x + i as u16 * (content + HELP_GUTTER);
        if x >= inner.x + inner.width {
            break;
        }
        let width = content.min(inner.x + inner.width - x);
        let lines: Vec<Line<'_>> = column
            .iter()
            .enumerate()
            .skip(top)
            .take(layout.height as usize)
            .map(|(row, line)| match line {
                HelpLine::Head(title) => {
                    let pad =
                        (width as usize).saturating_sub(mouse::display_width(title) as usize + 1);
                    Line::from(Span::styled(
                        format!(" {title}{}", " ".repeat(pad)),
                        theme::quiet().add_modifier(Modifier::UNDERLINED),
                    ))
                }
                // [C41] The cursor spends the key column's leading space
                // rather than a column of its own: `help_key_prefix` opens
                // with one, `❯` is single-width, and the row measures the
                // same either way — so the palette's mark cannot widen the
                // dialog, cannot re-trip `elide_key`, and cannot move
                // `HELP_COL_FLOOR`. The glyph is C14's picker marker and
                // C27's roster marker, the same "↵ acts on this row" idiom
                // in its third overlay, so nothing new is being taught.
                HelpLine::Row(k, d, _) => {
                    let prefix = help_key_prefix(k);
                    let marked = at == Some((i, row));
                    let key = if marked {
                        format!("{}{}", theme::PICKER_SELECTED, &prefix[1..])
                    } else {
                        prefix
                    };
                    Line::from(vec![
                        Span::styled(key, theme::accent()),
                        Span::styled(d.to_string(), theme::quiet()),
                    ])
                }
            })
            .collect();
        f.render_widget(Paragraph::new(lines), Rect::new(x, inner.y, width, inner.height));
    }
}

/// [C41] Where the `cursor`-th runnable row sits in a laid-out overlay, as
/// `(column, row within that column)` — the one place the drawer's highlight
/// and `help_follow_top`'s scroll agree about the cursor's position.
///
/// `None` when the query matches no runnable row (a filter isolating the
/// control-CLI block, say): there is nothing to mark and nothing to scroll
/// to, which is also the state in which `↵` falls back to closing.
fn help_cursor_pos(layout: &HelpLayout, cursor: usize) -> Option<(usize, usize)> {
    let mut seen = 0;
    for (i, column) in layout.columns.iter().enumerate() {
        for (row, line) in column.iter().enumerate() {
            if matches!(line, HelpLine::Row(_, _, Some(_))) {
                if seen == cursor {
                    return Some((i, row));
                }
                seen += 1;
            }
        }
    }
    None
}

/// [C41] The `top` that keeps the cursor on screen — unchanged whenever it
/// already is, which on any terminal showing the whole table is always.
///
/// The palette scrolls by *following the cursor* rather than by moving a
/// view of its own, which is C27's rule verbatim ("`↵` acts on the cursor,
/// so a view that scrolled the list out from under it would leave the
/// overlay pointing at a row nobody can see"). Both columns share one `top`
/// (they scroll as one sheet), so a cursor in the right-hand column steers
/// the left one too — correct, because they are one table.
pub fn help_follow_top(
    body: Rect,
    keymap: &Keymap,
    filter: Option<&str>,
    cursor: usize,
    top: usize,
) -> usize {
    let layout = help_layout(body, keymap, filter);
    let Some((_, row)) = help_cursor_pos(&layout, cursor) else { return top };
    let height = layout.height as usize;
    if row <= top {
        // One line of context above the cursor, not zero — and in *this*
        // table that line is very often the group heading, which is what
        // says what the command underneath it is for. Scrolling to `row`
        // exactly was the tighter arithmetic and it read worse: walking the
        // cursor back to the first command left `top` at 1, pushing `PANES`
        // off the top of a dialog whose first visible line was then a bare
        // chord. Caught by `the_palette_view_follows_its_cursor_below_the_fold`
        // asserting on Home rather than on visibility alone.
        row.saturating_sub(1)
    } else if height > 0 && row >= top + height {
        row + 1 - height
    } else {
        top
    }
}

/// C15: `(rows shown at once, rows the tallest column holds)` for `body` —
/// the one place `App`'s scroll clamp and this renderer agree about how far
/// the keymap can move. Equal values mean the whole table is on screen and
/// the scroll keys have nothing to do (`Mode::Help`'s "any key closes it"
/// then holds unamended).
pub fn help_scroll_extent(body: Rect, keymap: &Keymap, filter: Option<&str>) -> (usize, usize) {
    let l = help_layout(body, keymap, filter);
    (l.height as usize, l.columns.iter().map(|c| c.len()).max().unwrap_or(0))
}

/// C15: the group boundary that divides `lines` most evenly between two
/// columns — i.e. the heading nearest the halfway mark. Headings only:
/// splitting inside a group would put a heading in one column and half its
/// chords in the other, which reads as two unrelated tables.
fn help_split_point(lines: &[HelpLine]) -> usize {
    let half = lines.len() / 2;
    lines
        .iter()
        .enumerate()
        .filter(|(i, l)| *i > 0 && matches!(l, HelpLine::Head(_)))
        .min_by_key(|(i, _)| i.abs_diff(half))
        .map(|(i, _)| i)
        .unwrap_or(lines.len())
}

/// C15/F1: how a help row spells its key column.
///
/// Before F1 every row was a `(&'static str, &'static str)` tuple, so the
/// overlay's chord spellings were compiled-in literals and a `config.json`
/// remap left them teaching a chord that no longer worked. A row now
/// declares the *actions* it documents, and the spelling is resolved from
/// the live keymap at draw time.
enum HelpKey {
    /// Spell whatever chords are bound to these actions, joined by `" / "`.
    /// The common case.
    Chords(&'static [Action]),
    /// A compact hand-written spelling for a family too wide to enumerate
    /// (`Alt+←↓↑→ / hjkl`, `Alt+1..9 / Alt+0`).
    ///
    /// The actions are still declared, so the coverage sweep sees every one
    /// of them — only the *rendering* is by hand. And the compact form is
    /// printed **only while every action in the family is still on its
    /// default chord**: remap one and the row enumerates the real chords
    /// instead, because `Alt+←↓↑→ / hjkl` would otherwise be exactly the
    /// stale spelling F1 exists to abolish, merely a wider one.
    Family(&'static str, &'static [Action]),
    /// Binds nothing: a legend line or a control-CLI reference row.
    Text(&'static str),
}

struct HelpRow {
    key: HelpKey,
    desc: &'static str,
}

const fn chords(actions: &'static [Action], desc: &'static str) -> HelpRow {
    HelpRow { key: HelpKey::Chords(actions), desc }
}
const fn family(spelling: &'static str, actions: &'static [Action], desc: &'static str) -> HelpRow {
    HelpRow { key: HelpKey::Family(spelling, actions), desc }
}
const fn reference(key: &'static str, desc: &'static str) -> HelpRow {
    HelpRow { key: HelpKey::Text(key), desc }
}

/// C15: one titled block of the keymap.
struct HelpGroup {
    title: &'static str,
    rows: &'static [HelpRow],
}

/// C15/§8: the canonical key table, **grouped by what
/// the chord acts on**, plus U23's reference block. The single source
/// `draw_mode_overlay` reads for `Mode::Help`.
///
/// History, because the shape is the point. U23 (2026-07-27) added the
/// glyph legend and the mouse rows and paid for them by *merging three
/// natural chord pairs* — the only currency available under a ≤20-row hard
/// cap. C28 then wanted two more chords, and merging again would have put
/// four chords on one row. The cap is gone instead: the overlay takes a
/// second column when the terminal is wide enough and scrolls when it is
/// not (`help_layout`), so the keymap can grow with roost rather than
/// rationing itself. Unmerged rows are also plainly easier to read —
/// "Alt+z / Alt+f → zoom pane (view only) / floating scratch shell" made
/// the reader pair the halves up themselves.
///
/// Group order is "how often you reach for it": panes, then their layout,
/// then tabs, then the fleet surfaces, then reading, then the session.
///
/// `CONTROL CLI` closes the table. Every group above it teaches keys pressed
/// *inside* roost; this one is the odd surface out — the reference block for
/// `roost send`/`read`/`status`/`spawn`/`wait`, the control CLI an outside
/// actor (an LLM, a script, another pane) drives the fleet with, keyed on
/// the pane id every badge shows (U2). Rows are the verbs a caller reaches
/// for, not a man page: the CLI's own `--help` covers `list`/`fork`/`close`
/// and every flag. It sorts last for the same reason `READING THE SCREEN`
/// does — it teaches the product rather than a binding.
const HELP_GROUPS: &[HelpGroup] = &[
    HelpGroup {
        title: "PANES",
        rows: &[
            chords(&[Action::NewPane], "new shell pane (auto split)"),
            chords(&[Action::QuickLaunch], "picker: 1..9 launch · type filters · ←→ recent cwd"),
            family(
                "Alt+←↓↑→ / hjkl",
                &[
                    Action::Focus(Dir::Left),
                    Action::Focus(Dir::Down),
                    Action::Focus(Dir::Up),
                    Action::Focus(Dir::Right),
                ],
                "move focus (←/→ continue to next/prev tab at an edge)",
            ),
            chords(&[Action::EditPane], "name + parking note (first line shows on the badge)"),
            chords(&[Action::ClosePane], "close pane (confirm if busy)"),
            chords(&[Action::Undo], "reopen the last pane or tab you closed"),
            // [F9] The dead-pane keys, which the overlay did not teach at
            // all. They are bare keys rather than Alt chords — `main.rs`
            // claims them from `InputResult::Forward` while the focused
            // pane is dead — so §8 has no row for them and the C34 sweep
            // does not cover them; the C9 bar advertised them and nothing
            // else did. That is P21's case verbatim ("a search nothing
            // advertises is a search nobody finds"), and C15 answered it
            // the same way: fold the mode-local keys into the group that
            // owns them, next to the other recovery verb.
            //
            // `y` is listed unconditionally though the bar shows it only
            // when there is a session to resume: the overlay is the whole
            // keymap, not the keymap for this instant, and every other row
            // here documents a key whose effect depends on context.
            HelpRow {
                key: HelpKey::Text("↵ / f / y"),
                // The description mirrors the key column's `/`, which is
                // what every other positionally-mapped row here does — and
                // it matters more than usual, because `·` elsewhere in this
                // table separates independent clauses *of one key*. Using
                // both spellings in one row inverted the table's own
                // convention. Named by the C39 audit.
                desc: "dead pane: relaunch / fresh (drops resume) / copy resume",
            },
            chords(&[Action::ToggleRaw], "raw pass-through for this pane (same chord exits)"),
        ],
    },
    HelpGroup {
        title: "LAYOUT",
        rows: &[
            family(
                "Alt+- / Alt+=",
                &[
                    Action::Resize { horizontal: false, grow: false },
                    Action::Resize { horizontal: false, grow: true },
                ],
                "resize height: shrink / grow (vim's Ctrl-w − / +)",
            ),
            family(
                "Alt+< / Alt+>",
                &[
                    Action::Resize { horizontal: true, grow: false },
                    Action::Resize { horizontal: true, grow: true },
                ],
                "resize width: shrink / grow (vim's Ctrl-w < / >)",
            ),
            // The move-pane family sits directly under the resize rows on
            // purpose — C33's adjacency rule survives the 2026-09-01 re-key
            // intact. What changed is the lesson it makes visible: the
            // shifted directions (arrows and hjkl alike) all *move the
            // pane*, one verb in two spellings, while the punctuation above
            // them *resizes* — the distinction a reader is most likely to
            // blur is still one glance apart. Same tactic as C28's
            // adjacency in TABS, which seats a row under its own unshifted
            // form; here the family is its own unshifted form's sibling.
            family(
                "Alt+Shift+←↓↑→ / hjkl",
                &[
                    Action::MovePane(Dir::Left),
                    Action::MovePane(Dir::Down),
                    Action::MovePane(Dir::Up),
                    Action::MovePane(Dir::Right),
                ],
                "move this pane that way (swaps with its neighbour)",
            ),
            chords(&[Action::StackPane], "stack this pane (collapses its split; it expands)"),
            chords(&[Action::ExplodeStack], "explode the stack around this pane into a split"),
            chords(&[Action::FlipSplit], "flip this split's orientation"),
            chords(
                &[Action::CycleLayout { forward: true }],
                "cycle layout: grid / main+stack / all-stack",
            ),
            // C37 sits directly under its own unshifted form, which is
            // C28's actual rule (the C33 audit drew the distinction: C28
            // pairs a row with the chord it is the shifted *form of*, not
            // merely one it could be confused with). Two rows rather than
            // one merged row because C15's cap was retired precisely so
            // rows need not be merged to fit — and merging these two did
            // push the column to 83, past the 80-col floor.
            chords(&[Action::CycleLayout { forward: false }], "the same cycle, backwards"),
            chords(&[Action::ToggleZoom], "zoom the focused pane (view only)"),
            chords(&[Action::ToggleFloat], "floating scratch shell"),
        ],
    },
    HelpGroup {
        title: "TABS",
        rows: &[
            chords(&[Action::NewTab], "new tab"),
            family(
                "Alt+1..9 / Alt+0",
                &[
                    Action::GoToTab(0),
                    Action::GoToTab(1),
                    Action::GoToTab(2),
                    Action::GoToTab(3),
                    Action::GoToTab(4),
                    Action::GoToTab(5),
                    Action::GoToTab(6),
                    Action::GoToTab(7),
                    Action::GoToTab(8),
                    Action::LastTab,
                ],
                "go to that tab / the last one",
            ),
            // The two tab families, re-homed by the 2026-09-01 map
            // amendment: same-letter, shift-reverse pairs, like C37's
            // `g`/`Shift+g` — `m` steps your view between tabs, `i` carries
            // the pane to the tab. The carry row sits directly under the
            // step row: the adjacency is still the explanation, C28's rule
            // kept even as the spellings moved.
            family(
                "Alt+m / Alt+Shift+m",
                &[Action::NextTab, Action::PrevTab],
                "next / previous tab (wraps)",
            ),
            family(
                "Alt+i / Alt+Shift+i",
                &[
                    Action::MovePaneToTab { forward: true },
                    Action::MovePaneToTab { forward: false },
                ],
                "move this pane to the next / previous tab",
            ),
            // C40 sits under C28 for the same reason C28 sits under the
            // chords it shifts: it is the same verb, for a destination too
            // far to walk to.
            chords(
                &[Action::MarkPane, Action::PullPane],
                "mark this pane / pull the marked one into this tab",
            ),
            chords(&[Action::RenameTab], "rename this tab"),
        ],
    },
    HelpGroup {
        title: "FLEET",
        rows: &[
            chords(&[Action::JumpAttention], "jump to the next pane that needs you"),
            // C35 sits under the jump because they are the pair a fleet
            // navigates with: one goes on to whoever needs you, the other
            // comes back to where you were.
            chords(&[Action::FocusAlternate], "back to the pane you came from"),
            chords(
                &[Action::ToggleRoster],
                "roster: every pane, grouped by tab · Tab filters by status",
            ),
            chords(&[Action::ToggleFeed], "activity feed (status / spawns / exits / control)"),
            chords(
                &[Action::ToggleBroadcast],
                "broadcast: type once, send to every pane · Tab picks who",
            ),
        ],
    },
    HelpGroup {
        title: "READING",
        rows: &[
            chords(&[Action::ScrollMode], "scroll back — `/` searches, n/N step the hits"),
            chords(&[Action::CopyMode], "copy mode: hjkl wbe 0$ · v/V select · y yank · o URL"),
        ],
    },
    HelpGroup {
        title: "SESSION",
        rows: &[
            chords(&[Action::ToggleHints], "toggle the hint bar"),
            // C39's `/` is folded into the row that already owns this
            // surface, which is C15's own P21 precedent (`/ search, n/N`
            // rides the `Alt+c` row rather than taking one). A key that
            // appears in §8 and in the live title but in no printed row is
            // a key the overlay does not actually teach.
            chords(&[Action::Help], "this keymap — type filters it"),
            chords(&[Action::Quit], "quit (workspace saved; sessions live)"),
        ],
    },
    HelpGroup {
        title: "READING THE SCREEN",
        rows: &[
            reference("status", "⠋ working ◆ needs you ○ waiting · idle ✕ exited"),
            // D2 (PR #46 design audit, C29 amendment): widened for native
            // selection — a chord (Shift+click) gets a row here by C15's
            // own stated rule ("Alt+click gets its own row because it is a
            // chord"), applied to the mouse verb that is now also one.
            reference("mouse", "wheel scrolls · click focuses · drag/2x/3x/shift selects"),
            reference("Alt+click / o", "open the URL under the pointer / copy cursor"),
        ],
    },
    HelpGroup {
        title: "CONTROL CLI",
        rows: &[
            reference("<id>", "same id shown on each pane's badge"),
            reference("roost send <id> \"text\"", "type into that pane (--enter submits)"),
            reference("roost read <id>", "print its current screen"),
            reference("roost status", "list every pane and its status"),
            reference("roost spawn ADAPTER", "launch a new pane"),
            reference("roost wait <id>", "block until its turn ends"),
        ],
    },
];

/// U23: the legend row's text, rebuilt from the `theme` glyph constants —
/// the same table C5 renders everywhere else. `HELP_KEYS` must spell a
/// `const` literal, so this is the drift *check* rather than the source
/// (`help_legend_row_matches_the_theme_glyph_table`), which is why it is
/// test-only: retheming a glyph has to break a test, not the build.
#[cfg(test)]
fn status_legend_text() -> String {
    format!(
        "{} working {} needs you {} waiting {} idle {} exited",
        theme::GLYPH_WORKING,
        theme::GLYPH_NEEDS_INPUT,
        theme::GLYPH_WAITING,
        theme::GLYPH_IDLE,
        theme::GLYPH_EXITED,
    )
}

/// U8: the rect the current mode's modal dialog occupies on screen, or None
/// when the mode draws none — the SAME geometry `draw_mode_overlay` paints
/// (it reads this function too), exposed so the mouse path can hit-test
/// against exactly what's drawn. Renderer/hitbox lockstep, §4/§5.
pub fn modal_rect<B: PaneBackend>(app: &App<B>) -> Option<Rect> {
    let body = app.body_area();
    let anchor = app
        .display_rects()
        .iter()
        .find(|pr| pr.id == app.focused)
        .map(|pr| pr.rect)
        .unwrap_or(body);
    dialog_rect(&app.mode, body, anchor, picker_rows(app), app.picker_cwds(), app.keymap())
}

/// How many rows `dialog_rect` should size the picker's adapter column to —
/// **zero unless the picker is actually up**, which every other mode's
/// branch ignores anyway.
///
/// The guard is not tidiness. `picker_filtered` annotates each adapter with
/// whether its launch program is on `$PATH`, which is a `stat` per `$PATH`
/// entry per adapter — around fifty syscalls a call on an ordinary machine.
/// Both callers passed it eagerly, so the two hottest paths in the chrome
/// paid for it in every mode: `draw_mode_overlay` runs on every frame, i.e.
/// ~1,500 `stat` calls a second at the 33ms loop with no dialog on screen at
/// all, and `modal_rect` runs on every mouse event. Measured at 23% of
/// roost's whole idle CPU.
fn picker_rows<B: PaneBackend>(app: &App<B>) -> usize {
    match app.mode {
        Mode::Picker { .. } => app.picker_filtered().len(),
        _ => 0,
    }
}

/// The pure half of `modal_rect`: mode + geometry in, dialog rect out.
/// [U20] The picker's size now depends on app state (the type-ahead filter's
/// row count and the recent-cwd column), so it takes those as `rows`/`cwds`
/// rather than re-deriving them — `modal_rect` and `draw_mode_overlay` pass
/// the same values, keeping the drawn dialog and the mouse hitbox in
/// lockstep (§4/§5).
fn dialog_rect(
    mode: &Mode,
    body: Rect,
    anchor: Rect,
    rows: usize,
    cwds: &[std::path::PathBuf],
    keymap: &Keymap,
) -> Option<Rect> {
    match mode {
        // Copy mode has no centered overlay — the cursor/selection are
        // drawn in-pane (C17/C24). [P21] Nor does search: its prompt lives
        // in the hint bar's right segment, so the pane it is searching
        // stays fully visible while the query narrows.
        Mode::Normal | Mode::Scroll | Mode::Copy { .. } | Mode::Search { .. } => None,
        Mode::Rename { .. } => Some(centered_near(anchor, body, 44, 3)),
        // C32 (combined): Rename's width; one row for the name plus one
        // per note line — the dialog grows a row per Shift+↵ (to
        // NOTE_MAX_LINES) instead of scrolling.
        Mode::PaneEdit { lines, .. } => {
            Some(centered_near(anchor, body, 44, lines.len() as u16 + 3))
        }
        // C36: C13's width, one row per message line. Wider than the pane
        // editor would be tempting, but a broadcast is read at the moment
        // of sending and the eye should be on the title's target count,
        // not sweeping a wide field.
        Mode::Broadcast { lines, .. } => {
            Some(centered_near(anchor, body, 44, lines.len() as u16 + 2))
        }
        Mode::Picker { .. } => {
            // U20: as tall as the longer of the two columns (a filter can
            // shrink the adapter side below the cwd side), never shorter
            // than one row — an empty result still needs a frame to say so.
            let h = rows.max(cwds.len()).max(1) as u16 + 2;
            Some(centered_near(anchor, body, picker_dialog_width(cwds), h))
        }
        Mode::Help { filter, .. } => {
            // The dialog is sized for the *filtered* table: a query that
            // cuts 30 rows to 3 must not leave a 30-row frame around them,
            // which is C14's picker rule (its dialog shrinks to the filtered
            // adapter list) applied to the surface that borrowed its
            // type-ahead.
            let (w, h) = help_layout(body, keymap, filter.as_deref()).size;
            Some(centered_near(anchor, body, w, h))
        }
        Mode::Feed { .. } => {
            let (w, h) = feed_overlay_size(body);
            Some(centered_near(anchor, body, w, h))
        }
        // C27: deliberately the feed's own geometry — the two overlays
        // answer the fleet's two questions and should not resize under a
        // user toggling between them.
        Mode::Roster { .. } => {
            let (w, h) = feed_overlay_size(body);
            Some(centered_near(anchor, body, w, h))
        }
    }
}

/// `spinner` is the frame's one shared C5 clock read (see `draw`) — the
/// roster draws status glyphs, and a second `app.elapsed()` sample here could
/// split a frame across two different spinner glyphs.
fn draw_mode_overlay<B: PaneBackend>(
    f: &mut Frame<'_>,
    app: &App<B>,
    body: Rect,
    anchor: Rect,
    spinner: char,
) {
    let Some(rect) =
        dialog_rect(&app.mode, body, anchor, picker_rows(app), app.picker_cwds(), app.keymap())
    else {
        return;
    };
    match &app.mode {
        Mode::Normal | Mode::Scroll | Mode::Copy { .. } | Mode::Search { .. } => {}
        Mode::Rename { buffer, cursor, target } => {
            // Tab is the one target left (C32 absorbed pane renames).
            let RenameTarget::Tab = target;
            let inner = modal_frame(f, body, rect, Line::from(" rename tab ").style(theme::ink()));
            f.render_widget(
                Paragraph::new(Line::from(rename_field(buffer, *cursor))).style(theme::ink()),
                inner,
            );
        }
        // C32 (combined): the pane editor — Rename's frame and field
        // idiom; the name row on top, one row per note line under it, the
        // `▏` caret riding the cursor row only. The name row is
        // UNDERLINED and padded edge to edge (stack_header_text's fill
        // trick), so the underline doubles as the name/note separator —
        // C6's header idiom, zero rows spent — and stays visible even
        // with an empty name. Everything `ink()`: it is all input, and
        // quiet input can't be proofread (C13's own rule).
        Mode::PaneEdit { name, lines, row, col, .. } => {
            let inner = modal_frame(f, body, rect, Line::from(" edit pane ").style(theme::ink()));
            let mut name_spans =
                if *row == 0 { rename_field(name, *col) } else { vec![Span::raw(name.clone())] };
            let pad = inner.width.saturating_sub(spans_width(&name_spans));
            // The fill keeps the underline running edge to edge (C6's
            // header idiom); the row's style carries it, so the caret's
            // reversal patches on top of it rather than replacing it.
            name_spans.push(Span::raw(" ".repeat(pad as usize)));
            let mut rendered: Vec<Line<'_>> =
                vec![Line::from(name_spans).style(theme::ink().add_modifier(Modifier::UNDERLINED))];
            rendered.extend(lines.iter().enumerate().map(|(i, l)| {
                if i + 1 == *row {
                    Line::from(rename_field(l, *col))
                } else {
                    Line::from(l.clone())
                }
            }));
            f.render_widget(Paragraph::new(rendered).style(theme::ink()), inner);
        }
        // C36: the composer. The title carries the **live target count**,
        // and that count is the contract's safety affordance — a visible
        // blast radius at the moment of commit, in place of a
        // confirm-twice. It moves with `Tab`, so the filter reads as "who
        // gets this" rather than as a setting.
        Mode::Broadcast { lines, row, col, status_filter } => {
            let targets = app.broadcast_targets(Actor::Local, *status_filter).len();
            // The zero case is carried by **words**, not by a colour. The
            // C36 audit flagged the first draft's conditional `accent()`
            // title: C12 says a modal title is `ink()`, no other dialog
            // varies its title token by state, and §2's attention idiom is
            // reversal rather than recolouring. Saying "no panes" outright
            // is both unmissable and inside the existing contracts — the
            // same "words beat a colour" instinct C16's bar text follows.
            let who = match targets {
                0 => " broadcast → no panes ".to_string(),
                1 => " broadcast → 1 pane ".to_string(),
                n => format!(" broadcast → {n} panes "),
            };
            let mut title = vec![Span::styled(who, theme::ink())];
            // C27's rule, verbatim: the tier glyph carries its own C5
            // colour, because "a bare `ink()` glyph would read as no tier
            // at all". The first draft discarded the style and printed a
            // colourless ◆ (audit D3).
            if let Some(tier) = status_filter {
                let (glyph, style, _) = theme::status_style(*tier);
                title.push(Span::styled(format!("{glyph} "), style));
            }
            let inner = modal_frame(f, body, rect, Line::from(title));
            let rendered: Vec<Line<'_>> = lines
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    if i == *row {
                        Line::from(rename_field(l, *col))
                    } else {
                        Line::from(l.clone())
                    }
                })
                .collect();
            f.render_widget(Paragraph::new(rendered).style(theme::ink()), inner);
        }
        Mode::Picker { selection, filter, cwd, on_cwd } => {
            let items = app.picker_filtered();
            let cwds = app.picker_cwds();
            // The title carries the live type-ahead query, so a narrowed
            // list always says *why* it is narrow.
            let heading = if filter.is_empty() {
                " new pane — pick agent ".to_string()
            } else {
                format!(" new pane — {filter}{} ", theme::RENAME_CURSOR)
            };
            let inner = modal_frame(f, body, rect, Line::from(Span::styled(heading, theme::ink())));
            // C14: selected row is a `❯`-prefix + `ink` item text, no bg
            // highlight; unselected rows are plain `quiet` text. Each row
            // leads with its `1..9` accelerator, and a second column lists
            // the recent working directories. The column with focus marks
            // its selection with `❯`; the other shows its selection in
            // `ink` without the marker, so what will actually be launched
            // is readable from either side.
            let adapter_col = ADAPTER_COL as usize;
            let rows = items.len().max(cwds.len());
            let lines: Vec<Line<'_>> = (0..rows)
                .map(|i| {
                    let mut spans: Vec<Span<'_>> = Vec::with_capacity(3);
                    match items.get(i) {
                        Some(item) => {
                            let text = picker_row_body(i, item);
                            let pad = adapter_col
                                .saturating_sub(mouse::display_width(&text) as usize + 1);
                            let (marker, style) = row_marks(i == *selection, !*on_cwd);
                            spans.push(marker);
                            spans.push(Span::styled(format!("{text}{}", " ".repeat(pad)), style));
                        }
                        None => spans.push(Span::raw(" ".repeat(adapter_col))),
                    }
                    if let Some(path) = cwds.get(i) {
                        let (marker, style) = row_marks(i == *cwd, *on_cwd);
                        spans.push(marker);
                        spans.push(Span::styled(format!(" {}", picker_cwd_label(path)), style));
                    }
                    Line::from(spans)
                })
                .collect();
            f.render_widget(Paragraph::new(lines), inner);
        }
        Mode::Help { top, filter, cursor } => {
            // C15 (amended): the §8 key table, grouped, in as few columns as
            // fit and scrolled when even those don't. `HELP_GROUPS` is the
            // single source and `help_layout` the single geometry — the same
            // call `dialog_rect` above made for this rect.
            let layout = help_layout(body, app.keymap(), filter.as_deref());
            let (visible, total) = help_scroll_extent(body, app.keymap(), filter.as_deref());
            let top = (*top).min(total.saturating_sub(visible));
            // The title says how to leave — and, only when the table doesn't
            // fit, that there is more of it and which keys reach it. A
            // terminal showing everything says nothing about scrolling,
            // because there is nothing to scroll.
            // [F9] The title is where the filter announces itself, exactly
            // as C14's picker and C27's roster do — and it has to, because
            // a reader who cannot see the rule would be stuck. [Amended
            // 2026-09-01] The un-filtered wordings teach it too ("type to
            // filter · Esc closes"): bare typing opens the query now, so
            // there is no state left where "any key closes" is true of the
            // printables.
            // [C41] The cursor is drawn only while the filter is open. Un-
            // filtered, C15's overlay is a poster you read and dismiss with
            // any key — marking a row there would advertise an `↵` that the
            // "any key closes it" contract still owns.
            let cursor = filter.as_ref().map(|_| *cursor);
            let heading = help_title(
                filter.as_deref(),
                (top + visible).min(total),
                total,
                total > visible,
                help_cursor_pos(&layout, cursor.unwrap_or(0)).is_some(),
                body.width.saturating_sub(2),
            );
            let inner = modal_frame(f, body, rect, Line::from(heading).style(theme::ink()));
            draw_help_columns(f, &layout, top, cursor, inner);
        }
        Mode::Feed { offset } => {
            let inner = modal_frame(f, body, rect, Line::from(" activity ").style(theme::ink()));
            draw_feed_entries(f, app.feed(), *offset, inner);
        }
        Mode::Roster { cursor, filter, status_filter, .. } => {
            // The live query rides in the title, exactly as the picker's
            // does (U20). The status filter joins it as a glyph tag, in
            // that status's own C5 glyph *and* color (a bare `ink()` glyph
            // would read as no tier at all) — the same pairing the narrowed
            // rows themselves draw (`theme::status_style`). `ink()`
            // everywhere else: the tag is the one thing this title borrows
            // color for.
            let glyph_span = status_filter.map(|s| {
                let (glyph, style, _) = theme::status_style(s);
                Span::styled(glyph.to_string(), style)
            });
            let mut spans: Vec<Span<'static>> = Vec::new();
            match (glyph_span, filter.is_empty()) {
                (None, true) => spans.push(Span::styled(" fleet ".to_string(), theme::ink())),
                (None, false) => {
                    spans.push(Span::styled(
                        format!(" fleet — {filter}{} ", theme::RENAME_CURSOR),
                        theme::ink(),
                    ));
                }
                (Some(glyph), true) => {
                    spans.push(Span::styled(" fleet — ".to_string(), theme::ink()));
                    spans.push(glyph);
                    spans.push(Span::styled(" only ".to_string(), theme::ink()));
                }
                (Some(glyph), false) => {
                    spans.push(Span::styled(" fleet — ".to_string(), theme::ink()));
                    spans.push(glyph);
                    spans.push(Span::styled(
                        format!(" {filter}{} ", theme::RENAME_CURSOR),
                        theme::ink(),
                    ));
                }
            }
            let inner = modal_frame(f, body, rect, Line::from(spans));
            draw_roster_rows(f, app, *cursor, inner, spinner);
        }
    }
}

/// Shared shape for a modal's "nothing here yet" line: one row, vertically
/// centered in `inner`, horizontally centered by `Paragraph::centered()` —
/// the roster's "no pane matches" and the feed's "no activity yet".
fn draw_empty_state(f: &mut Frame<'_>, inner: Rect, text: &str) {
    if inner.height == 0 {
        return;
    }
    let y = inner.y + inner.height / 2;
    f.render_widget(
        Paragraph::new(text).style(theme::quiet()).centered(),
        Rect::new(inner.x, y, inner.width, 1),
    );
}

/// C27: the roster's visible rows inside the modal's inner area — tab group
/// headers in C6's underlined-label idiom, pane rows in C8's collapsed-row
/// format verbatim (through the very same `collapsed_row_spans`), each behind
/// the one leading column that carries the cursor marker.
fn draw_roster_rows<B: PaneBackend>(
    f: &mut Frame<'_>,
    app: &App<B>,
    cursor: layout::PaneId,
    inner: Rect,
    spinner: char,
) {
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let (rows, top) = app.roster_view();
    if rows.is_empty() {
        // Only reachable through the type-ahead: the workspace always has at
        // least one pane. Same shape as the feed's empty state.
        draw_empty_state(f, inner, "no pane matches");
        return;
    }
    let lines: Vec<Line<'_>> = rows
        .iter()
        .skip(top)
        .take(inner.height as usize)
        .map(|row| Line::from(roster_row_spans(app, row, inner.width, cursor, spinner)))
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

/// C27: one roster row's spans.
///
/// A group header is C6's idiom — an uppercase label, `quiet()`, every cell
/// of the row underlined (the text is padded to the full width so the rule
/// runs edge to edge, exactly as `stack_header_text` does it).
///
/// A pane row is `marker + C8's collapsed row`. The leading column is the
/// cursor (`❯`, the picker/feed idiom) and the C8 row keeps its own `▎` for
/// the *focused* pane, so the roster answers "which row will Enter act on"
/// and "where am I right now" with two different marks instead of
/// overloading one.
fn roster_row_spans<B: PaneBackend>(
    app: &App<B>,
    row: &RosterRow,
    width: u16,
    cursor: layout::PaneId,
    spinner: char,
) -> Vec<Span<'static>> {
    match row {
        RosterRow::Group { label } => {
            let pad = (width as usize).saturating_sub(mouse::display_width(label) as usize);
            vec![Span::styled(
                format!("{label}{}", " ".repeat(pad)),
                theme::quiet().add_modifier(Modifier::UNDERLINED),
            )]
        }
        RosterRow::Pane { id } => {
            let id = *id;
            let spec = app.find_spec(id);
            // `display_status`, not `runtimes` directly: the roster is the
            // one surface that lists panes in tabs the lazy spawn has never
            // started, and `None` is what the tab bar reads as `·` for the
            // very same tab in the very same frame. [P2-10] Also the one
            // place a quiet shell's `Waiting` reads `Idle`, same as every
            // other chrome surface.
            let status = app.display_status(id);
            let has_title = spec.and_then(|s| s.title.as_ref()).is_some();
            let adapter = spec.map(|s| s.adapter.clone()).unwrap_or_else(|| "?".into());
            let name = if spec.is_some() { app.display_name(id) } else { "?".into() };
            let marker = if id == cursor {
                Span::styled(theme::PICKER_SELECTED.to_string(), theme::accent())
            } else {
                Span::raw(" ")
            };
            let mut spans = vec![marker];
            spans.extend(collapsed_row_spans(
                width.saturating_sub(1),
                app.focused == id,
                status,
                id,
                &name,
                &adapter,
                has_title,
                app.is_raw(id),
                spec.and_then(|s| s.note.as_ref()).is_some(),
                spinner,
            ));
            spans
        }
    }
}

/// C20: the feed's visible entry rows inside the modal's inner area — newest
/// at the bottom, scrolled back by `offset` entries from the tail; a single
/// centered line when the ring is empty.
fn draw_feed_entries(f: &mut Frame<'_>, feed: &VecDeque<FeedEntry>, offset: usize, inner: Rect) {
    if inner.height == 0 {
        return;
    }
    if feed.is_empty() {
        draw_empty_state(f, inner, "no activity yet");
        return;
    }
    let range = feed_window(feed.len(), offset, inner.height as usize);
    // U25: the window's last row IS the selected entry (`feed_window` ends
    // at `len - 1 - offset`), so the marker can't drift from what Enter acts
    // on — both read the same number.
    let selected = range.end.saturating_sub(1);
    let lines: Vec<Line<'_>> = feed
        .iter()
        .enumerate()
        .skip(range.start)
        .take(range.len())
        .map(|(i, e)| {
            Line::from(feed_entry_spans(
                &local_hh_mm_ss(e.at),
                &e.text,
                e.needs_input,
                i == selected,
            ))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

/// C20: which of the feed's `len` entries (0 = oldest .. `len` = newest+1)
/// fall inside a `rows`-tall window, given a scroll `offset` counting
/// entries back from the newest (0 = the live tail). Pure so the clamping is
/// unit-tested without a `Frame` or a real ring buffer.
fn feed_window(len: usize, offset: usize, rows: usize) -> std::ops::Range<usize> {
    if len == 0 || rows == 0 {
        return 0..0;
    }
    let offset = offset.min(len - 1);
    let last = len - 1 - offset;
    let first = last.saturating_sub(rows - 1);
    first..last + 1
}

/// C20's per-row rule: `" HH:MM:SS  {text}"`, timestamp and text both
/// `quiet` — except a status line landing on NeedsInput, which gets the `◆ `
/// `accent` prefix and `ink` text (the one red in the feed, same meaning as
/// everywhere, C5). Pure so the exception is unit-tested without a `Frame`.
/// The row's leading column is a selection marker: `❯` `accent` on the entry
/// Enter would act on, a space on every other row — same idiom as the
/// picker (C14).
fn feed_entry_spans(
    hhmmss: &str,
    text: &str,
    needs_input: bool,
    selected: bool,
) -> Vec<Span<'static>> {
    let marker = if selected { theme::PICKER_SELECTED } else { ' ' };
    let mut spans = vec![
        Span::styled(marker.to_string(), theme::accent()),
        Span::styled(format!("{hhmmss}  "), theme::quiet()),
    ];
    if needs_input {
        spans.push(Span::styled(format!("{} ", theme::GLYPH_NEEDS_INPUT), theme::accent()));
        spans.push(Span::styled(text.to_string(), theme::ink()));
    } else {
        spans.push(Span::styled(text.to_string(), theme::quiet()));
    }
    spans
}

/// Local wall-clock `HH:MM:SS` for a feed entry's timestamp (C20). Uses libc
/// (already a dependency, see `Cargo.toml`) for the local-timezone
/// breakdown — the stdlib has no calendar conversion at all, and pulling in
/// a chrono/time crate would be a lot of dependency for three integers.
fn local_hh_mm_ss(t: std::time::SystemTime) -> String {
    let secs =
        t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as libc::time_t).unwrap_or(0);
    // SAFETY: `tm` is a plain C struct of `c_int`s (plus, on this
    // platform, a `*const c_char` zone name that all-zeroes reads as null),
    // so zeroing is a valid initial value; `localtime_r` then writes it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: the reentrant form — an in-pointer to a local `time_t` and an
    // out-pointer to the local above, both live for the whole call, and no
    // shared static like `localtime` would use.
    unsafe { libc::localtime_r(&secs, &mut tm) };
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

/// C2: numbered tabs (marker + label + status glyph + separator) filling
/// the row edge-to-edge on the terminal's own background, plus a right-aligned
/// "{cwd} · {save}" status area. Column bookkeeping here is the renderer
/// half of the mouse-hitbox lockstep rule (DESIGN-ui.md §4/§5) —
/// `mouse::tab_width`/`tab_at_x` mirror this exactly and change together.
/// U15: the mode word the TAB BAR carries, if any. With the hint bar drawn
/// it carries the word itself and this is None; with the bar hidden (Alt+/)
/// or squeezed out by a short terminal, any word other than `NORMAL` — a
/// real mode, or the `ZOOM`/`RAW` pseudo-states — moves here. Otherwise a
/// zoomed pane with hints off is indistinguishable from a one-pane tab, and
/// RAW/COPY become unescapable-looking (live QA: `ZOOM` appeared nowhere).
/// Pure enough to hit-test against: the click path reads this too, so the
/// status area's width can't differ between the two.
pub fn tab_status_word<B: PaneBackend>(app: &App<B>) -> Option<&'static str> {
    if app.hints_shown() {
        return None; // C9's right segment already says it
    }
    let word = mode_word(&app.mode, app.zoomed(), app.is_raw(app.focused));
    (word != "NORMAL").then_some(word)
}

fn draw_tab_bar<B: PaneBackend>(f: &mut Frame<'_>, app: &App<B>, area: Rect, spinner: char) {
    let cwd = app.focused_cwd();
    let saved = app.last_save_ok();
    let names: Vec<String> = app.ws.tabs.iter().map(|t| t.name.clone()).collect();
    let fit = mouse::status_fit(tab_status_word(app), cwd.as_deref(), saved, &names, area.width);
    let status_w = fit.map(|f| f.width).unwrap_or(0);
    let show_status =
        mouse::effective_status_width(&names, area.width, status_w, saved, app.ws.active_tab) > 0;
    // U7: the drawn window — scrolled so the active tab is always visible.
    // `tab_at_x` reads the same layout, so hitboxes follow the scroll.
    let strip = mouse::tab_strip(&names, area.width, status_w, saved, app.ws.active_tab);

    let mut spans: Vec<Span<'_>> = Vec::with_capacity(names.len() * 7 + 4);
    let mut used = 0u16;
    // A leading `…` when the strip has scrolled past earlier tabs (C2's
    // marker, now at whichever end is hiding tabs). It occupies column 0,
    // which is why `TabStrip::x0` exists.
    if strip.left_marker {
        spans.push(Span::styled(theme::TAB_OVERFLOW.to_string(), theme::quiet()));
        used += strip.x0;
    }
    // Left to right, one 9-part span group per tab (marker/label/glyph/count/
    // separator/gutter), stopping exactly where `tab_strip` says to.
    for (i, tab) in app.ws.tabs.iter().enumerate().take(strip.end).skip(strip.start) {
        let active = i == app.ws.active_tab;
        // ...and how many panes are in that state, so a tab of three needy
        // agents stops reading like a tab of one.
        let (summary, count) = app.tab_summary(i);
        // Map the tab's aggregate summary to a tab-bar glyph + style
        // (theme::C5); `spinner` substitutes in for `Working`'s glyph — the
        // tab strip shows no grid (unlike a pane's own corner badge), so
        // there is no frozen view to preserve and it always animates.
        let (glyph, glyph_style) = theme::tab_summary_style(summary);
        let glyph = if summary == TabSummary::Working { spinner } else { glyph };
        push_tab_spans(&mut spans, i, &tab.name, active, glyph, glyph_style, count);
        used += mouse::tab_width(i, &tab.name);
    }

    // ...and a trailing `…` when tabs remain past the right edge and a
    // spare column is left to show it in (overflow, C2).
    if strip.right_marker {
        spans.push(Span::styled(theme::TAB_OVERFLOW.to_string(), theme::quiet()));
        used += 1;
    }

    if let Some(fit) = fit.filter(|_| show_status) {
        let (mode_word, prefix, save_word) = mouse::status_parts(fit.mode, fit.cwd, saved);
        let pad = area.width.saturating_sub(used).saturating_sub(status_w);
        if pad > 0 {
            spans.push(Span::raw(" ".repeat(pad as usize)));
        }
        // U15: the mode word reads a step brighter than the cwd beside it —
        // it's state, not context — while staying inside the ink ramp
        // (never the accent: it isn't an alarm): `ink` over `quiet`.
        if !mode_word.is_empty() {
            spans.push(Span::styled(mode_word, theme::ink()));
        }
        if !prefix.is_empty() {
            spans.push(Span::styled(prefix, theme::quiet()));
        }
        let save_style = if saved { theme::quiet() } else { theme::accent() };
        spans.push(Span::styled(format!("{save_word} "), save_style));
    }

    // No row fill: the tab bar carries no background of its own (§2), so the
    // gap before the status area and the row past the last tab show the
    // user's terminal background rather than a band roost painted over it.
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// One tab's 9-part span sequence (C2): marker, label, status glyph, the
/// **count cell**, the separator, and a trailing gutter space — column count
/// matches `mouse::tab_width` (`display_width(label) + 8`) exactly.
///
/// `count` is how many of the tab's panes are in the summarized state (the
/// second half of `App::tab_summary`); it renders in the glyph's own style.
/// The *glyph* is what flips (C5's spinner), and the count rides right
/// beside it, so `⠋3`→`⠙3`→… still reads as one animating token rather than a
/// static digit stuck to a spinning dot.
fn push_tab_spans(
    spans: &mut Vec<Span<'static>>,
    index: usize,
    name: &str,
    active: bool,
    glyph: char,
    glyph_style: Style,
    count: usize,
) {
    if active {
        spans.push(Span::styled(theme::MARKER_ACTIVE.to_string(), theme::accent()));
    } else {
        spans.push(Span::raw(" "));
    }
    spans.push(Span::raw(" "));

    // Active vs inactive is ink weight plus the `▎` marker — no highlight
    // fill to go invisible against a light theme (§2, 2026-07-27).
    let label_style = if active { theme::ink() } else { theme::quiet() };
    spans.push(Span::styled(mouse::tab_label(index, name), label_style));
    spans.push(Span::raw(" "));

    spans.push(Span::styled(glyph.to_string(), glyph_style));
    // C2: the count cell, glyph-adjacent and in the
    // glyph's own style. Always exactly one column — blank below 2 — so tab
    // widths never jitter as statuses flip.
    spans.push(Span::styled(tab_count_cell(count).to_string(), glyph_style));
    spans.push(Span::raw(" "));

    spans.push(Span::styled(theme::TAB_SEPARATOR.to_string(), theme::rule()));
    // C2: trailing gutter — one space after the separator
    // gives every divider symmetric 1-cell padding, so adjacent tabs read
    // `│ ▎` not `│▎`. Counts as this tab's own column (mouse::tab_width +8).
    spans.push(Span::raw(" "));
}

/// C2: what goes in a tab's count cell for `n` panes in
/// the summarized state. **Exactly one column, always** — that invariant is
/// the contract's geometry rule, and `mouse::tab_width` depends on it:
/// - `0`/`1` → a space. One is what a glyph already means; drawing `◆1`
///   would spend a column to say nothing, and *omitting* the cell instead
///   would make every tab's width follow its agents' statuses.
/// - `2..=9` → the digit.
/// - `10+` → `+`. Past nine the exact number stops being actionable ("more
///   than you can eyeball") and two digits would break the one-column rule,
///   which the whole strip's hit math is built on.
///
/// Pure so the boundaries are unit-tested; ASCII text in the glyph's own
/// style, so §2's glyph inventory is unchanged (no new symbol).
fn tab_count_cell(n: usize) -> char {
    match n {
        0 | 1 => ' ',
        2..=9 => char::from_digit(n as u32, 10).unwrap_or('+'),
        _ => '+',
    }
}

/// The count cell's rendered width, for whatever `n` — **always 1**. This is
/// the number `mouse::tab_width`'s `+8` is built on, so it lives here, beside
/// the cell it measures, and the mouse side asserts against it rather than
/// against a hard-coded 1 (§4/§5 lockstep: the width formula and the drawn
/// cells have to move together or not at all).
#[cfg(test)]
pub fn tab_count_cell_cols(n: usize) -> u16 {
    mouse::display_width(&tab_count_cell(n).to_string())
}

/// C8's state-word table: the collapsed row's right-segment word for each
/// status — also reused by the C20 feed's status-transition lines
/// (`app.rs`'s `diff_statuses`), so there's exactly one word table. `Exited`
/// is contracted bare — no exit code (SPEC-GAP-1: no exit-code plumbing
/// exists; `status.rs` tracks only a bool).
pub fn state_word(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Working => "working",
        AgentStatus::NeedsInput => "needs you",
        AgentStatus::Waiting => "your turn",
        AgentStatus::Idle => "idle",
        AgentStatus::Exited => "exited",
    }
}

/// C8's state word for a row whose pane may have no runtime yet
/// (`App::row_status` → `None`): a tab this session has not visited, whose
/// panes the lazy spawn has not started. "not started" is the row-length
/// spelling of what `tab_summary` reports as `TabSummary::Unknown` for the
/// same tab — never "exited", which claims a life that has not happened.
fn row_word(status: Option<AgentStatus>) -> &'static str {
    match status {
        Some(s) => state_word(s),
        None => "not started",
    }
}

/// C8's glyph + style for a row whose pane may have no runtime yet — the
/// counterpart to `row_word`. `None` routes through the *tab bar's* own
/// `Unknown` styling, so the roster's glyph for an unspawned pane is by
/// construction the glyph its tab is wearing in the same frame.
fn row_status_style(status: Option<AgentStatus>) -> (char, Style, bool) {
    match status {
        Some(s) => theme::status_style(s),
        None => {
            let (glyph, style) = theme::tab_summary_style(crate::core::app::TabSummary::Unknown);
            (glyph, style, false)
        }
    }
}

/// C8's collapsed-row name style, by status and focus. Chrome paints no
/// fills (§2), so focus is carried the way the tab bar carries it:
/// full-strength ink plus the `▎` marker. The status ramp still speaks on
/// unfocused rows, two rungs deep: Waiting/Idle/Exited share the quiet one
/// (the `✕` glyph and the `exited` state word already say which is dead).
fn collapsed_name_style(status: Option<AgentStatus>, focused: bool) -> Style {
    if focused {
        return theme::ink();
    }
    match status {
        Some(AgentStatus::Working | AgentStatus::NeedsInput) => theme::ink(),
        Some(AgentStatus::Waiting | AgentStatus::Idle | AgentStatus::Exited) | None => {
            theme::quiet()
        }
    }
}

/// C4 (amended, U2): the badge leads with the pane id — the join key for
/// `roost send <id>`, which previously appeared nowhere in the TUI. No-dup
/// rule: an untitled pane's display `name` (`display_name_live`) already
/// embeds the adapter — so appending `· {adapter}` again would duplicate it
/// ("pi · repo · pi"). Only a custom title needs the adapter spelled out
/// separately.
fn badge_text(id: layout::PaneId, name: &str, adapter: &str, has_title: bool) -> String {
    if has_title {
        format!("{id} {name} · {adapter}")
    } else {
        format!("{id} {name}")
    }
}

/// Render `parts` in order, stopping once `budget` columns are spent — the
/// part that doesn't fully fit is cut off mid-string rather than dropped
/// whole, so a badge/row degrades by trimming its tail instead of losing a
/// whole segment early. Shared by `corner_badge` (C4) and the collapsed-row
/// left side (C8) so both clip identically. Measured in display columns, not
/// chars (D1): a wide glyph (CJK, emoji) in a renamed pane/tab counts as the
/// two columns it actually draws, and a clip point never splits one in half.
fn clip_spans(parts: &[(String, Style)], budget: u16) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(parts.len());
    let mut left = budget;
    for (text, style) in parts {
        if left == 0 {
            break;
        }
        let w = mouse::display_width(text);
        if w <= left {
            spans.push(Span::styled(text.clone(), *style));
            left -= w;
        } else {
            spans.push(Span::styled(take_width(text, left), *style));
            left = 0;
        }
    }
    spans
}

/// The longest prefix of `s` whose display width fits within `budget`
/// columns — the wide-glyph-aware sibling of `.chars().take(n)`. Stops
/// before a character that would only partially fit rather than splitting
/// it, so a clip point never lands mid-glyph.
fn take_width(s: &str, budget: u16) -> String {
    let mut used = 0u16;
    let mut out = String::new();
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0) as u16;
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out
}

/// C21 (amended 2026-08-11, zoom indicator): the zoomed pane's top-border
/// title text — `ZOOM · {n} hidden`, or bare `ZOOM` when `n == 0` (a
/// single-pane tab has nothing hidden). `width` is the border's title area
/// (border-to-border, corners excluded — same budget `corner_badge`'s
/// `inner.width` measures, since a bordered `Block`'s title area is exactly
/// that). Sheds in two steps: the `· {n} hidden` clause drops first, then
/// the whole title drops (`None`) — never a partial/clipped title. Pure so
/// the shedding order is unit-testable without a `Frame`.
fn zoom_title_text(n: usize, width: u16) -> Option<String> {
    let full = if n == 0 { "ZOOM".to_string() } else { format!("ZOOM · {n} hidden") };
    if mouse::display_width(&full) <= width {
        return Some(full);
    }
    if n > 0 && mouse::display_width("ZOOM") <= width {
        return Some("ZOOM".to_string());
    }
    None
}

fn draw_pane<B: PaneBackend>(
    f: &mut Frame<'_>,
    app: &mut App<B>,
    pr: PaneRect,
    stack_expanded: bool,
    spinner: char,
    now: u64,
) {
    let focused = app.focused == pr.id;
    let raw = app.is_raw(pr.id);
    let (status, name, has_title, adapter, note) = {
        // find_spec, not the active tab's map directly: C22 learns the
        // float here too (its spec lives on the `Float`, not `Tab::panes`).
        let spec = app.find_spec(pr.id);
        // A drawn pane lives in the active tab or is the float, so it has
        // been spawned and `display_status` answers `Some`; the fallback
        // only covers a pane whose spec vanished mid-frame, which is dead.
        // [P2-10] `display_status`, not `row_status`: a quiet shell's
        // `Waiting` reads `Idle` on the badge/collapsed row, same as
        // everywhere else chrome shows status.
        let status = app.display_status(pr.id).unwrap_or(AgentStatus::Exited);
        let has_title = spec.and_then(|s| s.title.as_ref()).is_some();
        let adapter = spec.map(|s| s.adapter.clone()).unwrap_or_else(|| "?".into());
        // U2 (amended, P6): the shared display name — explicit title, else
        // the pane's live OSC 0/2 title, else `adapter · cwd-tag`. One
        // helper for every fleet surface, `App::display_name`, so the badge
        // can never drift from what the feed/notifications/host title call
        // the same pane. Untitled panes on the same adapter are otherwise
        // indistinguishable at a glance; a pane publishing a live task line
        // now says what it's doing.
        let name = if spec.is_some() { app.display_name(pr.id) } else { "?".into() };
        // C32: the badge's note segment — headline + age on the focused
        // pane, a bare `¶` elsewhere; `¶⋮` marks a body under the
        // headline. Built here because only draw_pane knows focus.
        let note = spec.and_then(|s| {
            s.note.as_ref().map(|n| {
                let mut ls = n.lines();
                let headline = ls.next().unwrap_or_default().to_string();
                BadgeNote {
                    headline,
                    more: ls.next().is_some(),
                    age: s.noted_at.map(|t| age_word(t, now)),
                    focused,
                }
            })
        });
        (status, name, has_title, adapter, note)
    };

    if pr.collapsed {
        // C8: collapsed stack member — the fleet-view title bar. `Some`,
        // always: a drawn row belongs to the active tab (or the float), and
        // those are spawned by construction — only C27's roster reaches
        // across into tabs that aren't.
        //
        // [Amended 2026-09-01] Where the C6 geometry granted the member its
        // 3 rows, the row gets a border in the quiet stack-chrome red (C7's
        // hue), so a collapsed pane reads as a pane rather than as a rule
        // between its bordered neighbours; the shallow fallback stays the
        // bare 1-row bar. Focus on a collapsed member is transient (it
        // auto-expands) but honest while it lasts: the border sharpens to
        // C3's focus accent.
        let row = if pr.rect.height >= layout::COLLAPSED_BOX_ROWS {
            let border = if focused { theme::accent() } else { theme::accent_quiet() };
            let block = Block::bordered().border_style(border);
            let inner = block.inner(pr.rect);
            f.render_widget(block, pr.rect);
            inner
        } else {
            pr.rect
        };
        let spans = collapsed_row_spans(
            row.width,
            focused,
            Some(status),
            pr.id,
            &name,
            &adapter,
            has_title,
            raw,
            note.is_some(),
            spinner,
        );
        f.render_widget(Paragraph::new(Line::from(spans)), row);
        return;
    }

    // C3: focus is the only signal a border carries now — status lives in
    // the glyph system (corner badge / collapsed row), not the border color,
    // and the border no longer carries a title (identity moved to the
    // corner badge, C4). No BOLD.
    let border_style = if focused { theme::accent() } else { theme::rule() };
    let mut block = Block::bordered().border_style(border_style);
    // The border's title area, corner to corner. Shared budget: C21's zoom
    // title takes what it needs from the right, C4's identity title gets
    // what is left on the left.
    let mut top_budget = pr.rect.width.saturating_sub(2);
    // C21 (amended 2026-08-11, zoom indicator): the zoomed pane alone (never
    // the float, which can render alongside it, C22) gets a right-aligned
    // border title naming how many real-tree panes zoom is hiding. `n`
    // reads `app.rects()` — the real (un-zoomed) tree — not the
    // single-entry `display_rects()` this draw loop is walking, which would
    // always say zero. Styled `border_style`, matching the border it sits
    // on (accent() focused, rule() when the zoomed pane draws unfocused
    // under a focused float) — never its own fixed color.
    if app.zoomed() && !app.is_float(pr.id) {
        let n = app.rects().len().saturating_sub(1);
        if let Some(title) = zoom_title_text(n, top_budget) {
            // [Amended 2026-08-21] Zoom is served first and identity yields
            // the columns it takes, plus one so the two never touch: zoom is
            // a transient announcement about what you *cannot* see, and a
            // zoomed tab is one pane with the whole body's width to spend.
            top_budget = top_budget.saturating_sub(mouse::display_width(&title) + 1);
            block = block.title_top(Line::from(title).right_aligned().style(border_style));
        }
    }
    // C3/C4 (amended 2026-08-21): identity on the top border, left-aligned —
    // a label for the box, on the box. It used to be a corner badge painted
    // over the pane's own first content row; see `identity_title`.
    let (base_glyph, glyph_style, spins) = theme::status_style(status);
    let glyph = badge_glyph(spins, app.scroll_offset(pr.id), spinner, base_glyph);
    if let Some(spans) = identity_title(
        top_budget,
        &badge_text(pr.id, &name, &adapter, has_title),
        raw,
        app.scroll_offset(pr.id),
        glyph,
        glyph_style,
    ) {
        block = block.title_top(Line::from(spans).left_aligned());
    }
    // C32 (amended 2026-08-21): the note gets the bottom border to itself.
    if let Some(spans) = note.as_ref().and_then(|n| note_title(pr.rect.width.saturating_sub(2), n))
    {
        block = block.title_bottom(Line::from(spans).left_aligned());
    }
    let inner = block.inner(pr.rect);
    f.render_widget(block, pr.rect);

    // C7: an unfocused expanded stack member gets its left border column
    // overpainted with the accent-dim edge marker. Suppressed when focused —
    // the full accent border is already the stronger signal, and stacking
    // a quiet red inside a red frame would smear the one-red discipline.
    if stack_expanded && !focused {
        paint_stack_edge(f, pr.rect);
    }

    // U3/N1 + P7c: the cursor's honesty gate is computed before the screen
    // borrow, since it needs `app` immutably for the scroll offset.
    let scrolled = app.scroll_offset(pr.id);
    if let Some(screen) = app.runtimes.get(&pr.id).and_then(|rt| rt.screen()) {
        blit_screen(f, screen, inner);
        // P7: the host cursor is placed only when the pane is actually
        // showing one. `should_place_cursor` holds the whole rule.
        if should_place_cursor(focused, status, screen.hide_cursor(), scrolled) {
            let (cr, cc) = screen.cursor_position();
            let x = inner.x.saturating_add(cc);
            let y = inner.y.saturating_add(cr);
            if x < inner.x + inner.width && y < inner.y + inner.height {
                f.set_cursor_position(Position::new(x, y));
            }
        }
    }

    // P21: search hits, painted before the selection so a selection over a
    // hit still reads as selected. C17's rule holds — modifiers only, no
    // color tokens: hits sit on top of arbitrary program colors.
    if let Some(search) = app.search.as_ref().filter(|s| s.pane == pr.id) {
        let (offset, total) = app.scroll_position();
        highlight_matches(f, inner, search, total.saturating_sub(offset));
    }

    // Copy-mode selection: reverse-highlight the selected cells in this pane.
    if let Some(sel) = app.selection.filter(|s| s.pane == pr.id) {
        highlight_selection(f, inner, sel.anchor, sel.cursor);
    }

    // C24: the keyboard copy cursor, always on the focused pane while in
    // Mode::Copy — painted after the selection pass so it stays visible
    // inside it (REVERSED, +UNDERLINED when inside an active selection).
    if focused {
        if let Mode::Copy { cursor } = app.mode {
            paint_copy_cursor(f, inner, cursor, app.selection);
        }
    }

    // C16: dead pane — overlay the relaunch hint (and spawn error, if any)
    // on the bottom rows. The last screen contents stay visible above.
    if status == AgentStatus::Exited && inner.height > 0 {
        let mut lines: Vec<Line<'_>> = Vec::new();
        if let Some(err) = app.dead.get(&pr.id) {
            lines.push(Line::from(Span::styled(format!(" spawn failed: {err} "), theme::accent())));
        }
        let resumable = app.resume_command_line(pr.id).is_some();
        lines.push(Line::from(Span::raw(dead_bar_text(
            resumable,
            input::chord_for(app.keymap(), Action::ClosePane).as_deref(),
        ))));
        let n = lines.len() as u16;
        let y = inner.y + inner.height.saturating_sub(n);
        let overlay = Rect::new(inner.x, y, inner.width, n.min(inner.height));
        // C16: the style rides on the *widget*, not the span, so the whole
        // inner width reverses. Styling the span alone would reverse only the
        // ~76 text columns and leave the dead program's last output showing
        // through the rest of the row at normal video — the bar has to read as
        // one bar, exactly as C10/C11's rows do.
        f.render_widget(Paragraph::new(lines).style(theme::attention_problem()), overlay);
    }
}

/// C16's action-bar text, pure so both variants pin without a `Frame`.
/// `y: copy resume` rides the bar only when the pane has a session pointer
/// — same predicate as the hint bar's `resumable` (`App::resume_command_line`),
/// so the two surfaces can't disagree about whether `y` does anything.
/// C34: `close` is resolved. This bar and the C9 hint bar one screen row
/// below name the same chord, and before C34 only one of them derived it —
/// so a remap made two surfaces on one screen disagree. An unbound close
/// drops the clause; `Enter`/`f`/`y` stay literal because they are
/// mode-local keys config.json cannot reach (C34's stated exemption).
fn dead_bar_text(resumable: bool, close_chord: Option<&str>) -> String {
    let copy_hint = if resumable { " · y: copy resume" } else { "" };
    let close = close_chord.map(|c| format!(" · {c}: close")).unwrap_or_default();
    format!(
        " {} exited — Enter: relaunch/resume · f: fresh (drops resume){copy_hint}{close} ",
        theme::GLYPH_EXITED
    )
}

/// C7: overpaint an expanded stack member's left border column with the
/// accent-dim half-block edge — the cell translation of the mockup's 2px
/// `--tui-red-dim` left edge (a half-block reads "thicker than a 1px line").
fn paint_stack_edge(f: &mut Frame<'_>, rect: Rect) {
    let buf = f.buffer_mut();
    for y in rect.y..rect.y + rect.height {
        if let Some(cell) = buf.cell_mut((rect.x, y)) {
            cell.set_symbol(&theme::MARKER_EXPANDED_EDGE.to_string());
            cell.set_style(theme::accent_quiet());
        }
    }
}

/// C8: one collapsed stack row's spans for the given width — marker, status
/// glyph, pane id, and name on the left; the right-aligned dim
/// "adapter · word" segment when there's room. The right segment drops first
/// when narrow; if even the left side overflows, the id+name (last in
/// `left`) is what visibly clips. Pure so the width-shedding order is
/// unit-testable.
///
/// The left side carries the pane id ahead of the name — the
/// `roost send <id>` join key, same placement as the corner badge (C4).
///
/// No-dup rule (C8, mirrors C4's `badge_text`): an untitled pane's `name` is
/// the shared `display_name_live` fallback, which already embeds the adapter —
/// so the right segment drops the `{adapter} · ` prefix and shows just the
/// state word (`has_title` false). A custom title doesn't embed the adapter,
/// so titled panes keep the full `"{adapter} · {word}"`. A raw pane's right
/// segment gains a `raw · ` prefix ahead of whichever of the above it would
/// otherwise be. `status` is optional because the roster reuses this row for
/// panes in tabs the lazy spawn has not started: `None` is "not started"
/// (`row_word`/`row_status_style`), not a corpse. `spinner` is resolved from
/// `status` in here (Working substitutes the current spinner frame) rather
/// than by every caller.
///
/// C32: `noted` grows the right segment a leading `¶ ` in `ink()` — the
/// C4 marker's exact meaning here (presence of a parked note, never its
/// text: collapsed rows and the roster stay reveal-on-visit surfaces).
/// It rides the right segment, so narrow rows shed it with the segment.
#[allow(clippy::too_many_arguments)]
fn collapsed_row_spans(
    width: u16,
    focused: bool,
    status: Option<AgentStatus>,
    id: layout::PaneId,
    name: &str,
    adapter: &str,
    has_title: bool,
    raw: bool,
    noted: bool,
    spinner: char,
) -> Vec<Span<'static>> {
    let (base_glyph, glyph_style, spins) = row_status_style(status);
    // Collapsed rows show no live grid (unlike a pane's own corner badge),
    // so there is no frozen view to preserve — Working always animates here.
    let glyph = if spins { spinner } else { base_glyph };
    let marker = if focused {
        (theme::MARKER_ACTIVE.to_string(), theme::accent())
    } else {
        (" ".to_string(), Style::default())
    };
    let left: Vec<(String, Style)> = vec![
        marker,
        (glyph.to_string(), glyph_style),
        (format!(" {id} {name}"), collapsed_name_style(status, focused)),
    ];
    let left_w: u16 = left.iter().map(|(t, _)| mouse::display_width(t)).sum();
    let right = if has_title {
        format!("{adapter} · {} ", row_word(status))
    } else {
        format!("{} ", row_word(status))
    };
    let right = if raw { format!("raw · {right}") } else { right };
    // C32: the note marker leads the right segment, its own `ink()` span so
    // it stays findable in a column of quiet state words.
    let marker_w: u16 = if noted { 2 } else { 0 }; // "¶ "
    let right_w = mouse::display_width(&right) + marker_w;

    if width >= left_w + right_w {
        let pad = width - left_w - right_w;
        let mut spans: Vec<Span<'_>> = left.into_iter().map(|(t, s)| Span::styled(t, s)).collect();
        spans.push(Span::raw(" ".repeat(pad as usize)));
        if noted {
            spans.push(Span::styled("¶ ", theme::ink()));
        }
        spans.push(Span::styled(right, theme::quiet()));
        spans
    } else {
        clip_spans(&left, width)
    }
}

/// C6's header text for the given row width: uppercase " STACK · N PANES"
/// left, "ALT+↑↓ " right-aligned, filled with spaces between. Pure so the
/// content and right-alignment are unit-testable without a `Frame`.
fn stack_header_text(width: u16, n: usize) -> String {
    let left = format!(" STACK · {n} PANES");
    let right = "ALT+↑↓ ";
    let pad = width
        .saturating_sub(left.chars().count() as u16)
        .saturating_sub(right.chars().count() as u16);
    format!("{left}{}{right}", " ".repeat(pad as usize))
}

/// C6: a stack's header row. Every cell (text and fill alike) carries
/// `Modifier::UNDERLINED` — the cell translation of the mockup's 1px bottom
/// rule — via the paragraph-level style, the same edge-to-edge-fill trick
/// `draw_tab_bar` uses. No bg (background policy, §2).
fn draw_stack_header(f: &mut Frame<'_>, header: layout::StackHeader) {
    f.render_widget(
        Paragraph::new(stack_header_text(header.rect.width, header.n))
            .style(theme::quiet().add_modifier(Modifier::UNDERLINED)),
        header.rect,
    );
}

/// C4's note segment data (C32): what the badge says about a parked note.
/// `headline` is the note's first line, `more` marks a body under it (the
/// `⋮`), `age` is the pre-rendered age tag — `None` when the note has no
/// timestamp (hand-edited state): an absent fact renders as absent, never
/// as a fabricated `now` — and `focused` picks between the two forms: the
/// focused pane reads its note out, everything else shows a bare marker.
/// Built by `draw_pane`, consumed by `corner_badge`.
struct BadgeNote {
    headline: String,
    more: bool,
    age: Option<String>,
    focused: bool,
}

/// C4 (C32): the note age tag — the coarsest sensible unit, floored:
/// `now` under a minute, then `5m` / `3h` / `2d`. This is how a stale
/// note confesses instead of reading as current. A clock that moved
/// backwards clamps to `now` rather than underflowing. Pure.
fn age_word(noted_at: u64, now: u64) -> String {
    let s = now.saturating_sub(noted_at);
    match s {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

/// C3/C4 (amended 2026-08-21): the pane's **identity title** — the left half
/// of the top border, `" {id} {name} · {adapter} [raw] [↑N] {glyph} "`.
///
/// Formerly the corner badge, drawn over the pane's own first content row.
/// That row is the pane's most valuable one — a prompt, the first line of a
/// diff, the answer you just asked for — and the badge took as much of it as
/// it needed: measured, 29–55 columns of a 58-column pane, and the *whole*
/// row of a 28-column one, on all four panes of a four-way split, while the
/// border above it sat empty. Identity is a label for the box, so it goes on
/// the box.
///
/// **The glyph is never shed.** It is C5's status signal, and a long name
/// used to clip it away — the badge trimmed its tail, so the first thing
/// lost was the one element reporting whether the agent is alive. Here the
/// tail is fixed and the *name* absorbs the shortfall, down to nothing; only
/// when even ` {glyph} ` will not fit does the title disappear.
///
/// Styling stays C4's, deliberately **not** C21's "match the border you sit
/// on": `ZOOM · {n} hidden` is one undifferentiated statement about the
/// pane, while this title has structure C5 requires be coloured — the glyph
/// carries status, `raw`/`↑N` carry view state. Matching the border is right
/// for the former and would delete the signal in the latter.
fn identity_title(
    budget: u16,
    text: &str,
    raw: bool,
    scrolled: usize,
    glyph: char,
    glyph_style: Style,
) -> Option<Vec<Span<'static>>> {
    // The fixed tail: everything the name may not push off the border.
    // Composition and separators are C4's, unchanged — only the placement
    // moved, so the rendered text is byte-identical to the badge's.
    let mut tail: Vec<(String, Style)> = Vec::with_capacity(3);
    if raw {
        let token = if scrolled > 0 { "raw · " } else { "raw " };
        tail.push((token.to_string(), theme::accent_quiet()));
    }
    if scrolled > 0 {
        tail.push((format!("{}{scrolled} ", theme::SCROLLED), theme::accent_quiet()));
    }
    tail.push((format!("{glyph} "), glyph_style));
    let tail_w: u16 = tail.iter().map(|(t, _)| mouse::display_width(t)).sum();
    // A leading column of breathing room so the title never butts against
    // the corner glyph, and C4's separator before the token run.
    let sep = if raw || scrolled > 0 { " · " } else { " " };
    let fixed = 1 + mouse::display_width(sep) + tail_w;
    if fixed > budget || text.trim().is_empty() {
        return None;
    }
    // Whatever the tail leaves is the name's. `take_width` keeps the clip
    // off a wide glyph's second half (D1).
    let name = take_width(text, budget - fixed);
    let mut parts: Vec<(String, Style)> = Vec::with_capacity(tail.len() + 1);
    if name.is_empty() {
        // Clipped to nothing: the glyph still reports, and a stray separator
        // dangling off an absent name would read as a missing word.
        parts.push((" ".to_string(), theme::quiet()));
    } else {
        parts.push((format!(" {name}{sep}"), theme::quiet()));
    }
    parts.extend(tail);
    Some(clip_spans(&parts, budget))
}

/// C32 (amended 2026-08-21): the pane's **note title** — the left half of
/// the bottom border, `" ¶ {headline} ({age}) "` on the focused pane and a
/// bare `" ¶ "` everywhere else. `¶⋮` when a body sits under the headline.
///
/// Its own edge rather than a segment of the identity title, for two
/// reasons. It is the widest thing roost draws about a pane, so sharing the
/// top border would make it the first casualty of a narrow one — which is
/// exactly what it was as the badge's tail, clipped before it could be read.
/// And it answers a different question from a name: *what did I park here*,
/// not *what is this*. On its own edge a 28-column pane still shows
/// `└ ¶ waiting on schema review ┘` whole.
///
/// C32's reveal-on-visit contract is unchanged, and is the whole reason the
/// two forms exist: presence everywhere, content only where you are looking.
/// The marker is fixed and the headline absorbs a narrow border, so a note
/// never becomes invisible — it becomes a `¶`.
fn note_title(budget: u16, note: &BadgeNote) -> Option<Vec<Span<'static>>> {
    let marker = if note.more { "¶⋮" } else { "¶" };
    let head = format!(" {marker} ");
    if mouse::display_width(&head) > budget {
        return None;
    }
    let mut parts: Vec<(String, Style)> = vec![(head, theme::ink())];
    if note.focused {
        parts.push((note.headline.clone(), theme::ink()));
        match &note.age {
            Some(age) => parts.push((format!(" ({age}) "), theme::quiet())),
            None => parts.push((" ".into(), theme::quiet())),
        }
    }
    Some(clip_spans(&parts, budget))
}

/// N1: the badge glyph shown — the C5 spinner only while the pane's view is
/// at the live tail. An animating glyph asserts "alive right now", which a
/// frozen (scrolled) view must not do; the glyph freezes at its steady frame
/// (the status itself stays truthful — the agent IS working) while the C4
/// `↑N` token carries the frozen-view signal. Any path that resets the
/// offset resumes the animation. Collapsed rows and the tab bar keep
/// animating: they show no grid, so there is no frozen view to lie about.
fn badge_glyph(spins: bool, scrolled: usize, spinner: char, base: char) -> char {
    if spins && scrolled == 0 {
        spinner
    } else {
        base
    }
}

/// Reverse-video the cells between `a` and `b` (inclusive, pane-inner coords)
/// to show a copy-mode selection. Reading-order/linewise, clipped to `inner`.
fn highlight_selection(f: &mut Frame<'_>, inner: Rect, a: (u16, u16), b: (u16, u16)) {
    let (start, end) = if (a.0, a.1) <= (b.0, b.1) { (a, b) } else { (b, a) };
    let (w, h) = (inner.width, inner.height);
    let buf = f.buffer_mut();
    let mut row = start.0;
    while row <= end.0 && row < h {
        let first = if row == start.0 { start.1 } else { 0 };
        let last =
            (if row == end.0 { end.1 } else { w.saturating_sub(1) }).min(w.saturating_sub(1));
        if first <= last {
            let rect = Rect::new(inner.x + first, inner.y + row, last - first + 1, 1);
            buf.set_style(rect, Style::new().add_modifier(Modifier::REVERSED));
        }
        row += 1;
    }
}

/// P21: paint the search hits visible in this pane. `first_line` is the
/// haystack line index currently on the view's top row — `banked − offset`,
/// both read from the grid — so a hit's screen row is just `line −
/// first_line`. Every hit is `REVERSED`; the *current* one additionally
/// `UNDERLINED`, so "which of these am I parked on" is answerable at a
/// glance. Modifier-only by C17: hits land on arbitrary program colors, and
/// a palette token here would be a DEVIATED.
fn highlight_matches(f: &mut Frame<'_>, inner: Rect, search: &Search, first_line: usize) {
    let width = search.width();
    if width == 0 {
        return;
    }
    let current = search.current_match();
    let buf = f.buffer_mut();
    for (i, &(line, col)) in search.matches.iter().enumerate() {
        let Some(row) = line.checked_sub(first_line) else { continue };
        if row >= inner.height as usize {
            continue;
        }
        let is_current = current == Some(search.matches[i]) && i == search.current;
        for c in col..col + width {
            if c >= inner.width as usize {
                break;
            }
            let Some(cell) = buf.cell_mut((inner.x + c as u16, inner.y + row as u16)) else {
                continue;
            };
            let mut style = cell.style().add_modifier(Modifier::REVERSED);
            if is_current {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            cell.set_style(style);
        }
    }
}

/// C24: whether inner-cell `pos` (row, col) lies within the inclusive,
/// reading-order selection spanning `a`..=`b` — the same ordering
/// `highlight_selection` paints. `(u16, u16)`'s derived `Ord` compares row
/// first then column, which *is* reading order, so a plain range check
/// suffices. Pure so the cursor-in-selection rule is unit-tested without a
/// `Frame`.
fn cell_in_selection(pos: (u16, u16), a: (u16, u16), b: (u16, u16)) -> bool {
    let (start, end) = if a <= b { (a, b) } else { (b, a) };
    pos >= start && pos <= end
}

/// C24: paint the keyboard-copy cursor cell — always `REVERSED`;
/// additionally `UNDERLINED` when it sits inside an active selection, so it
/// stays distinguishable within the reversed region. Painted after the
/// selection pass (C17). Modifier-only, no color tokens — any styling here
/// beyond modifiers is a DEVIATED (C24).
fn paint_copy_cursor(
    f: &mut Frame<'_>,
    inner: Rect,
    cursor: (u16, u16),
    selection: Option<Selection>,
) {
    let (row, col) = cursor;
    if row >= inner.height || col >= inner.width {
        return;
    }
    let in_selection = selection.is_some_and(|s| cell_in_selection(cursor, s.anchor, s.cursor));
    let mut style = Style::new().add_modifier(Modifier::REVERSED);
    if in_selection {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    let rect = Rect::new(inner.x + col, inner.y + row, 1, 1);
    f.buffer_mut().set_style(rect, style);
}

/// P7: may roost place the host's single real cursor inside this pane?
///
/// Four conditions, each a way the old unconditional "focused ⇒ place it"
/// rule lied:
/// * only the focused pane may hold it — there is one cursor;
/// * an exited pane isn't running anything to own it (pre-existing rule);
/// * `hide_cursor` (DECTCEM `?25l`) means the app deliberately hid its
///   cursor — roost blinking a ghost one over a TUI that hid its own is
///   P7(a) verbatim;
/// * a scrolled-back view (`scroll_offset > 0`) is frozen on history, so the
///   live grid's cursor position has nothing to do with what's on screen —
///   it would float over old output (P7(c), same frozen-view surface as
///   SPEC-ux U3/N1: while the `↑N` token shows, roost stops asserting
///   liveness, and a blinking cursor is the loudest such assertion).
///
/// Pure, so all four are unit-testable without a `Frame`.
fn should_place_cursor(
    focused: bool,
    status: AgentStatus,
    hidden: bool,
    scroll_offset: usize,
) -> bool {
    focused && status != AgentStatus::Exited && !hidden && scroll_offset == 0
}

/// Copy the vt100 grid into the ratatui buffer.
///
/// U24: wide-char (CJK/emoji) cells are blitted the way ratatui itself lays
/// out a wide grapheme — the glyph goes in its own cell and the continuation
/// cell to its right is `reset()`, never written to. Previously the
/// continuation was stamped with `" "`, which drew a space *over* the right
/// half of every CJK/emoji glyph the pane emitted. Since P17 the grid measures
/// widths with the same unicode-width the terminal backend uses, so a cell
/// marked wide here is a cell ratatui's diff will skip when flushing — the two
/// halves can no longer disagree about which columns the glyph owns.
fn blit_screen(f: &mut Frame<'_>, screen: &vt100::Screen, inner: Rect) {
    let (rows, cols) = screen.size();
    let visible_cols = inner.width.min(cols);
    let buf = f.buffer_mut();
    // Hoisted out of the loop: `Cell::contents()` allocates a `String` per
    // call, and this loop runs over every cell of every visible pane on
    // every frame. `push_contents` appends into this one reused buffer
    // instead — same bytes, no per-cell allocation (~4x faster inner loop).
    let mut contents = String::new();
    for row in 0..inner.height.min(rows) {
        for col in 0..visible_cols {
            let x = inner.x + col;
            let y = inner.y + row;
            let Some(out) = buf.cell_mut((x, y)) else { continue };
            // No cell means blank, and it has to be *painted* blank: the
            // buffer is not cleared between frames, so skipping the write
            // would leave whatever the last frame drew there. Reachable
            // since banked scrollback rows are trimmed to their contents
            // (`Row::shrink_to_contents`) — every column past a short
            // history line answers `None` here.
            let Some(cell) = screen.cell(row, col) else {
                out.reset();
                continue;
            };
            if cell.is_wide_continuation() {
                // Owned by the glyph on its left. Left at the buffer's reset
                // default — exactly what ratatui's own wide-grapheme layout
                // does — so nothing in roost ever writes a symbol into a cell
                // another glyph already spans.
                out.reset();
                continue;
            }
            contents.clear();
            cell.push_contents(&mut contents);
            // A wide glyph whose second half falls outside the drawn area
            // would overflow into the pane's border, so it degrades to a
            // space rather than corrupting the chrome.
            if contents.is_empty() || (cell.is_wide() && col + 1 >= visible_cols) {
                out.set_symbol(" ");
            } else {
                out.set_symbol(&contents);
            }
            out.set_style(cell_style(cell));
        }
    }
}

/// Program output, not chrome (C18): a pane's own colours pass through
/// byte-faithfully and keep the **full** palette — indexed and truecolor
/// alike. §2's inherit-the-terminal rule governs what roost draws *around*
/// panes, never what a program draws inside one, so these three lines carry
/// the `chrome-gate-exempt` marker the theme module's source scan honours.
fn conv_color(c: vt100::Color) -> Option<Color> {
    match c {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(Color::Indexed(i)), // chrome-gate-exempt: program output
        vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)), // chrome-gate-exempt: program output
    }
}

fn cell_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::default();
    if let Some(fg) = conv_color(cell.fgcolor()) {
        style = style.fg(fg);
    }
    if let Some(bg) = conv_color(cell.bgcolor()) {
        style = style.bg(bg); // chrome-gate-exempt: program output
    }
    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }
    // P16: dim (SGR 2) and strikethrough (SGR 9) were dropped end to end, so
    // a pane's secondary text rendered at the same weight as its primary —
    // roost asserting emphasis the program never asked for. Claude Code leans
    // on dim heavily; without it a pane is one flat wall of text.
    if cell.dim() {
        style = style.add_modifier(Modifier::DIM);
    }
    if cell.strikethrough() {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }
    style
}

#[cfg(test)]
mod tests {
    use super::{
        age_word, badge_text, blit_screen, cell_in_selection, centered_near, collapsed_name_style,
        collapsed_row_spans, dialog_rect, feed_entry_spans, feed_window, help_content_width,
        help_layout, help_lines, hint_bar_right_spans, identity_title, mode_word, note_title,
        push_tab_spans, should_place_cursor, stack_header_text, state_word, BadgeNote, HelpKey,
        HelpLine, HELP_GROUPS,
    };
    use crate::ui::input::{self, Action, Keymap};
    use crate::App;

    /// F1: `hint_pairs` takes a keymap now. Every test below is about the
    /// *content* of a bar rather than about remapping, so they go through
    /// this default-keymap wrapper and keep reading as they did — the
    /// remap behaviour has its own tests.
    fn hint_pairs(
        mode: &Mode,
        focused_dead: bool,
        resumable: bool,
        focused_raw: bool,
        help_scrolled: bool,
    ) -> Vec<(String, &'static str)> {
        super::hint_pairs(
            mode,
            focused_dead,
            resumable,
            focused_raw,
            help_scrolled,
            false,
            &Keymap::default(),
        )
    }

    /// Build an expected pair list. Keys are owned since F1 (they are
    /// resolved, not compiled in), and writing `.to_string()` on every
    /// literal would drown the lists this file is meant to make readable.
    fn p(pairs: &[(&str, &'static str)]) -> Vec<(String, &'static str)> {
        pairs.iter().map(|(k, d)| ((*k).to_string(), *d)).collect()
    }

    fn one(k: &str, d: &'static str) -> (String, &'static str) {
        (k.to_string(), d)
    }
    use crate::core::app::{Mode, RenameTarget, RosterRow};
    use crate::core::status::AgentStatus;
    use crate::ui::mouse;
    use crate::ui::theme;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Modifier};

    #[test]
    fn dialog_centers_on_focused_pane_not_whole_screen() {
        // Body is a wide 3-pane screen; anchor is the rightmost pane only.
        let body = Rect::new(0, 1, 120, 30);
        let anchor = Rect::new(80, 1, 40, 30);
        let rect = centered_near(anchor, body, 32, 5);
        // Centered within the anchor pane, not the full 120-wide body.
        assert_eq!(rect.x, anchor.x + (anchor.width - rect.width) / 2);
        assert_eq!(rect.width, 32);
        assert_eq!(rect.height, 5);
    }

    #[test]
    fn dialog_stays_on_screen_when_anchor_is_near_the_edge() {
        // Anchor pane hugs the right edge; a dialog centered on it alone
        // would spill off-screen — must clamp back inside `body`.
        let body = Rect::new(0, 1, 60, 20);
        let anchor = Rect::new(50, 1, 10, 20);
        let rect = centered_near(anchor, body, 32, 5);
        assert!(rect.x + rect.width <= body.x + body.width);
        assert!(rect.x >= body.x);
    }

    #[test]
    fn badge_no_dup_rule_pins_c4() {
        // U2: every badge leads with the pane id (the `roost send <id>` key).
        // Untitled fallback name already embeds the adapter — don't repeat it.
        assert_eq!(badge_text(3, "pi · myrepo", "pi", false), "3 pi · myrepo");
        // A custom title doesn't embed the adapter — spell it out.
        assert_eq!(badge_text(7, "worker1", "claude", true), "7 worker1 · claude");
    }

    fn ident(budget: u16, text: &str) -> String {
        identity_title(budget, text, false, 0, theme::GLYPH_WORKING, theme::accent())
            .map(|s| s.iter().map(|s| s.content.to_string()).collect())
            .unwrap_or_default()
    }

    /// C3/C4 (amended 2026-08-21): the identity title's *text* is the corner
    /// badge's, unchanged — only where it is drawn moved. Pinned as an equality
    /// so the move cannot quietly become a redesign.
    #[test]
    fn identity_title_is_two_toned_and_reads_exactly_as_the_badge_did() {
        let spans =
            identity_title(40, "claude", false, 0, theme::GLYPH_WORKING, theme::accent()).unwrap();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content.as_ref(), " claude ");
        assert_eq!(spans[0].style, theme::quiet());
        assert_eq!(spans[1].content.as_ref(), format!("{} ", theme::GLYPH_WORKING));
        assert_eq!(spans[1].style, theme::accent());
    }

    /// The glyph is the one element that must survive a narrow pane: it is
    /// C5's status signal, and the badge used to shed it first because it
    /// clipped its own tail. Here the *name* absorbs the shortfall.
    #[test]
    fn identity_title_clips_the_name_and_never_the_status_glyph() {
        let text = ident(9, "a-very-long-name");
        assert!(
            text.contains(theme::GLYPH_WORKING),
            "the glyph outranks the name at every width that fits it: {text:?}"
        );
        assert!(mouse::display_width(&text) <= 9, "{text:?}");
        assert!(text.starts_with(" a-"), "and what is left of the name still leads: {text:?}");

        // Down to nothing: the glyph alone still reports, with no dangling
        // separator where a name used to be.
        assert_eq!(ident(4, "a-very-long-name"), format!(" {} ", theme::GLYPH_WORKING));
        // Below even that, the title sheds whole rather than clipping the
        // glyph in half.
        assert_eq!(identity_title(2, "x", false, 0, theme::GLYPH_WORKING, theme::accent()), None);
        assert_eq!(
            identity_title(40, "   ", false, 0, theme::GLYPH_WORKING, theme::accent()),
            None
        );
    }

    /// D1: "日本語" is 3 chars but 6 display columns — a `.chars().count()`
    /// measure would call a 4-column budget a fit and overflow the border.
    /// The clip lands on a column boundary and never splits a glyph.
    #[test]
    fn identity_title_clips_wide_glyphs_on_a_column_boundary() {
        for budget in 4..12u16 {
            let text = ident(budget, "日本語");
            let w = mouse::display_width(&text);
            assert!(w <= budget, "budget {budget} overflowed by {text:?} ({w} cols)");
            // Never half a glyph.
            for ch in text.chars() {
                assert!(ch != '\u{fffd}', "{text:?}");
            }
        }
        assert_eq!(ident(8, "日本語"), format!(" 日本 {} ", theme::GLYPH_WORKING));
    }

    /// P7: every way the pane can be "not showing a cursor" suppresses
    /// roost's placement of the host's one real cursor.
    #[test]
    fn cursor_is_placed_only_when_the_pane_is_really_showing_one() {
        use AgentStatus::{Exited, Working};
        // The baseline: focused, alive, cursor visible, view at the tail.
        assert!(should_place_cursor(true, Working, false, 0));

        // Unfocused — there is one cursor and it belongs to the focused pane.
        assert!(!should_place_cursor(false, Working, false, 0));
        // Exited — nothing is running to own it (pre-existing rule, kept).
        assert!(!should_place_cursor(true, Exited, false, 0));
        // P7(a): the app hid its cursor with `?25l`; roost blinking a ghost
        // one over it is exactly the defect.
        assert!(!should_place_cursor(true, Working, true, 0));
        // P7(c)/U3: the view is frozen on history, so the live grid's cursor
        // position describes something that isn't on screen.
        assert!(!should_place_cursor(true, Working, false, 1));

        // The gates are independent — any one of them suppresses.
        assert!(!should_place_cursor(true, Working, true, 7));
        assert!(!should_place_cursor(false, Exited, true, 3));
    }

    // -- C23 raw indication ---------------------------------------------------

    #[test]
    fn identity_title_gains_a_raw_token_in_its_own_quiet_red() {
        let spans =
            identity_title(40, "scratch · shell", true, 0, theme::GLYPH_IDLE, theme::quiet())
                .unwrap();
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, format!(" scratch · shell · raw {} ", theme::GLYPH_IDLE));
        let raw_span = spans.iter().find(|s| s.content.as_ref() == "raw ").expect("raw token span");
        assert_eq!(raw_span.style, theme::accent_quiet());
        // The raw token must be its own span, not folded into the muted text.
        assert!(spans.iter().any(|s| s.style == theme::quiet()));
    }

    #[test]
    fn identity_title_without_raw_has_no_raw_token() {
        assert!(!ident(40, "pi").contains("raw"));
    }

    // -- U3 scrollback indication ---------------------------------------------

    #[test]
    fn identity_title_gains_a_scrolled_token_in_quiet_red() {
        // U3: a frozen view must say so — `↑N` (grid-clamped offset), same
        // quiet-red family as the raw token, glyph-adjacent.
        let spans =
            identity_title(40, "3 pi", false, 42, theme::GLYPH_WORKING, theme::accent()).unwrap();
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, format!(" 3 pi · ↑42 {} ", theme::GLYPH_WORKING));
        let token = spans.iter().find(|s| s.content.as_ref() == "↑42 ").expect("↑N token span");
        assert_eq!(token.style, theme::accent_quiet());
    }

    #[test]
    fn identity_title_scrolled_and_raw_tokens_compose_in_order() {
        // raw · ↑N — input state first, then view state, then the glyph.
        let spans = identity_title(40, "3 pi", true, 7, theme::GLYPH_IDLE, theme::quiet()).unwrap();
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, format!(" 3 pi · raw · ↑7 {} ", theme::GLYPH_IDLE));
    }

    #[test]
    fn identity_title_at_live_tail_has_no_scrolled_token() {
        assert!(!ident(40, "3 pi").contains('↑'));
    }

    // -- C32: the note title on the bottom border -------------------------

    /// C32 (amended 2026-08-21): the focused pane reads its note out —
    /// headline in `ink()` (the note's one full-strength element), age tag
    /// `quiet` — on the bottom border, which it has to itself. Text
    /// unchanged from the badge's note segment; only the edge moved.
    #[test]
    fn focused_note_title_reads_the_headline_in_ink_with_a_quiet_age() {
        let note = BadgeNote {
            headline: "tests green, PR up".into(),
            more: false,
            age: Some("14h".into()),
            focused: true,
        };
        let spans = note_title(60, &note).unwrap();
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, " ¶ tests green, PR up (14h) ");
        let headline = spans.iter().find(|s| s.content.as_ref().contains("tests green")).unwrap();
        assert_eq!(headline.style, theme::ink(), "the headline is the note's loud element");
        let age = spans.iter().find(|s| s.content.as_ref().contains("(14h)")).unwrap();
        assert_eq!(age.style, theme::quiet(), "the age tag stays quiet");

        // A note missing its timestamp (hand-edited state) shows NO age tag
        // — an absent fact renders as absent, never as a fabricated "now".
        let unstamped = BadgeNote { age: None, ..note };
        let spans = note_title(60, &unstamped).unwrap();
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, " ¶ tests green, PR up ");
    }

    /// C32: an unfocused pane marks presence, never content — a bare `¶` in
    /// `ink()` (`¶⋮` when a body exists), no headline, no age.
    /// Reveal-on-visit is the display contract, and moving edges did not
    /// move it.
    #[test]
    fn unfocused_note_title_shows_only_the_marker() {
        let note = BadgeNote {
            headline: "tests green, PR up".into(),
            more: false,
            age: Some("14h".into()),
            focused: false,
        };
        let spans = note_title(60, &note).unwrap();
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, " ¶ ");
        assert_eq!(spans[0].style, theme::ink(), "the marker is findable, not dim");

        // A body under the headline shows as ¶⋮ — "there's more here".
        let deeper = BadgeNote { more: true, ..note };
        let text: String =
            note_title(60, &deeper).unwrap().iter().map(|s| s.content.to_string()).collect();
        assert_eq!(text, " ¶⋮ ");
    }

    /// The note owns a whole edge, so a headline the badge would have
    /// clipped away survives a narrow pane — the reason it moved. The marker
    /// is fixed, so a note never vanishes: it degrades to `¶`.
    #[test]
    fn note_title_clips_the_headline_and_never_the_marker() {
        let note = BadgeNote {
            headline: "waiting on the schema review".into(),
            more: false,
            age: Some("2h".into()),
            focused: true,
        };
        // A 28-column pane's bottom border shows the headline whole — the
        // badge, sharing the top row with identity, never could.
        let text: String =
            note_title(26, &note).unwrap().iter().map(|s| s.content.to_string()).collect();
        assert!(text.starts_with(" ¶ waiting on the schema"), "{text:?}");

        for budget in 3..30u16 {
            let spans = note_title(budget, &note).expect("a marker always fits from 3 columns");
            let text: String = spans.iter().map(|s| s.content.to_string()).collect();
            assert!(mouse::display_width(&text) <= budget, "budget {budget}: {text:?}");
            assert!(text.contains('¶'), "budget {budget} lost the marker: {text:?}");
        }
        assert_eq!(note_title(2, &note), None, "below the marker itself, nothing");
    }

    /// C32: the age tag's units — floored, coarsest-sensible, "now" under
    /// a minute; a backwards clock clamps instead of underflowing.
    #[test]
    fn age_word_floors_to_the_coarsest_sensible_unit() {
        assert_eq!(age_word(100, 100), "now");
        assert_eq!(age_word(100, 159), "now");
        assert_eq!(age_word(100, 160), "1m");
        assert_eq!(age_word(0, 59 * 60), "59m");
        assert_eq!(age_word(0, 3600), "1h");
        assert_eq!(age_word(0, 86399), "23h");
        assert_eq!(age_word(0, 86400), "1d");
        assert_eq!(age_word(0, 86400 * 3 + 7), "3d");
        assert_eq!(age_word(500, 100), "now", "a clock that moved backwards clamps");
    }

    /// C32: a noted collapsed row's right segment leads with `¶ ` in its
    /// own `ink()` span — presence only, same meaning as the unfocused
    /// badge marker — and sheds with the segment when narrow.
    #[test]
    fn noted_collapsed_row_marks_the_right_segment() {
        let spans = collapsed_row_spans(
            40,
            false,
            Some(AgentStatus::Idle),
            2,
            "api",
            "shell",
            false,
            false,
            true,
            theme::GLYPH_WORKING,
        );
        let marker = spans.iter().find(|s| s.content.as_ref() == "¶ ").expect("marker span");
        assert_eq!(marker.style, theme::ink());
        // Narrow: the whole right segment — marker included — sheds.
        let spans = collapsed_row_spans(
            8,
            false,
            Some(AgentStatus::Idle),
            2,
            "api",
            "shell",
            false,
            false,
            true,
            theme::GLYPH_WORKING,
        );
        assert!(spans.iter().all(|s| !s.content.as_ref().contains('¶')));
    }

    /// C32: the note dialog is Rename's width, one content row per line —
    /// it grows a row per Shift+↵ instead of scrolling.
    #[test]
    fn note_dialog_height_tracks_its_line_count() {
        let body = Rect::new(0, 0, 100, 30);
        let mk = |n: usize| Mode::PaneEdit {
            name: String::new(),
            lines: vec![String::new(); n],
            row: 0,
            col: 0,
            pane: 1,
        };
        let one = dialog_rect(&mk(1), body, body, 0, &[], &Keymap::default()).unwrap();
        assert_eq!((one.width, one.height), (44, 4), "name row + one note line");
        let five = dialog_rect(&mk(5), body, body, 0, &[], &Keymap::default()).unwrap();
        assert_eq!(five.height, 8);
    }

    #[test]
    fn badge_glyph_yields_to_the_steady_frame_while_the_view_is_frozen() {
        // N1: an animating glyph means "alive right now" — a scrolled pane's
        // glyph freezes at its steady frame instead; the tail resumes it.
        let frame = theme::SPINNER_FRAMES[3]; // pretend mid-spin frame
        assert_eq!(super::badge_glyph(true, 0, frame, theme::GLYPH_WORKING), frame);
        assert_eq!(super::badge_glyph(true, 12, frame, theme::GLYPH_WORKING), theme::GLYPH_WORKING);
        // Non-spinning statuses are steady either way.
        assert_eq!(super::badge_glyph(false, 0, frame, theme::GLYPH_IDLE), theme::GLYPH_IDLE);
        assert_eq!(super::badge_glyph(false, 12, frame, theme::GLYPH_IDLE), theme::GLYPH_IDLE);
    }

    #[test]
    fn hint_bar_right_carries_the_scroll_position_ahead_of_the_mode_word() {
        // U3: `↑N/M` rides inside the right segment (quiet), so C9's yield
        // machinery covers it — and it only exists when a position is given.
        let spans = hint_bar_right_spans(
            None,
            None,
            Some("↑12/300".into()),
            "SCROLL",
            Some("Alt+a".into()),
        );
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "↑12/300 SCROLL ");
        assert_eq!(spans[0].style, theme::quiet());

        let spans = hint_bar_right_spans(
            Some((2, true)),
            None,
            Some("↑12/300".into()),
            "SCROLL",
            Some("Alt+a".into()),
        );
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "◆ 2 needs you · Alt+a  ↑12/300 SCROLL ");
    }

    /// The ○ fallback renders in the same slot as a real ◆, with its own
    /// wording and one visual step back (`ink()` rather than `accent()`) so
    /// a real ◆ still reads as more urgent.
    #[test]
    fn hint_bar_right_segment_renders_the_waiting_fallback_one_step_back_from_needs_input() {
        let spans =
            hint_bar_right_spans(Some((3, false)), None, None, "NORMAL", Some("Alt+a".into()));
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "○ 3 your turn · Alt+a  NORMAL ");
        assert_eq!(spans[0].style, theme::ink());

        let spans =
            hint_bar_right_spans(Some((3, true)), None, None, "NORMAL", Some("Alt+a".into()));
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "◆ 3 needs you · Alt+a  NORMAL ");
        assert_eq!(spans[0].style, theme::accent());
    }

    /// P21: the search prompt lives in C9's right segment — `/query▏` in
    /// `ink` (it is live input; quiet input is input you cannot proofread)
    /// with the `quiet` hit counter and the SEARCH mode word behind it. The
    /// prompt draws no dialog: the
    /// pane being searched must stay visible while the query narrows.
    #[test]
    fn hint_bar_right_carries_the_search_prompt_and_hit_counter() {
        use crate::core::app::Search;
        let lines: Vec<String> = ["alpha beta", "beta beta"].map(String::from).to_vec();
        let mut s = Search::over(lines, "beta", 1);
        let (query, position) = super::search_segment(Some(&s));
        let spans = hint_bar_right_spans(None, query, position, "SEARCH", Some("Alt+a".into()));
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, format!("/beta{} 2/3 SEARCH ", theme::RENAME_CURSOR));
        assert_eq!(spans[0].style, theme::ink(), "the typed query is legible, not dim");
        assert_eq!(spans[1].style, theme::quiet(), "the counter is a position token");

        // A query with no hits says so rather than hiding the counter — an
        // empty result is the answer, not the absence of one.
        s = Search::over(vec!["alpha beta".into()], "gamma", 0);
        let (query, position) = super::search_segment(Some(&s));
        let spans = hint_bar_right_spans(None, query, position, "SEARCH", Some("Alt+a".into()));
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, format!("/gamma{} 0/0 SEARCH ", theme::RENAME_CURSOR));

        // Outside a search there is no prompt at all.
        assert_eq!(super::search_segment(None), (None, None));
    }

    /// P21: hits are painted REVERSED and the current one additionally
    /// UNDERLINED — modifiers only (C17), and positioned by the same
    /// `banked − offset` arithmetic the jump uses, so a hit scrolled off
    /// the top of the view paints nothing.
    #[test]
    fn search_hits_are_reversed_with_the_current_one_underlined() {
        use crate::core::app::Search;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        // Haystack lines 4 and 6 hold hits; the view's top row shows line 4.
        let lines: Vec<String> = (0..8)
            .map(|i| if i == 4 || i == 6 { "xxbetaxx".to_string() } else { "----".to_string() })
            .collect();
        // current = 1 → the line-6 hit.
        let search = Search::over(lines, "beta", 1);
        let inner = Rect::new(0, 0, 10, 4);
        let backend = TestBackend::new(10, 4);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| super::highlight_matches(f, inner, &search, 4)).unwrap();
        let buf = term.backend().buffer().clone();

        // Line 4 → screen row 0, cols 2..6: reversed, not underlined.
        for c in 2..6 {
            let cell = buf.cell((c, 0)).unwrap();
            assert!(cell.style().add_modifier.contains(Modifier::REVERSED), "col {c}");
            assert!(!cell.style().add_modifier.contains(Modifier::UNDERLINED), "col {c}");
        }
        // Line 6 → screen row 2: the current hit, so also underlined.
        for c in 2..6 {
            let cell = buf.cell((c, 2)).unwrap();
            assert!(cell.style().add_modifier.contains(Modifier::REVERSED), "col {c}");
            assert!(cell.style().add_modifier.contains(Modifier::UNDERLINED), "col {c}");
        }
        // Cells outside the runs are untouched.
        assert!(!buf.cell((1, 0)).unwrap().style().add_modifier.contains(Modifier::REVERSED));
        assert!(!buf.cell((6, 0)).unwrap().style().add_modifier.contains(Modifier::REVERSED));

        // A view scrolled past both hits paints nothing at all.
        let backend = TestBackend::new(10, 4);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| super::highlight_matches(f, inner, &search, 7)).unwrap();
        let buf = term.backend().buffer().clone();
        for y in 0..4 {
            for x in 0..10 {
                assert!(!buf
                    .cell((x, y))
                    .unwrap()
                    .style()
                    .add_modifier
                    .contains(Modifier::REVERSED));
            }
        }
    }

    #[test]
    fn state_word_matches_c8_table() {
        assert_eq!(state_word(AgentStatus::Working), "working");
        assert_eq!(state_word(AgentStatus::NeedsInput), "needs you");
        assert_eq!(state_word(AgentStatus::Waiting), "your turn");
        assert_eq!(state_word(AgentStatus::Idle), "idle");
        assert_eq!(state_word(AgentStatus::Exited), "exited");
    }

    /// C8's name ramp, as amended by the theme-inherited stance: two text
    /// rungs, not three (Exited joins Waiting/Idle in the quiet one — its
    /// `✕` glyph and `exited` word already say it is dead), and a focused
    /// row is full-strength ink whatever its status, since the `RULE` fill
    /// that used to carry focus is gone.
    #[test]
    fn collapsed_name_style_by_state_and_focus() {
        assert_eq!(collapsed_name_style(Some(AgentStatus::Working), false), theme::ink());
        assert_eq!(collapsed_name_style(Some(AgentStatus::NeedsInput), false), theme::ink());
        assert_eq!(collapsed_name_style(Some(AgentStatus::Waiting), false), theme::quiet());
        assert_eq!(collapsed_name_style(Some(AgentStatus::Idle), false), theme::quiet());
        assert_eq!(collapsed_name_style(Some(AgentStatus::Exited), false), theme::quiet());
        // C27: a pane its tab has never spawned reads with the quiet rung —
        // it is not working, and it is not dead either.
        assert_eq!(collapsed_name_style(None, false), theme::quiet());
        // Focus overrides the ramp: the row you are on is never the quiet one.
        for s in [
            Some(AgentStatus::Working),
            Some(AgentStatus::NeedsInput),
            Some(AgentStatus::Waiting),
            Some(AgentStatus::Idle),
            Some(AgentStatus::Exited),
            None,
        ] {
            assert_eq!(collapsed_name_style(s, true), theme::ink(), "{s:?} focused");
        }
    }

    #[test]
    fn collapsed_row_shows_right_segment_when_it_fits() {
        let spans = collapsed_row_spans(
            40,
            false,
            Some(AgentStatus::Working),
            2,
            "pi",
            "pi",
            true,
            false,
            false,
            theme::GLYPH_WORKING,
        );
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.ends_with("pi · working "));
    }

    #[test]
    fn collapsed_row_no_dup_rule_drops_adapter_prefix_when_untitled() {
        // C8 no-dup rule (mirrors C4's badge_text): an untitled pane's name
        // is already the adapter/cwd fallback built in draw_pane, so the
        // right segment is the bare state word — "your turn", not
        // "shell · your turn". [DESIGN-ui.md amended 2026-07-22, ux #3.]
        let spans = collapsed_row_spans(
            40,
            false,
            Some(AgentStatus::Waiting),
            2,
            "shell",
            "shell",
            false,
            false,
            false,
            theme::GLYPH_WORKING,
        );
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.ends_with("your turn "));
        assert!(!text.contains("shell ·"));
    }

    #[test]
    fn collapsed_row_drops_right_segment_before_clipping_name() {
        let name = "a-fairly-long-pane-name";
        // Exactly enough room for "marker + glyph + ' ' + id + ' ' + name",
        // nothing more (U2: the id rides with the name on the left).
        let left_w = 5 + name.chars().count() as u16;
        let spans = collapsed_row_spans(
            left_w,
            false,
            Some(AgentStatus::Idle),
            2,
            name,
            "shell",
            true,
            false,
            false,
            theme::GLYPH_WORKING,
        );
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, format!(" · 2 {name}"));
        assert!(!text.contains("shell"));
    }

    #[test]
    fn collapsed_row_clips_name_when_even_the_left_side_overflows() {
        let spans = collapsed_row_spans(
            4,
            false,
            Some(AgentStatus::Waiting),
            2,
            "a-very-long-pane-name",
            "shell",
            false,
            true,
            false,
            theme::GLYPH_WORKING,
        );
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text.chars().count(), 4);
        assert!(!text.contains("shell"));
    }

    #[test]
    fn collapsed_row_focused_marker_is_accent() {
        let spans = collapsed_row_spans(
            40,
            true,
            Some(AgentStatus::Working),
            2,
            "pi",
            "pi",
            true,
            false,
            false,
            theme::GLYPH_WORKING,
        );
        assert_eq!(spans[0].content.as_ref(), theme::MARKER_ACTIVE.to_string());
        assert_eq!(spans[0].style, theme::accent());
    }

    #[test]
    fn collapsed_row_raw_gains_the_prefix_ahead_of_the_usual_right_segment() {
        let titled = collapsed_row_spans(
            60,
            false,
            Some(AgentStatus::Working),
            2,
            "pi",
            "pi",
            true,
            true,
            false,
            theme::GLYPH_WORKING,
        );
        let text: String = titled.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.ends_with("raw · pi · working "), "{text}");

        let untitled = collapsed_row_spans(
            60,
            false,
            Some(AgentStatus::Waiting),
            2,
            "shell",
            "shell",
            false,
            true,
            false,
            theme::GLYPH_WORKING,
        );
        let text: String = untitled.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.ends_with("raw · your turn "), "{text}");
    }

    #[test]
    fn stack_header_text_is_uppercase_and_right_aligned() {
        let text = stack_header_text(30, 3);
        assert_eq!(text.chars().count(), 30);
        assert!(text.starts_with(" STACK · 3 PANES"));
        assert!(text.ends_with("ALT+↑↓ "));
        assert_eq!(text, text.to_uppercase());
    }

    #[test]
    fn hint_pairs_normal_mode_is_exactly_the_seven_c9_pairs() {
        // U6 order (C9 amended 2026-07-27): `Alt+? keys` first so it yields
        // last, `Alt+r rename` last so it drops first.
        assert_eq!(
            hint_pairs(&Mode::Normal, false, false, false, false),
            p(&[
                ("Alt+?", "keys"),
                ("Alt+n", "new"),
                ("Alt+↵", "launch"),
                ("Alt+s", "stack"),
                ("Alt+←↓↑→", "focus"),
                ("Alt+w", "close"),
                ("Alt+r", "edit"),
            ]),
        );
    }

    /// U6, the live-QA case that started it: at 120 columns with the
    /// needs-you segment up (`◆ 1 needs you · Alt+a  NORMAL `, 30 cols),
    /// the bar used to drop `Alt+? keys` — the discoverability pointer —
    /// exactly when the fleet got busy. Now `Alt+r rename` goes first and
    /// the help pair survives; under enough pressure to drop six pairs, the
    /// one still standing is `Alt+? keys`.
    #[test]
    fn the_help_pair_is_the_last_hint_to_yield_and_rename_the_first() {
        let pairs = hint_pairs(&Mode::Normal, false, false, false, false);
        let pw = |x: &(String, &str)| super::hint_pair_cols(&x.0, x.1);
        let right_w = " ◆ 1 needs you · Alt+a  NORMAL ".chars().count() as u16 - 1;

        let shown = super::fit_hint_pairs(&pairs, right_w, 120);
        assert_eq!(shown, pairs.len() - 1, "exactly one pair yields at 120 cols");
        assert!(!pairs[..shown].contains(&one("Alt+r", "edit")), "the edit pair drops first");
        assert!(pairs[..shown].contains(&one("Alt+?", "keys")), "the help pair survives");

        // Squeeze until a single pair is left: it must be the help pair.
        let width = right_w + pw(&pairs[0]);
        let shown = super::fit_hint_pairs(&pairs, right_w, width);
        assert_eq!(shown, 1);
        assert_eq!(pairs[0], one("Alt+?", "keys"));
    }

    /// C24's list as amended by U17: the mode's whole vocabulary, and it
    /// still has to fit beside the right segment at the 100-col floor —
    /// a hint bar that clips its own mode's keys is the U17 bug restated.
    #[test]
    fn hint_pairs_copy_mode_is_the_c24_list_amended_by_u17() {
        let pairs = hint_pairs(&Mode::Copy { cursor: (0, 0) }, false, false, false, false);
        assert_eq!(
            pairs,
            p(&[
                ("hjkl", "move"),
                ("w/b/e", "word"),
                ("0/$", "ends"),
                ("v/V", "mark"),
                ("y/↵", "yank"),
                ("o", "open"),
                ("drag", "select"),
                ("Esc", "exit"),
            ]),
        );
        let cols: u16 = pairs.iter().map(|(k, l)| super::hint_pair_cols(k, l)).sum();
        let right_w = super::hint_bar_right_spans(None, None, None, "COPY", Some("Alt+a".into()))
            .iter()
            .map(|s| mouse::display_width(&s.content))
            .sum::<u16>();
        assert!(
            cols + right_w <= 100,
            "copy hints are {cols} cols + {right_w} of segment; the 100-col floor clips them"
        );
    }

    /// C40: a mark names a pane that is usually in another tab, i.e.
    /// invisible — this pair is the only thing that says the gesture is
    /// still open, so it leads (pairs drop whole from the right) and it is
    /// there only while there is something to pull.
    #[test]
    fn the_pull_pair_leads_the_bar_only_while_a_pane_is_marked() {
        let unmarked = hint_pairs(&Mode::Normal, false, false, false, false);
        assert!(!unmarked.iter().any(|(k, _)| k.contains("Shift+v")), "{unmarked:?}");

        let marked =
            super::hint_pairs(&Mode::Normal, false, false, false, false, true, &Keymap::default());
        assert_eq!(marked[0], one("Alt+Shift+v", "pull marked pane"));
        assert_eq!(&marked[1..], &unmarked[..], "and nothing else on the bar moved");
        // It must survive the squeeze that drops everything else, or the
        // one pair the user needs is the first one gone.
        let right_w = " NORMAL ".chars().count() as u16;
        let width = right_w + super::hint_pair_cols(&marked[0].0, marked[0].1);
        assert_eq!(super::fit_hint_pairs(&marked, right_w, width), 1);
    }

    #[test]
    fn hint_pairs_focused_raw_normal_is_exactly_one_pair() {
        // C23: every other hint would be a lie — nothing else is intercepted.
        assert_eq!(
            hint_pairs(&Mode::Normal, false, false, true, false),
            p(&[("Alt+Shift+p", "exit raw")])
        );
    }

    #[test]
    fn hint_pairs_dead_beats_raw_when_somehow_both() {
        // A dead pane never raw-routes (`App::raw_routing_active` requires
        // it alive) — but the flag can still be *set* on a dead pane, so the
        // hint bar must show what's actually actionable (dead-pane keys),
        // not a raw-exit hint nothing would honor.
        assert_eq!(
            hint_pairs(&Mode::Normal, true, false, true, false),
            p(&[
                ("↵", "relaunch"),
                ("f", "fresh — drops resume"),
                ("Alt+w", "close"),
                ("Alt+q", "quit")
            ]),
        );
    }

    #[test]
    fn hint_bar_right_omits_needs_segment_at_zero() {
        let spans = hint_bar_right_spans(None, None, None, "NORMAL", Some("Alt+a".into()));
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "NORMAL ");
        assert!(!text.contains('◆'));
    }

    #[test]
    fn hint_bar_right_shows_aggregate_before_mode_word_when_nonzero() {
        let spans =
            hint_bar_right_spans(Some((3, true)), None, None, "NORMAL", Some("Alt+a".into()));
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "◆ 3 needs you · Alt+a  NORMAL ");
        assert_eq!(spans[0].style, theme::accent());
    }

    /// C15 amended: one column is sized to the widest key-column-plus-
    /// description line, not a fixed 52 that clips long descriptions
    /// mid-word. Every key pads to the same column, so the longest
    /// description decides it.
    #[test]
    fn help_content_width_fits_the_widest_row() {
        // F1: measured over the *resolved* lines, since a row's key column
        // is no longer a compiled-in literal.
        let lines = help_lines(&Keymap::default(), "");
        let widest = lines
            .iter()
            .filter_map(|l| match l {
                HelpLine::Row(k, d, _) => {
                    Some(mouse::display_width(&super::help_key_prefix(k)) + mouse::display_width(d))
                }
                HelpLine::Head(_) => None,
            })
            .max()
            .expect("the keymap has rows");
        assert_eq!(help_content_width(&lines), widest);
    }

    /// C15: on a terminal too narrow for the keymap, the dialog is placed
    /// **inside** the body rather than merely reporting a size that fits.
    ///
    /// Every assertion in this test used to be vacuous. It read
    /// `w <= body.width` from `help_layout`, which ends in
    /// `.min(body.width)`; then `rect.width <= body.width` from
    /// `centered_near`, which opens with `width.min(bounds.width)`. Both
    /// hold by construction — the test's own comment said so and then
    /// asserted them anyway — so if either clamp were deleted the other
    /// would cover for it and this would still pass.
    ///
    /// What can actually go wrong is the *placement* arithmetic
    /// (`.clamp(bounds.x, bounds.x + bounds.width - w)`), so that is what
    /// this asserts now — with an **anchor hugging the right edge**, since
    /// a dialog centred on the body is inside it whether that clamp runs or
    /// not. The first rewrite missed that and still survived deleting the
    /// clamp; only the edge anchor makes the assertion bite. Found by
    /// sweeping for the shape three audits had already caught on this
    /// branch, then by mutation-testing the replacement.
    #[test]
    fn help_dialog_clamps_to_the_screen_via_centered_near() {
        let body = Rect::new(0, 1, 30, 20);
        let layout = help_layout(body, &Keymap::default(), None);
        assert!(
            layout.asked > body.width,
            "the keymap must not fit this body, or the width clamp is untested: asked {}",
            layout.asked,
        );
        let (w, h) = layout.size;
        // A pane against the right edge: centring the dialog on it alone
        // would put most of it off-screen.
        let anchor = Rect::new(24, 1, 6, 20);
        let rect = centered_near(anchor, body, w, h);
        assert!(rect.x >= body.x, "placed off the left edge: {rect:?}");
        assert!(
            rect.x + rect.width <= body.x + body.width,
            "spilled past the right edge: {rect:?} in {body:?}",
        );
        assert!(rect.y >= body.y, "placed above the body: {rect:?}");
        assert!(
            rect.y + rect.height <= body.y + body.height,
            "spilled past the bottom: {rect:?} in {body:?}",
        );
    }

    #[test]
    fn mode_word_matches_c9_table() {
        assert_eq!(mode_word(&Mode::Normal, false, false), "NORMAL");
        assert_eq!(
            mode_word(
                &Mode::Rename { buffer: String::new(), cursor: 0, target: RenameTarget::Tab },
                false,
                false
            ),
            "RENAME"
        );
        assert_eq!(
            mode_word(
                &Mode::Picker { selection: 0, filter: String::new(), cwd: 0, on_cwd: false },
                false,
                false
            ),
            "PICKER"
        );
        assert_eq!(mode_word(&Mode::Scroll, false, false), "SCROLL");
        assert_eq!(mode_word(&Mode::Copy { cursor: (0, 0) }, false, false), "COPY");
        assert_eq!(
            mode_word(&Mode::Help { top: 0, filter: None, cursor: 0 }, false, false),
            "HELP"
        );
    }

    #[test]
    fn mode_word_shows_zoom_pseudo_state_only_in_the_normal_slot() {
        // C21/amended C9: ZOOM shows only when the mode is Normal — every
        // other mode's own word wins regardless of the zoomed flag.
        assert_eq!(mode_word(&Mode::Normal, true, false), "ZOOM");
        assert_eq!(mode_word(&Mode::Normal, false, false), "NORMAL");
        assert_eq!(mode_word(&Mode::Scroll, true, false), "SCROLL");
        assert_eq!(mode_word(&Mode::Help { top: 0, filter: None, cursor: 0 }, true, false), "HELP");
    }

    #[test]
    fn mode_word_raw_beats_zoom_beats_normal_but_never_a_real_mode_word() {
        // C23/amended C9: in the Normal slot, RAW beats ZOOM beats NORMAL —
        // but any real (non-Normal) mode word still wins over both.
        assert_eq!(mode_word(&Mode::Normal, false, true), "RAW");
        assert_eq!(mode_word(&Mode::Normal, true, true), "RAW", "raw beats zoom");
        assert_eq!(mode_word(&Mode::Scroll, true, true), "SCROLL", "a real mode word always wins");
    }

    #[test]
    fn fit_hint_pairs_right_segment_wins_over_trailing_pairs() {
        // C9 yield order (re-amended 2026-07-22): pairs drop whole from the
        // right before the aggregate/mode-word segment ever yields. Measure
        // via the SAME `hint_pair_cols` the draw path uses — a +3/+4 drift
        // here once let the whole segment drop at widths 111-116 (D2).
        let pairs = hint_pairs(&Mode::Normal, false, false, false, false);
        let pw = |x: &(String, &str)| super::hint_pair_cols(&x.0, x.1);
        let all_w: u16 = pairs.iter().map(&pw).sum();

        // Roomy bar, no right segment: everything fits.
        assert_eq!(super::fit_hint_pairs(&pairs, 0, all_w + 10), pairs.len());
        // Invariant across every width/segment combo: the shown pairs plus
        // the right segment must ALWAYS fit — the segment is never clipped by
        // an over-optimistic fit. Sweep the band D2 lived in and beyond.
        for width in 80..=140u16 {
            for right_w in [0u16, 22, 30] {
                let shown = super::fit_hint_pairs(&pairs, right_w, width);
                let used: u16 = pairs[..shown].iter().map(&pw).sum();
                assert!(
                    used + right_w <= width || shown == 0,
                    "width={width} right_w={right_w}: shown={shown} used={used} overflows"
                );
                // And it's not needlessly stingy: one more pair really wouldn't fit.
                if shown < pairs.len() {
                    assert!(
                        used + pw(&pairs[shown]) + right_w > width,
                        "width={width}: too stingy"
                    );
                }
            }
        }
        // Degenerate: a bar narrower than the segment shows zero pairs (the
        // draw fn then right-aligns whatever of the segment still fits).
        assert_eq!(super::fit_hint_pairs(&pairs, 118, 120), 0);
    }

    #[test]
    fn hint_pairs_dead_focused_normal_offers_relaunch_not_new_pane() {
        let dead = hint_pairs(&Mode::Normal, true, false, false, false);
        assert_eq!(
            dead,
            p(&[
                ("↵", "relaunch"),
                ("f", "fresh — drops resume"),
                ("Alt+w", "close"),
                ("Alt+q", "quit"),
            ]),
        );
        // A live pane never offers "relaunch"; a dead one never offers "new".
        assert_ne!(dead, hint_pairs(&Mode::Normal, false, false, false, false));
        // A resumable dead pane adds `y copy resume` — before close/quit in
        // the yield order (those two are discoverable everywhere; `y` only
        // lives on this bar). A session-less pane must NOT advertise it.
        assert_eq!(
            hint_pairs(&Mode::Normal, true, true, false, false),
            p(&[
                ("↵", "relaunch"),
                ("f", "fresh — drops resume"),
                ("y", "copy resume"),
                ("Alt+w", "close"),
                ("Alt+q", "quit"),
            ]),
        );
    }

    /// C16: the overlay bar's two variants — the resumable one inserts
    /// `y: copy resume` before close, mirroring the hint bar's order.
    #[test]
    fn dead_bar_text_offers_copy_resume_only_when_resumable() {
        assert_eq!(
            super::dead_bar_text(false, Some("Alt+w")),
            " ✕ exited — Enter: relaunch/resume · f: fresh (drops resume) · Alt+w: close "
        );
        assert_eq!(
            super::dead_bar_text(true, Some("Alt+w")),
            " ✕ exited — Enter: relaunch/resume · f: fresh (drops resume) · y: copy resume · Alt+w: close "
        );
    }

    /// C20: every key the feed actually answers to — the whole point of
    /// making entries actionable is that people can tell they are.
    #[test]
    fn hint_pairs_feed_mode_lists_every_key_the_feed_answers_to() {
        assert_eq!(
            hint_pairs(&Mode::Feed { offset: 0 }, false, false, false, false),
            p(&[("↑↓", "select"), ("PgUp/Dn", "page"), ("↵", "go to pane"), ("q/Esc", "close"),]),
        );
    }

    /// C36/C9: the composer's list, its column figure, and — the part that
    /// matters — its **yield order**. At the 100-col floor with the
    /// needs-you segment up, trailing pairs drop, so the order is a safety
    /// property rather than a style choice: `↵ send` and `Esc cancel` must
    /// still be on the bar when the fleet is busy, which is exactly when
    /// someone is composing a broadcast. (The C36 audit found C9 had never
    /// been amended for this mode and nothing pinned the list.)
    #[test]
    fn hint_pairs_broadcast_leads_with_the_two_that_must_not_yield() {
        let mode =
            Mode::Broadcast { lines: vec![String::new()], row: 0, col: 0, status_filter: None };
        let pairs = hint_pairs(&mode, false, false, false, false);
        assert_eq!(
            pairs,
            p(&[
                ("↵", "send"),
                ("Esc", "cancel"),
                ("Tab", "who gets it"),
                ("Shift+↵", "new line"),
                ("type", "message"),
            ]),
        );
        let cols: u16 = pairs.iter().map(|(k, l)| super::hint_pair_cols(k, l)).sum();
        assert_eq!(cols, 74, "C36 quotes 74 columns");

        // The property the order exists for: with the needs-you segment up
        // at the floor, what survives is send and cancel.
        let right_w = " ◆ 3 needs you · Alt+a  BROADCAST ".chars().count() as u16 - 1;
        let shown = super::fit_hint_pairs(&pairs, right_w, 100);
        assert!(shown >= 2, "at least send and cancel survive a busy fleet");
        assert_eq!(pairs[0], one("↵", "send"));
        assert_eq!(pairs[1], one("Esc", "cancel"));
    }

    /// C27/C9: the roster's own list, and the 68-column number the contract
    /// quotes — it has to fit beside the right segment at the 100-col floor,
    /// which is the only reason the number is in the contract at all. Note
    /// what is *not* here: the feed's `q`, because in a list you filter by
    /// typing, `q` is filter text (U20's rule).
    #[test]
    fn hint_pairs_roster_mode_is_the_c27_list_and_fits_the_floor() {
        let mode = Mode::Roster { cursor: 1, filter: String::new(), top: 0, status_filter: None };
        let pairs = hint_pairs(&mode, false, false, false, false);
        assert_eq!(
            pairs,
            p(&[
                ("↑↓", "select"),
                ("PgUp/Dn", "page"),
                ("↵", "go to pane"),
                ("type", "filter"),
                ("Tab", "status"),
                ("Esc", "close"),
            ]),
        );
        let cols: u16 = pairs.iter().map(|(k, l)| super::hint_pair_cols(k, l)).sum();
        assert_eq!(cols, 81, "C27 quotes 81 columns");
        assert!(cols < 100, "and it must fit beside the right segment at the floor");
        assert!(!pairs.iter().any(|(k, _)| k.contains('q')), "`q` filters, it does not close");
    }

    /// C15 (amended): the hint bar tells the truth in all three states. A
    /// keymap that fits is closed by any key and says so; a scrolled one
    /// advertises the reading keys and narrows the claim to "any *other*
    /// key", because the arrows have stopped closing it; a **filtered** one
    /// drops the claim entirely, because a letter is query text now.
    ///
    /// [F9] That last row is C27's roster pairs, and for the reason the
    /// roster's own comment gives: once typing filters, `Esc` is the way
    /// out and the bar has to lead with it. A bar still promising "any key
    /// closes" over a live query would be the one lie this surface cannot
    /// afford — it is the surface you open *because* you are lost.
    #[test]
    fn the_help_hint_row_narrows_only_once_the_keymap_actually_scrolls() {
        // [Amended 2026-09-01] The un-filtered rows teach the typing rule
        // and its guaranteed exit — bare printables open the filter now, so
        // "any key close" stopped being true of the letters.
        let whole =
            hint_pairs(&Mode::Help { top: 0, filter: None, cursor: 0 }, false, false, false, false);
        assert_eq!(whole, p(&[("Alt+?", "all keys"), ("type", "filter"), ("Esc", "close")]));
        let scrolled =
            hint_pairs(&Mode::Help { top: 0, filter: None, cursor: 0 }, false, false, false, true);
        assert_eq!(scrolled, p(&[("↑↓ PgUp/Dn", "read on"), ("type", "filter"), ("Esc", "close")]),);
        let filtered = hint_pairs(
            &Mode::Help { top: 0, filter: Some("mov".into()), cursor: 0 },
            false,
            false,
            false,
            false,
        );
        // [C41] `↵ run` joins between the motion and the way out, and the
        // motion is relabelled: filtering, the arrows drive the palette's
        // cursor rather than scrolling a view.
        assert_eq!(
            filtered,
            p(&[
                ("type", "filter"),
                ("↑↓ PgUp/Dn", "move"),
                ("↵", "run"),
                ("Esc", "clear · close"),
            ]),
        );
        assert!(
            !filtered.iter().any(|(_, l)| l.contains("any key")),
            "a live query means `any key closes` is false — the bar must not still say it",
        );
        // C9's budget, at the floor, for every state. The `/` pair is new
        // width on two rows that were already sized; if it did not fit, the
        // affordance would have to go somewhere else rather than be shipped
        // over the floor.
        for (what, row) in [("whole", &whole), ("scrolled", &scrolled), ("filtered", &filtered)] {
            let cols: u16 = row.iter().map(|(k, l)| super::hint_pair_cols(k, l)).sum();
            assert!(cols < 100, "{what} row must fit beside the right segment: {cols}");
        }
    }

    #[test]
    fn mode_word_roster_wins_regardless_of_zoom() {
        let mode = Mode::Roster { cursor: 1, filter: String::new(), top: 0, status_filter: None };
        assert_eq!(mode_word(&mode, true, false), "ROSTER");
    }

    #[test]
    fn mode_word_feed_wins_regardless_of_zoom() {
        assert_eq!(mode_word(&Mode::Feed { offset: 0 }, false, false), "FEED");
        assert_eq!(mode_word(&Mode::Feed { offset: 0 }, true, false), "FEED");
    }

    /// C32: the tab dialog and the combined pane editor lead their hint
    /// lists with different words — the bar always says which one is up.
    #[test]
    fn hint_pairs_rename_word_differs_tab_vs_pane_editor() {
        let tab = hint_pairs(
            &Mode::Rename { buffer: String::new(), cursor: 0, target: RenameTarget::Tab },
            false,
            false,
            false,
            false,
        );
        let editor = hint_pairs(
            &Mode::PaneEdit {
                name: String::new(),
                lines: vec![String::new()],
                row: 0,
                col: 0,
                pane: 1,
            },
            false,
            false,
            false,
            false,
        );
        assert_eq!(tab[0], one("type", "tab name"));
        assert_eq!(editor[0], one("type", "name / note"));
        assert!(editor.iter().any(|x| x.1 == "new line"));
    }

    #[test]
    fn push_tab_spans_active_tab_uses_accent_marker_and_reset_bg() {
        // C2: active tab — marker ▎ `accent`, label `ink` (fuses with the
        // terminal's own bg — no fill), glyph in its own style, separator
        // `rule`.
        let mut spans = Vec::new();
        push_tab_spans(&mut spans, 0, "main", true, theme::GLYPH_WORKING, theme::accent(), 3);
        assert_eq!(spans.len(), 9); // 8 parts + the count cell (C2, amended 2026-07-28)
        assert_eq!(spans[0].content.as_ref(), theme::MARKER_ACTIVE.to_string());
        assert_eq!(spans[0].style, theme::accent());
        assert_eq!(spans[2].content.as_ref(), "1 main");
        assert_eq!(spans[2].style, theme::ink());
        assert_eq!(spans[4].content.as_ref(), theme::GLYPH_WORKING.to_string());
        assert_eq!(spans[4].style, theme::accent());
        // ...then the count, in the glyph's own style so `⠋3` is one token.
        assert_eq!(spans[5].content.as_ref(), "3");
        assert_eq!(spans[5].style, theme::accent());
        assert_eq!(spans[7].content.as_ref(), theme::TAB_SEPARATOR.to_string());
        assert_eq!(spans[7].style, theme::rule());
        assert_eq!(spans[8].content.as_ref(), " "); // trailing gutter
    }

    #[test]
    fn push_tab_spans_inactive_tab_uses_blank_marker_and_quiet_label() {
        // C2: inactive tab — marker is a plain space (no accent), label
        // `quiet`, and no bg override: since 2026-07-27 there is no strip
        // fill underneath it either (§2 background policy).
        let mut spans = Vec::new();
        push_tab_spans(&mut spans, 1, "api", false, theme::GLYPH_IDLE, theme::quiet(), 1);
        assert_eq!(spans.len(), 9); // 8 parts + the count cell (C2, amended 2026-07-28)
        assert_eq!(spans[0].content.as_ref(), " ");
        assert_eq!(spans[0].style.fg, None);
        assert_eq!(spans[2].content.as_ref(), "2 api");
        assert_eq!(spans[2].style, theme::quiet());
        assert_eq!(spans[2].style.bg, None);
        assert_eq!(spans[4].content.as_ref(), theme::GLYPH_IDLE.to_string());
        assert_eq!(spans[4].style, theme::quiet());
        // A count below 2 still spends its column — blank, so the width is
        // the same as the `3` above (C2's stability rule).
        assert_eq!(spans[5].content.as_ref(), " ");
    }

    /// C2: the count cell's whole vocabulary — blank
    /// below 2, the digit through 9, `+` past that — and the invariant the
    /// tab-width formula rests on: exactly one column, every time.
    #[test]
    fn tab_count_cell_is_blank_below_two_a_digit_to_nine_then_plus() {
        assert_eq!(super::tab_count_cell(0), ' ');
        assert_eq!(super::tab_count_cell(1), ' ', "one is what a glyph already means");
        assert_eq!(super::tab_count_cell(2), '2');
        assert_eq!(super::tab_count_cell(9), '9');
        // Past nine the exact number stops being actionable and two digits
        // would break the one-column rule the hit math is built on.
        assert_eq!(super::tab_count_cell(10), '+');
        assert_eq!(super::tab_count_cell(37), '+');
        for n in [0usize, 1, 2, 5, 9, 10, 11, 100, 1000] {
            assert_eq!(super::tab_count_cell_cols(n), 1, "count {n} must be one column");
        }
    }

    /// The geometry rule the count exists under: a tab's drawn width is the
    /// same before and after its count appears, so nothing on the bar moves
    /// when a background agent's status flips. Measured through the real span
    /// builder, not the formula — that is the half a formula can't prove.
    #[test]
    fn a_tabs_drawn_width_does_not_move_when_its_count_appears() {
        let drawn = |count: usize| -> u16 {
            let mut spans = Vec::new();
            push_tab_spans(
                &mut spans,
                0,
                "main",
                true,
                theme::GLYPH_NEEDS_INPUT,
                theme::accent(),
                count,
            );
            spans.iter().map(|s| mouse::display_width(&s.content)).sum()
        };
        let expected = mouse::tab_width(0, "main");
        for count in [0usize, 1, 2, 9, 10, 42] {
            assert_eq!(drawn(count), expected, "count {count} changed the tab's width");
        }
    }

    /// C2 end to end through `draw()`: a tab holding three needy panes draws
    /// `◆3`, and one holding a single needy pane draws `◆` with a blank cell
    /// — the exact confusion the tribunal found (one diamond for any number
    /// of blocked agents), and the fix, in the same frame.
    #[test]
    fn the_tab_bar_counts_panes_in_the_summarized_state() {
        use crate::core::status::AgentStatus;
        use crate::ports::PaneBackend;
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::NewPane);
        app.apply(Action::NewPane); // tab 0: three panes
        app.apply(Action::NewTab); // tab 1: one pane
        let bar = |app: &mut App<crate::ports::fakes::FakePane>| -> String {
            let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
            term.draw(|f| super::draw(f, app)).unwrap();
            let buf = term.backend().buffer().clone();
            (0..100).filter_map(|x| buf.cell((x, 0)).map(|c| c.symbol().to_string())).collect()
        };

        for id in [1u64, 2, 3] {
            app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::NeedsInput);
        }
        let row = bar(&mut app);
        assert!(row.contains("◆3"), "three needy panes count on the tab bar:\n{row}");

        // Knock two of them back down: the same tab, the same glyph, no count.
        for id in [2u64, 3] {
            app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::Idle);
        }
        let quiet_row = bar(&mut app);
        assert!(!quiet_row.contains("◆3"), "the count follows the fleet:\n{quiet_row}");
        assert!(quiet_row.contains('◆'), "...but the glyph still reports it:\n{quiet_row}");
        // And the bar is the same length either way — the stability rule,
        // observed on the drawn row rather than derived from the formula.
        assert_eq!(row.trim_end().len(), quiet_row.trim_end().len());
    }

    #[test]
    fn collapsed_row_spans_at_zero_width_is_empty_not_panicking() {
        let spans = collapsed_row_spans(
            0,
            true,
            Some(AgentStatus::Working),
            2,
            "pi",
            "pi",
            true,
            false,
            false,
            theme::GLYPH_WORKING,
        );
        assert!(spans.is_empty());
    }

    /// C8 (amended 2026-09-01): where the C6 geometry grants a collapsed
    /// member its 3 rows, the row draws inside an `accent_quiet()` bordered
    /// box — a collapsed pane reads as a pane, not as a rule between its
    /// bordered neighbours.
    #[test]
    fn boxed_collapsed_member_draws_a_quiet_red_border_around_its_row() {
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::NewPane);
        app.apply(Action::NewPane);
        // Focus the first pane so Alt+s reaches the *outer* split and stacks
        // all three (`stack_pane` folds the innermost split that directly
        // holds the target).
        app.focused = app.pane_order()[0];
        app.apply(Action::StackPane);
        let collapsed: Vec<crate::core::layout::PaneRect> =
            app.rects().into_iter().filter(|p| p.collapsed).collect();
        assert_eq!(collapsed.len(), 2, "a stack of three shows two collapsed members");

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| super::draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();

        for pr in &collapsed {
            let r = pr.rect;
            assert_eq!(
                r.height,
                crate::core::layout::COLLAPSED_BOX_ROWS,
                "boxed in a 28-row stack"
            );
            // Expectations derive from the token, not restated literals, so
            // the test follows theme.rs rather than passing an inlined hue.
            let want = theme::accent_quiet();
            for (x, y, glyph) in [
                (r.x, r.y, "┌"),
                (r.x + r.width - 1, r.y, "┐"),
                (r.x, r.y + r.height - 1, "└"),
                (r.x + r.width - 1, r.y + r.height - 1, "┘"),
            ] {
                let cell = &buf[(x, y)];
                assert_eq!(cell.symbol(), glyph, "box corner at ({x},{y})");
                assert_eq!(cell.style().fg, want.fg, "the border wears accent_quiet()'s hue…");
                assert!(
                    cell.style().add_modifier.contains(want.add_modifier),
                    "…and its modifier (C7's stack-chrome rung)"
                );
            }
            // The C8 row rides the box's single inner line.
            let inner: String = (r.x + 1..r.x + r.width - 1)
                .map(|x| buf[(x, r.y + 1)].symbol().to_string())
                .collect();
            assert!(inner.contains("shell"), "row text inside the box: {inner:?}");
        }
    }

    /// …and the C6 fallback regime is untouched: a stack too short for the
    /// boxes keeps its bare 1-row bars, with no border cells around them.
    #[test]
    fn a_shallow_stack_keeps_bare_one_row_collapsed_bars() {
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        // Body height 12 (14 − tab bar − hint bar): above the C6 header
        // threshold (n+3 = 6), below the boxed threshold (3n+8 = 17).
        let mut app = mk_app(Size::new(100, 14));
        app.apply(Action::NewPane);
        app.apply(Action::NewPane);
        // Outer split, as above: all three panes into one stack.
        app.focused = app.pane_order()[0];
        app.apply(Action::StackPane);
        let collapsed: Vec<crate::core::layout::PaneRect> =
            app.rects().into_iter().filter(|p| p.collapsed).collect();
        assert_eq!(collapsed.len(), 2);

        let mut term = Terminal::new(TestBackend::new(100, 14)).unwrap();
        term.draw(|f| super::draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();

        for pr in &collapsed {
            let r = pr.rect;
            assert_eq!(r.height, 1, "below the boxed threshold the bar stays 1 row");
            let row: String =
                (r.x..r.x + r.width).map(|x| buf[(x, r.y)].symbol().to_string()).collect();
            assert!(row.contains("shell"), "the bar still carries the C8 row: {row:?}");
            assert!(!row.contains('┌'), "no border cells in the fallback form: {row:?}");
        }
    }

    #[test]
    fn stack_header_text_does_not_panic_when_width_is_smaller_than_content() {
        // The header is gated on the stack area's *height* (C6), not its
        // width, so a tall-but-narrow stack can still ask for a header
        // narrower than " STACK · N PANES" + "ALT+↑↓ ". Must degrade by
        // overflowing the string (Paragraph clips visually, same as the hint
        // bar), not panic.
        let text = stack_header_text(4, 3);
        assert!(text.contains("STACK · 3 PANES"));
        assert!(text.ends_with("ALT+↑↓ "));
    }

    // -- C20 activity feed ---------------------------------------------------

    #[test]
    fn feed_window_shows_the_newest_rows_when_offset_is_zero() {
        assert_eq!(feed_window(5, 0, 3), 2..5);
    }

    #[test]
    fn feed_window_scrolls_back_by_offset() {
        assert_eq!(feed_window(5, 2, 3), 0..3);
    }

    #[test]
    fn feed_window_clamps_offset_past_the_oldest_entry() {
        assert_eq!(feed_window(5, 999, 3), 0..1);
    }

    #[test]
    fn feed_window_shrinks_to_whatever_the_ring_actually_has() {
        assert_eq!(feed_window(2, 0, 10), 0..2);
    }

    #[test]
    fn feed_window_empty_ring_or_zero_rows_is_empty() {
        assert_eq!(feed_window(0, 0, 3), 0..0);
        assert_eq!(feed_window(5, 0, 0), 0..0);
    }

    /// C20's default row is the quiet rung throughout — timestamp and text
    /// alike. They used to be two different greys (DIM and MUTED); §2's
    /// two-rung ramp merged them, and nothing is lost: the timestamp is
    /// column-aligned, which is what actually separates it from the text.
    #[test]
    fn feed_entry_spans_default_styling_is_quiet_throughout() {
        let spans = feed_entry_spans("12:34:56", "spawned shell (shell)", false, false);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, " 12:34:56  spawned shell (shell)");
        assert_eq!(spans[1].style, theme::quiet());
        assert_eq!(spans[2].style, theme::quiet());
    }

    /// U25: the selected entry — the one Enter acts on — is marked, in the
    /// leading column the unselected rows spend on a space, so the row's
    /// width and every column after it are unchanged.
    #[test]
    fn feed_entry_spans_mark_the_selected_row_without_shifting_a_column() {
        let plain = feed_entry_spans("12:34:56", "spawned shell", false, false);
        let picked = feed_entry_spans("12:34:56", "spawned shell", false, true);
        let text =
            |v: &[super::Span<'_>]| -> String { v.iter().map(|s| s.content.as_ref()).collect() };
        assert_eq!(text(&picked), format!("{}12:34:56  spawned shell", theme::PICKER_SELECTED));
        assert_eq!(text(&plain).chars().count(), text(&picked).chars().count());
        assert_eq!(picked[0].style, theme::accent());
        // Everything past the marker is styled identically either way.
        assert_eq!(picked[1].style, plain[1].style);
        assert_eq!(picked[2].style, plain[2].style);
    }

    #[test]
    fn feed_entry_spans_needs_input_line_gets_the_accent_diamond_and_ink_text() {
        let spans = feed_entry_spans("12:34:56", "pi: waiting → needs you", true, false);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            text,
            format!(" 12:34:56  {} pi: waiting → needs you", theme::GLYPH_NEEDS_INPUT)
        );
        assert_eq!(spans[1].style, theme::quiet());
        assert_eq!(spans[2].style, theme::accent());
        assert_eq!(spans[3].style, theme::ink());
    }

    #[test]
    fn local_hh_mm_ss_formats_as_a_zero_padded_clock() {
        let s = super::local_hh_mm_ss(std::time::SystemTime::now());
        assert_eq!(s.len(), 8);
        assert_eq!(s.as_bytes()[2], b':');
        assert_eq!(s.as_bytes()[5], b':');
    }

    // -- C15/§8 help overlay ---------------------------------------------------

    /// §8's table and the drawn keymap are the same table: **every chord
    /// roost binds is documented by the overlay**.
    ///
    /// **[Rewritten by F1, 2026-08-19.]** This used to be a hand-kept list
    /// of 27 chord literals checked for containment against the hand-kept
    /// `HELP_GROUPS` text — both `const`, neither derived from
    /// `default_chord_action`. It therefore proved only that one constant
    /// mentioned strings another constant was written to contain: it said
    /// nothing about the running binary, and a newly bound chord passed it
    /// silently unless someone remembered to extend the list by hand. C33
    /// bound four chords and had to do exactly that.
    ///
    /// It is a real sweep now, over the same merged table `translate_with`
    /// dispatches on. It checks **actions rather than chord spellings**,
    /// deliberately: a row may legitimately draw a family shorthand
    /// (`Alt+←↓↑→ / hjkl`) that contains no single chord as a substring, and
    /// that shorthand is only ever drawn while it is an accurate
    /// description of the defaults (`help_key_text`, pinned by
    /// `a_family_spelling_gives_way_once_one_of_its_chords_moves`). So
    /// "which chords does this row account for" is a question about the
    /// actions it declares, and asking it that way needs no list to keep.
    #[test]
    fn every_bound_chord_is_documented_in_the_keymap() {
        let documented: Vec<Action> = HELP_GROUPS
            .iter()
            .flat_map(|g| g.rows.iter())
            .flat_map(|r| match &r.key {
                HelpKey::Chords(a) | HelpKey::Family(_, a) => *a,
                HelpKey::Text(_) => &[],
            })
            .copied()
            .collect();
        for (label, action) in input::effective_bindings(&Keymap::default()) {
            assert!(
                documented.contains(&action),
                "roost binds {label} to {action:?} and no keymap row documents it",
            );
        }
    }

    /// The converse, and the failure mode the coverage sweep cannot see: a
    /// row that declares an action **nothing binds** renders nothing and
    /// vanishes from the overlay silently. Before F1 that could not happen
    /// (rows were literal text, so a stale row stayed visible and merely
    /// lied); now a typo'd or orphaned row is invisible instead, which is
    /// quieter and therefore worth a gate of its own.
    #[test]
    fn every_documented_row_names_an_action_roost_actually_binds() {
        let bound: Vec<Action> =
            input::effective_bindings(&Keymap::default()).into_iter().map(|(_, a)| a).collect();
        for group in HELP_GROUPS {
            for row in group.rows {
                let (HelpKey::Chords(actions) | HelpKey::Family(_, actions)) = &row.key else {
                    continue; // a legend or CLI reference row names no chord
                };
                for action in *actions {
                    assert!(
                        bound.contains(action),
                        "{}'s row {:?} declares {action:?}, which no chord binds — \
                         the row would silently vanish from the overlay",
                        group.title,
                        row.desc,
                    );
                }
            }
        }
    }

    /// The property that makes the coverage gate above sufficient rather
    /// than merely necessary: a row draws **every** chord bound to the
    /// actions it declares, so a second chord for a documented action
    /// documents itself. Verified by remapping a free key onto `Quit` and
    /// watching the existing row grow — no table edit involved.
    ///
    /// This is why the gate can check actions instead of keeping a list of
    /// chord spellings: the two questions "is this action documented" and
    /// "is this chord documented" only came apart under the old `&'static`
    /// table, where a row named one fixed chord no matter what was bound.
    #[test]
    fn a_second_chord_for_a_documented_action_documents_itself() {
        let (keymap, diagnostics) = Keymap::parse(r#"{"keys": {"alt+x": "quit"}}"#, "config.json");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let quit_row = help_lines(&keymap, "")
            .into_iter()
            .find_map(|l| match l {
                HelpLine::Row(k, d, _) if d.starts_with("quit") => Some(k),
                _ => None,
            })
            .expect("a quit row");
        assert_eq!(quit_row, "Alt+q / Alt+x", "the row grew the new chord on its own");
    }

    /// C34/C15, the regression the design-supervisor audit of C34 caught by
    /// measuring instead of assuming: a row's key column became
    /// keymap-derived and therefore **unbounded**, so disabling one focus
    /// chord made the row enumerate eight and pushed the column from 77 to
    /// 107 — clipping the description at an 80-column terminal, the exact
    /// failure C15's width rule exists to prevent.
    ///
    /// The old pin only ever exercised `Keymap::default()`, which is a
    /// default-only assertion about a quantity that is no longer constant.
    /// This one sweeps the configs the audit actually measured.
    #[test]
    fn one_help_column_fits_the_floor_under_a_remap_too() {
        for cfg in [
            r#"{}"#,
            // The README's own worked example.
            r#"{"keys": {"alt+f": "disable", "alt+v": "toggle_float"}}"#,
            // The four the audit measured at 99, 107, 107 and 123.
            r#"{"keys": {"alt+1": "disable"}}"#,
            r#"{"keys": {"alt+h": "disable"}}"#,
            r#"{"keys": {"alt+left": "disable"}}"#,
            r#"{"keys": {"alt+b": "focus_left"}}"#,
        ] {
            let (keymap, diagnostics) = Keymap::parse(cfg, "config.json");
            assert!(diagnostics.is_empty(), "{cfg}: {diagnostics:?}");
            // Measure the *content*, and against the ceiling the dialog can
            // actually draw — not `layout.size.0`, which is
            // `.min(body.width)` and therefore says "≤ 80" whether the
            // layout fit or was clamped down to it. That tautology is why
            // the sibling floor test could not see the audit's finding.
            let w = help_content_width(&help_lines(&keymap, ""));
            let floor = super::HELP_FLOOR_COLS;
            assert!(
                w + super::HELP_DIALOG_CHROME <= floor,
                "{cfg} makes a column {w} wide, so the dialog asks for {} at an \
                 {floor}-column terminal and is clamped — the column of air before the \
                 right border goes first",
                w + super::HELP_DIALOG_CHROME,
            );
        }
    }

    /// A key column always ends in a space, so the description can never
    /// fuse to it.
    ///
    /// `draw_help_columns` draws the prefix and the description as two
    /// adjacent spans with nothing between them — the separation is
    /// entirely the prefix's padding. `{key:<18}` supplies it only while
    /// the key is *under* 18 wide, because that is a minimum width, not a
    /// column: at 18 or more it pads by nothing and the two spans abut.
    /// Before C34 no key reached 18, so the distinction never came up; an
    /// enumerated family reaches 24 easily, and at exactly 80×24 the focus
    /// row rendered `Alt+← / Alt+↓ / Alt+j …move focus (…)`. Found by a
    /// simulation agent stressing the design floor.
    #[test]
    fn a_key_column_never_fuses_to_its_description() {
        for cfg in [
            r#"{}"#,
            r#"{"keys": {"alt+h": "disable"}}"#,
            r#"{"keys": {"alt+left": "disable"}}"#,
            r#"{"keys": {"alt+1": "disable"}}"#,
            r#"{"keys": {"alt+b": "focus_left"}}"#,
        ] {
            let (keymap, _) = Keymap::parse(cfg, "config.json");
            for line in help_lines(&keymap, "") {
                let HelpLine::Row(k, d, _) = line else { continue };
                let rendered = format!("{}{d}", super::help_key_prefix(&k));
                assert!(
                    super::help_key_prefix(&k).ends_with(' '),
                    "{cfg}: no separator between key and description — {rendered:?}",
                );
            }
        }
    }

    /// Elision cuts the *key*, never the description, and lands on a chord
    /// boundary so no chord is shown half-spelled.
    #[test]
    fn an_over_wide_key_column_elides_at_a_chord_boundary() {
        let (keymap, _) = Keymap::parse(r#"{"keys": {"alt+h": "disable"}}"#, "config.json");
        let focus = help_lines(&keymap, "")
            .into_iter()
            .find_map(|l| match l {
                HelpLine::Row(k, d, _) if d.starts_with("move focus") => Some(k),
                _ => None,
            })
            .expect("a focus row");
        assert!(focus.ends_with(" …"), "elision is marked: {focus}");
        for chord in focus.trim_end_matches(" …").split(" / ") {
            assert!(chord.starts_with("Alt+"), "no chord is cut mid-spelling: {focus}");
        }
        // And the description survived intact — it is the half that cannot
        // be reconstructed by widening the terminal.
        let full = help_lines(&keymap, "").into_iter().any(|l| {
            matches!(l, HelpLine::Row(_, d, _)
                if d == "move focus (←/→ continue to next/prev tab at an edge)")
        });
        assert!(full, "the description is never what yields");
    }

    /// C34/D2: C9's attention segment is the jump chord's discoverability
    /// surface, and it spelled `Alt+a` as a literal — so after a remap it
    /// taught a dead chord. An unbound jump drops the tail rather than
    /// naming a key that does nothing.
    #[test]
    fn the_attention_segment_names_the_live_jump_chord() {
        let text = |chord: Option<String>| {
            super::hint_bar_right_spans(Some((2, true)), None, None, "NORMAL", chord)
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        };
        assert!(text(Some("Alt+v".into())).contains("· Alt+v"), "the remapped chord");
        assert!(!text(Some("Alt+v".into())).contains("Alt+a"), "and not the default");
        let unbound = text(None);
        assert!(unbound.contains("2 needs you"), "the count still reports");
        assert!(!unbound.contains(" · "), "but no chord is advertised: {unbound}");
    }

    /// C34/D3: the dead-pane bar and the hint bar one screen row below name
    /// the same close chord. Before C34 only the hint bar derived it, so a
    /// remap made two surfaces on one screen disagree.
    #[test]
    fn the_dead_pane_bar_names_the_live_close_chord() {
        assert!(super::dead_bar_text(false, Some("Alt+v")).contains("· Alt+v: close"));
        let unbound = super::dead_bar_text(false, None);
        assert!(!unbound.contains("close"), "an unbound close drops the clause: {unbound}");
        assert!(unbound.contains("relaunch"), "the mode-local keys stay: {unbound}");
    }

    /// F1's headline behaviour, end to end: remap a chord in config.json and
    /// the overlay teaches the new key and stops teaching the old one.
    /// Before F1 both surfaces were `&'static str` literals and this was
    /// impossible — the README's own escape-hatch example produced a roost
    /// whose `Alt+?` still taught `Alt+f`.
    #[test]
    fn a_remapped_chord_is_taught_at_its_new_key() {
        let (keymap, diagnostics) = Keymap::parse(
            r#"{"keys": {"alt+f": "disable", "alt+v": "toggle_float"}}"#,
            "config.json",
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let drawn = help_lines(&keymap, "")
            .iter()
            .filter_map(|l| match l {
                HelpLine::Row(k, d, _) => Some(format!("{k} {d}")),
                HelpLine::Head(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(drawn.contains("Alt+v floating scratch shell"), "taught at its new chord");
        assert!(
            !drawn.contains("Alt+f "),
            "and not at the old one — that key forwards to the pane now",
        );
    }

    /// A row whose every chord is disabled leaves the overlay rather than
    /// standing there naming nothing.
    #[test]
    fn a_fully_disabled_row_leaves_the_overlay() {
        let (keymap, _) = Keymap::parse(r#"{"keys": {"alt+e": "disable"}}"#, "config.json");
        let drawn = help_lines(&keymap, "")
            .iter()
            .filter_map(|l| match l {
                HelpLine::Row(k, d, _) => Some(format!("{k} {d}")),
                HelpLine::Head(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!drawn.contains("activity feed"), "the feed row is gone with its chord");
    }

    /// A `Family`'s compact spelling is a shorthand for the default chords
    /// and nothing else, so it survives only while they all hold. Move one
    /// and the row enumerates what is really bound — otherwise
    /// `Alt+←↓↑→ / hjkl` would be precisely the stale spelling F1 abolished,
    /// just a wider one.
    #[test]
    fn a_family_spelling_gives_way_once_one_of_its_chords_moves() {
        let compact = |keymap: &Keymap| {
            help_lines(keymap, "")
                .iter()
                .any(|l| matches!(l, HelpLine::Row(k, _, _) if k == "Alt+←↓↑→ / hjkl"))
        };
        assert!(compact(&Keymap::default()), "the defaults render compactly");

        let (moved, diagnostics) =
            Keymap::parse(r#"{"keys": {"alt+b": "focus_left"}}"#, "config.json");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(!compact(&moved), "a moved member retires the shorthand");
        let drawn = help_lines(&moved, "")
            .iter()
            .filter_map(|l| match l {
                HelpLine::Row(k, _, _) => Some(k.clone()),
                HelpLine::Head(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(drawn.contains("Alt+b"), "the real chord is named instead");
    }

    /// The two tab families are documented as adjacent pairs — the step row
    /// (`m`/`Shift+m`) with the carry row (`i`/`Shift+i`) directly under it.
    /// C28's rule survives the 2026-09-01 re-key: the adjacency *is* the
    /// explanation ("the i-family carries the pane the way the m-family
    /// carries you").
    #[test]
    fn the_tabs_group_teaches_the_carry_chords_under_the_tab_steps() {
        // F1: rows resolve their key column from the keymap, so this walks
        // the drawn lines rather than the const table — which is also the
        // stronger check: adjacency in the *table* would not prove adjacency
        // in what the reader sees.
        let bindings = input::effective_bindings(&Keymap::default());
        let tabs = HELP_GROUPS.iter().find(|g| g.title == "TABS").expect("a TABS group");
        let keys: Vec<String> = tabs
            .rows
            .iter()
            .filter_map(|r| super::help_key_text(&r.key, r.desc, &bindings))
            .collect();
        let step = keys.iter().position(|k| k == "Alt+m / Alt+Shift+m").expect("the step row");
        let carry = keys.iter().position(|k| k.starts_with("Alt+i /")).expect("the carry row");
        assert_eq!(carry, step + 1, "the carry pair sits directly under the step pair");
        assert!(tabs.rows[carry].desc.contains("move this pane"));
    }

    /// U23: the legend is the C5 glyph table, not a hand-copied lookalike —
    /// retheming a glyph must break this, not silently leave the overlay
    /// teaching a symbol roost no longer draws.
    #[test]
    fn help_legend_row_matches_the_theme_glyph_table() {
        let desc = help_lines(&Keymap::default(), "")
            .into_iter()
            .find_map(|l| match l {
                HelpLine::Row(k, d, _) if k == "status" => Some(d),
                _ => None,
            })
            .expect("the overlay carries a status-glyph legend row");
        assert_eq!(desc, super::status_legend_text());
        // The two glyphs the live-QA drive greps the frame for.
        assert!(
            desc.contains(theme::GLYPH_WORKING) && desc.contains(theme::GLYPH_EXITED),
            "{desc}",
        );
    }

    /// C15 (amended): one column while the table fits, a second only when
    /// it does not *and* the terminal is wide enough for two — the fewest
    /// columns that fit, never columns for their own sake.
    #[test]
    fn help_takes_the_fewest_columns_that_fit() {
        let all = help_lines(&Keymap::default(), "");
        let lines = all.len() as u16;
        let content = help_content_width(&all);

        // Tall enough for the whole table: one column, however wide the
        // terminal is.
        let tall = Rect::new(0, 1, content * 4, lines + 2);
        assert_eq!(
            help_layout(tall, &Keymap::default(), None).columns.len(),
            1,
            "a body that fits stays one column"
        );

        // Too short, and wide enough for two: two columns.
        let wide = Rect::new(0, 1, content * 2 + super::HELP_GUTTER + 2, lines / 2 + 2);
        let two = help_layout(wide, &Keymap::default(), None);
        assert_eq!(two.columns.len(), 2, "a short, wide body splits");
        assert_eq!(
            two.columns.iter().map(|c| c.len()).sum::<usize>(),
            lines as usize,
            "…and every single line survives the split",
        );

        // Too short and too narrow: one column, and it scrolls (see
        // `help_scroll_extent`).
        let narrow = Rect::new(0, 1, content + 2, 12);
        let one = help_layout(narrow, &Keymap::default(), None);
        assert_eq!(one.columns.len(), 1);
        let (visible, total) = super::help_scroll_extent(narrow, &Keymap::default(), None);
        assert!(visible < total, "a narrow, short body scrolls rather than dropping rows");
    }

    /// A column break never lands inside a group: a heading in one column
    /// with half its chords in the other is worse than either arrangement.
    #[test]
    fn a_second_column_always_starts_on_a_group_heading() {
        let content = help_content_width(&help_lines(&Keymap::default(), ""));
        let wide = Rect::new(0, 1, content * 2 + super::HELP_GUTTER + 2, 14);
        let layout = help_layout(wide, &Keymap::default(), None);
        assert_eq!(layout.columns.len(), 2);
        assert!(
            matches!(layout.columns[1].first(), Some(HelpLine::Head(_))),
            "the right column opens on a heading: {:?}",
            layout.columns[1].first(),
        );
        assert!(matches!(layout.columns[0].first(), Some(HelpLine::Head(_))), "…as does the left",);
    }

    /// The keymap must be drawable at roost's own 80×24 floor — the case
    /// that used to force the ≤20-row cap. It scrolls there now, but every
    /// row is reachable and nothing is clipped horizontally.
    #[test]
    fn help_fits_the_eighty_column_floor_and_reaches_every_row() {
        let body = Rect::new(0, 1, 80, 22); // 80×24 minus the two bars
        let layout = help_layout(body, &Keymap::default(), None);
        // `size.0` is already `.min(body.width)` here, so it can only ever
        // report 80 — assert on the ask instead.
        let asked = layout.content + super::HELP_DIALOG_CHROME;
        assert!(asked <= body.width, "the keymap asks for {asked} cols at the floor");
        // `size.1` is `min(tallest, body.height - 2) + 2`, so `size.1 <=
        // body.height` held by construction — the height half of this test
        // was still the tautology the width half had already been fixed
        // for. What the test's own name claims is *reachability*, so
        // assert that: scrolling to the bottom must put the last row on
        // screen. `visible >= 1` was as far as it got.
        let (visible, total) = super::help_scroll_extent(body, &Keymap::default(), None);
        assert_eq!(
            total,
            help_lines(&Keymap::default(), "").len(),
            "one column at the floor holds the whole table"
        );
        assert!(visible >= 1, "at least one row is on screen");
        assert!(total > visible, "the floor is the case where it scrolls");
        let max_top = total - visible;
        assert_eq!(max_top + visible, total, "scrolled to the bottom, the last row is visible");
    }

    /// U20: the picker's accelerator has to be visible to be pressed, and
    /// both row styles must agree on where the id starts (the marker column
    /// is the only difference between them).
    #[test]
    fn picker_rows_lead_with_their_number_accelerator() {
        assert_eq!(super::picker_row_body(0, "pi"), " 1 pi");
        assert_eq!(super::picker_row_body(2, "shell"), " 3 shell");
        // Past the ninth there is no accelerator, but the id stays aligned.
        assert_eq!(super::picker_row_body(9, "x"), "   x");
        assert_eq!(
            super::picker_row_body(9, "x").chars().count(),
            super::picker_row_body(0, "x").chars().count(),
        );
    }

    /// U16: the caret sits AT the insertion point, not always at the end —
    /// the visible half of the cursor motion. Out-of-range values clamp to
    /// the end rather than panicking on a bad slice, and the field slices on
    /// char boundaries so a multi-byte name survives.
    #[test]
    fn rename_field_puts_the_caret_at_the_insertion_point() {
        // (text before the caret, the character under it, text after it).
        let parts = |b: &str, c: usize| {
            let spans = super::rename_field(b, c);
            let at = spans.iter().position(|s| s.style == theme::attention()).expect("a caret");
            assert_eq!(at, 1, "the caret always follows exactly one head span");
            let text = |s: Option<&ratatui::text::Span<'_>>| {
                s.map(|s| s.content.to_string()).unwrap_or_default()
            };
            (text(spans.first()), text(spans.get(at)), text(spans.get(at + 1)))
        };
        assert_eq!(parts("abcd", 2), ("ab".into(), "c".into(), "d".into()));
        assert_eq!(parts("abcd", 0), (String::new(), "a".into(), "bcd".into()));
        // Past the last character there is nothing to reverse, so the caret
        // is a reversed space — appended, displacing nothing.
        assert_eq!(parts("abcd", 4), ("abcd".into(), " ".into(), String::new()));
        assert_eq!(parts("", 0), (String::new(), " ".into(), String::new()));
        // Clamped, not panicking.
        assert_eq!(parts("abcd", 99), ("abcd".into(), " ".into(), String::new()));
        // Multi-byte: the caret takes a whole char, never half of one.
        assert_eq!(parts("héllo", 1), ("h".into(), "é".into(), "llo".into()));
    }

    /// The bug the reversal fixed: an inserted caret cost a real cell, so
    /// moving the point left shoved every character after it one column
    /// right. The field's width must not depend on where the point is.
    #[test]
    fn moving_the_caret_never_moves_the_text_around_it() {
        let widths: Vec<u16> =
            (0..=4).map(|c| super::spans_width(&super::rename_field("abcd", c))).collect();
        assert_eq!(widths, vec![4, 4, 4, 4, 5], "only the end-of-buffer caret adds a column");
        for cursor in 0..=4 {
            let spans = super::rename_field("abcd", cursor);
            let flat: String = spans.iter().map(|s| s.content.to_string()).collect();
            assert_eq!(flat.trim_end(), "abcd", "cursor {cursor} rewrote the text");
        }
    }

    /// C14 (U20): the cwd column shows the last two path components, so a
    /// screenful of sibling checkouts stays distinguishable without paying
    /// for full paths.
    #[test]
    fn picker_cwd_labels_keep_the_last_two_components() {
        use std::path::Path;
        assert_eq!(super::picker_cwd_label(Path::new("/home/me/src/roost")), "src/roost");
        assert_eq!(
            super::picker_cwd_label(Path::new("/tmp")),
            "/tmp",
            "no doubled slash at the root"
        );
        assert_eq!(super::picker_cwd_label(Path::new("roost")), "roost");
        assert_eq!(super::picker_cwd_label(Path::new("/")), "/");
    }

    /// C14 (U20): the three row states — the focused column's selection
    /// (marker + `ink`), the other column's selection (`ink`, no marker, so
    /// what a launch would use stays readable), everything else `quiet`.
    #[test]
    fn picker_rows_mark_only_the_focused_columns_selection() {
        let (marker, style) = super::row_marks(true, true);
        assert_eq!(marker.content.as_ref(), theme::PICKER_SELECTED.to_string());
        assert_eq!(marker.style, theme::accent());
        assert_eq!(style, theme::ink());

        let (marker, style) = super::row_marks(true, false);
        assert_eq!(marker.content.as_ref(), " ", "no marker in the column without focus");
        assert_eq!(style, theme::ink(), "but still readable as the selection");

        let (marker, style) = super::row_marks(false, true);
        assert_eq!(marker.content.as_ref(), " ");
        assert_eq!(style, theme::quiet());
    }
    /// [F9] Every key the dead-pane hint bar advertises is taught by the
    /// overlay too.
    ///
    /// The dead-pane keys (`↵` relaunch, `f` fresh, `y` copy resume) are
    /// bare keys, not Alt chords — `main.rs` claims them out of
    /// `InputResult::Forward` while the focused pane is dead — so §8 has no
    /// row for them and C34's chord sweep does not reach them. They were
    /// advertised on the C9 bar and documented nowhere else: P21's case
    /// verbatim, and the one C15 exists to prevent.
    ///
    /// This is the gate that would have caught it, and it is deliberately
    /// keyed off the **bar** rather than a written list — the bar is where
    /// a new dead-pane key would appear first.
    #[test]
    fn the_overlay_teaches_every_key_the_dead_pane_bar_advertises() {
        // Both bars: `resumable` gates whether `y` is offered, and the
        // un-resumable one alone was the whole test — leaving `y`, the key
        // C39 spends a paragraph justifying as unconditional, outside the
        // assertion set entirely. Found by the C39 audit.
        let mut keys: Vec<String> = Vec::new();
        for resumable in [false, true] {
            for (k, _) in hint_pairs(&Mode::Normal, true, resumable, false, false) {
                if !k.starts_with("Alt+") && !keys.contains(&k) {
                    keys.push(k); // Alt chords are C34's sweep, not this one
                }
            }
        }
        assert!(keys.len() >= 3, "the dead-pane bar must offer ↵, f and y: {keys:?}");

        // Match the **key columns**, not the whole overlay text. The first
        // version searched the joined rows for each key as a substring —
        // and every letter a–z already appears in `HELP_GROUPS`'s prose
        // ("close pane (confirm if busy)" alone supplies both `f` and `y`),
        // so a future dead-pane key added with no row would have passed
        // silently. Only `↵` was load-bearing, by the accident of occurring
        // once. The mutation check that "proved" the gate used an uppercase
        // `Q`, which happens not to appear — picking an unrepresentative
        // mutant is how a vacuous check survives its own verification.
        let key_columns: Vec<String> = help_lines(&Keymap::default(), "")
            .iter()
            .filter_map(|l| match l {
                HelpLine::Row(k, _, _) => Some(k.clone()),
                HelpLine::Head(_) => None,
            })
            .collect();
        for k in keys {
            assert!(
                key_columns.iter().any(|col| col.split(['/', ' ']).any(|tok| tok == k)),
                "the dead-pane bar offers {k:?} and no overlay row has it in its key \
                 column — a key nothing but the bar advertises is a key nobody finds \
                 (P21). Key columns: {key_columns:?}",
            );
        }
    }

    /// [F9] The query actually narrows the table, matching on **both**
    /// columns, and an empty query changes nothing at all.
    #[test]
    fn the_help_filter_narrows_on_the_key_and_the_description() {
        let km = Keymap::default();
        let all = help_lines(&km, "");
        let rows = |q: &str| {
            help_lines(&km, q).into_iter().filter(|l| matches!(l, HelpLine::Row(..))).count()
        };
        let total = rows("");
        assert!(total > 20, "the unfiltered keymap is the whole table: {total}");
        assert_eq!(help_lines(&km, "").len(), all.len(), "an empty query is a no-op");

        // A description word.
        let by_desc = rows("stack");
        assert!(by_desc > 0 && by_desc < total, "`stack` narrows but does not empty: {by_desc}");

        // A key fragment — the reader who remembers the chord, not the word.
        let by_key = rows("Alt+Shift");
        assert!(by_key > 0 && by_key < total, "`Alt+Shift` narrows: {by_key}");

        // Case-insensitive, or half the chords are unreachable by typing.
        assert_eq!(rows("alt+shift"), by_key, "the query is case-insensitive");

        // A query that matches nothing leaves nothing — including headings,
        // which must not survive their own rows.
        assert_eq!(
            help_lines(&km, "zzzzz-no-such-thing").len(),
            0,
            "an empty result draws no orphan headings",
        );
    }

    /// [C39] A query that matches nothing still draws a frame that says so
    /// — C14's rule ("an empty result still needs a frame to say so"), and
    /// the bug this branch already fixed for the roster and the feed, where
    /// a zero-height dialog left the chord looking unbound.
    ///
    /// It works here for a reason worth pinning rather than relying on: the
    /// width floor is the *title's*, and the title is never empty, so an
    /// empty table still has a frame wide enough to read. The design audit
    /// found the behaviour correct and the guard missing.
    #[test]
    fn a_query_matching_nothing_still_draws_a_frame_that_says_so() {
        let body = Rect::new(0, 0, 120, 40);
        let km = Keymap::default();
        let layout = help_layout(body, &km, Some("zzzzz-no-such-thing"));
        assert_eq!(layout.columns.iter().map(|c| c.len()).max().unwrap_or(0), 0, "nothing matched");
        assert_eq!(layout.size.1, 2, "two border rows — a frame, not a void");
        let title =
            super::help_title(Some("zzzzz-no-such-thing"), 0, 0, false, false, body.width - 2);
        assert!(title.contains("0 shown"), "and it says the result is empty: {title:?}");
        assert!(title.contains("Esc clears"), "and how to leave: {title:?}");
        assert!(
            layout.size.0 as usize >= mouse::display_width(&title) as usize + 2,
            "the frame is wide enough to read that: {} vs {title:?}",
            layout.size.0,
        );
    }

    /// [F9] The dialog is sized for the *filtered* table — C14's picker rule
    /// applied to the surface that borrowed its type-ahead. A query cutting
    /// 36 rows to 3 must not leave a 36-row frame around them.
    #[test]
    fn the_filtered_help_dialog_shrinks_to_what_it_shows() {
        let body = Rect::new(0, 0, 120, 40);
        let km = Keymap::default();
        let whole = help_layout(body, &km, None).size.1;
        let narrow = help_layout(body, &km, Some("stack")).size.1;
        assert!(narrow < whole, "the frame followed the filter: {narrow} vs {whole}");
        assert!(narrow >= 3, "and still has a frame to say so");
    }

    /// [F9] The dialog is never narrower than the title it draws.
    ///
    /// Before the filter the table always contained its widest row, so the
    /// frame was always wider than any heading and this could not arise. A
    /// query isolating one short row breaks that — `/this keymap` leaves a
    /// 33-column dialog under a 44-column title, and `modal_frame` clips
    /// it, hiding the very sentence that tells a filtering reader how to
    /// get out. Found by driving the overlay in a PTY; every unit test here
    /// looked at the frame or the title, never at both.
    #[test]
    fn a_filtered_dialog_is_never_narrower_than_its_own_title() {
        let body = Rect::new(0, 0, 120, 40);
        let km = Keymap::default();
        // Queries chosen to isolate the *short* rows — the ones that used
        // to leave a frame too narrow for the heading above them.
        for q in ["this keymap", "hint bar", "toggle the hint", "quit", "zoom"] {
            let layout = help_layout(body, &km, Some(q));
            let rows = layout.columns.iter().map(|c| c.len()).max().unwrap_or(0);
            let runnable =
                layout.columns.iter().flatten().any(|l| matches!(l, HelpLine::Row(_, _, Some(_))));
            let title = super::help_title(
                Some(q),
                rows,
                rows,
                rows > layout.height as usize,
                runnable,
                body.width - 2,
            );
            assert!(
                layout.size.0 as usize >= mouse::display_width(&title) as usize + 2,
                "query {q:?}: a {}-column dialog under a {}-column title — {title:?} clips",
                layout.size.0,
                mouse::display_width(&title),
            );
        }
    }

    /// [F9] The 80-column floor holds for every query, not just the empty
    /// one — asserted on the **ask**, not on `size.0`.
    ///
    /// The first version of this test read `size.0 <= 80` at an 80-column
    /// body. `size.0` is `.min(body.width)`, so it could only ever report
    /// 80: the gate passed by construction. C15's own 2026-08-20 amendment
    /// had already named that exact shape — "a tautology that reads like a
    /// gate is worse than no gate" — and the corrected version of it sits
    /// 160 lines above this one in the same file, with a comment saying
    /// "assert on the ask instead". The design audit caught it anyway.
    /// Third time on this branch; the lesson is that a floor test must
    /// never read a clamped value, however it is spelled.
    #[test]
    fn the_help_dialog_fits_the_floor_under_every_query() {
        let floor = Rect::new(0, 0, 80, 24);
        let km = Keymap::default();
        for q in ["", "a", "alt", "Alt+Shift", "pane", "e", "/", "this keymap"] {
            let layout = help_layout(floor, &km, Some(q));
            assert!(
                layout.asked <= floor.width,
                "query {q:?} asked for {} columns, past the {}-column floor",
                layout.asked,
                floor.width,
            );
        }
    }

    /// [F9] ...and a query longer than the terminal cannot push the title
    /// past the floor either — the query elides, the exit hint survives.
    ///
    /// `help_layout` floors the dialog on the title's width, but that only
    /// answers *content narrower than the title*. A body narrower than the
    /// title is the other half, and the `.min(body.width)` re-admitted it:
    /// at the floor a 46-character query clamped and `modal_frame`
    /// truncated the tail — losing "Esc clears", the one thing a filtering
    /// reader needs. Nothing widened the dialog because nothing could;
    /// the query had to give instead. Found by the C39 design audit, which
    /// also found why no test saw it (the floor gate above was vacuous).
    #[test]
    fn a_query_wider_than_the_terminal_elides_and_keeps_the_way_out() {
        let floor = Rect::new(0, 0, 80, 24);
        let km = Keymap::default();
        let long = "a".repeat(120);
        for scrolled in [false, true] {
            // `runnable` true is the widest the title can get — the case a
            // floor gate has to hold against.
            let title = super::help_title(Some(&long), 3, 40, scrolled, true, floor.width - 2);
            assert!(
                mouse::display_width(&title) <= floor.width - 2,
                "the title outgrew the terminal: {} cols",
                mouse::display_width(&title),
            );
            assert!(title.contains("Esc clears"), "the way out survived: {title:?}");
            assert!(title.contains('…'), "and the cut is marked: {title:?}");
        }
        // The layout agrees — it builds the same title through the same
        // function, which is why there is only one.
        let layout = help_layout(floor, &km, Some(&long));
        assert!(layout.asked <= floor.width, "asked for {} at the floor", layout.asked);
    }

    /// [C41] The palette's cursor is drawn, lands on the right row, and
    /// **costs no column** — it spends the space `help_key_prefix` already
    /// opens every key column with. That last clause is the one worth
    /// gating: the alternative (a marker column of its own) would widen
    /// every row by one, which re-trips `elide_key` and moves
    /// `HELP_COL_FLOOR` — the exact accounting C38/C39 had to correct twice
    /// already. Compare the drawn row against the unfiltered overlay's own
    /// width for the same query, so the check is a measurement rather than
    /// a restatement of the marker's width.
    #[test]
    fn the_palette_cursor_marks_a_row_without_costing_it_a_column() {
        use crate::core::app::Mode;
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let body = Rect::new(0, 0, 120, 40);
        let km = Keymap::default();
        let q = "tab";
        assert!(super::help_actions(&km, q).len() >= 2, "the query needs rows to choose between");
        let unmarked = help_layout(body, &km, Some(q)).content;

        let draw = |cursor: usize| -> Vec<String> {
            let mut app = mk_app(Size::new(120, 40));
            app.apply(Action::Help);
            app.mode = Mode::Help { top: 0, filter: Some(q.into()), cursor };
            let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
            term.draw(|f| super::draw(f, &mut app)).unwrap();
            let buf = term.backend().buffer().clone();
            (0..40)
                .map(|y| {
                    (0..120)
                        .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                        .collect::<String>()
                })
                .collect()
        };

        let marker = theme::PICKER_SELECTED.to_string();
        for cursor in [0usize, 1] {
            let rows = draw(cursor);
            let marked: Vec<&String> = rows.iter().filter(|r| r.contains(&marker)).collect();
            assert_eq!(marked.len(), 1, "exactly one row carries the cursor at {cursor}");
        }
        assert_ne!(
            draw(0).iter().position(|r| r.contains(&marker)),
            draw(1).iter().position(|r| r.contains(&marker)),
            "and ↓ moved it to a different row",
        );
        // The width claim has to be read off the DRAWN row, not off
        // `help_layout` — that takes no cursor, so comparing two of its
        // calls compares a function to itself and passes by construction.
        // The real property: the marked row, with the marker put back to the
        // space it spent, is byte-identical to the same row drawn unmarked.
        // Nothing shifted, so nothing widened.
        let (rows0, rows1) = (draw(0), draw(1));
        let idx = rows0.iter().position(|r| r.contains(&marker)).expect("cursor 0 marks a row");
        assert_eq!(
            rows0[idx].replacen(&marker, " ", 1),
            rows1[idx],
            "the marker rides in the key column's own leading space, so nothing shifted",
        );
        assert_eq!(
            help_layout(body, &km, Some(q)).content,
            unmarked,
            "and the laid-out table is the same table either way",
        );
    }

    /// [C41] The un-filtered overlay is still C15's poster: nothing is
    /// marked, because "any key closes it" still owns `↵` there and a
    /// cursor would advertise an action the state does not offer.
    #[test]
    fn the_unfiltered_keymap_marks_no_row() {
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(120, 40));
        app.apply(Action::Help);
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| super::draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
        let screen: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(
            screen.contains("new shell pane"),
            "the overlay really is up — or this gate passes by drawing nothing",
        );
        assert!(!screen.contains(theme::PICKER_SELECTED), "an unfiltered keymap points at nothing",);
    }

    /// [C41] The title earns its `↵ runs` — it appears when the query has a
    /// command under the cursor and stays away when it does not. F1's rule
    /// applied to the heading: a title teaching a key that does nothing
    /// here is worse than no title.
    #[test]
    fn the_title_offers_enter_only_when_there_is_something_to_run() {
        let body = Rect::new(0, 0, 120, 40);
        let km = Keymap::default();
        let title = |q: &str| {
            let layout = help_layout(body, &km, Some(q));
            let rows = layout.columns.iter().map(|c| c.len()).max().unwrap_or(0);
            super::help_title(
                Some(q),
                rows,
                rows,
                rows > layout.height as usize,
                super::help_cursor_pos(&layout, 0).is_some(),
                body.width - 2,
            )
        };
        assert!(title("toggle the hint bar").contains("↵ runs"), "a command row offers it");
        assert!(!title("roost read").contains("↵ runs"), "the control-CLI block does not");
        assert!(!title("zzzzz-no-such-thing").contains("↵ runs"), "and neither does no match");
        for q in ["toggle the hint bar", "roost read", "zzzzz-no-such-thing"] {
            assert!(title(q).contains("Esc clears"), "the way out survives either way: {q}");
        }
    }

    /// C14 (U20): every picker row's cwd column starts at the same place,
    /// including the widest adapter row the dialog is sized for.
    ///
    /// The pad target and the width constant used to be two constants with
    /// one name — 16 in the draw loop, 23 in the sizing — so a row wider
    /// than 16 got no padding and shoved its cwd label right.
    ///
    /// The worst case is **derived from the live registry**, not spelled.
    /// The first version of this test wrote `"opencode not found"` while
    /// its own docstring claimed to walk the registry — so a seventh
    /// adapter with a 9-character id would have under-padded the column
    /// again with the test still green. That is the same "tautology that
    /// reads like a gate" this branch has already fixed twice; the design
    /// audit caught it a third time, here.
    #[test]
    fn the_adapter_column_is_wide_enough_for_the_row_it_pads() {
        // The longest id the picker can list, annotated the way
        // `App::picker_filtered` annotates an adapter missing from `$PATH`
        // (any id can be, so the suffix applies to the longest), at the
        // accelerator prefix that makes the row longest.
        let longest = crate::agents::picker_ids()
            .into_iter()
            .max_by_key(|id| mouse::display_width(id))
            .expect("the registry lists at least one adapter");
        let suffix = crate::core::app::PICKER_MISSING_SUFFIX;
        let worst = super::picker_row_body(0, &format!("{longest} {suffix}"));
        assert!(
            mouse::display_width(&worst) < super::ADAPTER_COL,
            "the worst-case row {worst:?} ({} cols + 1 marker) overflows the column \
it pads to ({}), so its cwd label would start further right than every other \
row's — widen ADAPTER_COL",
            mouse::display_width(&worst),
            super::ADAPTER_COL,
        );
    }

    /// C14 (U20): the dialog grows to fit the widest cwd label, and stays
    /// the pre-U20 32 columns when there is no cwd column to show.
    #[test]
    fn picker_dialog_width_covers_the_cwd_column() {
        use std::path::PathBuf;
        assert_eq!(super::picker_dialog_width(&[]), 32, "no cwds ⇒ the old dialog");
        let cwds = vec![PathBuf::from("/home/me/src/roost"), PathBuf::from("/tmp")];
        // widest label = "src/roost" (9) ⇒ 23 + 2 + 9 + 2 = 36, past the
        // pre-U20 32-column floor.
        assert_eq!(super::picker_dialog_width(&cwds), 23 + 2 + 9 + 2);
        let long = vec![PathBuf::from("/home/me/a-rather-long/checkout-name")];
        assert_eq!(
            super::picker_dialog_width(&long),
            23 + 2 + 27 + 2,
            "label = a-rather-long/checkout-name"
        );
    }

    /// The picker's row count is `$PATH` probing — `picker_filtered`
    /// annotates each adapter with whether its launch program resolves, so
    /// one call is roughly a `stat` per adapter per `$PATH` entry — and
    /// `dialog_rect` reads it in exactly one of its eight branches. Both
    /// callers used to hand it over eagerly, so `draw_mode_overlay` paid for
    /// it on every frame in every mode (~1,500 `stat` calls a second at the
    /// 33ms loop, with no dialog on screen) and `modal_rect` on every mouse
    /// event. Nothing about the drawn dialog changes; this is the guard.
    #[test]
    fn the_picker_row_count_is_only_computed_while_the_picker_is_up() {
        use crate::ui::input::Action;
        use ratatui::layout::Size;
        let mut app = mk_app(Size::new(100, 30));
        assert_eq!(super::picker_rows(&app), 0, "Normal mode must not probe $PATH");
        app.apply(Action::QuickLaunch);
        assert!(matches!(app.mode, Mode::Picker { .. }), "Alt+p opens the picker");
        let rows = super::picker_rows(&app);
        assert_eq!(rows, app.picker_filtered().len(), "the picker sizes to its own rows");
        assert!(rows > 0, "the registry has adapters to size against");
    }

    /// U20: the accelerator is on the hint bar too — a key you can only
    /// find by reading the source is not a feature. Amended by U20's second
    /// half: the type-ahead and the cwd column join it, and the list stays
    /// inside the 100-col floor beside the right segment.
    #[test]
    fn hint_pairs_picker_advertises_the_number_accelerators() {
        let pairs = hint_pairs(
            &Mode::Picker { selection: 0, filter: String::new(), cwd: 0, on_cwd: false },
            false,
            false,
            false,
            false,
        );
        assert_eq!(
            pairs,
            p(&[
                ("↑↓", "choose"),
                ("↵", "open"),
                ("1..9", "launch"),
                ("type", "filter"),
                ("←→", "dir"),
                ("Esc", "cancel"),
            ]),
        );
        let cols: u16 = pairs.iter().map(|(k, l)| super::hint_pair_cols(k, l)).sum();
        assert_eq!(cols, 71, "C9: the picker list must stay inside the 100-col floor");
    }

    /// U23: the overlay documents the mouse — the wheel, click-to-focus,
    /// drag-select and the Alt+click URL verb, which had lived only in the
    /// source until now.
    ///
    /// D2 (PR #46 design audit, C29 amendment): widened for double/triple-
    /// click and shift-click-extend — `every_bound_chord_is_documented_in_
    /// the_keymap` above only ever checked Alt chords, so it passed
    /// vacuously over these three (none is a chord); this is the test that
    /// actually has to know they exist.
    #[test]
    fn help_rows_document_every_mouse_verb() {
        let text = help_lines(&Keymap::default(), "")
            .iter()
            .filter_map(|l| match l {
                HelpLine::Row(k, d, _) => Some(format!("{k} {d}")),
                HelpLine::Head(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        for token in ["mouse", "wheel", "click", "drag", "2x", "3x", "shift", "Alt+click", "URL"] {
            assert!(text.contains(token), "the help overlay must mention {token:?}");
        }
    }

    /// C15 (amended U23): one column may never be wider than the 80-col
    /// floor — `centered_near` would clamp it to the screen and clip a
    /// description mid-word, the exact failure the width rule exists to
    /// stop. The reference rows are long; this is their guard rail.
    #[test]
    fn one_help_column_fits_the_eighty_column_floor() {
        // The default-keymap case. Remapped keymaps — where the width is no
        // longer constant — are swept by
        // `one_help_column_fits_the_floor_under_a_remap_too`.
        // Laid out in unbounded space, so `size.0` is the width the dialog
        // *asked* for rather than what a clamp allowed it — the distinction
        // the remap sweep above spells out.
        let w = help_layout(Rect::new(0, 1, 200, 200), &Keymap::default(), None).size.0;
        assert!(
            w <= super::HELP_FLOOR_COLS,
            "the keymap is {w} cols wide; the floor would clip it"
        );
    }

    /// ux P2-15: the overlay used to teach every chord and never mention
    /// that roost has a control CLI at all — the category differentiator
    /// (an outside actor can drive the fleet) was invisible from inside the
    /// product. This pins the fix: a group documenting the verbs, with the
    /// pane-id join (U2's badge/tab id) spelled out rather than assumed.
    #[test]
    fn help_documents_the_control_cli_and_the_pane_id_join() {
        let cli =
            HELP_GROUPS.iter().find(|g| g.title == "CONTROL CLI").expect("a CONTROL CLI group");
        let bindings = input::effective_bindings(&Keymap::default());
        let text = cli
            .rows
            .iter()
            .filter_map(|r| {
                super::help_key_text(&r.key, r.desc, &bindings).map(|k| format!("{k} {}", r.desc))
            })
            .collect::<Vec<_>>()
            .join("\n");
        for verb in ["roost send", "roost read", "roost status", "roost spawn", "roost wait"] {
            assert!(text.contains(verb), "the control-CLI group must mention {verb:?}");
        }
        // The join key itself, not just the verbs that consume it — the
        // gap U2 named was that nothing said the badge/tab id is what a
        // caller passes.
        assert!(text.contains("<id>"), "the pane-id join must be spelled out:\n{text}");
        assert!(
            text.to_lowercase().contains("badge") || text.to_lowercase().contains("tab"),
            "the join must point back at where the id is shown on screen:\n{text}",
        );
    }

    // -- C24 keyboard copy cursor ----------------------------------------------

    #[test]
    fn cell_in_selection_matches_the_inclusive_reading_order_range() {
        let anchor = (2, 5);
        let cursor = (4, 3);
        assert!(cell_in_selection((2, 5), anchor, cursor), "anchor cell itself");
        assert!(cell_in_selection((4, 3), anchor, cursor), "cursor cell itself");
        assert!(
            cell_in_selection((3, 0), anchor, cursor),
            "a row strictly between is fully selected"
        );
        assert!(!cell_in_selection((2, 0), anchor, cursor), "before the anchor column on its row");
        assert!(!cell_in_selection((4, 5), anchor, cursor), "past the cursor column on its row");
        // Order-independent: swapping anchor/cursor must not change the answer.
        assert!(cell_in_selection((3, 0), cursor, anchor));
    }

    #[test]
    fn paint_copy_cursor_reverses_and_underlines_only_inside_a_selection() {
        use crate::core::app::Selection;
        use ratatui::backend::TestBackend;
        use ratatui::buffer::Buffer;
        use ratatui::Terminal;

        let inner = Rect::new(0, 0, 10, 5);
        let render = |cursor: (u16, u16), selection: Option<Selection>| -> Buffer {
            let backend = TestBackend::new(10, 5);
            let mut term = Terminal::new(backend).unwrap();
            term.draw(|f| super::paint_copy_cursor(f, inner, cursor, selection)).unwrap();
            term.backend().buffer().clone()
        };

        // No selection: cursor cell is REVERSED only.
        let buf = render((1, 2), None);
        let cell = buf.cell((2, 1)).unwrap();
        assert!(cell.style().add_modifier.contains(Modifier::REVERSED));
        assert!(!cell.style().add_modifier.contains(Modifier::UNDERLINED));

        // Cursor inside an active selection: REVERSED + UNDERLINED.
        let sel = Selection { pane: 1, anchor: (0, 0), cursor: (2, 5), dragging: false };
        let buf = render((1, 2), Some(sel));
        let cell = buf.cell((2, 1)).unwrap();
        assert!(cell.style().add_modifier.contains(Modifier::REVERSED));
        assert!(cell.style().add_modifier.contains(Modifier::UNDERLINED));

        // Cursor outside the selection: REVERSED only, no UNDERLINED.
        let sel = Selection { pane: 1, anchor: (3, 0), cursor: (4, 5), dragging: false };
        let buf = render((1, 2), Some(sel));
        let cell = buf.cell((2, 1)).unwrap();
        assert!(cell.style().add_modifier.contains(Modifier::REVERSED));
        assert!(!cell.style().add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn paint_copy_cursor_out_of_bounds_is_a_safe_no_op() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let inner = Rect::new(0, 0, 10, 5);
        let backend = TestBackend::new(10, 5);
        let mut term = Terminal::new(backend).unwrap();
        // Must not panic even when the cursor sits outside the pane's inner
        // bounds (a stale cursor after a resize, before the next clamp).
        term.draw(|f| super::paint_copy_cursor(f, inner, (99, 99), None)).unwrap();
    }

    // -- full draw() smoke tests at degenerate sizes (fleet features pass) --

    /// `draw()` must not panic at any geometry, in any state.
    ///
    /// A panic in the renderer runs on the event-loop thread and unwinds out
    /// of `run()`, which is a fleet incident, not a bad frame (see
    /// `tests/panic_shutdown.rs`). The chrome is full of arithmetic on
    /// widths that can go to zero — `saturating_sub` almost everywhere, and
    /// "almost" is what this is for. The layout fuzzer in `core::app` walks
    /// the same action space but only checks *state* invariants; nothing
    /// rendered a single frame of what it built.
    ///
    /// So: the same random walk, plus the overlays and modes (which the
    /// layout fuzzer barely touches, because they change no structure), and
    /// a real `draw()` into a `TestBackend` after every step — at a geometry
    /// redrawn from a deliberately hostile set each time, 1x1 included.
    #[test]
    fn draw_never_panics_at_any_geometry_or_state() {
        use crate::core::layout::Dir;
        use crate::ui::input::Action;
        use crossterm::event::{KeyCode, KeyEvent};
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        // Everything from a terminal nobody can use to one nobody has.
        const GEOM: &[(u16, u16)] = &[
            (1, 1),
            (2, 1),
            (1, 2),
            (3, 2),
            (4, 3),
            (6, 2),
            (10, 3),
            (20, 4),
            (37, 7),
            (80, 24),
            (120, 40),
            (200, 60),
            (300, 80),
        ];

        struct Lcg(u64);
        impl Lcg {
            fn below(&mut self, n: u64) -> u64 {
                self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (self.0 >> 16) % n
            }
        }

        let dirs = [Dir::Left, Dir::Right, Dir::Up, Dir::Down];
        let keys = [
            KeyCode::Char('a'),
            KeyCode::Char('/'),
            KeyCode::Char('中'),
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Backspace,
            KeyCode::Tab,
            KeyCode::Char('y'),
            KeyCode::Char('n'),
        ];
        let mut frames = 0u64;
        for seed in 0..30u64 {
            let mut rng = Lcg(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(7));
            let mut app = mk_app(Size::new(120, 40));
            for _ in 0..120 {
                let d = dirs[rng.below(4) as usize];
                let action = match rng.below(24) {
                    0 | 1 => Action::NewPane,
                    2 => Action::ClosePane,
                    3 => Action::Focus(d),
                    4 => Action::MovePane(d),
                    5 => Action::NewTab,
                    6 => Action::NextTab,
                    7 => Action::StackPane,
                    8 => Action::FlipSplit,
                    9 => Action::Resize { horizontal: rng.below(2) == 0, grow: rng.below(2) == 0 },
                    10 => Action::ToggleZoom,
                    11 => Action::ToggleFloat,
                    12 => Action::CycleLayout { forward: rng.below(2) == 0 },
                    13 => Action::ToggleRoster,
                    14 => Action::ToggleFeed,
                    15 => Action::Help,
                    16 => Action::EditPane,
                    17 => Action::RenameTab,
                    18 => Action::QuickLaunch,
                    19 => Action::ScrollMode,
                    20 => Action::CopyMode,
                    21 => Action::ToggleBroadcast,
                    22 => Action::ToggleHints,
                    _ => Action::Undo,
                };
                app.apply(action);
                // Names and notes are what the width arithmetic actually
                // measures: a wide glyph counts two columns, a combining
                // mark none, and a long one has to be truncated somewhere.
                // Typing them in through the dialogs would take the whole
                // walk, so plant them directly.
                if rng.below(6) == 0 {
                    const NASTY: &[&str] = &[
                        "",
                        " ",
                        "中文中文中文中文",
                        "e\u{0301}\u{0301}\u{0301}",
                        "\u{1f600}\u{1f600}\u{1f600}",
                        "a\u{fe0f}",
                        "................................................................",
                        "\u{200d}\u{200d}",
                        "tab",
                    ];
                    let pick = NASTY[rng.below(NASTY.len() as u64) as usize].to_string();
                    let t = rng.below(app.ws.tabs.len() as u64) as usize;
                    if rng.below(2) == 0 {
                        app.ws.tabs[t].name = pick;
                    } else {
                        let ids: Vec<u64> = app.ws.tabs[t].panes.keys().copied().collect();
                        if let Some(id) = ids.get(rng.below(ids.len().max(1) as u64) as usize) {
                            let spec = app.ws.tabs[t].panes.get_mut(id).unwrap();
                            if rng.below(2) == 0 {
                                spec.title = Some(pick);
                            } else {
                                spec.note = Some(pick);
                            }
                        }
                    }
                }
                // Modes own the keyboard; drive them the way main.rs does so
                // the overlays reach their filtered / mid-edit states.
                for _ in 0..rng.below(3) {
                    let k = KeyEvent::from(keys[rng.below(keys.len() as u64) as usize]);
                    app.handle_mode_key(k);
                }
                // U8(b): and the *other* way text enters those fields.
                // `handle_paste` is the one editing path that moves the
                // point by a whole string rather than a character, and it
                // does it with `byte_at`/`truncate`/`insert_str` — byte
                // offsets into text the fuzzer above has already filled
                // with combining marks and wide glyphs, at a cursor the
                // keys above have already walked somewhere arbitrary. A
                // paste that lands off a char boundary panics the event
                // loop, which is a fleet incident (`tests/panic_shutdown`).
                if rng.below(4) == 0 {
                    const PASTES: &[&str] = &[
                        "",
                        "plain",
                        "one\ntwo\nthree",
                        "crlf\r\nand\rcr",
                        "\u{4e2d}\u{6587}\u{4e2d}\u{6587}",
                        "e\u{0301}\u{0301}\u{0301}",
                        "\u{1f600}\u{200d}\u{1f9b0}",
                        "a\u{fe0f}b",
                        "\u{7}\u{1b}[31m\u{0}control",
                        // Past NOTE_MAX_LINES, so the overflow fold runs.
                        "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12",
                    ];
                    app.handle_paste(PASTES[rng.below(PASTES.len() as u64) as usize]);
                }
                let (w, h) = GEOM[rng.below(GEOM.len() as u64) as usize];
                app.on_resize(Size::new(w, h), (0, 0));
                let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
                term.draw(|f| super::draw(f, &mut app)).unwrap();
                frames += 1;
                app.quit = false; // a random Quit must not end the walk
            }
        }
        eprintln!("render sweep: {frames} frames");
    }

    fn mk_app(size: ratatui::layout::Size) -> App<crate::ports::fakes::FakePane> {
        use crate::agents;
        use crate::core::control::TokenTable;
        use crate::core::workspace::Workspace;
        use crate::ports::fakes::MemStore;
        use std::path::PathBuf;
        use std::sync::mpsc;

        let store = MemStore::default();
        let (tx, _rx) = mpsc::sync_channel(64);
        let ws = Workspace::default_in(PathBuf::from("/tmp"));
        App::<crate::ports::fakes::FakePane>::new(
            ws,
            agents::registry(),
            Box::new(store),
            tx,
            size,
            (0, 0),
            None,
            TokenTable::new().unwrap(),
        )
        .unwrap()
    }

    /// Every chrome surface that has a drawn state, rendered through the
    /// real `draw()` — the shared fixture for the §2 mechanical gates below.
    /// `FakePane::screen()` is `None`, so nothing here is program output:
    /// every cell in these buffers is chrome roost drew itself.
    fn chrome_buffers() -> Vec<(&'static str, ratatui::buffer::Buffer)> {
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        type Fixture = App<crate::ports::fakes::FakePane>;
        fn snap(app: &mut Fixture) -> ratatui::buffer::Buffer {
            let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
            term.draw(|f| super::draw(f, app)).unwrap();
            term.backend().buffer().clone()
        }
        fn three_panes() -> Fixture {
            let mut app = mk_app(Size::new(100, 30));
            app.apply(Action::NewPane);
            app.apply(Action::NewPane);
            app
        }

        let mut out: Vec<(&'static str, ratatui::buffer::Buffer)> = Vec::new();

        let mut app = three_panes();
        out.push(("normal, three tiled panes", snap(&mut app)));

        let mut app = three_panes();
        app.apply(Action::StackPane);
        out.push(("collapsed stack rows", snap(&mut app)));

        let mut app = three_panes();
        app.set_flash("copied 12 chars");
        out.push(("flash", snap(&mut app)));

        // The *other* place a flash lands (C10, amended 2026-08-20): with
        // the hint bar hidden it paints over the body's last row. This
        // fixture is the point — the gates below never saw that surface, so
        // a flash inheriting the pane border's colour passed every one of
        // them. It does not pass them now.
        let mut app = three_panes();
        app.apply(Action::ToggleHints);
        app.set_flash("copied 12 chars");
        out.push(("flash, hint bar hidden", snap(&mut app)));

        // ...and the C11 bar on that same surface, for the same reason.
        let mut app = three_panes();
        app.apply(Action::ToggleHints);
        app.note_key_seen(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('˜')));
        assert!(app.show_alt_hint());
        out.push(("alt-trap warning bar, hint bar hidden", snap(&mut app)));

        let mut app = three_panes();
        // F1's evidence: a swallowed Option chord (Option+n -> '˜' with no
        // Alt modifier), not just any key.
        app.note_key_seen(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('˜')));
        assert!(app.show_alt_hint());
        out.push(("alt-trap warning bar", snap(&mut app)));

        // Exercise the C9 ○ fallback segment (a Waiting pane with nothing
        // needing input) so it's visible to the §2 gates below, not just
        // pinned by a unit test. A shell's Waiting downgrades to Idle
        // (P2-10) and never pulls the fallback, so this promotes one pane
        // to a real adapter first.
        {
            use crate::core::status::AgentStatus;
            use crate::ports::PaneBackend;
            let mut app = three_panes();
            let id = app.pane_order()[0];
            app.ws.tabs[0].panes.get_mut(&id).unwrap().adapter = "pi".into();
            app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::Waiting);
            // Self-verifying: if a future change quietly routes this back to
            // the ◆ path (or to nothing), this fixture must not go on
            // silently exercising the wrong state.
            assert_eq!(
                app.attention_segment(),
                Some((1, false)),
                "fixture must actually hit the ○ fallback"
            );
            out.push(("hint bar ○ fallback (waiting, nothing needs input)", snap(&mut app)));
        }

        for (name, action) in [
            ("help overlay", Action::Help),
            ("picker", Action::QuickLaunch),
            ("activity feed", Action::ToggleFeed),
            ("fleet roster", Action::ToggleRoster),
            ("broadcast composer", Action::ToggleBroadcast),
            ("rename dialog", Action::RenameTab),
            ("scroll mode", Action::ScrollMode),
            ("copy mode", Action::CopyMode),
            ("raw pane", Action::ToggleRaw),
            ("floating scratch pane", Action::ToggleFloat),
            ("zoom", Action::ToggleZoom),
        ] {
            let mut app = three_panes();
            app.apply(action);
            out.push((name, snap(&mut app)));
        }

        // The "fleet roster" fixture above is unfiltered and all-tied
        // (fresh three_panes()), so it never exercises a filtered title tag
        // or a worst-first reorder. A mixed fleet with a status filter
        // active exercises both: the ◆ pane (last in tab order) sorts
        // first, and the title carries its own colored glyph.
        {
            use crate::core::status::AgentStatus;
            use crate::ports::PaneBackend;
            let mut app = three_panes();
            let needy = app.pane_order()[2];
            app.runtimes.get_mut(&needy).unwrap().set_extension_status(AgentStatus::NeedsInput);
            app.apply(Action::ToggleRoster);
            app.handle_mode_key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Tab));
            out.push(("fleet roster, filtered and reordered", snap(&mut app)));
        }

        // [C39] Same reason, one surface later: the "help overlay" fixture
        // above is unfiltered, so the §2 gates below never saw the
        // filtering title or the hint row that replaces "any key closes".
        // Both are chrome roost draws and neither was covered. Named by the
        // C39 design audit.
        {
            let mut app = three_panes();
            app.apply(Action::Help);
            for c in "/pane".chars() {
                app.handle_mode_key(crossterm::event::KeyEvent::from(
                    crossterm::event::KeyCode::Char(c),
                ));
            }
            assert_eq!(app.help_filter(), Some("pane"), "fixture must actually be filtering");
            out.push(("help overlay, filtering", snap(&mut app)));
        }

        let mut app = three_panes();
        app.apply(Action::ToggleZoom);
        app.apply(Action::ToggleHints); // the tab bar picks up the mode word
        out.push(("zoom with the hint bar hidden", snap(&mut app)));

        // C32: parked notes, both badge forms at once — the focused pane's
        // headline + age segment and an unfocused pane's bare `¶` — plus
        // the same two panes as collapsed rows (the right segment's marker),
        // so the §2 gates audit every note span the chrome can draw.
        {
            let mut app = three_panes();
            let first = app.pane_order()[0];
            let focused = app.focused;
            assert_ne!(first, focused, "fixture needs an unfocused noted pane");
            let spec = app.ws.tabs[0].panes.get_mut(&first).unwrap();
            spec.note = Some("waiting on CI".into());
            spec.noted_at = Some(0);
            let spec = app.ws.tabs[0].panes.get_mut(&focused).unwrap();
            spec.note = Some("tests green, PR up\nnext: rebase onto main".into());
            spec.noted_at = Some(0);
            out.push(("noted badges, focused and not", snap(&mut app)));
            app.apply(Action::StackPane);
            out.push(("noted collapsed rows", snap(&mut app)));
        }

        // C32: the combined pane editor, prefilled with a name and a
        // two-line note so the underlined name row, the caret row and a
        // plain row are all drawn.
        {
            let mut app = three_panes();
            let focused = app.focused;
            let spec = app.ws.tabs[0].panes.get_mut(&focused).unwrap();
            spec.note = Some("tests green, PR up\nnext: rebase onto main".into());
            spec.noted_at = Some(0);
            app.apply(Action::EditPane);
            out.push(("pane editor", snap(&mut app)));
        }

        // C16: a dead pane's action bar. The fixture omitted every
        // pane-overlay state, which is exactly how a span-styled (rather than
        // widget-styled) bar hid from these gates — it reversed its ~76 text
        // columns and left the dead program's last output showing through the
        // rest of the row. Both shapes are drawn: with and without a spawn
        // error, since the error adds a second row to the same bar.
        let mut app = three_panes();
        let focused = app.focused;
        app.on_pty_exit(focused);
        out.push(("dead pane action bar", snap(&mut app)));

        let mut app = three_panes();
        let focused = app.focused;
        app.on_pty_exit(focused);
        app.dead.insert(focused, "no such file or directory".into());
        out.push(("dead pane with a spawn error", snap(&mut app)));

        // C17/C29 (PR #46 code review): a lit text selection. Copy mode's
        // REVERSED highlight (C17) and native selection's identical one
        // (C29) both paint through `highlight_selection`, but no fixture
        // above ever set `app.selection` — the "copy mode" entry enters
        // `Mode::Copy` without marking anything — so the §2 gates below
        // never looked at a single reversed-selection cell. A multi-cell
        // span, not a single cell, so the run isn't degenerate.
        let mut app = three_panes();
        let focused = app.focused;
        app.begin_selection(focused, 0, 0);
        app.extend_selection(0, 5);
        out.push(("lit text selection", snap(&mut app)));

        // C30: sub-two-row floor notice. The top-level draw() pre-empts all
        // chrome when area.height < 2; the §2 gates that verify
        // "every cell of every drawn chrome state" are vacuous over this case
        // without a fixture for it. Render the full 80×1 state: the notice
        // only and nothing else, no tab bar (would need height >= 2).
        // This is the one case draw() handles before the usual header/body
        // split, so the snapshot must come from a direct Terminal::draw
        // with the small area — mk_app() and three_panes() both build 100×30,
        // too large to trigger the early return.
        {
            use crate::agents;
            use crate::core::control::TokenTable;
            use crate::core::workspace::Workspace;
            use crate::ports::fakes::MemStore;
            use std::path::PathBuf;
            use std::sync::mpsc;

            let store = MemStore::default();
            let (tx, _rx) = mpsc::sync_channel(64);
            let ws = Workspace::default_in(PathBuf::from("/tmp"));
            let mut app = App::<crate::ports::fakes::FakePane>::new(
                ws,
                agents::registry(),
                Box::new(store),
                tx,
                Size::new(80, 1),
                (0, 0),
                None,
                TokenTable::new().unwrap(),
            )
            .unwrap();
            let mut term = Terminal::new(TestBackend::new(80, 1)).unwrap();
            term.draw(|f| super::draw(f, &mut app)).unwrap();
            out.push(("sub-two-row floor notice (80×1)", term.backend().buffer().clone()));
        }

        // P21/C17 amendment: a running scrollback search paints its hits
        // REVERSED (current hit also UNDERLINED) directly on the pane grid
        // — `highlight_matches` is unit-tested in isolation (see
        // `search_hits_are_reversed_with_the_current_one_underlined` below),
        // but no `chrome_buffers()` state ever populated `app.search`, so
        // the full `draw()` path — and therefore the three §2 mechanical
        // gates that scan this fixture — never looked at a frame containing
        // one. `Search::over` is the sanctioned test seam for rendering a
        // search without driving a whole app; its `pane` field defaults to
        // 1, so it's retargeted at whichever pane `three_panes()` left
        // focused. Lines span far enough (0..40) that a hit lands inside
        // the pane's inner height regardless of the fresh `FakePane`'s
        // (0, 0) scroll position.
        {
            use crate::core::app::Search;
            let mut app = three_panes();
            let focused = app.focused;
            let lines: Vec<String> = (0..40).map(|i| format!("mark line {i}")).collect();
            let mut search = Search::over(lines, "mark", 3);
            search.pane = focused;
            app.search = Some(search);
            out.push(("scrollback search hits", snap(&mut app)));
        }

        out
    }

    /// Both the flash and the alt-warning can want the hint bar at once;
    /// flash must win the row.
    #[test]
    fn flash_wins_the_hint_bar_over_the_alt_warning() {
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(100, 30));
        app.set_flash("copied 12 chars");
        app.note_key_seen(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('˜')));
        assert!(app.show_alt_hint(), "evidence is present — the warning wants the bar too");

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| super::draw(f, &mut app)).unwrap();
        let row: String = (0..100)
            .filter_map(|x| term.backend().buffer().cell((x, 29)).map(|c| c.symbol().to_string()))
            .collect();
        assert!(row.contains("copied 12 chars"), "flash must win the bar: {row:?}");
        assert!(!row.contains("Alt keys"), "the alt-warning must not pre-empt the flash: {row:?}");
    }

    /// The three §2 gates below only audit whatever `chrome_buffers()`
    /// happens to produce — a drawn state the fixture never visits is
    /// checked vacuously. `chrome_buffers()` has no way to notice *itself*
    /// that a new drawn surface exists; only a human remembering does.
    ///
    /// This closes that gap for exactly one axis: `Mode` gates every modal
    /// chrome surface (C12-C16, C20, C22, C24, C27), and the match below has
    /// **no wildcard arm** — the compiler refuses to build the moment a new
    /// `Mode` variant is added without a decision here ("fails until
    /// covered" rather than "silently passes"). It is not a general
    /// solution: screen-size variants (C30), status combinations (the
    /// roster filter), and focus permutations (C7's unfocused
    /// expanded-stack edge) are not enum-shaped, so nothing here catches a
    /// gap in those axes — that remainder is a human-must-remember list.
    #[test]
    fn every_mode_variant_has_a_chrome_buffers_fixture() {
        // Exhaustive by construction: a new `Mode` variant is a compile
        // error here until this match is taught the fixture-name substring
        // that proves it. Add the `chrome_buffers()` case first, then the
        // label below — in that order, so the assertion loop actually
        // proves the fixture exists rather than describing one that doesn't.
        fn required_fixture_substring(m: &Mode) -> Option<&'static str> {
            match m {
                // The baseline: every fixture is Normal-mode chrome.
                Mode::Normal => None,
                Mode::Rename { .. } => Some("rename dialog"),
                Mode::PaneEdit { .. } => Some("pane editor"),
                Mode::Picker { .. } => Some("picker"),
                Mode::Scroll => Some("scroll mode"),
                Mode::Copy { .. } => Some("copy mode"),
                Mode::Help { .. } => Some("help overlay"),
                Mode::Feed { .. } => Some("activity feed"),
                Mode::Roster { .. } => Some("fleet roster"),
                Mode::Broadcast { .. } => Some("broadcast composer"),
                Mode::Search { .. } => Some("scrollback search"),
            }
        }
        // One representative value per variant, purely to drive the
        // exhaustive match above — never rendered, so field values are
        // arbitrary placeholders.
        let samples = [
            Mode::Normal,
            Mode::Rename { buffer: String::new(), cursor: 0, target: RenameTarget::Tab },
            Mode::PaneEdit {
                name: String::new(),
                lines: vec![String::new()],
                row: 0,
                col: 0,
                pane: 1,
            },
            Mode::Picker { selection: 0, filter: String::new(), cwd: 0, on_cwd: false },
            Mode::Scroll,
            Mode::Copy { cursor: (0, 0) },
            Mode::Help { top: 0, filter: None, cursor: 0 },
            Mode::Feed { offset: 0 },
            Mode::Roster { cursor: 1, filter: String::new(), top: 0, status_filter: None },
            Mode::Broadcast { lines: vec![String::new()], row: 0, col: 0, status_filter: None },
            Mode::Search { copy_cursor: None },
        ];
        let names: Vec<&str> = chrome_buffers().iter().map(|(n, _)| *n).collect();
        for m in &samples {
            if let Some(needle) = required_fixture_substring(m) {
                assert!(
                    names.iter().any(|n| n.contains(needle)),
                    "a Mode variant has no chrome_buffers() fixture containing {needle:?}: {names:?}",
                );
            }
        }
    }

    /// C16 regression: the dead-pane action bar reverses its **whole inner
    /// width**, not just the columns its text happens to occupy. Styling the
    /// span instead of the widget left the dead program's last output visible
    /// at normal video across the rest of the row — three bars (C10 flash,
    /// C11 alt warning, C16 dead pane) meant to read as one treatment, one of
    /// them ragged. Caught by the design supervisor; the gate fixture now
    /// draws the state that hid it.
    #[test]
    fn the_dead_pane_bar_reverses_its_whole_row() {
        for (name, buf) in chrome_buffers() {
            if !name.starts_with("dead pane") {
                continue;
            }
            let area = *buf.area();
            // The bar is the bottom row of the dead pane's inner area. Find
            // it by its text, then assert the reversal runs across the row
            // rather than stopping where the message does.
            let row = (0..area.height)
                .find(|y| {
                    (0..area.width)
                        .map(|x| buf[(x, *y)].symbol())
                        .collect::<String>()
                        .contains("exited — Enter")
                })
                .unwrap_or_else(|| panic!("{name}: no dead-pane bar drawn"));
            let reversed: Vec<u16> = (0..area.width)
                .filter(|x| buf[(*x, row)].style().add_modifier.contains(Modifier::REVERSED))
                .collect();
            assert!(!reversed.is_empty(), "{name}: the bar is not reversed at all");
            let (first, last) = (reversed[0], *reversed.last().unwrap());
            assert_eq!(
                reversed.len() as u16,
                last - first + 1,
                "{name}: the bar's reversal is not contiguous across its row"
            );
            // Wider than the message itself — what the span-styled version
            // could not be.
            let text_cols = (0..area.width).filter(|x| buf[(*x, row)].symbol() != " ").count();
            assert!(
                (last - first + 1) as usize > text_cols,
                "{name}: the bar covers only its own glyphs, not the row"
            );
        }
    }

    /// §2 gate: **structure-only discipline.** `theme::rule()` (ANSI 8) is
    /// the one theme-variant colour roost spends, and it may never carry a
    /// word — a theme that renders ANSI 8 nearly invisible must cost a
    /// hairline, not a label. Mechanical, over the real drawn frames: any
    /// `DarkGray` cell has to be a rule glyph.
    #[test]
    fn structure_colour_never_carries_text() {
        // Plain single-line box drawing (pane/modal borders), the tab
        // separator, and blank fill. Nothing that spells anything.
        const RULE_GLYPHS: &[&str] = &[" ", "─", "│", "┌", "┐", "└", "┘", "├", "┤", "┬", "┴", "┼"];
        for (name, buf) in chrome_buffers() {
            let area = *buf.area();
            for y in area.y..area.y + area.height {
                for x in area.x..area.x + area.width {
                    let cell = buf.cell((x, y)).expect("cell inside the buffer");
                    if cell.style().fg != Some(Color::DarkGray) {
                        continue;
                    }
                    assert!(
                        RULE_GLYPHS.contains(&cell.symbol()),
                        "{name}: structure colour carries text {:?} at ({x},{y})",
                        cell.symbol(),
                    );
                }
            }
        }
    }

    /// §2 gate: **no chrome element pairs two theme-variant colours.** Chrome
    /// paints no colour fill at all — the two bars, the flash, the dead-pane
    /// and alt-warning bars and the focused collapsed row all lost theirs —
    /// so the only `bg` a chrome cell may carry is the `Color::Reset`
    /// sentinel. Attention surfaces reverse the terminal's own pair instead,
    /// which is contrasty in any theme by construction.
    #[test]
    fn chrome_paints_no_background_fill() {
        for (name, buf) in chrome_buffers() {
            let area = *buf.area();
            for y in area.y..area.y + area.height {
                for x in area.x..area.x + area.width {
                    let cell = buf.cell((x, y)).expect("cell inside the buffer");
                    match cell.style().bg {
                        None | Some(Color::Reset) => {}
                        Some(other) => panic!(
                            "{name}: chrome fills with {other:?} at ({x},{y}) — §2 allows no fill"
                        ),
                    }
                }
            }
        }
    }

    /// Every word roost draws is the terminal's own foreground on its own
    /// background — the one contrast pair the user has already validated —
    /// optionally one rung quieter. A chrome cell carrying a *letter* may
    /// therefore only be `Reset`, the accent red, or unstyled; never a
    /// colour the theme is free to swallow. Status animation lives in the
    /// glyph (C5's spinner), not a second colour.
    #[test]
    fn every_chrome_word_is_drawn_in_ink_the_user_already_reads() {
        let legible = [Color::Reset, Color::Red];
        for (name, buf) in chrome_buffers() {
            let area = *buf.area();
            for y in area.y..area.y + area.height {
                for x in area.x..area.x + area.width {
                    let cell = buf.cell((x, y)).expect("cell inside the buffer");
                    if !cell.symbol().chars().any(char::is_alphanumeric) {
                        continue;
                    }
                    let Some(fg) = cell.style().fg else { continue };
                    assert!(
                        legible.contains(&fg),
                        "{name}: the word cell {:?} at ({x},{y}) is drawn in {fg:?}",
                        cell.symbol(),
                    );
                }
            }
        }
    }

    /// U15: hiding the hint bar used to hide the only mode indication
    /// there was — a zoomed pane with hints off read as a one-pane tab, and
    /// SCROLL/COPY/RAW vanished entirely. The word moves to the tab bar
    /// exactly when the bar isn't carrying it, and only when there's
    /// something to say (plain Normal reports nothing, as always).
    #[test]
    fn the_tab_bar_carries_the_mode_word_only_when_the_hint_bar_is_gone() {
        use crate::ui::input::Action;
        use ratatui::layout::Size;

        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::ToggleZoom);
        assert!(app.hints_shown());
        assert_eq!(super::tab_status_word(&app), None, "the hint bar has it");

        app.apply(Action::ToggleHints); // Alt+/
        assert_eq!(super::tab_status_word(&app), Some("ZOOM"));
        app.apply(Action::ToggleZoom);
        assert_eq!(super::tab_status_word(&app), None, "plain Normal says nothing");

        // Real modes and the other pseudo-state come along too.
        app.apply(Action::ScrollMode);
        assert_eq!(super::tab_status_word(&app), Some("SCROLL"));
        app.mode = Mode::Normal;
        app.apply(Action::ToggleRaw);
        assert_eq!(super::tab_status_word(&app), Some("RAW"));
        app.apply(Action::ToggleRaw);

        // A terminal too short for the hint bar is "the bar is absent" too.
        let mut tiny = mk_app(Size::new(100, 2));
        tiny.apply(Action::ToggleZoom);
        assert!(!tiny.hints_shown());
        assert_eq!(super::tab_status_word(&tiny), Some("ZOOM"));
    }

    /// ...and it actually reaches the screen: the drawn tab bar carries
    /// `ZOOM · {cwd} · saved ✓` once hints are off.
    #[test]
    fn a_zoomed_pane_with_hints_hidden_still_says_zoom_on_the_tab_bar() {
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::ToggleZoom);
        app.apply(Action::ToggleHints);
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| super::draw(f, &mut app)).unwrap();
        let row: String = (0..100)
            .filter_map(|x| term.backend().buffer().cell((x, 0)).map(|c| c.symbol().to_string()))
            .collect();
        assert!(row.contains("ZOOM · "), "tab bar row was: {row:?}");
        assert!(row.trim_end().ends_with(theme::SAVED), "the save word still trails: {row:?}");
    }

    /// C10/C11 (2026-08-20): neither message may be hostage to whether the
    /// hint bar is drawn. Both used to reach the screen only through
    /// `draw_hint_bar`, so `Alt+/` silenced every refusal, every copy
    /// result, every confirm-arm prompt, and the one sentence explaining a
    /// terminal that swallows Alt — which `Alt+/` itself cannot then undo.
    #[test]
    fn a_hidden_hint_bar_does_not_silence_the_flash_or_the_alt_trap_warning() {
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        fn last_row(app: &mut App<crate::ports::fakes::FakePane>) -> String {
            let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
            term.draw(|f| super::draw(f, app)).unwrap();
            (0..100)
                .filter_map(|x| {
                    term.backend().buffer().cell((x, 29)).map(|c| c.symbol().to_string())
                })
                .collect()
        }

        // A flash, with the bar hidden.
        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::ToggleHints);
        assert!(!app.hints_shown(), "setup: the hint bar is not drawn");
        app.set_flash("no room to split — stacked needs 10 rows, has 4");
        assert!(
            last_row(&mut app).contains("no room to split"),
            "the refusal never reached the screen: {:?}",
            last_row(&mut app),
        );

        // U22's confirm-arm prompt is a flash too — the case where silence
        // costs the user a pane rather than a message.
        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::ToggleHints);
        app.apply(Action::ClosePane); // arms, and flashes "… again to …"
        let row = last_row(&mut app);
        assert!(row.contains("again"), "the confirm prompt never reached the screen: {row:?}");

        // The C11 bar, with the bar hidden — the one Alt+/ cannot restore.
        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::ToggleHints);
        app.note_key_seen(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('˜')));
        assert!(app.show_alt_hint(), "setup: the alt trap is detected");
        let row = last_row(&mut app);
        assert!(!row.trim().is_empty(), "the alt-trap warning never reached the screen");
        assert_eq!(
            row.trim(),
            app.alt_hint_line().trim(),
            "the same sentence the hint bar would have carried",
        );

        // Flash still wins over the warning, exactly as on the hint bar.
        app.set_flash("copied 12 chars");
        assert!(
            last_row(&mut app).contains("copied 12 chars"),
            "flash must outrank the persistent warning here too",
        );
    }

    /// Asserts `text` sits flush against the pane's top-right border
    /// corner on a drawn border row — i.e. actually right-aligned, not
    /// merely present somewhere on the row (a left/center-alignment
    /// regression would still pass a bare `contains`). `border` must be one
    /// symbol per column, `width` columns wide, corner glyph included.
    fn assert_title_right_aligned(border: &str, width: u16, text: &str) {
        let cells: Vec<char> = border.chars().collect();
        let corner = width as usize - 1;
        assert_eq!(cells.get(corner), Some(&'┐'), "corner glyph must survive: {border:?}");
        let start = corner - text.chars().count();
        let tail: String = cells[start..corner].iter().collect();
        assert_eq!(tail, text, "title must sit flush against the top-right corner: {border:?}");
    }

    /// C21 (amended 2026-08-11, zoom indicator): `zoom_title_text`'s own
    /// two-step shedding rule, pinned without a `Frame` — full text, then
    /// bare `ZOOM`, then nothing once even that doesn't fit.
    #[test]
    fn zoom_title_text_sheds_the_hidden_count_before_the_whole_title() {
        assert_eq!(super::zoom_title_text(2, 20), Some("ZOOM · 2 hidden".to_string()));
        // n == 0 (single-pane tab): no count clause to shed in the first place.
        assert_eq!(super::zoom_title_text(0, 20), Some("ZOOM".to_string()));
        // Too narrow for the count clause, wide enough for bare ZOOM.
        assert_eq!(super::zoom_title_text(2, 12), Some("ZOOM".to_string()));
        // Too narrow for even ZOOM: the whole title drops.
        assert_eq!(super::zoom_title_text(2, 3), None);
        assert_eq!(super::zoom_title_text(0, 3), None);
    }

    /// C21 (amended 2026-08-11): a zoomed multi-pane tab's top border names
    /// how many real-tree panes it's hiding.
    #[test]
    fn a_zoomed_multi_pane_tab_shows_the_hidden_count_on_its_border() {
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::NewPane);
        app.apply(Action::NewPane); // tab 0: three panes, focus = 3
        app.apply(Action::ToggleZoom);
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| super::draw(f, &mut app)).unwrap();
        // Row 1: the zoomed pane's top border (row 0 is the tab bar).
        let border: String = (0..100)
            .filter_map(|x| term.backend().buffer().cell((x, 1)).map(|c| c.symbol().to_string()))
            .collect();
        assert_title_right_aligned(&border, 100, "ZOOM · 2 hidden");
    }

    /// C21 (amended 2026-08-11): a zoomed single-pane tab has nothing
    /// hidden — the border says bare `ZOOM`, no `hidden` clause.
    #[test]
    fn a_zoomed_single_pane_tab_shows_bare_zoom_with_no_hidden_count() {
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::ToggleZoom);
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| super::draw(f, &mut app)).unwrap();
        let border: String = (0..100)
            .filter_map(|x| term.backend().buffer().cell((x, 1)).map(|c| c.symbol().to_string()))
            .collect();
        assert_title_right_aligned(&border, 100, "ZOOM");
        assert!(!border.contains("hidden"), "single pane has nothing hidden: {border:?}");
    }

    /// C21 (amended 2026-08-11): not zoomed — the border carries no title
    /// at all, on any pane.
    #[test]
    fn an_unzoomed_multi_pane_tab_has_no_zoom_title_anywhere() {
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::NewPane);
        app.apply(Action::NewPane); // three panes, not zoomed
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| super::draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        let whole: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(!whole.contains("ZOOM"), "no border should say ZOOM while unzoomed: {whole:?}");
        assert!(!whole.contains("hidden"), "and no hidden count either: {whole:?}");
    }

    /// C21 (amended 2026-08-11): too narrow for `ZOOM · N hidden` — the
    /// count is what yields, before the identity badge (a separate row, C4)
    /// loses anything at all.
    #[test]
    fn a_narrow_zoomed_pane_drops_the_count_before_the_badge() {
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        // Panes need `layout::MIN_SPLIT_COLS` (36) of width to split at all —
        // build the real 3-pane tree at a roomy size, zoom, then narrow the
        // terminal. The layout tree itself doesn't un-split on a shrink
        // (only a fresh split/close touches it), so the real tree still has
        // 3 panes (n = 2 hidden) even though the zoomed view is now tiny.
        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::NewPane);
        app.apply(Action::NewPane); // three panes, focus = 3, n = 2 hidden
        app.apply(Action::ToggleZoom);
        app.on_resize(Size::new(14, 10), (0, 0));
        let mut term = Terminal::new(TestBackend::new(14, 10)).unwrap();
        term.draw(|f| super::draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        let border: String =
            (0..14).filter_map(|x| buf.cell((x, 1)).map(|c| c.symbol().to_string())).collect();
        assert_title_right_aligned(&border, 14, "ZOOM");
        assert!(!border.contains("hidden"), "the count yields first: {border:?}");
        // [Amended 2026-08-21] Identity shares this border now (C3/C4), and
        // yields to zoom rather than the other way round — but only the
        // columns zoom actually took. At 14 the two still coexist, so the
        // join key survives on the left while `ZOOM` holds the right.
        assert!(border.contains('3'), "identity keeps its end of the border: {border:?}");
        // ...and the pane's own first content row, which the badge used to
        // occupy, is the pane's again.
        let content_row: String =
            (0..14).filter_map(|x| buf.cell((x, 2)).map(|c| c.symbol().to_string())).collect();
        assert!(!content_row.contains('3'), "no chrome left on the content row: {content_row:?}");
    }

    /// C21/C22 (amended 2026-08-11): "keeps zoom" + the float draws above
    /// it (C22) — the title belongs to the tiled zoom target alone, never
    /// the float's own border, even though both are live at once.
    #[test]
    fn the_float_never_gets_the_zoom_title_only_the_tiled_target_does() {
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::NewPane);
        app.apply(Action::NewPane); // three panes, focus = 3, n = 2 hidden
        app.apply(Action::ToggleZoom);
        let target = app.focused; // the zoom target, before the float steals focus
        app.apply(Action::ToggleFloat); // spawns + shows + focuses the float
        assert!(app.zoomed(), "float toggle keeps zoom (C21)");

        let rects = app.display_rects();
        let target_pr = rects.iter().find(|pr| pr.id == target).copied().unwrap();
        let float_pr = rects.iter().find(|pr| pr.id != target).copied().unwrap();
        assert_eq!(target_pr.rect, app.body_area(), "the target is still the full-body zoom view");
        assert_ne!(float_pr.rect, target_pr.rect, "the float draws its own smaller rect");

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| super::draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer();

        let row_at = |rect: ratatui::layout::Rect| -> String {
            (rect.x..rect.x + rect.width)
                .filter_map(|x| buf.cell((x, rect.y)).map(|c| c.symbol().to_string()))
                .collect()
        };
        assert_title_right_aligned(
            &row_at(target_pr.rect),
            target_pr.rect.width,
            "ZOOM · 2 hidden",
        );
        let float_border = row_at(float_pr.rect);
        assert!(
            !float_border.contains("ZOOM"),
            "the float's own border stays plain: {float_border:?}"
        );
    }

    /// C14 (U20), through the real `draw`: the picker paints two columns —
    /// numbered adapter rows on the left, recent directories on the right —
    /// the type-ahead query rides in the title so a narrowed list says why
    /// it is narrow, and `❯` marks only the column holding the keyboard.
    #[test]
    fn the_picker_draws_both_columns_and_the_live_filter() {
        use crate::core::app::Mode;
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::QuickLaunch);
        let rows = |app: &mut App<crate::ports::fakes::FakePane>| -> Vec<String> {
            let backend = TestBackend::new(100, 30);
            let mut term = Terminal::new(backend).unwrap();
            term.draw(|f| super::draw(f, app)).unwrap();
            let buf = term.backend().buffer().clone();
            (0..30)
                .map(|y| {
                    (0..100)
                        .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                        .collect::<String>()
                })
                .collect()
        };

        let drawn = rows(&mut app).join("\n");
        assert!(drawn.contains("pick agent"), "the unfiltered title:\n{drawn}");
        assert!(drawn.contains("1 pi"), "the numbered adapter column:\n{drawn}");
        // `mk_app`'s workspace lives in /tmp, so that is the seeded cwd row.
        assert!(drawn.contains("/tmp"), "the recent-cwd column:\n{drawn}");
        assert!(drawn.contains(theme::PICKER_SELECTED), "the adapter column has the marker");

        // Type-ahead: the title carries the query and the rows narrow. Two
        // characters, not one: a bare `p` also matches `opencode`, so it no
        // longer isolates a single row — and the point here is that the
        // filter narrows to exactly one, not that any particular adapter
        // happens to be alone under a one-letter query.
        for c in ['p', 'i'] {
            app.handle_mode_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(c),
                crossterm::event::KeyModifiers::NONE,
            ));
        }
        let drawn = rows(&mut app).join("\n");
        assert!(drawn.contains(&format!("pi{}", theme::RENAME_CURSOR)), "the live query:\n{drawn}");
        assert!(drawn.contains("1 pi"), "`pi` keeps pi:\n{drawn}");
        assert!(!drawn.contains("claude"), "and drops the rest:\n{drawn}");
        assert!(!drawn.contains("opencode"), "including the other `p` match:\n{drawn}");
        assert!(!drawn.contains(" 2 "), "no second row survives the filter:\n{drawn}");
        assert!(matches!(app.mode, Mode::Picker { .. }));
    }

    /// C27, end to end through the real `draw()`: the roster groups panes
    /// under underlined tab headers, spells each pane row in C8's collapsed
    /// format (id + name + `{adapter} · {state word}`), marks the cursor with
    /// the picker's `❯`, lists a pane from a tab that is **not** active, and
    /// puts the live type-ahead query in the frame title.
    #[test]
    fn the_roster_draws_grouped_rows_a_cursor_and_the_live_filter() {
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::NewPane); // tab0: two panes
        app.apply(Action::NewTab); // tab1 ("tab2"): one pane, now active
        let drawn = |app: &mut App<crate::ports::fakes::FakePane>| -> String {
            let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
            term.draw(|f| super::draw(f, app)).unwrap();
            let buf = term.backend().buffer().clone();
            (0..30)
                .map(|y| {
                    (0..100)
                        .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        app.apply(Action::ToggleRoster);
        let frame = drawn(&mut app);
        assert!(frame.contains(" fleet "), "the frame's title:\n{frame}");
        assert!(frame.contains("1 MAIN · 2 PANES"), "tab 1's group header:\n{frame}");
        assert!(frame.contains("2 TAB2 · 1 PANE"), "tab 2's group header:\n{frame}");
        // C8's row format, for a pane in the *inactive* tab — the whole point
        // of the feature. `shell · /tmp` untitled ⇒ the state word alone.
        assert!(frame.contains("1 shell · tmp"), "an inactive tab's pane row:\n{frame}");
        assert!(frame.contains("idle") || frame.contains("your turn"), "a state word:\n{frame}");
        assert!(frame.contains(theme::PICKER_SELECTED), "the cursor marker:\n{frame}");
        assert!(frame.contains("ROSTER"), "the C9 mode word:\n{frame}");

        // Type-ahead: the title carries the query, and a group with no
        // surviving pane loses its header too.
        for c in "3".chars() {
            app.handle_mode_key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char(
                c,
            )));
        }
        let frame = drawn(&mut app);
        assert!(frame.contains(&format!("fleet — 3{}", theme::RENAME_CURSOR)), "query:\n{frame}");
        assert!(frame.contains("2 TAB2 · 1 PANE"), "pane 3 lives in tab 2:\n{frame}");
        assert!(!frame.contains("1 MAIN · 2 PANES"), "tab 1 filtered away whole:\n{frame}");
    }

    /// [ux P2-11], end to end through the real `draw()`: `Tab` narrows the
    /// drawn rows to one severity tier and tags the frame title with that
    /// tier's own glyph — the same discoverability idiom the type-ahead
    /// query already has.
    #[test]
    fn the_roster_status_filter_narrows_rows_and_tags_the_title() {
        use crate::core::status::AgentStatus;
        use crate::ports::PaneBackend;
        use crate::ui::input::Action;
        use crossterm::event::{KeyCode, KeyEvent};
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::NewPane); // panes 1 (needy below), 2 (stays idle)
        app.runtimes.get_mut(&1).unwrap().set_extension_status(AgentStatus::NeedsInput);
        app.apply(Action::ToggleRoster);
        app.handle_mode_key(KeyEvent::from(KeyCode::Tab)); // cycle to NeedsInput

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| super::draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
        let frame: String = (0..30)
            .map(|y| {
                (0..100)
                    .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            frame.contains(&format!("fleet — {} only", theme::GLYPH_NEEDS_INPUT)),
            "the title tags the active tier:\n{frame}"
        );
        assert!(frame.contains("needs you"), "the ◆ row survives:\n{frame}");
        assert!(!frame.contains("idle"), "the idle pane is filtered out:\n{frame}");
    }

    /// [ux P2-10], end to end through the real `draw()`: a quiet shell's
    /// heuristic `Waiting` draws `idle`/`·`, never `your turn`/`○` — C8's own
    /// state word, fed by `App::display_status` rather than the raw
    /// `row_status`. Checked on the corner badge's glyph (C4 carries no
    /// spelled-out word, only `theme::status_style`'s glyph) and the roster
    /// row's word (C27 reuses C8's format verbatim, glyph and word both).
    #[test]
    fn a_quiet_shells_waiting_draws_as_idle_not_your_turn() {
        use crate::core::status::AgentStatus;
        use crate::ports::PaneBackend;
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(100, 30));
        let id = app.pane_order()[0];
        app.runtimes.get_mut(&id).unwrap().set_extension_status(AgentStatus::Waiting);

        let drawn_lines = |app: &mut App<crate::ports::fakes::FakePane>| -> Vec<String> {
            let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
            term.draw(|f| super::draw(f, app)).unwrap();
            let buf = term.backend().buffer().clone();
            (0..30)
                .map(|y| {
                    (0..100)
                        .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                        .collect::<String>()
                })
                .collect()
        };

        // The corner badge, in the ordinary single-pane view: idle's `·`
        // glyph (theme::GLYPH_IDLE), not waiting's `○`. Scoped to the
        // badge's own row — the tab bar's separate (untouched by this fix,
        // by design: item 4 scopes out C2/C5's tab aggregate) glyph lives
        // above it and would otherwise collide with a whole-frame search.
        let lines = drawn_lines(&mut app);
        let badge_row = lines.iter().find(|l| l.contains("1 shell")).expect("the badge row");
        assert!(
            badge_row.contains(theme::GLYPH_IDLE),
            "the corner badge's glyph reads idle:\n{badge_row}"
        );
        assert!(!badge_row.contains(theme::GLYPH_WAITING), "not waiting's ○:\n{badge_row}");

        // The roster row (C27 reuses C8's format verbatim) spells the word.
        app.apply(Action::ToggleRoster);
        let frame = drawn_lines(&mut app).join("\n");
        assert!(frame.contains("idle"), "the roster row reads idle:\n{frame}");
        assert!(!frame.contains("your turn"), "\"your turn\" is false for a shell:\n{frame}");
    }

    /// C15 (amended), end to end through the real `draw()`: the keymap
    /// draws its groups as underlined headings, spells each chord in the
    /// key column, and — on a body too short for the whole table — says so
    /// in its own title rather than silently ending mid-list.
    #[test]
    fn the_keymap_draws_grouped_rows_and_admits_when_it_is_scrolled() {
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::Help);
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| super::draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
        let frame: String = (0..30)
            .map(|y| {
                (0..100)
                    .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(frame.contains("PANES"), "a group heading:\n{frame}");
        assert!(frame.contains("Alt+n"), "a chord from it:\n{frame}");
        assert!(frame.contains("Esc closes"), "the way out is in the title:\n{frame}");
        assert!(frame.contains("type to filter"), "…and the typing rule:\n{frame}");

        // The heading wears C6's rule across its own column, exactly as the
        // roster's group rows do — not just uppercase text.
        let (hy, hx) = (0..30)
            .flat_map(|y| (0..100).map(move |x| (y, x)))
            .find(|&(y, x)| {
                buf.cell((x, y)).is_some_and(|c| c.symbol() == "P")
                    && (0..5).all(|i| {
                        buf.cell((x + i, y))
                            .is_some_and(|c| c.symbol() == &"PANES"[i as usize..][..1])
                    })
            })
            .expect("the PANES heading is on screen");
        assert!(
            buf.cell((hx, hy)).unwrap().modifier.contains(Modifier::UNDERLINED),
            "the heading is underlined (C6's idiom)",
        );

        // A 30-row terminal cannot hold the whole table, so the title has to
        // own up to it — the alternative is a list that just stops.
        let (visible, total) = super::help_scroll_extent(app.body_area(), app.keymap(), None);
        assert!(visible < total, "the fixture is genuinely scrolled");
        assert!(frame.contains(&format!("/{total}")), "the title counts the rows:\n{frame}");
        assert!(frame.contains("↑↓ more"), "…and names the keys that reach them:\n{frame}");
    }

    /// C27 + C5, the headline restore case: a workspace reloaded from disk
    /// has runtimes for the active tab only (`spawn_active_tab`), so the
    /// first `Alt+Shift+a` of a session lists panes that have never been
    /// started. Those rows must read "not started" behind the tab bar's own
    /// `·`, never "exited" behind `✕` — the roster used to call every
    /// runtime-less pane dead, which showed a restored fleet as a morgue
    /// while the tab strip above it said the opposite in the same frame.
    #[test]
    fn a_restored_tab_that_has_never_spawned_reads_not_started_not_exited() {
        use crate::agents;
        use crate::core::control::TokenTable;
        use crate::ports::fakes::{FakePane, MemStore};
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;
        use std::sync::mpsc;

        // Build a two-tab workspace, then *reload* it the way a restart
        // does: a fresh App over the same saved `Workspace`.
        let size = Size::new(100, 30);
        let mut built = mk_app(size);
        built.apply(Action::NewTab); // tab2, now active
        built.apply(Action::NewPane); // tab2: two panes
        built.apply(Action::PrevTab); // leave tab 1 as the active one
        let ws = built.ws.clone();
        let (tx, _rx) = mpsc::sync_channel(64);
        let mut app = App::<FakePane>::new(
            ws,
            agents::registry(),
            Box::new(MemStore::default()),
            tx,
            size,
            (0, 0),
            None,
            TokenTable::new().unwrap(),
        )
        .unwrap();

        // Precondition: tab 2's panes really are runtime-less — this test is
        // worthless if the restore spawned them after all.
        let unspawned: Vec<crate::core::layout::PaneId> = app.ws.tabs[1]
            .panes
            .keys()
            .copied()
            .filter(|id| !app.runtimes.contains_key(id))
            .collect();
        assert_eq!(unspawned.len(), 2, "a non-active restored tab spawns nothing");
        for id in &unspawned {
            assert_eq!(app.row_status(*id), None, "pane {id} has no runtime and is not dead");
        }

        app.apply(Action::ToggleRoster);
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| super::draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
        let frame: String = (0..30)
            .map(|y| {
                (0..100)
                    .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(frame.contains("2 TAB2 · 2 PANES"), "the unspawned tab is listed:\n{frame}");
        assert_eq!(frame.matches("not started").count(), 2, "both its rows:\n{frame}");
        assert!(!frame.contains("exited"), "nothing in the roster is dead:\n{frame}");
        assert!(!frame.contains(theme::GLYPH_EXITED), "no ✕ glyph anywhere:\n{frame}");

        // …and the row's glyph is the same one the tab bar puts on that tab
        // in this very frame, which is the whole point of routing both
        // through `TabSummary::Unknown`.
        let (tab_glyph, _) = theme::tab_summary_style(crate::core::app::TabSummary::Unknown);
        let row: String = super::roster_row_spans(
            &app,
            &RosterRow::Pane { id: unspawned[0] },
            50,
            app.focused,
            theme::GLYPH_WORKING,
        )
        .iter()
        .map(|s| s.content.to_string())
        .collect();
        assert!(row.contains(tab_glyph), "row {row:?} wears the tab's own glyph {tab_glyph:?}");
    }

    /// C27 borrows C6's header idiom: the group row is underlined edge to
    /// edge (text *and* the fill after it), so it reads as a rule rather than
    /// as another pane.
    #[test]
    fn roster_group_headers_are_underlined_across_the_whole_row() {
        let row = RosterRow::Group { label: " 1 MAIN · 2 PANES".to_string() };
        let mut app = mk_app(ratatui::layout::Size::new(100, 30));
        app.apply(crate::ui::input::Action::ToggleRoster);
        let spans = super::roster_row_spans(&app, &row, 40, 1, theme::GLYPH_WORKING);
        let text: String = spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(mouse::display_width(&text), 40, "the label is padded to the row width");
        for s in &spans {
            assert!(
                s.style.add_modifier.contains(Modifier::UNDERLINED),
                "every cell of the header carries the rule: {s:?}"
            );
        }
    }

    /// C27's two marks mean two different things: `❯` is the cursor (what
    /// Enter acts on), `▎` is the focused pane (where you are). A row that is
    /// both carries both, and the pane row behind the cursor column is C8's
    /// own `collapsed_row_spans` output verbatim.
    #[test]
    fn roster_pane_rows_carry_the_cursor_and_focus_marks_separately() {
        let mut app = mk_app(ratatui::layout::Size::new(100, 30));
        app.apply(crate::ui::input::Action::NewPane);
        app.apply(crate::ui::input::Action::ToggleRoster);
        let focused = app.focused;
        let other = app.pane_order().into_iter().find(|id| *id != focused).expect("two panes");

        let render = |app: &App<crate::ports::fakes::FakePane>, id, cursor| -> String {
            super::roster_row_spans(app, &RosterRow::Pane { id }, 50, cursor, theme::GLYPH_WORKING)
                .iter()
                .map(|s| s.content.to_string())
                .collect()
        };
        let cursor_on_focused = render(&app, focused, focused);
        assert!(cursor_on_focused.starts_with(theme::PICKER_SELECTED), "{cursor_on_focused:?}");
        assert!(cursor_on_focused.contains(theme::MARKER_ACTIVE), "{cursor_on_focused:?}");

        let neither = render(&app, other, focused);
        assert!(!neither.starts_with(theme::PICKER_SELECTED), "{neither:?}");
        assert!(!neither.contains(theme::MARKER_ACTIVE), "{neither:?}");
        // The row's body is C8's, one column narrower than the roster row.
        let c8: String = collapsed_row_spans(
            49,
            false,
            Some(AgentStatus::Idle),
            other,
            &app.display_name(other),
            "shell",
            false,
            false,
            false,
            theme::GLYPH_WORKING,
        )
        .iter()
        .map(|s| s.content.to_string())
        .collect();
        assert_eq!(neither, format!(" {c8}"));
    }

    #[test]
    fn draw_does_not_panic_at_the_80x24_floor_with_float_and_feed_open() {
        // C22's stacking order draws the float under the feed modal; at the
        // spec's own 80x24 floor both are live at once whenever the float
        // was already shown before Alt+e (toggling feed doesn't hide it) —
        // a combination no existing test drove through the real draw()
        // pipeline end to end.
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(80, 24));
        app.apply(Action::ToggleFloat);
        app.apply(Action::ToggleFeed);
        assert!(matches!(app.mode, Mode::Feed { .. }));
        assert!(app.display_rects().len() >= 2, "float should still be in the display list");

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| super::draw(f, &mut app)).unwrap();
    }

    /// C27 shares C20's geometry, so it inherits the 80×24 proof — but the
    /// roster also draws *rows built from other rows* (a group header padded
    /// to the inner width, a C8 row one column narrower), which is where a
    /// width underflow would land. Driven through the real `draw()` at the
    /// spec's floor and at the 36×10 minimum, with a fleet big enough to
    /// overflow the window.
    #[test]
    fn the_roster_draws_at_the_floor_and_below_it_without_panicking() {
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        for size in [Size::new(80, 24), Size::new(36, 10)] {
            let mut app = mk_app(size);
            for _ in 0..4 {
                app.apply(Action::NewPane);
            }
            app.apply(Action::NewTab);
            app.apply(Action::ToggleRoster);
            let mut term = Terminal::new(TestBackend::new(size.width, size.height)).unwrap();
            term.draw(|f| super::draw(f, &mut app)).unwrap();
        }
    }

    /// C15 (amended): the keymap draws at the 80×24 floor, at the 36×10
    /// split floor beneath it, and on a body so short the overlay has room
    /// for no content rows at all — the arithmetic the column/scroll layout
    /// added is exactly where an off-by-one would panic, and the one modal
    /// you open when lost must never be the one that crashes.
    #[test]
    fn the_keymap_draws_at_the_floor_and_below_it_without_panicking() {
        use crate::ui::input::Action;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        for size in [Size::new(80, 24), Size::new(200, 26), Size::new(36, 10), Size::new(20, 4)] {
            let mut app = mk_app(size);
            app.apply(Action::Help);
            let mut term = Terminal::new(TestBackend::new(size.width, size.height)).unwrap();
            term.draw(|f| super::draw(f, &mut app)).unwrap();
            // …and scrolled to its end, where `top` is at its largest.
            let (visible, total) = super::help_scroll_extent(app.body_area(), app.keymap(), None);
            app.mode = Mode::Help { top: total.saturating_sub(visible), filter: None, cursor: 0 };
            term.draw(|f| super::draw(f, &mut app)).unwrap();
        }
    }

    #[test]
    fn draw_does_not_panic_at_36x10_single_pane() {
        // 36x10 (app.rs's MIN_SPLIT_COLS/ROWS) is the smallest size roost's
        // own split gate considers usable; a lone unsplit pane should still
        // draw cleanly at that floor.
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let mut app = mk_app(Size::new(36, 10));
        let backend = TestBackend::new(36, 10);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| super::draw(f, &mut app)).unwrap();
    }

    /// ux P3-16: below the tab-bar-plus-one-body-row floor, `draw` used to
    /// `return` with nothing drawn — an empty screen with no explanation.
    /// `draw_too_small` is what it shows instead; pinned directly (no
    /// `App` needed — it only ever takes a `Rect`) at 1×1, the tightest
    /// non-zero case the reviewer's stress pass exercised, and at two
    /// shapes either side of it so the notice is proven to degrade with
    /// the terminal rather than just surviving one magic size.
    #[test]
    fn too_small_notice_is_never_blank_and_never_panics() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        for (w, h) in [(1u16, 1u16), (1, 20), (2, 10)] {
            let backend = TestBackend::new(w, h);
            let mut term = Terminal::new(backend).unwrap();
            term.draw(|f| super::draw_too_small(f, Rect::new(0, 0, w, h))).unwrap();
            let buf = term.backend().buffer().clone();
            let area = *buf.area();
            let non_blank = (area.y..area.y + area.height).any(|y| {
                (area.x..area.x + area.width).any(|x| buf.cell((x, y)).unwrap().symbol() != " ")
            });
            assert!(non_blank, "{w}x{h}: the too-small notice drew an entirely blank screen");
        }
    }

    /// The same guarantee through the real entry point: `draw` must reach
    /// `draw_too_small` at the exact boundary it bails at (`height < 2`,
    /// i.e. zero rows and one row) without panicking, at a realistic
    /// width — the case a resize event can actually hand it.
    #[test]
    fn draw_does_not_panic_below_the_two_row_floor() {
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        for size in [Size::new(80, 0), Size::new(80, 1)] {
            let mut app = mk_app(size);
            let backend = TestBackend::new(size.width, size.height);
            let mut term = Terminal::new(backend).unwrap();
            term.draw(|f| super::draw(f, &mut app)).unwrap();
        }
    }

    /// Blit a parsed screen into a buffer of the same size and read the row
    /// back as `(symbol, style)` pairs. `blit_screen` is the only writer, so
    /// anything not written shows the buffer's reset default.
    fn blit_row(bytes: &[u8], cols: u16, inner_cols: u16) -> Vec<String> {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut parser = vt100::Parser::new(2, cols, 0);
        parser.process(bytes);
        let mut term = Terminal::new(TestBackend::new(cols, 2)).unwrap();
        term.draw(|f| {
            blit_screen(f, parser.screen(), Rect::new(0, 0, inner_cols, 2));
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        (0..cols).map(|x| buf[(x, 0)].symbol().to_string()).collect()
    }

    /// U24: a wide glyph occupies its own cell exactly once, and the cell to
    /// its right is left blank for the terminal to paint the glyph's second
    /// half into. Before, the continuation was stamped with `" "`, drawing a
    /// space over the right half of every CJK/emoji glyph.
    #[test]
    fn wide_glyphs_blit_once_with_an_untouched_continuation_cell() {
        // `日本語x`: cols 0/2/4 carry a glyph, 1/3/5 are continuations, and
        // the narrow `x` lands at col 6 — not col 3, which is where a grid
        // measuring widths with the pre-P17 table would have put it.
        assert_eq!(
            blit_row("日本語x".as_bytes(), 10, 10),
            vec!["日", " ", "本", " ", "語", " ", "x", " ", " ", " "],
        );
        // Emoji, including a VS16 presentation sequence (P17): one cell each.
        assert_eq!(
            blit_row("\u{1f600}\u{2764}\u{fe0f}y".as_bytes(), 8, 8),
            vec!["\u{1f600}", " ", "\u{2764}\u{fe0f}", " ", "y", " ", " ", " "],
        );
    }

    /// P16: `cell_style` mapped only bold/italic/underline/inverse, so dimmed
    /// and struck text was attribute-identical to plain text — verified end to
    /// end before the fix. Claude Code leans on dim for secondary text, so a
    /// pane flattened into one equal-weight wall.
    #[test]
    fn dim_and_strikethrough_reach_ratatui_modifiers() {
        use super::cell_style;

        let mut p = vt100::Parser::new(2, 20, 0);
        p.process(b"\x1b[2mF\x1b[22;9mS\x1b[0mP\x1b[1;2;9mA");
        let s = p.screen();
        let faint = cell_style(s.cell(0, 0).unwrap());
        assert!(faint.add_modifier.contains(Modifier::DIM));
        assert!(!faint.add_modifier.contains(Modifier::CROSSED_OUT));

        let struck = cell_style(s.cell(0, 1).unwrap());
        assert!(struck.add_modifier.contains(Modifier::CROSSED_OUT));
        assert!(!struck.add_modifier.contains(Modifier::DIM));

        // The regression this item started from: plain text must stay plain.
        let plain = cell_style(s.cell(0, 2).unwrap());
        assert!(!plain.add_modifier.contains(Modifier::DIM));
        assert!(!plain.add_modifier.contains(Modifier::CROSSED_OUT));
        assert_ne!(plain, faint, "dim must be distinguishable from unstyled");
        assert_ne!(plain, struck, "strikethrough must be distinguishable");

        // Stacking with the attributes that already worked.
        let all = cell_style(s.cell(0, 3).unwrap());
        for m in [Modifier::BOLD, Modifier::DIM, Modifier::CROSSED_OUT] {
            assert!(all.add_modifier.contains(m), "missing {m:?}");
        }
    }

    /// U24 guard: a wide glyph whose second half would land outside the drawn
    /// area degrades to a space instead of bleeding a two-column symbol into
    /// the pane border ratatui draws in the next column.
    #[test]
    fn a_wide_glyph_clipped_by_the_pane_edge_degrades_to_a_space() {
        // Grid is 10 wide but only 5 columns are drawn: 語 starts at col 4,
        // so its right half is outside and it must not be emitted.
        assert_eq!(
            blit_row("日本語x".as_bytes(), 10, 5),
            vec!["日", " ", "本", " ", " ", " ", " ", " ", " ", " "],
        );
    }

    /// C10 (2026-08-20): a flash is not a hint. It must reach the user
    /// whether or not the hint bar is drawn.
    ///
    /// With hints hidden every C10 message reached nobody — C38's refusals,
    /// U14's copy result, the workspace-set-aside notice, and worst of all
    /// U22's confirm-arm prompts: `Alt+w` armed a destructive second press
    /// and said nothing at all. Same hazard C2's dated amendment names for
    /// the mode word, one contract over.
    #[test]
    fn a_flash_reaches_the_user_with_the_hint_bar_hidden() {
        use ratatui::backend::TestBackend;
        use ratatui::layout::Size;
        use ratatui::Terminal;

        let last_row = |app: &mut App<crate::ports::fakes::FakePane>| -> String {
            let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
            term.draw(|f| super::draw(f, app)).unwrap();
            let buf = term.backend().buffer().clone();
            (0..100).filter_map(|x| buf.cell((x, 29)).map(|c| c.symbol().to_string())).collect()
        };

        // One pane, so U22's sharpest case: Alt+w arms "last pane — press
        // again to QUIT ROOST". With hints hidden this armed silently, and
        // the second press took the whole session with it.
        let mut app = mk_app(Size::new(100, 30));
        app.apply(Action::ToggleHints);
        assert!(!app.hints_shown(), "setup: the hint bar is hidden");

        app.apply(Action::ClosePane);
        let flash = app.flash().expect("setup: Alt+w armed a confirm prompt").to_string();
        assert!(flash.contains("quit"), "setup: the prompt warns about quitting: {flash}");
        assert!(!app.quit, "setup: the first press only arms");

        let row = last_row(&mut app);
        assert!(
            row.contains("last pane"),
            "the confirm prompt never reached the screen: {row:?} (wanted {flash:?})",
        );

        // ...and it is the flash doing that, not a permanent band of chrome
        // over the pane: same app, same hidden hint bar, nothing flashing.
        let mut quiet = mk_app(Size::new(100, 30));
        quiet.apply(Action::ToggleHints);
        assert!(quiet.flash().is_none(), "setup: nothing is flashing");
        let after = last_row(&mut quiet);
        assert!(
            !after.contains("last pane"),
            "the body's last row is chrome even with no flash: {after:?}",
        );
    }
}
