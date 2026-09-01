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
  syncs the Homebrew tap. **The tap sync works on its own now** — the
  `HOMEBREW_TAP_TOKEN` secret got configured sometime after 2026-08-21:
  on v0.1.15 (2026-09-01) the workflow itself pushed `roost 0.1.15` to
  navbytes/homebrew-tap one second after publishing, verified
  byte-identical to a hand-render. So do NOT hand-sync by default; the
  hand-sync era (every release v0.1.5→v0.1.12; v0.1.8/v0.1.9 missed and
  the tap quietly served 0.1.8 until tap PRs #3–#5 caught it up) is
  history. After a release, confirm the tap's `Formula/roost.rb` hit the
  new version; only if the sync step skipped or failed, fall back to
  rendering with
  `RENDER_ONLY=1 RELEASE_TAG=vX.Y.Z scripts/update-homebrew-formula.sh`
  (SUMS_FILE pointed at the release's SHA256SUMS.txt) and PR the result
  to navbytes/homebrew-tap. Built this way (v0.1.7,
  2026-08-15) because
  agent-session credentials can merge PRs but get 403 on both
  `refs/tags/*` pushes and the workflow-dispatch API — don't re-derive
  that. Human tag pushes and the Actions "Run workflow" button still work.
  **Keep the version bump as a separate second commit** on the release PR:
  GitHub defaults a squash subject to the sole commit's subject when a PR
  has exactly one commit — the PR title only wins with two or more, so a
  one-commit release PR lands on `main` under the feature's subject instead
  of `vX.Y.Z everywhere: …` (v0.1.13, commit cb4747c). Nothing downstream
  breaks — the release keys off Cargo.toml — but the log stops being
  readable by release.
- **`main` is protected** (2026-08-21): PRs required, both CI matrix jobs
  must be green, no force-push, no branch deletion, zero required
  approvals so a solo maintainer can self-merge. Admins are exempt, and
  agent sessions run as the repo admin — so the gate is a guardrail, not a
  wall, and a direct push to `main` will still land. `v*` tags are
  immutable (a ruleset blocks delete and force-update; creation stays open
  so release.yml can make its own tag). ci.yml has no `paths-ignore`: a
  required check that never triggers never reports, and a docs-only PR
  would wait for a status forever.
