import { useEffect, useMemo, useState } from "react";
import type { EChartsOption } from "echarts";
import {
  errMsg,
  getEarningsDriverTree,
  runEarningsDriverShock,
  type DriverParameter,
  type DriverBranch,
  type DriverValueOrigin,
  type EarningsDriverTree,
  type EarningsShockBridge,
} from "../lib/api";
import { finiteNumber, fmtNum, fmtPct, fmtText, fmtYiWan, pctClass } from "../lib/format";
import Chart from "./Chart";
import { EmptyBox, ErrorBox, Loading, Term } from "./ui";

interface Props {
  symbol: string;
}

type View = "tree" | "scenario" | "shock" | "evidence";

const ORIGINS: Record<DriverValueOrigin, string> = {
  historical_fact: "历史事实",
  management_guidance: "管理层指引",
  market_consensus: "市场一致预期",
  user_assumption: "用户假设",
  agent_assumption: "AI 助手假设",
  industry_prior: "行业先验",
};

const SHOCKS = [
  ["raw_material", "原材料价格"],
  ["energy", "能源价格"],
  ["transport", "运输成本"],
  ["product_price", "产品售价"],
  ["volume", "产品销量"],
  ["capacity", "产能"],
  ["fx", "汇率"],
  ["opex", "期间费用"],
  ["working_capital", "营运资金占用"],
] as const;

function ratio(value: unknown): string {
  const n = finiteNumber(value);
  return n == null ? "暂无" : fmtPct(n * 100, 1, false);
}

function parameterValue(parameter: DriverParameter): string {
  if (parameter.unit === "decimal") return ratio(parameter.value);
  if (parameter.unit === "CNY") return fmtYiWan(parameter.value);
  return `${fmtNum(parameter.value)} ${parameter.unit === "share" ? "股" : parameter.unit}`;
}

function parameterRange(parameter: DriverParameter): string {
  if (parameter.low == null || parameter.high == null) return "暂无区间";
  if (parameter.unit === "decimal") return `${ratio(parameter.low)} ～ ${ratio(parameter.high)}`;
  if (parameter.unit === "CNY") return `${fmtYiWan(parameter.low)} ～ ${fmtYiWan(parameter.high)}`;
  return `${fmtNum(parameter.low)} ～ ${fmtNum(parameter.high)}`;
}

function copyText(value: string) {
  void navigator.clipboard?.writeText(value);
}

function BranchTree({ title, branch }: { title: string; branch: DriverBranch }) {
  return (
    <div className="rounded border border-slate-200 p-3 text-xs dark:border-slate-800">
      <div className="font-medium">{title}：{branch.label}</div>
      <div className="muted mt-1">{branch.formula}</div>
      <div className="mt-2 grid gap-1.5 md:grid-cols-2">
        {branch.children.map((child) => (
          <div key={child.id} className="rounded bg-slate-50 p-2 dark:bg-slate-900">
            <div className="flex items-center justify-between gap-2">
              <span className="font-medium">{child.label}</span>
              <span className="text-[10px] text-amber-600 dark:text-amber-300">等待分部披露</span>
            </div>
            <div className="muted mt-1">{child.formula}</div>
          </div>
        ))}
      </div>
    </div>
  );
}

