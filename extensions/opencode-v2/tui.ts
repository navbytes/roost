/**
 * opencode 2.x: reports the pane's open session id to roost.
 *
 * Runs in the pane's own TUI process (unlike v2's shared server), so the
 * pane env is present. Same wire format and one-shot socket write as the
 * 1.x plugin (extensions/opencode-plugin.ts):
 *   { pane, token, event: "session", session: "ses_..." }
 *
 * Reports the *root* of whatever session the TUI is showing, on change — a
 * subagent view reports its parent, never the child (anomalyco/opencode#2086
 * is why roost only ever resumes by explicit id). v2's session.created
 * events cover every client of the shared server, other panes included, so
 * they can't identify this pane's session; the router can.
 *
 * Polled, not reactive: router.current() is only reactive inside a Solid
 * computation, and importing solid-js would tie this dependency-free file
 * to opencode's bundled copy.
 */
import * as net from "node:net";

const pane = process.env.ROOST_PANE ?? "";
const sockPath = process.env.ROOST_SOCK ?? "";

function emit(session: string) {
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

export default {
  id: "roost",
  setup(ctx: any) {
    if (!pane || !sockPath) return; // not running inside roost
    let last = "";
    const timer = setInterval(() => {
      try {
        const route = ctx.ui.router.current();
        if (route?.type !== "session") return;
        const root = ctx.data.session.root(route.sessionID) || route.sessionID;
        if (root === last) return;
        last = root;
        emit(root);
      } catch {} // a plugin error must never take down the pane's TUI
    }, 1000);
    return () => clearInterval(timer);
  },
};
