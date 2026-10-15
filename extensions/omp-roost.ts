/**
 * omp-roost.ts — oh-my-pi (omp) extension reporting exact agent status to roost.
 *
 * Install: roost installs/updates this automatically at startup when omp is
 * present (into ~/.omp/agent/extensions/roost.ts — omp derives the extension's
 * name from the file name, and does NOT read ~/.pi/agent/extensions, so it
 * never double-loads with the pi extension); set ROOST_NO_EXT_INSTALL to manage
 * it yourself. If omp is not running inside roost (no ROOST_PANE env var or
 * no socket), this extension no-ops at zero cost.
 *
 * Same wire protocol as extensions/roost.ts (the pi extension): one JSON object
 * per line over the unix socket ($ROOST_SOCK, $XDG_RUNTIME_DIR/roost.sock or
 * ~/.local/state/roost/roost.sock), every message carrying the pane's
 * ROOST_TOKEN:
 *   { pane, token, event: "session", session: "<uuid>" }
 *   { pane, token, event: "status" , status: "working" | "waiting" | "needs_input", message? }
 * Reconnects on error/close with a flat 500ms retry and replays the last known
 * session/status, exactly like roost.ts (see its header for why).
 *
 * omp is a fork of pi, but its extension API is not pi's. The deviations from
 * roost.ts, and why (oh-my-pi v18.11.0, packages/coding-agent/src/extensibility):
 *
 * 1. SUBAGENTS RUN IN-PROCESS. "Factories are rebound to every subagent session
 *    (task tool, eval agent(), /tan clones)" and the module graph is NOT
 *    re-evaluated. So the default-export factory runs again per subagent in the
 *    SAME process — stdout is still the TTY, so roost.ts's `isTTY` guard cannot
 *    tell — and an advisor's tool calls reach the main session's
 *    tool_call/tool_result handlers with `ctx.agent.kind === "sub"`. Hence: the
 *    socket connection and the `last` replay state live in MODULE scope (one
 *    per process, created once, however often the factory runs), the factory
 *    only registers handlers, and every handler returns early for a "sub" ctx
 *    (an older omp without `ctx.agent` counts as main). The `isTTY` guard stays
 *    for nested `omp -p` runs from a bash tool, which are separate processes
 *    with piped stdio that inherit ROOST_PANE/ROOST_TOKEN.
 *
 * 2. THE SESSION ID CHANGES MID-PROCESS. /new, /resume and /fork swap the
 *    session id without restarting omp (`session_switch`, reason
 *    "new"|"resume"|"fork"; `session_branch` for in-session branching), so the
 *    id is re-read from `ctx.sessionManager.getSessionId()` on each of them —
 *    otherwise roost would persist a stale id and resume the wrong
 *    conversation. `last` dedups, so re-reporting an unchanged id is free.
 *
 * 3. NO agent_settled, NO project_trust. omp has neither: it never asks for
 *    directory trust, and `agent_end` itself carries `willContinue` — true when
 *    the run is not a terminal settle (auto-retry, compaction continuation,
 *    queued/background work). So the turn end is `agent_end` with
 *    `willContinue !== true` (an omp predating the field reads as final; see
 *    docs/BACKLOG.md "omp adapter" for the flap that costs). There is no
 *    settle latch and no trust timer, and no `ctx.isIdle()` guard either:
 *    omp's `isIdle()` is still false inside the terminal agent_end (the
 *    prompt that ran the turn has not unwound yet — measured live on 18.11.0),
 *    so guarding on it would suppress every real turn-end report.
 *
 * 4. omp HAS REAL APPROVAL PROMPTS, which pi lacks: `tool_approval_requested`
 *    / `tool_approval_resolved` and `plan_review_requested` /
 *    `plan_review_resolved` are exact blocked-on-the-human signals, so they
 *    report needs_input directly (no name allowlist guess). The built-in ask
 *    tool is named `ask` with `{ questions: [{ question }] }`, already covered
 *    by ASK_TOOLS/askMessage below.
 *
 * Deliberately NO status report on shutdown (same reasoning as roost.ts): roost
 * sees the pane's PTY hit EOF when the process really exits; a shutdown report
 * from a nested process would mark a live pane exited.
 */
