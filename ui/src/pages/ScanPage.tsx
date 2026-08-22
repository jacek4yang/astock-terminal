import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { scanStart, scanStatus, errMsg, type ScanResultItem } from "../lib/api";
import { onScanProgress, onScanResult, type ScanProgress } from "../lib/events";
import { ErrorBox, Term } from "../components/ui";

export default function ScanPage() {
  const [running, setRunning] = useState(false);
  const [progress, setProgress] = useState<ScanProgress | null>(null);
  const [results, setResults] = useState<ScanResultItem[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const navigate = useNavigate();
  const pollRef = useRef<ReturnType<typeof setInterval>>();

  // 订阅扫描事件(进度 + 增量结果)
  useEffect(() => {
    let unProgress: (() => void) | undefined;
    let unResult: (() => void) | undefined;
    onScanProgress((p) => {
      setProgress(p);
      setRunning(true);
    }).then((u) => (unProgress = u));
    onScanResult((r) => {
      setResults((prev) => (prev.some((x) => x.symbol === r.symbol) ? prev : [...prev, r]));
    }).then((u) => (unResult = u));
    return () => {
      unProgress?.();
      unResult?.();
    };
  }, []);

  // 运行中轮询状态兜底(事件丢失时仍能收敛)
  useEffect(() => {
    if (!running) return;
    pollRef.current = setInterval(async () => {
      try {
        const st = await scanStatus();
        setProgress({ done: st.done, total: st.total, current_symbol: st.current_symbol });
        if (!st.running) {
          setRunning(false);
          if (st.results.length > 0) setResults(st.results);
        }
      } catch {
        /* 忽略轮询错误,等下一轮 */
      }
    }, 1500);
    return () => clearInterval(pollRef.current);
  }, [running]);

  const start = async () => {
    setErr(null);
    setResults([]);
    setProgress(null);
    try {
      await scanStart();
      setRunning(true);
    } catch (e) {
      setErr(errMsg(e));
    }
  };

  const pct =
    progress && progress.total > 0 ? Math.round((progress.done / progress.total) * 100) : 0;

  const sorted = [...results].sort((a, b) => b.score - a.score);

  return (
    <div className="flex h-full flex-col gap-3 overflow-hidden p-3">
      <h1 className="shrink-0 text-sm font-semibold text-slate-700 dark:text-slate-300">智能扫描</h1>
      <div className="card shrink-0 px-4 py-3">
        <div className="flex flex-wrap items-center gap-3">
          <button className="btn-primary" onClick={start} disabled={running}>
            开始扫描
          </button>
          <button
            className="btn-danger"
            disabled={!running}
            onClick={() => setRunning(false)}
            title="停止等待结果(后台任务完成后状态会自动同步)"
          >
            取消
          </button>
          {progress && (
            <span className="muted text-xs">
              {progress.done}/{progress.total}
              {progress.current_symbol && (
                <>
                  {" "}· 正在分析 <span className="num">{progress.current_symbol}</span>
                </>
              )}
            </span>
          )}
          {!running && results.length > 0 && (
            <span className="muted text-xs">完成,共 {results.length} 条信号</span>
          )}
        </div>
        {(running || pct > 0) && (
          <div className="mt-3 h-2 w-full overflow-hidden rounded-full bg-slate-200 dark:bg-slate-800">
            <div
              className="h-full rounded-full bg-blue-600 transition-all"
              style={{ width: `${pct}%` }}
            />
          </div>
        )}
      </div>
      {err && <ErrorBox message={err} onRetry={start} />}
      <div className="card flex min-h-0 flex-1 flex-col overflow-hidden">
        <div className="card-title shrink-0">
          扫描结果
          <span className="muted text-xs font-normal">按评分排序,点击行进入个股工作台</span>
        </div>
        {sorted.length === 0 ? (
          <div className="muted py-8 text-center text-sm">
            {running ? "等待首个结果…" : "点击「开始扫描」对全市场运行信号分析"}
          </div>
        ) : (
          <div className="min-h-0 flex-1 overflow-auto">
            <table className="w-full border-collapse">
              <thead className="sticky top-0 z-10 bg-white dark:bg-slate-900">
              <tr className="border-b border-slate-200 dark:border-slate-800">
                <th className="th">代码</th>
                <th className="th">名称</th>
                <th className="th">
                  <Term label="评分" tip="趋势/形态/量价/突破/CANSLIM 五模块加权综合分(0-100)" />
                </th>
                <th className="th">操作</th>
                <th className="th">
                  <Term label="置信度" tip="信号可靠程度(0-100),越高代表各模块结论越一致" />
                </th>
              </tr>
            </thead>
            <tbody>
              {sorted.map((r) => (
                <tr
                  key={r.symbol}
                  className="cursor-pointer border-b border-slate-100 hover:bg-slate-50 dark:border-slate-800/50 dark:hover:bg-slate-800/50"
                  onClick={() => navigate(`/stock/${r.symbol}`)}
                >
                  <td className="td num muted">{r.symbol}</td>
                  <td className="td">{r.name}</td>
                  <td className="td">
                    <div className="flex items-center gap-2">
                      <div className="h-1.5 w-16 overflow-hidden rounded-full bg-slate-200 dark:bg-slate-800">
                        <div
                          className={
                            "h-full " +
                            (r.score >= 70 ? "bg-up" : r.score >= 45 ? "bg-amber-500" : "bg-down")
                          }
                          style={{ width: `${Math.min(100, r.score)}%` }}
                        />
                      </div>
                      <span className="num">{r.score}</span>
                    </div>
                  </td>
                  <td className="td">
                    <span
                      className={
                        "tag " +
                        (r.action.includes("买")
                          ? "bg-up/10 text-up"
                          : r.action.includes("卖")
                            ? "bg-down/10 text-down"
                            : "bg-slate-200 text-slate-600 dark:bg-slate-800 dark:text-slate-300")
                      }
                    >
                      {r.action}
                    </span>
                  </td>
                  <td className="td num">{r.confidence}</td>
                </tr>
              ))}
            </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  );
}
