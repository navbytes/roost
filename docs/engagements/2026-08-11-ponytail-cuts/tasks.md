# Worker scopes (disjoint file sets — one builder per set)

- A vendor: vendor/vt100/** + README.md (fork-claim sentence only)
- B tests: tests/** (delete live_qa+ux_nav_qa, spawn_or_skip, fixture fns)
- C infra: src/infra/sock.rs, src/infra/pty.rs
- D core: src/core/** (app, status, control, layout, session_resolver)
- E agents: src/agents/**, extensions/roost.ts
- F ui+main: src/ui/**, src/ports.rs, src/main.rs, src/infra/notify.rs, Cargo.toml (fs2)

Integration (Principal): cargo build + full test suite, fix loop, verify wave
(reviewer, design-supervisor), final report.