export default function EarningsDriverPanel({ symbol }: Props) {
  const [data, setData] = useState<EarningsDriverTree | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [view, setView] = useState<View>("tree");
  const [origin, setOrigin] = useState<DriverValueOrigin | "all">("all");
  const [page, setPage] = useState(1);
  const [shockKind, setShockKind] = useState("raw_material");
  const [magnitude, setMagnitude] = useState("10");
  const [passThrough, setPassThrough] = useState("0");
  const [lagMonths, setLagMonths] = useState("3");
  const [evidenceId, setEvidenceId] = useState("");
  const [bridge, setBridge] = useState<EarningsShockBridge | null>(null);
  const [shockBusy, setShockBusy] = useState(false);
  const [shockError, setShockError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    setData(null);
    setError(null);
    setBridge(null);
    getEarningsDriverTree(symbol)
      .then((value) => alive && setData(value))
      .catch((reason) => alive && setError(errMsg(reason)));
    return () => {
      alive = false;
    };
  }, [symbol]);

  const scenarioChart = useMemo<EChartsOption | null>(() => {
    if (!data?.scenarios.length) return null;
    const labels: Record<string, string> = { bear: "悲观", base: "基准", bull: "乐观" };
    return {
      animation: false,
      tooltip: { trigger: "axis" },
      legend: { top: 0, textStyle: { fontSize: 10 } },
      grid: { left: 52, right: 18, top: 38, bottom: 28 },
      xAxis: { type: "category", data: data.scenarios.map((row) => labels[row.scenario] ?? row.scenario) },
      yAxis: { type: "value", name: "亿元", nameTextStyle: { fontSize: 10 }, splitLine: { lineStyle: { opacity: 0.15 } } },
      series: [
        { name: "营业收入", type: "bar", data: data.scenarios.map((row) => row.revenue / 1e8) },
        { name: "归母净利润", type: "bar", data: data.scenarios.map((row) => row.parent_net_profit / 1e8) },
        { name: "自由现金流", type: "bar", data: data.scenarios.map((row) => row.free_cash_flow / 1e8) },
      ],
    };
  }, [data]);

  if (error) return <div className="card"><div className="card-title">盈利驱动树</div><ErrorBox message={error} /></div>;
  if (!data) return <div className="card"><div className="card-title">盈利驱动树</div><Loading text="正在连接经营参数、财务报表与估值…" /></div>;

  const filtered = origin === "all" ? data.parameters : data.parameters.filter((item) => item.origin === origin);
  const pageSize = 8;
  const pageCount = Math.max(1, Math.ceil(filtered.length / pageSize));
  const safePage = Math.min(page, pageCount);
  const parameters = filtered.slice((safePage - 1) * pageSize, safePage * pageSize);

  async function runShock() {
    const value = finiteNumber(magnitude);
    const pass = finiteNumber(passThrough);
    const lag = finiteNumber(lagMonths);
    if (value == null) {
      setShockError("请输入有效的变化幅度");
      return;
    }
    setShockBusy(true);
    setShockError(null);
    try {
      const result = await runEarningsDriverShock(symbol, [{
        kind: shockKind,
        magnitude: value / 100,
        lag_months: Math.max(0, Math.round(lag ?? 0)),
        pass_through: pass == null ? null : pass / 100,
        evidence_version_id: evidenceId.trim() || null,
        note: "用户在盈利驱动面板进行的冲击试算",
      }]);
      setBridge(result);
    } catch (reason) {
      setShockError(errMsg(reason));
    } finally {
      setShockBusy(false);
    }
  }

  return (
    <div className="card">
      <div className="card-title flex-wrap gap-2">
        <div>
          <Term label="盈利驱动树" tip="把销量、价格、成本、费用和营运资金连接到利润与现金流；缺少分部披露时只给区间" />
          <span className="ml-2 rounded bg-blue-50 px-1.5 py-0.5 text-[10px] text-blue-700 dark:bg-blue-950/50 dark:text-blue-300">
            {data.industry_template_label}模型
          </span>
          {data.golden_template_reviewed && <span className="ml-1 text-[10px] text-down">行业公式已人工核对</span>}
        </div>
        <div className="ml-auto flex flex-wrap gap-1">
          {(["tree", "scenario", "shock", "evidence"] as View[]).map((key) => (
            <button key={key} className={`btn text-xs ${view === key ? "btn-primary" : ""}`} onClick={() => setView(key)}>
              {{ tree: "驱动树", scenario: "情景与敏感性", shock: "冲击试算", evidence: "参数与证据" }[key]}
            </button>
          ))}
        </div>
      </div>

      <div className="border-b border-slate-200 px-4 py-3 text-xs dark:border-slate-800">
        <div className="grid gap-2 md:grid-cols-2">
          <div><span className="muted">收入逻辑：</span>{data.revenue_formula}</div>
          <div><span className="muted">成本逻辑：</span>{data.cost_formula}</div>
        </div>
        <div className="muted mt-2 flex flex-wrap items-center gap-x-4 gap-y-1">
          <span>报告期：{fmtText(data.report_period)}</span>
          <span>模型完整度：{ratio(data.quality.model_completeness)}</span>
          <button className="underline decoration-dotted" onClick={() => copyText(data.snapshot_id)} title="复制快照编号">
            快照：{data.snapshot_id.slice(0, 18)}…
          </button>
          <button className="underline decoration-dotted" onClick={() => copyText(data.parameter_snapshot_id)} title="复制共享参数快照编号">
            参数口径：{data.parameter_snapshot_id.slice(0, 14)}…
          </button>
        </div>
        {!data.quality.exact_eps_available && (
          <div className="mt-2 rounded border border-amber-300/60 bg-amber-50 px-2 py-1.5 text-amber-800 dark:border-amber-800/60 dark:bg-amber-950/30 dark:text-amber-200">
            不输出精确 EPS：缺少{data.quality.missing_core_drivers.join("、")}。下方数字是可审计的宽情景，不是单点预测。
          </div>
        )}
      </div>

      {view === "tree" && (
        <div className="space-y-2 p-3">
          <div className="grid gap-2 lg:grid-cols-2">
            <BranchTree title="收入树" branch={data.revenue_tree} />
            <BranchTree title="成本树" branch={data.cost_tree} />
          </div>
          {data.formula_nodes.length === 0 ? <EmptyBox text={data.quality.refusal_reason ?? "核心财务数据不足"} /> : data.formula_nodes.map((node) => (
            <details key={node.id} className="rounded border border-slate-200 px-3 py-2 dark:border-slate-800" open={node.id === "forecast_revenue"}>
              <summary className="cursor-pointer list-none">
                <div className="grid grid-cols-[minmax(120px,1fr)_repeat(3,minmax(70px,0.7fr))] items-center gap-2 text-xs">
                  <strong>{node.name}</strong>
                  <span><span className="muted">悲观 </span>{node.unit === "CNY/share" ? fmtNum(node.forecast_low) : fmtYiWan(node.forecast_low)}</span>
                  <span><span className="muted">基准 </span>{node.unit === "CNY/share" ? fmtNum(node.forecast_base) : fmtYiWan(node.forecast_base)}</span>
                  <span><span className="muted">乐观 </span>{node.unit === "CNY/share" ? fmtNum(node.forecast_high) : fmtYiWan(node.forecast_high)}</span>
                </div>
              </summary>
              <div className="muted mt-2 border-t border-slate-200 pt-2 text-xs dark:border-slate-800">
                <div>公式：{node.formula}</div>
                <div className="mt-1">参数：{node.parameter_ids.join(" + ")}</div>
              </div>
            </details>
          ))}
        </div>
      )}

      {view === "scenario" && (
        <div className="space-y-3 p-3">
          {scenarioChart ? <Chart option={scenarioChart} height={260} /> : <EmptyBox text={data.quality.refusal_reason ?? "暂无可计算情景"} />}
          {data.scenarios.length > 0 && (
            <div className="overflow-x-auto">
              <table className="w-full text-xs">
                <thead><tr className="muted border-b border-slate-200 text-left dark:border-slate-800"><th className="p-2">情景</th><th>收入</th><th>毛利润</th><th>归母净利润</th><th>EPS</th><th>经营现金流</th><th>自由现金流</th></tr></thead>
                <tbody>{data.scenarios.map((row) => <tr key={row.scenario} className="border-b border-slate-100 dark:border-slate-900"><td className="p-2 font-medium">{{ bear: "悲观", base: "基准", bull: "乐观" }[row.scenario] ?? row.scenario}</td><td>{fmtYiWan(row.revenue)}</td><td>{fmtYiWan(row.gross_profit)}</td><td>{fmtYiWan(row.parent_net_profit)}</td><td>{fmtNum(row.eps)}</td><td>{fmtYiWan(row.operating_cash_flow)}</td><td>{fmtYiWan(row.free_cash_flow)}</td></tr>)}</tbody>
              </table>
            </div>
          )}
          {data.monte_carlo && (
            <div className="rounded border border-slate-200 p-3 text-xs dark:border-slate-800">
              <div className="font-medium">Monte Carlo 区间（{data.monte_carlo.samples} 次）</div>
              <div className="mt-2 grid grid-cols-3 gap-2 text-center">
                <div><div className="muted">EPS 较低区间 P10</div><div className="num">{fmtNum(data.monte_carlo.eps_p10)}</div></div>
                <div><div className="muted">EPS 中位 P50</div><div className="num">{fmtNum(data.monte_carlo.eps_p50)}</div></div>
                <div><div className="muted">EPS 较高区间 P90</div><div className="num">{fmtNum(data.monte_carlo.eps_p90)}</div></div>
              </div>
              <div className="muted mt-2">{data.monte_carlo.method}</div>
            </div>
          )}
          <div className="rounded border border-slate-200 p-3 text-xs dark:border-slate-800">
            <div className="font-medium">现价反向求解</div>
            <div className="mt-1">现价隐含 FCF 年增长率：<strong>{data.implied_assumption.implied_fcf_growth == null ? "当前数据无法求解" : ratio(data.implied_assumption.implied_fcf_growth)}</strong></div>
            <div className="muted mt-1">{data.implied_assumption.explanation}</div>
          </div>
          {data.sensitivity.length > 0 && (
            <div>
              <div className="mb-1 text-xs font-medium">收入增长 × 毛利率敏感性（格内为 EPS）</div>
              <div className="grid grid-cols-3 gap-1">{data.sensitivity.map((cell, index) => (
                <div key={index} className="rounded border border-slate-200 p-2 text-center text-[11px] dark:border-slate-800">
                  <div>{ratio(cell.revenue_growth)} / {ratio(cell.gross_margin)}</div><div className="num mt-1 font-medium">{fmtNum(cell.eps)}</div>
                </div>
              ))}</div>
            </div>
          )}
        </div>
      )}

      {view === "shock" && (
        <div className="space-y-3 p-3 text-xs">
          <div className="grid gap-2 md:grid-cols-5">
            <label>冲击项目<select className="input mt-1 w-full" value={shockKind} onChange={(event) => setShockKind(event.target.value)}>{SHOCKS.map(([key, label]) => <option key={key} value={key}>{label}</option>)}</select></label>
            <label>变化幅度（%）<input className="input mt-1 w-full" value={magnitude} onChange={(event) => setMagnitude(event.target.value)} inputMode="decimal" /></label>
            <label>可向下游传导（%）<input className="input mt-1 w-full" value={passThrough} onChange={(event) => setPassThrough(event.target.value)} inputMode="decimal" /></label>
            <label>滞后（月）<input className="input mt-1 w-full" value={lagMonths} onChange={(event) => setLagMonths(event.target.value)} inputMode="numeric" /></label>
            <label>证据版本（可选）<input className="input mt-1 w-full" value={evidenceId} onChange={(event) => setEvidenceId(event.target.value)} placeholder="如 source-v123" /></label>
          </div>
          <button className="btn btn-primary" disabled={shockBusy} onClick={() => void runShock()}>{shockBusy ? "正在重算财务桥接…" : "计算冲击如何影响利润和现金流"}</button>
          {shockError && <ErrorBox message={shockError} />}
          {bridge?.delta && (
            <div className="rounded border border-slate-200 p-3 dark:border-slate-800">
              <div className="font-medium">相对基准情景的变化</div>
              <div className="mt-2 grid grid-cols-2 gap-2 md:grid-cols-4">
                {[["营业收入", bridge.delta.revenue], ["毛利润", bridge.delta.gross_profit], ["归母净利润", bridge.delta.parent_net_profit], ["自由现金流", bridge.delta.free_cash_flow]].map(([label, value]) => (
                  <div key={String(label)}><div className="muted">{label}</div><div className={`num mt-0.5 ${pctClass(value)}`}>{fmtYiWan(value)}</div></div>
                ))}
              </div>
              <div className="muted mt-2">冲击快照：{bridge.shocked_snapshot_id}</div>
            </div>
          )}
        </div>
      )}

      {view === "evidence" && (
        <div className="p-3">
          <div className="mb-2 flex flex-wrap items-center gap-2 text-xs">
            <span className="muted">来源类别：</span>
            <select className="input" value={origin} onChange={(event) => { setOrigin(event.target.value as DriverValueOrigin | "all"); setPage(1); }}>
              <option value="all">全部</option>
              {Object.entries(ORIGINS).map(([key, label]) => <option key={key} value={key}>{label}</option>)}
            </select>
            <span className="muted">共 {filtered.length} 个参数</span>
          </div>
          <div className="space-y-1.5">{parameters.map((parameter) => (
            <details key={parameter.id} className="rounded border border-slate-200 px-3 py-2 text-xs dark:border-slate-800">
              <summary className="cursor-pointer list-none"><div className="grid grid-cols-[minmax(120px,1fr)_minmax(100px,0.7fr)_minmax(130px,1fr)_80px] gap-2"><strong>{parameter.name}</strong><span>{parameterValue(parameter)}</span><span className="muted">{parameterRange(parameter)}</span><span>{ORIGINS[parameter.origin]}</span></div></summary>
              <div className="mt-2 space-y-1 border-t border-slate-200 pt-2 dark:border-slate-800">
                <div>{parameter.note}</div>
                <div className="muted">报告期：{fmtText(parameter.report_period)} · 置信度：{ratio(parameter.confidence)}</div>
                {parameter.evidence.length === 0 ? <div className="text-amber-600">没有公司级原始证据，属于明确标注的假设/先验</div> : parameter.evidence.map((evidence) => (
                  <div key={evidence.source_version_id} className="rounded bg-slate-50 p-2 dark:bg-slate-900">
                    <div>{evidence.source_name} · {evidence.locator}</div>
                    <button className="muted mt-1 underline decoration-dotted" onClick={() => copyText(evidence.source_version_id)}>证据版本：{evidence.source_version_id}</button>
                    <div className="muted">期间 {fmtText(evidence.report_period)} · 公告 {fmtText(evidence.announced_date)} · 单位 {evidence.unit}</div>
                  </div>
                ))}
              </div>
            </details>
          ))}</div>
          <div className="mt-3 flex items-center justify-center gap-2 text-xs">
            <button className="btn" disabled={safePage <= 1} onClick={() => setPage((value) => Math.max(1, value - 1))}>上一页</button>
            <span>第 {safePage} / {pageCount} 页</span>
            <button className="btn" disabled={safePage >= pageCount} onClick={() => setPage((value) => Math.min(pageCount, value + 1))}>下一页</button>
          </div>
        </div>
      )}
    </div>
  );
}
