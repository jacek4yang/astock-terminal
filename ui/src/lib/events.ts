/** Proton 事件订阅封装；扫描状态同时由 Engine 快照轮询收敛。 */
import { isProton, subscribeNativeEvent } from "../bridge";
import type { ScanResultItem } from "./api";

export interface ScanProgress {
  done: number;
  total: number;
  current_symbol: string;
}

type Handler<T> = (payload: T) => void;
type UnlistenFn = () => void;

function subscribe<T>(event: string, handler: Handler<T>): Promise<UnlistenFn> {
  if (!isProton()) return Promise.resolve(() => {});
  return Promise.resolve(subscribeNativeEvent(event, (payload) => handler(payload as T)));
}

export const onScanProgress = (handler: Handler<ScanProgress>) =>
  subscribe<ScanProgress>("scan-progress", handler);

export const onScanResult = (handler: Handler<ScanResultItem>) =>
  subscribe<ScanResultItem>("scan-result", handler);

// ==================== AI Agent ====================
