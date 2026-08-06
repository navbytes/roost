# Competitive research — AI-agent session/fleet multiplexers, 2026

Lens: researcher · read README.md, DESIGN.md §1–2, ROADMAP.md, Cargo.toml,
ci.yml; verified via `gh release list` that zero releases exist (install is
cargo-build-only).

## (a) Competitor → differentiators → what roost lacks

| Tool | Differentiators | What roost lacks |
|---|---|---|
| claude-squad (6.8k★, Go+tmux) | Git-worktree-per-agent, diff review before apply, auto-accept, 6 CLIs | Worktree/branch isolation; diff-review UI; CLI breadth |
| bosun (Rust+ratatui, near-identical pitch) | tmux-native detach/SSH, Homebrew+cargo+binaries+self-update, editable resume spec | Any prebuilt install channel; tmux-riding detach |
| agent-mux | Live output preview pane per agent, grouped by workspace | Content preview in roster (roost shows id/name/status only) |
| tmux-agent-sidebar | Streams prompt/tool-call text live across sessions | Same — activity feed logs status changes, not content |
| agent-manager | 5 CLIs incl. Grok, prompt templates, Windows/WSL2 | Grok adapter, template library, Windows |
| VibeTunnel (4.6k★) | Browser + iOS remote access, asciinema recording | Any remote/mobile access |
| Zellij ≥0.43 | Built-in web client, bookmarkable URLs, read-only observer tokens | Same |
| Claude Code itself | Agent Teams (peer sub-agents, Feb 2026); `--teleport` cloud→local resume | Teams-awareness in claude panes; teleport resume path |

## (b) Table stakes ranked by demand

1. Prebuilt install (Homebrew/binaries) — HIGH; roost ships 0 channels, every peer ships 3+.
2. Cross-CLI breadth (codex, gemini minimum) — HIGH.
3. Git-worktree isolation for same-repo parallel agents — MED-HIGH (claude-squad's most-cited value prop).
4. Remote/mobile check-in — MED (normalized by Zellij web, VibeTunnel, Cowork).
5. Send-never-races-child-readiness — MED, as a *stated proof* (claude-squad's most-commented bug is exactly this; roost already prevents it by design).
6. Sync panes / session groups / plugins — LOW for fleet users; skip.

## (c) Adapters + resume mechanisms

- codex: `codex resume <SESSION_ID>` / `resume --last`; JSONL `~/.codex/sessions/YYYY/MM/DD/`. Do first.
- gemini: `gemini --resume <index|UUID>`; project-scoped history; optional `--checkpointing`. Second.
- opencode: `--session <id>` / `--continue`; defend against sst/opencode#2086 (continue grabs subagent thread) by always resuming explicit id. Third.
- amp: thread-based (`amp threads continue`), breaks (adapter,cwd,session-id) — last, if ever.
- aider: no session-id resume; `.aider.chat.history.md` per dir; heuristic-only tier — skip.
- claude adapter note: `~/.claude/projects/<encoded-cwd>/*.jsonl` confirmed still exact-cwd-scoped in 2026 → worktrees get clean session namespaces for free. `--teleport` is a distinct unverified path (cloud Cowork session, not local jsonl).

## (d) Distribution

Nothing exists: no crates.io, no tap, no binstall metadata, no GitHub releases.
Peers: bosun (tap + cargo install + binaries + self-update), claude-squad
(tap + curl installer), VibeTunnel (cask + npm + binaries).

## (e) Top-5 moves

1. Homebrew tap + GitHub Release binaries now — roost is invisible without a Rust toolchain.
2. codex + gemini adapters — closes the biggest capability gap.
3. Opt-in `roost spawn --worktree` — neutralizes claude-squad's headline without abandoning zero-config.
4. Thin opt-in read-only remote status ("did my agent finish" from a phone) — DECIDE first: collides with HTTP-transport descope + §5.5 consent posture.
5. Market the send/wait no-race guarantee explicitly in README.

Sources: github.com/smtg-ai/claude-squad · github.com/yetidevworks/bosun ·
github.com/leonardcser/agent-mux · github.com/hiroppy/tmux-agent-sidebar ·
github.com/YoanWai/agent-manager · github.com/amantus-ai/vibetunnel ·
zellij.dev/tutorials/web-client · code.claude.com/docs/en/claude-code-on-the-web ·
deepwiki.com/openai/codex · developers.googleblog.com (gemini session mgmt) ·
opencode.ai/docs/cli · ampcode.com/manual · rust-cli.github.io/book/tutorial/packaging
