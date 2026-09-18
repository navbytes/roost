# projectmem - roost

_Last updated: 2026-09-18_

## Project purpose
A session-native terminal multiplexer for AI agent CLIs (pi, Claude Code, ...). No daemon: layouts and session IDs persist; processes are disposable.

## Recent issues
- [DONE] #legacy_cedf Legacy issue: v0.1.21 everywhere: stack focus fix shipped, expansion-stability fuzz property, release request (#185) -> v0.1.21 everywhere: stack focus fix shipped, expansion-stability fuzz property, release request (#185) (fixed)

## Decisions
- Refactor: Refactor code structure for improved readability and maintainability [.gitignore]

## Notes
- archive named-workspaces; sync its three capability specs to main
- archive named-workspaces; sync its three capability specs to main (#182)
- Add a stall watchdog: catch the main loop wedging, with a real backtrace (#183)
- stack: a focused member is always the expanded one; retire C7's edge marker (#184)
- Solo view (C43): one pane at a time, the rest listed in a per-tab rail (#186)
- v0.1.22 everywhere: Cargo, README, landing page, release sentinel (#187)
- Synchronise the claim scenarios on the claim, not on settle (#188)
- Sweep the remaining hand-rolled deadline loops onto wait_until (#189)
- Finish the deadline-loop sweep onto wait_until (#189 remainder) (#190)
- feat: add new skills for source command operations

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
