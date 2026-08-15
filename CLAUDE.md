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
