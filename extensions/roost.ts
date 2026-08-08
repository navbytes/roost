/**
 * roost.ts — pi extension reporting exact agent status to roost.
 *
 * Install: roost installs/updates this automatically at startup when pi is
 * present (into ~/.pi/agent/extensions/roost.ts); set ROOST_NO_EXT_INSTALL to
 * manage it yourself. If pi is not running inside roost (no ROOST_PANE env var
 * or no socket), this extension no-ops at zero cost.
 *
 * Events reported over the unix socket ($XDG_RUNTIME_DIR/roost.sock or
 * ~/.local/state/roost/roost.sock), one JSON object per line. Every message
 * carries the pane's ROOST_TOKEN; roost rejects any whose token doesn't match
 * the pane it claims to be, so one pane can't spoof another's status/session:
 *   { pane, token, event: "session"  , session: "<uuid>" }
 *   { pane, token, event: "status"   , status: "working" | "waiting" | "needs_input" }
 *
 * A needs_input status may carry an optional `message` — the question the
 * agent is asking (extracted from the ask-tool's args) — which roost surfaces
 * in the feed line and the desktop notification. Best on pi ≥ 0.80.4
 * (agent_settled + isIdle); an older pi falls back to agent_end reporting —
 * see the settled handler.
 *
 * promotion-auth-gate: reconnects with backoff on `error`/`close` (up to 5
 * attempts, 250ms doubling to 4s, ±25% jitter; the budget refills once a
 * connection has stayed up 30s) and replays the last known session/status on
 * reconnect, so a transient drop — including a new roost's auth gate closing
 * an old connection that was never authenticated (see the door in
 * `src/infra/sock.rs`) — costs at most one stale status for seconds, not the
 * rest of the session. See `send()` for why a disconnected send must itself
 * kick a reconnect rather than silently drop.
 */
