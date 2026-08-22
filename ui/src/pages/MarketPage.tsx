import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import {
  getAllAShares,
  getIndexKline,
  getMarketBreadth,
  errMsg,
  type AllShare,
  type Bar,
  type MarketBreadth,
} from "../lib/api";
import { fmtNum, fmtPct, fmtText, fmtYiWan, pctClass } from "../lib/format";
import { sourceDisplayName } from "../lib/agentLabels";
import { ErrorBox, Loading, Term } from "../components/ui";

const INDEXES = [
  { secid: "1.000001", name: "上证指数" },
  { secid: "0.399001", name: "深证成指" },
  { secid: "0.399006", name: "创业板指" },
];

const BOARD_LABELS: Record<string, string> = {
  main: "主板",
  chi_next: "创业板",
  star: "科创板",
  beijing: "北交所",
  fund: "基金",
  other: "其他",
};

type SortKey = "code" | "name" | "market" | "board" | "price" | "pct" | "amount";
type PctFilter = "all" | "up" | "down" | "flat" | "limit_up" | "limit_down";

function IndexCard({ secid, name }: { secid: string; name: string }) {
  const [bar, setBar] = useState<Bar | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    // Keep enough context for the backend's weekend/spike quality filter.
    getIndexKline(secid, 10)
      .then((bars) => alive && setBar(bars.at(-1) ?? null))
      .catch((reason) => alive && setError(errMsg(reason)));
    return () => {
      alive = false;
    };
  }, [secid]);

  return (
    <div className="card relative overflow-hidden px-4 py-3">
      <div className="muted text-[11px] font-medium tracking-wide">{name}</div>
      {error ? (
        <div className="mt-2 text-xs text-red-500">暂不可用</div>
      ) : bar ? (
        <div className="mt-1 flex items-end justify-between">
          <div className={`num text-xl font-semibold ${pctClass(bar.pct)}`}>
            {fmtNum(bar.close)}
          </div>
          <div className={`num rounded px-1.5 py-0.5 text-xs ${pctClass(bar.pct)}`}>
            {fmtPct(bar.pct)}
          </div>
        </div>
      ) : (
        <div className="muted mt-2 text-xs">同步中…</div>
      )}
    </div>
  );
}

function BreadthBar({ data }: { data: MarketBreadth }) {
  const upPct = data.total ? (data.up / data.total) * 100 : 0;
  const downPct = data.total ? (data.down / data.total) * 100 : 0;
  return (
    <div className="card flex items-center gap-4 px-4 py-3">
      <span className="num w-20 text-xs text-up">上涨 {data.up}</span>
      <div className="flex h-2 min-w-32 flex-1 overflow-hidden rounded-full bg-slate-200 dark:bg-slate-800">
        <div className="bg-up" style={{ width: `${upPct}%` }} />
        <div className="flex-1 bg-slate-300 dark:bg-slate-700" />
        <div className="bg-down" style={{ width: `${downPct}%` }} />
      </div>
      <span className="muted text-xs">平盘 {data.flat}</span>
      <span className="num w-20 text-right text-xs text-down">下跌 {data.down}</span>
    </div>
  );
}

export function pageTokens(page: number, total: number): Array<number | "ellipsis"> {
  const visible = new Set([1, total, page - 2, page - 1, page, page + 1, page + 2]);
  const pages = [...visible].filter((value) => value >= 1 && value <= total).sort((a, b) => a - b);
  const tokens: Array<number | "ellipsis"> = [];
  pages.forEach((value, index) => {
    if (index > 0 && value - pages[index - 1] > 1) tokens.push("ellipsis");
    tokens.push(value);
  });
  return tokens;
}

