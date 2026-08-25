import { useEffect, useMemo, useRef, useState } from "react";
import type * as echarts from "echarts";
import {
  backtestCancel,
  backtestStart,
  backtestStatus,
  errMsg,
  listStrategies,
  searchStocks,
  type BacktestJobSnapshot as ApiBacktestJobSnapshot,
  type SearchResult,
} from "../lib/api";
import Chart from "../components/Chart";
import { ErrorBox, LoadBar, Term } from "../components/ui";

/**
 * 回测实验室(纯前端页)。
 * 契约:docs/command-contract.md `run_backtest`;`list_strategies` 由后端并行
 * 补契约,前端 try/catch 优雅降级为内置策略清单(与 crates/backtest 注册表对齐)。
 * 长耗时回测统一在后端任务中运行，离开页面后仍可恢复状态与结果。
 */

// ==================== 类型(与 run_backtest_json 返回对齐) ====================

interface StrategyMeta {
  name: string;
  label: string;
  description: string;
  multi_symbol: boolean;
}

interface BacktestTrade {
  date: string;
  side: "buy" | "sell";
  shares: number;
  price: number;
  amount: number;
  fees: number;
  reason: string;
}

interface BacktestJson {
  symbol: string;
  strategy: string;
  params: Record<string, unknown>;
  data: { start: string; end: string; bars: number };
  initial_cash: number;
  final_equity: number;
  total_return: number;
  cagr: number;
  annualized_volatility: number;
  sharpe: number;
  sortino: number;
  calmar: number;
  max_drawdown: number;
  max_drawdown_duration_bars: number;
  round_trips: number;
  hit_rate: number;
  payoff_ratio: number;
  profit_factor: number;
  trades_count: number;
  rejections: number;
  fees_total: number;
  equity_curve: [string, number][];
  trades_tail: BacktestTrade[];
  note?: string;
}

type BacktestJobSnapshot = ApiBacktestJobSnapshot<BacktestJson>;

// ==================== 策略清单与参数定义 ====================

/** 内置清单:list_strategies 命令未就绪时的降级(与 crates/backtest 注册表 + 现有 run_backtest 对齐) */
const BUILTIN_STRATEGIES: StrategyMeta[] = [
  {
    name: "ma_cross",
    label: "双均线交叉",
    description: "金叉全仓买、死叉清仓,fast/slow 窗口可调(单标的)",
    multi_symbol: false,
  },
  {
    name: "turtle",
    label: "海龟突破",
    description: "entry_n 日新高买入、exit_n 日新低离场(单标的)",
    multi_symbol: false,
  },
  {
    name: "buy_hold",
    label: "买入持有",
    description: "基准策略:首日满仓买入并持有到期(单标的)",
    multi_symbol: false,
  },
  {
    name: "zscore_mean_reversion",
    label: "Zscore 均值回归",
    description: "close−MA 的 z 分数,≤入场阈值满仓买、≥出场阈值清仓(单标的,需新版后端)",
    multi_symbol: false,
  },
  {
    name: "formula_dsl",
    label: "AI 公式策略",
    description: "由 AI 或用户组合历史价格、均线、区间高低点与 RSI；禁止任意代码、文件和网络访问",
    multi_symbol: false,
  },
  {
    name: "min_corr_etf_rotation",
    label: "最小相关轮动",
    description: "候选池两两相关矩阵,持有平均相关最低的 N 只等权,月度再平衡(多标的,需新版后端)",
    multi_symbol: true,
  },
];

interface ParamField {
  key: string;
  label: string;
  tip?: string;
  /** int 取整,float 保留小数 */
  kind: "int" | "float";
  def: number;
  min?: number;
  max?: number;
  step?: number;
}

