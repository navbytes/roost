# projectmem - roost

_Last updated: 2026-09-19_

## Project purpose
A session-native terminal multiplexer for AI agent CLIs (pi, Claude Code, ...). No daemon: layouts and session IDs persist; processes are disposable.

## Recent issues
- [DONE] #legacy_cedf Legacy issue: v0.1.21 everywhere: stack focus fix shipped, expansion-stability fuzz property, release request (#185) -> v0.1.21 everywhere: stack focus fix shipped, expansion-stability fuzz property, release request (#185) (fixed)
- [DONE] #0001 4ef5ee1's same_dir kept spec.cwd non-canonical; session_root(&spec.cwd) then missed the agent's session dir for symlinked cwds (no resume) [src/core/app.rs] -> Reverted 4ef5ee1 on main (6fa8dea); re-land in PR #191 drops same_dir, restoring the per-tick overwrite of spec.cwd with the kernel-resolved cwd. cargo test green. [src/core/app.rs] (fixed)

## Decisions
- Refactor: Refactor code structure for improved readability and maintainability [.gitignore]
- Any chrome text cut short ends in `…` (theme::OVERFLOW) via clip_spans/elide_to; roster+feed overlays size to max(unfiltered fleet, feed), capped, body-centred (DESIGN-ui.md 2026-09-18 amendments) [src/ui/render.rs]
- 4ef5ee1's changes shipped in corrected form as #191 (… clip marker, content-sized body-centred fleet overlays, picker opens on an installed adapter); its same_dir cwd comparison was dropped as a session-resume regression [src/core/app.rs]
- A text dialog (rename/pane editor/broadcast) holding unsaved typing refuses stray Alt chords with a flash (↵ saves · Esc discards); its own entry chord and Alt+q still exit. Resolves DESIGN-ui.md §7 (#192) [src/core/app.rs]
- Palette (#193): read-only rows' keys draw accent_quiet and the ❯ row lifts to ink while the filter is open. Solo rail (#194): glyph tier 12 cols with id+name; labelled tier stays clamp(w/5,20,32) — content-sized width was reverted because live OSC titles resized the shown pane's PTY [src/ui/render.rs]

## Notes
- High churn detected: src/core/app.rs (4 edits in 10 min) [src/core/app.rs]
- gotcha: spec.cwd must stay the kernel-resolved path — adapters encode it literally into session dirs (claude encode_cwd), so never keep a symlinked spelling for display's sake [src/core/app.rs]
- lesson: frame-level UX audits work by driving the real binary through tests/harness (spawn_or_skip_sized + vt100 screen dump); allow ~700ms settle per key, and vt100 cell contents() is "" for never-written blanks [tests/harness/mod.rs]
- High churn detected: src/core/app.rs (4 edits in 10 min) [src/core/app.rs]
- High churn detected: src/ui/render.rs (4 edits in 10 min) [src/ui/render.rs]
- High churn detected: DESIGN-ui.md (4 edits in 10 min) [DESIGN-ui.md]
- Merge: Refuse, don't discard: Alt chords no longer throw away unsaved notes, renames or broadcasts (#192)
- High churn detected: src/ui/render.rs (4 edits in 10 min) [src/ui/render.rs]
- gotcha: tests/firehose.rs fails deterministically (echo of 'g' within 250ms) when run from a checkout with a long directory name — the shell prompt shows the cwd basename and typed text wraps in pane B's ~58 cols. Run it from a short path before calling it a regression. [tests/firehose.rs]
- Merge: Solo rail: name the panes at 80 cols, not just their ids (#194)

## Key files
- `.github/workflows/release.yml`
- `CLAUDE.md`
- `.github/release-request`
- `Cargo.lock`
- `Cargo.toml`
- `README.md`
- `docs/index.html`
- `v0.1.18`
- `DESIGN.md`
- `ROADMAP.md`
- `docs/KEYBINDINGS.md`
- `src/cli.rs`
- `src/infra/config.rs`
- `tests/config_keys.rs`
- `tests/harness/mod.rs`
- `config.json`
- `/.config`
- `v0.1.19`
- `.claude/commands/opsx/apply.md`
- `.claude/commands/opsx/archive.md`

## Open questions
- None logged yet.
