/**
 * 技术指标前端计算,算法与旧版 dashboard/index.html 保持一致:
 * - MA:简单移动平均
 * - MACD(12,26,9):DIF = EMA12 − EMA26,DEA = EMA9(DIF),BAR = 2×(DIF−DEA)
 * - RSI:Wilder 平滑
 * - KDJ(9,3,3)
 * - BOLL(20,2)
 */
import type { Bar } from "./api";

export type Series = (number | null)[];

export function calcMA(bars: Bar[], period: number): Series {
  const result: Series = new Array(bars.length).fill(null);
  for (let i = period - 1; i < bars.length; i++) {
    let sum = 0;
    for (let j = 0; j < period; j++) sum += bars[i - j].close;
    result[i] = sum / period;
  }
  return result;
}

function calcEMA(data: number[], period: number): Series {
  const result: Series = new Array(data.length).fill(null);
  if (data.length < period) return result;
  const k = 2 / (period + 1);
  let ema = data.slice(0, period).reduce((a, b) => a + b, 0) / period;
  result[period - 1] = ema;
  for (let i = period; i < data.length; i++) {
    ema = data[i] * k + ema * (1 - k);
    result[i] = ema;
  }
  return result;
}

export interface MacdResult {
  dif: Series;
  dea: Series;
  macd: Series;
}

export function calcMACD(closes: number[]): MacdResult {
  const fast = 12;
  const slow = 26;
  const signal = 9;
  const emaFast = calcEMA(closes, fast);
  const emaSlow = calcEMA(closes, slow);
  const dif: Series = closes.map((_, i) =>
    emaFast[i] == null || emaSlow[i] == null ? null : emaFast[i]! - emaSlow[i]!,
  );
  const difValid = dif.map((v) => v ?? 0);
  const dea = calcEMA(difValid, signal);
  const macd: Series = dif.map((d, i) =>
    d == null || dea[i] == null ? null : (d - dea[i]!) * 2,
  );
  return { dif, dea, macd };
}

export function calcRSI(closes: number[], period: number): Series {
  const result: Series = new Array(closes.length).fill(null);
  if (closes.length < period + 1) return result;
  let avgGain = 0;
  let avgLoss = 0;
  for (let i = 1; i <= period; i++) {
    const change = closes[i] - closes[i - 1];
    if (change >= 0) avgGain += change;
    else avgLoss -= change;
  }
  avgGain /= period;
  avgLoss /= period;
  result[period] = avgLoss === 0 ? 100 : 100 - 100 / (1 + avgGain / avgLoss);
  for (let i = period + 1; i < closes.length; i++) {
    const change = closes[i] - closes[i - 1];
    const gain = change >= 0 ? change : 0;
    const loss = change < 0 ? -change : 0;
    avgGain = (avgGain * (period - 1) + gain) / period;
    avgLoss = (avgLoss * (period - 1) + loss) / period;
    result[i] = avgLoss === 0 ? 100 : 100 - 100 / (1 + avgGain / avgLoss);
  }
  return result;
}

export interface KdjResult {
  k: Series;
  d: Series;
  j: Series;
}

export function calcKDJ(highs: number[], lows: number[], closes: number[], period = 9): KdjResult {
  const k: Series = new Array(closes.length).fill(null);
  const d: Series = new Array(closes.length).fill(null);
  const j: Series = new Array(closes.length).fill(null);
  let prevK = 50;
  let prevD = 50;
  for (let i = period - 1; i < closes.length; i++) {
    let hh = -Infinity;
    let ll = Infinity;
    for (let m = 0; m < period; m++) {
      hh = Math.max(hh, highs[i - m]);
      ll = Math.min(ll, lows[i - m]);
    }
    const rsv = hh === ll ? 50 : ((closes[i] - ll) / (hh - ll)) * 100;
    const curK = (2 / 3) * prevK + (1 / 3) * rsv;
    const curD = (2 / 3) * prevD + (1 / 3) * curK;
    k[i] = curK;
    d[i] = curD;
    j[i] = 3 * curK - 2 * curD;
    prevK = curK;
    prevD = curD;
  }
  return { k, d, j };
}

export interface BollResult {
  mid: Series;
  upper: Series;
  lower: Series;
}

export function calcBOLL(closes: number[], period = 20, mult = 2): BollResult {
  const mid: Series = new Array(closes.length).fill(null);
  const upper: Series = new Array(closes.length).fill(null);
  const lower: Series = new Array(closes.length).fill(null);
  for (let i = period - 1; i < closes.length; i++) {
    const slice = closes.slice(i - period + 1, i + 1);
    const ma = slice.reduce((a, b) => a + b, 0) / period;
    const variance = slice.reduce((a, b) => a + (b - ma) ** 2, 0) / period;
    const std = Math.sqrt(variance);
    mid[i] = ma;
    upper[i] = ma + mult * std;
    lower[i] = ma - mult * std;
  }
  return { mid, upper, lower };
}
