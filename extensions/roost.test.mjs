// End-to-end check of extensions/roost.ts against a fake roost socket:
// exercises the real module (no copy-paste of logic) — turn-end reporting,
// the prose-question heuristic (settle-only), question one-shot/clearing,
// the agent_end latch, ask-tool messages, and the isIdle race guard.
//
// Run: node extensions/roost.test.mjs   (node ≥ 23 — imports the .ts via
// native type stripping; not wired into CI, which is cargo-only today).
//
// Assertions are RAW wire sequences — deliberately no de-duplication, so a
// regression that double-sends needs_input (each one costs a desktop
// notification roost-side) fails loudly. The one nondeterministic burst —
// the initial connect's session/status replay — is drained after
// session_start instead of masked in the assertions.
import assert from "node:assert/strict";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import fs from "node:fs";

const sockPath = path.join(fs.mkdtempSync(path.join(os.tmpdir(), "roost-ext-")), "s.sock");
process.env.ROOST_PANE = "7";
process.env.ROOST_TOKEN = "tok";
process.env.ROOST_SOCK = sockPath;
Object.defineProperty(process.stdout, "isTTY", { value: true });

const lines = [];
const server = net.createServer((c) => {
  let buf = "";
  c.on("data", (d) => {
    buf += d;
    let i;
    while ((i = buf.indexOf("\n")) >= 0) {
      lines.push(JSON.parse(buf.slice(0, i)));
      buf = buf.slice(i + 1);
    }
  });
});
await new Promise((r) => server.listen(sockPath, r));

// Minimal pi stub: capture handlers, let the test fire events.
const handlers = new Map();
const pi = { on: (ev, fn) => handlers.set(ev, fn) };
const mod = await import(path.resolve("extensions/roost.ts"));
mod.default(pi);
const fire = (ev, event = {}, ctx = {}) => handlers.get(ev)?.(event, ctx);
const settle = () => new Promise((r) => setTimeout(r, 150)); // let socket writes land
const takeStatuses = () => {
  const s = lines.filter((l) => l.event === "status").map((l) => [l.status, l.message]);
  lines.length = 0;
  return s;
};
const question = (text) => ({
  role: "assistant",
  content: [{ type: "text", text }],
});

// Drain the connect-time replay burst: session_start's first sends can race
// the socket's own connect, and the connect handler replays last
// session/status. Everything after this runs on a stable connection, so
// the raw assertions below are deterministic.
await fire("session_start", {}, { sessionManager: { getSessionId: () => "abc" } });
await settle();
lines.length = 0;

// 1. UNLATCHED turn ending on a question: agent_end reports plain waiting —
// never the question. Pre-settle, agent_end can be a mid-recovery run with
// a half-answered question (pi's extension-side AgentEndEvent has no
// finality field), and a false ◆ rings the desktop. The question is only
// captured here, for settle to report.
await fire("agent_start");
await fire("agent_end", { messages: [question("Setup done.\nWhich database should I use?")] });
await settle();
assert.deepEqual(takeStatuses(), [["working", undefined], ["waiting", undefined]]);

// 2. The first settled reports the captured question — and latches.
await fire("agent_settled", {}, { isIdle: () => true });
await settle();
assert.deepEqual(takeStatuses(), [["needs_input", "Which database should I use?"]]);

// 3. One-shot: a second settled with no run in between must not re-ring.
await fire("agent_settled", {}, { isIdle: () => true });
await settle();
assert.deepEqual(takeStatuses(), [["waiting", undefined]]);

// 4. Latched turn, no question: agent_end is silent, settled reports.
await fire("agent_start");
await fire("agent_end", { messages: [question("Committed.")] });
await fire("agent_settled", {}, { isIdle: () => true });
await settle();
assert.deepEqual(takeStatuses(), [["working", undefined], ["waiting", undefined]]);

// 5. Latched turn with a question: exactly one needs_input, at settle.
await fire("agent_start");
await fire("agent_end", { messages: [
  { role: "user", content: "which db?" },
  question("Two options exist.\nPostgres or SQLite?"),
] });
await fire("agent_settled", {}, { isIdle: () => true });
await settle();
assert.deepEqual(takeStatuses(), [
  ["working", undefined],
  ["needs_input", "Postgres or SQLite?"],
]);

// 6. Ask-tool question rides needs_input; result returns to working.
await fire("agent_start");
await fire("tool_call", { toolName: "ask_user", input: { question: "Deploy to prod?" } });
await fire("tool_result", { toolName: "ask_user" });
await settle();
assert.deepEqual(takeStatuses(), [
  ["working", undefined],
  ["needs_input", "Deploy to prod?"],
  ["working", undefined],
]);

// 7. isIdle false at settled (new turn racing): no report, and the kept
// question lands on the settle that does report.
await fire("agent_end", { messages: [question("Merge it?")] });
await fire("agent_settled", {}, { isIdle: () => false });
await settle();
assert.deepEqual(takeStatuses(), []);
await fire("agent_settled", {}, { isIdle: () => true });
await settle();
assert.deepEqual(takeStatuses(), [["needs_input", "Merge it?"]]);

server.close();
console.log("ext-e2e: all 7 scenarios pass");
process.exit(0);
