import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import {
  getIndexKline,
  getMarketBreadth,
  getAllAShares,
  errMsg,
  type Bar,
  type MarketBreadth,
  type AllShare,
} from "../lib/api";
import { fmtPct, fmtYiWan, pctClass } from "../lib/format";
import { Loading, ErrorBox, Term } from "../components/ui";

const INDEXES = [
  { secid: "1.000001", name: "上证指数" },
  { secid: "0.399001", name: "深证成指" },
  { secid: "0.399006", name: "创业板指" },
];

type SortKey = "code" | "name" | "price" | "pct" | "amount";

function IndexCard({ secid, name }: { secid: string; name: string }) {
  const [bar, setBar] = useState<Bar | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    getIndexKline(secid, 2)
      .then((bars) => alive && setBar(bars[bars.length - 1] ?? null))
      .catch((e) => alive && setErr(errMsg(e)));
    return () => {
      alive = false;
    };
  }, [secid]);

  const pct = bar ? bar.pct : null;
  return (
    <div className="card px-4 py-3">
      <div className="muted text-xs">{name}</div>
      {err ? (
        <div className="mt-1 text-xs text-red-500">{err}</div>
      ) : bar ? (
        <>
          <div className={"num mt-1 text-xl font-semibold " + pctClass(pct)}>
            {bar.close.toFixed(2)}
          </div>
          <div className={"num text-xs " + pctClass(pct)}>{fmtPct(pct)}</div>
        </>
      ) : (
        <div className="muted mt-1 text-xs">加载中…</div>
      )}
    </div>
  );
}

function BreadthBar({ data }: { data: MarketBreadth }) {
  const upPct = data.total > 0 ? (data.up / data.total) * 100 : 0;
  const downPct = data.total > 0 ? (data.down / data.total) * 100 : 0;
  return (
    <div className="card px-4 py-3">
      <div className="mb-2 flex items-center justify-between text-xs">
        <span className="text-up num">上涨 {data.up}</span>
        <span className="muted">
          <Term label="涨跌家数" tip="全市场上涨/平盘/下跌的股票数量,反映市场整体情绪" /> · 平盘{" "}
          {data.flat}
        </span>
        <span className="text-down num">下跌 {data.down}</span>
      </div>
      <div className="flex h-2.5 w-full overflow-hidden rounded-full bg-slate-200 dark:bg-slate-800">
        <div className="bg-up" style={{ width: `${upPct}%` }} />
        <div className="flex-1 bg-slate-300 dark:bg-slate-700" />
        <div className="bg-down" style={{ width: `${downPct}%` }} />
      </div>
    </div>
  );
}

export default function MarketPage() {
  const [breadth, setBreadth] = useState<MarketBreadth | null>(null);
  const [shares, setShares] = useState<AllShare[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [sortKey, setSortKey] = useState<SortKey>("amount");
  const [sortAsc, setSortAsc] = useState(false);
  const [filter, setFilter] = useState("");
  const navigate = useNavigate();

  const load = () => {
    setErr(null);
    getMarketBreadth()
      .then(setBreadth)
      .catch((e) => setErr(errMsg(e)));
    getAllAShares()
      .then(setShares)
      .catch((e) => setErr(errMsg(e)));
  };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(load, []);

  const sorted = useMemo(() => {
    if (!shares) return null;
    let list = shares;
    if (filter.trim()) {
      const f = filter.trim().toLowerCase();
      list = list.filter(
        (s) => s.code.includes(f) || s.name.toLowerCase().includes(f),
      );
    }
    const dir = sortAsc ? 1 : -1;
    return [...list].sort((a, b) => {
      const va = a[sortKey];
      const vb = b[sortKey];
      if (typeof va === "string") return va.localeCompare(vb as string) * dir;
      return ((va as number) - (vb as number)) * dir;
    });
  }, [shares, sortKey, sortAsc, filter]);

  const header = (key: SortKey, label: string) => (
    <th
      className="th cursor-pointer select-none hover:text-slate-700 dark:hover:text-slate-200"
      onClick={() => {
        if (sortKey === key) setSortAsc(!sortAsc);
        else {
          setSortKey(key);
          setSortAsc(key === "code" || key === "name");
        }
      }}
    >
      {label}
      {sortKey === key ? (sortAsc ? " ↑" : " ↓") : ""}
    </th>
  );

  return (
    <div className="flex h-full flex-col gap-3 overflow-hidden p-3">
      <h1 className="shrink-0 text-sm font-semibold text-slate-700 dark:text-slate-300">今日市场</h1>
      <div className="stagger grid shrink-0 grid-cols-[repeat(auto-fill,minmax(240px,1fr))] gap-3">
        {INDEXES.map((ix) => (
          <IndexCard key={ix.secid} secid={ix.secid} name={ix.name} />
        ))}
      </div>
      {breadth && (
        <div className="shrink-0">
          <BreadthBar data={breadth} />
        </div>
      )}
      {err && <ErrorBox message={err} onRetry={load} />}
      <div className="card flex min-h-0 flex-1 flex-col overflow-hidden">
        <div className="card-title justify-between">
          <span>
            全市场A股
            {shares && <span className="muted ml-2 font-normal">{shares.length} 只</span>}
          </span>
          <input
            className="input w-48 !py-1 text-xs"
            placeholder="筛选代码/名称"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
        </div>
        {!sorted ? (
          <Loading text="加载全市场行情(数据量大,请稍候)…" />
        ) : (
          <div className="min-h-0 flex-1 overflow-auto">
            <table className="w-full border-collapse">
              <thead className="sticky top-0 z-10 bg-white dark:bg-slate-900">
                <tr className="border-b border-slate-200 dark:border-slate-800">
                  {header("code", "代码")}
                  {header("name", "名称")}
                  {header("price", "现价")}
                  {header("pct", "涨跌幅")}
                  {header("amount", "成交额")}
                </tr>
              </thead>
              <tbody>
                {sorted.slice(0, 500).map((s) => (
                  <tr
                    key={s.code}
                    className="cursor-pointer border-b border-slate-100 hover:bg-slate-50 dark:border-slate-800/50 dark:hover:bg-slate-800/50"
                    onClick={() => navigate(`/stock/${s.code}`)}
                  >
                    <td className="td num muted">{s.code}</td>
                    <td className="td">{s.name}</td>
                    <td className={"td num " + pctClass(s.pct)}>{s.price.toFixed(2)}</td>
                    <td className={"td num " + pctClass(s.pct)}>{fmtPct(s.pct)}</td>
                    <td className="td num">{fmtYiWan(s.amount)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            {sorted.length > 500 && (
              <div className="muted py-2 text-center text-xs">
                仅显示前 500 条,请用筛选缩小范围(共 {sorted.length} 条)
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
