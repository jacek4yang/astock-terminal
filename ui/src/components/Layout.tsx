import { useEffect, useMemo, useRef, useState } from "react";
import { NavLink, Outlet, useLocation, useNavigate } from "react-router-dom";
import { searchStocks, type SearchResult } from "../lib/api";
import { useAppStore } from "../store";
import AgentChat from "./AgentChat";
import ErrorBoundary from "./ErrorBoundary";
import logo from "../assets/logo.svg";

function NavIcon({ d }: { d: string }) {
  return (
    <svg
      className="h-4 w-4 shrink-0"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d={d} />
    </svg>
  );
}

const AGENT_ICON = "M12 3v3m0 12v3M3 12h3m12 0h3M8 8h8v8H8zM10.5 11.5h.01M13.5 11.5h.01M10.5 14.5h3";

const NAV = [
  { to: "/", label: "今日市场", icon: "M3 12h4l3-8 4 16 3-8h4" },
  { to: "/news", label: "资讯中心", icon: "M4 5h16v14H4zM7 9h3m-3 3h10m-10 3h10M13 9h4" },
  { to: "/agent", label: "智能助手", icon: AGENT_ICON },
  { to: "/watchlist", label: "自选股", icon: "M12 2l3 7h7l-5.5 4.5L18 21l-6-4-6 4 1.5-7.5L2 9h7z" },
  { to: "/scan", label: "智能扫描", icon: "M11 4a7 7 0 1 0 4.9 12L21 21M11 8v3l2 2" },
  { to: "/datahub", label: "数据中心", icon: "M4 19V5m0 14h16M8 15v-4m4 4V8m4 7v-6" },
  { to: "/backtest", label: "回测实验室", icon: "M9 17v-6M13 17V7M17 17v-3M5 21h14" },
  { to: "/graph", label: "图谱分析", icon: "M12 5a2 2 0 1 0 0 .01M5 19a2 2 0 1 0 0 .01M19 19a2 2 0 1 0 0 .01M12 7l-5.6 10M12 7l5.6 10M7 19h10" },
  { to: "/settings", label: "设置", icon: "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8zM4 12h2m12 0h2M12 4v2m0 12v2" },
];

function navCls(isActive: boolean): string {
  return (
    "relative flex h-9 items-center gap-2.5 rounded px-3 text-[13px] transition-colors duration-150 " +
    (isActive
      ? "bg-blue-600/10 font-medium text-blue-600 dark:text-blue-400"
      : "text-slate-600 hover:bg-slate-100 dark:text-slate-300 dark:hover:bg-slate-800")
  );
}

/** 激活项左侧 3px 品牌 tick */
function ActiveTick() {
  return (
    <span className="absolute left-0 top-1/2 h-4 w-[3px] -translate-y-1/2 rounded-r-full bg-blue-600" />
  );
}

