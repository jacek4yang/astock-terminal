import { spawn } from "node:child_process";

const executable = process.argv[2];
if (!executable) throw new Error("usage: node scripts/credential-readback-smoke.mjs <engine.exe>");

const child = spawn(executable, [], { stdio: ["pipe", "pipe", "pipe"], windowsHide: true });
let buffer = Buffer.alloc(0);
let stderr = "";
let requestSequence = 0;
const pending = new Map();

child.stderr.setEncoding("utf8");
child.stderr.on("data", (chunk) => { stderr = (stderr + chunk).slice(-2000); });
child.stdout.on("data", (chunk) => {
  buffer = Buffer.concat([buffer, chunk]);
  while (buffer.length >= 4) {
    const length = buffer.readUInt32LE(0);
    if (length <= 0 || length > 8 * 1024 * 1024) throw new Error(`invalid Engine frame length ${length}`);
    if (buffer.length < length + 4) return;
    const body = buffer.subarray(4, length + 4);
    buffer = buffer.subarray(length + 4);
    const response = JSON.parse(body.toString("utf8"));
    const waiter = pending.get(response.request_id);
    if (!waiter) continue;
    clearTimeout(waiter.timer);
    pending.delete(response.request_id);
    if (response.ok) waiter.resolve(response.payload);
    else waiter.reject(new Error(`${response.error?.code ?? "engine_error"}: ${response.error?.message ?? "unknown"}`));
  }
});
child.on("exit", (code) => {
  for (const waiter of pending.values()) waiter.reject(new Error(`Engine exited ${code}; stderr=${stderr}`));
  pending.clear();
});

function request(kind, payload = {}, deadlineMs = 15_000) {
  requestSequence += 1;
  const requestId = `credential-readback-${requestSequence}`;
  const body = Buffer.from(JSON.stringify({
    protocol_version: 1,
    request_id: requestId,
    kind,
    payload,
    deadline_ms: deadlineMs,
  }), "utf8");
  const header = Buffer.alloc(4);
  header.writeUInt32LE(body.length);
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      pending.delete(requestId);
      reject(new Error(`${kind} timed out; stderr=${stderr}`));
    }, deadlineMs + 5_000);
    pending.set(requestId, { resolve, reject, timer });
    child.stdin.write(Buffer.concat([header, body]));
  });
}

try {
  const handshake = await request("system.handshake", { app_version: "credential-readback", protocol_version: 1 });
  if (handshake.protocol_version !== 1) throw new Error("Engine protocol mismatch");
  const status = await request("credentials.status");
  const minimax = status.providers?.minimax === true;
  const joinquant = status.providers?.joinquant === true;
  if (!minimax || !joinquant) throw new Error("rotated MiniMax and JoinQuant credentials are not both readable");
  console.log(JSON.stringify({ ok: true, minimax, joinquant }));
} finally {
  child.stdin.end();
  await new Promise((resolve) => setTimeout(resolve, 100));
  if (child.exitCode == null) child.kill();
}
