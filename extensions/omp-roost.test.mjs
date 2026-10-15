// End-to-end check of extensions/omp-roost.ts against a fake roost socket:
// exercises the real module (no copy-paste of logic) — session report and
// switch, turn-end reporting (willContinue suppression; isIdle deliberately ignored), the
// trailing-question heuristic, approval and plan-review prompts, ask tools,
// in-process subagent isolation, and the factory being re-run (omp rebinds it
// per subagent session) without a second connection or duplicate sends.
//
// Run: bun extensions/omp-roost.test.mjs
//  or: node --experimental-strip-types extensions/omp-roost.test.mjs   (node 22)
// Not wired into CI, which is cargo-only today.
//
// Assertions are RAW wire sequences — deliberately no de-duplication, so a
// regression that double-sends needs_input (each one costs a desktop
// notification roost-side) fails loudly. The one nondeterministic burst — the
// initial connect's session/status replay — is drained after session_start
// instead of masked in the assertions.
import assert from "node:assert/strict";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import fs from "node:fs";

const sockPath = path.join(fs.mkdtempSync(path.join(os.tmpdir(), "roost-omp-ext-")), "s.sock");
process.env.ROOST_PANE = "7";
process.env.ROOST_TOKEN = "tok";
process.env.ROOST_SOCK = sockPath;
Object.defineProperty(process.stdout, "isTTY", { value: true });