/** 各策略的预定义参数字段;未列出的策略走通用 JSON 参数编辑 */
const PARAM_FIELDS: Record<string, ParamField[]> = {
  ma_cross: [
    { key: "fast", label: "快线窗口", kind: "int", def: 5, min: 1, max: 250, tip: "短期简单移动平均线天数，至少 1 天且小于慢线" },
    { key: "slow", label: "慢线窗口", kind: "int", def: 20, min: 2, max: 500, tip: "长期简单移动平均线天数，须大于快线" },
  ],
  turtle: [
    { key: "entry_n", label: "入场窗口 N", kind: "int", def: 20, min: 2, max: 250, tip: "突破最近 N 日新高买入,N >= 2" },
    { key: "exit_n", label: "离场窗口 N", kind: "int", def: 10, min: 1, max: 250, tip: "跌破最近 N 日新低离场,N >= 1" },
  ],
  zscore_mean_reversion: [
    { key: "ma_window", label: "均线窗口", kind: "int", def: 20, min: 1, max: 250, tip: "sub = close − MA(close, ma_window)" },
    { key: "z_window", label: "Z 分数窗口", kind: "int", def: 60, min: 2, max: 500, tip: "z 分数的滚动统计窗口,>= 2" },
    { key: "entry_z", label: "入场阈值", kind: "float", def: -2, min: -10, max: 0, step: 0.1, tip: "z ≤ 入场阈值时买入,须为负" },
    { key: "exit_z", label: "出场阈值", kind: "float", def: 1, min: 0, max: 10, step: 0.1, tip: "z ≥ 出场阈值时清仓,须大于入场阈值" },
  ],
  min_corr_etf_rotation: [
    { key: "lookback", label: "相关窗口", kind: "int", def: 60, min: 20, max: 500, tip: "计算相关系数的回看交易日数" },
    { key: "hold_n", label: "持仓只数", kind: "int", def: 4, min: 1, max: 12, tip: "持有平均相关最低的前 N 只" },
  ],
  buy_hold: [],
};

const FORMULA_TEMPLATE = JSON.stringify(
  {
    version: 1,
    name: "短期均线上穿长期均线",
    entry: {
      op: "cross_above",
      left: { kind: "sma", field: "close", window: 5 },
      right: { kind: "sma", field: "close", window: 20 },
    },
    exit: {
      op: "cross_below",
      left: { kind: "sma", field: "close", window: 5 },
      right: { kind: "sma", field: "close", window: 20 },
    },
  },
  null,
  2,
);

// ==================== 工具 ====================

const fmtPct = (v: number | null | undefined) => (v == null ? "—" : `${(v * 100).toFixed(2)}%`);
const fmt2 = (v: number | null | undefined) => (v == null ? "—" : v.toFixed(2));
const fmtMoney = (v: number | null | undefined) =>
  v == null ? "—" : v.toLocaleString("zh-CN", { maximumFractionDigits: 0 });

const toISODate = (d: Date) => d.toISOString().slice(0, 10);

/** 日期范围 → 约算交易日 bars(每周 5 个交易日,后端 clamp 60..2000) */
function rangeToBars(start: string, end: string): number {
  const ms = new Date(end).getTime() - new Date(start).getTime();
  const days = Math.max(1, Math.round(ms / 86_400_000));
  return Math.min(2000, Math.max(60, Math.round((days * 5) / 7)));
}

