import { describe, expect, it } from "vitest";

import { parseBrowserBridgeResponse } from "./index";

describe("browser test Bridge response parser", () => {
  it("accepts a valid protocol response", async () => {
    const response = new Response(JSON.stringify({
      protocol_version: 1,
      request_id: "request-1",
      kind: "diagnostics.status",
      ok: true,
      payload: { status: "ready" },
    }), { status: 200, headers: { "content-type": "application/json" } });
    await expect(parseBrowserBridgeResponse(response)).resolves.toMatchObject({ ok: true });
  });

  it("turns an HTML proxy response into an actionable error without exposing its body", async () => {
    const response = new Response("<html><body>upstream gateway details</body></html>", {
      status: 502,
      headers: { "content-type": "text/html" },
    });
    await expect(parseBrowserBridgeResponse(response)).rejects.toThrow(
      "本地浏览器测试桥返回了非 JSON 响应（HTTP 502）",
    );
    await expect(parseBrowserBridgeResponse(new Response("<secret>do-not-display</secret>", { status: 502 })))
      .rejects.not.toThrow("do-not-display");
  });

  it("preserves a bounded structured Bridge error", async () => {
    const response = new Response(JSON.stringify({ error: "Agent Worker 已退出" }), {
      status: 503,
      headers: { "content-type": "application/json" },
    });
    await expect(parseBrowserBridgeResponse(response)).rejects.toThrow("本地浏览器测试桥失败：Agent Worker 已退出");
  });

  it("rejects a declared oversized response before parsing", async () => {
    const response = new Response("{}", {
      status: 200,
      headers: { "content-length": String(8 * 1024 * 1024 + 1) },
    });
    await expect(parseBrowserBridgeResponse(response)).rejects.toThrow("超过 8 MiB 安全上限");
  });
});
