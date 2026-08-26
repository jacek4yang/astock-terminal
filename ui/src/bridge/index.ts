import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import {
  AGENT_RENDERER_REQUEST_KINDS,
  ENGINE_RENDERER_REQUEST_KINDS,
  HOST_RENDERER_REQUEST_KINDS,
  MAX_FRAME_BYTES,
  parseResponseEnvelope,
  type AgentRendererRequestKind,
  type EngineRendererRequestKind,
  type HostRendererRequestKind,
  type RequestEnvelope,
} from "./generated";

const PROTOCOL_VERSION = 1;

declare global {
  interface Window {
    /** Present only inside the Tauri host. Used for capability detection. */
    __TAURI_INTERNALS__?: unknown;
  }
}

/// True when running inside the Tauri desktop host.
///
/// Detection is by Tauri's own internals marker rather than a user-agent guess,
/// so a browser preview degrades to the presentation-only path instead of
/// failing on a missing native bridge.
function isTauriHost(): boolean {
  return typeof window !== "undefined" && window.__TAURI_INTERNALS__ !== undefined;
}

export type WorkerTarget = "engine" | "agent" | "host";

export interface NativeRequestOptions {
  deadlineMs?: number;
  cancellationId?: string;
}

interface BrowserTestConfig {
  port: number;
  bootstrapToken?: string;
  sessionToken?: string;
}

const BROWSER_TEST_HISTORY_KEY = "__astockBrowserBridge";
const BROWSER_TEST_SESSION_PREFIX = "astock.browser-bridge.session.";

function validBrowserBridgePort(value: unknown): value is number {
  return Number.isInteger(value) && Number(value) >= 1024 && Number(value) <= 65535;
}

function initializeBrowserTestConfig(): BrowserTestConfig | null {
  if (!import.meta.env.DEV || typeof window === "undefined") return null;
  if (window.location.hostname !== "127.0.0.1" && window.location.hostname !== "localhost") return null;
  const fragment = new URLSearchParams(window.location.hash.replace(/^#/, ""));
  if (fragment.get("nativeTest") === "1") {
    const port = Number(fragment.get("bridgePort"));
    const bootstrapToken = fragment.get("bridgeToken") ?? "";
    if (!validBrowserBridgePort(port) || bootstrapToken.length < 32) return null;
    const cleanUrl = new URL(window.location.href);
    cleanUrl.hash = "";
    const state = {
      ...(window.history.state && typeof window.history.state === "object" ? window.history.state : {}),
      [BROWSER_TEST_HISTORY_KEY]: { port },
    };
    window.history.replaceState(state, "", cleanUrl);
    return { port, bootstrapToken };
  }
  const historyConfig = window.history.state?.[BROWSER_TEST_HISTORY_KEY] as { port?: unknown } | undefined;
  const port = Number(historyConfig?.port);
  if (!validBrowserBridgePort(port)) return null;
  const sessionToken = window.sessionStorage.getItem(`${BROWSER_TEST_SESSION_PREFIX}${port}`) ?? "";
  return sessionToken.length >= 32 ? { port, sessionToken } : null;
}

const browserTestConfig = initializeBrowserTestConfig();
let browserTestSessionPromise: Promise<{ port: number; token: string }> | null = null;

async function browserTestSession(): Promise<{ port: number; token: string }> {
  if (!browserTestConfig) throw new Error("本地浏览器测试 Bridge 未初始化。");
  if (browserTestConfig.sessionToken) return { port: browserTestConfig.port, token: browserTestConfig.sessionToken };
  if (!browserTestSessionPromise) {
    const bootstrapToken = browserTestConfig.bootstrapToken ?? "";
    browserTestConfig.bootstrapToken = undefined;
    browserTestSessionPromise = fetch(`http://127.0.0.1:${browserTestConfig.port}/session`, {
      method: "POST",
      headers: { "X-AStock-Test-Token": bootstrapToken },
      cache: "no-store",
    }).then(parseBrowserBridgeResponse).then((value) => {
      const sessionToken = value && typeof value === "object" && "session_token" in value
        ? String((value as { session_token: unknown }).session_token)
        : "";
      if (sessionToken.length < 32) throw new Error("本地浏览器测试 Bridge 未返回有效会话。");
      browserTestConfig.sessionToken = sessionToken;
      window.sessionStorage.setItem(`${BROWSER_TEST_SESSION_PREFIX}${browserTestConfig.port}`, sessionToken);
      return { port: browserTestConfig.port, token: sessionToken };
    });
  }
  return browserTestSessionPromise;
}

const RENDERER_REQUEST_KIND_SETS: Record<WorkerTarget, ReadonlySet<string>> = {
  engine: new Set(ENGINE_RENDERER_REQUEST_KINDS),
  agent: new Set(AGENT_RENDERER_REQUEST_KINDS),
  host: new Set(HOST_RENDERER_REQUEST_KINDS),
};

export function isRendererRequestKind(target: WorkerTarget, kind: string): boolean {
  return RENDERER_REQUEST_KIND_SETS[target].has(kind);
}

export async function parseBrowserBridgeResponse(response: Response): Promise<unknown> {
  const declaredLength = Number(response.headers.get("content-length") ?? 0);
  if (Number.isFinite(declaredLength) && declaredLength > MAX_FRAME_BYTES) {
    throw new Error(`本地浏览器测试桥响应超过 ${MAX_FRAME_BYTES / 1024 / 1024} MiB 安全上限。`);
  }
  const text = await response.text();
  if (new TextEncoder().encode(text).byteLength > MAX_FRAME_BYTES) {
    throw new Error(`本地浏览器测试桥响应超过 ${MAX_FRAME_BYTES / 1024 / 1024} MiB 安全上限。`);
  }
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    throw new Error(`本地浏览器测试桥返回了非 JSON 响应（HTTP ${response.status}）；请确认 Bridge 仍在运行且端口未被代理或其他程序占用。`);
  }
  if (!response.ok) {
    const message = value && typeof value === "object" && "error" in value
      ? String((value as { error: unknown }).error).slice(0, 400)
      : `HTTP ${response.status}`;
    throw new Error(`本地浏览器测试桥失败：${message}`);
  }
  return value;
}

export function isBrowserTestBridge(): boolean {
  return browserTestConfig !== null;
}

/// True when a native host is reachable, whether the Tauri desktop or the
/// development browser bridge.
///
/// Retains the historical `isProton` name as a deprecated alias so existing call
/// sites keep compiling while they migrate; the meaning is now "native host
/// available", not "Proton".
export function isNativeHost(): boolean {
  return isTauriHost() || isBrowserTestBridge();
}

/** @deprecated Use {@link isNativeHost}. Kept so call sites migrate gradually. */
export function isProton(): boolean {
  return isNativeHost();
}

export function createRequestId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) return crypto.randomUUID();
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

