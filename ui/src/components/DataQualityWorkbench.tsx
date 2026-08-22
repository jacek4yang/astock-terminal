import { useCallback, useEffect, useMemo, useState } from "react";
import {
  errMsg,
  getDataHealthReport,
  getDataQualityObservations,
  getDataQualitySlo,
  getDataReconciliations,
  getFieldLineage,
  reconcileQuoteSources,
  reconcileValuationSources,
  type DataHealthReport,
  type DatasetKind,
  type DatasetSlo,
  type FieldLineageRecord,
  type QualityObservation,
  type QuoteReconciliationReport,
  type ReconciliationAudit,
  type ValuationReconciliationReport,
} from "../lib/api";

const DATASET_LABELS: Record<DatasetKind, string> = {
  realtime_quote: "实时行情",
  intraday_minute: "分时行情",
  daily_kline: "日线",
  weekly_kline: "周线",
  monthly_kline: "月线",
  fund_flow: "资金流",
  fundamentals: "财务报表",
  valuation: "估值",
  announcement: "正式公告",
  news: "财经资讯",
  knowledge_graph: "产业链图谱",
  macro: "宏观数据",
  backtest: "回测结果",
  search_discovery: "搜索线索",
  other: "其他数据",
};

const FRESHNESS_LABELS: Record<string, string> = {
  fresh: "新鲜",
  stale: "已陈旧",
  expired: "已硬过期",
};

const STATUS_LABELS: Record<string, string> = {
  matched: "完全一致",
  within_tolerance: "容差内一致",
  conflict: "数值冲突",
  incompatible_contract: "口径不兼容",
};

function dateTime(seconds: number | null): string {
  if (seconds == null || !Number.isFinite(Number(seconds))) return "尚无记录";
  return new Date(Number(seconds) * 1000).toLocaleString("zh-CN", { hour12: false });
}

function duration(seconds: number | null | undefined): string {
  const value = Number(seconds);
  if (!Number.isFinite(value)) return "未知";
  if (value < 60) return `${Math.round(value)} 秒`;
  if (value < 3600) return `${Math.round(value / 60)} 分钟`;
  if (value < 86400) return `${(value / 3600).toFixed(1)} 小时`;
  return `${(value / 86400).toFixed(1)} 天`;
}

function percent(value: number): string {
  const normalized = Number(value);
  return Number.isFinite(normalized) ? `${(normalized * 100).toFixed(2)}%` : "未知";
}

function freshnessClass(value: string): string {
  if (value === "fresh") return "text-down";
  if (value === "expired") return "text-up";
  return "text-amber-500";
}

function copyText(value: unknown) {
  return navigator.clipboard.writeText(typeof value === "string" ? value : JSON.stringify(value, null, 2));
}

function SloRow({ row }: { row: DatasetSlo }) {
  return (
    <details className="rounded-lg border border-slate-200 dark:border-slate-800">
      <summary className="grid cursor-pointer list-none grid-cols-[1.3fr_1fr_.8fr_.8fr] items-center gap-2 p-2.5 text-xs">
        <span className="font-medium">{row.dataset_name} · <span className="num">{row.provider}</span></span>
        <span className={freshnessClass(row.current_freshness)}>{FRESHNESS_LABELS[row.current_freshness] ?? row.current_freshness}</span>
        <span className="num">错误率 {percent(row.error_rate)}</span>
        <span className="num">P95 {row.latency_p95_ms == null ? "暂无" : `${row.latency_p95_ms} 毫秒`}</span>
      </summary>
      <div className="space-y-2 border-t border-slate-200 p-3 text-xs dark:border-slate-800">
        <div className="grid gap-2 sm:grid-cols-3">
          <div><span className="muted">真实观测：</span><span className="num">{row.observations} 次</span></div>
          <div><span className="muted">成功：</span><span className="num">{row.successes} 次</span></div>
          <div><span className="muted">最近成功：</span><span className="num">{dateTime(row.last_success_at)}</span></div>
          <div><span className="muted">P50：</span><span className="num">{row.latency_p50_ms == null ? "暂无" : `${row.latency_p50_ms} 毫秒`}</span></div>
          <div><span className="muted">连续陈旧/失败：</span><span className="num">{row.consecutive_stale} 次</span></div>
          <div><span className="muted">缺失/冲突：</span><span className="num">{row.missing_fields} / {row.conflicts}</span></div>
          <div><span className="muted">预期更新：</span><span className="num">{duration(row.expected_cadence_secs)}</span></div>
          <div><span className="muted">陈旧阈值：</span><span className="num">{duration(row.stale_after_secs)}</span></div>
          <div><span className="muted">硬过期：</span><span className="num">{duration(row.hard_expiry_secs)}</span></div>
        </div>
        {row.latest_quality_flags.length > 0 && (
          <div className="space-y-1">
            {row.latest_quality_flags.map((flag, index) => (
              <div key={`${flag.code}:${index}`} className={flag.severity === "blocking" ? "text-up" : "text-amber-500"}>
                {flag.field ? `${flag.field}：` : ""}{flag.message}
              </div>
            ))}
          </div>
        )}
      </div>
    </details>
  );
}

