import { spawn } from "node:child_process";

const [engineExecutable, agentExecutable] = process.argv.slice(2);
if (!engineExecutable || !agentExecutable) throw new Error("usage: node scripts/research-live-smoke.mjs <engine.exe> <agent-worker.exe>");
const MAX_FRAME_BYTES = 8 * 1024 * 1024;

class FramedWorker {
  constructor(name, executable) {
    this.name = name;
    this.pending = new Map();
    this.buffer = Buffer.alloc(0);
    this.stderr = "";
    this.child = spawn(executable, [], { stdio: ["pipe", "pipe", "pipe"], windowsHide: true });
    this.child.stderr.setEncoding("utf8");
    this.child.stderr.on("data", (chunk) => { this.stderr = (this.stderr + chunk).slice(-4000); });
    this.child.stdout.on("data", (chunk) => { this.buffer = Buffer.concat([this.buffer, chunk]); this.drain(); });
    this.child.on("exit", (code) => {
      for (const waiter of this.pending.values()) waiter.reject(new Error(`${name} exited ${code}`));
      this.pending.clear();
    });
  }

  drain() {
    while (this.buffer.length >= 4) {
      const length = this.buffer.readUInt32LE(0);
      if (length <= 0 || length > MAX_FRAME_BYTES) throw new Error(`${this.name} invalid frame length ${length}`);
      if (this.buffer.length < length + 4) return;
      const body = this.buffer.subarray(4, length + 4);
      this.buffer = this.buffer.subarray(length + 4);
      const response = JSON.parse(body.toString("utf8"));
      const waiter = this.pending.get(response.request_id);
      if (!waiter) continue;
      clearTimeout(waiter.timer);
      this.pending.delete(response.request_id);
      waiter.resolve(response);
    }
  }

  request(kind, payload = {}, deadlineMs = 180_000) {
    const requestId = `${this.name}-${Date.now()}-${Math.random().toString(36).slice(2)}`;
    const body = Buffer.from(JSON.stringify({ protocol_version: 1, request_id: requestId, kind, payload, deadline_ms: deadlineMs }), "utf8");
    if (body.length > MAX_FRAME_BYTES) throw new Error(`${kind} request exceeds 8 MiB`);
    const header = Buffer.alloc(4);
    header.writeUInt32LE(body.length);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(requestId);
        reject(new Error(`${kind} timed out; stderr=${this.stderr}`));
      }, deadlineMs + 15_000);
      this.pending.set(requestId, { resolve, reject, timer });
      this.child.stdin.write(Buffer.concat([header, body]));
    }).then((response) => {
      if (!response.ok) throw new Error(`${kind}: ${response.error?.code}: ${response.error?.message}`);
      return response.payload;
    });
  }

  async close() {
    this.child.stdin.end();
    await new Promise((resolve) => setTimeout(resolve, 200));
    if (this.child.exitCode == null) this.child.kill();
  }
}

const engine = new FramedWorker("engine", engineExecutable);
const agent = new FramedWorker("agent", agentExecutable);
const startedAt = Date.now();

async function persistCheckpoint(taskId, payload) {
  if (!payload?.state || payload.checkpoint == null) return;
  const loaded = await engine.request("agent.task.load", { task_id: taskId }, 30_000);
  let maximum = Math.max(0, ...(loaded.events ?? []).map((event) => Number(event.seq) || 0));
  while (maximum < payload.state.accepted_seq) {
    maximum += 1;
    await engine.request("agent.event.append", {
      task_id: taskId,
      seq: maximum,
      event_id: `live-workflow:${taskId}:${maximum}`,
      event_kind: "agent.workflow.transition",
      event: { worker_state: payload.state },
    }, 30_000);
  }
  await engine.request("agent.checkpoint.put", {
    task_id: taskId,
    accepted_seq: payload.state.accepted_seq,
    phase: payload.state.phase,
    checkpoint: payload.checkpoint,
  }, 30_000);
}

async function executeEffect(effect) {
  if (effect.target !== "engine") throw new Error(`effect target denied: ${effect.target}`);
  const history = await engine.request("agent.effect.list", { task_id: effect.task_id }, 30_000);
  const prior = (history.items ?? []).filter((item) => item.idempotency_key === effect.idempotency_key || item.idempotency_key?.startsWith(`${effect.idempotency_key}:retry:`));
  const cached = prior.find((item) => item.status === "succeeded" && item.result != null);
  if (cached) return { call_id: effect.call_id, ok: true, payload: cached.result, error: null, cache_hit: true };
  const replayableRead = effect.kind === "research.agent_prepare_context" ||
    effect.kind === "research.agent_security_context" ||
    effect.kind === "research.agent_report_verify";
  if (prior.some((item) => item.status === "pending") && !replayableRead) throw new Error(`pending effect cannot be replayed: ${effect.call_id}`);
  const retry = prior.length;
  const effectId = `live-tool:${effect.task_id}:${effect.call_id}:${retry}`;
  await engine.request("agent.effect.begin", {
    effect_id: effectId,
    task_id: effect.task_id,
    caused_by_seq: effect.caused_by_seq,
    effect_kind: `engine.${effect.kind}`,
    effect: { target: "engine", kind: effect.kind, payload: effect.payload },
    idempotency_key: retry ? `${effect.idempotency_key}:retry:${retry}` : effect.idempotency_key,
  }, 30_000);
  try {
    const result = await engine.request(effect.kind, effect.payload, effect.deadline_ms);
    await engine.request("agent.effect.complete", { effect_id: effectId, status: "succeeded", result }, 30_000);
    return { call_id: effect.call_id, ok: true, payload: result, error: null, cache_hit: false };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    await engine.request("agent.effect.complete", { effect_id: effectId, status: "failed", result: { error: message } }, 30_000);
    return { call_id: effect.call_id, ok: false, payload: null, error: message, cache_hit: false };
  }
}