export function requestNative<T = unknown>(
  target: "engine",
  kind: EngineRendererRequestKind,
  payload?: unknown,
  options?: NativeRequestOptions,
): Promise<T>;
export function requestNative<T = unknown>(
  target: "agent",
  kind: AgentRendererRequestKind,
  payload?: unknown,
  options?: NativeRequestOptions,
): Promise<T>;
export function requestNative<T = unknown>(
  target: "host",
  kind: HostRendererRequestKind,
  payload?: unknown,
  options?: NativeRequestOptions,
): Promise<T>;
export async function requestNative<T>(
  target: WorkerTarget,
  kind: string,
  payload: unknown = {},
  options: NativeRequestOptions = {},
): Promise<T> {
  if (!isRendererRequestKind(target, kind)) {
    throw new Error(`Renderer 请求未在 ${target} 协议白名单中声明：${kind}`);
  }
  const native = isTauriHost();
  if (!native && !isBrowserTestBridge()) {
    throw new Error("桌面宿主尚未连接；浏览器预览仅提供界面演示。");
  }

  const request: RequestEnvelope = {
    protocol_version: PROTOCOL_VERSION,
    request_id: createRequestId(),
    kind,
    payload,
    deadline_ms: options.deadlineMs ?? 30_000,
    ...(options.cancellationId ? { cancellation_id: options.cancellationId } : {}),
  };
  const testConfig = native ? null : await browserTestSession();
  const raw = native
    ? // One typed Tauri command carries every renderer request, so the wire
      // contract stays the protocol envelope the terminal adapter also uses
      // rather than a GUI-specific surface.
      await invoke("bridge_request", { target, envelope: request })
    : await fetch(`http://127.0.0.1:${testConfig?.port}/request`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "X-AStock-Test-Token": testConfig?.token ?? "",
        },
        body: JSON.stringify({ target, request }),
        cache: "no-store",
      }).then(parseBrowserBridgeResponse);
  const response = parseResponseEnvelope<T>(raw);
  if (!response || response.request_id !== request.request_id) {
    throw new Error("桌面宿主返回了无法关联的响应。");
  }
  if (!response.ok) {
    throw new Error(response.error?.message ?? response.error?.code ?? "本地服务请求失败");
  }
  return response.payload as T;
}

/// Subscribe to a native event.
///
/// Tauri's `listen` resolves asynchronously, so the returned unsubscribe
/// function detaches once the listener is attached. Callers keep a synchronous
/// cleanup signature, which is what React effects need.
export function subscribeNativeEvent(
  name: string,
  handler: (payload: unknown) => void,
): () => void {
  if (!isTauriHost()) return () => undefined;
  let detach: (() => void) | null = null;
  let cancelled = false;
  void listen(name, (event) => handler(event.payload)).then((unlisten) => {
    if (cancelled) {
      unlisten();
      return;
    }
    detach = unlisten;
  });
  return () => {
    cancelled = true;
    detach?.();
    detach = null;
  };
}

/// Start research through the shared Agent runtime.
///
/// The adapter forwards the prompt into the same canonical intent path the
/// terminal uses; the renderer performs no planning of its own.
export async function startAgentResearch(
  prompt: string,
  options: { sessionId?: string; depth?: string } = {},
): Promise<{ task_id: string; session_id: string }> {
  if (!isTauriHost()) throw new Error("桌面宿主尚未连接；无法启动研究任务。");
  return invoke("agent_start", {
    prompt,
    sessionId: options.sessionId ?? null,
    depth: options.depth ?? null,
  });
}

/// Cancel research cooperatively.
///
/// This reaches the same cancellation token the terminal's `/cancel` and
/// `先停一下` reach, so cancellation semantics are shared rather than duplicated.
export async function cancelAgentResearch(taskId: string): Promise<{ cancelled: boolean }> {
  if (!isTauriHost()) throw new Error("桌面宿主尚未连接；无法取消研究任务。");
  return invoke("agent_cancel", { taskId });
}
