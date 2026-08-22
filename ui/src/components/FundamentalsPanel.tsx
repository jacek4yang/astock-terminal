import { useEffect, useMemo, useState, type ReactNode } from "react";
import type { EChartsOption } from "echarts";
import {
  getFundamentals,
  errMsg,
  type FundamentalsJson,
  type GrowthPoint,
} from "../lib/api";
import { COLOR_UP, COLOR_DOWN, fmtNum, fmtPct, fmtYiWan, pctClass } from "../lib/format";
import { Loading, ErrorBox, EmptyBox, Term } from "./ui";
import Chart from "./Chart";

interface Props {
  symbol: string;
}

/** 成长序列最近两期的变化方向:1 升 / -1 降 / 0 平 / null 无法判断 */
function trendOf(series: GrowthPoint[], key: keyof GrowthPoint): number | null {
  const vals = series
    .map((p) => p[key])
    .filter((v): v is number => typeof v === "number" && Number.isFinite(v));
  if (vals.length < 2) return null;
  const d = vals[vals.length - 1] - vals[vals.length - 2];
  return d === 0 ? 0 : d > 0 ? 1 : -1;
}

/** 同比趋势小箭头(红升绿降,与行情配色一致) */
function TrendArrow({ dir }: { dir?: number | null }) {
  if (dir == null || dir === 0) return null;
  return (
    <span className={"text-xs " + (dir > 0 ? "text-up" : "text-down")}>
      {dir > 0 ? "▲" : "▼"}
    </span>
  );
}

/** 股本 → 亿股 */
function fmtShares(v: number | null | undefined): string {
  if (v == null || Number.isNaN(v)) return "—";
  return (v / 1e8).toFixed(2) + "亿股";
}

/** Altman  zone → 中文标签 + 色块类 */
function zoneInfo(zone: string | null | undefined): { label: string; cls: string } {
  const z = (zone ?? "").toLowerCase();
  if (z.includes("safe") || z.includes("安全"))
    return { label: "安全区", cls: "border-down/40 bg-down/10 text-down" };
  if (z.includes("distress") || z.includes("危险"))
    return { label: "危险区", cls: "border-up/40 bg-up/10 text-up" };
  if (!zone)
    return { label: "—", cls: "border-slate-300 text-slate-500 dark:border-slate-700 dark:text-slate-400" };
  return { label: "灰色区", cls: "border-amber-500/40 bg-amber-500/10 text-amber-500" };
}

/** 异常严重度 → 红/黄/灰标签类 */
function severityTag(severity: string): { label: string; cls: string } {
  const s = severity.toLowerCase();
  if (s === "high" || s.includes("高"))
    return { label: "高", cls: "bg-red-100 text-red-700 dark:bg-red-950/50 dark:text-red-300" };
  if (s === "warn" || s === "medium" || s.includes("中"))
    return { label: "中", cls: "bg-amber-100 text-amber-700 dark:bg-amber-950/50 dark:text-amber-300" };
  return { label: "低", cls: "bg-slate-200 text-slate-600 dark:bg-slate-800 dark:text-slate-400" };
}

interface MetricCardDef {
  label: string;
  tip: string;
  value: string;
  sub?: ReactNode;
  trend?: number | null;
}

