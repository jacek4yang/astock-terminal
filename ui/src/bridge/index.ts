import { parseResponseEnvelope, type RequestEnvelope } from "./generated";

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

function browserTestConfig(): { port: number; token: string } | null {
  if (!import.meta.env.DEV || typeof window === "undefined") return null;
  if (window.location.hostname !== "127.0.0.1" && window.location.hostname !== "localhost") return null;
  const search = new URLSearchParams(window.location.search);
  if (search.get("nativeTest") !== "1") return null;
  const port = Number(search.get("bridgePort"));
  const token = search.get("bridgeToken") ?? "";
  if (!Number.isInteger(port) || port < 1024 || port > 65535 || token.length < 32) return null;
  return { port, token };
}

export function isBrowserTestBridge(): boolean {
  return browserTestConfig() !== null;
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

export async function requestNative<T>(
  target: WorkerTarget,
  kind: string,
  payload: unknown = {},
  options: NativeRequestOptions = {},
): Promise<T> {
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
  const testConfig = browserTestConfig();
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
      }).then(async (response) => {
        const value = await response.json() as unknown;
        if (!response.ok) {
          const message = value && typeof value === "object" && "error" in value ? String(value.error) : `HTTP ${response.status}`;
          throw new Error(`本地浏览器测试桥失败：${message}`);
        }
        return value;
      });
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