/** 股票代码搜索联想(交互与 Layout 顶部搜索一致) */
function SymbolPicker({
  value,
  name,
  onPick,
  disabled,
}: {
  value: string;
  name: string;
  onPick: (code: string, name: string) => void;
  disabled?: boolean;
}) {
  const [kw, setKw] = useState(value ? `${name ? name + " " : ""}${value}` : "");
  const [results, setResults] = useState<SearchResult[]>([]);
  const [open, setOpen] = useState(false);
  const boxRef = useRef<HTMLDivElement>(null);
  const timer = useRef<ReturnType<typeof setTimeout>>();

  useEffect(() => {
    const onClick = (e: MouseEvent) => {
      if (boxRef.current && !boxRef.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onClick);
    return () => document.removeEventListener("mousedown", onClick);
  }, []);

  const onChange = (v: string) => {
    setKw(v);
    clearTimeout(timer.current);
    // 手输 6 位代码也可直接用
    if (/^\d{6}$/.test(v.trim())) onPick(v.trim(), "");
    if (!v.trim()) {
      setResults([]);
      setOpen(false);
      return;
    }
    timer.current = setTimeout(async () => {
      try {
        const r = await searchStocks(v.trim());
        setResults(r.slice(0, 10));
        setOpen(true);
      } catch {
        setResults([]);
      }
    }, 250);
  };

  return (
    <div ref={boxRef} className="relative">
      <input
        className="input w-44 text-xs"
        placeholder="代码 / 名称搜索"
        value={kw}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && results.length > 0) {
            onPick(results[0].code, results[0].name);
            setKw(`${results[0].name} ${results[0].code}`);
            setOpen(false);
          }
        }}
      />
      {open && results.length > 0 && (
        <div className="card anim-fade-in absolute left-0 top-full z-50 mt-1 max-h-64 w-64 overflow-auto py-1 shadow-lg">
          {results.map((r) => (
            <button
              key={r.code}
              className="flex w-full items-center justify-between px-3 py-1.5 text-left text-xs hover:bg-slate-100 dark:hover:bg-slate-800"
              onClick={() => {
                onPick(r.code, r.name);
                setKw(`${r.name} ${r.code}`);
                setOpen(false);
              }}
            >
              <span>{r.name}</span>
              <span className="num muted">{r.code}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

// ==================== 页面 ====================

export default function BacktestLabPage() {
  const today = useMemo(() => new Date(), []);
  const [strategies, setStrategies] = useState<StrategyMeta[]>(BUILTIN_STRATEGIES);
  const [remoteList, setRemoteList] = useState(false);
  const [strategy, setStrategy] = useState("ma_cross");
  const [symbol, setSymbol] = useState("600519");
  const [symbolName, setSymbolName] = useState("");
  const [startDate, setStartDate] = useState(() => {
    const d = new Date();
    d.setFullYear(d.getFullYear() - 3);
    return toISODate(d);
  });
  const [endDate, setEndDate] = useState(() => toISODate(new Date()));
  const [initialCash, setInitialCash] = useState(1_000_000);
  const [paramVals, setParamVals] = useState<Record<string, string>>({});
  const [paramsJson, setParamsJson] = useState("{}");
  const [pool, setPool] = useState("510300, 510500, 159915, 518880");
  const [running, setRunning] = useState(false);
  const [elapsed, setElapsed] = useState(0);
  const [jobId, setJobId] = useState<string | null>(null);
  const [jobPhase, setJobPhase] = useState("尚未运行");
  const [result, setResult] = useState<BacktestJson | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const startRef = useRef(0);

  const applyJobSnapshot = (snapshot: BacktestJobSnapshot) => {
    setJobId(snapshot.job_id);
    setJobPhase(snapshot.phase);
    setRunning(snapshot.status === "running");
    if (snapshot.started_at != null) {
      startRef.current = snapshot.started_at;
      setElapsed(Math.max(0, (Date.now() - snapshot.started_at) / 1000));
    }
    if (snapshot.result) setResult(snapshot.result);
    if (snapshot.status === "failed") setErr(snapshot.error || "后台回测失败");
    if (snapshot.status === "cancelled") setErr(null);
  };

  // 回测运行于后端；离开页面再回来时恢复任务、阶段和结果。
  useEffect(() => {
    backtestStatus<BacktestJson>().then(applyJobSnapshot).catch(() => {});
  }, []);

  useEffect(() => {
    if (!running) return;
    const timer = setInterval(() => {
      backtestStatus<BacktestJson>().then(applyJobSnapshot).catch(() => {});
    }, 750);
    return () => clearInterval(timer);
  }, [running]);

  // list_strategies:契约补齐后优先后端清单;失败则保持内置清单降级
  useEffect(() => {
    listStrategies()
      .then((list) => {
        if (!Array.isArray(list) || list.length === 0) return;
        setStrategies(
          list.map((s) => {
            const builtin = BUILTIN_STRATEGIES.find((b) => b.name === s.name);
            return {
              name: s.name,
              label: builtin?.label ?? s.name,
              description: s.description ?? builtin?.description ?? "",
              multi_symbol: s.multi_symbol ?? builtin?.multi_symbol ?? false,
            };
          }),
        );
        setRemoteList(true);
      })
      .catch(() => {
        /* 命令未注册:保持内置清单 */
      });
  }, []);

  const meta = strategies.find((s) => s.name === strategy) ?? strategies[0];
  const fields = PARAM_FIELDS[strategy];

  // 切换策略时重置参数为该策略默认值
  useEffect(() => {
    if (fields) {
      const next: Record<string, string> = {};
      for (const f of fields) next[f.key] = String(f.def);
      setParamVals(next);
    } else {
      setParamVals({});
      setParamsJson(strategy === "formula_dsl" ? FORMULA_TEMPLATE : "{}");
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [strategy]);

  // 执行计时
  useEffect(() => {
    if (!running) return;
    const t = setInterval(() => setElapsed((Date.now() - startRef.current) / 1000), 200);
    return () => clearInterval(t);
  }, [running]);

  const poolList = pool
    .split(/[,\s]+/)
    .map((s) => s.trim())
    .filter(Boolean);

  const run = async () => {
    setErr(null);
    setRunning(true);
    setResult(null);
    setJobPhase("正在创建后台回测任务");
    setElapsed(0);
    startRef.current = Date.now();
    try {
      // 组装策略参数
      let params: Record<string, unknown> = {};
      if (fields) {
        for (const f of fields) {
          const raw = paramVals[f.key];
          const n = raw === undefined || raw === "" ? f.def : Number(raw);
          if (!Number.isFinite(n)) throw new Error(`参数「${f.label}」不是有效数字`);
          params[f.key] = f.kind === "int" ? Math.round(n) : n;
        }
      } else {
        try {
          params = JSON.parse(paramsJson || "{}");
        } catch {
          throw new Error("高级策略参数格式无法识别，请检查填写内容");
        }
      }

      const bars = rangeToBars(startDate, endDate);
      const sym = meta.multi_symbol ? poolList[0] ?? symbol : symbol;
      if (!/^\d{6}$/.test(sym)) throw new Error("请先选择有效的 6 位股票代码");
      if (meta.multi_symbol && poolList.length < 2)
        throw new Error("轮动策略需要至少 2 只候选标的(逗号分隔代码)");

      const args: Record<string, unknown> = {
        symbol: sym,
        strategy,
        bars,
        // 现有后端的扁平参数(未知键会被忽略,新版后端读取 params/pool)
        fast: params.fast,
        slow: params.slow,
        entry_n: params.entry_n,
        exit_n: params.exit_n,
        params,
        pool: meta.multi_symbol ? poolList : undefined,
      };
      const started = await backtestStart(args);
      setJobId(started.job_id);
      const snapshot = await backtestStatus<BacktestJson>();
      applyJobSnapshot(snapshot);
    } catch (e) {
      setErr(errMsg(e));
      setResult(null);
      setRunning(false);
    }
  };

  const cancel = async () => {
    setErr(null);
    try {
      await backtestCancel();
      const snapshot = await backtestStatus<BacktestJson>();
      applyJobSnapshot(snapshot);
    } catch (e) {
      setErr(errMsg(e));
    }
  };

  // 初始资金与后端固定值不同时,净值曲线按比例缩放(比率先行指标不受影响)
  const scale = result && result.initial_cash > 0 ? initialCash / result.initial_cash : 1;
  const scaled = Math.abs(scale - 1) > 1e-9;

  const chartOption = useMemo<echarts.EChartsOption | null>(() => {
    if (!result || result.equity_curve.length === 0) return null;
    const dates = result.equity_curve.map((p) => p[0]);
    const equity = result.equity_curve.map((p) => +(p[1] * scale).toFixed(2));
    let peak = -Infinity;
    const dd = equity.map((v) => {
      peak = Math.max(peak, v);
      return +((v / peak - 1) * 100).toFixed(2);
    });
    const lineColor = result.total_return >= 0 ? "#e5484d" : "#2eb872";
    return {
      animationDuration: 300,
      tooltip: { trigger: "axis" },
      axisPointer: { link: [{ xAxisIndex: "all" }] },
      grid: [
        { left: 64, right: 16, top: 16, height: "52%" },
        { left: 64, right: 16, top: "74%", height: "17%" },
      ],
      xAxis: [
        {
          type: "category",
          data: dates,
          gridIndex: 0,
          axisLabel: { show: false },
          axisTick: { show: false },
        },
        { type: "category", data: dates, gridIndex: 1, axisTick: { show: false } },
      ],
      yAxis: [
        {
          scale: true,
          gridIndex: 0,
          splitLine: { lineStyle: { color: "rgba(148,163,184,0.12)" } },
        },
        {
          gridIndex: 1,
          axisLabel: { formatter: "{value}%" },
          splitLine: { lineStyle: { color: "rgba(148,163,184,0.12)" } },
        },
      ],
      dataZoom: [{ type: "inside", xAxisIndex: [0, 1] }],
      series: [
        {
          name: "净值",
          type: "line",
          data: equity,
          xAxisIndex: 0,
          yAxisIndex: 0,
          showSymbol: false,
          lineStyle: { width: 1.5, color: lineColor },
          itemStyle: { color: lineColor },
          markLine: {
            silent: true,
            symbol: "none",
            lineStyle: { type: "dashed", color: "rgba(148,163,184,0.6)" },
            data: [{ yAxis: +(result.initial_cash * scale).toFixed(2) }],
          },
        },
        {
          name: "回撤",
          type: "line",
          data: dd,
          xAxisIndex: 1,
          yAxisIndex: 1,
          showSymbol: false,
          lineStyle: { width: 1, color: "#2eb872" },
          itemStyle: { color: "#2eb872" },
          areaStyle: { color: "rgba(46,184,114,0.25)" },
        },
      ],
    };
  }, [result, scale]);

  const metrics: Array<{ label: string; tip?: string; value: string; cls?: string }> = result
    ? [
        {
          label: "复合年化收益",
          tip: "复合年化收益率",
          value: fmtPct(result.cagr),
          cls: result.cagr >= 0 ? "text-up" : "text-down",
        },
        { label: "年化波动", tip: "日收益年化标准差", value: fmtPct(result.annualized_volatility) },
        { label: "夏普比率", tip: "每承担一单位总风险获得的超额收益", value: fmt2(result.sharpe) },
        { label: "索提诺比率", tip: "只计算下行风险的风险调整后收益", value: fmt2(result.sortino) },
        {
          label: "最大回撤",
          tip: `净值自高点的最大跌幅，持续 ${result.max_drawdown_duration_bars} 个交易周期`,
          value: fmtPct(-Math.abs(result.max_drawdown)),
          cls: "text-down",
        },
        { label: "胜率", tip: `${result.round_trips} 个完整回合中盈利回合占比`, value: fmtPct(result.hit_rate) },
        { label: "盈亏比", tip: "平均盈利 / 平均亏损", value: fmt2(result.payoff_ratio) },
        { label: "交易次数", tip: `含费用合计 ${fmtMoney(result.fees_total)} 元`, value: String(result.trades_count) },
      ]
    : [];

  return (
    <div className="flex h-full flex-col gap-3 overflow-y-auto p-3">
      <h1 className="shrink-0 text-sm font-semibold text-slate-700 dark:text-slate-300">回测实验室</h1>

      {/* 参数表单 */}
      <div className="card shrink-0 px-4 py-3">
        <div className="flex flex-wrap items-end gap-x-4 gap-y-3">
          <label className="flex flex-col gap-1">
            <span className="micro-label">股票代码</span>
            <SymbolPicker
              value={symbol}
              name={symbolName}
              disabled={meta.multi_symbol}
              onPick={(c, n) => {
                setSymbol(c);
                setSymbolName(n);
              }}
            />
          </label>
          <label className="flex flex-col gap-1">
            <span className="micro-label">策略</span>
            <select
              className="input w-44 text-xs"
              value={strategy}
              onChange={(e) => setStrategy(e.target.value)}
            >
              {strategies.map((s) => (
                <option key={s.name} value={s.name}>
                  {s.label}
                </option>
              ))}
            </select>
          </label>
          <label className="flex flex-col gap-1">
            <span className="micro-label">开始日期</span>
            <input
              type="date"
              className="input text-xs"
              value={startDate}
              max={endDate}
              onChange={(e) => setStartDate(e.target.value)}
            />
          </label>
          <label className="flex flex-col gap-1">
            <span className="micro-label">结束日期</span>
            <input
              type="date"
              className="input text-xs"
              value={endDate}
              min={startDate}
              max={toISODate(today)}
              onChange={(e) => setEndDate(e.target.value)}
            />
          </label>
          <label className="flex flex-col gap-1">
            <span className="micro-label">
              <Term label="初始资金" tip="后端当前固定 100 万;此处不同时净值曲线按比例缩放,比率先行指标不变" />
            </span>
            <input
              type="number"
              className="input w-32 text-xs"
              min={10000}
              step={10000}
              value={initialCash}
              onChange={(e) => setInitialCash(Math.max(1, Number(e.target.value) || 0))}
            />
          </label>
          {fields?.map((f) => (
            <label key={f.key} className="flex flex-col gap-1">
              <span className="micro-label">
                {f.tip ? <Term label={f.label} tip={f.tip} /> : f.label}
              </span>
              <input
                type="number"
                className="input w-24 text-xs"
                min={f.min}
                max={f.max}
                step={f.step ?? 1}
                value={paramVals[f.key] ?? String(f.def)}
                onChange={(e) => setParamVals((p) => ({ ...p, [f.key]: e.target.value }))}
              />
            </label>
          ))}
          {!fields && (
            <label className="flex min-w-80 flex-1 flex-col gap-1">
              <span className="micro-label">
                <Term
                  label={strategy === "formula_dsl" ? "可审计策略条件" : "高级策略参数"}
                  tip={
                    strategy === "formula_dsl"
                      ? "只允许历史行情、SMA、区间最高/最低、RSI 与布尔比较；深度、节点、窗口均有限制，不执行任意脚本"
                      : '该策略暂无预设输入项，可按“参数名与数值”的结构填写高级条件'
                  }
                />
              </span>
              <textarea
                className="input min-h-28 w-full resize-y font-mono text-[11px] leading-4"
                value={paramsJson}
                onChange={(e) => setParamsJson(e.target.value)}
                spellCheck={false}
              />
              {strategy === "formula_dsl" && (
                <span className="muted text-[11px]">
                  Agent 可在对话中调用「策略回测」自动生成并验证这类条件；结果仍需样本外与人工风控复核
                </span>
              )}
            </label>
          )}
          {meta.multi_symbol && (
            <label className="flex min-w-64 flex-1 flex-col gap-1">
              <span className="micro-label">
                <Term label="轮动池" tip="逗号/空格分隔的 6 位代码,至少 2 只;多标的轮动在池内选低相关组合" />
              </span>
              <input
                className="input w-full text-xs"
                value={pool}
                onChange={(e) => setPool(e.target.value)}
                placeholder="510300, 510500, 159915, 518880"
                spellCheck={false}
              />
            </label>
          )}
          <div className="flex items-center gap-2">
            <button className="btn-primary" onClick={run} disabled={running}>
              {running ? "回测中…" : "运行回测"}
            </button>
            <button
              className="btn-danger"
              disabled={!running}
              onClick={cancel}
              title="取消后台回测"
            >
              取消
            </button>
            {running && (
              <span className="muted text-xs">
                {jobPhase} · <span className="num">{elapsed.toFixed(1)}s</span>
                {jobId && <span className="num ml-2" title={jobId}>任务 {jobId.slice(-8)}</span>}
              </span>
            )}
          </div>
        </div>
        {meta.description && <div className="muted mt-2 text-xs">{meta.description}</div>}
        {!remoteList && (
          <div className="muted mt-1 text-[11px]">
            策略清单为内置(list_strategies 未就绪);zscore / 轮动策略需新版后端支持
          </div>
        )}
        {running && (
          <div className="mt-3">
            <LoadBar />
          </div>
        )}
      </div>

      {err && <ErrorBox message={err} onRetry={run} />}

      {/* 结果区 */}
      {!result && !err && (
        <div className="card muted py-10 text-center text-sm">
          选择策略与参数后点击「运行回测」;结果含净值曲线、绩效指标与最近 50 笔交易
        </div>
      )}

      {result && (
        <div className="stagger flex flex-col gap-3">
          {/* 概要带 */}
          <div className="card anim-fade-up px-4 py-3">
            <div className="flex flex-wrap items-center gap-x-5 gap-y-1 text-xs">
              <span>
                <span className="muted">标的 </span>
                <span className="num">{result.symbol}</span>
              </span>
              <span>
                <span className="muted">策略 </span>
                {strategies.find((s) => s.name === result.strategy)?.label ?? result.strategy}
              </span>
              <span>
                <span className="muted">区间 </span>
                <span className="num">
                  {result.data.start} ~ {result.data.end}
                </span>
                <span className="muted">({result.data.bars} 根日线)</span>
              </span>
              <span>
                <span className="muted">期末权益 </span>
                <span className="num">{fmtMoney(result.final_equity * scale)}</span>
              </span>
              <span>
                <span className="muted">总收益 </span>
                <span className={"num font-semibold " + (result.total_return >= 0 ? "text-up" : "text-down")}>
                  {fmtPct(result.total_return)}
                </span>
              </span>
              {result.rejections > 0 && (
                <span className="muted">被拒单 {result.rejections} 笔(涨跌停/流动性规则)</span>
              )}
              {scaled && <span className="muted">净值已按初始资金 {fmtMoney(initialCash)} 等比缩放</span>}
            </div>
          </div>

          {/* 净值 + 回撤 */}
          <div className="card anim-fade-up">
            <div className="card-title">
              净值曲线
              <span className="muted text-xs font-normal">上:权益(虚线为初始资金);下:回撤%(滚轮缩放)</span>
            </div>
            {chartOption && <Chart option={chartOption} height={360} />}
          </div>

          {/* 指标卡片 */}
          <div className="stat-band anim-fade-up grid-cols-2 sm:grid-cols-4">
            {metrics.map((m) => (
              <div key={m.label} className="stat-cell" title={m.tip}>
                <div className="micro-label">{m.label}</div>
                <div className={"num mt-1 text-lg font-semibold leading-tight " + (m.cls ?? "")}>
                  {m.value}
                </div>
              </div>
            ))}
          </div>

          {/* 交易明细 */}
          <div className="card anim-fade-up flex flex-col overflow-hidden">
            <div className="card-title shrink-0">
              交易明细
              <span className="muted text-xs font-normal">
                最近 {result.trades_tail.length} 笔(共 {result.trades_count} 笔,后端仅返回尾部 50 笔)
              </span>
            </div>
            {result.trades_tail.length === 0 ? (
              <div className="muted py-6 text-center text-sm">区间内无成交</div>
            ) : (
              <div className="max-h-80 overflow-auto">
                <table className="w-full border-collapse">
                  <thead className="sticky top-0 z-10 bg-white dark:bg-slate-900">
                    <tr className="border-b border-slate-200 dark:border-slate-800">
                      <th className="th">日期</th>
                      <th className="th">方向</th>
                      <th className="th">股数</th>
                      <th className="th">价格</th>
                      <th className="th">金额</th>
                      <th className="th">费用</th>
                      <th className="th">原因</th>
                    </tr>
                  </thead>
                  <tbody>
                    {[...result.trades_tail].reverse().map((t, i) => (
                      <tr
                        key={`${t.date}-${i}`}
                        className="border-b border-slate-100 dark:border-slate-800/50"
                      >
                        <td className="td num muted">{t.date}</td>
                        <td className="td">
                          <span
                            className={
                              "tag " +
                              (t.side === "buy"
                                ? "bg-up/10 text-up"
                                : "bg-down/10 text-down")
                            }
                          >
                            {t.side === "buy" ? "买入" : "卖出"}
                          </span>
                        </td>
                        <td className="td num">{t.shares.toLocaleString()}</td>
                        <td className="td num">{t.price.toFixed(2)}</td>
                        <td className="td num">{fmtMoney(t.amount)}</td>
                        <td className="td num muted">{t.fees.toFixed(2)}</td>
                        <td className="td muted text-xs">{t.reason}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </div>

          {/* 免责声明 */}
          <div className="anim-fade-up rounded border border-amber-300/60 bg-amber-50 px-3 py-2 text-xs text-amber-700 dark:border-amber-900/60 dark:bg-amber-950/30 dark:text-amber-300">
            历史表现不代表未来收益。{result.note ?? "单组参数的历史回测不构成投资建议。"}
          </div>
        </div>
      )}
    </div>
  );
}
