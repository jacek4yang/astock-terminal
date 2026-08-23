import { useEffect, useMemo, useState } from "react";
import type { EChartsOption } from "echarts";
import Chart from "./Chart";
import { ErrorBox, Term } from "./ui";
import {
  errMsg,
  quantResearchCancel,
  quantResearchSnapshotGet,
  quantResearchSnapshotList,
  quantResearchStart,
  quantResearchStatus,
  type QuantMetric,
  type QuantPairInference,
  type QuantResearchConfig,
  type QuantResearchJob,
  type QuantResearchSnapshot,
  type QuantSnapshotListItem,
} from "../lib/api";

const METRICS: [QuantMetric, string, string][] = [
  ["pearson", "线性相关", "衡量线性共同变化；加入控制变量后计算偏相关"],
  ["spearman", "秩相关", "对极端值更稳健，衡量单调关系"],
  ["kendall", "Kendall 一致性", "基于相对排序，计算量随样本数平方增长"],
  ["distance_correlation", "距离相关", "识别非线性依赖，计算量随样本数平方增长"],
  ["mutual_information", "互信息", "衡量一般信息依赖；没有通用标准参数 p 值"],
  ["lead_lag", "预测领先关系", "扫描正负滞后，只说明样本中的时间先后"],
  ["granger", "Granger 预测检验", "检验历史值是否改善线性预测，不是结构性因果"],
];

const DEFAULT_CONFIG: QuantResearchConfig = {
  symbols: ["300308", "600519", "600036"],
  metric: "pearson",
  value_mode: "log_return",
  frequency: "daily",
  start_date: null,
  end_date: null,
  adjust: "qfq",
  lookback_bars: 750,
  missing_policy: "drop",
  rolling_window: 60,
  max_lag: 5,
  controls: [],
  bootstrap_reps: 199,
  permutation_reps: 199,
  alpha: 0.05,
  fdr_method: "benjamini_hochberg",
  max_pairs: 2000,
  max_observations_per_pair: 500,
  seed: 42,
  oos_ratio: 0.3,
};

const finite = (value: number | null | undefined, digits = 4) =>
  typeof value === "number" && Number.isFinite(value) ? value.toFixed(digits) : "暂无";

const etaText = (seconds: number | null) => {
  if (seconds == null) return "正在根据实际速度估算";
  if (seconds < 60) return `约 ${seconds} 秒`;
  if (seconds < 3600) return `约 ${Math.ceil(seconds / 60)} 分钟`;
  return `约 ${(seconds / 3600).toFixed(1)} 小时`;
};

const parseCodes = (text: string) =>
  [...new Set(text.split(/[\s,，;；]+/).map((value) => value.trim()).filter(Boolean))];

function ProgressCard({ job, onCancel }: { job: QuantResearchJob; onCancel: () => void }) {
  const [expanded, setExpanded] = useState(true);
  const diagnosis = JSON.stringify(job, null, 2);
  return (
    <section className="card shrink-0 overflow-hidden text-xs">
      <button
        type="button"
        className="flex w-full items-center gap-3 px-3 py-2 text-left hover:bg-slate-50 dark:hover:bg-slate-900"
        onClick={() => setExpanded((value) => !value)}
      >
        <span className={`h-2 w-2 rounded-full ${job.running ? "animate-pulse bg-blue-500" : job.error ? "bg-red-500" : "bg-emerald-500"}`} />
        <b>{job.phase}</b>
        <span className="num muted">{job.progress}%</span>
        <span className="muted">
          {job.current_pair ? `${job.current_pair[0]} ↔ ${job.current_pair[1]}` : "等待下一统计单元"}
        </span>
        <span className="muted ml-auto">{etaText(job.estimated_remaining_seconds)} · {expanded ? "收起详情" : "展开详情"}</span>
      </button>
      <div className="h-1 bg-slate-200 dark:bg-slate-800">
        <div className="h-full bg-blue-500 transition-all" style={{ width: `${job.progress}%` }} />
      </div>
      {expanded && (
        <div className="grid gap-3 border-t border-slate-200 p-3 dark:border-slate-800 lg:grid-cols-[minmax(0,1fr)_minmax(260px,0.7fr)]">
          <div>
            <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
              {[
                ["已获取行情", `${job.fetched_series}/${job.total_series} 只`],
                ["已检验关系", `${job.done_pairs}/${job.total_pairs || "待计算"}`],
                ["当前有效样本", job.effective_observations || "待计算"],
                ["任务状态", job.status === "running" ? "后台运行中" : job.status === "completed" ? "已完成" : job.status === "cancelled" ? "已取消" : "失败"],
              ].map(([label, value]) => (
                <div key={String(label)} className="rounded bg-slate-50 p-2 dark:bg-slate-900">
                  <div className="micro-label">{label}</div>
                  <div className="num mt-1 text-sm">{value}</div>
                </div>
              ))}
            </div>
            <div className="mt-2 flex flex-wrap gap-2">
              {job.running && <button type="button" className="btn-danger" onClick={onCancel}>安全取消后台研究</button>}
              <button type="button" className="btn" onClick={() => void navigator.clipboard.writeText(diagnosis)}>复制完整任务诊断</button>
              {job.error && <button type="button" className="btn" onClick={() => void navigator.clipboard.writeText(job.error!)}>复制错误</button>}
            </div>
            {job.error && <div className="mt-2 break-all rounded bg-red-500/10 p-2 text-red-600 dark:text-red-300">{job.error}</div>}
          </div>
          <div className="max-h-36 overflow-auto rounded bg-slate-950 p-2 font-mono text-[11px] text-slate-300">
            {job.recent_logs.map((log, index) => <div key={`${index}-${log}`} className="py-0.5">{index + 1}. {log}</div>)}
          </div>
        </div>
      )}
    </section>
  );
}

