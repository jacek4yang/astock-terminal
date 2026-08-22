import { useMemo } from "react";
import type { EChartsOption } from "echarts";
import Chart from "./Chart";
import type { MinuteData } from "../lib/api";
import { COLOR_UP, COLOR_DOWN } from "../lib/format";

interface Props {
  data: MinuteData;
  height?: number | string;
}

/** 分时视图:price / avg_price 双线 + 量柱 */
export default function MinuteChart({ data, height = 380 }: Props) {
  const option = useMemo<EChartsOption>(() => {
    const pts = data.points;
    const times = pts.map((p) => p.time);
    const prices = pts.map((p) => p.price);
    const avgs = pts.map((p) => p.avg_price);
    const pre = data.pre_close;

    // 以昨收为基准的涨跌幅坐标
    const pcts = prices.map((p) => (pre > 0 ? ((p - pre) / pre) * 100 : 0));
    const maxAbs = Math.max(0.2, ...pcts.map((v) => Math.abs(v)));
    const bound = Math.ceil(maxAbs * 10) / 10;

    const vols = pts.map((p, i) => ({
      value: p.volume,
      itemStyle: {
        color: i > 0 ? (p.price >= pts[i - 1].price ? COLOR_UP : COLOR_DOWN) : "#94a3b8",
      },
    }));

    return {
      animation: false,
      backgroundColor: "transparent",
      axisPointer: { link: [{ xAxisIndex: "all" }] },
      tooltip: {
        trigger: "axis",
        backgroundColor: "rgba(30,41,59,0.92)",
        borderWidth: 0,
        textStyle: { fontSize: 11, color: "#e2e8f0" },
      },
      legend: {
        top: 2,
        left: 8,
        itemWidth: 12,
        itemHeight: 8,
        textStyle: { fontSize: 10 },
        data: ["价格", "均价"],
      },
      grid: [
        { left: 52, right: 52, top: 28, height: "58%" },
        { left: 52, right: 52, top: "74%", height: "18%" },
      ],
      xAxis: [
        {
          type: "category",
          data: times,
          gridIndex: 0,
          axisLabel: { show: false },
          axisTick: { show: false },
        },
        {
          type: "category",
          data: times,
          gridIndex: 1,
          axisLabel: { fontSize: 10 },
          axisTick: { show: false },
        },
      ],
      yAxis: [
        {
          type: "value",
          gridIndex: 0,
          min: -bound,
          max: bound,
          axisLabel: { fontSize: 10, formatter: (v: number) => v.toFixed(2) + "%" },
          splitLine: { lineStyle: { opacity: 0.15 } },
        },
        {
          type: "value",
          gridIndex: 0,
          position: "right",
          min: pre * (1 - bound / 100),
          max: pre * (1 + bound / 100),
          axisLabel: { fontSize: 10, formatter: (v: number) => v.toFixed(2) },
          splitLine: { show: false },
        },
        {
          type: "value",
          gridIndex: 1,
          splitNumber: 2,
          axisLabel: { fontSize: 10 },
          splitLine: { lineStyle: { opacity: 0.15 } },
        },
      ],
      series: [
        {
          name: "价格",
          type: "line",
          data: pcts,
          xAxisIndex: 0,
          yAxisIndex: 0,
          showSymbol: false,
          lineStyle: { width: 1.2, color: "#4a90d9" },
          markLine: {
            silent: true,
            symbol: "none",
            data: [{ yAxis: 0, lineStyle: { color: "#94a3b8", type: "dashed" } }],
            label: { show: false },
          },
        },
        {
          name: "均价",
          type: "line",
          data: avgs.map((a) => (pre > 0 ? ((a - pre) / pre) * 100 : 0)),
          xAxisIndex: 0,
          yAxisIndex: 0,
          showSymbol: false,
          lineStyle: { width: 1, color: "#f5a623" },
        },
        {
          name: "成交量",
          type: "bar",
          data: vols,
          xAxisIndex: 1,
          yAxisIndex: 2,
          barWidth: "60%",
        },
      ],
    } as EChartsOption;
  }, [data]);

  return <Chart option={option} height={height} />;
}