function ObservationRow({ row }: { row: QualityObservation }) {
  const [copied, setCopied] = useState(false);
  return (
    <details className="rounded border border-slate-200 dark:border-slate-800">
      <summary className="flex cursor-pointer list-none flex-wrap items-center gap-x-3 gap-y-1 p-2 text-xs">
        <span className={`h-2 w-2 rounded-full ${row.success ? "bg-down" : "bg-up"}`} />
        <span className="font-medium">{DATASET_LABELS[row.dataset]} · <span className="num">{row.provider}</span></span>
        <span>{row.operation}</span>
        {row.entity_key && <span className="num muted">{row.entity_key}</span>}
        <span className="num muted">{row.latency_ms == null ? "耗时未采集" : `${row.latency_ms} 毫秒`}</span>
        <span className="num muted ml-auto">{dateTime(row.recorded_at)}</span>
      </summary>
      <div className="space-y-2 border-t border-slate-200 p-2.5 text-xs dark:border-slate-800">
        <div className="grid gap-2 sm:grid-cols-3">
          <div>实时性：<span className={freshnessClass(row.summary.freshness)}>{FRESHNESS_LABELS[row.summary.freshness]}</span></div>
          <div>数据年龄：<span className="num">{duration(row.summary.age_secs)}</span></div>
          <div>置信上限：<span className="num">{row.summary.confidence_ceiling}</span></div>
          <div>缺失字段：<span className="num">{row.missing_fields}</span></div>
          <div>跨源冲突：<span className="num">{row.conflicts}</span></div>
          <div>确定性计算：{row.summary.allow_deterministic_compute ? "允许" : <span className="text-up">已阻止</span>}</div>
        </div>
        {row.summary.quality_flags.map((flag, index) => (
          <div key={`${flag.code}:${index}`} className={`rounded px-2 py-1 ${flag.severity === "blocking" ? "bg-red-50 text-up dark:bg-red-950/30" : "bg-amber-50 text-amber-700 dark:bg-amber-950/30 dark:text-amber-300"}`}>
            {flag.field ? `${flag.field}：` : ""}{flag.message}
          </div>
        ))}
        {row.error_kind && (
          <div className="rounded bg-red-50 p-2 text-up dark:bg-red-950/30">
            <div className="break-all">{row.error_kind}</div>
            <button className="btn mt-2" onClick={() => copyText(row.error_kind).then(() => setCopied(true))}>
              {copied ? "已复制" : "复制错误和诊断信息"}
            </button>
          </div>
        )}
        <button className="btn" onClick={() => copyText(row).then(() => setCopied(true))}>
          {copied ? "已复制完整记录" : "复制完整记录"}
        </button>
      </div>
    </details>
  );
}

