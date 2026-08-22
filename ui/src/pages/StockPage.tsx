import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useParams } from "react-router-dom";
import {
  getQuote,
  getOrderBook,
  getStockBundle,
  getMinute,
  watchlistAdd,
  errMsg,
  type Quote,
  type OrderBook,
  type Bar,
  type KlinePeriod,
  type KlineAdjust,
  type MinuteData,
  type FundFlow,
  type SignalJson,
  type ChanlunDailyJson,
} from "../lib/api";
import { fmtPct, fmtVolume, fmtYiWan, fmtNum, pctClass } from "../lib/format";
import { sourceDisplayName } from "../lib/agentLabels";
import { Loading, ErrorBox, Stat, Term, useMinLoading } from "../components/ui";
import KlineChart, { type SubIndicator } from "../components/KlineChart";
import MinuteChart from "../components/MinuteChart";
import SignalCard from "../components/SignalCard";
import ChanlunPanel from "../components/ChanlunPanel";
import FundFlowPanel from "../components/FundFlowPanel";
import FundamentalsPanel from "../components/FundamentalsPanel";
import ValuationPanel from "../components/ValuationPanel";
import CanslimCard from "../components/CanslimCard";
import OrderBookPanel from "../components/OrderBookPanel";
import { useAppStore } from "../store";

const KLINE_COUNT = 500;

const RANGES: { label: string; bars: Record<KlinePeriod, number | null> }[] = [
  { label: "1月", bars: { day: 22, week: 4, month: 1 } },
  { label: "3月", bars: { day: 66, week: 13, month: 3 } },
  { label: "半年", bars: { day: 132, week: 26, month: 6 } },
  { label: "1年", bars: { day: 250, week: 52, month: 12 } },
  { label: "全部", bars: { day: null, week: null, month: null } },
];

const INDICATORS: { key: SubIndicator; tip: string }[] = [
  { key: "MACD", tip: "指数平滑异同移动平均线,判断趋势与背驰(DIF/DEA/柱)" },
  { key: "RSI", tip: "相对强弱指标(6/12/24),>70超买,<30超卖" },
  { key: "KDJ", tip: "随机指标(9,3,3),K线上穿D线为金叉" },
  { key: "BOLL", tip: "布林带(20,2),叠加在主图,触及上轨偏强、下轨偏弱" },
];

