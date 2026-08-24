const MAX_MESSAGE_BYTES = 16 * 1024 * 1024;
const MAX_DISCOVERY_BYTES = 1024 * 1024;

const delay = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

export async function waitForCdpPage(port, { timeoutMs = 30_000 } = {}) {
  if (!Number.isInteger(port) || port < 1024 || port > 65535) throw new Error(`invalid CDP port ${port}`);
  const deadline = Date.now() + timeoutMs;
  let lastError = "endpoint did not respond";
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/json/list`, { cache: "no-store" });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const targets = JSON.parse(await readBoundedText(response, MAX_DISCOVERY_BYTES));
      if (!Array.isArray(targets)) throw new Error("CDP target list was not an array");
      const page = targets.find((target) => target?.type === "page" && typeof target.webSocketDebuggerUrl === "string");
      if (page) {
        const debuggerUrl = new URL(page.webSocketDebuggerUrl);
        if (debuggerUrl.protocol !== "ws:" || debuggerUrl.hostname !== "127.0.0.1" || Number(debuggerUrl.port) !== port) {
          throw new Error("CDP advertised a debugger endpoint outside the expected loopback port");
        }
        if (debuggerUrl.username || debuggerUrl.password || !debuggerUrl.pathname.startsWith("/devtools/page/")) {
          throw new Error("CDP advertised an invalid page debugger endpoint");
        }
        return page;
      }
      lastError = "no page target was advertised";
    } catch (error) {
      lastError = error instanceof Error ? error.message : String(error);
    }
    await delay(100);
  }
  throw new Error(`CEF CDP page was not ready after ${timeoutMs}ms: ${lastError}`);
}

async function readBoundedText(response, maximumBytes) {
  const declaredLength = Number(response.headers.get("content-length"));
  if (Number.isFinite(declaredLength) && declaredLength > maximumBytes) throw new Error("CDP discovery response exceeded 1 MiB");
  if (!response.body) throw new Error("CDP discovery response had no body");
  const reader = response.body.getReader();
  const chunks = [];
  let length = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    length += value.byteLength;
    if (length > maximumBytes) {
      await reader.cancel();
      throw new Error("CDP discovery response exceeded 1 MiB");
    }
    chunks.push(value);
  }
  const bytes = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
}

export class CdpClient {
  constructor(socket) {
    this.socket = socket;
    this.nextId = 1;
    this.pending = new Map();
    this.listeners = new Map();
    socket.addEventListener("message", (event) => this.#onMessage(event.data));
    socket.addEventListener("close", () => this.#rejectAll(new Error("CDP socket closed")));
    socket.addEventListener("error", () => this.#rejectAll(new Error("CDP socket failed")));
  }

  static async connect(webSocketUrl, { timeoutMs = 10_000 } = {}) {
    if (typeof WebSocket !== "function") throw new Error("this Node runtime does not expose WebSocket");
    const socket = new WebSocket(webSocketUrl);
    await new Promise((resolve, reject) => {
      const cleanup = () => {
        clearTimeout(timer);
        socket.removeEventListener("open", onOpen);
        socket.removeEventListener("error", onError);
      };
      const onOpen = () => { cleanup(); resolve(); };
      const onError = () => { cleanup(); reject(new Error("CDP WebSocket connection failed")); };
      const timer = setTimeout(() => {
        cleanup();
        socket.close();
        reject(new Error("CDP WebSocket connection timed out"));
      }, timeoutMs);
      socket.addEventListener("open", onOpen, { once: true });
      socket.addEventListener("error", onError, { once: true });
    });
    return new CdpClient(socket);
  }

  #onMessage(raw) {
    const text = typeof raw === "string" ? raw : Buffer.from(raw).toString("utf8");
    if (Buffer.byteLength(text, "utf8") > MAX_MESSAGE_BYTES) {
      this.#rejectAll(new Error("CDP message exceeded 16 MiB"));
      this.socket.close();
      return;
    }
    let message;
    try { message = JSON.parse(text); }
    catch {
      this.#rejectAll(new Error("CDP returned malformed JSON"));
      return;
    }
    if (Number.isInteger(message.id)) {
      const pending = this.pending.get(message.id);
      if (!pending) return;
      this.pending.delete(message.id);
      clearTimeout(pending.timer);
      if (message.error) pending.reject(new Error(`${pending.method}: ${message.error.message ?? JSON.stringify(message.error)}`));
      else pending.resolve(message.result ?? {});
      return;
    }
    if (typeof message.method === "string") {
      for (const listener of this.listeners.get(message.method) ?? []) listener(message.params ?? {});
    }
  }

  #rejectAll(error) {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
  }

  call(method, params = {}, { timeoutMs = 30_000 } = {}) {
    if (this.socket.readyState !== 1) return Promise.reject(new Error("CDP socket is not open"));
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`${method} timed out after ${timeoutMs}ms`));
      }, timeoutMs);
      this.pending.set(id, { method, resolve, reject, timer });
      this.socket.send(JSON.stringify({ id, method, params }));
    });
  }

  on(method, listener) {
    const listeners = this.listeners.get(method) ?? new Set();
    listeners.add(listener);
    this.listeners.set(method, listeners);
    return () => listeners.delete(listener);
  }

  async evaluate(expression, { awaitPromise = true, returnByValue = true, timeoutMs = 30_000 } = {}) {
    const result = await this.call("Runtime.evaluate", {
      expression,
      awaitPromise,
      returnByValue,
      userGesture: false,
    }, { timeoutMs });
    if (result.exceptionDetails) throw new Error(`Runtime.evaluate failed: ${result.exceptionDetails.text ?? "JavaScript exception"}`);
    return result.result?.value;
  }

  close() {
    if (this.socket.readyState === 1 || this.socket.readyState === 0) this.socket.close();
  }
}