async function runWorkflow(taskId, acceptedSeq, payload) {
  const parentEffectId = `live-agent:${taskId}:${acceptedSeq}`;
  await engine.request("agent.effect.begin", {
    effect_id: parentEffectId,
    task_id: taskId,
    caused_by_seq: acceptedSeq,
    effect_kind: "agent.research.workflow",
    effect: { worker_request_kind: "agent.research.workflow", payload },
    idempotency_key: `${taskId}:agent.research.workflow:${acceptedSeq}`,
  }, 30_000);
  let response = await agent.request("agent.research.workflow", payload, 900_000);
  for (let round = 1; round <= 4; round += 1) {
    await persistCheckpoint(taskId, response);
    const effects = response.host_effects ?? [];
    if (!effects.length) {
      await engine.request("agent.effect.complete", { effect_id: parentEffectId, status: "succeeded", result: response }, 30_000);
      return response;
    }
    const toolResults = [];
    for (const effect of effects) toolResults.push(await executeEffect(effect));
    response = await agent.request(response.continuation.kind, { workflow: response.continuation.workflow, tool_results: toolResults }, 900_000);
  }
  throw new Error("Agent workflow exceeded four continuation rounds");
}

try {
  await Promise.all([
    engine.request("system.handshake", { app_version: "research-live-smoke", protocol_version: 1 }, 15_000),
    agent.request("system.handshake", { app_version: "research-live-smoke", protocol_version: 1 }, 15_000),
  ]);
  const taskId = crypto.randomUUID();
  const taskSpec = {
    objective: "基于截至当前可取得的最新数据，为2万元资金生成仅供人工执行的A股投资计划；反复核验新闻、行情、资金、财务与估值，证据不足就暂停或保留现金",
    security_universe: ["AGENT_BEST_AFTER_EVIDENCE"],
    as_of: new Date().toISOString(),
    research_start: "2025-08-24",
    research_end: "2026-08-24",
    investment_horizon: "1至3个月",
    comparison_benchmark: "000300",
    output_type: "manual_plan",
    evidence_requirement: "primary_sources",
  };
  await engine.request("agent.task.create", { task_id: taskId, reducer_version: "moonbit-agent-kernel-v1", task_spec: taskSpec, phase: "idle" }, 30_000);
  await engine.request("agent.event.append", { task_id: taskId, seq: 1, event_id: `live-start:${taskId}`, event_kind: "start", event: { spec: taskSpec } }, 30_000);
  const started = await agent.request("agent.start", { task_id: taskId, seq: 1, spec: taskSpec }, 30_000);
  if (started.state?.phase !== "preparing") throw new Error(`unexpected start phase ${started.state?.phase}`);
  await persistCheckpoint(taskId, started);
  const researched = await runWorkflow(taskId, started.state.accepted_seq, {
    task_id: taskId,
    depth: "exhaustive",
    tool_policy: "full",
    preferred_symbols: [],
    capital: 20_000,
  });
  if (researched.state?.phase !== "completed") throw new Error(`unexpected final phase ${researched.state?.phase}`);
  if (researched.state?.model_rounds !== 4) throw new Error(`expected 4 model rounds, got ${researched.state?.model_rounds}`);
  if (typeof researched.report !== "string" || researched.report.length < 800) throw new Error("final report is missing or too short");
  if (researched.verification?.version !== "engine-report-verifier-v1" || researched.verification.numeric_claims_checked < 1) {
    throw new Error("deterministic Engine report verification evidence is missing");
  }
  if (researched.report.includes("<think>")) throw new Error("private reasoning leaked into final report");
  if (!/(2万元|20000|20,000)/.test(researched.report)) throw new Error("final report lost the 20,000 CNY capital constraint");
  console.log(JSON.stringify({
    ok: true,
    elapsed_ms: Date.now() - startedAt,
    model_rounds: researched.state.model_rounds,
    evidence_count: researched.state.evidence_ids?.length ?? 0,
    report_chars: researched.report.length,
    phase: researched.state.phase,
    architecture: "MoonBit Agent effects -> durable Engine tools -> Agent continuation",
  }));
} finally {
  await Promise.all([engine.close(), agent.close()]);
}
