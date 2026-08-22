/** 数字格式化工具:亿/万换算、百分比、固定位数小数 */

export function fmtNum(v: number | null | undefined, digits = 2): string {
  if (v == null || Number.isNaN(v)) return "--";
  return v.toFixed(digits);
}

/** 金额/成交量:自动换算 亿 / 万 */
export function fmtYiWan(v: number | null | undefined, digits = 2): string {
  if (v == null || Number.isNaN(v)) return "--";
  const abs = Math.abs(v);
  if (abs >= 1e8) return (v / 1e8).toFixed(digits) + "亿";
  if (abs >= 1e4) return (v / 1e4).toFixed(digits) + "万";
  return v.toFixed(digits);
}

/** 成交量(股→手),自动换算 亿手 / 万手 / 手 */
export function fmtVolume(v: number | null | undefined): string {
  if (v == null || Number.isNaN(v)) return "--";
  const hands = v / 100;
  const abs = Math.abs(hands);
  if (abs >= 1e8) return (hands / 1e8).toFixed(2) + "亿手";
  if (abs >= 1e4) return (hands / 1e4).toFixed(2) + "万手";
  return hands.toFixed(0) + "手";
}

/** 字节 → 可读大小 */
export function fmtBytes(v: number | null | undefined): string {
  if (v == null || Number.isNaN(v)) return "--";
  if (v >= 1e9) return (v / 1e9).toFixed(2) + " GB";
  if (v >= 1e6) return (v / 1e6).toFixed(2) + " MB";
  if (v >= 1e3) return (v / 1e3).toFixed(2) + " KB";
  return v + " B";
}

/** 百分比:+3.25% 格式(带符号) */
export function fmtPct(v: number | null | undefined, digits = 2, signed = true): string {
  if (v == null || Number.isNaN(v)) return "--";
  const s = v.toFixed(digits) + "%";
  return signed && v > 0 ? "+" + s : s;
}

/** 涨跌幅对应的 Tailwind 文字颜色类(A股惯例:红涨绿跌) */
export function pctClass(v: number | null | undefined): string {
  if (v == null || Number.isNaN(v) || v === 0) return "muted";
  return v > 0 ? "text-up" : "text-down";
}

/** 涨跌幅对应的十六进制色(用于 ECharts) */
export const COLOR_UP = "#e5484d";
export const COLOR_DOWN = "#2eb872";

export function pctColor(v: number | null | undefined): string {
  if (v == null || Number.isNaN(v) || v === 0) return "#94a3b8";
  return v > 0 ? COLOR_UP : COLOR_DOWN;
}