function ResultDetail({ result }: { result: QuantPairInference }) {
  const option = useMemo<EChartsOption>(() => ({
    backgroundColor: "transparent",
    tooltip: { trigger: "axis" },
    grid: { left: 90, right: 20, top: 15, bottom: 35 },
    xAxis: { type: "category", data: result.stability_slices.map((slice) => slice.label), axisLabel: { rotate: 24, fontSize: 9 } },
    yAxis: { type: "value", scale: true, name: result.effect_name },
    series: [{
      type: "line",
      data: result.stability_slices.map((slice) => slice.effect),
      smooth: false,
      symbolSize: 6,
      markLine: { silent: true, data: [{ yAxis: result.effect, name: "全样本" }, { yAxis: 0 }] },
    }],
  }), [result]);
  return (
    <div className="space-y-3 p-3 text-xs">
      <div className="rounded border border-blue-200 bg-blue-50 p-3 leading-relaxed text-blue-900 dark:border-blue-900 dark:bg-blue-950/30 dark:text-blue-200">
        {result.conclusion}
      </div>
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
        <div><div className="micro-label">效应量</div><b className="num text-base">{finite(result.effect)}</b></div>
        <div><div className="micro-label">95% 区间</div><b className="num">[{finite(result.confidence_low)}, {finite(result.confidence_high)}]</b></div>
        <div><div className="micro-label">原始 p 值</div><b className="num">{finite(result.p_value)}</b></div>
        <div><div className="micro-label">校正后 p 值</div><b className="num">{finite(result.adjusted_p_value)}</b></div>
      </div>
      <Chart option={option} height={220} />
      <div className="grid gap-2 md:grid-cols-2">
        <div className="rounded border border-slate-200 p-2 dark:border-slate-800">
          <b>推断口径</b>
          <div className="muted mt-1 leading-relaxed">{result.confidence_method}；{result.p_value_method}</div>
          <div className="muted mt-1">有效样本 N={result.effective_n}{result.controls_used.length ? `；控制变量 ${result.controls_used.join("、")}` : ""}</div>
        </div>
        <div className="rounded border border-slate-200 p-2 dark:border-slate-800">
          <b>解释边界</b>
          <div className="muted mt-1 leading-relaxed">{result.interpretation}</div>
          <div className="muted mt-1">{result.stability.assessment}</div>
        </div>
      </div>
      {!!result.warnings.length && <div className="rounded bg-amber-500/10 p-2 text-amber-700 dark:text-amber-300">{result.warnings.join("；")}</div>}
    </div>
  );
}

