/**
 * opencode-plugin.ts — opencode plugin reporting the pane's session id to
 * roost.
 *
 * Install: roost installs/updates this automatically at startup when opencode
 * is present (into ~/.config/opencode/plugin/opencode-plugin.ts); set
 * ROOST_NO_EXT_INSTALL to manage it yourself. opencode auto-globs
 * {plugin,plugins}/*.{ts,js} under its config dir, so no registration step
 * exists or is needed. If opencode is not running inside roost (no
 * ROOST_PANE/ROOST_SOCK env), this plugin registers no hooks at all.
 *
 * Why a plugin at all: opencode keeps every session in one global SQLite
 * database ($XDG_DATA_HOME/opencode/opencode.db), so roost's usual
 * "scan the session dir" resume detection has nothing to scan. The plugin
 * pushes the session id over roost's control socket instead — the same path
 * the pi extension reports status on — and roost persists it so a restart
 * relaunches as `opencode --session <id>` (never `--continue`:
 * anomalyco/opencode#2086 can resume a subagent's thread).
 *
 * Reported over the unix socket ($ROOST_SOCK), one JSON object per line:
 *   { pane, token, event: "session", session: "ses_..." }
 * Every message carries the pane's ROOST_TOKEN; roost rejects any whose
 * token doesn't match the pane it claims to be, so one pane can't spoof
 * another's session.
 *
 * One-shot on purpose: unlike the pi extension (which streams status
 * continuously and reconnects/replays), this fires exactly once per session
 * creation — one `net.connect`, write, end. A failed connect (roost not
 * listening: standalone opencode) is swallowed, never crashing opencode; a
 * session.created that fails to report is simply lost, and the next one
 * reports.
 *
 * Subagent guard: opencode fires session.created for child sessions too, and
 * a child's id would overwrite the pane's persisted session id with the
 * wrong conversation. Child sessions carry info.parentID — bail on that.
 * (The pi extension discriminates nested invocations by isTTY instead;
 * opencode plugins run in-process, so the TTY test doesn't discriminate
 * anything here.)
 */
import * as net from "node:net";
import type { Plugin } from "@opencode-ai/plugin";

const pane = process.env.ROOST_PANE ?? "";
const sockPath = process.env.ROOST_SOCK ?? "";

const sent = new Set<string>();

function emit(session: string) {
  if (!sockPath || sent.has(session)) return;
  sent.add(session);
  const line =
    JSON.stringify({
      pane,
      token: process.env.ROOST_TOKEN ?? "",
      event: "session",
      session,
    }) + "\n";
  const c = net.connect(sockPath, () => c.end(line));
  c.on("error", () => {}); // roost not listening — never crash opencode
}

export const RoostPlugin: Plugin = async () => {
  if (!pane || !sockPath) return {}; // not running inside roost
  return {
    event: async ({ event }: any) => {
      if (event?.type !== "session.created") return;
      const p = (event as any).properties;
      if (p?.info?.parentID) return; // subagent session — not the pane's conversation
      if (typeof p?.sessionID === "string") emit(p.sessionID);
    },
  };
};
export default RoostPlugin;
