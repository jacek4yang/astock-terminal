import type { OrderBook, OrderBookLevel } from "../lib/api";
import { EMPTY_DISPLAY, fmtNum, fmtText, fmtVolume } from "../lib/format";
import { sourceDisplayName } from "../lib/agentLabels";

function Row({ side, row, max }: { side: "bid" | "ask"; row: OrderBookLevel; max: number }) {
  const width = max > 0 ? Math.max(2, (row.volume / max) * 100) : 0;
  return (
    <div className="relative grid grid-cols-[34px_1fr_1fr] items-center px-2 py-0.5 text-[11px]">
      <span
        className={`absolute inset-y-0 right-0 opacity-10 ${side === "bid" ? "bg-up" : "bg-down"}`}
        style={{ width: `${width}%` }}
      />
      <span className={side === "bid" ? "text-up" : "text-down"}>
        {side === "bid" ? "买" : "卖"}{row.level}
      </span>
      <span className={`num text-right ${side === "bid" ? "text-up" : "text-down"}`}>
        {row.price > 0 ? fmtNum(row.price) : EMPTY_DISPLAY}
      </span>
      <span className="num relative text-right">{row.volume > 0 ? fmtVolume(row.volume) : EMPTY_DISPLAY}</span>
    </div>
  );
}

export default function OrderBookPanel({ data }: { data: OrderBook }) {
  const max = Math.max(0, ...data.bids.map((row) => row.volume), ...data.asks.map((row) => row.volume));
  return (
    <div className="card overflow-hidden">
      <div className="card-title justify-between">
        <span>五档盘口</span>
        <span className="muted text-[10px]">{sourceDisplayName(data.source)} · {fmtText(data.server_time)}</span>
      </div>
      <div className="border-b border-slate-100 py-1 dark:border-slate-800/70">
        {[...data.asks].reverse().map((row) => <Row key={`a${row.level}`} side="ask" row={row} max={max} />)}
      </div>
      <div className="py-1">
        {data.bids.map((row) => <Row key={`b${row.level}`} side="bid" row={row} max={max} />)}
      </div>
      <div className="grid grid-cols-3 border-t border-slate-100 px-2 py-1.5 text-center text-[10px] dark:border-slate-800/70">
        <div><div className="muted">现量</div><div className="num">{fmtVolume(data.current_volume)}</div></div>
        <div><div className="muted">外盘</div><div className="num text-up">{fmtVolume(data.outer_volume)}</div></div>
        <div><div className="muted">内盘</div><div className="num text-down">{fmtVolume(data.inner_volume)}</div></div>
      </div>
      {!data.transaction_detail_available && (
        <div className="muted border-t border-slate-100 px-2 py-1.5 text-[10px] dark:border-slate-800/70" title={data.limitation}>
          逐笔成交：当前数据源不支持
        </div>
      )}
    </div>
  );
}
