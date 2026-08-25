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

interface ProtonRoot {
  core?: {
    invokeOp?: (operation: string, payload: unknown, options?: unknown) => Promise<unknown>;
  };
  events?: {
    on?: (name: string, handler: (event: { payload: unknown }) => void) => () => void;
  };
}

declare global {
  interface Window {
    __MoonBit__?: ProtonRoot;
  }
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

export function isProton(): boolean {
  return typeof window !== "undefined" && (
    typeof window.__MoonBit__?.core?.invokeOp === "function" || isBrowserTestBridge()
  );
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
  const invokeOp = window.__MoonBit__?.core?.invokeOp;
  if (!invokeOp && !isBrowserTestBridge()) throw new Error("Proton Host 尚未连接；浏览器预览仅提供界面演示。");

  const request: RequestEnvelope = {
    protocol_version: PROTOCOL_VERSION,
    request_id: createRequestId(),
    kind,
    payload,
    deadline_ms: options.deadlineMs ?? 30_000,
    ...(options.cancellationId ? { cancellation_id: options.cancellationId } : {}),
  };
  const testConfig = invokeOp ? null : await browserTestSession();
  const raw = invokeOp
    ? await invokeOp("app:request", { target, request })
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
    throw new Error("Host 返回了无法关联的响应。");
  }
  if (!response.ok) {
    throw new Error(response.error?.message ?? response.error?.code ?? "本地服务请求失败");
  }
  return response.payload as T;
}

export function subscribeNativeEvent(
  name: string,
  handler: (payload: unknown) => void,
): () => void {
  return window.__MoonBit__?.events?.on?.(name, (event) => handler(event.payload)) ?? (() => undefined);
}
