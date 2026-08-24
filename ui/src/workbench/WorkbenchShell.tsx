import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { isProton, requestNative } from "../bridge";
import logo from "../assets/logo.svg";
import { searchStocks } from "../lib/api";
import AgentTaskWorkbench from "./AgentTaskWorkbench";
import DesktopSettingsPanel from "./DesktopSettingsPanel";
import MarketHomePage from "./MarketHomePage";
import MarketSessionClock from "./MarketSessionClock";
import { useRuntimeStatus } from "./runtime";
import { useResearchContext, useWorkspaceStore, type WorkbenchPreset } from "./store";

const StockPage = lazy(() => import("../pages/StockPage"));
const ReleasePerformanceHarness = lazy(() => import("./ReleasePerformanceHarness"));

const primaryNavigation: Array<[WorkbenchPreset, string, string, string]> = [
  ["market", "今日市场", "大盘、自选与完整个股资料", "M3 12h4l3-8 4 16 3-8h4"],
  ["agent", "Agent 智研", "复杂分析、工具与证据核验", "M12 3v3m0 12v3M3 12h3m12 0h3M8 8h8v8H8zM10.5 11.5h.01M13.5 11.5h.01M10.5 14.5h3"],
  ["settings", "配置", "模型、数据与桌面诊断", "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8zM4 12h2m12 0h2M12 4v2m0 12v2"],
];

const pageCopy: Record<"market" | "agent" | "settings" | "stock", [string, string]> = {
  market: ["今日市场", "先看全局，再进入个股；行情、资料和研究数据均来自本地桌面服务"],
  agent: ["Agent 智研", "用自然语言发起复杂研究，查看工具进度、证据和经过校验的结论"],
  settings: ["配置", "管理模型、研究工具、数据目录和桌面运行状态"],
  stock: ["个股详情", "行情、盘口、趋势、资金、基本面、估值与研究证据"],
};

function AppIcon({ path }: { path: string }) {
  return <svg viewBox="0 0 24 24" aria-hidden="true"><path d={path} /></svg>;
}

