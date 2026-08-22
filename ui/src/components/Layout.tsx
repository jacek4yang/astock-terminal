import { useEffect, useRef, useState } from "react";
import { NavLink, Outlet, useLocation, useNavigate } from "react-router-dom";
import { searchStocks, type SearchResult } from "../lib/api";
import { useAppStore } from "../store";
import AgentChat from "./AgentChat";
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
  { to: "/agent", label: "AI 助手", icon: AGENT_ICON },
  { to: "/watchlist", label: "自选股", icon: "M12 2l3 7h7l-5.5 4.5L18 21l-6-4-6 4 1.5-7.5L2 9h7z" },
  { to: "/scan", label: "智能扫描", icon: "M11 4a7 7 0 1 0 4.9 12L21 21M11 8v3l2 2" },
  { to: "/datahub", label: "数据中心", icon: "M4 19V5m0 14h16M8 15v-4m4 4V8m4 7v-6" },
  { to: "/backtest", label: "回测实验室", icon: "M9 17v-6M13 17V7M17 17v-3M5 21h14" },
  { to: "/graph", label: "图谱分析", icon: "M12 5a2 2 0 1 0 0 .01M5 19a2 2 0 1 0 0 .01M19 19a2 2 0 1 0 0 .01M12 7l-5.6 10M12 7l5.6 10M7 19h10" },
  { to: "/settings", label: "设置", icon: "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8zM4 12h2m12 0h2M12 4v2m0 12v2" },
];

function navCls(isActive: boolean): string {
  return (
    "relative flex items-center gap-2 rounded px-3 py-2 text-sm transition-colors duration-150 " +
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

function StockSearch() {
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
    <div ref={boxRef} className="relative px-3">
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
  const location = useLocation();
  const onAgentPage = location.pathname.startsWith("/agent");

  if (onAgentPage) return null;

  return (
    <>
      {!open && (
        <button
          className="ease-app fixed bottom-12 right-5 z-40 flex h-11 w-11 items-center justify-center rounded-full bg-blue-600 text-white shadow-lg shadow-blue-600/30 transition-all duration-150 hover:-translate-y-0.5 hover:bg-blue-500 hover:shadow-xl"
          title="呼出 AI 助手"
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
          <div className="anim-drawer-in brand-bar-top absolute inset-y-0 right-0 w-[min(560px,96vw)] overflow-hidden bg-white shadow-2xl dark:bg-slate-950">
            <AgentChat variant="drawer" onClose={() => setOpen(false)} />
          </div>
        </div>
      )}
    </>
  );
}

export default function Layout() {
  const currentSymbol = useAppStore((s) => s.currentSymbol);
  const mode = useAppStore((s) => s.mode);

  return (
    <div className="flex h-screen w-screen flex-col overflow-hidden">
      <div className="flex min-h-0 flex-1">
        {/* 左侧导航 */}
        <aside className="flex w-44 shrink-0 flex-col border-r border-slate-200 bg-white dark:border-slate-800 dark:bg-slate-900">
          <div className="flex items-center gap-2.5 border-b border-slate-200 px-4 py-3 dark:border-slate-800">
            <img src={logo} alt="趋势智研" className="h-8 w-8 rounded-lg" />
            <div>
              <div className="text-sm font-bold leading-tight">趋势智研</div>
              <div className="muted mt-0.5 text-[10px]">实时买卖点终端</div>
            </div>
          </div>
          <div className="py-3">
            <StockSearch />
          </div>
          <nav className="min-h-0 flex-1 space-y-0.5 overflow-y-auto px-2">
            {NAV.map((n) => (
              <NavLink key={n.to} to={n.to} end={n.to === "/"} className={({ isActive }) => navCls(isActive)}>
                {({ isActive }) => (
                  <>
                    {isActive && <ActiveTick />}
                    <NavIcon d={n.icon} />
                    {n.label}
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
                    个股工作台
                  </>
                )}
              </NavLink>
            )}
          </nav>
          <div className="muted border-t border-slate-200 px-4 py-2 text-xs dark:border-slate-800">
            {mode === "pro" ? "专业模式" : "小白模式"}
          </div>
        </aside>
        {/* 主内容:窗口级不滚动,由各页面内部区域自行滚动 */}
        <main className="min-w-0 flex-1 overflow-hidden">
          <Outlet />
        </main>
      </div>
      {/* 免责条 */}
      <footer className="muted shrink-0 border-t border-slate-200 bg-white px-4 py-1.5 text-center text-xs dark:border-slate-800 dark:bg-slate-900">
        分析结果不构成投资建议,历史表现不代表未来收益
      </footer>
      {/* 全局 AI 助手入口 */}
      <AgentLauncher />
    </div>
  );
}