/** 基本面面板:公司概况 / 核心指标 / 成长图表 / 评分 / 异常预警 / 分红 */
export default function FundamentalsPanel({ symbol }: Props) {
  const [data, setData] = useState<FundamentalsJson | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    setData(null);
    setErr(null);
    getFundamentals(symbol)
      .then((d) => alive && setData(d))
      .catch((e) => alive && setErr(errMsg(e)));
    return () => {
      alive = false;
    };
  }, [symbol]);

  const metricCards = useMemo<MetricCardDef[]>(() => {
    const m = data?.metrics;
    if (!m) return [];
    const g = data?.growth_series ?? [];
    return [
      {
        label: "营业总收入",
        tip: "公司卖产品/服务一共收到的钱,规模指标",
        value: fmtYiWan(m.revenue),
        trend: trendOf(g, "revenue"),
        sub:
          m.revenue_yoy != null ? (
            <span className={pctClass(m.revenue_yoy)}>同比 {fmtPct(m.revenue_yoy)}</span>
          ) : null,
      },
      {
        label: "净利润",
        tip: "扣除所有成本费用后真正赚到的钱",
        value: fmtYiWan(m.net_profit),
        trend: trendOf(g, "net_profit"),
        sub:
          m.profit_yoy != null ? (
            <span className={pctClass(m.profit_yoy)}>同比 {fmtPct(m.profit_yoy)}</span>
          ) : null,
      },
      {
        label: "毛利率",
        tip: "(营收-成本)÷营收,反映产品竞争力与定价权,越高越好",
        value: fmtPct(m.gross_margin, 2, false),
        trend: trendOf(g, "gross_margin"),
      },
      {
        label: "营业利润率",
        tip: "营业利润÷营收,主营业务的赚钱效率",
        value: fmtPct(m.operating_margin, 2, false),
      },
      {
        label: "净利率",
        tip: "净利润÷营收,每100元收入最终留下多少利润",
        value: fmtPct(m.net_margin, 2, false),
      },
      {
        label: "ROE",
        tip: "净资产收益率,巴菲特最看重的指标之一,长期>15%为优秀",
        value: fmtPct(m.roe, 2, false),
        trend: trendOf(g, "roe"),
      },
      {
        label: "ROA",
        tip: "总资产收益率,每元资产能赚多少利润",
        value: fmtPct(m.roa, 2, false),
      },
      {
        label: "ROIC",
        tip: "投入资本回报率,衡量生意本身的赚钱能力,长期高于资金成本(WACC)才创造价值",
        value: fmtPct(m.roic, 2, false),
      },
      {
        label: "自由现金流",
        tip: "经营现金流减去资本开支,公司真正可自由支配的钱,长期为正才健康",
        value: fmtYiWan(m.fcf),
      },
      {
        label: "收现比",
        tip: "经营现金流÷净利润,>1 说明利润有真金白银支撑,长期<1 要警惕纸面利润",
        value: fmtNum(m.cfo_to_net_income),
      },
      {
        label: "现金转换周期",
        tip: "从付钱买原料到收回货款的天数,越短资金效率越高,负数说明占用上下游资金",
        value: m.ccc == null ? "—" : fmtNum(m.ccc, 0) + "天",
      },
      {
        label: "流动比率",
        tip: "流动资产÷流动负债,衡量短期偿债能力,>2 较安全",
        value: fmtNum(m.current_ratio),
      },
      {
        label: "资产负债率",
        tip: "总负债÷总资产,过高说明杠杆风险大(金融地产行业天然偏高)",
        value: fmtPct(m.debt_ratio, 2, false),
      },
    ];
  }, [data]);

  const growthOption = useMemo<EChartsOption | null>(() => {
    const g = data?.growth_series ?? [];
    if (g.length === 0) return null;
    return {
      animation: false,
      backgroundColor: "transparent",
      tooltip: {
        trigger: "axis",
        backgroundColor: "rgba(30,41,59,0.92)",
        borderWidth: 0,
        textStyle: { fontSize: 11, color: "#e2e8f0" },
        formatter: (params: unknown) => {
          const arr = params as { dataIndex: number }[];
          const p = g[arr[0]?.dataIndex ?? 0];
          if (!p) return "";
          return [
            `<b>${p.period_end ?? "—"}</b>`,
            `营收 ${fmtYiWan(p.revenue)}(同比 ${fmtPct(p.revenue_yoy)})`,
            `净利 ${fmtYiWan(p.net_profit)}(同比 ${fmtPct(p.profit_yoy)})`,
            `毛利率 ${fmtPct(p.gross_margin, 2, false)} · ROE ${fmtPct(p.roe, 2, false)}`,
          ].join("<br/>");
        },
      },
      legend: { top: 0, left: 8, itemWidth: 12, itemHeight: 8, textStyle: { fontSize: 10 } },
      grid: { left: 60, right: 48, top: 26, bottom: 20 },
      xAxis: {
        type: "category",
        data: g.map((p) => p.period_end ?? "—"),
        axisLabel: { fontSize: 9 },
        axisTick: { show: false },
      },
      yAxis: [
        {
          type: "value",
          scale: true,
          axisLabel: { fontSize: 10, formatter: (v: number) => fmtYiWan(v, 0) },
          splitLine: { lineStyle: { opacity: 0.15 } },
        },
        {
          type: "value",
          scale: true,
          axisLabel: { fontSize: 10, formatter: (v: number) => v.toFixed(0) + "%" },
          splitLine: { show: false },
        },
      ],
      series: [
        {
          name: "营收",
          type: "bar",
          data: g.map((p) => p.revenue),
          barMaxWidth: 24,
          itemStyle: { color: "#4a90d9" },
        },
        {
          name: "净利润",
          type: "bar",
          data: g.map((p) => p.net_profit),
          barMaxWidth: 24,
          itemStyle: { color: "#f5a623" },
        },
        {
          name: "营收同比",
          type: "line",
          yAxisIndex: 1,
          data: g.map((p) => p.revenue_yoy),
          showSymbol: false,
          lineStyle: { width: 1.5, color: COLOR_UP },
        },
        {
          name: "净利同比",
          type: "line",
          yAxisIndex: 1,
          data: g.map((p) => p.profit_yoy),
          showSymbol: false,
          lineStyle: { width: 1.5, color: COLOR_DOWN },
        },
      ],
    } as EChartsOption;
  }, [data]);

  if (err) {
    return (
      <div className="card">
        <div className="card-title">基本面</div>
        <ErrorBox message={err} />
      </div>
    );
  }
  if (!data) {
    return (
      <div className="card">
        <div className="card-title">基本面</div>
        <Loading text="加载基本面数据(财务报表计算中,请稍候)…" />
      </div>
    );
  }

  const p = data.profile;
  const lp = data.latest_period;
  const scores = data.scores;
  const anomalies = data.anomalies ?? [];
  const dividends = data.dividends ?? [];
  const missing = data.missing ?? [];
  const zone = zoneInfo(scores?.altman?.zone);
  const mScore = scores?.beneish?.m_score;

  return (
    <>
      {/* 公司概况条 */}
      <div className="card px-4 py-3">
        <div className="flex flex-wrap items-center gap-x-6 gap-y-2">
          <div>
            <span className="text-base font-bold">{p?.name ?? "—"}</span>
            <span className="num muted ml-2 text-xs">{symbol}</span>
          </div>
          <div className="text-sm">
            <span className="muted text-xs">行业 </span>
            {p?.industry ?? "—"}
          </div>
          <div className="text-sm">
            <span className="muted text-xs">上市日期 </span>
            <span className="num">{p?.listing_date ?? "—"}</span>
          </div>
          <div className="text-sm">
            <span className="muted text-xs">总股本 </span>
            <span className="num">{fmtShares(p?.total_shares)}</span>
          </div>
          <div className="text-sm">
            <span className="muted text-xs">流通股本 </span>
            <span className="num">{fmtShares(p?.float_shares)}</span>
          </div>
          {lp && (
            <div className="muted ml-auto text-xs">
              最新报告期 <span className="num">{lp.period_end ?? "—"}</span>
              {lp.report_type ? `(${lp.report_type})` : ""}
              {lp.announced_date ? (
                <>
                  {" "}· 披露于 <span className="num">{lp.announced_date}</span>
                </>
              ) : null}
            </div>
          )}
        </div>
      </div>

      {/* 核心指标 */}
      <div className="card">
        <div className="card-title">核心指标</div>
        {metricCards.length === 0 ? (
          <EmptyBox text="核心指标数据缺失" />
        ) : (
          <div className="p-3">
            <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-5">
              {metricCards.map((c) => (
                <div
                  key={c.label}
                  className="rounded border border-slate-200 px-2 py-2 dark:border-slate-800"
                >
                  <div className="muted text-xs">
                    <Term label={c.label} tip={c.tip} />
                  </div>
                  <div className="num mt-1 flex items-center gap-1 text-base font-semibold">
                    {c.value}
                    <TrendArrow dir={c.trend} />
                  </div>
                  {c.sub ? <div className="num mt-0.5 text-xs">{c.sub}</div> : null}
                </div>
              ))}
            </div>
            {data.metrics?.dupont && (
              <div className="muted mt-2 text-xs">
                <Term
                  label="杜邦分解"
                  tip="ROE = 净利率 × 资产周转率 × 权益乘数,拆解赚钱靠的是利润厚、周转快还是杠杆高"
                />
                <span className="num ml-2">
                  净利率 {fmtPct(data.metrics.dupont.net_margin, 2, false)} × 资产周转{" "}
                  {fmtNum(data.metrics.dupont.asset_turnover)} × 权益乘数{" "}
                  {fmtNum(data.metrics.dupont.equity_multiplier)}
                </span>
              </div>
            )}
          </div>
        )}
      </div>

      {/* 成长图表 */}
      <div className="card">
        <div className="card-title">
          <Term label="成长趋势" tip="各报告期营收/净利润(柱,左轴)与同比增速(线,右轴)" />
        </div>
        {growthOption ? (
          <Chart option={growthOption} height={260} />
        ) : (
          <EmptyBox text="暂无成长数据" />
        )}
      </div>

      {/* 评分区 */}
      <div className="card">
        <div className="card-title">财务评分</div>
        {!scores || (!scores.piotroski && !scores.altman && !scores.beneish) ? (
          <EmptyBox text="评分数据缺失" />
        ) : (
          <div className="grid grid-cols-1 gap-3 p-3 lg:grid-cols-3">
            {/* Piotroski F */}
            <div className="rounded border border-slate-200 p-2 dark:border-slate-800">
              <div className="flex items-center justify-between">
                <Term
                  label="Piotroski F"
                  tip="9项财务健康清单:盈利、现金流、杠杆、运营效率各维度逐项打分,≥7分质量优秀,≤2分警惕"
                />
                <span className="num text-sm font-semibold">
                  {scores.piotroski?.score != null ? `${scores.piotroski.score}/9` : "—"}
                </span>
              </div>
              {scores.piotroski && scores.piotroski.criteria.length > 0 ? (
                <div className="mt-2 grid grid-cols-1 gap-1">
                  {scores.piotroski.criteria.map((c, i) => (
                    <div key={i} className="flex items-center justify-between text-xs">
                      <span className="muted">{c.name}</span>
                      {/* 达标绿色√ / 未达标红色× / 数据不足灰色— */}
                      <span
                        className={
                          "num font-semibold " +
                          (c.passed == null ? "muted" : c.passed ? "text-down" : "text-up")
                        }
                      >
                        {c.passed == null ? "—" : c.passed ? "√" : "×"}
                      </span>
                    </div>
                  ))}
                </div>
              ) : (
                <div className="muted mt-2 text-xs">数据不足</div>
              )}
            </div>

            {/* Altman Z */}
            <div className="rounded border border-slate-200 p-2 dark:border-slate-800">
              <div className="flex items-center justify-between">
                <Term
                  label="Altman Z"
                  tip="破产风险预测模型:经典Z>2.99安全、<1.81危险;新兴市场Z''>2.60安全、<1.10危险(A股非金融企业适用Z'')"
                />
                <span className={"tag border " + zone.cls}>{zone.label}</span>
              </div>
              <div className="mt-3 space-y-1.5 text-sm">
                <div className="flex items-center justify-between">
                  <span className="muted text-xs">Z''(新兴市场)</span>
                  <span className="num font-semibold">{fmtNum(scores.altman?.z_emerging)}</span>
                </div>
                <div className="flex items-center justify-between">
                  <span className="muted text-xs">Z(经典)</span>
                  <span className="num">{fmtNum(scores.altman?.z_classic)}</span>
                </div>
              </div>
            </div>

            {/* Beneish M */}
            <div className="rounded border border-slate-200 p-2 dark:border-slate-800">
              <div className="flex items-center justify-between">
                <Term
                  label="Beneish M"
                  tip="财务造假概率模型,M > -1.78 提示存在利润操纵嫌疑,值越大风险越高"
                />
                <span
                  className={
                    "num text-sm font-semibold " +
                    (mScore == null ? "muted" : mScore > -1.78 ? "text-up" : "text-down")
                  }
                >
                  {fmtNum(mScore)}
                </span>
              </div>
              <div className="muted mt-3 text-xs">
                {mScore == null ? "数据不足" : scores.beneish?.interpretation ?? "—"}
              </div>
            </div>
          </div>
        )}
      </div>

      {/* 财务异常预警 */}
      <div className="card">
        <div className="card-title">
          <Term
            label="财务异常预警"
            tip="自动扫描财报中的矛盾信号,如增收不增现、存贷双高、毛利率异常偏离行业等"
          />
        </div>
        {anomalies.length === 0 ? (
          <EmptyBox text="未发现明显异常信号" />
        ) : (
          <div className="space-y-2 p-3">
            {anomalies.map((a, i) => {
              const tag = severityTag(a.severity);
              return (
                <div
                  key={i}
                  className="rounded border border-slate-200 px-3 py-2 dark:border-slate-800"
                >
                  <div className="flex items-center gap-2">
                    <span className={"tag " + tag.cls}>{tag.label}</span>
                    <span className="text-sm font-medium">{a.kind}</span>
                  </div>
                  <div className="mt-1 text-xs">{a.explanation}</div>
                  {a.evidence != null && (
                    <div className="num muted mt-0.5 text-xs">
                      {typeof a.evidence === "string" ? a.evidence : JSON.stringify(a.evidence)}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>

      {/* 分红 */}
      <div className="card">
        <div className="card-title">
          <Term label="分红" tip="近年分红方案,持续稳定分红是现金流真实性的旁证" />
        </div>
        {dividends.length === 0 ? (
          <EmptyBox text="暂无分红记录" />
        ) : (
          <div className="divide-y divide-slate-200 px-3 dark:divide-slate-800">
            {dividends.map((d, i) => (
              <div key={i} className="flex items-center gap-3 py-1.5 text-sm">
                <span className="num muted w-20 shrink-0 text-xs">
                  {d.year != null ? `${d.year}年度` : "—"}
                </span>
                <span>{d.plan ?? "—"}</span>
              </div>
            ))}
          </div>
        )}
      </div>

      {missing.length > 0 && (
        <div className="muted px-1 text-xs">数据缺失: {missing.join("、")}</div>
      )}
    </>
  );
}