export default function WorkbenchShell() {
  const securityInputRef = useRef<HTMLInputElement | null>(null);
  const windowDragStartRef = useRef<{ pointerId: number; x: number; y: number } | null>(null);
  const navigate = useNavigate();
  const { symbol: routeSymbol } = useParams();
  const [securityInput, setSecurityInput] = useState(routeSymbol ?? "");
  const [securityError, setSecurityError] = useState<string | null>(null);
  const [securityBusy, setSecurityBusy] = useState(false);
  const preset = useWorkspaceStore((state) => state.preset);
  const setPreset = useWorkspaceStore((state) => state.setPreset);
  const context = useResearchContext();
  const runtime = useRuntimeStatus();

  useEffect(() => {
    if (!routeSymbol) return;
    setSecurityInput(routeSymbol);
    context.setSecurity(routeSymbol);
  }, [routeSymbol]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    const listener = (event: KeyboardEvent) => {
      if (event.key === "F6") {
        event.preventDefault();
        securityInputRef.current?.focus();
        securityInputRef.current?.select();
      }
    };
    window.addEventListener("keydown", listener);
    return () => window.removeEventListener("keydown", listener);
  }, []);

  const submitSecurity = async (event: React.FormEvent) => {
    event.preventDefault();
    const query = securityInput.trim();
    if (!query) return;
    setSecurityBusy(true);
    setSecurityError(null);
    try {
      let next = query.toUpperCase();
      let name: string | null = null;
      if (!/^\d{6}$/.test(next)) {
        const hit = (await searchStocks(query))[0];
        if (!hit) throw new Error(`未找到“${query}”对应的 A 股证券`);
        next = hit.code;
        name = hit.name;
      }
      setSecurityInput(next);
      context.setSecurity(next, name);
      navigate(`/stock/${encodeURIComponent(next)}`);
    } catch (cause) {
      setSecurityError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSecurityBusy(false);
    }
  };

  const openPrimary = (next: WorkbenchPreset) => {
    navigate("/");
    setPreset(next);
  };

  const activePrimary: "market" | "agent" | "settings" = routeSymbol ? "market" : preset === "settings" ? "settings" : preset === "agent" ? "agent" : "market";
  const pageKind = routeSymbol ? "stock" : activePrimary;
  const [pageTitle, pageSubtitle] = pageCopy[pageKind];
  const releasePerformanceMode = isProton()
    && new URLSearchParams(window.location.search).get("releasePerf") === "1";

  const isWindowDragSurface = (target: EventTarget | null) => {
    if (!(target instanceof Element)) return true;
    return !target.closest("button, input, select, textarea, a, [role='button'], [data-window-no-drag]");
  };

  const handleTitlebarPointerDown = (event: React.PointerEvent<HTMLElement>) => {
    if (!isProton() || event.button !== 0 || !isWindowDragSurface(event.target)) return;
    windowDragStartRef.current = { pointerId: event.pointerId, x: event.screenX, y: event.screenY };
    event.currentTarget.setPointerCapture?.(event.pointerId);
  };

  const handleTitlebarPointerMove = (event: React.PointerEvent<HTMLElement>) => {
    const start = windowDragStartRef.current;
    if (!start || start.pointerId !== event.pointerId || (Math.abs(event.screenX - start.x) < 4 && Math.abs(event.screenY - start.y) < 4)) return;
    windowDragStartRef.current = null;
    if (event.currentTarget.hasPointerCapture?.(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
    void requestNative("host", "window.begin_drag").catch(() => undefined);
  };

  const clearTitlebarPointer = (event: React.PointerEvent<HTMLElement>) => {
    if (windowDragStartRef.current?.pointerId === event.pointerId) windowDragStartRef.current = null;
    if (event.currentTarget.hasPointerCapture?.(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
  };

  const handleTitlebarDoubleClick = (event: React.MouseEvent<HTMLElement>) => {
    if (!isProton() || !isWindowDragSurface(event.target)) return;
    event.preventDefault();
    void requestNative("host", "window.toggle_maximize").catch(() => undefined);
  };

  const handleTitlebarContextMenu = (event: React.MouseEvent<HTMLElement>) => {
    if (!isProton() || !isWindowDragSurface(event.target)) return;
    event.preventDefault();
    void requestNative("host", "window.system_menu").catch(() => undefined);
  };

  if (releasePerformanceMode) {
    return <Suspense fallback={<div className="panel-loader">正在载入打包性能夹具…</div>}>
      <ReleasePerformanceHarness />
    </Suspense>;
  }

  return <div className="classic-shell">
    <header
      className="classic-titlebar"
      onPointerDown={handleTitlebarPointerDown}
      onPointerMove={handleTitlebarPointerMove}
      onPointerUp={clearTitlebarPointer}
      onPointerCancel={clearTitlebarPointer}
      onDoubleClick={handleTitlebarDoubleClick}
      onContextMenu={handleTitlebarContextMenu}
    >
      <div className="classic-brand"><img className="brand-mark" src={logo} alt="" /><div><b>趋势智研</b><small>ASTOCK TERMINAL</small></div></div>
      <form className="security-command" onSubmit={submitSecurity}>
        <span className="scope-chip">沪深京</span>
        <input ref={securityInputRef} value={securityInput} onChange={(event) => { setSecurityInput(event.target.value); setSecurityError(null); }} placeholder="输入股票代码 / 名称，回车打开" aria-label="证券搜索" />
        <span className="shortcut-hint">{securityBusy ? "查询中" : "F6"}</span>
        {securityError && <span className="security-search-error">{securityError}</span>}
      </form>
      <MarketSessionClock />
      <div className="window-drag-region" aria-hidden="true" />
      <div className="runtime-indicator" title={runtime.error ?? "本地桌面运行状态"}><span className={runtime.status?.status === "ready" ? "connected" : isProton() ? "pending" : ""} />{runtime.status?.status === "ready" ? "桌面服务就绪" : isProton() ? "正在启动" : "浏览器无行情"}</div>
    </header>

    <aside className="classic-sidebar">
      <div className="sidebar-heading">功能</div>
      <nav aria-label="主功能导航">
        {primaryNavigation.map(([key, label, detail, icon]) => <button key={key} className={`${activePrimary === key && !routeSymbol ? "active" : ""} ${key === "agent" ? "agent-primary" : ""}`} onClick={() => openPrimary(key)}>
          <AppIcon path={icon} /><span><b>{label}</b><small>{detail}</small></span>{key === "agent" && <em>核心</em>}
        </button>)}
      </nav>
      <section className="sidebar-policy"><b>研究边界</b><p>只生成研究结论与人工交易计划，不连接券商，不自动下单。</p></section>
    </aside>

    <header className="classic-page-header">
      {routeSymbol && <button className="page-back" onClick={() => { navigate("/"); setPreset("market"); }}>‹ 返回市场</button>}
      <div><h1>{routeSymbol ? `${context.symbolName ?? routeSymbol} · ${pageTitle}` : pageTitle}</h1><p>{pageSubtitle}</p></div>
      {routeSymbol && <span className="page-symbol">{routeSymbol}</span>}
    </header>

    <main className={`classic-content page-${pageKind}`}>
      <Suspense fallback={<div className="panel-loader">正在载入本地界面…</div>}>
        {!routeSymbol && <div className={`persistent-agent-surface ${activePrimary === "agent" ? "active" : ""}`} aria-hidden={activePrimary !== "agent"}>
          <AgentTaskWorkbench />
        </div>}
        {routeSymbol ? <StockPage /> : activePrimary === "settings" ? <DesktopSettingsPanel /> : activePrimary === "market" ? <MarketHomePage /> : null}
      </Suspense>
    </main>

    <footer className="classic-status"><span><i className={runtime.status?.status === "ready" ? "ok" : ""} />{runtime.status ? "Host ready" : isProton() ? "Host connecting" : "浏览器预览 · 无行情"}</span><span>Engine {runtime.status?.engine?.status ?? "—"}</span><span>Agent {runtime.status?.agent?.status ?? "—"}</span><span className="status-spacer" /><span>只读研究 · 不连接券商下单</span><span>Windows x64</span></footer>
  </div>;
}
