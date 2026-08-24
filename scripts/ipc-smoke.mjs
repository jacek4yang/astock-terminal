import { spawn } from "node:child_process";
import { once } from "node:events";

const executables = process.argv.slice(2);
if (!executables.length) throw new Error("usage: node scripts/ipc-smoke.mjs <worker.exe> [...]");

function readFrame(stream) {
  return new Promise((resolve, reject) => {
    let buffer = Buffer.alloc(0);
    let expected = null;
    const cleanup = () => {
      stream.off("data", onData);
      stream.off("end", onEnd);
      stream.off("error", onError);
    };
    const onData = (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      if (expected == null && buffer.length >= 4) {
        expected = buffer.readUInt32LE(0);
        if (expected <= 0 || expected > 8 * 1024 * 1024) {
          cleanup(); reject(new Error(`invalid response length: ${expected}`)); return;
        }
      }
      if (expected != null && buffer.length >= expected + 4) {
        const body = buffer.subarray(4, expected + 4);
        cleanup(); resolve(JSON.parse(body.toString("utf8")));
      }
    };
    const onEnd = () => { cleanup(); reject(new Error(`stdout ended with ${buffer.length} buffered bytes`)); };
    const onError = (error) => { cleanup(); reject(error); };
    stream.on("data", onData);
    stream.once("end", onEnd);
    stream.once("error", onError);
  });
}

function withTimeout(promise, milliseconds, message) {
  let timer;
  return Promise.race([
    promise,
    new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(message)), milliseconds); }),
  ]).finally(() => clearTimeout(timer));
}

async function smoke(executable) {
  const child = spawn(executable, [], {
    stdio: ["pipe", "pipe", "pipe"],
    windowsHide: true,
    env: { ...process.env, ASTOCK_DATA_DIR: process.env.ASTOCK_SMOKE_DATA_DIR },
  });
  let stderr = "";
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => { stderr += chunk; });
  const request = Buffer.from(JSON.stringify({
    protocol_version: 1,
    request_id: "smoke-handshake",
    kind: "system.handshake",
    payload: { app_version: "ipc-smoke", protocol_version: 1 },
    deadline_ms: 5000,
  }), "utf8");
  const header = Buffer.alloc(4); header.writeUInt32LE(request.length);
  child.stdin.write(Buffer.concat([header, request]));
  try {
    const response = await withTimeout(readFrame(child.stdout), 10_000, "handshake response timeout");
    if (!response.ok || response.request_id !== "smoke-handshake" || response.protocol_version !== 1) {
      throw new Error(`invalid handshake response: ${JSON.stringify(response)}`);
    }
    child.stdin.end();
    const [code] = await withTimeout(once(child, "exit"), 5_000, "worker exit timeout");
    if (code !== 0) throw new Error(`${executable} exited ${code}`);
  } catch (error) {
    child.kill();
    throw new Error(`${executable}: ${error.message}; stderr=${stderr.slice(0, 2000)}`);
  }
  console.log(`IPC handshake passed: ${executable}`);
}

for (const executable of executables) await smoke(executable);
