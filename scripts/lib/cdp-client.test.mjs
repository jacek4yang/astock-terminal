import assert from "node:assert/strict";
import http from "node:http";
import test from "node:test";
import { CdpClient, waitForCdpPage } from "./cdp-client.mjs";

async function serve(handler) {
  const server = http.createServer(handler);
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  return {
    port: server.address().port,
    close: () => new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve())),
  };
}

test("accepts a page debugger only on the requested loopback port", async () => {
  let port;
  const fixture = await serve((_request, response) => {
    response.setHeader("content-type", "application/json");
    response.end(JSON.stringify([{ type: "page", webSocketDebuggerUrl: `ws://127.0.0.1:${port}/devtools/page/fixture` }]));
  });
  port = fixture.port;
  try {
    const page = await waitForCdpPage(port, { timeoutMs: 500 });
    assert.equal(page.type, "page");
  } finally {
    await fixture.close();
  }
});

test("rejects an advertised debugger endpoint on another host", async () => {
  const fixture = await serve((_request, response) => {
    response.setHeader("content-type", "application/json");
    response.end(JSON.stringify([{ type: "page", webSocketDebuggerUrl: "ws://example.com:9222/devtools/page/fixture" }]));
  });
  try {
    await assert.rejects(waitForCdpPage(fixture.port, { timeoutMs: 200 }), /outside the expected loopback port/);
  } finally {
    await fixture.close();
  }
});

test("rejects an oversized discovery response", async () => {
  const fixture = await serve((_request, response) => {
    response.setHeader("content-length", String(1024 * 1024 + 1));
    response.end("[]");
  });
  try {
    await assert.rejects(waitForCdpPage(fixture.port, { timeoutMs: 200 }), /exceeded 1 MiB/);
  } finally {
    await fixture.close();
  }
});

class FakeSocket {
  constructor() {
    this.readyState = 1;
    this.listeners = new Map();
    this.sent = [];
  }
  addEventListener(type, listener) {
    const listeners = this.listeners.get(type) ?? new Set();
    listeners.add(listener);
    this.listeners.set(type, listeners);
  }
  send(message) { this.sent.push(message); }
  close() { this.readyState = 3; }
  emit(type, value = {}) {
    for (const listener of this.listeners.get(type) ?? []) listener(value);
  }
}

test("correlates CDP replies and rejects a malformed stream", async () => {
  const socket = new FakeSocket();
  const client = new CdpClient(socket);
  const request = client.call("Runtime.enable");
  const envelope = JSON.parse(socket.sent[0]);
  socket.emit("message", { data: JSON.stringify({ id: envelope.id, result: { enabled: true } }) });
  assert.deepEqual(await request, { enabled: true });

  const malformed = client.call("Page.enable");
  socket.emit("message", { data: "<html>not cdp</html>" });
  await assert.rejects(malformed, /malformed JSON/);
});