const lines = [];
let connections = 0;
const server = net.createServer((c) => {
  connections++;
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

// Minimal omp stub: capture handlers, let the test fire events.
const makeOmp = () => {
  const handlers = new Map();
  return { api: { on: (ev, fn) => handlers.set(ev, fn) }, handlers };
};
const mod = await import(path.resolve("extensions/omp-roost.ts"));
const main = makeOmp();
mod.default(main.api);
const fire = (ev, event = {}, ctx = {}) => main.handlers.get(ev)?.(event, ctx);
const settle = () => new Promise((r) => setTimeout(r, 150)); // let socket writes land
const takeStatuses = () => {
  const s = lines.filter((l) => l.event === "status").map((l) => [l.status, l.message]);
  lines.length = 0;
  return s;
};
const takeSessions = () => {
  const s = lines.filter((l) => l.event === "session").map((l) => l.session);
  lines.length = 0;
  return s;
};
const question = (text) => ({ role: "assistant", content: [{ type: "text", text }] });
const ctxFor = (id, extra = {}) => ({
  sessionManager: { getSessionId: () => id },
  agent: { kind: "main" },
  ...extra,
});
const idle = (id = "abc") => ctxFor(id, { isIdle: () => true });
const busy = (id = "abc") => ctxFor(id, { isIdle: () => false });

// 1. session_start reports the session id and a resting status — with the
// wire envelope (pane + token) on every line.
await fire("session_start", { type: "session_start" }, ctxFor("abc"));
await settle();
// (The connect-time replay can duplicate these two lines — the one
// nondeterministic burst — so this single check compares the distinct lines.)
assert.deepEqual(
  [...new Set(lines.map((l) => JSON.stringify(l)))].map((l) => JSON.parse(l)),
  [
    { pane: "7", token: "tok", event: "session", session: "abc" },
    { pane: "7", token: "tok", event: "status", status: "waiting" },
  ],
);
lines.length = 0;
assert.equal(connections, 1);

// 2. session_switch (/new, /resume, /fork) and session_branch re-report the
// id; an unchanged id sends nothing.
await fire("session_switch", { reason: "new" }, ctxFor("def"));
await settle();
assert.deepEqual(takeSessions(), ["def"]);
await fire("session_switch", { reason: "resume" }, ctxFor("def"));
await settle();
assert.deepEqual(takeSessions(), [], "unchanged id is not re-sent");
await fire("session_branch", {}, ctxFor("ghi"));
await settle();
assert.deepEqual(takeSessions(), ["ghi"]);

// 3. working -> waiting on a plain turn.
await fire("agent_start", {}, idle("ghi"));
await fire("agent_end", { messages: [question("Committed.")] }, idle("ghi"));
await settle();
assert.deepEqual(takeStatuses(), [["working", undefined], ["waiting", undefined]]);

// 4. willContinue: true is not a settle (auto-retry / continuation) — silent;
// the final agent_end (willContinue false or absent) reports.
await fire("agent_start", {}, idle());
await fire("agent_end", { willContinue: true, messages: [question("Retrying?")] }, idle());
await settle();
assert.deepEqual(takeStatuses(), [["working", undefined]]);
await fire("agent_end", { willContinue: false, messages: [question("All done.")] }, idle());
await settle();
assert.deepEqual(takeStatuses(), [["waiting", undefined]]);
await fire("agent_end", { messages: [question("Old omp, no field.")] }, idle());
await settle();
assert.deepEqual(takeStatuses(), [["waiting", undefined]]);

// 5. isIdle() is still FALSE inside omp's terminal agent_end (measured live on
// omp 18.11.0: the prompt that ran the turn has not unwound yet), so it must
// NOT suppress the report — willContinue is the only filter.
await fire("agent_end", { messages: [question("Racing?")] }, busy());
await settle();
assert.deepEqual(takeStatuses(), [["needs_input", "Racing?"]]);
await fire("agent_end", { messages: [question("Plain.")] }, busy());
await settle();
assert.deepEqual(takeStatuses(), [["waiting", undefined]]);

// 6. Trailing question at the terminal settle -> needs_input with the question.
await fire("agent_start", {}, idle());
await fire(
  "agent_end",
  {
    messages: [
      { role: "user", content: "which db?" },
      question("Two options exist.\nPostgres or SQLite?"),
    ],
  },
  idle(),
);
await settle();
assert.deepEqual(takeStatuses(), [
  ["working", undefined],
  ["needs_input", "Postgres or SQLite?"],
]);

// 7. Tool approval: needs_input with the tool (and reason, when present);
// resolve -> working, approved or not.
await fire("tool_approval_requested", { toolName: "bash" }, idle());
await fire("tool_approval_resolved", { toolName: "bash", approved: true }, idle());
await fire(
  "tool_approval_requested",
  { toolName: "write", reason: "outside   the\nworkspace" },
  idle(),
);
await fire("tool_approval_resolved", { toolName: "write", approved: false }, idle());
await settle();
assert.deepEqual(takeStatuses(), [
  ["needs_input", "approve bash?"],
  ["working", undefined],
  ["needs_input", "approve write: outside the workspace"],
  ["working", undefined],
]);

// 8. Plan review: needs_input; resolved -> working if a run is under way,
// waiting if the agent is idle (dismissed).
await fire("plan_review_requested", { title: "Refactor the parser" }, idle());
await fire("plan_review_resolved", { choice: "execute" }, busy());
await fire("plan_review_requested", { title: "Second plan" }, idle());
await fire("plan_review_resolved", { choice: undefined }, idle());
await settle();
assert.deepEqual(takeStatuses(), [
  ["needs_input", "review plan: Refactor the parser"],
  ["working", undefined],
  ["needs_input", "review plan: Second plan"],
  ["waiting", undefined],
]);

// 9. Ask tool (omp's built-in `ask`, questions array): needs_input with the
// question; the result returns to working.
await fire("tool_call", {
  toolName: "ask",
  input: { questions: [{ question: "Deploy to prod?" }] },
}, idle());
await fire("tool_result", { toolName: "ask" }, idle());
await fire("tool_call", { toolName: "bash", input: { command: "ls" } }, idle());
await settle();
assert.deepEqual(takeStatuses(), [
  ["needs_input", "Deploy to prod?"],
  ["working", undefined],
]);

// 10. A "sub"-kind ctx (in-process subagent / advisor reaching the main
// session's handlers) produces nothing, whatever the event.
const sub = { sessionManager: { getSessionId: () => "sub-session" }, agent: { kind: "sub" } };
for (const [ev, event] of [
  ["session_start", {}],
  ["session_switch", { reason: "new" }],
  ["session_branch", {}],
  ["agent_start", {}],
  ["agent_end", { messages: [question("Sub asks?")] }],
  ["tool_approval_requested", { toolName: "bash" }],
  ["tool_approval_resolved", { toolName: "bash" }],
  ["plan_review_requested", { title: "x" }],
  ["plan_review_resolved", {}],
  ["tool_call", { toolName: "ask", input: { question: "sub?" } }],
  ["tool_result", { toolName: "ask" }],
  ["session_shutdown", {}],
]) {
  await fire(ev, event, sub);
}
await settle();
assert.deepEqual(lines, [], "sub-kind ctx must not report anything");

// 11. Factory re-run (omp rebinds factories per subagent session): the module
// keeps ONE connection, the rebound handlers on a sub ctx send nothing, and
// the main session's reports are not duplicated.
const rebound = makeOmp();
mod.default(rebound.api);
await rebound.handlers.get("session_start")({}, sub);
await rebound.handlers.get("agent_start")({}, sub);
await rebound.handlers.get("agent_end")({ messages: [question("Sub?")] }, sub);
await rebound.handlers.get("session_shutdown")({}, sub); // must not close the pane socket
await settle();
assert.deepEqual(lines, [], "rebound factory on a sub session must be silent");
assert.equal(connections, 1, "factory re-run must not open a second connection");
await fire("agent_start", {}, idle());
await fire("agent_end", { messages: [question("Done.")] }, idle());
await settle();
assert.deepEqual(takeStatuses(), [["working", undefined], ["waiting", undefined]]);
assert.equal(connections, 1, "socket survived a subagent's shutdown");

// 12. Shutdown of the main session closes the socket with no status report.
await fire("session_shutdown", {}, idle());
await settle();
assert.deepEqual(lines, []);

server.close();
console.log("omp-ext-e2e: all 12 scenarios pass");
process.exit(0);
