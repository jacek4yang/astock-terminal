import { lazy, Suspense, type ComponentType, type FunctionComponent } from "react";
import type { IDockviewPanelProps } from "dockview-react";
import { useParams } from "react-router-dom";
import AgentTaskWorkbench from "./AgentTaskWorkbench";
import DiagnosticsPanel from "./DiagnosticsPanel";
import DesktopSettingsPanel from "./DesktopSettingsPanel";
import MarketChartPanel from "./MarketChartPanel";

const MarketPage = lazy(() => import("../pages/MarketPage"));
const StockPage = lazy(() => import("../pages/StockPage"));
const NewsPage = lazy(() => import("../pages/NewsPage"));
const GraphPage = lazy(() => import("../pages/GraphPage"));
const BacktestLabPage = lazy(() => import("../pages/BacktestLabPage"));
const DataHubPage = lazy(() => import("../pages/DataHubPage"));
const WatchlistPage = lazy(() => import("../pages/WatchlistPage"));
const DisclosurePage = lazy(() => import("../pages/DisclosurePage"));
const GlobalPage = lazy(() => import("../pages/GlobalPage"));
const ScanPage = lazy(() => import("../pages/ScanPage"));
const QuantResearchWorkbench = lazy(() => import("../components/QuantResearchWorkbench"));
const SourceEvidenceWorkbench = lazy(() => import("../components/SourceEvidenceWorkbench"));

function PanelFrame({ children }: { children: React.ReactNode }) {
  return <div className="workbench-panel-scroll"><Suspense fallback={<div className="panel-loader">正在加载面板…</div>}>{children}</Suspense></div>;
}

function wrap(Component: ComponentType) {
  return function RegisteredPanel(_props: IDockviewPanelProps) { return <PanelFrame><Component /></PanelFrame>; };
}

function StockPanel() {
  const { symbol } = useParams();
  if (!symbol) return <div className="empty-state panel-empty"><strong>选择证券后打开行情研究面板</strong><span>在顶部输入证券代码，或从市场与自选面板中进入。</span></div>;
  return <PanelFrame><StockPage /></PanelFrame>;
}

export const panelComponents: Record<string, FunctionComponent<IDockviewPanelProps>> = {
  market: wrap(MarketChartPanel),
  marketList: wrap(MarketPage),
  security: StockPanel,
  watchlist: wrap(WatchlistPage),
  agentTask: wrap(AgentTaskWorkbench),
  news: wrap(NewsPage),
  disclosures: wrap(DisclosurePage),
  global: wrap(GlobalPage),
  scanner: wrap(ScanPage),
  graph: wrap(GraphPage),
  quant: wrap(QuantResearchWorkbench),
  backtest: wrap(BacktestLabPage),
  evidence: wrap(SourceEvidenceWorkbench),
  datahub: wrap(DataHubPage),
  settings: wrap(DesktopSettingsPanel),
  diagnostics: wrap(DiagnosticsPanel),
};

export const panelTitles: Record<string, string> = {
  market: "大盘行情", marketList: "全市场报价", security: "个股行情", watchlist: "自选监控", agentTask: "Agent 研究",
  news: "新闻与事件", disclosures: "公告", global: "全球市场", scanner: "机会扫描",
  graph: "关系图谱", quant: "Quant Lab", backtest: "回测", evidence: "证据与来源",
  datahub: "数据中心", settings: "设置", diagnostics: "Diagnostics",
};

export type PanelId = keyof typeof panelComponents;
