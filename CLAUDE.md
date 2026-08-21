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
- **Formatting and lints are enforced** (2026-08-21, reversing the old "roost
  is not rustfmt-formatted, never run `cargo fmt` here" rule — that is no
  longer true and following it now fails CI). Run `cargo fmt` and
  `cargo clippy --all-targets -- -D warnings` before pushing; CI's `lint` job
  runs both. `rustfmt.toml` is tuned to the style the tree already had
  (`max_width = 100`, `use_small_heuristics = "Max"` — measured as 3x less
  churn than the default heuristics), and the lint set lives in `Cargo.toml`'s
  `[lints]` tables so it applies to every local `cargo check`, not only to CI.
  The one-time reformat is listed in `.git-blame-ignore-revs`; run
  `git config blame.ignoreRevsFile .git-blame-ignore-revs` once so blame skips
  it. `vendor/vt100` is a path dependency, not a workspace member, so neither
  tool reaches it — deliberate, it is third-party code carrying roost patches.
  **The lint job pins its toolchain** (`LINT_TOOLCHAIN` in ci.yml): `-D
  warnings` against a moving `stable` breaks green branches on a clippy
  release, and a drifted local toolchain then cannot reproduce it. Lint
  locally with the same one — `cargo +$LINT_TOOLCHAIN clippy --all-targets --
  -D warnings` — or you will find out on CI. Only the lint job is pinned;
  build, test and release deliberately still float on the runner's stable.
- Records of past multi-agent engagements live in `docs/engagements/`
  (plans, worker scopes, handoffs, reports — historical, not specs).
- **Releasing is PR-mergeable end to end** — bump the version everywhere
  (Cargo.toml + Cargo.lock, README badge and pin examples, landing page),
  then touch `.github/release-request` and merge: the request workflow
  dispatches Release, which builds four targets, **creates the `v*` tag
  itself** from Cargo.toml's version, publishes with SHA256SUMS.txt, and
  syncs the Homebrew tap — but the tap sync needs the `HOMEBREW_TAP_TOKEN`
  secret, which is **still not configured** as of 2026-08-21 (the step skips
  with a warning). **Every release since v0.1.5 has needed a hand-sync, and
  v0.1.8 and v0.1.9 did not get one** — the tap sat on 0.1.8 for two
  releases while `brew install roost` quietly served it. So the sync is not
  optional cleanup: render with
  `RENDER_ONLY=1 RELEASE_TAG=vX.Y.Z scripts/update-homebrew-formula.sh`
  and PR the result to navbytes/homebrew-tap as part of the release, or set
  the secret and stop paying this every time. Built this way (v0.1.7,
  2026-08-15) because
  agent-session credentials can merge PRs but get 403 on both
  `refs/tags/*` pushes and the workflow-dispatch API — don't re-derive
  that. Human tag pushes and the Actions "Run workflow" button still work.
- **`main` is protected** (2026-08-21): PRs required, both CI matrix jobs
  must be green, no force-push, no branch deletion, zero required
  approvals so a solo maintainer can self-merge. Admins are exempt, and
  agent sessions run as the repo admin — so the gate is a guardrail, not a
  wall, and a direct push to `main` will still land. `v*` tags are
  immutable (a ruleset blocks delete and force-update; creation stays open
  so release.yml can make its own tag). ci.yml has no `paths-ignore`: a
  required check that never triggers never reports, and a docs-only PR
  would wait for a status forever.
