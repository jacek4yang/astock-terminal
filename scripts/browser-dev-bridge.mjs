import { createServer } from "node:http";
import { spawn } from "node:child_process";

const [engineExecutable, agentExecutable, bindHost = "127.0.0.1"] = process.argv.slice(2);
if (!engineExecutable || !agentExecutable) {
  throw new Error("usage: node scripts/browser-dev-bridge.mjs <engine.exe> <agent-worker.exe> [private-bind-host]");
}

const privateIpv4 = /^(?:127(?:\.\d{1,3}){3}|10(?:\.\d{1,3}){3}|192\.168(?:\.\d{1,3}){2}|172\.(?:1[6-9]|2\d|3[01])(?:\.\d{1,3}){2})$/;
if (bindHost !== "0.0.0.0" && !privateIpv4.test(bindHost)) {
  throw new Error("browser test bridge may only bind to loopback or an RFC1918 IPv4 address");
}

const MAX_FRAME_BYTES = 8 * 1024 * 1024;
const ALLOWED_ORIGINS = new Set([
  `http://${bindHost}:5173`,
  "http://host.docker.internal:5173",
  "http://host.containers.internal:5173",
  "http://127.0.0.1:5173",
  "http://localhost:5173",
]);

class WorkerChannel {
  constructor(name, executable) {
    this.name = name;
    this.pending = new Map();
    this.buffer = Buffer.alloc(0);
    this.requestCount = 0;
    this.failureCount = 0;
    this.child = spawn(executable, [], { stdio: ["pipe", "pipe", "inherit"], windowsHide: true });
    this.child.stdout.on("data", (chunk) => { this.buffer = Buffer.concat([this.buffer, chunk]); this.drain(); });
    this.child.on("exit", (code) => {
      for (const { reject, timer } of this.pending.values()) {
        clearTimeout(timer);
        reject(new Error(`${name} Worker 已退出（${code ?? "unknown"}）`));
      }
      this.pending.clear();
    });
  }

  drain() {
    while (this.buffer.length >= 4) {
      const length = this.buffer.readUInt32LE(0);
      if (length <= 0 || length > MAX_FRAME_BYTES) throw new Error(`${this.name} returned an invalid frame length`);
      if (this.buffer.length < length + 4) return;
      const body = this.buffer.subarray(4, length + 4);
      this.buffer = this.buffer.subarray(length + 4);
      let response;
      try { response = JSON.parse(body.toString("utf8")); }
      catch { throw new Error(`${this.name} returned non-JSON protocol data`); }
      const waiter = this.pending.get(response.request_id);
      if (!waiter) continue;
      clearTimeout(waiter.timer);
      this.pending.delete(response.request_id);
      waiter.resolve(response);
    }
  }

  request(request) {
    if (!request?.request_id || typeof request.request_id !== "string") return Promise.reject(new Error("request_id is required"));
    if (this.pending.size >= 256) return Promise.reject(new Error(`${this.name} request queue is full`));
    const body = Buffer.from(JSON.stringify(request), "utf8");
    if (body.length > MAX_FRAME_BYTES) return Promise.reject(new Error("request frame exceeds 8 MiB"));
    const header = Buffer.alloc(4);
    header.writeUInt32LE(body.length);
    const timeoutMs = Math.max(1_000, Math.min(300_000, Number(request.deadline_ms) || 30_000));
    this.requestCount += 1;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(request.request_id);
        this.failureCount += 1;
        reject(new Error(`${this.name} request timed out`));
      }, timeoutMs);
      this.pending.set(request.request_id, { resolve, reject, timer });
      this.child.stdin.write(Buffer.concat([header, body]));
    });
  }

  diagnostics() {
    return {
      name: this.name,
      pid: this.child.pid,
      request_count: this.requestCount,
      failure_count: this.failureCount,
      status: this.child.exitCode == null ? "ready" : "degraded",
    };
  }

  close() {
    this.child.stdin.end();
    setTimeout(() => { if (this.child.exitCode == null) this.child.kill(); }, 1000).unref();
  }
}

const engine = new WorkerChannel("engine", engineExecutable);
const agent = new WorkerChannel("agent", agentExecutable);
const channels = { engine, agent };

function json(response, status, body, origin) {
  response.writeHead(status, {
    "Content-Type": "application/json; charset=utf-8",
    "Cache-Control": "no-store",
    ...(origin ? { "Access-Control-Allow-Origin": origin, Vary: "Origin" } : {}),
  });
  response.end(JSON.stringify(body));
}

async function readBody(request) {
  let size = 0;
  const chunks = [];
  for await (const chunk of request) {
    size += chunk.length;
    if (size > MAX_FRAME_BYTES) throw new Error("request body exceeds 8 MiB");
    chunks.push(chunk);
  }
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

function hostResponse(request) {
  return {
    protocol_version: 1,
    request_id: request.request_id,
    kind: request.kind,
    ok: true,
    payload: request.kind === "diagnostics.status" ? {
      status: "ready",
      host_version: "browser-test-bridge-v1",
      engine: engine.diagnostics(),
      agent: agent.diagnostics(),
      queue_limit: 256,
      test_bridge: true,
    } : { accepted: false, browser_test_bridge: true },
  };
}

await Promise.all([
  engine.request({ protocol_version: 1, request_id: "browser-engine-handshake", kind: "system.handshake", payload: { app_version: "browser-test", protocol_version: 1 }, deadline_ms: 15_000 }),
  agent.request({ protocol_version: 1, request_id: "browser-agent-handshake", kind: "system.handshake", payload: { app_version: "browser-test", protocol_version: 1 }, deadline_ms: 15_000 }),
]);

const server = createServer(async (request, response) => {
  const origin = request.headers.origin;
  const allowedOrigin = origin && ALLOWED_ORIGINS.has(origin) ? origin : undefined;
  if (request.method === "OPTIONS") {
    if (!allowedOrigin) return json(response, 403, { error: "origin denied" });
    response.writeHead(204, {
      "Access-Control-Allow-Origin": allowedOrigin,
      "Access-Control-Allow-Methods": "GET,POST,OPTIONS",
      "Access-Control-Allow-Headers": "Content-Type",
      "Access-Control-Max-Age": "600",
      Vary: "Origin",
    });
    return response.end();
  }
  if (request.method === "GET" && request.url === "/health") {
    return json(response, 200, { status: "ready", engine: engine.diagnostics(), agent: agent.diagnostics() }, allowedOrigin);
  }
  if (request.method !== "POST" || request.url !== "/request" || !allowedOrigin) {
    return json(response, 404, { error: "not found" }, allowedOrigin);
  }
  try {
    const body = await readBody(request);
    if (!body || !["engine", "agent", "host"].includes(body.target)) throw new Error("unsupported target");
    const result = body.target === "host" ? hostResponse(body.request) : await channels[body.target].request(body.request);
    json(response, 200, result, allowedOrigin);
  } catch (error) {
    json(response, 502, { error: error instanceof Error ? error.message : String(error) }, allowedOrigin);
  }
});

server.listen(4174, bindHost, () => {
  console.log(JSON.stringify({ status: "ready", url: `http://${bindHost}:4174`, engine_pid: engine.child.pid, agent_pid: agent.child.pid }));
});

const shutdown = () => {
  server.close();
  engine.close();
  agent.close();
};
process.once("SIGINT", shutdown);
process.once("SIGTERM", shutdown);
