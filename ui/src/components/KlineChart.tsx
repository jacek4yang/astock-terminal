import { useMemo } from "react";
import type { EChartsOption } from "echarts";
import Chart from "./Chart";
import type { Bar, ChanlunDailyJson } from "../lib/api";
import { calcMA, calcMACD, calcRSI, calcKDJ, calcBOLL } from "../lib/indicators";
import { COLOR_UP, COLOR_DOWN, fmtVolume } from "../lib/format";

export type SubIndicator = "MACD" | "RSI" | "KDJ" | "BOLL";

interface Props {
  bars: Bar[];
  indicator: SubIndicator;
  /** dataZoom 起始百分比(0=全部,越大范围越近) */
  zoomStart?: number;
  /** 缠论叠加数据(可选) */
  chanlun?: ChanlunDailyJson | null;
  height?: number | string;
}

const UP = COLOR_UP;
const DOWN = COLOR_DOWN;
const MA_COLORS: Record<number, string> = { 5: "#f5a623", 10: "#4a90d9", 20: "#b36ae2", 60: "#8b8b8b" };

function baseAxisPointer() {
  return { link: [{ xAxisIndex: "all" as const }] };
}

export default function KlineChart({ bars, indicator, zoomStart = 0, chanlun, height = 620 }: Props) {
  const option = useMemo<EChartsOption>(() => {
    const dates = bars.map((b) => b.date);
    const ohlc = bars.map((b) => [b.open, b.close, b.low, b.high]);
    const closes = bars.map((b) => b.close);
    const highs = bars.map((b) => b.high);
    const lows = bars.map((b) => b.low);
    const vols = bars.map((b) => ({
      value: b.volume,
      itemStyle: { color: b.close >= b.open ? UP : DOWN },
    }));

    const ma5 = calcMA(bars, 5);
    const ma10 = calcMA(bars, 10);
    const ma20 = calcMA(bars, 20);
    const ma60 = calcMA(bars, 60);

    // 三栏布局:主图 / 成交量 / 副图指标(BOLL 叠加在主图)
    const hasSub = indicator !== "BOLL";
    const grids: Record<string, unknown>[] = [
      { left: 56, right: 16, top: 28, height: hasSub ? "46%" : "62%" },
      { left: 56, right: 16, top: hasSub ? "60%" : "76%", height: "12%" },
    ];
    if (hasSub) grids.push({ left: 56, right: 16, top: "78%", height: "16%" });

    const xAxes = grids.map((_, i) => ({
      type: "category" as const,
      data: dates,
      gridIndex: i,
      scale: true,
      boundaryGap: true,
      axisLine: { onZero: false },
      splitLine: { show: false },
      axisLabel: { show: i === grids.length - 1, fontSize: 10 },
      axisTick: { show: false },
      min: "dataMin" as const,
      max: "dataMax" as const,
    }));

    const yAxes = grids.map((_, i) => ({
      type: "value" as const,
      gridIndex: i,
      scale: true,
      splitNumber: i === 0 ? 4 : 2,
      axisLabel: { fontSize: 10, ...(i === 1 ? { formatter: (v: number) => fmtVolume(v) } : {}) },
      splitLine: { lineStyle: { opacity: 0.15 } },
    }));

    // 缠论叠加 payload(后端预生成,直接映射为 markPoint/markArea/markLine)
    const markPointData: Record<string, unknown>[] = [];
    const markAreaData: unknown[] = [];
    const markLineData: unknown[] = [];
    if (chanlun) {
      for (const f of chanlun.chart_fractals ?? []) {
        markPointData.push({
          coord: f.coord,
          symbol: f.symbol,
          symbolSize: f.symbolSize ?? 7,
          itemStyle: f.itemStyle,
        });
      }
      for (const s of chanlun.chart_signals ?? []) {
        markPointData.push({
          coord: s.coord,
          symbol: s.symbol ?? "pin",
          symbolRotate: s.symbolRotate,
          symbolSize: s.symbolSize ?? 14,
          itemStyle: s.itemStyle,
          label: s.label,
          name: s.type_name,
        });
      }
      for (const z of chanlun.chart_zhongshus ?? []) {
        markAreaData.push([
          { xAxis: z.xAxis[0], yAxis: z.yAxis[0], itemStyle: z.itemStyle },
          { xAxis: z.xAxis[1], yAxis: z.yAxis[1] },
        ]);
      }
      for (const st of chanlun.chart_strokes ?? []) {
        markLineData.push([
          { coord: st.coords[0], lineStyle: st.lineStyle },
          { coord: st.coords[1] },
        ]);
      }
    }

    const series: Record<string, unknown>[] = [
      {
        name: "K线",
        type: "candlestick",
        data: ohlc,
        xAxisIndex: 0,
        yAxisIndex: 0,
        itemStyle: { color: UP, color0: DOWN, borderColor: UP, borderColor0: DOWN },
        markPoint:
          markPointData.length > 0
            ? { data: markPointData, label: { fontSize: 10 } }
            : undefined,
        markArea:
          markAreaData.length > 0 ? { silent: true, data: markAreaData } : undefined,
        markLine:
          markLineData.length > 0
            ? { silent: true, symbol: "none", data: markLineData, animation: false }
            : undefined,
      },
      ...[5, 10, 20, 60].map((p) => {
        const data = p === 5 ? ma5 : p === 10 ? ma10 : p === 20 ? ma20 : ma60;
        return {
          name: `MA${p}`,
          type: "line",
          data,
          xAxisIndex: 0,
          yAxisIndex: 0,
          smooth: true,
          showSymbol: false,
          lineStyle: { width: 1, color: MA_COLORS[p] },
          emphasis: { disabled: true },
        };
      }),
      {
        name: "成交量",
        type: "bar",
        data: vols,
        xAxisIndex: 1,
        yAxisIndex: 1,
        barWidth: "60%",
      },
    ];

    if (indicator === "BOLL") {
      const boll = calcBOLL(closes);
      series.push(
        { name: "BOLL上", type: "line", data: boll.upper, xAxisIndex: 0, yAxisIndex: 0, showSymbol: false, lineStyle: { width: 1, color: "#4a90d9" } },
        { name: "BOLL中", type: "line", data: boll.mid, xAxisIndex: 0, yAxisIndex: 0, showSymbol: false, lineStyle: { width: 1, color: "#f5a623" } },
        { name: "BOLL下", type: "line", data: boll.lower, xAxisIndex: 0, yAxisIndex: 0, showSymbol: false, lineStyle: { width: 1, color: "#b36ae2" } },
      );
    } else if (indicator === "MACD") {
      const m = calcMACD(closes);
      series.push(
        {
          name: "MACD",
          type: "bar",
          data: m.macd.map((v) => ({
            value: v,
            itemStyle: { color: v != null && v >= 0 ? UP : DOWN },
          })),
          xAxisIndex: 2,
          yAxisIndex: 2,
          barWidth: "60%",
        },
        { name: "DIF", type: "line", data: m.dif, xAxisIndex: 2, yAxisIndex: 2, showSymbol: false, lineStyle: { width: 1, color: "#f5a623" } },
        { name: "DEA", type: "line", data: m.dea, xAxisIndex: 2, yAxisIndex: 2, showSymbol: false, lineStyle: { width: 1, color: "#4a90d9" } },
      );
    } else if (indicator === "RSI") {
      series.push(
        { name: "RSI6", type: "line", data: calcRSI(closes, 6), xAxisIndex: 2, yAxisIndex: 2, showSymbol: false, lineStyle: { width: 1, color: "#f5a623" } },
        { name: "RSI12", type: "line", data: calcRSI(closes, 12), xAxisIndex: 2, yAxisIndex: 2, showSymbol: false, lineStyle: { width: 1, color: "#4a90d9" } },
        { name: "RSI24", type: "line", data: calcRSI(closes, 24), xAxisIndex: 2, yAxisIndex: 2, showSymbol: false, lineStyle: { width: 1, color: "#b36ae2" } },
      );
    } else if (indicator === "KDJ") {
      const kdj = calcKDJ(highs, lows, closes);
      series.push(
        { name: "K", type: "line", data: kdj.k, xAxisIndex: 2, yAxisIndex: 2, showSymbol: false, lineStyle: { width: 1, color: "#f5a623" } },
        { name: "D", type: "line", data: kdj.d, xAxisIndex: 2, yAxisIndex: 2, showSymbol: false, lineStyle: { width: 1, color: "#4a90d9" } },
        { name: "J", type: "line", data: kdj.j, xAxisIndex: 2, yAxisIndex: 2, showSymbol: false, lineStyle: { width: 1, color: "#b36ae2" } },
      );
    }

    return {
      animation: false,
      backgroundColor: "transparent",
      axisPointer: baseAxisPointer(),
      legend: {
        top: 2,
        left: 8,
        itemWidth: 12,
        itemHeight: 8,
        textStyle: { fontSize: 10 },
        data: ["MA5", "MA10", "MA20", "MA60"],
      },
      tooltip: {
        trigger: "axis",
        axisPointer: { type: "cross" },
        backgroundColor: "rgba(30,41,59,0.92)",
        borderWidth: 0,
        textStyle: { fontSize: 11, color: "#e2e8f0" },
        formatter: (params: unknown) => {
          const arr = params as { seriesName: string; data: unknown; axisValue: string }[];
          if (!arr.length) return "";
          const k = arr.find((p) => p.seriesName === "K线");
          const idx = dates.indexOf(arr[0].axisValue);
          const bar = idx >= 0 ? bars[idx] : null;
          if (!bar) return arr[0].axisValue;
          const lines = [
            `<b>${bar.date}</b>`,
            `开 ${bar.open.toFixed(2)}　收 ${bar.close.toFixed(2)}`,
            `高 ${bar.high.toFixed(2)}　低 ${bar.low.toFixed(2)}`,
            `涨跌 ${bar.pct >= 0 ? "+" : ""}${bar.pct.toFixed(2)}%　换手 ${bar.turnover.toFixed(2)}%`,
            `成交量 ${fmtVolume(bar.volume)}`,
          ];
          for (const p of arr) {
            if (p.seriesName === "K线" || p.seriesName === "成交量") continue;
            const v = Array.isArray(p.data) ? p.data[1] : (p.data as { value?: number })?.value ?? p.data;
            if (typeof v === "number") lines.push(`${p.seriesName} ${v.toFixed(3)}`);
          }
          void k;
          return lines.join("<br/>");
        },
      },
      grid: grids,
      xAxis: xAxes,
      yAxis: yAxes,
      dataZoom: [
        { type: "inside", xAxisIndex: grids.map((_, i) => i), start: zoomStart, end: 100 },
        {
          type: "slider",
          xAxisIndex: grids.map((_, i) => i),
          start: zoomStart,
          end: 100,
          bottom: 2,
          height: 16,
          borderColor: "transparent",
          textStyle: { fontSize: 9 },
        },
      ],
      series,
    } as EChartsOption;
  }, [bars, indicator, zoomStart, chanlun]);

  return <Chart option={option} height={height} />;
}
