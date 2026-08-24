import { spawn } from "node:child_process";
import crypto from "node:crypto";
import { inferHandshakeRole, validateHandshakePayload } from "./handshake-contract.mjs";

export const MAX_FRAME_BYTES = 8 * 1024 * 1024;

export class FramedWorker {
  constructor(executable, { env = {}, unsetEnv = [], name = "worker" } = {}) {
    this.name = name;
    const childEnv = { ...process.env, ...env };
    for (const key of unsetEnv) delete childEnv[key];
    this.child = spawn(executable, [], {
      stdio: ["pipe", "pipe", "pipe"],
      windowsHide: true,
      env: childEnv,
    });
    this.buffer = Buffer.alloc(0);
    this.pending = new Map();
    this.responseOrder = [];
    this.stderr = "";
    this.child.stderr.setEncoding("utf8");
    this.child.stderr.on("data", (chunk) => {
      this.stderr = (this.stderr + chunk).slice(-16_384);
    });
    this.child.stdout.on("data", (chunk) => {
      this.buffer = Buffer.concat([this.buffer, chunk]);
      this.#drain();
    });
    this.child.on("error", (error) => this.#rejectAll(error));
    this.child.on("exit", (code, signal) => {
      this.#rejectAll(new Error(`${this.name} exited code=${code} signal=${signal ?? "none"}; stderr=${this.stderr}`));
    });
  }

  #rejectAll(error) {
    for (const { reject, timer } of this.pending.values()) {
      clearTimeout(timer);
      reject(error);
    }
    this.pending.clear();
  }

  #drain() {
    while (this.buffer.length >= 4) {
      const length = this.buffer.readUInt32LE(0);
      if (length <= 0 || length > MAX_FRAME_BYTES) {
        this.#rejectAll(new Error(`${this.name} returned invalid frame length ${length}`));
        this.buffer = Buffer.alloc(0);
        return;
      }
      if (this.buffer.length < length + 4) return;
      const body = this.buffer.subarray(4, length + 4);
      this.buffer = this.buffer.subarray(length + 4);
      let response;
      try {
        response = JSON.parse(body.toString("utf8"));
      } catch (error) {
        this.#rejectAll(new Error(`${this.name} returned malformed JSON: ${error}`));
        return;
      }
      const waiter = this.pending.get(response.request_id);
      if (!waiter) {
        this.#rejectAll(new Error(`${this.name} returned an unknown or duplicate request_id ${response.request_id}`));
        return;
      }
      this.pending.delete(response.request_id);
      this.responseOrder.push(response.request_id);
      clearTimeout(waiter.timer);
      waiter.resolve(response);
    }
  }

  request(kind, payload = {}, { deadlineMs = 30_000, cancellationId, requestId = `release-${crypto.randomUUID()}` } = {}) {
    if (this.child.exitCode != null || this.child.killed) {
      return Promise.reject(new Error(`${this.name} is not running`));
    }
    const body = Buffer.from(JSON.stringify({
      protocol_version: 1,
      request_id: requestId,
      kind,
      payload,
      deadline_ms: deadlineMs,
      ...(cancellationId ? { cancellation_id: cancellationId } : {}),
    }), "utf8");
    if (body.length > MAX_FRAME_BYTES) {
      return Promise.reject(new Error(`request body exceeds ${MAX_FRAME_BYTES} bytes`));
    }
    if (this.pending.has(requestId)) {
      return Promise.reject(new Error(`${this.name} already has an in-flight request_id ${requestId}`));
    }
    const header = Buffer.alloc(4);
    header.writeUInt32LE(body.length);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(requestId);
        reject(new Error(`${this.name} ${kind} timed out after ${deadlineMs}ms`));
      }, deadlineMs + 5_000);
      this.pending.set(requestId, { resolve, reject, timer });
      this.child.stdin.write(Buffer.concat([header, body]), (error) => {
        if (!error) return;
        const waiter = this.pending.get(requestId);
        if (!waiter) return;
        this.pending.delete(requestId);
        clearTimeout(waiter.timer);
        reject(error);
      });
    }).then((response) => {
      if (!response.ok) {
        const error = new Error(`${kind}: ${response.error?.code ?? "worker_error"}: ${response.error?.message ?? "unknown error"}`);
        error.response = response;
        throw error;
      }
      return response.payload;
    });
  }

  async shutdown() {
    if (this.child.exitCode != null) return;
    try {
      await this.request("system.shutdown", {}, { deadlineMs: 5_000 });
    } catch {
      this.child.stdin.end();
    }
    await this.waitForExit(5_000).catch(() => this.kill());
  }

  kill(signal = "SIGKILL") {
    if (this.child.exitCode == null) this.child.kill(signal);
  }

  waitForExit(timeoutMs = 5_000) {
    if (this.child.exitCode != null) return Promise.resolve(this.child.exitCode);
    return Promise.race([
      new Promise((resolve) => this.child.once("exit", resolve)),
      new Promise((_, reject) => setTimeout(() => reject(new Error(`${this.name} did not exit`)), timeoutMs)),
    ]);
  }
}

export async function handshake(worker, client = "release-evidence") {
  const response = await worker.request("system.handshake", {
    app_version: client,
    protocol_version: 1,
  }, { deadlineMs: 15_000 });
  return validateHandshakePayload(inferHandshakeRole(response), response);
}
