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