function LineageRow({ row }: { row: FieldLineageRecord }) {
  return (
    <details className="rounded border border-slate-200 dark:border-slate-800">
      <summary className="flex cursor-pointer list-none flex-wrap items-center gap-2 p-2 text-xs">
        <span className="font-medium">{row.entity_key} · {row.field_path}</span>
        <span className="num">{row.source}</span>
        <span className="muted">{row.unit ?? "未声明单位"}{row.currency ? ` / ${row.currency.toUpperCase()}` : ""}</span>
        <span className="num muted ml-auto">{dateTime(row.fetched_at)}</span>
      </summary>
      <div className="grid gap-2 border-t border-slate-200 p-2.5 text-xs sm:grid-cols-2 dark:border-slate-800">
        <div>数据时点：<span className="num">{dateTime(row.as_of_time)}</span></div>
        <div>发布时间：<span className="num">{dateTime(row.publish_time)}</span></div>
        <div>解析器版本：<span className="num">{row.parser_version}</span></div>
        <div>结构版本：<span className="num">{row.schema_version}</span></div>
        <div>复权口径：<span className="num">{row.adjustment}</span></div>
        <div>财务口径：<span className="num">{row.accounting_scope}</span></div>
        <div className="sm:col-span-2">许可说明：{row.license}</div>
        <div className="break-all sm:col-span-2">来源地址：{row.source_url ?? "该协议源没有网页地址"}</div>
        <button className="btn w-fit" onClick={() => copyText(row)}>复制字段血缘</button>
      </div>
    </details>
  );
}

function ReconciliationRow({ row }: { row: ReconciliationAudit }) {
  const result = row.result;
  return (
    <details className="rounded border border-slate-200 dark:border-slate-800">
      <summary className="flex cursor-pointer list-none flex-wrap items-center gap-2 p-2 text-xs">
        <span className={row.blocking ? "font-medium text-up" : "font-medium text-down"}>{STATUS_LABELS[result.status] ?? result.status}</span>
        <span>{row.entity_key} · {result.field}</span>
        <span className="num muted ml-auto">{dateTime(row.compared_at)}</span>
      </summary>
      <div className="space-y-2 border-t border-slate-200 p-2.5 text-xs dark:border-slate-800">
        <div className="grid gap-2 sm:grid-cols-2">
          <div>{result.left.provider}：<span className="num font-medium">{result.left.value}</span> {result.left.unit}</div>
          <div>{result.right.provider}：<span className="num font-medium">{result.right.value}</span> {result.right.unit}</div>
          <div>绝对偏差：<span className="num">{result.absolute_diff}</span></div>
          <div>相对偏差：<span className="num">{percent(result.relative_diff)}</span></div>
          <div>绝对容差：<span className="num">{result.tolerance.absolute}</span></div>
          <div>相对容差：<span className="num">{percent(result.tolerance.relative)}</span></div>
        </div>
        <div>{result.explanation}</div>
        <button className="btn" onClick={() => copyText(row)}>复制完整对账记录</button>
      </div>
    </details>
  );
}