function Seg<T extends string>({
  options,
  value,
  onChange,
  tips,
}: {
  options: { key: T; label: string }[];
  value: T;
  onChange: (v: T) => void;
  tips?: Record<string, string>;
}) {
  return (
    <div className="seg">
      {options.map((o) => (
        <button
          key={o.key}
          className={"seg-item" + (value === o.key ? " seg-item-active" : "")}
          title={tips?.[o.key]}
          onClick={() => onChange(o.key)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

/** 分区降级提示(bundle missing 对应分区,非硬错误) */
function DegradeBox({ text, onRetry }: { text: string; onRetry?: () => void }) {
  return (
    <div className="mx-3 my-3 rounded border border-amber-300 bg-amber-50 px-3 py-2 text-sm text-amber-700 dark:border-amber-900 dark:bg-amber-950/40 dark:text-amber-300">
      <div className="flex items-center justify-between gap-3">
        <span>{text}</span>
        {onRetry && (
          <button className="btn shrink-0" onClick={onRetry}>
            重试
          </button>
        )}
      </div>
    </div>
  );
}

export default function StockPage() {
  const { symbol = "" } = useParams();
  const setCurrentSymbol = useAppStore((s) => s.setCurrentSymbol);
  const setCurrentName = useAppStore((s) => s.setCurrentName);
  const mode = useAppStore((s) => s.mode);

  const [quote, setQuote] = useState<Quote | null>(null);
  const [quoteErr, setQuoteErr] = useState<string | null>(null);
  const [autoRefresh, setAutoRefresh] = useState(true);
  const [orderBook, setOrderBook] = useState<OrderBook | null>(null);
  const [orderBookErr, setOrderBookErr] = useState<string | null>(null);

  const [view, setView] = useState<"kline" | "minute" | "fundamentals">("kline");
  const [period, setPeriod] = useState<KlinePeriod>("day");
  const [adjust, setAdjust] = useState<KlineAdjust>("qfq");
  const [range, setRange] = useState("半年");
  const [indicator, setIndicator] = useState<SubIndicator>("MACD");

  const [bars, setBars] = useState<Bar[] | null>(null);
  const [klineErr, setKlineErr] = useState<string | null>(null);
  const [klineSource, setKlineSource] = useState<string | null>(null);
  const [fundFlow, setFundFlow] = useState<FundFlow[] | null>(null);
  const [missing, setMissing] = useState<string[]>([]);
  const [minute, setMinute] = useState<MinuteData | null>(null);
  const [minuteErr, setMinuteErr] = useState<string | null>(null);

  const [signal, setSignal] = useState<SignalJson | null>(null);
  const [signalErr, setSignalErr] = useState<string | null>(null);
  const [signalLoading, setSignalLoading] = useState(false);

  const [chanlun, setChanlun] = useState<ChanlunDailyJson | null>(null);
  const [chanlunErr, setChanlunErr] = useState<string | null>(null);
  const [showChanlun, setShowChanlun] = useState(false);
  const [sideOpen, setSideOpen] = useState(true);

  const [watchMsg, setWatchMsg] = useState<string | null>(null);
  const quoteRef = useRef<Quote | null>(null);
  quoteRef.current = quote;

  useEffect(() => {
    if (symbol) setCurrentSymbol(symbol);
    return () => {
      setCurrentSymbol(null);
      setCurrentName(null);
    };
  }, [symbol, setCurrentSymbol, setCurrentName]);

  // 行情报价(2s 轮询,可开关)
  const loadQuote = useCallback(async () => {
    try {
      const q = await getQuote(symbol);
      setQuote(q);
      if (q.name) setCurrentName(q.name);
      setQuoteErr(null);
    } catch (e) {
      setQuoteErr(errMsg(e));
    }
  }, [symbol, setCurrentName]);

  useEffect(() => {
    setQuote(null);
    loadQuote();
    if (!autoRefresh) return;
    const t = setInterval(loadQuote, 2000);
    return () => clearInterval(t);
  }, [loadQuote, autoRefresh]);

  useEffect(() => {
    if (mode !== "pro") return;
    let alive = true;
    const load = () =>
      getOrderBook(symbol)
        .then((book) => {
          if (alive) {
            setOrderBook(book);
            setOrderBookErr(null);
          }
        })
        .catch((reason) => alive && setOrderBookErr(errMsg(reason)));
    void load();
    if (!autoRefresh) return () => { alive = false; };
    const timer = setInterval(load, 2000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [symbol, mode, autoRefresh]);

  // 一次取数(get_stock_bundle):K线只拉一次,信号/缠论由同组 bars 推导,
  // 资金流走自带 TTL 通道;除 quote 硬错误外各分区独立降级(missing 记录)
  const bundleSeq = useRef(0);
  const loadBundle = useCallback(async () => {
    const seq = ++bundleSeq.current;
    setSignalLoading(true);
    try {
      const b = await getStockBundle(symbol, period, adjust, KLINE_COUNT);
      if (seq !== bundleSeq.current) return;
      setMissing(b.missing);
      setQuote(b.quote);
      if (b.quote.name) setCurrentName(b.quote.name);
      setQuoteErr(null);
      setBars(b.kline?.bars ?? null);
      setKlineSource(b.kline?.source ?? null);
      setKlineErr(null);
      setSignal(b.analysis);
      setSignalErr(null);
      setChanlun(b.chanlun_daily);
      setChanlunErr(null);
      setFundFlow(b.fund_flow_30d);
    } catch (e) {
      if (seq !== bundleSeq.current) return;
      const m = errMsg(e);
      setMissing([]);
      setQuoteErr(m);
      setKlineErr(m);
      setSignalErr(m);
      setChanlunErr(m);
    } finally {
      if (seq === bundleSeq.current) setSignalLoading(false);
    }
  }, [symbol, period, adjust, setCurrentName]);

  useEffect(() => {
    setMissing([]);
    setBars(null);
    setKlineErr(null);
    setKlineSource(null);
    setSignal(null);
    setSignalErr(null);
    setChanlun(null);
    setChanlunErr(null);
    setFundFlow(null);
    loadBundle();
  }, [loadBundle]);

  // 分时
  useEffect(() => {
    if (view !== "minute") return;
    let alive = true;
    setMinute(null);
    setMinuteErr(null);
    const load = () =>
      getMinute(symbol)
        .then((m) => alive && setMinute(m))
        .catch((e) => alive && setMinuteErr(errMsg(e)));
    load();
    const t = setInterval(load, 5000);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, [symbol, view]);

  // 分区降级状态:bundle 的 missing 非空时在对应卡片提示(非硬错误)
  const klineMissing = !bars && !klineErr && missing.includes("kline");
  const signalMissing = !signal && !signalErr && missing.includes("analysis");
  const chanlunMissing = !chanlun && !chanlunErr && missing.includes("chanlun_daily");
  const flowDegraded = !fundFlow && missing.includes("fund_flow_30d");

  const zoomStart = useMemo(() => {
    if (!bars || bars.length === 0) return 0;
    const n = RANGES.find((r) => r.label === range)?.bars[period] ?? null;
    if (n == null || n >= bars.length) return 0;
    return Math.max(0, 100 * (1 - n / bars.length));
  }, [bars, range, period]);

  // 加载态最小展示 450ms,避免缓存命中时一闪而过
  const barsLoading = useMinLoading(!bars && !klineErr);
  const minuteLoading = useMinLoading(!minute && !minuteErr);
  const signalWaiting = useMinLoading((signalLoading || !signal) && !signalErr);

  const q = quote;

  return (
    <div className="flex h-full flex-col gap-3 overflow-hidden p-3">
      {/* 顶部报价条(固定) */}
      <div className="card shrink-0 px-4 py-3">
        {quoteErr && !q ? (
          <ErrorBox
            message={quoteErr}
            onRetry={() => {
              loadQuote();
              loadBundle();
            }}
          />
        ) : !q ? (
          <Loading text="加载行情…" />
        ) : (
          <div className="flex flex-wrap items-end gap-x-6 gap-y-2">
            <div>
              <div className="flex items-center gap-2">
                <span className="text-lg font-bold">{q.name || symbol}</span>
                <span className="num muted text-xs">{q.symbol}</span>
                <span
                  className="chip bg-sky-500/10 text-sky-600 dark:text-sky-400"
                  title={`价格时间：${q.field_provenance?.price?.as_of ?? q.timestamp}`}
                >
                  行情来源：{sourceDisplayName(q.field_provenance?.price?.source)}
                </span>
                <span
                  className="chip bg-slate-500/10 text-slate-500"
                  title={`名称刷新：${q.field_provenance?.name?.fetched_at ?? "--"}`}
                >
                  股票资料：{sourceDisplayName(q.field_provenance?.name?.source)}
                </span>
              </div>
              <div className="flex items-baseline gap-2">
                <span className={"num text-2xl font-bold " + pctClass(q.pct)}>
                  {q.price.toFixed(2)}
                </span>
                <span className={"num text-sm " + pctClass(q.pct)}>
                  {q.change >= 0 ? "+" : ""}
                  {q.change.toFixed(2)} {fmtPct(q.pct)}
                </span>
              </div>
            </div>
            <Stat label="今开" value={fmtNum(q.open)} />
            <Stat label="最高" value={<span className="text-up">{fmtNum(q.high)}</span>} />
            <Stat label="最低" value={<span className="text-down">{fmtNum(q.low)}</span>} />
            <Stat label="昨收" value={fmtNum(q.pre_close)} />
            <Stat
              label={<Term label="量比" tip="当日每分钟均量 ÷ 过去5日每分钟均量,>1 说明放量" />}
              value={signal ? fmtNum(signal.volume_price.volume_ratio) : "--"}
            />
            <Stat
              label={<Term label="换手" tip="当日成交量占流通股的比例,反映交投活跃度" />}
              value={
                <span title={q.field_provenance?.turnover?.missing_reason ?? undefined}>
                  {fmtPct(q.turnover, 2, false)}
                </span>
              }
            />
            <Stat label="成交量" value={fmtVolume(q.volume)} />
            <Stat label="成交额" value={fmtYiWan(q.amount)} />
            <div className="ml-auto flex items-center gap-2">
              <label className="muted flex cursor-pointer items-center gap-1.5 text-xs">
                <input
                  type="checkbox"
                  checked={autoRefresh}
                  onChange={(e) => setAutoRefresh(e.target.checked)}
                />
                2秒自动刷新
              </label>
              <button
                className="btn"
                onClick={async () => {
                  try {
                    await watchlistAdd(symbol, "默认");
                    setWatchMsg("已加入自选(默认分组)");
                  } catch (e) {
                    setWatchMsg(errMsg(e));
                  }
                  setTimeout(() => setWatchMsg(null), 2500);
                }}
              >
                加自选
              </button>
              {watchMsg && <span className="muted text-xs">{watchMsg}</span>}
            </div>
          </div>
        )}
      </div>

      {/* 中部:图表列(自适应填满)+ 右侧信息栏(可折叠) */}
      <div className="relative flex min-h-0 flex-1 gap-3">
        {/* 图表列 */}
        <div className="card flex min-w-0 flex-1 flex-col overflow-hidden">
          <div className="card-title shrink-0 flex-wrap gap-y-2">
            <Seg
              options={[
                { key: "kline", label: "K线" },
                { key: "minute", label: "分时" },
                { key: "fundamentals", label: "基本面" },
              ]}
              value={view}
              onChange={setView}
            />
            {view === "kline" && (
              <>
                <Seg
                  options={[
                    { key: "day", label: "日K" },
                    { key: "week", label: "周K" },
                    { key: "month", label: "月K" },
                  ]}
                  value={period}
                  onChange={setPeriod}
                />
                <Seg
                  options={[
                    { key: "qfq", label: "前复权" },
                    { key: "hfq", label: "后复权" },
                    { key: "none", label: "不复权" },
                  ]}
                  value={adjust}
                  onChange={setAdjust}
                  tips={{
                    qfq: "以现价为基准向前调整历史价,保持走势连续(默认)",
                    hfq: "以上市价为基准向后调整,反映真实累计涨幅",
                    none: "原始成交价格",
                  }}
                />
                <div className="seg">
                  {RANGES.map((r) => (
                    <button
                      key={r.label}
                      className={"seg-item" + (range === r.label ? " seg-item-active" : "")}
                      onClick={() => setRange(r.label)}
                    >
                      {r.label}
                    </button>
                  ))}
                </div>
                <div className="seg">
                  {INDICATORS.map((it) => (
                    <button
                      key={it.key}
                      className={"seg-item" + (indicator === it.key ? " seg-item-active" : "")}
                      title={it.tip}
                      onClick={() => setIndicator(it.key)}
                    >
                      {it.key}
                    </button>
                  ))}
                </div>
                {klineSource === "cache" && (
                  <span
                    className="self-center rounded bg-slate-200 px-1.5 py-0.5 text-[10px] text-slate-500 dark:bg-slate-800 dark:text-slate-400"
                    title="K线命中本地持久缓存,未消耗网络请求"
                  >
                    缓存
                  </span>
                )}
              </>
            )}
          </div>
          {/* 图表区:高度由 flex 链路决定,容器尺寸变化时 ECharts 自动 resize */}
          <div className="min-h-0 flex-1 overflow-hidden">
            {view === "kline" ? (
              klineErr ? (
                <div className="h-full overflow-y-auto">
                  <ErrorBox message={klineErr} onRetry={loadBundle} />
                </div>
              ) : klineMissing ? (
                <div className="h-full overflow-y-auto">
                  <DegradeBox text="K线数据暂不可用(数据源降级,分析与缠论同步缺失)" onRetry={loadBundle} />
                </div>
              ) : !bars || barsLoading ? (
                <Loading text="加载K线…" />
              ) : (
                <KlineChart
                  bars={bars}
                  indicator={indicator}
                  zoomStart={zoomStart}
                  chanlun={showChanlun ? chanlun : null}
                  height="100%"
                />
              )
            ) : view === "minute" ? (
              minuteErr ? (
                <div className="h-full overflow-y-auto">
                  <ErrorBox message={minuteErr} />
                </div>
              ) : !minute || minuteLoading ? (
                <Loading text="加载分时…" />
              ) : (
                <MinuteChart data={minute} height="100%" />
              )
            ) : (
              /* 基本面标签:切换到该标签时才挂载并触发数据请求(懒加载) */
              <div className="h-full space-y-3 overflow-y-auto p-3">
                <FundamentalsPanel symbol={symbol} />
                <ValuationPanel symbol={symbol} />
              </div>
            )}
          </div>
        </div>

        {/* 右侧信息栏:可折叠;<xl 窄窗口时为覆盖式抽屉,不挤压图表 */}
        {sideOpen ? (
          <aside className="card anim-drawer-in absolute inset-y-0 right-0 z-20 flex w-[380px] max-w-[85%] flex-col overflow-hidden shadow-xl xl:static xl:z-auto xl:max-w-none xl:shrink-0 xl:shadow-none">
            <div className="card-title shrink-0 justify-between">
              分析面板
              <button
                className="muted text-xs font-normal hover:text-slate-700 dark:hover:text-slate-200"
                title="收起分析面板"
                onClick={() => setSideOpen(false)}
              >
                收起 »
              </button>
            </div>
            <div className="stagger min-h-0 flex-1 space-y-3 overflow-y-auto p-3">
              {mode === "pro" && (
                orderBook ? <OrderBookPanel data={orderBook} /> : orderBookErr ? (
                  <div className="card"><div className="card-title">五档盘口</div><DegradeBox text={orderBookErr} /></div>
                ) : (
                  <div className="card"><div className="card-title">五档盘口</div><Loading text="同步 TDX 盘口…" /></div>
                )
              )}
              {/* 信号卡 */}
              {signalErr ? (
                <div className="card">
                  <div className="card-title">信号卡</div>
                  <ErrorBox message={signalErr} onRetry={loadBundle} />
                </div>
              ) : signalMissing ? (
                <div className="card">
                  <div className="card-title">信号卡</div>
                  <DegradeBox text="信号分析暂不可用(数据源降级)" onRetry={loadBundle} />
                </div>
              ) : !signal || signalLoading || signalWaiting ? (
                <div className="card">
                  <div className="card-title">信号卡</div>
                  <Loading text="分析中(多模块计算,请稍候)…" />
                </div>
              ) : (
                <SignalCard signal={signal} />
              )}

              {mode === "pro" && (
                <>
                  {/* 缠论面板 */}
                  {chanlunErr ? (
                    <div className="card">
                      <div className="card-title">缠论分析</div>
                      <ErrorBox message={chanlunErr} onRetry={loadBundle} />
                    </div>
                  ) : chanlunMissing ? (
                    <div className="card">
                      <div className="card-title">缠论分析</div>
                      <DegradeBox text="缠论分析暂不可用(数据源降级)" onRetry={loadBundle} />
                    </div>
                  ) : !chanlun ? (
                    <div className="card">
                      <div className="card-title">缠论分析</div>
                      <Loading />
                    </div>
                  ) : (
                    <ChanlunPanel data={chanlun} showOverlay={showChanlun} onToggleOverlay={setShowChanlun} />
                  )}

                  {/* 资金流(30日数据来自 bundle,今日累计组件自取) */}
                  <FundFlowPanel symbol={symbol} dailyFlows={fundFlow} dailyDegraded={flowDegraded} />

                  {/* CANSLIM */}
                  {signal && <CanslimCard data={signal.canslim} />}
                </>
              )}
            </div>
          </aside>
        ) : (
          <div className="card flex w-10 shrink-0 flex-col items-center gap-2 py-3">
            <button
              className="btn !px-1.5"
              title="展开分析面板"
              onClick={() => setSideOpen(true)}
            >
              <svg
                className="h-3.5 w-3.5"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                <path d="M15 18l-6-6 6-6" />
              </svg>
            </button>
            <span className="muted text-xs [writing-mode:vertical-rl]">分析面板</span>
          </div>
        )}
      </div>
    </div>
  );
}
