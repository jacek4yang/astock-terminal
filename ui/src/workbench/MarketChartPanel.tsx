import { useEffect, useMemo, useState } from "react";
import KlineChart, { type SubIndicator } from "../components/KlineChart";
import { ErrorBox, Loading } from "../components/ui";
import { errMsg, getIndexKline, type Bar } from "../lib/api";
import { fmtNum, fmtPct, pctClass } from "../lib/format";

const INDEXES = [
  { secid: "1.000001", code: "000001", name: "上证指数" },
  { secid: "0.399001", code: "399001", name: "深证成指" },
  { secid: "0.399006", code: "399006", name: "创业板指" },
  { secid: "1.000688", code: "000688", name: "科创50" },
] as const;

const INDICATORS: SubIndicator[] = ["MACD", "KDJ", "RSI", "BOLL"];

export default function MarketChartPanel() {
  const [index, setIndex] = useState<(typeof INDEXES)[number]>(INDEXES[0]);
  const [bars, setBars] = useState<Bar[] | null>(null);
  const [indicator, setIndicator] = useState<SubIndicator>("MACD");
  const [error, setError] = useState<string | null>(null);
  const [refresh, setRefresh] = useState(0);

  useEffect(() => {
    let live = true;
    setBars(null);
    setError(null);
    getIndexKline(index.secid, 500)
      .then((rows) => live && setBars(rows))
      .catch((cause) => live && setError(errMsg(cause)));
    return () => { live = false; };
  }, [index, refresh]);

  const latest = bars?.at(-1) ?? null;
  const previous = bars && bars.length > 1 ? bars.at(-2) ?? null : null;
  const change = latest && previous ? latest.close - previous.close : null;
  const pct = latest?.pct ?? (latest && previous && previous.close ? (change! / previous.close) * 100 : null);
  const zoomStart = useMemo(() => bars?.length ? Math.max(0, 100 * (1 - 160 / bars.length)) : 0, [bars]);

  return <div className="market-chart-panel">
    <div className="quote-toolbar">
      <div className="index-tabs">{INDEXES.map((item) => <button key={item.secid} className={item.secid === index.secid ? "active" : ""} onClick={() => setIndex(item)}>{item.name}</button>)}</div>
      <span className="toolbar-divider" />
      <button className="active">日线</button><button disabled>周线</button><button disabled>月线</button>
      <span className="toolbar-divider" />
      <span className="toolbar-label">复权</span><button className="active">不复权</button>
      <span className="toolbar-spacer" />
      <button title="重新读取行情" onClick={() => setRefresh((value) => value + 1)}>刷新</button>
    </div>

    <div className="instrument-strip">
      <b>{index.name}</b><span className="num muted">{index.code}</span>
      {latest && <><strong className={pctClass(pct)}>{fmtNum(latest.close)}</strong><span className={`num ${pctClass(pct)}`}>{change != null && change >= 0 ? "+" : ""}{fmtNum(change)}&nbsp;&nbsp;{fmtPct(pct)}</span><span>开 {fmtNum(latest.open)}</span><span>高 {fmtNum(latest.high)}</span><span>低 {fmtNum(latest.low)}</span></>}
      <span className="toolbar-spacer" />
      <span className="market-session">A股 · 日线 · 行情只读</span>
    </div>

    <div className="chart-stage">
      {error ? <ErrorBox message={error} onRetry={() => setRefresh((value) => value + 1)} /> : !bars ? <Loading text="正在从本地 Engine 同步指数行情…" /> : <KlineChart bars={bars} indicator={indicator} zoomStart={zoomStart} height="100%" />}
    </div>
    <div className="indicator-strip"><span>副图指标</span>{INDICATORS.map((item) => <button key={item} className={indicator === item ? "active" : ""} onClick={() => setIndicator(item)}>{item}</button>)}<span className="toolbar-spacer" /><span className="muted">滚轮缩放 · 拖拽平移 · 十字光标查看明细</span></div>
  </div>;
}
