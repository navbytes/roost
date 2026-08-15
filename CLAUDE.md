# roost — orientation

roost: a session-native terminal multiplexer for AI agent CLIs (pi, Claude
Code, shell), in Rust + ratatui. No daemon — workspace resurrection rides on
each adapter's own session ids.

- **Specs of record:** DESIGN.md (philosophy), DESIGN-ui.md (chrome
  contracts), DESIGN-control.md (control surface), SPEC-ux.md,
  SPEC-parity.md; ROADMAP.md tracks deferred scope.
- TUI chrome is "ink · paper · one red" — terminal-theme-inherited, no fixed
  RGB, no fills; any `src/ui/**` change gets a design-supervisor audit.
- Tests: `cargo test` (unit + PTY harness integration under `tests/`).
  CI: `.github/workflows/ci.yml`.
- Conventions: single writer per file-set; effort scales to task weight;
  durable lessons/decisions get promoted to nt, not left in engagement docs.
- Records of past multi-agent engagements live in `docs/engagements/`
  (plans, worker scopes, handoffs, reports — historical, not specs).
- **Releasing is PR-mergeable end to end** — bump the version everywhere
  (Cargo.toml + Cargo.lock, README badge and pin examples, landing page),
  then touch `.github/release-request` and merge: the request workflow
  dispatches Release, which builds four targets, **creates the `v*` tag
  itself** from Cargo.toml's version, publishes with SHA256SUMS.txt, and
  syncs the Homebrew tap. Built this way (v0.1.7, 2026-08-15) because
  agent-session credentials can merge PRs but get 403 on both
  `refs/tags/*` pushes and the workflow-dispatch API — don't re-derive
  that. Human tag pushes and the Actions "Run workflow" button still work.
