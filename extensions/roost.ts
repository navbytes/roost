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
 *   { pane, event: "status"   , status: "working" | "waiting" | "exited" }
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
  pi.on("session_shutdown", async () => {
    send({ event: "status", status: "exited" });
    sock?.end();
  });
}
