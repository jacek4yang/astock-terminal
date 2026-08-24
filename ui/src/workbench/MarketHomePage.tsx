import { lazy, Suspense, useState, type CSSProperties } from "react";
import MarketChartPanel from "./MarketChartPanel";
import { useWorkspaceStore } from "./store";

const MarketPage = lazy(() => import("../pages/MarketPage"));
const WatchlistPage = lazy(() => import("../pages/WatchlistPage"));

type MarketView = "overview" | "watchlist";

export default function MarketHomePage() {
  const [view, setView] = useState<MarketView>("overview");
  const chartHeight = useWorkspaceStore((state) => state.marketChartHeight);
  const setChartHeight = useWorkspaceStore((state) => state.setMarketChartHeight);

  const beginResize = (event: React.PointerEvent) => {
    event.preventDefault();
    const startY = event.clientY;
    const startHeight = chartHeight;
    const move = (next: PointerEvent) => setChartHeight(startHeight + next.clientY - startY);
    const stop = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", stop);
      document.body.classList.remove("workbench-resizing");
    };
    document.body.classList.add("workbench-resizing");
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", stop, { once: true });
  };

  return <div className="market-home">
    <div className="market-home-tabs" role="tablist" aria-label="今日市场视图">
      <button className={view === "overview" ? "active" : ""} onClick={() => setView("overview")}>大盘行情</button>
      <button className={view === "watchlist" ? "active" : ""} onClick={() => setView("watchlist")}>自选股</button>
      <span />
      <small>点击任意股票进入完整个股详情</small>
    </div>
    <Suspense fallback={<div className="panel-loader">正在载入本地行情界面…</div>}>
      {view === "overview" ? <div className="market-overview-scroll" style={{ "--market-chart-height": `${chartHeight}px` } as CSSProperties}>
        <section className="market-primary-chart" aria-label="大盘行情图表"><MarketChartPanel /></section>
        <button className="market-section-resizer" aria-label="调整大盘图表高度" title="拖动调整图表与行情列表比例" onPointerDown={beginResize}><span>拖动调整比例</span></button>
        <section className="market-full-list" aria-label="全市场证券"><MarketPage /></section>
      </div> : <section className="market-watchlist-full"><WatchlistPage /></section>}
    </Suspense>
  </div>;
}
