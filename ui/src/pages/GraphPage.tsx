import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import * as echarts from "echarts";
import {
  agentAsk,
  errKind,
  errMsg,
  graphSubgraph,
  relationshipGraph,
  supplyChainShock,
  watchlistList,
  type GraphNode,
  type RelationshipEdge,
  type RelationshipGraph,
  type ShockEntry,
  type ShockJson,
  type SubgraphResult,
} from "../lib/api";
import { onAgentEvent } from "../lib/events";
import { ErrorBox, LoadBar, Term } from "../components/ui";
import Markdown from "../components/Markdown";
import { useAppStore } from "../store";

// ==================== 常量 ====================

const UP = "#e5484d";
const DOWN = "#2eb872";

const KIND_LABEL: Record<string, string> = {
  company: "公司",
  product: "产品",
  segment: "业务板块",
  material: "材料",
  commodity: "大宗商品",
  industry: "行业",
  region: "地区",
  policy: "政策",
};

const KIND_COLOR: Record<string, string> = {
  company: "#4f83ff",
  industry: "#a78bfa",
  product: "#22d3ee",
  material: "#fbbf24",
  commodity: "#fb923c",
  segment: "#2dd4bf",
  region: "#94a3b8",
  policy: "#f472b6",
};

const REL_LABEL: Record<string, string> = {
  supplies: "供应",
  customer_of: "客户",
  competes: "竞争",
  substitutes: "替代",
  exposed_to: "风险暴露",
  belongs_to: "归属",
  produces: "生产",
  consumes: "消耗",
};

const kindLabel = (k: string) => KIND_LABEL[k] ?? k;
const relLabel = (r: string) => REL_LABEL[r] ?? r;
const kindColor = (k: string) => KIND_COLOR[k] ?? "#94a3b8";

/** 流式文本去除 <think> 思考块 */
const cleanThink = (t: string) => t.replace(/<think>[\s\S]*?(<\/think>|$)/g, "").trim();

// ==================== 通用子组件 ====================

/** 图表加载骨架:呼吸圆点阵 + 滑动进度条 */
function ChartSkeleton({ text }: { text: string }) {
  return (
    <div className="anim-fade-in flex h-full flex-col items-center justify-center gap-5">
      <div className="grid grid-cols-3 gap-x-6 gap-y-4">
        {Array.from({ length: 6 }).map((_, i) => (
          <div
            key={i}
            className="h-9 w-16 animate-pulse rounded-full bg-slate-200 dark:bg-slate-800"
            style={{ animationDelay: `${i * 140}ms` }}
          />
        ))}
      </div>
      <LoadBar className="w-44" />
      <span className="muted text-xs">{text}</span>
    </div>
  );
}

interface GraphChartProps {
  option: echarts.EChartsOption;
  height?: number | string;
  /** 点击节点,回传节点 id */
  onNodeClick?: (id: string) => void;
  /** 点击边,回传边数据上的 __idx */
  onEdgeClick?: (idx: number) => void;
}

/** ECharts graph 封装:与 Chart.tsx 同主题/resize 约定,额外暴露节点/边点击 */
function GraphChart({ option, height = "100%", onNodeClick, onEdgeClick }: GraphChartProps) {
  const ref = useRef<HTMLDivElement>(null);
  const chartRef = useRef<echarts.ECharts | null>(null);
  const theme = useAppStore((s) => s.theme);
  const cbRef = useRef({ onNodeClick, onEdgeClick });
  cbRef.current = { onNodeClick, onEdgeClick };

  useEffect(() => {
    if (!ref.current) return;
    const chart = echarts.init(ref.current, theme === "dark" ? "dark" : undefined, {
      renderer: "canvas",
    });
    chartRef.current = chart;
    chart.on("click", (p: unknown) => {
      const params = p as { dataType?: string; data?: Record<string, unknown> };
      if (params.dataType === "node" && params.data) {
        const id = (params.data.id ?? params.data.name) as string | undefined;
        if (id) cbRef.current.onNodeClick?.(id);
      } else if (params.dataType === "edge" && params.data) {
        const idx = params.data.__idx;
        if (typeof idx === "number") cbRef.current.onEdgeClick?.(idx);
      }
    });
    const ro = new ResizeObserver(() => chart.resize());
    ro.observe(ref.current);
    const onWin = () => chart.resize();
    window.addEventListener("resize", onWin);
    return () => {
      ro.disconnect();
      window.removeEventListener("resize", onWin);
      chart.dispose();
      chartRef.current = null;
    };
  }, [theme]);

  useEffect(() => {
    chartRef.current?.setOption(option, true);
  }, [option]);

  return (
    <div
      ref={ref}
      style={{ width: "100%", height: typeof height === "number" ? `${height}px` : height }}
    />
  );
}

