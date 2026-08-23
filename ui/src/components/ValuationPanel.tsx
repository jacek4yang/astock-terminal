import { useEffect, useMemo, useState } from "react";
import type { EChartsOption } from "echarts";
import { getValuation, errMsg, type ValuationJson } from "../lib/api";
import { COLOR_UP, COLOR_DOWN, finiteNumber, fmtNum, fmtPct, fmtYiWan, pctClass } from "../lib/format";
import { Loading, ErrorBox, EmptyBox, Term, Stat } from "./ui";
import Chart from "./Chart";

interface Props {
  symbol: string;
}

/** 历史分位渐变进度条:左绿(低位)→右红(高位),白线标记当前位置 */
function PercentileBar({ label, tip, value }: { label: string; tip: string; value: unknown }) {
  const parsed = finiteNumber(value);
  const v = parsed == null ? null : Math.min(100, Math.max(0, parsed));
  const tag = v == null ? null : v <= 20 ? "历史低位" : v >= 80 ? "历史高位" : null;
  const tagCls = v == null ? "" : v <= 20 ? "text-down" : v >= 80 ? "text-up" : "";
  return (
    <div className="min-w-[180px] flex-1">
      <div className="flex items-center justify-between text-xs">
        <Term label={label} tip={tip} />
        <span>
          <span className="num">{v == null ? "—" : v.toFixed(1) + "%"}</span>
          {tag && <span className={"ml-1 font-medium " + tagCls}>{tag}</span>}
        </span>
      </div>
      <div className="relative mt-1 h-2 rounded bg-gradient-to-r from-down via-slate-500 to-up">
        {v != null && (
          <div
            className="absolute top-1/2 h-3.5 w-0.5 -translate-y-1/2 rounded bg-white shadow ring-1 ring-slate-900/60"
            style={{ left: `calc(${v}% - 1px)` }}
          />
        )}
      </div>
      <div className="muted mt-0.5 flex justify-between text-[10px]">
        <span>0%</span>
        <span>100%</span>
      </div>
    </div>
  );
}

/** wacc/growth 轴标签:小数按百分比显示(0.09 → 9.0%) */
function fmtAxis(v: unknown): string {
  const n = finiteNumber(v);
  if (n == null) return "—";
  return (Math.abs(n) <= 1 ? (n * 100).toFixed(1) : n.toFixed(1)) + "%";
}