export default function MarketPage() {
  const [breadth, setBreadth] = useState<MarketBreadth | null>(null);
  const [shares, setShares] = useState<AllShare[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [sortKey, setSortKey] = useState<SortKey>("amount");
  const [sortAsc, setSortAsc] = useState(false);
  const [keyword, setKeyword] = useState("");
  const [market, setMarket] = useState("all");
  const [board, setBoard] = useState("all");
  const [pctFilter, setPctFilter] = useState<PctFilter>("all");
  const [minPrice, setMinPrice] = useState("");
  const [maxPrice, setMaxPrice] = useState("");
  const [minAmountYi, setMinAmountYi] = useState("");
  const [availableOnly, setAvailableOnly] = useState(true);
  const [filtersOpen, setFiltersOpen] = useState(true);
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(50);
  const navigate = useNavigate();

  const load = () => {
    setError(null);
    void getMarketBreadth().then(setBreadth).catch((reason) => setError(errMsg(reason)));
    void getAllAShares().then(setShares).catch((reason) => setError(errMsg(reason)));
  };

  useEffect(load, []);

  const filtered = useMemo(() => {
    if (!shares) return [];
    const query = keyword.trim().toLowerCase();
    const minP = minPrice === "" ? null : Number(minPrice);
    const maxP = maxPrice === "" ? null : Number(maxPrice);
    const minAmount = minAmountYi === "" ? null : Number(minAmountYi) * 1e8;
    return shares.filter((row) => {
      if (query && !row.code.includes(query) && !row.name.toLowerCase().includes(query)) return false;
      if (market !== "all" && row.market !== market) return false;
      if (board !== "all" && row.board !== board) return false;
      if (availableOnly && (row.price == null || row.pct == null)) return false;
      if (minP != null && (row.price == null || row.price < minP)) return false;
      if (maxP != null && (row.price == null || row.price > maxP)) return false;
      if (minAmount != null && (row.amount == null || row.amount < minAmount)) return false;
      if (pctFilter === "up" && (row.pct == null || row.pct <= 0)) return false;
      if (pctFilter === "down" && (row.pct == null || row.pct >= 0)) return false;
      if (pctFilter === "flat" && row.pct !== 0) return false;
      if (pctFilter === "limit_up" && (row.pct == null || row.pct < 9.8)) return false;
      if (pctFilter === "limit_down" && (row.pct == null || row.pct > -9.8)) return false;
      return true;
    });
  }, [shares, keyword, market, board, pctFilter, minPrice, maxPrice, minAmountYi, availableOnly]);

  const sorted = useMemo(() => {
    return [...filtered].sort((a, b) => {
      const left = a[sortKey];
      const right = b[sortKey];
      if (left == null && right == null) return 0;
      if (left == null) return 1;
      if (right == null) return -1;
      const order = typeof left === "string"
        ? left.localeCompare(String(right), "zh-CN")
        : left - Number(right);
      return sortAsc ? order : -order;
    });
  }, [filtered, sortKey, sortAsc]);

  const pageCount = Math.max(1, Math.ceil(sorted.length / pageSize));
  const currentPage = Math.min(page, pageCount);
  const rows = sorted.slice((currentPage - 1) * pageSize, currentPage * pageSize);

  useEffect(() => setPage(1), [keyword, market, board, pctFilter, minPrice, maxPrice, minAmountYi, availableOnly, pageSize]);

  const resetFilters = () => {
    setKeyword("");
    setMarket("all");
    setBoard("all");
    setPctFilter("all");
    setMinPrice("");
    setMaxPrice("");
    setMinAmountYi("");
    setAvailableOnly(true);
  };

  const header = (key: SortKey, label: string, align = "text-left") => (
    <th
      className={`th cursor-pointer select-none ${align} hover:text-slate-800 dark:hover:text-white`}
      onClick={() => {
        if (sortKey === key) setSortAsc((value) => !value);
        else {
          setSortKey(key);
          setSortAsc(key === "code" || key === "name" || key === "market" || key === "board");
        }
      }}
    >
      {label}<span className="ml-1 opacity-60">{sortKey === key ? (sortAsc ? "↑" : "↓") : "↕"}</span>
    </th>
  );

  return (
    <div className="flex h-full flex-col gap-3 overflow-hidden p-3">
      <div className="flex shrink-0 items-center justify-between">
        <div>
          <h1 className="text-base font-semibold text-slate-800 dark:text-slate-100">A股全景行情</h1>
          <p className="muted mt-0.5 text-[11px]">字段级来源合并 · 红涨绿跌 · 缺失值不以 0 代替</p>
        </div>
        <button className="btn" onClick={load}>刷新行情</button>
      </div>

      <div className="grid shrink-0 grid-cols-3 gap-3">
        {INDEXES.map((index) => <IndexCard key={index.secid} {...index} />)}
      </div>
      {breadth && <BreadthBar data={breadth} />}
      {error && <ErrorBox message={error} onRetry={load} />}

      <section className="card flex min-h-0 flex-1 flex-col overflow-hidden">
        <div className="flex shrink-0 items-center justify-between border-b border-slate-200 px-3 py-2 dark:border-slate-800">
          <div className="flex items-center gap-3">
            <span className="text-sm font-semibold">全市场证券</span>
            <span className="muted num text-xs">命中 {sorted.length} / {shares?.length ?? 0}</span>
            {shares?.[0] && <span className="muted text-[10px]">数据来源：{sourceDisplayName(shares[0].source)}</span>}
          </div>
          <button className="btn !py-1 text-xs" onClick={() => setFiltersOpen((value) => !value)}>
            {filtersOpen ? "收起筛选" : "展开筛选"}
          </button>
        </div>

        {filtersOpen && (
          <div className="shrink-0 border-b border-slate-200 bg-slate-50/70 p-3 dark:border-slate-800 dark:bg-slate-950/30">
            <div className="grid grid-cols-[minmax(180px,1.5fr)_repeat(3,minmax(110px,0.7fr))_auto] gap-2">
              <input className="input text-xs" placeholder="代码 / 名称" value={keyword} onChange={(event) => setKeyword(event.target.value)} />
              <select className="input text-xs" value={market} onChange={(event) => setMarket(event.target.value)}>
                <option value="all">全部市场</option><option value="SH">上海</option><option value="SZ">深圳</option><option value="BJ">北京</option>
              </select>
              <select className="input text-xs" value={board} onChange={(event) => setBoard(event.target.value)}>
                <option value="all">全部板块</option><option value="main">主板</option><option value="chi_next">创业板</option><option value="star">科创板</option><option value="beijing">北交所</option>
              </select>
              <select className="input text-xs" value={pctFilter} onChange={(event) => setPctFilter(event.target.value as PctFilter)}>
                <option value="all">全部涨跌</option><option value="up">上涨</option><option value="down">下跌</option><option value="flat">平盘</option><option value="limit_up">接近涨停</option><option value="limit_down">接近跌停</option>
              </select>
              <button className="btn text-xs" onClick={resetFilters}>重置</button>
            </div>
            <div className="mt-2 flex flex-wrap items-center gap-2">
              <span className="muted text-[11px]">价格</span>
              <input className="input w-24 text-xs" inputMode="decimal" placeholder="最低" value={minPrice} onChange={(event) => setMinPrice(event.target.value)} />
              <span className="muted">—</span>
              <input className="input w-24 text-xs" inputMode="decimal" placeholder="最高" value={maxPrice} onChange={(event) => setMaxPrice(event.target.value)} />
              <span className="muted ml-2 text-[11px]">最低成交额</span>
              <input className="input w-28 text-xs" inputMode="decimal" placeholder="亿元" value={minAmountYi} onChange={(event) => setMinAmountYi(event.target.value)} />
              <label className="muted ml-2 flex items-center gap-1.5 text-[11px]">
                <input type="checkbox" checked={availableOnly} onChange={(event) => setAvailableOnly(event.target.checked)} />
                仅显示有实时行情
              </label>
              <span className="muted ml-auto text-[10px]">
                <Term label="缺失值" tip="数据源未返回时显示“暂无”，不会伪装成 0 参与筛选或排序" />
              </span>
            </div>
          </div>
        )}

        {!shares ? <Loading text="同步全市场行情与证券主数据…" /> : (
          <>
            <div className="min-h-0 flex-1 overflow-auto">
              <table className="w-full border-collapse text-xs">
                <thead className="sticky top-0 z-10 bg-white shadow-sm dark:bg-slate-900">
                  <tr className="border-b border-slate-200 dark:border-slate-800">
                    {header("code", "代码")}{header("name", "名称")}{header("market", "市场")}{header("board", "板块")}
                    {header("price", "现价", "text-right")}{header("pct", "涨跌幅", "text-right")}{header("amount", "成交额", "text-right")}
                  </tr>
                </thead>
                <tbody>
                  {rows.map((row) => (
                    <tr key={row.code} className="cursor-pointer border-b border-slate-100 hover:bg-blue-50/70 dark:border-slate-800/60 dark:hover:bg-slate-800/70" onClick={() => navigate(`/stock/${row.code}`)}>
                      <td className="td num text-sky-600 dark:text-sky-400">{row.code}</td>
                      <td className="td font-medium">{fmtText(row.name)}</td>
                      <td className="td muted">{row.market}</td>
                      <td className="td"><span className="rounded bg-slate-100 px-1.5 py-0.5 text-[10px] dark:bg-slate-800">{BOARD_LABELS[row.board] ?? row.board}</span></td>
                      <td className={`td num text-right ${pctClass(row.pct)}`}>{fmtNum(row.price)}</td>
                      <td className={`td num text-right font-medium ${pctClass(row.pct)}`}>{fmtPct(row.pct)}</td>
                      <td className="td num text-right">{fmtYiWan(row.amount)}</td>
                    </tr>
                  ))}
                  {rows.length === 0 && <tr><td colSpan={7} className="muted py-16 text-center">没有符合当前条件的证券</td></tr>}
                </tbody>
              </table>
            </div>

            <div className="flex shrink-0 items-center justify-between border-t border-slate-200 px-3 py-2 dark:border-slate-800">
              <div className="muted text-[11px]">
                第 {(currentPage - 1) * pageSize + (rows.length ? 1 : 0)}–{(currentPage - 1) * pageSize + rows.length} 条，共 {sorted.length} 条
              </div>
              <div className="flex items-center gap-1">
                <button className="btn !px-2 !py-1 text-xs" disabled={currentPage === 1} onClick={() => setPage(currentPage - 1)}>上一页</button>
                {pageTokens(currentPage, pageCount).map((token, index) => token === "ellipsis" ? (
                  <span key={`e-${index}`} className="muted px-1">…</span>
                ) : (
                  <button key={token} className={`btn min-w-7 !px-2 !py-1 text-xs ${token === currentPage ? "border-sky-500 bg-sky-500 text-white" : ""}`} onClick={() => setPage(token)}>{token}</button>
                ))}
                <button className="btn !px-2 !py-1 text-xs" disabled={currentPage === pageCount} onClick={() => setPage(currentPage + 1)}>下一页</button>
              </div>
              <label className="muted flex items-center gap-2 text-[11px]">
                每页
                <select className="input !py-1 text-xs" value={pageSize} onChange={(event) => setPageSize(Number(event.target.value))}>
                  <option value={25}>25</option><option value={50}>50</option><option value={100}>100</option><option value={200}>200</option>
                </select>
              </label>
            </div>
          </>
        )}
      </section>
    </div>
  );
}
