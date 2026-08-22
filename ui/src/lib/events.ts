/**
 * Tauri 事件订阅封装(扫描进度等长任务事件流)。
 * 契约:scan-progress {done,total,current_symbol};scan-result 单条结果。
 */
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { ScanResultItem } from "./api";
import { isTauri } from "./api";

export interface ScanProgress {
  done: number;
  total: number;
  current_symbol: string;
}

type Handler<T> = (payload: T) => void;

function subscribe<T>(event: string, handler: Handler<T>): Promise<UnlistenFn> {
  if (!isTauri()) return Promise.resolve(() => {});
  return listen<T>(event, (e) => handler(e.payload));
}

export const onScanProgress = (handler: Handler<ScanProgress>) =>
  subscribe<ScanProgress>("scan-progress", handler);

export const onScanResult = (handler: Handler<ScanResultItem>) =>
  subscribe<ScanResultItem>("scan-result", handler);

// ==================== AI Agent ====================

export interface AgentEvidence {
  tool: string;
  cache_key: string;
  source: string;
  fetched_at: string;
}

export interface AgentReport {
  task_id: string;
  answer: string;
  conclusions: unknown;
  evidence: AgentEvidence[];
  generated_at: string;
}

export type AgentEvent =
  | { type: "text_delta"; text: string }
  | { type: "tool_call_started"; name: string; args: unknown }
  | { type: "tool_call_finished"; name: string; cache_key: string; elapsed_ms: number }
  | { type: "suspended"; reset_at_unix: number }
  | { type: "completed"; report: AgentReport }
  | { type: "failed"; error: string };

interface AgentEventPayload {
  task_id: string;
  event: AgentEvent;
}

/** 订阅 agent-event 并按 task_id 过滤,返回退订函数 */
export function onAgentEvent(
  taskId: string,
  handler: (event: AgentEvent) => void,
): Promise<UnlistenFn> {
  if (!isTauri()) return Promise.resolve(() => {});
  return listen<AgentEventPayload>("agent-event", (e) => {
    if (e.payload && e.payload.task_id === taskId) handler(e.payload.event);
  });
}
