import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { FramedWorker, MAX_FRAME_BYTES, handshake } from "./lib/framed-worker.mjs";

const [engineExecutable, agentExecutable, testRoot] = process.argv.slice(2);
if (!engineExecutable || !agentExecutable || !testRoot || !path.isAbsolute(testRoot)) {
  throw new Error("usage: node scripts/fault-injection-core.mjs <engine.exe> <agent.exe> <absolute-test-root>");
}
fs.mkdirSync(testRoot, { recursive: true });

const cases = [];
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));
async function runCase(id, action) {
  const started = Date.now();
  await action();
  cases.push({ id, status: "PASSED", duration_ms: Date.now() - started });
}
const completeSpec = {
  objective: "验证可恢复的人工研究计划",
  security_universe: ["600519"],
  as_of: "2026-08-24T12:00:00+08:00",
  research_start: "2025-08-24",
  research_end: "2026-08-24",
  investment_horizon: "三个月",
  comparison_benchmark: "000300",
  output_type: "manual_plan",
  evidence_requirement: "strict",
};

function engineWorker(name, dataDirectory) {
  return new FramedWorker(engineExecutable, {
    name,
    env: { ASTOCK_DATA_DIR: dataDirectory },
  });
}

await runCase("engine-kill", async () => {
  const data = path.join(testRoot, "engine-kill-data");
  let engine = engineWorker("engine-kill-before", data);
  await handshake(engine, "fault-engine-kill");
  await engine.request("agent.conversation.save", {
    conversation_id: "engine-kill-proof",
    title: "Engine kill proof",
    session: {
      sessionId: "engine-kill-proof",
      messages: [{ id: "m1", role: "user", content: "durable-before-kill" }],
      task: { phase: "completed", evidence_ids: ["durable"] },
    },
  });
  engine.kill();
  await engine.waitForExit();
  engine = engineWorker("engine-kill-after", data);
  try {
    await handshake(engine, "fault-engine-restart");
    const restored = await engine.request("agent.conversation.load", { conversation_id: "engine-kill-proof" });
    assert(restored.session?.messages?.[0]?.content === "durable-before-kill", "Engine kill lost durable conversation state");
  } finally {
    await engine.shutdown();
  }
});

await runCase("agent-kill", async () => {
  let agent = new FramedWorker(agentExecutable, { name: "agent-kill-before" });
  await handshake(agent, "fault-agent-kill");
  const started = await agent.request("agent.start", { task_id: "agent-kill-task", seq: 1, spec: completeSpec });
  assert(started.state?.phase === "preparing", "Agent did not reach a recoverable preparation checkpoint");
  agent.kill();
  await agent.waitForExit();
  agent = new FramedWorker(agentExecutable, { name: "agent-kill-after" });
  try {
    await handshake(agent, "fault-agent-restart");
    const restored = await agent.request("agent.restore", { state: started.checkpoint });
    assert(restored.state?.accepted_seq === started.state.accepted_seq, "Agent restart changed accepted sequence");
    const snapshot = await agent.request("agent.task.snapshot", { task_id: "agent-kill-task", seq: 2, event_kind: "snapshot" });
    assert(snapshot.phase === "preparing", "Agent did not restore its pre-kill phase");
  } finally {
    agent.kill();
    await agent.waitForExit();
  }
});

const checkpointData = path.join(testRoot, "checkpoint-data");
let checkpointEngine = engineWorker("checkpoint-before", checkpointData);
await handshake(checkpointEngine, "checkpoint-before");
await checkpointEngine.request("agent.task.create", {
  task_id: "checkpoint-task",
  reducer_version: "moonbit-agent-kernel-v1",
  task_spec: completeSpec,
  phase: "preparing",
});
await checkpointEngine.request("agent.event.append", {
  task_id: "checkpoint-task",
  seq: 1,
  event_id: "checkpoint-event-1",
  event_kind: "started",
  event: { phase: "preparing" },
});
await checkpointEngine.request("agent.checkpoint.put", {
  task_id: "checkpoint-task",
  accepted_seq: 1,
  phase: "preparing",
  checkpoint: { task_id: "checkpoint-task", accepted_seq: 1, marker: "before-crash" },
});
checkpointEngine.kill();
await checkpointEngine.waitForExit();