import * as net from "node:net";
import * as os from "node:os";
import * as path from "node:path";
import type { ExtensionAPI } from "@oh-my-pi/pi-coding-agent";

// A local unix socket has nothing like network latency to back off from — a
// flat retry is plenty.
const RECONNECT_DELAY_MS = 500;

type Last = { session?: string; status?: string; message?: string };
type Reporter = {
  send: (msg: Record<string, unknown>) => void;
  close: () => void;
  last: Last;
};

/** The per-process connection; `undefined` until the first factory call. */
let reporter: Reporter | null | undefined;

/** Build the process-wide reporter, or null when this process must not report. */
const createReporter = (): Reporter | null => {
  const pane = process.env.ROOST_PANE;
  if (!pane) return null; // not running inside roost
  // Only the pane's own interactive agent may report. The ROOST_* env is
  // inherited by every descendant process, so a *nested* omp — a one-shot
  // `omp -p` run by a tool of the pane's agent — would otherwise authenticate
  // as the pane, overwrite its persisted session id with its own (breaking
  // resume into the wrong conversation) and flap its status. Nested
  // invocations run with piped stdio; the agent that owns the pane runs on the
  // pane's tty. (In-process subagents are NOT caught here — see header, item 1.)
  if (!process.stdout.isTTY) return null;
  // Per-pane secret roost issued to this child; every message must carry it or
  // roost drops it (prevents one pane spoofing another over the shared socket).
  const token = process.env.ROOST_TOKEN ?? "";

  const sockPath =
    process.env.ROOST_SOCK ??
    (process.env.XDG_RUNTIME_DIR
      ? path.join(process.env.XDG_RUNTIME_DIR, "roost.sock")
      : path.join(os.homedir(), ".local", "state", "roost", "roost.sock"));

  let sock: net.Socket | null = null;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let shuttingDown = false; // session_shutdown initiated the close — no retry
  // Last known session/status (and the status's message, so a replayed
  // needs_input keeps its question), kept up to date by every send() (even
  // while disconnected) so a reconnect can replay it.
  const last: Last = {};

  // `close` and `error` both fire for the same failure, so this must be
  // idempotent per failure, not just per call.
  const scheduleReconnect = () => {
    if (shuttingDown || reconnectTimer) return;
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      connect();
    }, RECONNECT_DELAY_MS);
  };

  const connect = () => {
    const s = net.connect(sockPath);
    sock = s;
    s.on("connect", () => {
      if (sock !== s) return; // superseded by a newer attempt
      // Re-establish App's link refcount (a link-up requires a line) and hand
      // back the exact status a false rejection would otherwise have cost —
      // every reconnected socket's first line also authenticates it.
      if (last.session !== undefined) send({ event: "session", session: last.session });
      if (last.status !== undefined)
        send({
          event: "status",
          status: last.status,
          ...(last.message !== undefined ? { message: last.message } : {}),
        });
    });
    s.on("error", () => {
      if (sock !== s) return;
      sock = null;
      scheduleReconnect();
    });
    s.on("close", () => {
      if (sock !== s) return;
      sock = null;
      if (!shuttingDown) scheduleReconnect();
    });
  };

  const send = (msg: Record<string, unknown>) => {
    if (msg.event === "session" && typeof msg.session === "string") last.session = msg.session;
    if (msg.event === "status" && typeof msg.status === "string") {
      last.status = msg.status;
      // Overwrite (not preserve) so a stale question never rides along with a
      // later working/waiting replay.
      last.message = typeof msg.message === "string" ? msg.message : undefined;
    }
    // Dropped while disconnected — `last` above lets the next reconnect replay
    // it, and the flat retry means that's never far off.
    if (!sock) return;
    try {
      // ponytail: accepted race — a sub-millisecond window where the server
      // already closed `sock` but the "error"/"close" event hasn't reached this
      // callback yet can silently lose this one write. `last` already recorded
      // it, and the next lifecycle event's send() retries.
      sock.write(JSON.stringify({ pane, token, ...msg }) + "\n");
    } catch {
      sock = null;
      scheduleReconnect();
    }
  };

  const close = () => {
    shuttingDown = true;
    if (reconnectTimer) clearTimeout(reconnectTimer);
    sock?.end();
  };

  connect();
  return { send, close, last };
};