/** 估值面板:当前倍数 / 历史分位 / 估值带 / DCF 三情景 + 敏感性热力表 */
export default function ValuationPanel({ symbol }: Props) {
  const [data, setData] = useState<ValuationJson | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    setData(null);
    setErr(null);
    getValuation(symbol)
      .then((d) => alive && setData(d))
      .catch((e) => alive && setErr(errMsg(e)));
    return () => {
      alive = false;
    };
  }, [symbol]);

  const bandOption = useMemo<EChartsOption | null>(() => {
    const hs = data?.history_series ?? [];
    if (hs.length === 0) return null;
    const cur = finiteNumber(data?.current?.pe_ttm);
    return {
      animation: false,
      backgroundColor: "transparent",
      tooltip: {
        trigger: "axis",
        backgroundColor: "rgba(30,41,59,0.92)",
        borderWidth: 0,
        textStyle: { fontSize: 11, color: "#e2e8f0" },
        valueFormatter: (v: unknown) => (typeof v === "number" ? v.toFixed(2) : "—"),
      },
      grid: { left: 48, right: 16, top: 12, bottom: 20 },
      xAxis: {
        type: "category",
        data: hs.map((p) => p.date),
        axisLabel: { fontSize: 9 },
        axisTick: { show: false },
      },
      yAxis: {
        type: "value",
        scale: true,
        axisLabel: { fontSize: 10 },
        splitLine: { lineStyle: { opacity: 0.15 } },
      },
      series: [
        {
          name: "PE(TTM)",
          type: "line",
          data: hs.map((p) => p.pe_ttm),
          showSymbol: false,
          lineStyle: { width: 1.5, color: "#4a90d9" },
          markLine:
            cur != null
              ? {
                  silent: true,
                  symbol: "none",
                  lineStyle: { type: "dashed", color: "#f5a623" },
                  label: { formatter: `当前 ${cur.toFixed(2)}`, fontSize: 10, position: "insideEndTop" },
                  data: [{ yAxis: cur }],
                }
              : undefined,
        },
      ],
    } as EChartsOption;
  }, [data]);

  const heatOption = useMemo<EChartsOption | null>(() => {
    const s = data?.dcf?.sensitivity;
    if (!s?.wacc?.length || !s.growth?.length || !s.values?.length) return null;
    const pts: [number, number, number][] = [];
    let min = Infinity;
    let max = -Infinity;
    s.values.forEach((row, i) => {
      row?.forEach((value, j) => {
        const v = finiteNumber(value);
        if (v == null) return;
        pts.push([j, i, v]);
        if (v < min) min = v;
        if (v > max) max = v;
      });
    });
    if (pts.length === 0) return null;
    return {
      animation: false,
      backgroundColor: "transparent",
      tooltip: {
        backgroundColor: "rgba(30,41,59,0.92)",
        borderWidth: 0,
        textStyle: { fontSize: 11, color: "#e2e8f0" },
        formatter: (p: unknown) => {
          const d = (p as { data: [number, number, number] }).data;
          return `WACC ${fmtAxis(s.wacc![d[1]])} · 永续增长 ${fmtAxis(s.growth![d[0]])}<br/>目标价 <b>${fmtNum(d[2])}</b>`;
        },
      },
      grid: { left: 64, right: 16, top: 28, bottom: 52 },
      xAxis: {
        type: "category",
        name: "永续增长",
        nameTextStyle: { fontSize: 10 },
        data: s.growth.map(fmtAxis),
        axisLabel: { fontSize: 9 },
        axisTick: { show: false },
        splitArea: { show: true },
      },
      yAxis: {
        type: "category",
        name: "WACC",
        nameTextStyle: { fontSize: 10 },
        data: s.wacc.map(fmtAxis),
        axisLabel: { fontSize: 9 },
        axisTick: { show: false },
        splitArea: { show: true },
      },
      visualMap: {
        min,
        max,
        calculable: false,
        orient: "horizontal",
        left: "center",
        bottom: 0,
        itemWidth: 10,
        textStyle: { fontSize: 10 },
        formatter: (v: unknown) => fmtNum(v, 1),
        inRange: { color: [COLOR_DOWN, "#475569", COLOR_UP] },
      },
      series: [
        {
          type: "heatmap",
          data: pts,
          label: {
            show: true,
            fontSize: 9,
            formatter: (p: unknown) => fmtNum((p as { data: [number, number, number] }).data[2]),
          },
          emphasis: { itemStyle: { shadowBlur: 6, shadowColor: "rgba(0,0,0,0.5)" } },
        },
      ],
    } as EChartsOption;
  }, [data]);

  if (err) {
    return (
      <div className="card">
        <div className="card-title">估值</div>
        <ErrorBox message={err} />
      </div>
    );
  }
  if (!data) {
    return (
      <div className="card">
        <div className="card-title">估值</div>
        <Loading text="加载估值数据…" />
      </div>
    );
  }

  const c = data.current;
  const pct = data.percentile;
  const dcf = data.dcf;
  const price = finiteNumber(c?.price);
  const scenarios: { label: string; tip: string; value: number | null }[] = [
    { label: "悲观", tip: "增长下调、折现率上调情形下的每股价值", value: finiteNumber(dcf?.bear) },
    { label: "中性", tip: "基准假设下的每股价值", value: finiteNumber(dcf?.base) },
    { label: "乐观", tip: "增长上调、折现率下调情形下的每股价值", value: finiteNumber(dcf?.bull) },
  ];

  return (
    <>
      {/* 当前估值 */}
      <div className="card">
        <div className="card-title">当前估值</div>
        {!c ? (
          <EmptyBox text="估值数据缺失" />
        ) : (
          <div className="flex flex-wrap items-center gap-x-6 gap-y-2 px-4 py-3">
            <Stat
              label={<Term label="PE(TTM)" tip="市值÷最近12个月净利润,最常用的估值倍数,越低越便宜(亏损股无意义)" />}
              value={fmtNum(c.pe_ttm)}
            />
            <Stat
              label={<Term label="PE(静)" tip="市值÷上一个完整会计年度净利润" />}
              value={fmtNum(c.pe_static)}
            />
            <Stat
              label={<Term label="PB" tip="市值÷净资产,重资产/周期行业更看重,破净(<1)说明市值低于账面净资产" />}
              value={fmtNum(c.pb)}
            />
            <Stat
              label={<Term label="PS(TTM)" tip="市值÷最近12个月营收,适用于尚未盈利的成长股" />}
              value={fmtNum(c.ps_ttm)}
            />
            <Stat
              label={<Term label="PCF" tip="市值÷经营现金流,比利润更难被粉饰的估值口径" />}
              value={fmtNum(c.pcf)}
            />
            <Stat label="总市值" value={fmtYiWan(c.market_cap)} />
          </div>
        )}
      </div>

      {/* 历史分位 */}
      <div className="card">
        <div className="card-title">
          <Term
            label="历史分位"
            tip="当前估值在自身历史中的位置:0%=历史最便宜,100%=历史最贵。低分位≠马上涨,只说明相对自身历史便宜"
          />
          {pct?.days != null && (
            <span className="muted text-xs font-normal">近 {pct.days} 个交易日</span>
          )}
        </div>
        {!pct ? (
          <EmptyBox text="历史分位数据缺失" />
        ) : (
          <div className="flex flex-wrap gap-x-8 gap-y-3 px-4 py-3">
            <PercentileBar label="PE(TTM)分位" tip="当前PE(TTM)在历史PE序列中的分位" value={pct.pe_ttm_pct} />
            <PercentileBar label="PB分位" tip="当前PB在历史PB序列中的分位" value={pct.pb_pct} />
            <PercentileBar label="PS分位" tip="当前PS在历史PS序列中的分位" value={pct.ps_pct} />
          </div>
        )}
      </div>

      {/* 估值带 */}
      <div className="card">
        <div className="card-title">
          <Term label="估值带" tip="历史PE(TTM)走势,虚线为当前值,看当前处于历史通道的什么位置" />
        </div>
        {bandOption ? (
          <Chart option={bandOption} height={240} />
        ) : (
          <EmptyBox text="暂无估值历史数据" />
        )}
      </div>

      {/* DCF */}
      <div className="card">
        <div className="card-title">
          <Term
            label="DCF 估值"
            tip="现金流折现:把公司未来能赚的自由现金流折算成今天的钱。结果是区间而非精确点位,对假设极其敏感,仅供锚定参考"
          />
          {data.parameter_snapshot_id && (
            <span className="muted ml-auto text-[10px] font-normal" title="应与盈利驱动树的参数口径编号一致">
              共享参数口径 {data.parameter_snapshot_id.slice(0, 14)}…
            </span>
          )}
        </div>
        {!dcf ? (
          <EmptyBox text="DCF 数据缺失" />
        ) : (
          <div className="p-3">
            <div className="grid grid-cols-3 gap-2">
              {scenarios.map((s) => {
                const upside =
                  s.value != null && price != null && price > 0
                    ? (s.value / price - 1) * 100
                    : null;
                return (
                  <div
                    key={s.label}
                    className="rounded border border-slate-200 px-2 py-2 text-center dark:border-slate-800"
                    title={s.tip}
                  >
                    <div className="muted cursor-help text-xs underline decoration-dotted underline-offset-2">
                      {s.label}
                    </div>
                    <div className="num mt-1 text-lg font-semibold">{fmtNum(s.value)}</div>
                    <div className={"num mt-0.5 text-xs " + pctClass(upside)}>
                      {upside == null ? "—" : fmtPct(upside) + (upside >= 0 ? " 上行" : " 下行")}
                    </div>
                  </div>
                );
              })}
            </div>
            {price != null && (
              <div className="muted mt-2 text-xs">
                现价 <span className="num">{fmtNum(price)}</span>,百分比为目标价相对现价的空间
              </div>
            )}
            {heatOption && (
              <div className="mt-3">
                <div className="muted px-1 text-xs">
                  <Term
                    label="敏感性分析(WACC × 永续增长)"
                    tip="目标价对折现率和永续增长假设的敏感程度,格子越红估值越高、越绿越低"
                  />
                </div>
                <Chart option={heatOption} height={260} />
              </div>
            )}
            {dcf.caveat && <div className="muted mt-2 text-xs">{dcf.caveat}</div>}
          </div>
        )}
      </div>
    </>
  );
}
