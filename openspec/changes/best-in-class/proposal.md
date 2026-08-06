# Proposal: best-in-class

## Why

roost's core is complete and green, but "complete" is not "category-best."
The client wants every issue, gap, and UI/UX gap identified, folded into one
living phased plan, and fixed — autonomously, PR by PR — until roost is the
best session-native terminal multiplexer for AI-agent fleets.

## What Changes

- A five-lens audit (UX, code review, security, black-box QA, competitive
  research) produces findings; each lands in **PLAN.md** (the living phased
  document) and stays there until shipped or explicitly descoped.
- Fixes ship phase by phase: each phase is one or more small PRs, CI-green,
  design-supervisor-audited when `src/ui/**` is touched, auto-merged under
  the client's standing low-risk-merge rule, riskier merges batched for
  client ratification in the final report.
- ROADMAP.md is updated as items ship so it stays the public truth;
  PLAN.md is the engagement's working ledger.
- Non-goals: items ROADMAP marks **[descoped]** (MCP bridge, event
  subscription, HTTP transport) stay descoped unless the competitive
  research shows a concrete user demand; no rewrite of the layout engine;
  no new chrome that violates DESIGN-ui.md's ink-paper-red stance.

## Impact

- Affected: potentially all of `src/`, `tests/`, docs, CI, packaging.
- Risk posture: read-only audit first; every behavior change gated by
  tests + review; single writer per file-set per phase.
- Done means: all P0/P1 findings shipped or descoped with rationale,
  verify wave clean (tester, reviewer, qa-verifier, design-supervisor,
  security-auditor on touched surfaces), final report with evidence.