// ==================== Tab 1:产业链地图 ====================

function SupplyChainView() {
  const [query, setQuery] = useState("");
  const [hops, setHops] = useState(2);
  const [loading, setLoading] = useState(false);
  const [data, setData] = useState<SubgraphResult | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);

  // 冲击传导模拟
  const [shockSubject, setShockSubject] = useState("");
  const [shockDir, setShockDir] = useState<"up" | "down">("up");
  const [shockMag, setShockMag] = useState(10);
  const [shockLoading, setShockLoading] = useState(false);
  const [shock, setShock] = useState<ShockJson | null>(null);
  const [shockErr, setShockErr] = useState<string | null>(null);

  const navigate = useNavigate();

  const load = async () => {
    const q = query.trim();
    if (!q) return;
    setLoading(true);
    setErr(null);
    setSelected(null);
    setShock(null);
    setShockErr(null);
    try {
      const r = await graphSubgraph(q, hops);
      setData(r);
      setShockSubject(q);
    } catch (e) {
      setErr(errMsg(e));
      setData(null);
    } finally {
      setLoading(false);
    }
  };

  const runShock = async () => {
    const subject = shockSubject.trim();
    if (!subject) return;
    setShockLoading(true);
    setShockErr(null);
    try {
      setShock(await supplyChainShock(subject, shockDir, shockMag));
    } catch (e) {
      setShockErr(errMsg(e));
      setShock(null);
    } finally {
      setShockLoading(false);
    }
  };

  /** 受冲击节点 → 受益/受损/潜在(用于图上高亮) */
  const affected = useMemo(() => {
    const m = new Map<string, "benefit" | "harm" | "potential">();
    if (!shock) return m;
    const put = (list: ShockEntry[], v: "benefit" | "harm" | "potential", overwrite: boolean) =>
      list.forEach((e) => {
        if (overwrite || !m.has(e.node_id)) m.set(e.node_id, v);
      });
    put(shock.primary_benefit, "benefit", true);
    put(shock.primary_harm, "harm", true);
    put(shock.secondary_benefit, "benefit", false);
    put(shock.secondary_harm, "harm", false);
    put(shock.potential, "potential", false);
    return m;
  }, [shock]);

  const option = useMemo<echarts.EChartsOption>(() => {
    if (!data) return {};
    const kinds = [...new Set(data.nodes.map((n) => n.kind))];
    const degree = new Map<string, number>();
    data.edges.forEach((e) => {
      degree.set(e.src, (degree.get(e.src) ?? 0) + 1);
      degree.set(e.dst, (degree.get(e.dst) ?? 0) + 1);
    });
    const hasShock = affected.size > 0;
    const subjectId = shock?.subject.id;
    const nodes = data.nodes.map((n) => {
      const aff = affected.get(n.id);
      const dim = hasShock && !aff && n.id !== subjectId;
      const color =
        aff === "benefit" ? UP : aff === "harm" ? DOWN : kindColor(n.kind);
      return {
        id: n.id,
        name: n.name,
        category: kinds.indexOf(n.kind),
        symbolSize: Math.min(48, 18 + (degree.get(n.id) ?? 0) * 4),
        itemStyle: {
          color,
          opacity: dim ? 0.25 : 1,
          borderColor: aff ? color : "transparent",
          borderWidth: aff ? 3 : 0,
          shadowBlur: aff ? 14 : 0,
          shadowColor: aff ? color : "transparent",
        },
        label: { opacity: dim ? 0.35 : 1 },
      };
    });
    const links = data.edges.map((e, i) => {
      const hot = affected.has(e.src) || affected.has(e.dst);
      return {
        __idx: i,
        source: e.src,
        target: e.dst,
        lineStyle: {
          width: 1 + e.weight * 3,
          opacity: hasShock ? (hot ? 0.9 : 0.1) : 0.55,
          curveness: 0.1,
        },
        emphasis: {
          label: {
            show: true,
            formatter: relLabel(e.relation),
            color: "#cbd5e1",
            fontSize: 10,
          },
        },
      };
    });
    return {
      backgroundColor: "transparent",
      tooltip: {
        formatter: (p: unknown) => {
          const params = p as {
            dataType?: string;
            data?: Record<string, unknown>;
          };
          if (params.dataType === "edge") {
            const e = data.edges[(params.data?.__idx as number) ?? -1];
            if (!e) return "";
            const s = data.nodes.find((n) => n.id === e.src)?.name ?? e.src;
            const d = data.nodes.find((n) => n.id === e.dst)?.name ?? e.dst;
            return `${s} <b>${relLabel(e.relation)}</b> ${d}<br/>强度 ${e.weight.toFixed(2)} · 置信度 ${e.confidence.toFixed(2)}<br/><span style="opacity:.7">来源:${e.source_name}</span>`;
          }
          const id = params.data?.id as string | undefined;
          const n = data.nodes.find((x) => x.id === id);
          if (!n) return "";
          return `<b>${n.name}</b>${n.code ? ` (${n.code})` : ""}<br/>${kindLabel(n.kind)}`;
        },
      },
      legend: [
        {
          data: kinds.map((k) => kindLabel(k)),
          top: 4,
          textStyle: { fontSize: 11 },
        },
      ],
      series: [
        {
          type: "graph",
          layout: "force",
          roam: true,
          draggable: true,
          categories: kinds.map((k) => ({
            name: kindLabel(k),
            itemStyle: { color: kindColor(k) },
          })),
          data: nodes,
          links,
          edgeSymbol: ["none", "arrow"],
          edgeSymbolSize: 7,
          force: { repulsion: 260, edgeLength: [70, 150], gravity: 0.08 },
          label: { show: true, fontSize: 11, color: "#cbd5e1" },
          lineStyle: { color: "#64748b" },
          emphasis: { focus: "adjacency" },
        },
      ],
    } as echarts.EChartsOption;
  }, [data, affected, shock]);

  const selectedNode: GraphNode | null = useMemo(() => {
    if (!data || !selected) return null;
    return data.nodes.find((n) => n.id === selected) ?? null;
  }, [data, selected]);

  /** 选中节点的相关系列表 */
  const selectedRels = useMemo(() => {
    if (!data || !selected) return [];
    return data.edges
      .filter((e) => e.src === selected || e.dst === selected)
      .map((e) => {
        const out = e.src === selected;
        const otherId = out ? e.dst : e.src;
        const other = data.nodes.find((n) => n.id === otherId)?.name ?? otherId;
        return { out, relation: e.relation, other, weight: e.weight, confidence: e.confidence };
      });
  }, [data, selected]);

  const shockCount = affected.size;
  const maxHop = useMemo(() => {
    if (!shock) return 0;
    const all = [
      ...shock.primary_benefit,
      ...shock.primary_harm,
      ...shock.secondary_benefit,
      ...shock.secondary_harm,
      ...shock.potential,
    ];
    return all.reduce((m, e) => Math.max(m, e.hop), 0);
  }, [shock]);

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-3">
      {/* 查询 + 冲击控制条 */}
      <div className="card shrink-0 px-4 py-3">
        <div className="flex flex-wrap items-center gap-2">
          <input
            className="input w-44"
            placeholder="股票代码 / 名称 / 节点"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && load()}
          />
          <select
            className="input w-28"
            value={hops}
            onChange={(e) => setHops(Number(e.target.value))}
            title="从中心节点向外扩展的层级数"
          >
            <option value={1}>1 层关联</option>
            <option value={2}>2 层关联</option>
            <option value={3}>3 层关联</option>
          </select>
          <button className="btn-primary" onClick={load} disabled={loading || !query.trim()}>
            {loading ? "查询中…" : "查询产业链"}
          </button>
          {data && (
            <span className="muted text-xs">
              {data.nodes.length} 个节点 · {data.edges.length} 条关系
            </span>
          )}
        </div>
        <div className="mt-2.5 flex flex-wrap items-center gap-2 border-t border-slate-100 pt-2.5 dark:border-slate-800/60">
          <span className="muted text-xs">
            <Term label="冲击传导" tip="模拟某标的涨价/跌价事件沿供应链图谱的传播路径与影响范围" />
          </span>
          <input
            className="input w-36"
            placeholder="冲击主体(如 铜)"
            value={shockSubject}
            onChange={(e) => setShockSubject(e.target.value)}
          />
          <select
            className="input w-24"
            value={shockDir}
            onChange={(e) => setShockDir(e.target.value as "up" | "down")}
          >
            <option value="up">上涨</option>
            <option value="down">下跌</option>
          </select>
          <input
            type="range"
            min={1}
            max={50}
            step={1}
            value={shockMag}
            onChange={(e) => setShockMag(Number(e.target.value))}
            className="w-28 accent-blue-600"
            title="冲击幅度(%)"
          />
          <span className="num muted w-10 text-xs">{shockMag}%</span>
          <button
            className="btn"
            onClick={runShock}
            disabled={shockLoading || !shockSubject.trim()}
          >
            {shockLoading ? "模拟中…" : "模拟冲击"}
          </button>
          {shock && (
            <button className="muted text-xs underline decoration-dotted underline-offset-2" onClick={() => setShock(null)}>
              清除高亮
            </button>
          )}
        </div>
      </div>
      {err && <ErrorBox message={err} onRetry={load} />}
      {shockErr && <ErrorBox message={shockErr} onRetry={runShock} />}

      {/* 主区:图 + 右侧信息栏 */}
      <div className="flex min-h-0 flex-1 gap-3">
        <div className="card flex min-w-0 flex-1 flex-col overflow-hidden">
          {loading ? (
            <ChartSkeleton text="正在构建产业链图谱…" />
          ) : !data ? (
            <div className="muted flex h-full flex-col items-center justify-center gap-2 py-8 text-sm">
              <span>输入股票代码或名称,查询其产业链上下游图谱</span>
              <span className="text-xs">节点按类型着色(公司/行业/产品/材料/大宗),点击节点查看详情</span>
            </div>
          ) : (
            <GraphChart option={option} onNodeClick={setSelected} />
          )}
        </div>

        {/* 右栏:节点详情 + 冲击结果 */}
        {(selectedNode || shock) && (
          <div className="flex w-72 shrink-0 flex-col gap-3 overflow-y-auto">
            {selectedNode && (
              <div className="card anim-fade-up shrink-0 px-3.5 py-3">
                <div className="flex items-start justify-between gap-2">
                  <div className="min-w-0">
                    <div className="truncate text-sm font-semibold">{selectedNode.name}</div>
                    <div className="mt-1 flex flex-wrap items-center gap-1.5">
                      <span
                        className="tag"
                        style={{ backgroundColor: kindColor(selectedNode.kind) + "1a", color: kindColor(selectedNode.kind) }}
                      >
                        {kindLabel(selectedNode.kind)}
                      </span>
                      {selectedNode.code && <span className="num muted text-xs">{selectedNode.code}</span>}
                    </div>
                  </div>
                  <button className="muted shrink-0 text-xs" onClick={() => setSelected(null)}>
                    ✕
                  </button>
                </div>
                {selectedNode.code && (
                  <button
                    className="btn-primary mt-2.5 w-full"
                    onClick={() => navigate(`/stock/${selectedNode.code}`)}
                  >
                    打开个股工作台
                  </button>
                )}
                <div className="micro-label mt-3">相关系({selectedRels.length})</div>
                {selectedRels.length === 0 ? (
                  <div className="muted mt-1 text-xs">无关联关系</div>
                ) : (
                  <ul className="mt-1 space-y-1.5">
                    {selectedRels.map((r, i) => (
                      <li key={i} className="text-xs leading-relaxed">
                        {r.out ? (
                          <span>
                            本节点 <span className="font-medium text-blue-600 dark:text-blue-400">{relLabel(r.relation)}</span>{" "}
                            {r.other}
                          </span>
                        ) : (
                          <span>
                            {r.other} <span className="font-medium text-blue-600 dark:text-blue-400">{relLabel(r.relation)}</span>{" "}
                            本节点
                          </span>
                        )}
                        <span className="num muted ml-1">
                          强度 {r.weight.toFixed(2)} · 置信 {r.confidence.toFixed(2)}
                        </span>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            )}

            {shock && (
              <div className="card anim-fade-up shrink-0 px-3.5 py-3">
                <div className="text-sm font-semibold">{shock.event_title}</div>
                <div className="muted mt-1 flex flex-wrap gap-x-3 text-xs">
                  <span>影响 {shockCount} 个节点</span>
                  <span>
                    <Term label="传播深度" tip="冲击沿供应链传导的最大层级:一级为直接上下游,二级为间接关联" /> {maxHop} 级
                  </span>
                </div>
                <div className="muted mt-2 text-xs leading-relaxed">{shock.summary}</div>
                {(
                  [
                    ["一级受益", shock.primary_benefit, UP],
                    ["一级受损", shock.primary_harm, DOWN],
                    ["二级受益", shock.secondary_benefit, UP],
                    ["二级受损", shock.secondary_harm, DOWN],
                    ["潜在映射", shock.potential, "#94a3b8"],
                  ] as [string, ShockEntry[], string][]
                ).map(([title, list, color]) =>
                  list.length === 0 ? null : (
                    <div key={title} className="mt-3">
                      <div className="micro-label" style={{ color }}>
                        {title}({list.length})
                      </div>
                      <ul className="mt-1 space-y-2">
                        {list.map((e, i) => (
                          <li key={i} className="rounded border border-slate-100 px-2 py-1.5 text-xs dark:border-slate-800/60">
                            <div className="flex flex-wrap items-center gap-x-2">
                              <span className="font-medium">{e.name}</span>
                              {e.code && <span className="num muted">{e.code}</span>}
                              <span className="num muted ml-auto">
                                {e.expected_lag_days != null && `滞后约 ${e.expected_lag_days} 日`}
                                {e.magnitude_estimate_pct != null && ` · 幅度 ${e.magnitude_estimate_pct.toFixed(1)}%`}
                                {` · 置信 ${e.confidence.toFixed(2)}`}
                              </span>
                            </div>
                            <div className="muted mt-1 leading-relaxed">{e.logic_chain}</div>
                          </li>
                        ))}
                      </ul>
                    </div>
                  ),
                )}
                <div className="muted mt-3 rounded border border-amber-200 bg-amber-50 px-2 py-1.5 text-xs dark:border-amber-900/60 dark:bg-amber-950/30">
                  {shock.disclaimer}
                </div>
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

// ==================== Tab 2:关系网络 ====================

interface AiState {
  status: "idle" | "running" | "done" | "failed";
  text: string;
  err?: string;
}

function RelationshipView() {
  const [symbols, setSymbols] = useState<string[]>([]);
  const [names, setNames] = useState<Record<string, string>>({});
  const [input, setInput] = useState("");
  const [windowDays, setWindowDays] = useState(250);
  const [threshold, setThreshold] = useState(0.3);
  const [loading, setLoading] = useState(false);
  const [data, setData] = useState<RelationshipGraph | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [selEdge, setSelEdge] = useState<RelationshipEdge | null>(null);
  const [ai, setAi] = useState<AiState>({ status: "idle", text: "" });
  const unlistenRef = useRef<(() => void) | undefined>(undefined);

  useEffect(() => () => unlistenRef.current?.(), []);

  const nameOf = useCallback((code: string) => names[code] ?? code, [names]);

  const addSymbol = () => {
    const code = input.trim();
    if (!code) return;
    if (symbols.includes(code)) {
      setInput("");
      return;
    }
    if (symbols.length >= 12) {
      setErr("最多 12 只股票");
      return;
    }
    setSymbols([...symbols, code]);
    setInput("");
    setErr(null);
  };

  const importWatchlist = async () => {
    setErr(null);
    try {
      const list = await watchlistList();
      const codes = [...new Set(list.map((i) => i.code))].slice(0, 12);
      if (codes.length === 0) {
        setErr("自选股为空,请先添加自选股或手动输入代码");
        return;
      }
      setSymbols(codes);
      setNames((prev) => {
        const next = { ...prev };
        list.forEach((i) => {
          if (i.name) next[i.code] = i.name;
        });
        return next;
      });
    } catch (e) {
      setErr(errMsg(e));
    }
  };

  const build = async () => {
    if (symbols.length < 2) return;
    setLoading(true);
    setErr(null);
    setSelEdge(null);
    setAi({ status: "idle", text: "" });
    unlistenRef.current?.();
    try {
      setData(await relationshipGraph(symbols, windowDays));
    } catch (e) {
      setErr(errMsg(e));
      setData(null);
    } finally {
      setLoading(false);
    }
  };

  const filtered = useMemo(
    () => (data ? data.edges.filter((e) => Math.abs(e.pearson) >= threshold) : []),
    [data, threshold],
  );

  const option = useMemo<echarts.EChartsOption>(() => {
    if (!data) return {};
    // 中心性 = 过滤后各节点 |相关系数| 之和
    const cent = new Map<string, number>();
    data.nodes.forEach((n) => cent.set(n.symbol, 0));
    filtered.forEach((e) => {
      cent.set(e.pair[0], (cent.get(e.pair[0]) ?? 0) + Math.abs(e.pearson));
      cent.set(e.pair[1], (cent.get(e.pair[1]) ?? 0) + Math.abs(e.pearson));
    });
    const maxCent = Math.max(0.0001, ...cent.values());
    const nodes = data.nodes.map((n) => ({
      id: n.symbol,
      name: names[n.symbol] ? `${names[n.symbol]}\n${n.symbol}` : n.symbol,
      symbolSize: 20 + 28 * ((cent.get(n.symbol) ?? 0) / maxCent),
      itemStyle: { color: "#4f83ff" },
      label: { fontSize: 10 },
    }));
    const links = filtered.map((e, i) => ({
      __idx: i,
      source: e.pair[0],
      target: e.pair[1],
      lineStyle: {
        width: 1 + 6 * Math.abs(e.pearson),
        color: e.pearson >= 0 ? UP : DOWN,
        opacity: 0.3 + 0.65 * Math.abs(e.pearson),
        curveness: 0.08,
      },
    }));
    const edgeTip = (e: RelationshipEdge) => {
      const lagText =
        e.best_lag === 0
          ? "无明显领先/滞后"
          : `${nameOf(e.leader ?? "")} 领先约 ${Math.abs(e.best_lag)} 日`;
      return (
        `<b>${nameOf(e.pair[0])} ↔ ${nameOf(e.pair[1])}</b><br/>` +
        `Pearson r = <b style="color:${e.pearson >= 0 ? UP : DOWN}">${e.pearson.toFixed(3)}</b>(${e.pearson >= 0 ? "正相关" : "负相关"})<br/>` +
        `${lagText} · 滞后相关 ${e.lag_corr.toFixed(3)}<br/>` +
        `p 值 ${e.p_value != null ? e.p_value.toFixed(3) : "未知"} · ${e.significant ? "显著" : "不显著"}`
      );
    };
    return {
      backgroundColor: "transparent",
      tooltip: {
        formatter: (p: unknown) => {
          const params = p as { dataType?: string; data?: Record<string, unknown> };
          if (params.dataType === "edge") {
            const e = filtered[(params.data?.__idx as number) ?? -1];
            return e ? edgeTip(e) : "";
          }
          const sym = params.data?.id as string | undefined;
          if (!sym) return "";
          const c = cent.get(sym) ?? 0;
          return `<b>${nameOf(sym)}</b>(${sym})<br/>中心性 ${c.toFixed(2)}(过滤后 |r| 之和)`;
        },
      },
      series: [
        {
          type: "graph",
          layout: "force",
          roam: true,
          draggable: true,
          data: nodes,
          links,
          force: { repulsion: 320, edgeLength: [80, 180], gravity: 0.06 },
          label: { show: true, color: "#cbd5e1" },
          emphasis: { focus: "adjacency" },
        },
      ],
    } as echarts.EChartsOption;
  }, [data, filtered, names, nameOf]);

  /** Agent 解释:把图摘要作为上下文提问 */
  const askAgent = async () => {
    if (!data || !selEdge) return;
    unlistenRef.current?.();
    setAi({ status: "running", text: "" });
    const [a, b] = selEdge.pair;
    const top = [...data.edges]
      .sort((x, y) => Math.abs(y.pearson) - Math.abs(x.pearson))
      .slice(0, 10)
      .map((e) => `${e.pair[0]}-${e.pair[1]} r=${e.pearson}`)
      .join("; ");
    const q =
      `【上下文】以下为 ${data.window_days} 个交易日窗口(实际对齐 ${data.aligned_bars} 天)` +
      `内一组股票日收益率 Pearson 相关网络,相关性最强的边:${top}。` +
      `其中 ${nameOf(a)}(${a}) 与 ${nameOf(b)}(${b}) 的相关系数 r=${selEdge.pearson},` +
      `最优滞后 ${selEdge.best_lag} 日(滞后相关 ${selEdge.lag_corr},` +
      `p=${selEdge.p_value ?? "未知"},${selEdge.significant ? "显著" : "不显著"})。\n` +
      `请解释这两只股票为什么相关,并提示相关性不等于因果的风险。`;
    try {
      const r = await agentAsk(q, null);
      unlistenRef.current = await onAgentEvent(r.task_id, (ev) => {
        if (ev.type === "text_delta") {
          setAi((s) => ({ ...s, text: s.text + ev.text }));
        } else if (ev.type === "completed") {
          setAi({ status: "done", text: ev.report.answer });
        } else if (ev.type === "failed") {
          setAi({ status: "failed", text: "", err: ev.error });
        } else if (ev.type === "suspended") {
          setAi({ status: "failed", text: "", err: "配额已用尽,任务挂起;请到 AI 助手页继续。" });
        }
      });
    } catch (e) {
      if (errKind(e) === "no_key") {
        setAi({ status: "failed", text: "", err: "尚未配置 MiniMax API Key,请先到「设置」页填写后再提问。" });
      } else {
        setAi({ status: "failed", text: "", err: errMsg(e) });
      }
    }
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-3">
      {/* 控制条 */}
      <div className="card shrink-0 px-4 py-3">
        <div className="flex flex-wrap items-center gap-2">
          <input
            className="input w-36"
            placeholder="输入股票代码"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && addSymbol()}
          />
          <button className="btn" onClick={addSymbol} disabled={!input.trim()}>
            添加
          </button>
          <button className="btn" onClick={importWatchlist}>
            从自选股导入
          </button>
          <select
            className="input w-32"
            value={windowDays}
            onChange={(e) => setWindowDays(Number(e.target.value))}
            title="回看交易日数"
          >
            <option value={60}>近 60 日</option>
            <option value={120}>近 120 日</option>
            <option value={250}>近 250 日</option>
            <option value={500}>近 500 日</option>
          </select>
          <button className="btn-primary" onClick={build} disabled={loading || symbols.length < 2}>
            {loading ? "计算中…" : "构建网络"}
          </button>
          {data && (
            <span className="muted text-xs">
              对齐 {data.aligned_bars} 天 · {data.period.start ?? "?"} ~ {data.period.end ?? "?"}
            </span>
          )}
        </div>
        {symbols.length > 0 && (
          <div className="mt-2.5 flex flex-wrap gap-1.5">
            {symbols.map((s) => (
              <span
                key={s}
                className="tag flex items-center gap-1 bg-slate-200 text-slate-600 dark:bg-slate-800 dark:text-slate-300"
              >
                {nameOf(s)}
                <button
                  className="muted hover:text-red-500"
                  title="移除"
                  onClick={() => setSymbols(symbols.filter((x) => x !== s))}
                >
                  ✕
                </button>
              </span>
            ))}
          </div>
        )}
        {data && (
          <div className="mt-2.5 flex flex-wrap items-center gap-3 border-t border-slate-100 pt-2.5 dark:border-slate-800/60">
            <span className="muted text-xs">
              <Term label="相关阈值" tip="仅显示 |相关系数| ≥ 阈值的边,过滤掉弱相关噪声" />
            </span>
            <input
              type="range"
              min={0}
              max={0.9}
              step={0.05}
              value={threshold}
              onChange={(e) => setThreshold(Number(e.target.value))}
              className="w-36 accent-blue-600"
            />
            <span className="num muted text-xs">|r| ≥ {threshold.toFixed(2)}</span>
            <span className="muted text-xs">
              显示 {filtered.length}/{data.edges.length} 条边 ·{" "}
              <span style={{ color: UP }}>红=正相关</span> / <span style={{ color: DOWN }}>绿=负相关</span>{" "}
              · 边粗=强度 · 节点大=中心性
            </span>
          </div>
        )}
      </div>
      {err && <ErrorBox message={err} onRetry={symbols.length >= 2 ? build : undefined} />}
      {data && data.errors.length > 0 && (
        <div className="shrink-0 rounded border border-amber-200 bg-amber-50 px-3 py-1.5 text-xs text-amber-800 dark:border-amber-900/60 dark:bg-amber-950/40 dark:text-amber-300">
          部分数据处理失败:{data.errors.join(";")}
        </div>
      )}

      {/* 主区:图 + 右侧信息栏 */}
      <div className="flex min-h-0 flex-1 gap-3">
        <div className="card flex min-w-0 flex-1 flex-col overflow-hidden">
          {loading ? (
            <ChartSkeleton text="正在拉取日K并计算相关性…" />
          ) : !data ? (
            <div className="muted flex h-full flex-col items-center justify-center gap-2 py-8 text-sm">
              <span>添加 2-12 只股票(或从自选股一键导入)后构建相关性网络</span>
              <span className="text-xs">基于日收益率 Pearson 相关 + lead-lag 检验,点击边查看详情</span>
            </div>
          ) : filtered.length === 0 ? (
            <div className="muted flex h-full flex-col items-center justify-center gap-2 py-8 text-sm">
              <span>当前阈值下没有满足条件的相关边</span>
              <span className="text-xs">试着降低 |r| 阈值,或扩大回看窗口</span>
            </div>
          ) : (
            <GraphChart option={option} onEdgeClick={(i) => { setSelEdge(filtered[i] ?? null); setAi({ status: "idle", text: "" }); unlistenRef.current?.(); }} />
          )}
        </div>

        {/* 右栏:边详情 + Agent 解释 */}
        {selEdge && data && (
          <div className="flex w-80 shrink-0 flex-col gap-3 overflow-y-auto">
            <div className="card anim-fade-up shrink-0 px-3.5 py-3">
              <div className="flex items-start justify-between gap-2">
                <div className="text-sm font-semibold">
                  {nameOf(selEdge.pair[0])} ↔ {nameOf(selEdge.pair[1])}
                </div>
                <button className="muted shrink-0 text-xs" onClick={() => { setSelEdge(null); setAi({ status: "idle", text: "" }); unlistenRef.current?.(); }}>
                  ✕
                </button>
              </div>
              <div className="mt-2 space-y-1.5 text-xs">
                <div className="flex justify-between">
                  <span className="muted">相关系数</span>
                  <span className="num font-semibold" style={{ color: selEdge.pearson >= 0 ? UP : DOWN }}>
                    {selEdge.pearson.toFixed(3)}({selEdge.pearson >= 0 ? "正相关" : "负相关"})
                  </span>
                </div>
                <div className="flex justify-between">
                  <span className="muted">
                    <Term label="最优滞后" tip="交叉相关扫描(±5 日)中相关最强的滞后天数,非零代表一只票领先另一只" />
                  </span>
                  <span className="num">
                    {selEdge.best_lag === 0
                      ? "无领先/滞后"
                      : `${nameOf(selEdge.leader ?? "")} 领先 ${Math.abs(selEdge.best_lag)} 日`}
                  </span>
                </div>
                <div className="flex justify-between">
                  <span className="muted">滞后相关</span>
                  <span className="num">{selEdge.lag_corr.toFixed(3)}</span>
                </div>
                <div className="flex justify-between">
                  <span className="muted">p 值(bootstrap)</span>
                  <span className="num">
                    {selEdge.p_value != null ? selEdge.p_value.toFixed(3) : "未知"}
                    <span className={"ml-1.5 tag " + (selEdge.significant ? "bg-down/10 text-down" : "bg-slate-200 text-slate-600 dark:bg-slate-800 dark:text-slate-300")}>
                      {selEdge.significant ? "显著" : "不显著"}
                    </span>
                  </span>
                </div>
              </div>
              <button
                className="btn-primary mt-3 w-full"
                onClick={askAgent}
                disabled={ai.status === "running"}
              >
                {ai.status === "running" ? "Agent 分析中…" : "Agent 解释:这两只股票为什么相关"}
              </button>
              <div className="muted mt-2 text-xs leading-relaxed">{data.note}</div>
            </div>

            {ai.status !== "idle" && (
              <div className="card anim-fade-up shrink-0 px-3.5 py-3">
                <div className="micro-label mb-2">Agent 解读</div>
                {ai.status === "running" && <LoadBar className="mb-2 w-full" />}
                {ai.err ? (
                  <div className="rounded border border-red-300 bg-red-50 px-2.5 py-1.5 text-xs text-red-700 dark:border-red-900 dark:bg-red-950/40 dark:text-red-300">
                    {ai.err}
                  </div>
                ) : ai.status === "done" ? (
                  <Markdown src={ai.text} />
                ) : (
                  <div className="muted whitespace-pre-wrap text-xs leading-relaxed">
                    {cleanThink(ai.text) || "正在思考…"}
                  </div>
                )}
                {ai.status === "done" && (
                  <div className="muted mt-2 rounded border border-amber-200 bg-amber-50 px-2 py-1.5 text-xs dark:border-amber-900/60 dark:bg-amber-950/30">
                    免责声明:以上内容由 AI 生成,仅供参考,不构成投资建议。
                  </div>
                )}
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

// ==================== 页面 ====================

const TABS = [
  { key: "chain", label: "产业链地图" },
  { key: "rel", label: "关系网络" },
] as const;

export default function GraphPage() {
  const [tab, setTab] = useState<(typeof TABS)[number]["key"]>("chain");

  return (
    <div className="flex h-full flex-col gap-3 overflow-hidden p-3">
      <div className="flex shrink-0 items-center gap-3">
        <h1 className="text-sm font-semibold text-slate-700 dark:text-slate-300">图谱分析</h1>
        <div className="flex gap-0.5 rounded-lg bg-slate-100 p-0.5 dark:bg-slate-800">
          {TABS.map((t) => (
            <button
              key={t.key}
              className={
                "rounded-md px-3 py-1 text-xs transition-colors " +
                (tab === t.key
                  ? "bg-white font-medium text-blue-600 shadow-sm dark:bg-slate-900 dark:text-blue-400"
                  : "text-slate-500 hover:text-slate-700 dark:text-slate-400 dark:hover:text-slate-200")
              }
              onClick={() => setTab(t.key)}
            >
              {t.label}
            </button>
          ))}
        </div>
      </div>
      {/* 两个视图常驻挂载(切 Tab 不丢状态),用 hidden 切换 */}
      <div className={"flex min-h-0 flex-1 flex-col " + (tab === "chain" ? "" : "hidden")}>
        <SupplyChainView />
      </div>
      <div className={"flex min-h-0 flex-1 flex-col " + (tab === "rel" ? "" : "hidden")}>
        <RelationshipView />
      </div>
    </div>
  );
}