function StockSearch({ compact = false }: { compact?: boolean }) {
  const [kw, setKw] = useState("");
  const [results, setResults] = useState<SearchResult[]>([]);
  const [open, setOpen] = useState(false);
  const navigate = useNavigate();
  const boxRef = useRef<HTMLDivElement>(null);
  const timer = useRef<ReturnType<typeof setTimeout>>();

  useEffect(() => {
    const onClick = (e: MouseEvent) => {
      if (boxRef.current && !boxRef.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onClick);
    return () => document.removeEventListener("mousedown", onClick);
  }, []);

  const onChange = (v: string) => {
    setKw(v);
    clearTimeout(timer.current);
    if (!v.trim()) {
      setResults([]);
      setOpen(false);
      return;
    }
    timer.current = setTimeout(async () => {
      try {
        const r = await searchStocks(v.trim());
        setResults(r.slice(0, 10));
        setOpen(true);
      } catch {
        setResults([]);
      }
    }, 250);
  };

  const go = (code: string) => {
    setOpen(false);
    setKw("");
    setResults([]);
    navigate(`/stock/${code}`);
  };

  return (
    <div ref={boxRef} className={compact ? "relative" : "relative px-3"}>
      <input
        className="input w-full text-xs"
        placeholder="搜索股票代码/名称"
        value={kw}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && results.length > 0) go(results[0].code);
        }}
      />
      {open && results.length > 0 && (
        <div className="card anim-fade-in absolute left-3 right-3 top-full z-50 mt-1 max-h-64 overflow-auto py-1 shadow-lg">
          {results.map((r) => (
            <button
              key={r.code}
              className="flex w-full items-center justify-between px-3 py-1.5 text-left text-xs hover:bg-slate-100 dark:hover:bg-slate-800"
              onClick={() => go(r.code)}
            >
              <span>{r.name}</span>
              <span className="num muted">{r.code}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

/** 全局呼出 Agent:浮动按钮 + 右侧抽屉(非 AI 助手页显示) */
function AgentLauncher() {
  const [open, setOpen] = useState(false);
  const [width, setWidth] = useState(() => Number(localStorage.getItem("agent-drawer-width")) || 640);
  const [resizing, setResizing] = useState(false);
  const location = useLocation();
  const onAgentPage = location.pathname.startsWith("/agent");

  useEffect(() => {
    if (!resizing) return;
    const move = (event: MouseEvent) => setWidth(Math.max(420, Math.min(960, window.innerWidth - event.clientX)));
    const up = () => {
      setResizing(false);
      localStorage.setItem("agent-drawer-width", String(width));
    };
    document.addEventListener("mousemove", move);
    document.addEventListener("mouseup", up, { once: true });
    return () => {
      document.removeEventListener("mousemove", move);
      document.removeEventListener("mouseup", up);
    };
  }, [resizing, width]);

  if (onAgentPage) return null;

  return (
    <>
      {!open && (
        <button
          className="ease-app fixed bottom-12 right-5 z-40 flex h-11 w-11 items-center justify-center rounded-full bg-blue-600 text-white shadow-lg shadow-blue-600/30 transition-all duration-150 hover:-translate-y-0.5 hover:bg-blue-500 hover:shadow-xl"
          title="打开智能助手"
          onClick={() => setOpen(true)}
        >
          <svg
            className="h-5 w-5"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.8"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <path d={AGENT_ICON} />
          </svg>
        </button>
      )}
      {open && (
        <div className="fixed inset-0 z-50">
          <div
            className="anim-fade-in absolute inset-0 bg-slate-950/40"
            onClick={() => setOpen(false)}
          />
          <div
            className="anim-drawer-in brand-bar-top absolute inset-y-0 right-0 max-w-[96vw] overflow-hidden bg-white shadow-2xl dark:bg-slate-950"
            style={{ width }}
          >
            <button
              aria-label="调整 Agent 宽度"
              className="absolute inset-y-0 left-0 z-20 w-1 cursor-col-resize bg-transparent hover:bg-blue-500/70"
              onMouseDown={() => setResizing(true)}
            />
            <ErrorBoundary scope="智能助手" compact>
              <AgentChat variant="drawer" onClose={() => setOpen(false)} />
            </ErrorBoundary>
          </div>
        </div>
      )}
    </>
  );
}

export default function Layout() {
  const currentSymbol = useAppStore((s) => s.currentSymbol);
  const currentName = useAppStore((s) => s.currentName);
  const mode = useAppStore((s) => s.mode);
  const theme = useAppStore((s) => s.theme);
  const setTheme = useAppStore((s) => s.setTheme);
  const location = useLocation();
  const [sidebarWidth, setSidebarWidth] = useState(
    () => Number(localStorage.getItem("terminal-sidebar-width")) || 208,
  );
  const [collapsed, setCollapsed] = useState(false);
  const [resizing, setResizing] = useState(false);
  const [clock, setClock] = useState(() => new Date());

  useEffect(() => {
    const timer = setInterval(() => setClock(new Date()), 1000);
    return () => clearInterval(timer);
  }, []);

  useEffect(() => {
    if (!resizing) return;
    const move = (event: MouseEvent) => setSidebarWidth(Math.max(176, Math.min(320, event.clientX)));
    const up = () => {
      setResizing(false);
      localStorage.setItem("terminal-sidebar-width", String(sidebarWidth));
    };
    document.addEventListener("mousemove", move);
    document.addEventListener("mouseup", up, { once: true });
    return () => {
      document.removeEventListener("mousemove", move);
      document.removeEventListener("mouseup", up);
    };
  }, [resizing, sidebarWidth]);

  const pageTitle = useMemo(() => {
    if (location.pathname.startsWith("/stock/")) return currentName || currentSymbol || "个股工作台";
    return NAV.find((item) => item.to === location.pathname)?.label ?? "研究工作台";
  }, [location.pathname, currentName, currentSymbol]);
  const chinaTime = new Intl.DateTimeFormat("zh-CN", {
    timeZone: "Asia/Shanghai",
    hour12: false,
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(clock);
  const marketOpen = (() => {
    const parts = new Intl.DateTimeFormat("en-US", {
      timeZone: "Asia/Shanghai",
      weekday: "short",
      hour12: false,
      hour: "2-digit",
      minute: "2-digit",
    }).formatToParts(clock);
    const part = (type: string) => parts.find((item) => item.type === type)?.value ?? "0";
    const day = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"].indexOf(part("weekday"));
    const minutes = Number(part("hour")) * 60 + Number(part("minute"));
    return day > 0 && day < 6 && ((minutes >= 570 && minutes < 690) || (minutes >= 780 && minutes < 900));
  })();

  return (
    <div className="flex h-screen w-screen flex-col overflow-hidden">
      <div className="flex min-h-0 flex-1">
        {/* 左侧导航 */}
        <aside
          className="relative flex shrink-0 flex-col border-r border-slate-200 bg-white dark:border-slate-800 dark:bg-[#0b1220]"
          style={{ width: collapsed ? 64 : sidebarWidth }}
        >
          <div className={`flex h-14 items-center border-b border-slate-200 dark:border-slate-800 ${collapsed ? "justify-center" : "gap-2.5 px-4"}`}>
            <img src={logo} alt="趋势智研" className="h-8 w-8 rounded-lg" />
            {!collapsed && <div className="min-w-0">
              <div className="text-sm font-bold leading-tight">趋势智研</div>
              <div className="muted mt-0.5 truncate text-[10px]">A股智能研究终端</div>
            </div>}
          </div>
          {!collapsed && <div className="py-3"><StockSearch /></div>}
          <nav className={`min-h-0 flex-1 space-y-0.5 overflow-y-auto ${collapsed ? "px-2 pt-3" : "px-2"}`}>
            {NAV.map((n) => (
              <NavLink key={n.to} to={n.to} end={n.to === "/"} title={collapsed ? n.label : undefined} className={({ isActive }) => navCls(isActive)}>
                {({ isActive }) => (
                  <>
                    {isActive && <ActiveTick />}
                    <NavIcon d={n.icon} />
                    {!collapsed && n.label}
                  </>
                )}
              </NavLink>
            ))}
            {currentSymbol && (
              <NavLink to={`/stock/${currentSymbol}`} className={({ isActive }) => navCls(isActive)}>
                {({ isActive }) => (
                  <>
                    {isActive && <ActiveTick />}
                    <NavIcon d="M3 17l5-6 4 3 6-8M15 6h6v6" />
                    {!collapsed && "个股工作台"}
                  </>
                )}
              </NavLink>
            )}
          </nav>
          <div className={`border-t border-slate-200 py-2 dark:border-slate-800 ${collapsed ? "flex justify-center" : "px-3"}`}>
            <button className="btn w-full justify-center !border-0 !bg-transparent" title={collapsed ? "展开导航" : "收起导航"} onClick={() => setCollapsed((value) => !value)}>
              <span className="num">{collapsed ? "»" : "«"}</span>{!collapsed && " 收起导航"}
            </button>
          </div>
          {!collapsed && <button aria-label="调整导航宽度" className="absolute inset-y-0 right-0 z-20 w-1 cursor-col-resize hover:bg-blue-500/70" onMouseDown={() => setResizing(true)} />}
        </aside>
        <div className="flex min-w-0 flex-1 flex-col overflow-hidden">
          <header className="flex h-11 shrink-0 items-center gap-3 border-b border-slate-200 bg-white px-3 dark:border-slate-800 dark:bg-[#0d1524]">
            <div className="min-w-0 flex-1">
              <div className="truncate text-sm font-semibold">{pageTitle}</div>
            </div>
            {currentSymbol && <div className="hidden items-center gap-2 rounded border border-slate-200 px-2 py-1 text-xs dark:border-slate-700 lg:flex"><span className="muted">上下文</span><span className="font-medium">{currentName || currentSymbol}</span><span className="num muted">{currentSymbol}</span></div>}
            <div className="flex items-center gap-2 text-[11px]">
              <span className={`inline-block h-1.5 w-1.5 rounded-full ${marketOpen ? "bg-down" : "bg-slate-500"}`} />
              <span className="muted">{marketOpen ? "交易中" : "休市"}</span>
              <span className="num muted">{chinaTime}</span>
            </div>
            <button className="btn !px-2" title="切换明暗主题" onClick={() => setTheme(theme === "dark" ? "light" : "dark")}>{theme === "dark" ? "☀" : "☾"}</button>
          </header>
          {/* 主内容:窗口级不滚动,由各页面内部区域自行滚动 */}
          <main className="min-h-0 min-w-0 flex-1 overflow-hidden">
            <ErrorBoundary scope="当前页面" compact resetKey={location.pathname}>
              <Outlet />
            </ErrorBoundary>
          </main>
        </div>
      </div>
      {/* 终端状态条 */}
      <footer className="muted flex h-6 shrink-0 items-center justify-between border-t border-slate-200 bg-white px-3 text-[10px] dark:border-slate-800 dark:bg-[#0b1220]">
        <span>{mode === "pro" ? "专业模式" : "小白模式"} · 人工决策支持 · 不连接券商下单</span>
        <span>数据按字段标注来源与缺失 · 历史表现不代表未来收益</span>
      </footer>
      {/* 全局 AI 助手入口 */}
      <AgentLauncher />
    </div>
  );
}
