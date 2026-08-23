import { useEffect, useState } from "react";
import { HashRouter, Navigate, Route, Routes } from "react-router-dom";
import Layout from "./components/Layout";
import { BrandLoader } from "./components/ui";
import MarketPage from "./pages/MarketPage";
import NewsPage from "./pages/NewsPage";
import DisclosurePage from "./pages/DisclosurePage";
import GlobalPage from "./pages/GlobalPage";
import WatchlistPage from "./pages/WatchlistPage";
import StockPage from "./pages/StockPage";
import ScanPage from "./pages/ScanPage";
import AgentPage from "./pages/AgentPage";
import BacktestLabPage from "./pages/BacktestLabPage";
import GraphPage from "./pages/GraphPage";
import DataHubPage from "./pages/DataHubPage";
import SettingsPage from "./pages/SettingsPage";
import { applyTheme, useAppStore } from "./store";

export default function App() {
  const theme = useAppStore((s) => s.theme);
  // 启动品牌加载屏:至少展示 450ms,不阻塞路由挂载(覆盖层)
  const [booting, setBooting] = useState(true);

  useEffect(() => {
    applyTheme(theme);
  }, [theme]);

  useEffect(() => {
    const t = setTimeout(() => setBooting(false), 450);
    return () => clearTimeout(t);
  }, []);

  return (
    <>
      <HashRouter>
        <Routes>
          <Route element={<Layout />}>
            <Route path="/" element={<MarketPage />} />
            <Route path="/watchlist" element={<WatchlistPage />} />
            <Route path="/news" element={<NewsPage />} />
            <Route path="/disclosures" element={<DisclosurePage />} />
            <Route path="/global" element={<GlobalPage />} />
            <Route path="/stock/:symbol" element={<StockPage />} />
            <Route path="/scan" element={<ScanPage />} />
            <Route path="/agent" element={<AgentPage />} />
            <Route path="/backtest" element={<BacktestLabPage />} />
            <Route path="/graph" element={<GraphPage />} />
            <Route path="/datahub" element={<DataHubPage />} />
            <Route path="/settings" element={<SettingsPage />} />
            <Route path="*" element={<Navigate to="/" replace />} />
          </Route>
        </Routes>
      </HashRouter>
      {booting && (
        <div className="fixed inset-0 z-[100]">
          <BrandLoader text="正在启动终端…" />
        </div>
      )}
    </>
  );
}