await runCase("checkpoint-before-crash", async () => {
  checkpointEngine = engineWorker("checkpoint-restore-before", checkpointData);
  await handshake(checkpointEngine, "checkpoint-restore-before");
  const loaded = await checkpointEngine.request("agent.task.load", { task_id: "checkpoint-task" });
  assert(loaded.task?.accepted_seq === 1, "pre-crash checkpoint sequence was not durable");
  assert(loaded.task?.checkpoint?.marker === "before-crash", "pre-crash checkpoint payload was lost");
});

await checkpointEngine.request("agent.event.append", {
  task_id: "checkpoint-task",
  seq: 2,
  event_id: "checkpoint-event-2",
  event_kind: "tool_result",
  event: { phase: "reviewing" },
});
await checkpointEngine.request("agent.effect.begin", {
  effect_id: "checkpoint-effect",
  task_id: "checkpoint-task",
  caused_by_seq: 2,
  effect_kind: "engine.diagnostics.status",
  effect: { target: "engine", kind: "diagnostics.status", payload: {} },
  idempotency_key: "checkpoint-effect-key",
});
await checkpointEngine.request("agent.effect.complete", {
  effect_id: "checkpoint-effect",
  status: "succeeded",
  result: { status: "ready" },
});
await checkpointEngine.request("agent.checkpoint.put", {
  task_id: "checkpoint-task",
  accepted_seq: 2,
  phase: "reviewing",
  checkpoint: { task_id: "checkpoint-task", accepted_seq: 2, marker: "after-result" },
});
checkpointEngine.kill();
await checkpointEngine.waitForExit();

await runCase("checkpoint-after-crash", async () => {
  checkpointEngine = engineWorker("checkpoint-restore-after", checkpointData);
  try {
    await handshake(checkpointEngine, "checkpoint-restore-after");
    const loaded = await checkpointEngine.request("agent.task.load", { task_id: "checkpoint-task" });
    const effects = await checkpointEngine.request("agent.effect.list", { task_id: "checkpoint-task" });
    assert(loaded.task?.accepted_seq === 2 && loaded.task?.checkpoint?.marker === "after-result", "post-result checkpoint was not durable");
    assert(effects.items?.length === 1 && effects.items[0]?.status === "succeeded", "effect result was duplicated or lost after crash");
  } finally {
    await checkpointEngine.shutdown();
  }
});

async function rawFrameFault(executable, dataDirectory, frame) {
  return new Promise((resolve, reject) => {
    const child = spawn(executable, [], {
      stdio: ["pipe", "pipe", "pipe"],
      windowsHide: true,
      env: { ...process.env, ASTOCK_DATA_DIR: dataDirectory },
    });
    const stdout = [];
    const stderr = [];
    child.stdout.on("data", (chunk) => stdout.push(chunk));
    child.stderr.on("data", (chunk) => stderr.push(chunk));
    const timer = setTimeout(() => {
      child.kill("SIGKILL");
      reject(new Error(`${path.basename(executable)} did not terminate after invalid IPC`));
    }, 15_000);
    child.once("error", reject);
    child.once("exit", (code) => {
      clearTimeout(timer);
      resolve({ code, stdout: Buffer.concat(stdout), stderr: Buffer.concat(stderr).toString("utf8") });
    });
    child.stdin.end(frame);
  });
}

await runCase("oversized-ipc", async () => {
  const header = Buffer.alloc(4);
  header.writeUInt32LE(MAX_FRAME_BYTES + 1);
  for (const [index, executable] of [engineExecutable, agentExecutable].entries()) {
    const result = await rawFrameFault(executable, path.join(testRoot, `oversized-${index}`), header);
    assert(result.stdout.length === 0, "oversized frame produced protocol output");
    assert(result.stderr.length > 0, "oversized frame failure was not diagnosed on stderr");
  }
});

