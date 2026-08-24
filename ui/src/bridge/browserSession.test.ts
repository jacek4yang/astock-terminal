import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const bootstrapToken = "b".repeat(43);
const sessionToken = "s".repeat(43);
const port = 43123;

function protocolReply(body: string) {
  const { request } = JSON.parse(body) as { request: { request_id: string; kind: string } };
  return new Response(JSON.stringify({
    protocol_version: 1,
    request_id: request.request_id,
    kind: request.kind,
    ok: true,
    payload: { status: "ready" },
  }), { status: 200, headers: { "content-type": "application/json" } });
}

describe("development browser Bridge session", () => {
  beforeEach(() => {
    vi.resetModules();
    vi.restoreAllMocks();
    window.sessionStorage.clear();
    window.history.replaceState({}, "", "/");
  });

  afterEach(() => {
    window.sessionStorage.clear();
    window.history.replaceState({}, "", "/");
  });

  it("scrubs and consumes the URL-fragment bootstrap exactly once", async () => {
    window.history.replaceState({}, "", `/#nativeTest=1&bridgePort=${port}&bridgeToken=${bootstrapToken}`);
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = String(input);
      if (url.endsWith("/session")) {
        expect(init?.headers).toEqual({ "X-AStock-Test-Token": bootstrapToken });
        return new Response(JSON.stringify({ session_token: sessionToken }), { status: 200 });
      }
      expect(init?.headers).toMatchObject({ "X-AStock-Test-Token": sessionToken });
      return protocolReply(String(init?.body));
    });

    const bridge = await import("./index");
    expect(window.location.hash).toBe("");
    expect(window.history.state.__astockBrowserBridge).toEqual({ port });
    await bridge.requestNative("host", "diagnostics.status");
    await bridge.requestNative("host", "diagnostics.status");

    expect(fetchMock).toHaveBeenCalledTimes(3);
    expect(fetchMock.mock.calls.filter(([url]) => String(url).endsWith("/session"))).toHaveLength(1);
    expect(window.sessionStorage.getItem(`astock.browser-bridge.session.${port}`)).toBe(sessionToken);
  });

  it("reuses only the tab session after a renderer reload", async () => {
    window.history.replaceState({ __astockBrowserBridge: { port } }, "", "/");
    window.sessionStorage.setItem(`astock.browser-bridge.session.${port}`, sessionToken);
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      expect(String(input)).toBe(`http://127.0.0.1:${port}/request`);
      expect(init?.headers).toMatchObject({ "X-AStock-Test-Token": sessionToken });
      return protocolReply(String(init?.body));
    });

    const bridge = await import("./index");
    await bridge.requestNative("host", "diagnostics.status");
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});