export default function DataQualityWorkbench() {
  const [windowSecs, setWindowSecs] = useState(86400);
  const [rows, setRows] = useState<DatasetSlo[]>([]);
  const [observations, setObservations] = useState<QualityObservation[]>([]);
  const [lineage, setLineage] = useState<FieldLineageRecord[]>([]);
  const [reconciliations, setReconciliations] = useState<ReconciliationAudit[]>([]);
  const [report, setReport] = useState<DataHealthReport | null>(null);
  const [symbol, setSymbol] = useState("600519");
  const [result, setResult] = useState<QuoteReconciliationReport | null>(null);
  const [valuationResult, setValuationResult] = useState<ValuationReconciliationReport | null>(null);
  const [busyAction, setBusyAction] = useState<"quote" | "valuation" | null>(null);
  const [elapsed, setElapsed] = useState(0);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const [slo, recent, health] = await Promise.all([
        getDataQualitySlo(windowSecs),
        getDataQualityObservations(null, null, 100),
        getDataHealthReport(windowSecs),
      ]);
      setRows(slo);
      setObservations(recent);
      setReport(health);
      setError(null);
    } catch (loadError) {
      setError(errMsg(loadError));
    }
  }, [windowSecs]);

  useEffect(() => { load(); }, [load]);

  useEffect(() => {
    if (!busyAction) return;
    const started = Date.now();
    const timer = window.setInterval(() => setElapsed(Math.floor((Date.now() - started) / 1000)), 1000);
    return () => window.clearInterval(timer);
  }, [busyAction]);

  const symbolValid = /^\d{6}$/.test(symbol.trim());
  const providers = useMemo(() => [...new Set(observations.map((row) => row.provider))], [observations]);

  const inspectSymbol = async (runReconciliation: boolean) => {
    const code = symbol.trim();
    if (!/^\d{6}$/.test(code)) return;
    setBusyAction(runReconciliation ? "quote" : null);
    setElapsed(0);
    try {
      if (runReconciliation) setResult(await reconcileQuoteSources(code));
      const [fields, audits] = await Promise.all([
        getFieldLineage("realtime_quote", code, 100),
        getDataReconciliations("realtime_quote", code, 100),
      ]);
      setLineage(fields);
      setReconciliations(audits);
      setError(null);
      await load();
    } catch (inspectError) {
      setError(errMsg(inspectError));
    } finally {
      setBusyAction(null);
    }
  };

  const inspectValuation = async () => {
    const code = symbol.trim();
    if (!/^\d{6}$/.test(code)) return;
    setBusyAction("valuation");
    setElapsed(0);
    try {
      setValuationResult(await reconcileValuationSources(code));
      const [fields, audits] = await Promise.all([
        getFieldLineage("valuation", code, 100),
        getDataReconciliations("valuation", code, 100),
      ]);
      setLineage((current) => [...fields, ...current.filter((row) => row.dataset !== "valuation")]);
      setReconciliations((current) => [...audits, ...current.filter((row) => row.dataset !== "valuation")]);
      setError(null);
      await load();
    } catch (inspectError) {
      setError(errMsg(inspectError));
    } finally {
      setBusyAction(null);
    }
  };

  return (
    <div className="card">
      <div className="card-title flex flex-wrap items-center justify-between gap-2">
        <span>数据质量、字段血缘与双源校验</span>
        <div className="flex items-center gap-2">
          <select className="input !py-1 text-xs" value={windowSecs} onChange={(event) => setWindowSecs(Number(event.target.value))}>
            <option value={86400}>最近 24 小时</option>
            <option value={604800}>最近 7 天</option>
            <option value={2592000}>最近 30 天</option>
          </select>
          <button className="btn !px-2 !py-1 text-xs" onClick={load}>刷新本地观测</button>
        </div>
      </div>
      <div className="space-y-4 p-4 text-xs">
        <div className="muted leading-relaxed">
          本面板只读取已发生的真实调用记录，不会为了“健康”而主动轰击上游。展开每一行可查看延迟、失败、陈旧、缺失、冲突、单位、币种、复权、财务口径和许可信息。
        </div>
        {error && (
          <div className="flex items-start gap-2 rounded bg-red-50 p-2 text-up dark:bg-red-950/30">
            <span className="min-w-0 flex-1 break-all">{error}</span>
            <button className="btn" onClick={() => copyText(error)}>复制错误</button>
          </div>
        )}

        {report && (
          <div className={`rounded-lg border p-3 ${report.continuous_window_satisfied ? "border-emerald-300 bg-emerald-50/50 dark:border-emerald-800 dark:bg-emerald-950/20" : "border-amber-300 bg-amber-50/50 dark:border-amber-800 dark:bg-amber-950/20"}`}>
            <div className="flex flex-wrap items-center gap-3">
              <span className="font-medium">连续健康报告</span>
              <span className="num">真实观测 {report.actual_observations} 条</span>
              <span className="num">覆盖 {duration(report.coverage_secs)}</span>
              <span>{report.continuous_window_satisfied ? "已满足所选连续窗口" : "样本持续积累中"}</span>
              <button className="btn ml-auto" onClick={() => copyText(report.markdown)}>复制报告</button>
            </div>
            {report.limitation && <div className="mt-2 text-amber-700 dark:text-amber-300">{report.limitation}</div>}
          </div>
        )}

        <div>
          <div className="mb-2 flex items-center justify-between"><span className="font-medium">按数据集与来源统计</span><span className="muted">共 {rows.length} 组</span></div>
          <div className="space-y-1.5">
            {rows.length === 0 ? <div className="muted">尚无观测；使用行情或 Agent 工具后会自动出现。</div> : rows.map((row) => <SloRow key={`${row.dataset}:${row.provider}`} row={row} />)}
          </div>
        </div>

        <div className="rounded-lg border border-slate-200 p-3 dark:border-slate-800">
          <div className="font-medium">指定股票双源校验与字段下钻</div>
          <div className="muted mt-1">通达信与东方财富并发获取，逐字段比较原值及容差。通常约 1–15 秒；若通达信正在探测可用节点会更久，任务不会因前端预估时间被强行中止。</div>
          <div className="mt-2 flex flex-wrap gap-2">
            <input className="input w-36" value={symbol} onChange={(event) => setSymbol(event.target.value.replace(/\D/g, "").slice(0, 6))} placeholder="6 位股票代码" />
            <button className="btn-primary" disabled={!symbolValid || busyAction != null} onClick={() => inspectSymbol(true)}>{busyAction === "quote" ? `行情校验中 ${elapsed} 秒` : "校验实时行情"}</button>
            <button className="btn-primary" disabled={!symbolValid || busyAction != null} onClick={inspectValuation}>{busyAction === "valuation" ? `估值校验中 ${elapsed} 秒` : "校验估值口径"}</button>
            <button className="btn" disabled={!symbolValid || busyAction != null} onClick={() => inspectSymbol(false)}>只看已有详情</button>
          </div>
          {busyAction && (
            <div className="mt-3 space-y-1.5 rounded bg-blue-50 p-2 dark:bg-blue-950/30">
              <div className="h-1.5 overflow-hidden rounded bg-blue-100 dark:bg-blue-900"><div className="h-full w-2/3 animate-pulse rounded bg-blue-600" /></div>
              <div>正在并行等待 {busyAction === "quote" ? "2 个行情" : "东方财富、聚宽、Tushare 中已配置的估值"}来源 · 已运行 {elapsed} 秒</div>
              <div className="muted">返回后将继续：解析字段 → 校验单位和币种 → 计算绝对/相对偏差 → 保存字段血缘与对账记录。</div>
            </div>
          )}
          {result && (
            <div className={`mt-3 rounded p-2 ${result.blocking ? "bg-red-50 text-up dark:bg-red-950/30" : "bg-emerald-50 text-down dark:bg-emerald-950/30"}`}>
              成功来源 {result.comparable_sources}/2 · 比较字段 {result.results.length} 项 · {result.blocking ? "存在阻断项，不得升级为高置信结论" : "未发现阻断冲突"}
              {result.limitation && <div className="mt-1">{result.limitation}</div>}
              {result.failures.map((failure) => <div className="mt-1 break-all" key={failure.provider}>{failure.provider}：{failure.error}</div>)}
            </div>
          )}
          {valuationResult && (
            <div className={`mt-3 rounded p-2 ${valuationResult.blocking ? "bg-red-50 text-up dark:bg-red-950/30" : "bg-emerald-50 text-down dark:bg-emerald-950/30"}`}>
              估值来源 {valuationResult.comparable_sources} 个 · 比较字段 {valuationResult.results.length} 项 · {valuationResult.blocking ? "估值证据不足或有冲突，已限制置信度" : "估值跨源口径在容差内"}
              {valuationResult.limitation && <div className="mt-1">{valuationResult.limitation}</div>}
              {valuationResult.failures.map((failure) => <div className="mt-1 break-all" key={failure.provider}>{failure.provider}：{failure.error}</div>)}
            </div>
          )}
          {reconciliations.length > 0 && <div className="mt-3 space-y-1.5">{reconciliations.map((row, index) => <ReconciliationRow key={row.reconciliation_id ?? index} row={row} />)}</div>}
          {lineage.length > 0 && (
            <details className="mt-3">
              <summary className="cursor-pointer font-medium">查看 {lineage.length} 条字段血缘记录</summary>
              <div className="mt-2 space-y-1.5">{lineage.map((row, index) => <LineageRow key={row.lineage_id ?? index} row={row} />)}</div>
            </details>
          )}
        </div>

        <details>
          <summary className="cursor-pointer font-medium">最近 100 条调用明细（逐层展开）</summary>
          <div className="muted mt-1">涉及来源：{providers.length ? providers.join("、") : "暂无"}</div>
          <div className="mt-2 space-y-1.5">{observations.map((row, index) => <ObservationRow key={row.observation_id ?? index} row={row} />)}</div>
        </details>
      </div>
    </div>
  );
}