// clip: shared normalization for every human-facing message this extension
// sends — whitespace-collapsed, truncated by code points (a UTF-16 slice can
// split a surrogate pair, and the resulting lone surrogate makes the JSON line
// unparseable server-side, dropping the whole report).
const clip = (raw: string): string | undefined => {
  const s = raw.trim().replace(/\s+/g, " ");
  if (!s) return undefined;
  const points = Array.from(s);
  return points.length > 200 ? `${points.slice(0, 199).join("")}…` : s;
};

// Prose-question heuristic: a turn whose final assistant line ends with a
// question mark is (almost always) the agent waiting on *you*, yet omp emits no
// event for it, so it's structurally identical to "finished". Deliberately a
// heuristic and deliberately cheap: a borderline ◆ still shows the question
// itself in roost's feed/notification, and roost's needs-input decay self-heals
// a wrong ◆. Only the LAST assistant message counts.
const trailingQuestion = (messages: { role?: string; content?: unknown }[]): string | undefined => {
  for (let k = messages.length - 1; k >= 0; k--) {
    const m = messages[k];
    if (m?.role !== "assistant") continue;
    const parts: { type?: string; text?: unknown }[] = Array.isArray(m.content) ? m.content : [];
    const text = parts
      .map((p) => (p?.type === "text" && typeof p.text === "string" ? p.text : ""))
      .join("\n");
    const lines = text
      .split("\n")
      .map((l) => l.trim())
      .filter(Boolean);
    const lastLine = lines[lines.length - 1];
    return lastLine?.endsWith("?") ? clip(lastLine) : undefined;
  }
  return undefined;
};

// `tool_call` fires for every tool, so we can't key off it directly without
// flagging routine read/grep/bash as "needs you". Instead we watch for an
// explicit "ask the human" tool by name — omp's built-in `ask`, plus the
// elicitation tools shipped by MCP servers and custom extensions. Anything not
// on the list stays "working" — never a false ◆. (Approval prompts have their
// own exact events, handled separately.)
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

// The question the ask tool is asking, pulled from its args so roost can show
// *what* the agent wants. Probe the common shapes (a `questions` array — omp's
// `ask` — then flat `question`/`prompt`/`message`/`text`) and take the first
// non-empty string.
const askMessage = (input: unknown): string | undefined => {
  const i = input as Record<string, unknown> | null | undefined;
  const qs = Array.isArray(i?.questions) ? (i?.questions as unknown[]) : [];
  const first = qs.find((q) => typeof (q as any)?.question === "string") as any;
  for (const cand of [first?.question, i?.question, i?.prompt, i?.message, i?.text]) {
    if (typeof cand === "string") {
      const c = clip(cand);
      if (c) return c;
    }
  }
  return undefined;
};

// In-process subagents (and advisors) share this process and these handlers;
// only the main session may report. An omp without `ctx.agent` is main.
const isSub = (ctx: any): boolean => ctx?.agent?.kind === "sub";

