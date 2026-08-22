/** 数字格式化工具:亿/万换算、百分比、固定位数小数 */

/** 面向普通用户的统一缺失值文案，避免相邻表格单元格显示成“------”。 */
export const EMPTY_DISPLAY = "暂无";

/** 清洗上游文本占位符；连续横线、空串和常见空值都按缺失处理。 */
export function fmtText(v: unknown, fallback = EMPTY_DISPLAY): string {
  if (v == null) return fallback;
  const text = String(v).trim();
  if (!text || /^[-—–]+$/.test(text) || /^(?:n\/?a|null|undefined)$/i.test(text)) return fallback;
  return text;
}

/**
 * IPC and third-party market-data payloads are runtime data: an upstream may
 * encode a decimal as a JSON string even when our TypeScript contract says
 * `number`. Keep that boundary tolerant, while rejecting blanks, booleans,
 * infinities and arbitrary text instead of accidentally displaying them as 0.
 */
export function finiteNumber(v: unknown): number | null {
  if (typeof v === "number") return Number.isFinite(v) ? v : null;
  if (typeof v !== "string" || v.trim() === "") return null;
  const n = Number(v);
  return Number.isFinite(n) ? n : null;
}

export function fmtNum(v: unknown, digits = 2): string {
  const n = finiteNumber(v);
  if (n == null) return EMPTY_DISPLAY;
  return n.toFixed(digits);
}

/** 金额/成交量:自动换算 亿 / 万 */
export function fmtYiWan(v: unknown, digits = 2): string {
  const n = finiteNumber(v);
  if (n == null) return EMPTY_DISPLAY;
  const abs = Math.abs(n);
  if (abs >= 1e8) return (n / 1e8).toFixed(digits) + "亿";
  if (abs >= 1e4) return (n / 1e4).toFixed(digits) + "万";
  return n.toFixed(digits);
}

/** 成交量(行情契约统一为手),自动换算 亿手 / 万手 / 手 */
export function fmtVolume(v: unknown): string {
  const hands = finiteNumber(v);
  if (hands == null) return EMPTY_DISPLAY;
  const abs = Math.abs(hands);
  if (abs >= 1e8) return (hands / 1e8).toFixed(2) + "亿手";
  if (abs >= 1e4) return (hands / 1e4).toFixed(2) + "万手";
  return hands.toFixed(0) + "手";
}

/** 字节 → 可读大小 */
export function fmtBytes(v: unknown): string {
  const n = finiteNumber(v);
  if (n == null) return EMPTY_DISPLAY;
  if (n >= 1e9) return (n / 1e9).toFixed(2) + " GB";
  if (n >= 1e6) return (n / 1e6).toFixed(2) + " MB";
  if (n >= 1e3) return (n / 1e3).toFixed(2) + " KB";
  return n + " B";
}

/** 百分比:+3.25% 格式(带符号) */
export function fmtPct(v: unknown, digits = 2, signed = true): string {
  const n = finiteNumber(v);
  if (n == null) return EMPTY_DISPLAY;
  const s = n.toFixed(digits) + "%";
  return signed && n > 0 ? "+" + s : s;
}

/** 涨跌幅对应的 Tailwind 文字颜色类(A股惯例:红涨绿跌) */
export function pctClass(v: unknown): string {
  const n = finiteNumber(v);
  if (n == null || n === 0) return "muted";
  return n > 0 ? "text-up" : "text-down";
}

/** 涨跌幅对应的十六进制色(用于 ECharts) */
export const COLOR_UP = "#e5484d";
export const COLOR_DOWN = "#2eb872";

export function pctColor(v: unknown): string {
  const n = finiteNumber(v);
  if (n == null || n === 0) return "#94a3b8";
  return n > 0 ? COLOR_UP : COLOR_DOWN;
}
