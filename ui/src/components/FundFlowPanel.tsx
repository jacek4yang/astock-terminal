import { useEffect, useMemo, useState } from "react";
import type { EChartsOption } from "echarts";
import {
  getFundFlow,
  getRealtimeFlow,
  errMsg,
  type FundFlow,
  type RealtimeFlow,
} from "../lib/api";
import { COLOR_UP, COLOR_DOWN, fmtYiWan } from "../lib/format";
import { Loading, ErrorBox, Term } from "./ui";
import Chart from "./Chart";

interface Props {
  symbol: string;
  /** 外部传入的近30日资金流(get_stock_bundle);不传则组件自行拉取 */
  dailyFlows?: FundFlow[] | null;
  /** 近30日资金流已降级(bundle missing 含 fund_flow_30d) */
  dailyDegraded?: boolean;
}

/** 资金流面板:近30日主力净流入柱状 + 今日累计曲线 */
export default function FundFlowPanel({ symbol, dailyFlows, dailyDegraded }: Props) {
  const external = dailyFlows !== undefined;
  const [ownFlows, setOwnFlows] = useState<FundFlow[] | null>(null);
  const [rt, setRt] = useState<RealtimeFlow | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [rtErr, setRtErr] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    setOwnFlows(null);
    setRt(null);
    setErr(null);
    setRtErr(null);
    if (!external) {
      getFundFlow(symbol, 30)
        .then((f) => alive && setOwnFlows(f))
        .catch((e) => alive && setErr(errMsg(e)));
    }
    getRealtimeFlow(symbol)
      .then((r) => alive && setRt(r))
      .catch((e) => alive && setRtErr(errMsg(e)));
    return () => {
      alive = false;
    };
  }, [symbol, external]);

  const flows = external ? dailyFlows : ownFlows;

  const histOption = useMemo<EChartsOption | null>(() => {
    if (!flows) return null;
    return {
      animation: false,
      backgroundColor: "transparent",
      tooltip: {
        trigger: "axis",
        backgroundColor: "rgba(30,41,59,0.92)",
        borderWidth: 0,
        textStyle: { fontSize: 11, color: "#e2e8f0" },
        formatter: (params: unknown) => {
          const arr = params as { axisValue: string; dataIndex: number }[];
          const f = flows[arr[0].dataIndex];
          if (!f) return "";
          return [
            `<b>${f.date}</b>`,
            `主力净流入 ${fmtYiWan(f.main_net)}(${f.main_pct.toFixed(2)}%)`,
            `超大单 ${fmtYiWan(f.super_large_net)}`,
            `大单 ${fmtYiWan(f.large_net)}`,
            `中单 ${fmtYiWan(f.medium_net)}`,
            `小单 ${fmtYiWan(f.small_net)}`,
          ].join("<br/>");
        },
      },
      grid: { left: 60, right: 12, top: 8, bottom: 20 },
      xAxis: {
        type: "category",
        data: flows.map((f) => f.date.slice(5)),
        axisLabel: { fontSize: 9 },
        axisTick: { show: false },
      },
      yAxis: {
        type: "value",
        scale: true,
        axisLabel: { fontSize: 10, formatter: (v: number) => fmtYiWan(v, 0) },
        splitLine: { lineStyle: { opacity: 0.15 } },
      },
      series: [
        {
          name: "主力净流入",
          type: "bar",
          data: flows.map((f) => ({
            value: f.main_net,
            itemStyle: { color: f.main_net >= 0 ? COLOR_UP : COLOR_DOWN },
          })),
          barWidth: "60%",
        },
      ],
    } as EChartsOption;
  }, [flows]);

  const rtOption = useMemo<EChartsOption | null>(() => {
    if (!rt || rt.points.length === 0) return null;
    return {
      animation: false,
      backgroundColor: "transparent",
      tooltip: {
        trigger: "axis",
        backgroundColor: "rgba(30,41,59,0.92)",
        borderWidth: 0,
        textStyle: { fontSize: 11, color: "#e2e8f0" },
        valueFormatter: (v: unknown) => fmtYiWan(v as number),
      },
      legend: { top: 0, left: 8, itemWidth: 12, itemHeight: 8, textStyle: { fontSize: 10 } },
      grid: { left: 60, right: 12, top: 26, bottom: 20 },
      xAxis: {
        type: "category",
        data: rt.points.map((p) => p.time),
        axisLabel: { fontSize: 9 },
        axisTick: { show: false },
      },
      yAxis: {
        type: "value",
        scale: true,
        axisLabel: { fontSize: 10, formatter: (v: number) => fmtYiWan(v, 0) },
        splitLine: { lineStyle: { opacity: 0.15 } },
      },
      series: [
        {
          name: "主力累计",
          type: "line",
          data: rt.points.map((p) => p.main_net),
          showSymbol: false,
          lineStyle: { width: 1.5, color: "#4a90d9" },
        },
        {
          name: "超大单累计",
          type: "line",
          data: rt.points.map((p) => p.super_large_net),
          showSymbol: false,
          lineStyle: { width: 1, color: "#f5a623" },
        },
      ],
    } as EChartsOption;
  }, [rt]);

  return (
    <div className="card">
      <div className="card-title">
        <Term
          label="资金流"
          tip="按单笔成交金额划分大单/小单,主力净流入=超大单+大单的买入-卖出,正值代表大资金在买"
        />
      </div>
      <div className="grid grid-cols-1 gap-2 p-2 lg:grid-cols-2">
        <div>
          <div className="muted px-1 text-xs">近30日主力净流入</div>
          {dailyDegraded && !histOption ? (
            <div className="mx-2 my-3 rounded border border-amber-300 bg-amber-50 px-3 py-2 text-xs text-amber-700 dark:border-amber-900 dark:bg-amber-950/40 dark:text-amber-300">
              30日资金流暂不可用(数据源降级,今日累计不受影响)
            </div>
          ) : err ? (
            <ErrorBox message={err} />
          ) : !histOption ? (
            <Loading />
          ) : (
            <Chart option={histOption} height={200} />
          )}
        </div>
        <div>
          <div className="muted px-1 text-xs">今日累计资金流</div>
          {rtErr ? (
            <ErrorBox message={rtErr} />
          ) : !rtOption ? (
            <Loading />
          ) : (
            <>
              {rt && (
                <div className="muted px-1 pb-1 text-xs">
                  主力累计{" "}
                  <span className={"num " + (rt.summary.main_net >= 0 ? "text-up" : "text-down")}>
                    {fmtYiWan(rt.summary.main_net)}
                  </span>
                </div>
              )}
              <Chart option={rtOption} height={180} />
            </>
          )}
        </div>
      </div>
    </div>
  );
}
