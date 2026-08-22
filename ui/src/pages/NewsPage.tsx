import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import {
  errMsg,
  getNewsArchiveRevisions,
  getNewsEventClusterDetail,
  getNewsProviderHealth,
  queryNewsCenter,
  refreshNewsCenter,
  setNewsItemState,
  type ArchivedNewsRevision,
  type NewsCenterItem,
  type NewsCenterPage,
  type NewsCenterQuery,
  type NewsEventClusterDetail,
  type NewsProviderHealthItem,
} from "../lib/api";
import { useAgentSession } from "../agentSession";
import { ErrorBox, Loading } from "../components/ui";

const CATEGORY = [
  ["all", "实时流"],
  ["important", "重要快讯"],
  ["disclosure", "正式公告"],
  ["company", "公司事件"],
  ["macro", "宏观政策"],
  ["global", "海外传导"],
] as const;

const EVENT_LABEL: Record<string, string> = {
  earnings: "业绩/财报",
  policy: "政策监管",
  announcement: "公告披露",
  order: "订单/中标",
  capital: "资本动作",
  risk: "风险事件",
  global: "海外事件",
  market: "市场异动",
  other: "其他",
};

const VERIFICATION_CLASS: Record<string, string> = {
  primary: "border-emerald-300 bg-emerald-50 text-emerald-700 dark:border-emerald-900 dark:bg-emerald-950/40 dark:text-emerald-300",
  verified_media: "border-blue-300 bg-blue-50 text-blue-700 dark:border-blue-900 dark:bg-blue-950/40 dark:text-blue-300",
  archived: "border-slate-300 bg-slate-50 text-slate-600 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-300",
  discovery_only: "border-amber-300 bg-amber-50 text-amber-700 dark:border-amber-900 dark:bg-amber-950/40 dark:text-amber-300",
};

const SESSION_ROLE: Record<string, string> = {
  same_day_premarket: "当日盘前",
  intraday: "当日盘中",
  next_trading_day: "下一交易日",
  historical_only: "仅作历史背景",
};

const MARKET_PHASE: Record<string, string> = {
  premarket: "盘前",
  opening_auction: "开盘集合竞价",
  morning_trading: "上午连续交易",
  lunch_break: "午间休市",
  afternoon_trading: "下午连续交易",
  closing_auction: "收盘集合竞价",
  after_close: "收盘后",
  non_trading_day: "休市日",
};

const EMPTY_QUERY: NewsCenterQuery = {
  keyword: "",
  category: "all",
  source_id: "",
  importance: "",
  entity_keywords: [],
  event_type: "",
  language: "",
  verification: "",
  user_state: "",
  from_utc: null,
  to_utc: null,
  page: 1,
  page_size: 200,
};

const ROW_HEIGHT = 166;

export interface VirtualRange {
  start: number;
  end: number;
  offset: number;
  totalHeight: number;
}

/** Constant-height windowing; remains O(1) even for 100k filtered rows. */
export function virtualRange(
  scrollTop: number,
  viewportHeight: number,
  count: number,
  rowHeight = ROW_HEIGHT,
  overscan = 4,
): VirtualRange {
  if (count <= 0 || rowHeight <= 0) return { start: 0, end: 0, offset: 0, totalHeight: 0 };
  const visibleStart = Math.floor(Math.max(0, scrollTop) / rowHeight);
  const start = Math.max(0, visibleStart - overscan);
  const visibleCount = Math.ceil(Math.max(0, viewportHeight) / rowHeight);
  const end = Math.min(count, visibleStart + visibleCount + overscan);
  return { start, end, offset: start * rowHeight, totalHeight: count * rowHeight };
}

function displayTime(revision: ArchivedNewsRevision): string {
  const seconds = revision.publish_time.utc ?? revision.event_time.utc ?? revision.first_seen_time_utc;
  return new Date(seconds * 1000).toLocaleString("zh-CN", { hour12: false });
}