await runCase("corrupt-ipc", async () => {
  const header = Buffer.alloc(4);
  header.writeUInt32LE(1);
  const frame = Buffer.concat([header, Buffer.from("<")]);
  for (const [index, executable] of [engineExecutable, agentExecutable].entries()) {
    const result = await rawFrameFault(executable, path.join(testRoot, `corrupt-${index}`), frame);
    assert(result.stdout.length === 0, "corrupt frame produced protocol output");
    assert(result.stderr.length > 0, "corrupt frame failure was not diagnosed on stderr");
  }
});

await runCase("duplicate-ipc", async () => {
  const engine = engineWorker("duplicate-engine", path.join(testRoot, "duplicate-data"));
  try {
    await handshake(engine, "duplicate-ipc");
    await engine.request("diagnostics.status", {}, { requestId: "duplicate-request" });
    let rejected;
    try {
      await engine.request("diagnostics.status", {}, { requestId: "duplicate-request" });
    } catch (error) {
      rejected = error.response;
    }
    assert(rejected?.error?.code === "duplicate_request_id", "duplicate request_id was not explicitly rejected");
  } finally {
    await engine.shutdown();
  }
});

await runCase("out-of-order-ipc", async () => {
  const engine = engineWorker("out-of-order-engine", path.join(testRoot, "out-of-order-data"));
  try {
    await handshake(engine, "out-of-order-ipc");
    const slowId = "out-of-order-slow";
    const fastId = "out-of-order-fast";
    const slow = engine.request(
      "research.agent_prepare_context",
      { depth: "fast", capital: 20_000 },
      { deadlineMs: 120_000, requestId: slowId },
    );
    await sleep(5);
    const fast = engine.request("diagnostics.status", {}, { deadlineMs: 5_000, requestId: fastId });
    const [fastResult, slowResult] = await Promise.all([fast, slow]);
    assert(fastResult.status === "ready" && slowResult.market_overview, "correlated responses returned the wrong payload");
    assert(engine.responseOrder.indexOf(fastId) < engine.responseOrder.indexOf(slowId), "Engine did not exercise out-of-order response delivery");
  } finally {
    await engine.shutdown();
  }
});

await runCase("cancel-safety", async () => {
  const engine = engineWorker("cancel-engine", path.join(testRoot, "cancel-data"));
  try {
    await handshake(engine, "cancel-safety");
    const cancellationId = "cancel-research-context";
    const research = engine.request(
      "research.agent_prepare_context",
      { depth: "deep", capital: 20_000 },
      { deadlineMs: 120_000, cancellationId, requestId: "cancelled-research" },
    );
    await sleep(5);
    const cancel = await engine.request("system.cancel", { cancellation_id: cancellationId }, { requestId: "cancel-command" });
    assert(cancel.cancelled === true, "Engine did not acknowledge the active cancellation token");
    let cancelled;
    try { await research; } catch (error) { cancelled = error.response; }
    assert(cancelled?.error?.code === "cancelled", "cancelled research published a normal result");
    const status = await engine.request("diagnostics.status");
    assert(status.status === "ready", "Engine did not remain usable after cancellation");
  } finally {
    await engine.shutdown();
  }
});

await runCase("quota-suspension-resume", async () => {
  const agent = new FramedWorker(agentExecutable, { name: "quota-agent" });
  try {
    await handshake(agent, "quota-suspension");
    const started = await agent.request("agent.start", { task_id: "quota-task", seq: 1, spec: completeSpec });
    const suspended = await agent.request("agent.event", { task_id: "quota-task", seq: 2, event_kind: "quota_suspended" });
    assert(started.state?.phase === "preparing" && suspended.state?.phase === "suspended", "quota event did not suspend the task");
    assert(suspended.state?.terminal_reason === "provider_quota_or_rate_limit", "quota suspension reason was lost");
    const resumed = await agent.request("agent.event", { task_id: "quota-task", seq: 3, event_kind: "resume" });
    assert(resumed.state?.phase === "reasoning" && resumed.state?.terminal_reason == null, "quota recovery did not resume from durable reasoning state");
  } finally {
    agent.kill();
    await agent.waitForExit();
  }
});

process.stdout.write(`${JSON.stringify({ ok: true, cases })}\n`);
