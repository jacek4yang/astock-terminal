import { afterEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
const listen = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: (...args: unknown[]) => listen(...args) }));

const bridge = await import("./index");

/** Pretend the renderer is running inside the Tauri host. */
function enterTauriHost(): void {
  (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
}

function leaveTauriHost(): void {
  delete (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
}

afterEach(() => {
  leaveTauriHost();
  invoke.mockReset();
  listen.mockReset();
});

describe("Tauri transport", () => {
  it("sends the protocol envelope through one typed command", async () => {
    enterTauriHost();
    invoke.mockImplementation((_command: string, args: { envelope: { request_id: string } }) =>
      Promise.resolve({
        protocol_version: 1,
        request_id: args.envelope.request_id,
        kind: "diagnostics.status",
        ok: true,
        payload: { status: "ready" },
      }),
    );

    await expect(bridge.requestNative("host", "diagnostics.status")).resolves.toMatchObject({
      status: "ready",
    });

    // One command carries every renderer request, so the desktop cannot grow a
    // GUI-specific surface that diverges from the terminal's contract.
    expect(invoke).toHaveBeenCalledTimes(1);
    const [command, args] = invoke.mock.calls[0] as [string, { target: string; envelope: unknown }];
    expect(command).toBe("bridge_request");
    expect(args.target).toBe("host");
    expect(args.envelope).toMatchObject({ protocol_version: 1, kind: "diagnostics.status" });
  });

  it("refuses a response whose request id does not correlate", async () => {
    enterTauriHost();
    invoke.mockResolvedValue({
      protocol_version: 1,
      request_id: "someone-elses-request",
      kind: "diagnostics.status",
      ok: true,
      payload: {},
    });
    await expect(bridge.requestNative("host", "diagnostics.status")).rejects.toThrow(
      "无法关联的响应",
    );
  });

  it("surfaces a typed host error rather than a generic failure", async () => {
    enterTauriHost();
    invoke.mockImplementation((_command: string, args: { envelope: { request_id: string } }) =>
      Promise.resolve({
        protocol_version: 1,
        request_id: args.envelope.request_id,
        kind: "diagnostics.status",
        ok: false,
        payload: null,
        error: { code: "unsupported_host_request", message: "不支持的宿主请求", retryable: false },
      }),
    );
    await expect(bridge.requestNative("host", "diagnostics.status")).rejects.toThrow(
      "不支持的宿主请求",
    );
  });

  it("still enforces the request-kind allowlist before reaching the host", async () => {
    enterTauriHost();
    await expect(
      (bridge.requestNative as (target: "host", kind: string) => Promise<unknown>)(
        "host",
        "window.evaluate_script",
      ),
    ).rejects.toThrow("协议白名单");
    expect(invoke).not.toHaveBeenCalled();
  });

  it("reports no native host outside Tauri and outside the browser bridge", () => {
    expect(bridge.isNativeHost()).toBe(false);
    enterTauriHost();
    expect(bridge.isNativeHost()).toBe(true);
  });

  it("detaches an event subscription even when it unsubscribes before attaching", async () => {
    enterTauriHost();
    const unlisten = vi.fn();
    const attached: { resolve?: (value: unknown) => void } = {};
    listen.mockImplementation(
      () =>
        new Promise((resolve) => {
          attached.resolve = resolve;
        }),
    );

    const stop = bridge.subscribeNativeEvent("astock://agent-event", () => undefined);
    // Unsubscribe first, then let the listener finish attaching. Without the
    // cancellation guard this would leak a listener for the window's lifetime.
    stop();
    attached.resolve?.(unlisten);
    await Promise.resolve();
    await Promise.resolve();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it("starts and cancels research through the shared runtime commands", async () => {
    enterTauriHost();
    invoke.mockResolvedValueOnce({ task_id: "task-1", session_id: "session-1" });
    await expect(bridge.startAgentResearch("分析紫金矿业", { depth: "deep" })).resolves.toMatchObject({
      task_id: "task-1",
    });
    expect(invoke).toHaveBeenCalledWith("agent_start", {
      prompt: "分析紫金矿业",
      sessionId: null,
      depth: "deep",
    });

    invoke.mockResolvedValueOnce({ cancelled: true });
    await expect(bridge.cancelAgentResearch("task-1")).resolves.toMatchObject({ cancelled: true });
    expect(invoke).toHaveBeenLastCalledWith("agent_cancel", { taskId: "task-1" });
  });

  it("refuses to start research when no native host is present", async () => {
    await expect(bridge.startAgentResearch("分析紫金矿业")).rejects.toThrow("桌面宿主尚未连接");
    expect(invoke).not.toHaveBeenCalled();
  });
});
