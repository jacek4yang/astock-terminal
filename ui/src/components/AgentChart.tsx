import { useMemo } from "react";
import type { EChartsOption, SeriesOption } from "echarts";
import Chart from "./Chart";

export interface AgentChartSeries {
  name: string;
  type: "line" | "bar";
  data: Array<number | null>;
}

export interface AgentChartSpec {
  title: string;
  unit?: string;
  x: string[];
  series: AgentChartSeries[];
}

export type AgentContentBlock =
  | { type: "text"; content: string }
  | { type: "chart"; raw: string; spec: AgentChartSpec | null };

function shortText(value: unknown, max: number): string | null {
  if (typeof value !== "string") return null;
  const text = value.trim();
  return text && text.length <= max ? text : null;
}

/** Parse the deliberately tiny chart DSL; arbitrary ECharts options are rejected. */
export function parseAgentChart(raw: string): AgentChartSpec | null {
  try {
    const value = JSON.parse(raw) as Record<string, unknown>;
    if (!value || typeof value !== "object" || Array.isArray(value)) return null;
    const title = shortText(value.title, 80);
    const unit = value.unit == null ? undefined : shortText(value.unit, 16) ?? undefined;
    if (!title || !Array.isArray(value.x) || !Array.isArray(value.series)) return null;
    if (value.x.length < 2 || value.x.length > 500 || value.series.length < 1 || value.series.length > 6) {
      return null;
    }
    const x = value.x.map((item) => shortText(item, 40));
    if (x.some((item) => item == null)) return null;
    const series: AgentChartSeries[] = [];
    for (const candidate of value.series) {
      if (!candidate || typeof candidate !== "object" || Array.isArray(candidate)) return null;
      const item = candidate as Record<string, unknown>;
      const name = shortText(item.name, 40);
      // A missing type is a harmless, common model omission. Default it to a
      // line; any unknown explicit type is still rejected.
      const type = item.type == null ? "line" : item.type === "bar" ? "bar" : item.type === "line" ? "line" : null;
      if (!name || !type || !Array.isArray(item.data) || item.data.length !== x.length) return null;
      const data = item.data.map((point) => {
        if (point == null) return null;
        return typeof point === "number" && Number.isFinite(point) ? point : Number.NaN;
      });
      if (data.some((point) => typeof point === "number" && !Number.isFinite(point))) return null;
      series.push({ name, type, data });
    }
    return { title, unit, x: x as string[], series };
  } catch {
    return null;
  }
}

/** Split only complete chart fences; an unfinished streaming fence remains ordinary Markdown. */
export function splitAgentContent(src: string): AgentContentBlock[] {
  const blocks: AgentContentBlock[] = [];
  const pattern = /```astock-chart\s*\n([\s\S]*?)```/g;
  let cursor = 0;
  for (let match = pattern.exec(src); match; match = pattern.exec(src)) {
    if (match.index > cursor) blocks.push({ type: "text", content: src.slice(cursor, match.index) });
    blocks.push({ type: "chart", raw: match[1].trim(), spec: parseAgentChart(match[1].trim()) });
    cursor = match.index + match[0].length;
  }
  if (cursor < src.length) blocks.push({ type: "text", content: src.slice(cursor) });
  return blocks.length > 0 ? blocks : [{ type: "text", content: src }];
}

export default function AgentChart({ spec }: { spec: AgentChartSpec }) {
  const option = useMemo<EChartsOption>(() => {
    const series: SeriesOption[] = spec.series.map((item) => ({
      name: item.name,
      type: item.type,
      data: item.data,
      showSymbol: item.type === "line" && spec.x.length <= 20,
      smooth: item.type === "line",
      lineStyle: item.type === "line" ? { width: 2 } : undefined,
      barMaxWidth: item.type === "bar" ? 28 : undefined,
      emphasis: { focus: "series" },
    } as SeriesOption));
    return {
      animationDuration: 350,
      backgroundColor: "transparent",
      color: ["#3b82f6", "#e5484d", "#2eb872", "#f59e0b", "#8b5cf6", "#06b6d4"],
      tooltip: { trigger: "axis", confine: true },
      legend: { top: 4, type: "scroll", textStyle: { fontSize: 11 } },
      grid: { left: 52, right: 20, top: 42, bottom: spec.x.length > 20 ? 62 : 38 },
      xAxis: {
        type: "category",
        data: spec.x,
        boundaryGap: spec.series.every((item) => item.type === "bar"),
        axisLabel: { fontSize: 10, hideOverlap: true },
      },
      yAxis: {
        type: "value",
        scale: true,
        name: spec.unit,
        nameTextStyle: { fontSize: 10 },
        axisLabel: { fontSize: 10 },
        splitLine: { lineStyle: { opacity: 0.14 } },
      },
      dataZoom: spec.x.length > 20
        ? [{ type: "inside", start: 40, end: 100 }, { type: "slider", height: 18, bottom: 6 }]
        : undefined,
      series,
    };
  }, [spec]);

  return (
    <figure className="my-3 overflow-hidden rounded-lg border border-slate-200 bg-slate-50/70 dark:border-slate-800 dark:bg-slate-900/60">
      <figcaption className="flex flex-wrap items-center justify-between gap-2 border-b border-slate-200 px-3 py-2 dark:border-slate-800">
        <span className="text-sm font-semibold">{spec.title}</span>
        <span className="muted text-[11px]">悬停查看数值{spec.x.length > 20 ? "，拖动底部滑块缩放区间" : ""}</span>
      </figcaption>
      <Chart option={option} height={300} />
    </figure>
  );
}