export default function QuantResearchWorkbench() {
  const [config, setConfig] = useState<QuantResearchConfig>(DEFAULT_CONFIG);
  const [symbolsText, setSymbolsText] = useState(DEFAULT_CONFIG.symbols.join("，"));
  const [controlsText, setControlsText] = useState("");
  const [job, setJob] = useState<QuantResearchJob | null>(null);
  const [snapshot, setSnapshot] = useState<QuantResearchSnapshot | null>(null);
  const [history, setHistory] = useState<QuantSnapshotListItem[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [showHistory, setShowHistory] = useState(false);
  const [significance, setSignificance] = useState<"all" | "passed" | "failed">("all");
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<"effect" | "p" | "stability">("p");
  const [page, setPage] = useState(1);
  const [selected, setSelected] = useState<QuantPairInference | null>(null);

  useEffect(() => {
    void quantResearchStatus(null).then((latest) => {
      if (latest) {
        setJob(latest);
        if (latest.result) setSnapshot(latest.result);
      }
    }).catch(() => undefined);
    void quantResearchSnapshotList().then(setHistory).catch(() => undefined);
  }, []);

  useEffect(() => {
    if (!job?.running) return;
    const timer = window.setInterval(() => {
      void quantResearchStatus(job.job_id).then((latest) => {
        if (!latest) return;
        setJob(latest);
        if (latest.result) {
          setSnapshot(latest.result);
          setSelected(latest.result.results[0] ?? null);
          void quantResearchSnapshotList().then(setHistory);
        }
      }).catch((cause) => setError(errMsg(cause)));
    }, 600);
    return () => window.clearInterval(timer);
  }, [job?.job_id, job?.running]);

  const start = async () => {
    const symbols = parseCodes(symbolsText);
    if (symbols.length < 2) {
      setError("至少输入两只股票，多个代码可用逗号或空格分隔");
      return;
    }
    const next = { ...config, symbols, controls: parseCodes(controlsText) };
    setError(null);
    setSelected(null);
    try {
      const created = await quantResearchStart(next);
      setConfig(next);
      setJob(created);
    } catch (cause) {
      setError(errMsg(cause));
    }
  };

  const cancel = async () => {
    if (!job) return;
    try {
      await quantResearchCancel(job.job_id);
      const latest = await quantResearchStatus(job.job_id);
      if (latest) setJob(latest);
    } catch (cause) {
      setError(errMsg(cause));
    }
  };

  const openSnapshot = async (item: QuantSnapshotListItem) => {
    setError(null);
    try {
      const loaded = await quantResearchSnapshotGet(item.snapshot_id);
      if (!loaded) throw new Error("本地快照不存在");
      setSnapshot(loaded);
      setConfig(loaded.config);
      setSymbolsText(loaded.config.symbols.join("，"));
      setControlsText(loaded.config.controls.join("，"));
      setSelected(loaded.results[0] ?? null);
      setShowHistory(false);
    } catch (cause) {
      setError(errMsg(cause));
    }
  };

  const filtered = useMemo(() => {
    if (!snapshot) return [];
    const needle = query.trim().toLowerCase();
    const rows = snapshot.results.filter((row) => {
      if (significance === "passed" && row.significant_after_correction !== true) return false;
      if (significance === "failed" && row.significant_after_correction !== false) return false;
      return !needle || row.left.toLowerCase().includes(needle) || row.right.toLowerCase().includes(needle);
    });
    return [...rows].sort((a, b) => {
      if (sort === "effect") return Math.abs(b.effect) - Math.abs(a.effect);
      if (sort === "stability") return (b.stability.same_direction_rate ?? -1) - (a.stability.same_direction_rate ?? -1);
      return (a.adjusted_p_value ?? 2) - (b.adjusted_p_value ?? 2);
    });
  }, [snapshot, query, significance, sort]);
  const pageSize = 20;
  const pages = Math.max(1, Math.ceil(filtered.length / pageSize));
  const pageRows = filtered.slice((Math.min(page, pages) - 1) * pageSize, Math.min(page, pages) * pageSize);

  const overviewOption = useMemo<EChartsOption>(() => {
    const rows = filtered.slice(0, 20).reverse();
    return {
      backgroundColor: "transparent",
      tooltip: {
        trigger: "axis",
        formatter: (raw) => {
          const params = Array.isArray(raw) ? raw[0] : raw;
          const row = rows[Number((params as { dataIndex?: number }).dataIndex ?? -1)];
          return row ? `<b>${row.left}${row.directed ? " → " : " ↔ "}${row.right}</b><br/>效应 ${finite(row.effect)}<br/>95% 区间 [${finite(row.confidence_low)}, ${finite(row.confidence_high)}]<br/>校正后 p=${finite(row.adjusted_p_value)}` : "";
        },
      },
      grid: { left: 105, right: 24, top: 12, bottom: 28 },
      xAxis: { type: "value", scale: true },
      yAxis: { type: "category", data: rows.map((row) => `${row.left}${row.directed ? "→" : "↔"}${row.right}`), axisLabel: { fontSize: 9 } },
      series: [{
        type: "bar",
        data: rows.map((row) => ({ value: row.effect, itemStyle: { color: row.significant_after_correction ? "#2563eb" : "#94a3b8" } })),
        markLine: { silent: true, data: [{ xAxis: 0 }] },
      }],
    };
  }, [filtered]);

  const metric = METRICS.find(([value]) => value === config.metric)!;
  return (
    <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-auto pb-1">
      <section className="card shrink-0 p-3 text-xs">
        <div className="flex flex-wrap items-end gap-2">
          <label className="min-w-72 flex-1">
            <span className="muted mb-1 block">股票池（2-50 只）</span>
            <input className="input w-full" value={symbolsText} onChange={(event) => setSymbolsText(event.target.value)} placeholder="如 300308，600519，600036" />
          </label>
          <label>
            <span className="muted mb-1 block">研究指标</span>
            <select className="input w-40" value={config.metric} onChange={(event) => setConfig({ ...config, metric: event.target.value as QuantMetric })}>
              {METRICS.map(([value, label]) => <option key={value} value={value}>{label}</option>)}
            </select>
          </label>
          <label>
            <span className="muted mb-1 block">数据口径</span>
            <select className="input" value={config.value_mode} onChange={(event) => setConfig({ ...config, value_mode: event.target.value as QuantResearchConfig["value_mode"] })}>
              <option value="log_return">对数收益率（推荐）</option><option value="arithmetic_return">普通收益率</option><option value="price_level">价格水平（谨慎）</option>
            </select>
          </label>
          <button type="button" className="btn" onClick={() => setShowAdvanced((value) => !value)}>{showAdvanced ? "收起高级参数" : "高级参数"}</button>
          <button type="button" className="btn" onClick={() => setShowHistory((value) => !value)}>历史快照 {history.length}</button>
          <button type="button" className="btn-primary" disabled={job?.running} onClick={start}>{job?.running ? "后台研究中…" : "开始后台研究"}</button>
        </div>
        <div className="muted mt-2">{metric[2]}。所有随机过程使用固定种子；智能助手与本界面共用同一研究引擎。</div>
        {showAdvanced && (
          <div className="mt-3 grid gap-2 border-t border-slate-200 pt-3 dark:border-slate-800 sm:grid-cols-2 lg:grid-cols-5">
            <label><span className="muted mb-1 block">频率</span><select className="input w-full" value={config.frequency} onChange={(event) => setConfig({ ...config, frequency: event.target.value as QuantResearchConfig["frequency"] })}><option value="daily">日频</option><option value="weekly">周频</option><option value="monthly">月频</option></select></label>
            <label><span className="muted mb-1 block">复权</span><select className="input w-full" value={config.adjust} onChange={(event) => setConfig({ ...config, adjust: event.target.value as QuantResearchConfig["adjust"] })}><option value="qfq">前复权</option><option value="hfq">后复权</option><option value="none">不复权</option></select></label>
            <label><span className="muted mb-1 block">回看日线根数</span><input className="input w-full" type="number" min={60} max={2000} value={config.lookback_bars} onChange={(event) => setConfig({ ...config, lookback_bars: Number(event.target.value) })} /></label>
            <label><span className="muted mb-1 block">开始日期（可选）</span><input className="input w-full" type="date" value={config.start_date ?? ""} onChange={(event) => setConfig({ ...config, start_date: event.target.value || null })} /></label>
            <label><span className="muted mb-1 block">结束日期（可选）</span><input className="input w-full" type="date" value={config.end_date ?? ""} onChange={(event) => setConfig({ ...config, end_date: event.target.value || null })} /></label>
            <label><span className="muted mb-1 block">缺失值</span><select className="input w-full" value={config.missing_policy} onChange={(event) => setConfig({ ...config, missing_policy: event.target.value as QuantResearchConfig["missing_policy"] })}><option value="drop">按共同日期剔除</option><option value="forward_fill">向前填充</option><option value="zero">填零（谨慎）</option></select></label>
            <label><span className="muted mb-1 block">滚动窗口</span><input className="input w-full" type="number" min={10} max={500} value={config.rolling_window} onChange={(event) => setConfig({ ...config, rolling_window: Number(event.target.value) })} /></label>
            <label><span className="muted mb-1 block">最大滞后</span><input className="input w-full" type="number" min={1} max={20} value={config.max_lag} onChange={(event) => setConfig({ ...config, max_lag: Number(event.target.value) })} /></label>
            <label><span className="muted mb-1 block">控制变量代码</span><input className="input w-full" value={controlsText} onChange={(event) => setControlsText(event.target.value)} placeholder="仅偏相关使用" /></label>
            <label><span className="muted mb-1 block">多重检验</span><select className="input w-full" value={config.fdr_method} onChange={(event) => setConfig({ ...config, fdr_method: event.target.value as QuantResearchConfig["fdr_method"] })}><option value="benjamini_hochberg">BH-FDR（推荐）</option><option value="bonferroni">Bonferroni</option><option value="none">不校正（不推荐）</option></select></label>
            <label><span className="muted mb-1 block">Bootstrap 次数</span><input className="input w-full" type="number" min={99} max={4999} step={100} value={config.bootstrap_reps} onChange={(event) => setConfig({ ...config, bootstrap_reps: Number(event.target.value) })} /></label>
            <label><span className="muted mb-1 block">置换次数</span><input className="input w-full" type="number" min={99} max={4999} step={100} value={config.permutation_reps} onChange={(event) => setConfig({ ...config, permutation_reps: Number(event.target.value) })} /></label>
            <label><span className="muted mb-1 block">单对最大样本</span><input className="input w-full" type="number" min={30} max={2000} value={config.max_observations_per_pair} onChange={(event) => setConfig({ ...config, max_observations_per_pair: Number(event.target.value) })} /></label>
            <label><span className="muted mb-1 block">最大关系数</span><input className="input w-full" type="number" min={1} max={10000} value={config.max_pairs} onChange={(event) => setConfig({ ...config, max_pairs: Number(event.target.value) })} /></label>
            <label><span className="muted mb-1 block">固定随机种子</span><input className="input w-full" type="number" min={0} value={config.seed} onChange={(event) => setConfig({ ...config, seed: Number(event.target.value) })} /></label>
          </div>
        )}
        {showHistory && (
          <div className="mt-3 max-h-44 overflow-auto border-t border-slate-200 pt-2 dark:border-slate-800">
            {history.length ? history.map((item) => (
              <button type="button" key={item.snapshot_id} className="flex w-full items-center gap-3 rounded px-2 py-1.5 text-left hover:bg-slate-100 dark:hover:bg-slate-900" onClick={() => void openSnapshot(item)}>
                <span className="num">{new Date(item.created_at * 1000).toLocaleString()}</span><span>{item.metric}</span><span className="muted truncate">{item.symbols.join("、")}</span><span className="num muted ml-auto max-w-52 truncate">{item.snapshot_id}</span>
              </button>
            )) : <div className="muted py-3 text-center">暂无已保存研究快照</div>}
          </div>
        )}
      </section>

      {error && <ErrorBox message={error} />}
      {job && <ProgressCard job={job} onCancel={() => void cancel()} />}

      {snapshot ? (
        <>
          <section className="card shrink-0 p-3 text-xs">
            <div className="flex flex-wrap items-start gap-3">
              <div className="mr-auto"><b>研究快照</b><div className="num muted mt-1">{snapshot.snapshot_id}</div></div>
              <div><div className="micro-label">关系数量</div><b className="num">{snapshot.results.length}/{snapshot.budget.requested_pairs}</b></div>
              <div><div className="micro-label">复杂度</div><b>{snapshot.budget.complexity}</b></div>
              <div><div className="micro-label">预估运算量</div><b className="num">{snapshot.budget.estimated_operations.toLocaleString()}</b></div>
              <button type="button" className="btn" onClick={() => void navigator.clipboard.writeText(JSON.stringify(snapshot, null, 2))}>复制完整快照</button>
            </div>
            <div className="muted mt-2">{snapshot.budget.explanation}</div>
            <div className="mt-2 rounded bg-amber-500/10 p-2 text-amber-800 dark:text-amber-300"><b>因果解释边界：</b>{snapshot.causality_boundary}</div>
          </section>

          <div className="grid min-h-[320px] shrink-0 gap-3 lg:grid-cols-[minmax(0,1fr)_minmax(360px,0.8fr)]">
            <section className="card overflow-hidden">
              <div className="card-title">效应量总览 <span className="muted ml-auto text-[10px]">蓝色=FDR 校正后显著，灰色=未通过</span></div>
              <Chart option={overviewOption} height={280} />
            </section>
            <section className="card overflow-auto">
              <div className="card-title">{selected ? `${selected.left}${selected.directed ? " → " : " ↔ "}${selected.right}` : "点击结果查看稳健性"}</div>
              {selected ? <ResultDetail result={selected} /> : <div className="muted p-8 text-center">从下方表格选择一个关系</div>}
            </section>
          </div>

          <section className="card min-h-[360px] shrink-0 overflow-hidden">
            <div className="flex flex-wrap items-end gap-2 border-b border-slate-200 p-3 text-xs dark:border-slate-800">
              <div className="mr-auto"><b>全部关系检验</b><div className="muted mt-1">同时展示效应量、区间、原始/校正后显著性、有效样本和跨窗口稳定性</div></div>
              <input className="input w-36" value={query} onChange={(event) => { setQuery(event.target.value); setPage(1); }} placeholder="筛选证券代码" />
              <select className="input" value={significance} onChange={(event) => { setSignificance(event.target.value as typeof significance); setPage(1); }}><option value="all">全部显著性</option><option value="passed">FDR 后显著</option><option value="failed">FDR 后不显著</option></select>
              <select className="input" value={sort} onChange={(event) => setSort(event.target.value as typeof sort)}><option value="p">按校正后 p 值</option><option value="effect">按效应绝对值</option><option value="stability">按方向稳定性</option></select>
            </div>
            <div className="overflow-x-auto">
              <table className="w-full border-collapse text-xs">
                <thead><tr className="bg-slate-50 dark:bg-slate-900"><th className="th">关系</th><th className="th"><Term label="效应量" tip="指标的实际关系强度；显著不等于效应足够大" /></th><th className="th">95% 区间</th><th className="th">原始 p</th><th className="th"><Term label="校正后 p" tip="对本次全部关系统一做多重检验，避免只挑偶然显著结果" /></th><th className="th">有效 N</th><th className="th">跨窗口方向</th><th className="th">结论</th></tr></thead>
                <tbody>{pageRows.map((row) => (
                  <tr key={`${row.left}-${row.right}`} className={`cursor-pointer border-t border-slate-100 hover:bg-blue-50 dark:border-slate-800 dark:hover:bg-blue-950/20 ${selected === row ? "bg-blue-50 dark:bg-blue-950/20" : ""}`} onClick={() => setSelected(row)}>
                    <td className="td font-medium">{row.left}{row.directed ? " → " : " ↔ "}{row.right}{row.best_lag != null && <div className="muted text-[10px]">最佳滞后 {row.best_lag} 期</div>}</td>
                    <td className="td num">{finite(row.effect)}</td>
                    <td className="td num">[{finite(row.confidence_low)}, {finite(row.confidence_high)}]</td>
                    <td className="td num">{finite(row.p_value)}</td>
                    <td className="td"><span className={`chip ${row.significant_after_correction ? "bg-blue-500/15 text-blue-600" : "bg-slate-500/10 text-slate-500"}`}>{finite(row.adjusted_p_value)} · {row.significant_after_correction ? "通过" : "未通过"}</span></td>
                    <td className="td num">{row.effective_n}</td>
                    <td className="td num">{row.stability.same_direction_rate == null ? "看幅度切片" : `${(row.stability.same_direction_rate * 100).toFixed(0)}%`}</td>
                    <td className="td max-w-96 whitespace-normal text-[11px] leading-relaxed">{row.stability.assessment}</td>
                  </tr>
                ))}</tbody>
              </table>
            </div>
            <div className="flex items-center justify-between border-t border-slate-200 px-3 py-2 text-xs dark:border-slate-800"><span className="muted">共 {filtered.length} 条 · 第 {Math.min(page, pages)}/{pages} 页</span><div className="flex gap-1"><button className="btn" disabled={page <= 1} onClick={() => setPage((value) => Math.max(1, value - 1))}>上一页</button><button className="btn" disabled={page >= pages} onClick={() => setPage((value) => Math.min(pages, value + 1))}>下一页</button></div></div>
          </section>
        </>
      ) : !job?.running && (
        <section className="card flex min-h-72 shrink-0 flex-col items-center justify-center p-8 text-center">
          <div className="text-sm font-semibold">从可复现研究问题开始</div>
          <div className="muted mt-2 max-w-2xl text-xs leading-relaxed">系统不会只展示“看起来最好”的关系。每个结论都会同时给出效应量、置信区间、多重检验校正、有效样本、年度/市场状态/滚动窗口/异常值/参数/样本外稳定性，并保存数据和函数版本。</div>
        </section>
      )}
    </div>
  );
}
