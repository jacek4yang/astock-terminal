import { useEffect, useMemo, useState, type ReactNode } from "react";
import type { EChartsOption } from "echarts";
import {
  getZtPool,
  getPool,
  getBillboard,
  getMarginDaily,
  getOrgSurvey,
  getHolderNum,
  getEarningsPredict,
  getLiftStage,
  getNotices,
  errMsg,
  type DcResult,
  type PoolKind,
  type PoolRow,
  type ZtPoolRow,
  type BillboardRow,
  type MarginDailyRow,
  type OrgSurveyRow,
  type HolderNumRow,
  type EarningsPredictRow,
  type LiftStageRow,
  type NoticeRow,
} from "../lib/api";
import { EMPTY_DISPLAY, fmtNum, fmtYiWan, fmtPct, fmtText, pctClass, COLOR_UP } from "../lib/format";
import { Loading, ErrorBox, EmptyBox, Term } from "../components/ui";
import Chart from "../components/Chart";

/** 表格最多渲染行数(超出截断并提示) */
const MAX_ROWS = 200;

/** fetched_at(RFC3339)→ 本地可读时间 */
function fmtDateTime(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleString("zh-CN", { hour12: false });
}

/** 今天 ± offset 天的 YYYY-MM-DD(本地时区) */
function dayStr(offsetDays = 0): string {
  const d = new Date();
  d.setDate(d.getDate() + offsetDays);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/** 数据中心通用取数:加载骨架 / 错误重试 / 手动刷新(tick) */
function useDc<T>(fetcher: () => Promise<DcResult<T>>, deps: unknown[]) {
  const [data, setData] = useState<DcResult<T> | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [tick, setTick] = useState(0);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    setErr(null);
    fetcher()
      .then((d) => {
        if (alive) {
          setData(d);
          setLoading(false);
        }
      })
      .catch((e) => {
        if (alive) {
          setErr(errMsg(e));
          setLoading(false);
        }
      });
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, tick]);

  return { data, err, loading, reload: () => setTick((t) => t + 1) };
}

interface Col<T> {
  label: ReactNode;
  render: (row: T) => ReactNode;
  /** 追加到 td 的类名(如 "num"、"max-w-[280px] truncate") */
  className?: string;
}

