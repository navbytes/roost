# Security audit — control surface (security-auditor, 2026-08-06)

Threat model: a compromised/malicious AI agent *inside a pane* is the primary
adversary. Verdict: **fix-first** — one High escapes roost's rendering sandbox
into the operator's real terminal; the rest are containment/accountability
gaps that don't block a considered ship.

## H1 — Escape-sequence injection into the HOST terminal via pane cwd/title (CONFIRMED)

`app.rs:1934-1936` writes `ESC]2;` + `feed_label(focused)` + BEL straight to
roost's stdout, bypassing ratatui (which *does* filter control chars —
ratatui-core/buffer.rs:351). The label is `display_name_live` (app.rs:4726) →
`spec.title` verbatim, or `cwd.file_name()` verbatim. Only the *live OSC*
title is sanitized (`sanitize_title`, app.rs:4749).

Attack: in-pane agent runs
`mkdir -p "/tmp/$(printf '\e]52;c;<b64>\a')"` then
`roost spawn shell --cwd '/tmp/<that>'` (ctl_spawn app.rs:1590, cwd
unvalidated). When the human focuses that pane, roost emits the attacker's
bytes to their real terminal: OSC 52 clipboard poisoning, title spoofing,
any host-supported sequence. Same via hand-edited `title` in workspace.json.
The comment at app.rs:1927 asserting "nothing here can carry a control byte"
is false for both inputs.

**Fix:** `sanitize_title(&want, cap)` in `sync_host_title` before queueing.

## M1 — The "private" scratch float is readable/writable over the control plane (CONFIRMED)

`ctl_close` (app.rs:1748) and `ctl_broadcast` (app.rs:1690) exclude the float;
`ctl_read` (app.rs:1716) and `ctl_send` (app.rs:1643) do not, and `find_spec`
resolves float ids (app.rs:1114). Any agent that reads `control.token`
(documented as possible) can `roost read <n>` over small ids and capture the
human's private scratch shell — the one pane the design says is never a fleet
member. **Fix:** same `is_float` refusal in ctl_read/ctl_send. Also README:311
claims broadcast reaches the float; code always excludes it (doc bug).

## M2 — Audit log not tamper-evident against the adversary it exists to hold accountable (CONFIRMED)

`audit` (app.rs:1393) appends to `<state>/control.log`; `rotate_audit_log`
(app.rs:4779) renames at 4 MiB keeping one generation. A same-uid pane can
truncate it, or — with no rate cap — flood ~200k trivial `list` calls to roll
it twice and erase all prior evidence. DESIGN-control.md §Open-decisions-3
rests the whole fallback position on this file. **Fix:** rate cap (M3) so
rotation can't be forced, plus optional out-of-process sink (syslog /
inherited `ROOST_AUDIT_FD`); at minimum document that the log is advisory
against a same-uid adversary.

## M3 — §5.6 unimplemented: no per-principal connection or rate cap (CONFIRMED)

`sock.rs:148` is a single global MAX_CONN=64. One pane opens 64 connections
(or 16 long waits, MAX_WAITS app.rs:224) and locks the human's CLI and the
real orchestrator out — exactly the starvation §5.6 predicted. No fork-bomb
depth/budget guard either; spawn is bounded only accidentally by
MIN_SPLIT_COLS. **Fix:** per-token connection cap (~8) + token bucket keyed
on resolved `Actor`.

## L1 — Control spawn splits the *human's* active tab (CONFIRMED)

`spawn_child` (app.rs:2884) uses `ws.active_tab()`/`self.focused`; focus and
tab are restored, the split is not. A background agent rearranges and shrinks
the layout the human is looking at; with M3, until "not enough room to split".

## L2 — `write_control_token` can't reset an existing file's mode (CONFIRMED)

main.rs:448 `.mode(0o600)` applies at creation only; a token left by a crash
keeps whatever mode it had. **Fix:** unlink then `create_new`, or
`set_permissions` after open.

## L3 — Pane token/socket leak when the listener fails to bind (PLAUSIBLE)

main.rs:238 uses `.ok()`; `spawn_pane` (app.rs:4498) sets ROOST_SOCK/
ROOST_TOKEN only when `sock_path` is Some, so panes inherit roost's own env —
a nested roost hands its children the *outer* pane's live token and socket.
**Fix:** `env_remove` both unconditionally before conditionally setting.

## Info

(a) sock.rs:148 conn-cap load-then-add races a few over 64. (b) notify.rs:17
interpolates pane text into AppleScript with only `"`→`'` / `\`→`""`; no
break-out found (vte drops C0 in OSC payloads) but it's quoting-by-luck —
pass the body as an `on run {b}` argument. (c) DESIGN-control.md §5.3 + §8
checklist still claim "control verbs rejected from any pane token"; code
accepts pane tokens for every verb (app.rs:1243). §Open-decisions-3
supersedes it — the stale checklist should say so.

## Clean (verified, no gap)

Authz uniform across all nine verbs incl. the `wait` re-check at fire time
(app.rs:1502). No shell interpolation anywhere (adapter registry-bounded,
argv only). `valid_session_id` (agents/mod.rs:140) blocks argument
injection/traversal. Pane-id recycling can't re-parent a live subtree
(workspace.rs:57). `sanitize` (app.rs:4469) blocks audit-log line forgery.

## §5.5 / §5.6 — defensible to ship?

Yes, **with M1 fixed**. §5.5 is moot as a boundary — the token file is
same-uid readable, so consent-gating reads buys nothing against a
shell-capable agent — *except* for the float, which the design does claim as
private. §5.6 is weaker: not just starvation, it's the enabler for M2's
log-rolling, and it's the cheapest fix here. Minimum to ship: H1, M1, M3.
Consent-gated reads can stay unbuilt.

## Not checked

Runtime/live behavior; ui/mouse.rs + ui/input.rs; the vendored vt100 parser
generally (only OSC 9/52/777 arms); extensions/roost.ts + Claude hooks;
inspect.rs adapter promotion (a pane can `exec -a pi` to relabel — cosmetic
as traced); clipboard.rs/open.rs argv paths; dependency CVEs (cargo audit not
run); Linux XDG_RUNTIME_DIR split where socket dir ≠ state dir (state dir
gets no `dir_is_private_and_ours` check, unlike sock.rs:132).