import * as net from "node:net";
import * as os from "node:os";
import * as path from "node:path";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export default function (pi: ExtensionAPI) {
  const pane = process.env.ROOST_PANE;
  if (!pane) return; // not running inside roost
  // Only the pane's own interactive agent may report. The ROOST_* env is
  // inherited by every descendant process, so a *nested* pi — a subagent, a
  // one-shot `pi -p` run by a tool of the pane's agent (pi's or even a
  // claude pane's) — would otherwise authenticate as the pane, overwrite its
  // persisted session id with its own (breaking resume into the wrong
  // conversation) and flap its status. Nested invocations run with piped
  // stdio; the agent that owns the pane — including one launched by hand in
  // a shell pane — runs on the pane's tty.
  if (!process.stdout.isTTY) return;
  // Per-pane secret roost issued to this child; every message must carry it or
  // roost drops it (prevents one pane spoofing another over the shared socket).
  const token = process.env.ROOST_TOKEN ?? "";

  const sockPath =
    process.env.ROOST_SOCK ??
    (process.env.XDG_RUNTIME_DIR
      ? path.join(process.env.XDG_RUNTIME_DIR, "roost.sock")
      : path.join(os.homedir(), ".local", "state", "roost", "roost.sock"));

  // Retry policy (design decision, criterion 6): 5 attempts per burst,
  // 250ms * 2^(n-1) capped at 4s — 250, 500, 1000, 2000, 4000 — ±25% jitter
  // so a dozen panes reconnecting after the same roost restart don't thunder
  // in lockstep. A burst's budget only refills after a connection survives
  // `HEALTHY_AFTER_MS`: a connection that drops again quickly must not get a
  // fresh 5 attempts immediately, or a persistently-broken socket retries
  // forever at full speed.
  const MAX_RETRY_ATTEMPTS = 5;
  const BASE_DELAY_MS = 250;
  const MAX_DELAY_MS = 4000;
  const JITTER_FRACTION = 0.25;
  const HEALTHY_AFTER_MS = 30_000;

  let sock: net.Socket | null = null;
  let attempt = 0; // attempts already spent in the current burst
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let healthyTimer: ReturnType<typeof setTimeout> | null = null;
  let shuttingDown = false; // session_shutdown initiated the close — no retry
  // Last known session/status (and the status's message, so a replayed
  // needs_input keeps its question), kept up to date by every send() (even
  // while disconnected) so a reconnect can replay it — see the "connect"
  // handler.
  const last: { session?: string; status?: string; message?: string } = {};

  const clearHealthyTimer = () => {
    if (healthyTimer) {
      clearTimeout(healthyTimer);
      healthyTimer = null;
    }
  };

  // Schedule the next attempt in the current burst, or do nothing once one
  // is already pending or the burst is spent — `close` and `error` both fire
  // for the same failure (Node emits `close` after `error`), so this must be
  // idempotent per failure, not just per call.
  const scheduleReconnect = () => {
    if (shuttingDown || reconnectTimer || attempt >= MAX_RETRY_ATTEMPTS) return;
    const backoff = Math.min(BASE_DELAY_MS * 2 ** attempt, MAX_DELAY_MS);
    attempt += 1;
    const jitter = backoff * JITTER_FRACTION * (Math.random() * 2 - 1); // ±25%
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      connect();
    }, Math.max(0, Math.round(backoff + jitter)));
  };

  const connect = () => {
    clearHealthyTimer();
    const s = net.connect(sockPath);
    sock = s;
    s.on("connect", () => {
      if (sock !== s) return; // superseded by a newer attempt
      // Re-establish App's link refcount (a link-up requires a line) and
      // hand back the exact status a false rejection would otherwise have
      // cost — every reconnected socket's first line also authenticates it,
      // so it's never idle-killed by the pre-auth gate again either.
      if (last.session !== undefined) send({ event: "session", session: last.session });
      if (last.status !== undefined)
        send({
          event: "status",
          status: last.status,
          ...(last.message !== undefined ? { message: last.message } : {}),
        });
      clearHealthyTimer();
      healthyTimer = setTimeout(() => {
        attempt = 0;
        healthyTimer = null;
      }, HEALTHY_AFTER_MS);
    });
    s.on("error", () => {
      if (sock !== s) return;
      sock = null;
      clearHealthyTimer();
      scheduleReconnect();
    });
    s.on("close", () => {
      if (sock !== s) return;
      sock = null;
      clearHealthyTimer();
      if (!shuttingDown) scheduleReconnect();
    });
  };
  connect();

  // A disconnected gap with nothing pending must not just sit there: a
  // >15s pi cold start would otherwise burn all five retries on idle
  // connections (five pre-auth 2s kills, since nothing is sent before
  // session_start fires) before session_start ever calls send() for real,
  // leaving the session permanently silent. A real send() matters more than
  // an idle burst's history, so it starts a fresh one; it does NOT kick if a
  // reconnect is already in flight (`connect()` itself sets `sock`
  // synchronously, so "no socket and nothing scheduled" only happens once a
  // burst has genuinely run out).
  const kickReconnect = () => {
    if (sock || reconnectTimer) return;
    attempt = 0;
    connect();
  };

  const send = (msg: Record<string, unknown>) => {
    if (msg.event === "session" && typeof msg.session === "string") last.session = msg.session;
    if (msg.event === "status" && typeof msg.status === "string") {
      last.status = msg.status;
      // Overwrite (not preserve) so a stale question never rides along with
      // a later working/waiting replay.
      last.message = typeof msg.message === "string" ? msg.message : undefined;
    }
    if (!sock) {
      kickReconnect(); // must not silently no-op — see the comment above
      return;
    }
    try {
      // ponytail: accepted race — a sub-millisecond window where the server
      // already closed `sock` but the "error"/"close" event hasn't reached
      // this callback yet can silently lose this one write. `last` (above)
      // already recorded it, and the next lifecycle event's send() retries;
      // not worth a write-then-confirm round trip for a one-shot report.
      sock.write(JSON.stringify({ pane, token, ...msg }) + "\n");
    } catch {
      sock = null;
      scheduleReconnect();
    }
  };

  // [F3] project_trust fires on *every* launch of a project that needs a
  // trust decision — remembered or not: pi emits it to extensions before it
  // ever consults the trust store (verified against installed 0.81.1's
  // dist/core/project-trust.js — resolveProjectTrusted calls
  // emitProjectTrustEvent unconditionally, then only falls back to
  // trustStore.get() if no extension decided). Reporting needs_input the
  // instant it fires was wrong: a remembered "yes" resolves in milliseconds
  // and the run proceeds straight through, so every pi launch in an
  // already-trusted project fired a spurious "waiting for you" notification.
  // Fix: schedule the send instead of firing it, and cancel the timer the
  // moment any other lifecycle event proves the run is already moving.
  // Remembered trust ⇒ session_start lands eventually ⇒ cancelled, nothing
  // sent. A real blocking dialog ⇒ nothing else arrives ⇒ needs_input
  // fires, correctly.
  //
  // [F3 residual] session_start is the *earliest* cancellable checkpoint —
  // traced end to end through the installed dist (project-trust.js →
  // resource-loader.js's reload() → main.js's resolveModelScope →
  // createAgentSessionFromServices → agent-session.js's bindExtensions,
  // which is what actually fires session_start): every one of those steps
  // runs between project_trust and session_start, none of them touches the
  // extension runner, so there is no earlier event to hook. On a cold
  // launch that chain (package/resource resolution, model scope
  // resolution) can genuinely run past 400ms, so 400ms was still firing a
  // spurious ◆ on a slow-but-remembered-trust cold start. 1200ms is
  // generous headroom above that while still landing well inside "a human
  // would notice something's wrong" for a real blocking dialog — accept
  // the residual race for a cold start slower than that; nothing shorter
  // of adding a new pi-side event (out of scope here) closes it further.
  let projectTrustTimer: ReturnType<typeof setTimeout> | null = null;
  const cancelProjectTrustNeedsInput = () => {
    if (projectTrustTimer) {
      clearTimeout(projectTrustTimer);
      projectTrustTimer = null;
    }
  };

  pi.on("session_start", async (event, ctx) => {
    cancelProjectTrustNeedsInput();
    // Report the session id so roost can persist it for resume.
    const id = ctx.sessionManager?.getSessionId?.() ?? (event as any)?.sessionId;
    if (id) send({ event: "session", session: id });
    send({ event: "status", status: "waiting" });
  });

  pi.on("agent_start", async () => {
    cancelProjectTrustNeedsInput();
    send({ event: "status", status: "working" });
  });
  // agent_settled (pi ≥ 0.80.4) is the true turn-end: agent_end also fires
  // between an agent run and its automatic follow-ups (retry after a
  // provider error, compaction, a queued continuation), so reporting there
  // unguarded flapped the pane ○ waiting → ● working on every recovery.
  // agent_end stays registered as the fallback for a pi old enough to lack
  // agent_settled — same isIdle guard on both, so wherever isIdle exists a
  // mid-recovery agent_end stays silent, and on a pi with neither
  // refinement the fallback degrades to exactly the old behavior. Verified
  // against installed pi 0.84.1's docs/extensions.md: "ctx.isIdle() is
  // false while Pi is processing an agent run, automatic retry,
  // auto-compaction retry, or queued continuation" — and the same doc
  // recommends agent_settled "for status integrations" outright.
  // A duplicate "waiting" (agent_end then agent_settled, both idle) is
  // harmless — roost's tracker is idempotent on repeated resting reports.
  // Registered inline (not one shared handler) so each ctx keeps pi's real
  // parameter type and tsc actually checks the isIdle call.
  pi.on("agent_settled", async (_event, ctx) => {
    if (ctx.isIdle?.() === false) return;
    send({ event: "status", status: "waiting" });
  });
  pi.on("agent_end", async (_event, ctx) => {
    if (ctx.isIdle?.() === false) return;
    send({ event: "status", status: "waiting" });
  });
  pi.on("input", async () => cancelProjectTrustNeedsInput());

  // "Needs input" — the agent is explicitly blocked on *you*, mid-turn. pi
  // ships no generic per-tool permission/approval prompt at all (verified
  // against installed pi 0.81.1's dist/core/extensions/runner.js — there is
  // no "approval dialog" event of any kind to hook here), so there are
  // exactly two real carriers:
  //
  // 1. project_trust — pi's one built-in *blocking* prompt: whether to trust
  // this project directory, shown before a session can really do anything.
  // Handlers run in registration order; the first to return "yes"/"no" wins
  // and the dialog never shows at all, "undecided" falls through to the next
  // handler and, if none decide, to the interactive dialog itself (see
  // emitProjectTrustEvent in runner.js). We are a status reporter, not a
  // trust policy, so we always report "undecided" — the human's own dialog
  // must still show and decide it. The ◆ this (maybe) reports needs no
  // explicit clear: once the dialog resolves, the run proceeds and
  // agent_start/tool_call send "working" same as any other turn; agent_settled
  // settles "waiting" at the end of it. The NeedsInput time-decay
  // (roost-side, STUCK_WORKING) also backstops a dialog nobody ever answers.
  // `hasUI: false` means no dialog can ever show for this decision at all
  // (see resolveProjectTrusted's own early return for it) — nothing to
  // report as blocked on the human, so don't even schedule the send.
  pi.on("project_trust", async (_event, ctx) => {
    if (ctx.hasUI) {
      cancelProjectTrustNeedsInput();
      projectTrustTimer = setTimeout(() => {
        projectTrustTimer = null;
        send({ event: "status", status: "needs_input", message: "trust this project?" });
      }, 1200);
    }
    return { trusted: "undecided" };
  });
  //
  // 2. `tool_call` fires for every tool, so we can't key off it directly
  // without flagging routine read/grep/bash as "needs you". Instead we watch
  // for an explicit "ask the human" tool by name: an allowlist that captures
  // the elicitation tools shipped by MCP servers and custom extensions.
  // Anything not on the list stays "working" — never a false ◆. When the ask
  // resolves (tool_result) we drop back to "working"; agent_settled will settle
  // it to "waiting" at the true end of the turn.
  const ASK_TOOLS = new Set([
    "ask",
    "ask_user",
    "ask_question",
    "ask_followup_question",
    "request_user_input",
    "user_input",
    "elicit",
    "elicitation",
    "prompt_user",
    "confirm",
  ]);
  const isAsk = (name: unknown) => typeof name === "string" && ASK_TOOLS.has(name);

  // The question the ask tool is asking, pulled from its args so roost can
  // show *what* the agent wants, not just that it wants something. Ask/elicit
  // tools have no shared schema, so probe the common shapes (a `questions`
  // array, then flat `question`/`prompt`/`message`/`text`) and take the first
  // non-empty string. Whitespace-collapsed and capped — this rides a
  // one-line socket protocol into a one-line feed entry.
  const askMessage = (input: unknown): string | undefined => {
    const i = input as Record<string, unknown> | null | undefined;
    const qs = Array.isArray(i?.questions) ? (i?.questions as unknown[]) : [];
    const first = qs.find((q) => typeof (q as any)?.question === "string") as any;
    for (const cand of [first?.question, i?.question, i?.prompt, i?.message, i?.text]) {
      if (typeof cand === "string" && cand.trim()) {
        const s = cand.trim().replace(/\s+/g, " ");
        // Truncate by code points, not UTF-16 units — String.prototype.slice
        // can split a surrogate pair, and the resulting lone surrogate makes
        // the JSON line unparseable server-side, dropping the whole report.
        const points = Array.from(s);
        return points.length > 200 ? `${points.slice(0, 199).join("")}…` : s;
      }
    }
    return undefined;
  };

  pi.on("tool_call", async (event) => {
    cancelProjectTrustNeedsInput();
    if (isAsk(event.toolName)) {
      const message = askMessage((event as any).input);
      send({ event: "status", status: "needs_input", ...(message ? { message } : {}) });
    }
  });
  pi.on("tool_result", async (event) => {
    if (isAsk(event.toolName)) send({ event: "status", status: "working" });
  });

  // Deliberately NO status report on shutdown: "the process died" has exactly
  // one ground truth — roost sees the pane's PTY hit EOF the moment its child
  // exits. This extension also runs in *nested* pi processes (a subagent, a
  // one-shot `pi -p` tool call, a pi launched by hand inside a shell pane —
  // they all inherit ROOST_PANE/ROOST_TOKEN), so a shutdown report from here
  // would mark the pane exited whenever a nested pi merely finished its work,
  // while the pane's real process is alive. Roost demotes any "exited" a
  // stale extension still sends to "waiting" for the same reason.
  pi.on("session_shutdown", async () => {
    shuttingDown = true;
    if (reconnectTimer) clearTimeout(reconnectTimer);
    clearHealthyTimer();
    sock?.end();
  });
}
