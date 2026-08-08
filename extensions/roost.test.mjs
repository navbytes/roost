// End-to-end check of extensions/roost.ts against a fake roost socket:
// exercises the real module (no copy-paste of logic) — turn-end reporting,
// the prose-question heuristic, ask-tool messages, and the agent_end latch.
//
// Run: node extensions/roost.test.mjs   (node ≥ 23 — imports the .ts via
// native type stripping; not wired into CI, which is cargo-only today).
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
// Semantic transitions: reconnect replays legitimately duplicate the last
// status on the wire (roost's tracker is idempotent on those), so collapse
// consecutive identical [status, message] pairs before asserting.
const takeStatuses = () => {
  const raw = lines.filter((l) => l.event === "status").map((l) => [l.status, l.message]);
  lines.length = 0;
  return raw.filter((s, i) => i === 0 || s[0] !== raw[i - 1][0] || s[1] !== raw[i - 1][1]);
};

// 1. Ordinary turn: working, then settled with no question -> waiting.
// The FIRST turn's agent_end also reports (the latch hasn't seen a settled
// yet — deliberate: that's the old-pi fallback proving itself harmless), so
// turn 1 carries a duplicate waiting.
await fire("session_start", {}, { sessionManager: { getSessionId: () => "abc" } });
await fire("agent_start");
await fire("agent_end", { messages: [
  { role: "assistant", content: [{ type: "text", text: "All done.\nThe tests pass." }] },
] });
await fire("agent_settled", {}, { isIdle: () => true });
await settle();
assert.deepEqual(takeStatuses(), [
  ["waiting", undefined],
  ["working", undefined],
  ["waiting", undefined],
]);

// 2. Prose question in the final assistant line -> needs_input with the line.
await fire("agent_start");
await fire("agent_end", { messages: [
  { role: "user", content: "which db?" },
  { role: "assistant", content: [{ type: "text", text: "Two options exist.\nWhich database should I use?" }] },
] });
await fire("agent_settled", {}, { isIdle: () => true });
await settle();
assert.deepEqual(takeStatuses(), [["working", undefined], ["needs_input", "Which database should I use?"]]);

// 3. A new run clears the stale question; a non-question end -> waiting.
await fire("agent_start");
await fire("agent_end", { messages: [
  { role: "assistant", content: [{ type: "text", text: "Committed." }] },
] });
await fire("agent_settled", {}, { isIdle: () => true });
await settle();
assert.deepEqual(takeStatuses(), [["working", undefined], ["waiting", undefined]]);

// 4. Latch: settled has fired, so a mid-recovery agent_end reports nothing.
await fire("agent_start");
await fire("agent_end", { messages: [] });
await settle();
assert.deepEqual(takeStatuses(), [["working", undefined]], "latched agent_end must stay silent");
await fire("agent_settled", {}, { isIdle: () => true });
await settle();
assert.deepEqual(takeStatuses(), [["waiting", undefined]]);

// 5. Ask-tool question rides needs_input; result returns to working.
await fire("agent_start");
await fire("tool_call", { toolName: "ask_user", input: { question: "Deploy to prod?" } });
await fire("tool_result", { toolName: "ask_user" });
await settle();
assert.deepEqual(takeStatuses(), [
  ["working", undefined],
  ["needs_input", "Deploy to prod?"],
  ["working", undefined],
]);

// 6. isIdle false at settled (new turn racing) -> no report.
await fire("agent_end", { messages: [] });
await fire("agent_settled", {}, { isIdle: () => false });
await settle();
assert.deepEqual(takeStatuses(), []);

server.close();
console.log("ext-e2e: all 6 scenarios pass");
process.exit(0);
