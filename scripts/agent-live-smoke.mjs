import { spawn } from "node:child_process";

const executable = process.argv[2];
if (!executable) throw new Error("usage: node scripts/agent-live-smoke.mjs <agent-worker.exe>");

class FramedWorker {
  constructor(path) {
    this.child = spawn(path, [], { stdio: ["pipe", "pipe", "pipe"], windowsHide: true });
    this.buffer = Buffer.alloc(0);
    this.waiters = [];
    this.stderr = "";
    this.child.stderr.setEncoding("utf8");
    this.child.stderr.on("data", (chunk) => { this.stderr = (this.stderr + chunk).slice(-4000); });
    this.child.stdout.on("data", (chunk) => { this.buffer = Buffer.concat([this.buffer, chunk]); this.drain(); });
    this.child.on("exit", (code) => {
      while (this.waiters.length) this.waiters.shift().reject(new Error(`Agent Worker exited ${code}`));
    });
  }

  drain() {
    while (this.waiters.length && this.buffer.length >= 4) {
      const length = this.buffer.readUInt32LE(0);
      if (length <= 0 || length > 8 * 1024 * 1024) {
        this.waiters.shift().reject(new Error(`invalid response frame length ${length}`));
        this.buffer = Buffer.alloc(0);
        continue;
      }
      if (this.buffer.length < length + 4) return;
      const body = this.buffer.subarray(4, length + 4);
      this.buffer = this.buffer.subarray(length + 4);
      const waiter = this.waiters.shift();
      try { waiter.resolve(JSON.parse(body.toString("utf8"))); }
      catch { waiter.reject(new Error("Agent Worker returned non-JSON protocol data")); }
    }
  }

  request(kind, payload = {}, deadlineMs = 180_000) {
    const requestId = `live-${Date.now()}-${Math.random().toString(36).slice(2)}`;
    const body = Buffer.from(JSON.stringify({
      protocol_version: 1,
      request_id: requestId,
      kind,
      payload,
      deadline_ms: deadlineMs,
    }), "utf8");
    const header = Buffer.alloc(4);
    header.writeUInt32LE(body.length);
    const reply = new Promise((resolve, reject) => this.waiters.push({ resolve, reject }));
    this.child.stdin.write(Buffer.concat([header, body]));
    this.drain();
    return Promise.race([
      reply,
      new Promise((_, reject) => setTimeout(() => reject(new Error(`${kind} timed out`)), deadlineMs + 15_000)),
    ]).then((response) => {
      if (response.request_id !== requestId) throw new Error(`${kind} response correlation failed`);
      if (!response.ok) throw new Error(`${kind}: ${response.error?.code ?? "provider failure"}: ${response.error?.message ?? "unknown"}`);
      return response.payload;
    });
  }

  async close() {
    this.child.stdin.end();
    const exited = new Promise((resolve) => this.child.once("exit", resolve));
    await Promise.race([exited, new Promise((resolve) => setTimeout(resolve, 1000))]);
    if (this.child.exitCode == null) {
      this.child.kill();
      await exited;
    }
  }
}

const worker = new FramedWorker(executable);
try {
  await worker.request("system.handshake", { app_version: "agent-live-smoke", protocol_version: 1 }, 10_000);
  const provider = await worker.request("agent.provider.test", {}, 90_000);
  if (!provider.catalog_verified || !provider.model) throw new Error("MiniMax model catalog was not verified");

  const taskId = crypto.randomUUID();
  const started = await worker.request("agent.start", {
    task_id: taskId,
    seq: 1,
    spec: {
      objective: "基于给定快照生成仅供人工执行的投资研究计划，并明确数据缺口",
      security_universe: ["600519"],
      as_of: "2026-08-24T15:00:00+08:00",
      research_start: "2025-08-24",
      research_end: "2026-08-24",
      investment_horizon: "1至3个月",
      comparison_benchmark: "000300",
      output_type: "manual_plan",
      evidence_requirement: "strict",
    },
  }, 20_000);
  if (started.state?.phase !== "preparing") throw new Error(`unexpected start phase ${started.state?.phase}`);

  const beganAt = Date.now();
  const researched = await worker.request("agent.research", {
    task_id: taskId,
    context: {
      source: "live_smoke_fixture",
      symbol: "600519",
      retrieved_at: "2026-08-24T15:00:00+08:00",
      snapshot: {
        quote: { name: "贵州茅台", price: 1500, change_pct: 0.5 },
        data_boundary: "故意只提供最小行情快照，用于验证模型会标注数据缺口",
      },
    },
  }, 240_000);
  if (researched.state?.phase !== "completed") throw new Error(`unexpected research phase ${researched.state?.phase}`);
  if (typeof researched.report !== "string" || researched.report.length < 120) throw new Error("research report was missing or too short");
  if (researched.report.includes("<think>")) throw new Error("private reasoning leaked into the report");
  console.log(JSON.stringify({
    ok: true,
    provider: provider.provider,
    model: provider.model,
    region: provider.api_host.includes("minimaxi.com") ? "mainland" : "international",
    report_chars: researched.report.length,
    elapsed_ms: Date.now() - beganAt,
    phase: researched.state.phase,
  }));
} finally {
  await worker.close();
}