/** 通用表格:sticky 表头 + 截断前 MAX_ROWS 行 */
function DataTable<T>({
  cols,
  rows,
  rowKey,
}: {
  cols: Col<T>[];
  rows: T[];
  rowKey: (row: T, i: number) => string;
}) {
  if (rows.length === 0) return <EmptyBox text="暂无数据" />;
  return (
    <div className="min-h-0 flex-1 overflow-auto">
      <table className="w-full border-collapse">
        <thead className="sticky top-0 z-10 bg-white dark:bg-slate-900">
          <tr className="border-b border-slate-200 dark:border-slate-800">
            {cols.map((c, i) => (
              <th key={i} className="th">
                {c.label}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.slice(0, MAX_ROWS).map((r, i) => (
            <tr
              key={rowKey(r, i)}
              className="border-b border-slate-100 hover:bg-slate-50 dark:border-slate-800/50 dark:hover:bg-slate-800/50"
            >
              {cols.map((c, j) => (
                <td key={j} className={"td " + (c.className ?? "")}>
                  {c.render(r)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
      {rows.length > MAX_ROWS && (
        <div className="muted py-2 text-center text-xs">
          仅显示前 {MAX_ROWS} 条(共 {rows.length} 条)
        </div>
      )}
    </div>
  );
}

interface DcQuery<T> {
  data: DcResult<T> | null;
  err: string | null;
  loading: boolean;
  reload: () => void;
}

/** 通用分区卡片:标题 + 控件 + 数据时间 + 骨架/错误/表格 */
function DcSection<T>({
  title,
  controls,
  query,
  cols,
  rowKey,
}: {
  title: ReactNode;
  controls?: ReactNode;
  query: DcQuery<T>;
  cols: Col<T>[];
  rowKey: (row: T, i: number) => string;
}) {
  const { data, err, loading, reload } = query;
  return (
    <div className="card flex min-h-0 flex-1 flex-col overflow-hidden">
      <div className="card-title shrink-0 justify-between">
        <span>
          {title}
          {data && <span className="muted ml-2 font-normal">{data.count} 条</span>}
        </span>
        <span className="flex items-center gap-3">
          {controls}
          {data && (
            <span className="muted text-xs font-normal">
              数据时间 {fmtDateTime(data.fetched_at)}
            </span>
          )}
        </span>
      </div>
      {err && <ErrorBox message={err} onRetry={reload} />}
      {loading ? (
        <Loading text="加载数据…" />
      ) : data ? (
        <DataTable cols={cols} rows={data.rows} rowKey={rowKey} />
      ) : null}
    </div>
  );
}

/** 天数下拉选择器 */
function DaysSelect({
  value,
  options,
  onChange,
}: {
  value: number;
  options: number[];
  onChange: (v: number) => void;
}) {
  return (
    <select
      className="input !py-1 text-xs"
      value={value}
      onChange={(e) => onChange(Number(e.target.value))}
    >
      {options.map((d) => (
        <option key={d} value={d}>
          近 {d} 天
        </option>
      ))}
    </select>
  );
}

// ==================== 涨停池 ====================

const POOL_OPTIONS: { key: PoolKind; label: string }[] = [
  { key: "zt", label: "涨停池" },
  { key: "prev", label: "昨日涨停" },
  { key: "strong", label: "强势股" },
  { key: "sub_new", label: "次新股" },
  { key: "broken", label: "炸板池" },
  { key: "dt", label: "跌停池" },
];

/** 通用池字段中文表头(未列出的字段原样显示 key) */
const POOL_FIELD_LABELS: Record<string, string> = {
  code: "代码",
  name: "名称",
  price: "最新价",
  limit_price: "涨停价",
  pct: "涨跌幅",
  amount: "成交额",
  float_market_cap: "流通市值",
  total_market_cap: "总市值",
  turnover: "换手率",
  amplitude: "振幅",
  speed: "涨速",
  volume_ratio: "量比",
  lock_fund: "封单资金",
  first_lock_time: "首次封板",
  last_lock_time: "最后封板",
  yesterday_lock_time: "昨日封板",
  yesterday_limit_times: "昨日连板",
  limit_times: "连板数",
  break_times: "炸板次数",
  is_new_high: "60日新高",
  reason: "入选理由",
  open_days: "开板几日",
  open_date: "开板日期",
  ipo_date: "上市日期",
  pe_dynamic: "动态市盈率",
  board_amount: "板上成交额",
  limit_down_days: "连续跌停",
  open_times: "开板次数",
  limit_stat: "涨停统计",
  industry: "所属行业",
};

/** 大额资金字段(元 → 亿/万) */
const MONEY_FIELDS = new Set([
  "amount",
  "float_market_cap",
  "total_market_cap",
  "lock_fund",
  "board_amount",
]);

function poolCell(key: string, v: unknown): ReactNode {
  if (v == null) return EMPTY_DISPLAY;
  if (typeof v === "number") {
    if (key === "pct") return <span className={pctClass(v)}>{fmtPct(v)}</span>;
    if (MONEY_FIELDS.has(key)) return fmtYiWan(v);
    return fmtNum(v);
  }
  if (typeof v === "object") {
    // limit_stat 等结构:{days, times} → "times/days"
    const o = v as Record<string, unknown>;
    if ("times" in o && "days" in o) return `${o.times}/${o.days}`;
    return JSON.stringify(v);
  }
  if (typeof v === "boolean") return v ? "是" : "否";
  return fmtText(v);
}

/** 非涨停池的通用表:列由首行字段驱动 */
function genericPoolCols(rows: PoolRow[]): Col<PoolRow>[] {
  if (rows.length === 0) return [];
  return Object.keys(rows[0]).map((k) => ({
    label: POOL_FIELD_LABELS[k] ?? k,
    className: k === "name" || k === "reason" || k === "industry" ? undefined : "num",
    render: (r) => poolCell(k, r[k]),
  }));
}

function ZtPoolTable({ date }: { date: string }) {
  const query = useDc<ZtPoolRow>(() => getZtPool(date || undefined), [date]);

  const view = useMemo(() => {
    if (!query.data) return null;
    const rows = [...query.data.rows].sort((a, b) => (b.lock_fund ?? 0) - (a.lock_fund ?? 0));
    return { ...query.data, rows };
  }, [query.data]);

  const cols: Col<ZtPoolRow>[] = [
    { label: "代码", className: "num muted", render: (r) => r.code },
    { label: "名称", render: (r) => r.name },
    {
      label: "最新价",
      className: "num",
      render: (r) => <span className={pctClass(r.pct)}>{fmtNum(r.price)}</span>,
    },
    {
      label: "涨跌幅",
      className: "num",
      render: (r) => <span className={pctClass(r.pct)}>{fmtPct(r.pct)}</span>,
    },
    {
      label: (
        <Term label="封单额" tip="涨停价上的封单资金,越大表示封板越坚决(按此列降序)" />
      ),
      className: "num",
      render: (r) => fmtYiWan(r.lock_fund),
    },
    {
      label: "连板数",
      className: "num",
      render: (r) =>
        r.limit_times != null && r.limit_times >= 2 ? (
          <span className="tag bg-up/10 text-up">{r.limit_times} 连板</span>
        ) : (
          (r.limit_times ?? EMPTY_DISPLAY)
        ),
    },
    { label: "首次封板", className: "num", render: (r) => fmtText(r.first_lock_time) },
    {
      label: <Term label="炸板次数" tip="盘中打开涨停的次数,越多说明分歧越大" />,
      className: "num",
      render: (r) => r.break_times ?? EMPTY_DISPLAY,
    },
  ];

  return (
    <DcSection
      title="涨停股池"
      query={{ ...query, data: view }}
      cols={cols}
      rowKey={(r) => r.code}
    />
  );
}

function GenericPoolTable({ pool, date }: { pool: PoolKind; date: string }) {
  const query = useDc<PoolRow>(() => getPool(pool, date || undefined), [pool, date]);
  return (
    <DcSection
      title={POOL_OPTIONS.find((o) => o.key === pool)?.label ?? pool}
      query={query}
      cols={genericPoolCols(query.data?.rows ?? [])}
      rowKey={(r, i) => String(r.code ?? i)}
    />
  );
}

function ZtPoolTab() {
  const [date, setDate] = useState("");
  const [pool, setPool] = useState<PoolKind>("zt");
  return (
    <div className="flex min-h-0 flex-1 flex-col gap-3">
      <div className="flex shrink-0 items-center gap-2">
        <select
          className="input !py-1 text-xs"
          value={pool}
          onChange={(e) => setPool(e.target.value as PoolKind)}
        >
          {POOL_OPTIONS.map((o) => (
            <option key={o.key} value={o.key}>
              {o.label}
            </option>
          ))}
        </select>
        <input
          type="date"
          className="input !py-1 text-xs"
          value={date}
          onChange={(e) => setDate(e.target.value)}
          title="留空为最近交易日"
        />
      </div>
      {pool === "zt" ? <ZtPoolTable date={date} /> : <GenericPoolTable pool={pool} date={date} />}
    </div>
  );
}

// ==================== 龙虎榜 ====================

function BillboardTab() {
  const [days, setDays] = useState(7);
  const query = useDc<BillboardRow>(() => getBillboard(days), [days]);

  const cols: Col<BillboardRow>[] = [
    { label: "日期", className: "num muted", render: (r) => fmtText(r.trade_date) },
    { label: "代码", className: "num muted", render: (r) => r.code },
    { label: "名称", render: (r) => r.name },
    {
      label: "上榜原因",
      className: "max-w-[260px] truncate",
      render: (r) => <span title={r.explanation}>{r.explanation}</span>,
    },
    { label: "买入额", className: "num", render: (r) => fmtYiWan(r.buy_amt) },
    { label: "卖出额", className: "num", render: (r) => fmtYiWan(r.sell_amt) },
    {
      label: "净买额",
      className: "num",
      render: (r) => <span className={pctClass(r.net_amt)}>{fmtYiWan(r.net_amt)}</span>,
    },
    {
      label: <Term label="成交额占比" tip="龙虎榜成交额占当日该股总成交额的比例" />,
      className: "num",
      render: (r) => (r.deal_amount_ratio == null ? EMPTY_DISPLAY : r.deal_amount_ratio.toFixed(2) + "%"),
    },
  ];

  return (
    <DcSection
      title="龙虎榜"
      controls={<DaysSelect value={days} options={[7, 14, 30, 60]} onChange={setDays} />}
      query={query}
      cols={cols}
      rowKey={(r, i) => `${r.trade_date}-${r.code}-${i}`}
    />
  );
}

// ==================== 两融 ====================

function MarginTab() {
  const query = useDc<MarginDailyRow>(() => getMarginDaily(), []);
  const { data, err, loading, reload } = query;

  const chartOption = useMemo<EChartsOption | null>(() => {
    if (!data || data.rows.length === 0) return null;
    // 后端按日期倒序;图表取最近 250 个交易日并转为升序
    const asc = [...data.rows].reverse().slice(-250);
    return {
      animation: false,
      backgroundColor: "transparent",
      tooltip: {
        trigger: "axis",
        backgroundColor: "rgba(30,41,59,0.92)",
        borderWidth: 0,
        textStyle: { fontSize: 11, color: "#e2e8f0" },
        valueFormatter: (v: unknown) => (v == null ? EMPTY_DISPLAY : `${Number(v).toFixed(1)} 亿`),
      },
      grid: { left: 64, right: 12, top: 8, bottom: 20 },
      xAxis: {
        type: "category",
        data: asc.map((r) => (r.statistics_date ?? "").slice(5)),
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
          name: "融资余额",
          type: "line",
          data: asc.map((r) => r.fin_balance_yi),
          showSymbol: false,
          lineStyle: { color: COLOR_UP, width: 1.5 },
          itemStyle: { color: COLOR_UP },
          areaStyle: { opacity: 0.08 },
        },
      ],
    } as EChartsOption;
  }, [data]);

  const cols: Col<MarginDailyRow>[] = [
    { label: "日期", className: "num muted", render: (r) => fmtText(r.statistics_date) },
    {
      label: <Term label="融资余额(亿)" tip="投资者借钱买入尚未偿还的余额,反映杠杆做多情绪" />,
      className: "num",
      render: (r) => fmtNum(r.fin_balance_yi, 1),
    },
    {
      label: "融券余额(亿)",
      className: "num",
      render: (r) => fmtNum(r.loan_balance_yi, 1),
    },
    { label: "两融余额(亿)", className: "num", render: (r) => fmtNum(r.margin_balance_yi, 1) },
    { label: "融资买入额(亿)", className: "num", render: (r) => fmtNum(r.fin_buy_amt_yi, 1) },
    {
      label: <Term label="平均担保比例(%)" tip="担保物价值/两融负债,越低说明杠杆越满、风险越高" />,
      className: "num",
      render: (r) => fmtNum(r.avg_guarantee_ratio, 1),
    },
  ];

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-3">
      <div className="card shrink-0 overflow-hidden">
        <div className="card-title justify-between">
          <span>融资余额趋势(亿元,近 250 个交易日)</span>
          {data && (
            <span className="muted text-xs font-normal">
              数据时间 {fmtDateTime(data.fetched_at)}
            </span>
          )}
        </div>
        {loading ? (
          <Loading text="加载两融数据…" />
        ) : chartOption ? (
          <Chart option={chartOption} height={240} />
        ) : null}
      </div>
      <DcSection
        title="两融账户统计"
        query={{ data, err, loading, reload }}
        cols={cols}
        rowKey={(r) => r.statistics_date ?? ""}
      />
    </div>
  );
}

// ==================== 机构调研 ====================

function OrgSurveyTab() {
  const [days, setDays] = useState(30);
  const query = useDc<OrgSurveyRow>(() => getOrgSurvey(days), [days]);

  const cols: Col<OrgSurveyRow>[] = [
    { label: "代码", className: "num muted", render: (r) => r.code },
    { label: "名称", render: (r) => r.name },
    {
      label: "调研日期",
      className: "num muted",
      render: (r) => fmtText(r.receive_start_date ?? r.notice_date),
    },
    {
      label: "机构数量",
      className: "num",
      render: (r) => (r.org_count == null ? EMPTY_DISPLAY : r.org_count.toFixed(0)),
    },
    { label: "调研方式", render: (r) => fmtText(r.receive_way_explain) },
    {
      label: <Term label="调研机构" tip="参与调研的机构名单(原始文本,可能多家拼接)" />,
      className: "max-w-[360px] truncate",
      render: (r) => <span title={fmtText(r.receive_object)}>{fmtText(r.receive_object)}</span>,
    },
  ];

  return (
    <DcSection
      title="机构调研"
      controls={<DaysSelect value={days} options={[30, 90, 180, 365]} onChange={setDays} />}
      query={query}
      cols={cols}
      rowKey={(r, i) => `${r.code}-${r.receive_start_date}-${i}`}
    />
  );
}

// ==================== 更多数据 ====================

function HolderPanel() {
  const [input, setInput] = useState("");
  const [code, setCode] = useState("");
  const query = useDc<HolderNumRow>(() => getHolderNum(code || undefined), [code]);

  const cols: Col<HolderNumRow>[] = [
    { label: "代码", className: "num muted", render: (r) => r.code },
    { label: "名称", render: (r) => r.name },
    { label: "截止日", className: "num muted", render: (r) => fmtText(r.end_date) },
    { label: "股东户数", className: "num", render: (r) => fmtYiWan(r.holder_num, 0) },
    {
      label: (
        <Term label="较上期" tip="股东户数环比增减;户数减少通常意味着筹码集中" />
      ),
      className: "num",
      render: (r) => fmtPct(r.holder_num_ratio),
    },
    {
      label: "区间涨跌幅",
      className: "num",
      render: (r) => <span className={pctClass(r.interval_change)}>{fmtPct(r.interval_change)}</span>,
    },
    { label: "户均持股市值", className: "num", render: (r) => fmtYiWan(r.avg_market_cap) },
    { label: "公告日", className: "num muted", render: (r) => fmtText(r.hold_notice_date) },
  ];

  return (
    <DcSection
      title="股东户数"
      controls={
        <span className="flex items-center gap-2">
          <input
            className="input w-32 !py-1 text-xs"
            placeholder="代码(留空查全部)"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && setCode(input.trim())}
          />
          <button className="btn !py-1 text-xs" onClick={() => setCode(input.trim())}>
            查询
          </button>
        </span>
      }
      query={query}
      cols={cols}
      rowKey={(r) => r.code}
    />
  );
}

function predictTypeCls(t: string): string {
  if (/(预增|扭亏|减亏|略增)/.test(t)) return "tag bg-up/10 text-up";
  if (/(预减|首亏|增亏|略减|续亏)/.test(t)) return "tag bg-down/10 text-down";
  return "tag bg-slate-200 text-slate-600 dark:bg-slate-800 dark:text-slate-300";
}

function EarningsPanel() {
  const query = useDc<EarningsPredictRow>(() => getEarningsPredict(), []);

  const cols: Col<EarningsPredictRow>[] = [
    { label: "代码", className: "num muted", render: (r) => r.code },
    { label: "名称", render: (r) => r.name },
    { label: "公告日", className: "num muted", render: (r) => fmtText(r.notice_date) },
    {
      label: "预告类型",
      render: (r) => <span className={predictTypeCls(r.predict_type)}>{r.predict_type}</span>,
    },
    {
      label: "变动幅度",
      className: "num",
      render: (r) =>
        r.add_amp_lower == null && r.add_amp_upper == null
          ? EMPTY_DISPLAY
          : `${fmtNum(r.add_amp_lower, 0)}% ~ ${fmtNum(r.add_amp_upper, 0)}%`,
    },
    {
      label: "预测金额",
      className: "num",
      render: (r) =>
        r.predict_amt_lower == null && r.predict_amt_upper == null
          ? EMPTY_DISPLAY
          : `${fmtYiWan(r.predict_amt_lower)} ~ ${fmtYiWan(r.predict_amt_upper)}`,
    },
    {
      label: "摘要",
      className: "max-w-[360px] truncate",
      render: (r) => <span title={fmtText(r.predict_content)}>{fmtText(r.predict_content)}</span>,
    },
  ];

  return (
    <DcSection
      title="业绩预告"
      query={query}
      cols={cols}
      rowKey={(r, i) => `${r.code}-${r.notice_date}-${i}`}
    />
  );
}

function LiftPanel() {
  const [startInput, setStartInput] = useState(dayStr(0));
  const [endInput, setEndInput] = useState(dayStr(90));
  const [range, setRange] = useState<[string, string]>([dayStr(0), dayStr(90)]);
  const query = useDc<LiftStageRow>(() => getLiftStage(range[0], range[1]), [range]);

  const cols: Col<LiftStageRow>[] = [
    { label: "代码", className: "num muted", render: (r) => r.code },
    { label: "名称", render: (r) => r.name },
    { label: "解禁日期", className: "num muted", render: (r) => fmtText(r.free_date) },
    { label: "解禁数量", className: "num", render: (r) => fmtYiWan(r.able_free_shares) },
    { label: "解禁市值", className: "num", render: (r) => fmtYiWan(r.lift_market_cap) },
    {
      label: "占流通市值比",
      className: "num",
      render: (r) => (r.free_ratio == null ? EMPTY_DISPLAY : (r.free_ratio * 100).toFixed(2) + "%"),
    },
    {
      label: "解禁前20日",
      className: "num",
      render: (r) => <span className={pctClass(r.b20_change)}>{fmtPct(r.b20_change)}</span>,
    },
    {
      label: "限售股类型",
      className: "max-w-[200px] truncate",
      render: (r) => <span title={fmtText(r.free_shares_type)}>{fmtText(r.free_shares_type)}</span>,
    },
  ];

  return (
    <DcSection
      title="限售解禁"
      controls={
        <span className="flex items-center gap-2">
          <input
            type="date"
            className="input !py-1 text-xs"
            value={startInput}
            onChange={(e) => setStartInput(e.target.value)}
          />
          <span className="muted text-xs">至</span>
          <input
            type="date"
            className="input !py-1 text-xs"
            value={endInput}
            onChange={(e) => setEndInput(e.target.value)}
          />
          <button
            className="btn !py-1 text-xs"
            onClick={() => startInput && endInput && setRange([startInput, endInput])}
          >
            查询
          </button>
        </span>
      }
      query={query}
      cols={cols}
      rowKey={(r, i) => `${r.code}-${r.free_date}-${i}`}
    />
  );
}

function NoticePanel({ code, days }: { code: string; days: number }) {
  const query = useDc<NoticeRow>(() => getNotices(code, days), [code, days]);

  const cols: Col<NoticeRow>[] = [
    { label: "日期", className: "num muted", render: (r) => fmtText(r.notice_date) },
    {
      label: "类型",
      render: (r) => (
        <span className="tag bg-slate-200 text-slate-600 dark:bg-slate-800 dark:text-slate-300">
          {r.column_name || "其他"}
        </span>
      ),
    },
    {
      label: "标题",
      className: "max-w-[520px] truncate",
      render: (r) => <span title={r.url}>{r.title}</span>,
    },
  ];

  return (
    <DcSection
      title={`个股公告 ${code}`}
      query={query}
      cols={cols}
      rowKey={(r) => r.art_code}
    />
  );
}

const MORE_KINDS = [
  { key: "holder", label: "股东户数" },
  { key: "earnings", label: "业绩预告" },
  { key: "lift", label: "限售解禁" },
  { key: "notices", label: "个股公告" },
] as const;

type MoreKind = (typeof MORE_KINDS)[number]["key"];

function MoreTab() {
  const [kind, setKind] = useState<MoreKind>("holder");
  const [noticeInput, setNoticeInput] = useState("");
  const [noticeCode, setNoticeCode] = useState("");
  const [noticeDays, setNoticeDays] = useState(90);

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-3">
      <div className="flex shrink-0 flex-wrap items-center gap-1">
        {MORE_KINDS.map((k) => (
          <button
            key={k.key}
            className={pillCls(kind === k.key)}
            onClick={() => setKind(k.key)}
          >
            {k.label}
          </button>
        ))}
        {kind === "notices" && (
          <span className="ml-2 flex items-center gap-2">
            <input
              className="input w-32 !py-1 text-xs"
              placeholder="输入股票代码"
              value={noticeInput}
              onChange={(e) => setNoticeInput(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && setNoticeCode(noticeInput.trim())}
            />
            <DaysSelect value={noticeDays} options={[30, 90, 180, 365]} onChange={setNoticeDays} />
            <button className="btn !py-1 text-xs" onClick={() => setNoticeCode(noticeInput.trim())}>
              查询
            </button>
          </span>
        )}
      </div>
      {kind === "holder" && <HolderPanel />}
      {kind === "earnings" && <EarningsPanel />}
      {kind === "lift" && <LiftPanel />}
      {kind === "notices" &&
        (noticeCode ? (
          <NoticePanel key={`${noticeCode}-${noticeDays}`} code={noticeCode} days={noticeDays} />
        ) : (
          <div className="card flex min-h-0 flex-1 items-center justify-center">
            <EmptyBox text="输入股票代码后点击「查询」查看公告" />
          </div>
        ))}
    </div>
  );
}

// ==================== 页面 ====================

const TABS = [
  { key: "zt", label: "涨停池" },
  { key: "billboard", label: "龙虎榜" },
  { key: "margin", label: "两融" },
  { key: "survey", label: "机构调研" },
  { key: "more", label: "更多数据" },
] as const;

type TabKey = (typeof TABS)[number]["key"];

function pillCls(active: boolean): string {
  return (
    "rounded-md px-3 py-1.5 text-xs transition-colors " +
    (active
      ? "bg-blue-600/10 font-medium text-blue-600 dark:text-blue-400"
      : "text-slate-500 hover:bg-slate-100 hover:text-slate-700 dark:text-slate-400 dark:hover:bg-slate-800 dark:hover:text-slate-200")
  );
}

export default function DataHubPage() {
  const [tab, setTab] = useState<TabKey>("zt");

  return (
    <div className="flex h-full flex-col gap-3 overflow-hidden p-3">
      <div className="flex shrink-0 items-center gap-3">
        <h1 className="text-sm font-semibold text-slate-700 dark:text-slate-300">数据中心</h1>
        <div className="flex gap-1">
          {TABS.map((t) => (
            <button key={t.key} className={pillCls(tab === t.key)} onClick={() => setTab(t.key)}>
              {t.label}
            </button>
          ))}
        </div>
      </div>
      {tab === "zt" && <ZtPoolTab />}
      {tab === "billboard" && <BillboardTab />}
      {tab === "margin" && <MarginTab />}
      {tab === "survey" && <OrgSurveyTab />}
      {tab === "more" && <MoreTab />}
    </div>
  );
}