function ageText(seconds: number | null): string {
  if (seconds == null) return "更新时间未知";
  if (seconds < 60) return `${seconds} 秒前`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分钟前`;
  if (seconds < 86_400) return `${Math.floor(seconds / 3600)} 小时前`;
  return `${Math.floor(seconds / 86_400)} 天前`;
}

export function pageTokens(page: number, total: number): Array<number | "ellipsis"> {
  const keep = new Set([1, total, page - 1, page, page + 1]);
  const pages = [...keep].filter((item) => item >= 1 && item <= total).sort((a, b) => a - b);
  const out: Array<number | "ellipsis"> = [];
  pages.forEach((item, index) => {
    if (index > 0 && item - pages[index - 1] > 1) out.push("ellipsis");
    out.push(item);
  });
  return out;
}

function keyboardMove(index: number, direction: -1 | 1) {
  document.querySelector<HTMLElement>(`[data-news-index="${index + direction}"]`)?.focus();
}

function NewsRow({
  item,
  index,
  selected,
  onOpen,
  onState,
}: {
  item: NewsCenterItem;
  index: number;
  selected: boolean;
  onOpen: (item: NewsCenterItem) => void;
  onState: (item: NewsCenterItem, action: "read" | "pinned" | "favorite" | "ignored", value: boolean) => void;
}) {
  const revision = item.revision;
  const verifiedEntities = item.entity_links.filter((link) => link.eligible_for_agent);
  return (
    <article
      data-news-index={index}
      tabIndex={0}
      aria-label={`${revision.title}，${revision.source_name}，${displayTime(revision)}`}
      className={`absolute left-0 right-0 mx-2 h-[158px] cursor-pointer overflow-hidden rounded border px-3 py-2 outline-none transition-colors focus:ring-2 focus:ring-blue-500 ${
        selected
          ? "border-blue-400 bg-blue-50/70 dark:border-blue-700 dark:bg-blue-950/25"
          : item.user_state.is_read
            ? "border-slate-200 bg-slate-50/60 dark:border-slate-800 dark:bg-slate-950/30"
            : "border-slate-200 bg-white dark:border-slate-800 dark:bg-slate-900"
      }`}
      style={{ top: index * ROW_HEIGHT }}
      onClick={() => onOpen(item)}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") onOpen(item);
        if (event.key === "ArrowDown") keyboardMove(index, 1);
        if (event.key === "ArrowUp") keyboardMove(index, -1);
      }}
    >
      <div className="flex items-start gap-2">
        <span className={`mt-1 h-2 w-2 shrink-0 rounded-full ${item.user_state.is_read ? "bg-slate-400" : "bg-blue-500"}`} />
        <h2 className="line-clamp-2 min-w-0 flex-1 text-[13px] font-semibold leading-5">{revision.title}</h2>
        <div className="flex shrink-0 gap-1" onClick={(event) => event.stopPropagation()}>
          <button type="button" className={`chip ${item.user_state.pinned ? "bg-blue-600 text-white" : ""}`} aria-label={item.user_state.pinned ? "取消置顶" : "置顶"} onClick={() => onState(item, "pinned", !item.user_state.pinned)}>置顶</button>
          <button type="button" className={`chip ${item.user_state.favorite ? "bg-amber-500 text-white" : ""}`} aria-label={item.user_state.favorite ? "取消收藏" : "收藏"} onClick={() => onState(item, "favorite", !item.user_state.favorite)}>收藏</button>
        </div>
      </div>
      <p className="muted mt-1 line-clamp-2 text-xs leading-5">{revision.factual_summary || "该来源没有提供可归档摘要，请打开原文核验。"}</p>
      <div className="mt-2 flex flex-wrap items-center gap-1.5 text-[10px]">
        {item.important && <span className="rounded bg-red-500/10 px-1.5 py-0.5 font-medium text-red-600 dark:text-red-300" title={item.importance_reason}>重要</span>}
        <span className={`rounded border px-1.5 py-0.5 ${VERIFICATION_CLASS[item.verification] ?? VERIFICATION_CLASS.archived}`}>{item.verification_name}</span>
        <span className="rounded bg-slate-100 px-1.5 py-0.5 dark:bg-slate-800">{EVENT_LABEL[item.event_type] ?? item.event_type}</span>
        {revision.supersedes_revision_id && <span className="rounded bg-violet-500/10 px-1.5 py-0.5 text-violet-600 dark:text-violet-300">修订版</span>}
        {item.event?.old_republication && <span className="rounded bg-amber-500/10 px-1.5 py-0.5 text-amber-600">旧闻重发</span>}
        <span className={`rounded px-1.5 py-0.5 ${item.effective_session.can_increase_confidence ? "bg-emerald-500/10 text-emerald-700 dark:text-emerald-300" : "bg-amber-500/10 text-amber-700 dark:text-amber-300"}`} title={item.effective_session.rationale}>
          影响 {item.effective_session.target_trading_date} · {SESSION_ROLE[item.effective_session.role]}
        </span>
        {item.event && <span>独立来源 {item.event.independent_sources}</span>}
        {item.event?.conflict_fields.length ? <span className="text-red-600 dark:text-red-300">{item.event.conflict_fields.length} 项冲突</span> : null}
        {verifiedEntities.slice(0, 3).map((link) => <span key={link.link_id} className="rounded bg-blue-500/10 px-1.5 py-0.5 text-blue-600 dark:text-blue-300">{link.final_entity_name}{link.listed_code ? ` ${link.listed_code}` : ""}</span>)}
      </div>
      <div className="muted mt-2 flex flex-wrap gap-x-3 text-[10px]">
        <span>{revision.source_name}</span>
        <span>{displayTime(revision)}</span>
        <span>首次发现 {new Date(revision.first_seen_time_utc * 1000).toLocaleString("zh-CN", { hour12: false })}</span>
        <span className="num truncate">{revision.revision_id}</span>
      </div>
    </article>
  );
}

function DetailPanel({
  item,
  cluster,
  revisions,
  loading,
  onClose,
  onStock,
  onGraph,
  onAgent,
  onIgnore,
}: {
  item: NewsCenterItem;
  cluster: NewsEventClusterDetail | null;
  revisions: ArchivedNewsRevision[];
  loading: boolean;
  onClose: () => void;
  onStock: (code: string) => void;
  onGraph: () => void;
  onAgent: (priceIn: boolean) => void;
  onIgnore: () => void;
}) {
  const revision = item.revision;
  return (
    <aside className="flex min-h-0 w-[460px] shrink-0 flex-col border-l border-slate-200 bg-white dark:border-slate-800 dark:bg-[#0d1524]">
      <div className="flex items-center justify-between border-b border-slate-200 px-3 py-2 dark:border-slate-800">
        <b className="text-sm">事件与证据详情</b>
        <button type="button" className="btn !px-2" onClick={onClose} aria-label="关闭详情">×</button>
      </div>
      <div className="min-h-0 flex-1 space-y-3 overflow-auto p-3 text-xs">
        <div>
          <div className="flex flex-wrap gap-1.5">
            <span className={`rounded border px-1.5 py-0.5 ${VERIFICATION_CLASS[item.verification] ?? VERIFICATION_CLASS.archived}`}>{item.verification_name}</span>
            <span className="rounded bg-slate-100 px-1.5 py-0.5 dark:bg-slate-800">{EVENT_LABEL[item.event_type]}</span>
            {item.important && <span className="rounded bg-red-500/10 px-1.5 py-0.5 text-red-600">重要 · {item.importance_reason}</span>}
          </div>
          <h2 className="mt-2 text-base font-semibold leading-6">{revision.title}</h2>
          <p className="muted mt-2 whitespace-pre-wrap leading-5">{revision.factual_summary || "暂无归档摘要"}</p>
        </div>
        <div className="grid grid-cols-2 gap-2 rounded bg-slate-50 p-2 dark:bg-slate-900/70">
          <div><span className="muted">发布时间</span><div>{displayTime(revision)}</div></div>
          <div><span className="muted">事件时间</span><div>{revision.event_time.utc ? new Date(revision.event_time.utc * 1000).toLocaleString("zh-CN", { hour12: false }) : "来源未提供"}</div></div>
          <div><span className="muted">首次发现</span><div>{new Date(revision.first_seen_time_utc * 1000).toLocaleString("zh-CN", { hour12: false })}</div></div>
          <div><span className="muted">最近修订</span><div>{revision.revision_time.utc ? new Date(revision.revision_time.utc * 1000).toLocaleString("zh-CN", { hour12: false }) : "未知"}</div></div>
          <div><span className="muted">来源等级</span><div>{revision.source_name}</div></div>
          <div><span className="muted">语言</span><div>{revision.language}</div></div>
        </div>
        <section className={`rounded border p-2 ${item.effective_session.can_increase_confidence ? "border-emerald-200 bg-emerald-50 dark:border-emerald-900 dark:bg-emerald-950/30" : "border-amber-200 bg-amber-50 dark:border-amber-900 dark:bg-amber-950/30"}`}>
          <h3 className="font-semibold">交易会话归属</h3>
          <div className="mt-1">影响交易日：<b>{item.effective_session.target_trading_date}</b> · {SESSION_ROLE[item.effective_session.role]} · {MARKET_PHASE[item.effective_session.phase]}</div>
          <div className="muted mt-1">最早可用：{item.effective_session.effective_at_china}{item.effective_session.time_uncertain ? " · 发布时间不精确，已保守处理" : ""}</div>
          <div className="mt-1 leading-5">{item.effective_session.rationale}</div>
          {!item.effective_session.can_increase_confidence && <div className="mt-1 font-medium text-amber-700 dark:text-amber-300">仅作核验线索/历史背景，不得据此提高仓位或结论置信度。</div>}
        </section>
        <div className="flex flex-wrap gap-2">
          <button type="button" className="btn-primary" onClick={() => onAgent(false)}>交给智能助手深度分析</button>
          <button type="button" className="btn" onClick={() => onAgent(true)}>分析是否已被市场交易</button>
          <button type="button" className="btn" onClick={onGraph}>查看产业链传导</button>
          <button type="button" className="btn-danger" onClick={onIgnore}>忽略此资讯</button>
          {revision.canonical_url && <a className="btn" href={revision.canonical_url} target="_blank" rel="noreferrer">打开原文</a>}
        </div>
        <section>
          <h3 className="font-semibold">关联公司与实体</h3>
          {item.entity_links.length ? <div className="mt-1.5 space-y-1.5">{item.entity_links.map((link) => (
            <div key={link.link_id} className="rounded border border-slate-200 p-2 dark:border-slate-800">
              <div className="flex flex-wrap items-center gap-2">
                <span>{link.final_entity_name ?? link.span_text}</span>
                {link.listed_code && <button type="button" className="num text-blue-600 underline" onClick={() => onStock(link.listed_code!)}>{link.listed_code}</button>}
                <span className="muted">置信度 {(link.confidence * 100).toFixed(0)}%</span>
                <span className={link.eligible_for_agent ? "text-emerald-600" : "text-amber-600"}>{link.eligible_for_agent ? "已核验" : "等待人工复核"}</span>
              </div>
              <div className="muted mt-1">{link.reasons.join("；")}</div>
            </div>
          ))}</div> : <div className="muted mt-1">尚无达到阈值的实体映射，不会仅凭同名猜测上市公司。</div>}
        </section>
        <section>
          <h3 className="font-semibold">事件证据与反方信息</h3>
          {loading ? <div className="muted mt-1">正在读取事件簇与修订链…</div> : cluster ? (
            <div className="mt-1.5 space-y-2">
              <div className="rounded border border-slate-200 p-2 dark:border-slate-800">
                <div>事件状态：{cluster.cluster.status} · 独立来源 {cluster.cluster.independent_sources}</div>
                <div className="muted mt-1">证据多样性 {(cluster.cluster.evidence_diversity * 100).toFixed(0)}% · 成员 {cluster.members.length}</div>
              </div>
              {cluster.conflicts.length > 0 ? cluster.conflicts.map((conflict) => (
                <div key={conflict.field_name} className="rounded border border-red-200 bg-red-50 p-2 text-red-700 dark:border-red-900 dark:bg-red-950/30 dark:text-red-300">
                  反方/冲突字段 {conflict.field_name}：{conflict.values.join(" ↔ ")}
                </div>
              )) : <div className="muted">当前事件簇没有已登记的字段冲突；这不代表不存在未知风险。</div>}
              {cluster.members.map((member) => (
                <div key={member.revision_id} className="rounded border border-slate-200 p-2 dark:border-slate-800">
                  {member.relationship} · 合并置信 {(member.merge_score * 100).toFixed(0)}%{member.old_republication ? " · 旧闻重发" : ""}
                  <div className="muted mt-1">{member.explanation.reasons.join("；")}</div>
                  <div className="num muted mt-1 break-all">{member.revision_id}</div>
                </div>
              ))}
            </div>
          ) : <div className="muted mt-1">该文档尚未形成跨来源事件簇，独立来源数按 1 处理。</div>}
        </section>
        <details className="rounded border border-slate-200 p-2 dark:border-slate-800">
          <summary className="cursor-pointer font-semibold">历史修订（{revisions.length}）</summary>
          <div className="mt-2 space-y-2">{revisions.map((row) => (
            <div key={row.revision_id} className="rounded bg-slate-50 p-2 dark:bg-slate-900/70">
              <div>{row.title}</div>
              <div className="num muted mt-1 break-all">{row.revision_id}{row.supersedes_revision_id ? ` · 修订 ${row.supersedes_revision_id}` : " · 初版"}</div>
            </div>
          ))}</div>
        </details>
        <details className="rounded border border-slate-200 p-2 dark:border-slate-800">
          <summary className="cursor-pointer font-semibold">可复制诊断与证据标识</summary>
          <pre className="mt-2 max-h-56 overflow-auto whitespace-pre-wrap break-all rounded bg-slate-950 p-2 text-[10px] text-slate-200">{JSON.stringify({ revision_id: revision.revision_id, document_id: revision.document_id, event: item.event, entity_links: item.entity_links, parser_version: revision.parser_version, content_hash: revision.content_hash }, null, 2)}</pre>
          <button type="button" className="btn mt-2" onClick={() => void navigator.clipboard.writeText(JSON.stringify(item, null, 2))}>复制完整记录</button>
        </details>
      </div>
    </aside>
  );
}

export default function NewsPage() {
  const navigate = useNavigate();
  const listRef = useRef<HTMLDivElement>(null);
  const [query, setQuery] = useState<NewsCenterQuery>(EMPTY_QUERY);
  const [entityInputs, setEntityInputs] = useState(["", "", "", ""]);
  const [range, setRange] = useState("7d");
  const [data, setData] = useState<NewsCenterPage | null>(null);
  const [pendingData, setPendingData] = useState<NewsCenterPage | null>(null);
  const [providers, setProviders] = useState<NewsProviderHealthItem[]>([]);
  const [selected, setSelected] = useState<NewsCenterItem | null>(null);
  const [cluster, setCluster] = useState<NewsEventClusterDetail | null>(null);
  const [revisions, setRevisions] = useState<ArchivedNewsRevision[]>([]);
  const [detailLoading, setDetailLoading] = useState(false);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportHeight, setViewportHeight] = useState(600);
  const [filtersOpen, setFiltersOpen] = useState(true);

  const effectiveQuery = useMemo(() => {
    const now = Math.floor(Date.now() / 1000);
    const seconds = range === "24h" ? 86_400 : range === "7d" ? 7 * 86_400 : range === "30d" ? 30 * 86_400 : null;
    return {
      ...query,
      entity_keywords: entityInputs.filter((item) => item.trim()),
      from_utc: seconds ? now - seconds : null,
      to_utc: null,
    };
  }, [query, entityInputs, range]);

  const load = useCallback(async (nextQuery: NewsCenterQuery, quiet = false) => {
    if (!quiet) setLoading(true);
    try {
      const page = await queryNewsCenter(nextQuery);
      setData(page);
      setPendingData(null);
      setError(null);
    } catch (reason) {
      setError(errMsg(reason));
    } finally {
      if (!quiet) setLoading(false);
    }
  }, []);

  useEffect(() => {
    const timer = setTimeout(() => void load(effectiveQuery), 180);
    return () => clearTimeout(timer);
  }, [effectiveQuery, load]);

  useEffect(() => {
    const updateHealth = () => getNewsProviderHealth().then(setProviders).catch(() => undefined);
    void updateHealth();
    const timer = setInterval(updateHealth, 30_000);
    return () => clearInterval(timer);
  }, []);

  useEffect(() => {
    const timer = setInterval(async () => {
      try {
        const newest = await queryNewsCenter({ ...effectiveQuery, page: 1 });
        if (newest.newest_first_seen !== data?.newest_first_seen) {
          if (query.page === 1 && (listRef.current?.scrollTop ?? 0) < 80) setData(newest);
          else setPendingData(newest);
        }
      } catch {
        // Polling failure must not replace a readable last-good page.
      }
    }, 20_000);
    return () => clearInterval(timer);
  }, [effectiveQuery, data?.newest_first_seen, query.page]);

  useEffect(() => {
    const element = listRef.current;
    if (!element) return;
    const update = () => setViewportHeight(element.clientHeight);
    update();
    const observer = new ResizeObserver(update);
    observer.observe(element);
    return () => observer.disconnect();
  }, [loading, data?.items.length]);

  const runRefresh = async () => {
    setRefreshing(true);
    try {
      const result = await refreshNewsCenter([], query.keyword || null, null, 100);
      if (result.errors.length > 0) setError(`部分来源失败：${result.errors.join("；")}`);
      await load(effectiveQuery, true);
      const nextProviders = await getNewsProviderHealth();
      setProviders(nextProviders);
    } catch (reason) {
      setError(`上游刷新失败，继续显示最近成功缓存：${errMsg(reason)}`);
    } finally {
      setRefreshing(false);
    }
  };

  const updateState = async (
    item: NewsCenterItem,
    action: "read" | "pinned" | "favorite" | "ignored",
    value: boolean,
  ) => {
    try {
      const userState = await setNewsItemState(item.revision.document_id, action, value);
      setData((current) => current ? {
        ...current,
        items: current.items
          .map((row) => row.revision.document_id === item.revision.document_id ? { ...row, user_state: userState } : row)
          .filter((row) => !row.user_state.ignored || effectiveQuery.user_state === "ignored"),
      } : current);
      setSelected((current) => current?.revision.document_id === item.revision.document_id ? { ...current, user_state: userState } : current);
    } catch (reason) {
      setError(errMsg(reason));
    }
  };

  const openItem = async (item: NewsCenterItem) => {
    setSelected(item);
    setCluster(null);
    setRevisions([]);
    if (!item.user_state.is_read) void updateState(item, "read", true);
    setDetailLoading(true);
    try {
      const [history, detail] = await Promise.all([
        getNewsArchiveRevisions(item.revision.document_id),
        item.event ? getNewsEventClusterDetail(item.event.cluster_id) : Promise.resolve(null),
      ]);
      setRevisions(history);
      setCluster(detail);
    } catch (reason) {
      setError(errMsg(reason));
    } finally {
      setDetailLoading(false);
    }
  };

  const handToAgent = (priceIn: boolean) => {
    if (!selected) return;
    const prompt = priceIn
      ? `基于事件证据 ${selected.revision.revision_id}，分析“${selected.revision.title}”是否已经被市场交易。必须核对事件前异常收益、成交量、板块相对表现、估值变化和反方证据。`
      : `深度分析资讯“${selected.revision.title}”。精确证据修订号：${selected.revision.revision_id}；事件簇：${selected.event?.cluster_id ?? "尚未聚类"}。请核验原始来源、关联公司、产业链路径、反方证据和失效条件。`;
    useAgentSession.getState().setInput(prompt);
    navigate("/agent");
  };

  const rangeWindow = virtualRange(scrollTop, viewportHeight, data?.items.length ?? 0);
  const visibleItems = data?.items.slice(rangeWindow.start, rangeWindow.end) ?? [];
  const totalPages = Math.max(1, Math.ceil((data?.total ?? 0) / effectiveQuery.page_size));
  const unavailable = providers.filter((provider) => !provider.enabled || provider.circuit_state !== "closed" || provider.last_error_kind);
  const staleProviders = providers.filter((provider) => provider.stale);

  const patchQuery = (patch: Partial<NewsCenterQuery>) => {
    setQuery((current) => ({ ...current, ...patch, page: patch.page ?? 1 }));
    listRef.current?.scrollTo({ top: 0 });
  };

  return (
    <div className="flex h-full min-w-0 flex-col overflow-hidden">
      <header className="shrink-0 border-b border-slate-200 bg-white px-3 py-2 dark:border-slate-800 dark:bg-[#0d1524]">
        <div className="flex flex-wrap items-center gap-2">
          <div className="mr-auto">
            <h1 className="text-base font-semibold">资讯与事件中心</h1>
            <p className="muted mt-0.5 text-[11px]">多源快讯、正式披露、修订追踪与字段级证据 · NewsNow 仅是发现来源之一</p>
          </div>
          <span className="muted text-[11px]">归档 {data?.total ?? 0} 条 · 最近同步 {ageText(data?.archive_age_secs ?? null)}</span>
          <button type="button" className="btn" onClick={() => setFiltersOpen((value) => !value)}>{filtersOpen ? "收起筛选" : "展开筛选"}</button>
          <button type="button" className="btn-primary" disabled={refreshing} onClick={runRefresh}>{refreshing ? "正在增量刷新…" : "刷新上游"}</button>
        </div>
        <nav className="mt-2 flex flex-wrap gap-1" aria-label="资讯分区">
          {CATEGORY.map(([id, label]) => <button key={id} type="button" className={`chip ${query.category === id ? "bg-blue-600 text-white" : ""}`} onClick={() => patchQuery({ category: id })}>{label}</button>)}
        </nav>
      </header>

      {(unavailable.length > 0 || staleProviders.length > 0) && (
        <div className="shrink-0 border-b border-amber-200 bg-amber-50 px-3 py-1.5 text-xs text-amber-800 dark:border-amber-900 dark:bg-amber-950/30 dark:text-amber-300">
          正在显示最近成功缓存（{ageText(data?.archive_age_secs ?? null)}）。
          {unavailable.length > 0 && ` ${unavailable.length} 个来源当前不可用。`}
          {staleProviders.length > 0 && ` ${staleProviders.length} 个来源缓存已过建议刷新间隔。`}
          可继续浏览、筛选和追溯，系统不会显示空白成功态。
        </div>
      )}
      {error && <div className="shrink-0 px-3 pt-2"><ErrorBox message={error} onRetry={() => void load(effectiveQuery)} /></div>}
      {pendingData && (
        <button type="button" className="shrink-0 border-b border-blue-200 bg-blue-50 py-1.5 text-xs text-blue-700 dark:border-blue-900 dark:bg-blue-950/30 dark:text-blue-300" onClick={() => { setData(pendingData); setPendingData(null); patchQuery({ page: 1 }); }}>
          发现新的资讯或修订，点击更新；当前阅读位置不会被自动打断
        </button>
      )}

      <div className="flex min-h-0 min-w-0 flex-1">
        {filtersOpen && (
          <aside className="w-64 shrink-0 overflow-auto border-r border-slate-200 bg-slate-50/60 p-3 dark:border-slate-800 dark:bg-slate-950/30">
            <div className="space-y-3 text-xs">
              <label className="block"><span className="muted">全文/标题搜索</span><input className="input mt-1 w-full" placeholder="关键词、政策、公司" value={query.keyword} onChange={(event) => patchQuery({ keyword: event.target.value })} /></label>
              <label className="block"><span className="muted">来源</span><select className="input mt-1 w-full" value={query.source_id} onChange={(event) => patchQuery({ source_id: event.target.value })}><option value="">全部来源</option>{data?.source_facets.map((source) => <option key={source.source_id} value={source.source_id}>{source.source_name}（{source.count}）</option>)}</select></label>
              <div className="grid grid-cols-2 gap-2">
                <label><span className="muted">重要性</span><select className="input mt-1 w-full" value={query.importance} onChange={(event) => patchQuery({ importance: event.target.value })}><option value="">全部</option><option value="important">仅重要</option></select></label>
                <label><span className="muted">时间范围</span><select className="input mt-1 w-full" value={range} onChange={(event) => { setRange(event.target.value); patchQuery({}); }}><option value="24h">24小时</option><option value="7d">7天</option><option value="30d">30天</option><option value="all">全部历史</option></select></label>
              </div>
              <label className="block"><span className="muted">事件类型</span><select className="input mt-1 w-full" value={query.event_type} onChange={(event) => patchQuery({ event_type: event.target.value })}><option value="">全部事件</option>{Object.entries(EVENT_LABEL).map(([id, label]) => <option key={id} value={id}>{label}</option>)}</select></label>
              <div className="grid grid-cols-2 gap-2">
                <label><span className="muted">语言</span><select className="input mt-1 w-full" value={query.language} onChange={(event) => patchQuery({ language: event.target.value })}><option value="">全部</option><option value="zh-CN">中文</option><option value="en">英文</option></select></label>
                <label><span className="muted">个人状态</span><select className="input mt-1 w-full" value={query.user_state} onChange={(event) => patchQuery({ user_state: event.target.value })}><option value="">全部</option><option value="unread">未读</option><option value="pinned">置顶</option><option value="favorite">收藏</option><option value="ignored">已忽略</option></select></label>
              </div>
              <label className="block"><span className="muted">核验状态</span><select className="input mt-1 w-full" value={query.verification} onChange={(event) => patchQuery({ verification: event.target.value })}><option value="">全部</option><option value="primary">一手披露</option><option value="verified_media">已归档媒体</option><option value="archived">已归档来源</option><option value="discovery_only">仅发现线索</option></select></label>
              <div className="border-t border-slate-200 pt-3 dark:border-slate-800">
                <div className="font-medium">实体与产业筛选</div>
                {["证券代码/公司", "行业", "产业链/产品", "商品"].map((label, index) => <input key={label} className="input mt-1.5 w-full" placeholder={label} value={entityInputs[index]} onChange={(event) => { setEntityInputs((current) => current.map((item, itemIndex) => itemIndex === index ? event.target.value : item)); patchQuery({}); }} />)}
                <div className="muted mt-1.5 text-[10px]">多项同时填写时采用交集；实体映射未核验的同名项不会进入 Agent。</div>
              </div>
              <button type="button" className="btn w-full justify-center" onClick={() => { setQuery(EMPTY_QUERY); setEntityInputs(["", "", "", ""]); setRange("7d"); }}>重置全部筛选</button>
            </div>
          </aside>
        )}

        <section className="flex min-w-0 flex-1 flex-col overflow-hidden">
          <div className="flex h-9 shrink-0 items-center justify-between border-b border-slate-200 px-3 text-xs dark:border-slate-800">
            <span>命中 <b className="num">{data?.total ?? 0}</b> 条 · 第 {query.page}/{totalPages} 页</span>
            <span className="muted">↑↓ 键移动 · Enter 展开 · 页面增量不会抢走阅读位置</span>
          </div>
          {loading && !data ? <Loading text="正在读取资讯档案与事件证据…" /> : data?.items.length ? (
            <div ref={listRef} className="relative min-h-0 flex-1 overflow-auto py-1" onScroll={(event) => setScrollTop(event.currentTarget.scrollTop)}>
              <div className="relative" style={{ height: rangeWindow.totalHeight }}>
                {visibleItems.map((item, localIndex) => {
                  const index = rangeWindow.start + localIndex;
                  return <NewsRow key={item.revision.revision_id} item={item} index={index} selected={selected?.revision.revision_id === item.revision.revision_id} onOpen={openItem} onState={updateState} />;
                })}
              </div>
            </div>
          ) : (
            <div className="flex min-h-0 flex-1 items-center justify-center p-8 text-center">
              <div><div className="text-sm font-semibold">当前筛选没有资讯</div><p className="muted mt-1 text-xs">可能是时间范围或核验条件过窄；若所有来源失败，最近成功缓存仍会保留在其他筛选中。</p><button type="button" className="btn mt-3" onClick={() => { setQuery(EMPTY_QUERY); setEntityInputs(["", "", "", ""]); setRange("all"); }}>查看全部历史</button></div>
            </div>
          )}
          <footer className="flex h-11 shrink-0 items-center justify-between border-t border-slate-200 px-3 text-xs dark:border-slate-800">
            <button type="button" className="btn" disabled={query.page <= 1} onClick={() => patchQuery({ page: query.page - 1 })}>上一页</button>
            <div className="flex gap-1">{pageTokens(query.page, totalPages).map((token, index) => token === "ellipsis" ? <span key={`e-${index}`} className="px-1">…</span> : <button key={token} type="button" className={`chip ${token === query.page ? "bg-blue-600 text-white" : ""}`} onClick={() => patchQuery({ page: token })}>{token}</button>)}</div>
            <button type="button" className="btn" disabled={!data?.has_more} onClick={() => patchQuery({ page: query.page + 1 })}>下一页</button>
          </footer>
        </section>

        {selected && <DetailPanel item={selected} cluster={cluster} revisions={revisions} loading={detailLoading} onClose={() => setSelected(null)} onStock={(code) => navigate(`/stock/${code}`)} onGraph={() => navigate(`/graph?evidence=${encodeURIComponent(selected.revision.revision_id)}`)} onAgent={handToAgent} onIgnore={() => void updateState(selected, "ignored", true)} />}
      </div>
    </div>
  );
}
