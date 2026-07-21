/**
 * roost.ts — pi extension reporting exact agent status to roost.
 *
 * Install: copy to ~/.pi/agent/extensions/roost.ts (roost offers to do this
 * on first run). If pi is not running inside roost (no ROOST_PANE env var or
 * no socket), this extension no-ops at zero cost.
 *
 * Events reported over the unix socket ($XDG_RUNTIME_DIR/roost.sock or
 * ~/.local/state/roost/roost.sock), one JSON object per line:
 *   { pane, event: "session"  , session: "<uuid>" }
 *   { pane, event: "status"   , status: "working" | "waiting" | "needs_input" | "exited" }
 */
import * as net from "node:net";
import * as os from "node:os";
import * as path from "node:path";
import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";

export default function (pi: ExtensionAPI) {
  const pane = process.env.ROOST_PANE;
  if (!pane) return; // not running inside roost

  const sockPath =
    process.env.ROOST_SOCK ??
    (process.env.XDG_RUNTIME_DIR
      ? path.join(process.env.XDG_RUNTIME_DIR, "roost.sock")
      : path.join(os.homedir(), ".local", "state", "roost", "roost.sock"));

  let sock: net.Socket | null = null;
  const connect = () => {
    sock = net.connect(sockPath);
    sock.on("error", () => (sock = null)); // roost gone → silent no-op
  };
  connect();

  const send = (msg: Record<string, unknown>) => {
    if (!sock) return;
    try {
      sock.write(JSON.stringify({ pane, ...msg }) + "\n");
    } catch {
      sock = null;
    }
  };

  pi.on("session_start", async (event, ctx) => {
    // Report the session id so roost can persist it for resume.
    const id = ctx.sessionManager?.getSessionId?.() ?? (event as any)?.sessionId;
    if (id) send({ event: "session", session: id });
    send({ event: "status", status: "waiting" });
  });

  pi.on("agent_start", async () => send({ event: "status", status: "working" }));
  pi.on("agent_end", async () => send({ event: "status", status: "waiting" }));

  // "Needs input" — the agent is explicitly blocked on *you*, mid-turn. pi has
  // no generic permission-prompt event (its built-in tool-approval UI isn't
  // surfaced to extensions), and `tool_call` fires for every tool — so we can't
  // key off it directly without flagging routine read/grep/bash as "needs you".
  // Instead we watch for an explicit "ask the human" tool by name: an allowlist
  // that captures the elicitation tools shipped by MCP servers and custom
  // extensions. Anything not on the list stays "working" — never a false ◆.
  // When the ask resolves (tool_result) we drop back to "working"; agent_end
  // will settle it to "waiting" at the true end of the turn.
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

  pi.on("tool_call", async (event) => {
    if (isAsk(event.toolName)) send({ event: "status", status: "needs_input" });
  });
  pi.on("tool_result", async (event) => {
    if (isAsk(event.toolName)) send({ event: "status", status: "working" });
  });

  pi.on("session_shutdown", async () => {
    send({ event: "status", status: "exited" });
    sock?.end();
  });
}