export default function (pi: ExtensionAPI) {
  // Created once per process, however many times omp re-runs this factory for
  // subagent sessions.
  if (reporter === undefined) reporter = createReporter();
  const r = reporter;
  if (!r) return;

  // Report the session id so roost can persist it for resume. Called on every
  // event that can change it; `last` makes an unchanged id a no-op.
  const reportSession = (event: any, ctx: any) => {
    const id = ctx?.sessionManager?.getSessionId?.() ?? event?.sessionId;
    if (id && id !== r.last.session) r.send({ event: "session", session: id });
  };

  pi.on("session_start", async (event, ctx) => {
    if (isSub(ctx)) return;
    reportSession(event, ctx);
    r.send({ event: "status", status: "waiting" });
  });
  // /new, /resume, /fork swap the session id under a running process.
  pi.on("session_switch", async (event, ctx) => {
    if (isSub(ctx)) return;
    reportSession(event, ctx);
  });
  pi.on("session_branch", async (event, ctx) => {
    if (isSub(ctx)) return;
    reportSession(event, ctx);
  });

  pi.on("agent_start", async (_event, ctx) => {
    if (isSub(ctx)) return;
    r.send({ event: "status", status: "working" });
  });

  // Turn end. `willContinue: true` means omp is about to keep going on its own
  // (auto-retry, compaction continuation, queued or background work) — still
  // working, not a settle. Otherwise this is the terminal settle: the trailing
  // question if there is one, else plain waiting.
  //
  // Deliberately NOT guarded by `ctx.isIdle()` (roost.ts's agent_settled
  // handler is): verified live against omp 18.11.0, `isIdle()` is still FALSE
  // inside the terminal agent_end handler — omp notifies extensions while the
  // prompt that ran the turn is still unwinding (`isStreaming` includes
  // `#promptInFlightCount > 0`), so such a guard would swallow every final
  // report and pin the pane at ● working. A new run racing this report is
  // harmless without it: its agent_start is delivered after this handler and
  // sends "working" over it.
  pi.on("agent_end", async (event, ctx) => {
    if (isSub(ctx)) return;
    if ((event as any)?.willContinue === true) return;
    const question = trailingQuestion((event as any)?.messages ?? []);
    if (question) r.send({ event: "status", status: "needs_input", message: question });
    else r.send({ event: "status", status: "waiting" });
  });

  // Exact blocked-on-the-human signals (pi has none of these).
  pi.on("tool_approval_requested", async (event, ctx) => {
    if (isSub(ctx)) return;
    const e = event as any;
    const name = typeof e?.toolName === "string" ? e.toolName : "tool";
    const reason = typeof e?.reason === "string" ? e.reason : "";
    const message = clip(reason ? `approve ${name}: ${reason}` : `approve ${name}?`);
    r.send({ event: "status", status: "needs_input", ...(message ? { message } : {}) });
  });
  // Approved or denied, the run carries on (a denial comes back as a tool
  // result); agent_end settles it at the true end of the turn.
  pi.on("tool_approval_resolved", async (_event, ctx) => {
    if (isSub(ctx)) return;
    r.send({ event: "status", status: "working" });
  });

  pi.on("plan_review_requested", async (event, ctx) => {
    if (isSub(ctx)) return;
    const title = typeof (event as any)?.title === "string" ? (event as any).title : "";
    const message = clip(`review plan: ${title}`);
    r.send({ event: "status", status: "needs_input", ...(message ? { message } : {}) });
  });
  // Choosing "execute"/"refine" starts a run (working); dismissing it leaves
  // the agent idle (waiting).
  pi.on("plan_review_resolved", async (_event, ctx) => {
    if (isSub(ctx)) return;
    r.send({ event: "status", status: ctx?.isIdle?.() === false ? "working" : "waiting" });
  });

  pi.on("tool_call", async (event, ctx) => {
    if (isSub(ctx)) return;
    if (isAsk(event.toolName)) {
      const message = askMessage((event as any).input);
      r.send({ event: "status", status: "needs_input", ...(message ? { message } : {}) });
    }
  });
  pi.on("tool_result", async (event, ctx) => {
    if (isSub(ctx)) return;
    if (isAsk(event.toolName)) r.send({ event: "status", status: "working" });
  });

  // Deliberately NO status report on shutdown — see header. Only the main
  // session's shutdown ends the process-wide connection; a subagent session
  // shutting down must not close the pane's socket.
  pi.on("session_shutdown", async (_event, ctx) => {
    if (isSub(ctx)) return;
    r.close();
  });
}
