import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import {
  watchlistList,
  watchlistAdd,
  watchlistRemove,
  watchlistPin,
  getQuote,
  errMsg,
  type WatchlistItem,
  type Quote,
} from "../lib/api";
import { fmtPct, pctClass } from "../lib/format";
import { Loading, ErrorBox, EmptyBox, Term } from "../components/ui";

export default function WatchlistPage() {
  const [items, setItems] = useState<WatchlistItem[] | null>(null);
  const [quotes, setQuotes] = useState<Record<string, Quote>>({});
  const [err, setErr] = useState<string | null>(null);
  const [newCode, setNewCode] = useState("");
  const [newGroup, setNewGroup] = useState("默认");
  const [busy, setBusy] = useState(false);
  const navigate = useNavigate();
  const quotesRef = useRef(quotes);
  quotesRef.current = quotes;

  const load = useCallback(async () => {
    try {
      setItems(await watchlistList());
      setErr(null);
    } catch (e) {
      setErr(errMsg(e));
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  // 实时报价轮询 2s
  useEffect(() => {
    if (!items || items.length === 0) return;
    let alive = true;
    const tick = async () => {
      const codes = items.map((i) => i.code);
      const results = await Promise.allSettled(codes.map((c) => getQuote(c)));
      if (!alive) return;
      const next: Record<string, Quote> = { ...quotesRef.current };
      results.forEach((r, i) => {
        if (r.status === "fulfilled") next[codes[i]] = r.value;
      });
      setQuotes(next);
    };
    tick();
    const timer = setInterval(tick, 2000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [items]);

  const groups = items
    ? [...new Set(items.map((i) => i.group_name))].sort((a, b) => a.localeCompare(b))
    : [];

  const add = async () => {
    const code = newCode.trim();
    if (!code) return;
    setBusy(true);
    try {
      await watchlistAdd(code, newGroup.trim() || "默认");
      setNewCode("");
      await load();
    } catch (e) {
      setErr(errMsg(e));
    } finally {
      setBusy(false);
    }
  };

  const remove = async (item: WatchlistItem) => {
    try {
      await watchlistRemove(item.code, item.group_name);
      await load();
    } catch (e) {
      setErr(errMsg(e));
    }
  };

  const pin = async (item: WatchlistItem) => {
    try {
      await watchlistPin(item.code, item.group_name, !item.pinned);
      await load();
    } catch (e) {
      setErr(errMsg(e));
    }
  };

  return (
    <div className="flex h-full flex-col gap-3 overflow-hidden p-3">
      <h1 className="shrink-0 text-sm font-semibold text-slate-700 dark:text-slate-300">自选股</h1>
      <div className="card flex shrink-0 flex-wrap items-center gap-2 px-4 py-3">
        <input
          className="input w-32"
          placeholder="股票代码"
          value={newCode}
          onChange={(e) => setNewCode(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && add()}
        />
        <input
          className="input w-28"
          placeholder="分组(默认)"
          value={newGroup}
          onChange={(e) => setNewGroup(e.target.value)}
        />
        <button className="btn-primary" onClick={add} disabled={busy || !newCode.trim()}>
          添加自选
        </button>
        <span className="muted text-xs">报价每 2 秒自动刷新</span>
      </div>
      {err && <ErrorBox message={err} onRetry={load} />}
      <div className="min-h-0 flex-1 space-y-3 overflow-y-auto">
        {!items ? (
          <Loading />
        ) : items.length === 0 ? (
          <div className="card">
            <EmptyBox text="暂无自选股,添加代码或在个股页加入自选" />
          </div>
        ) : (
          groups.map((g) => {
          const list = items
            .filter((i) => i.group_name === g)
            .sort((a, b) => Number(b.pinned) - Number(a.pinned) || a.code.localeCompare(b.code));
          return (
            <div className="card" key={g}>
              <div className="card-title">
                {g}
                <span className="muted font-normal">{list.length} 只</span>
              </div>
              <table className="w-full border-collapse">
                <thead>
                  <tr className="border-b border-slate-200 dark:border-slate-800">
                    <th className="th w-8"></th>
                    <th className="th">代码</th>
                    <th className="th">名称</th>
                    <th className="th">现价</th>
                    <th className="th">涨跌幅</th>
                    <th className="th">
                      <Term label="换手率" tip="当日成交量占流通股本的比例,反映交投活跃度" />
                    </th>
                    <th className="th w-24 text-right">操作</th>
                  </tr>
                </thead>
                <tbody>
                  {list.map((item) => {
                    const q = quotes[item.code];
                    return (
                      <tr
                        key={item.code}
                        className="cursor-pointer border-b border-slate-100 hover:bg-slate-50 dark:border-slate-800/50 dark:hover:bg-slate-800/50"
                        onClick={() => navigate(`/stock/${item.code}`)}
                      >
                        <td className="td">
                          <button
                            className={"text-sm " + (item.pinned ? "text-amber-500" : "muted")}
                            title={item.pinned ? "取消置顶" : "置顶"}
                            onClick={(e) => {
                              e.stopPropagation();
                              pin(item);
                            }}
                          >
                            ★
                          </button>
                        </td>
                        <td className="td num muted">{item.code}</td>
                        <td className="td">{q?.name ?? item.name ?? "--"}</td>
                        <td className={"td num " + pctClass(q?.pct)}>
                          {q ? q.price.toFixed(2) : "--"}
                        </td>
                        <td className={"td num " + pctClass(q?.pct)}>
                          {q ? fmtPct(q.pct) : "--"}
                        </td>
                        <td className="td num">{q ? q.turnover.toFixed(2) + "%" : "--"}</td>
                        <td className="td text-right">
                          <button
                            className="btn"
                            onClick={(e) => {
                              e.stopPropagation();
                              remove(item);
                            }}
                          >
                            删除
                          </button>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          );
          })
        )}
      </div>
    </div>
  );
}
