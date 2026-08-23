import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import {
  disclosureSyncCancel,
  disclosureSyncStart,
  disclosureSyncStatus,
  errMsg,
  getDisclosureDetail,
  getDisclosureProviderHealth,
  queryDisclosures,
  startRelationExtraction,
  type DisclosureDetail,
  type DisclosureListItem,
  type DisclosurePage,
  type DisclosureProviderHealth,
  type DisclosureSyncSnapshot,
  type RelationDocumentKind,
} from "../lib/api";
import { useAgentSession } from "../agentSession";
import { ErrorBox, Loading } from "../components/ui";

const CATEGORIES = [
  ["all", "全部类型"], ["periodic_report", "定期报告"], ["earnings_forecast", "业绩预告/快报"],
  ["inquiry_reply", "问询回复"], ["contract", "合同/中标"], ["buyback", "股份回购"],
  ["holding_change", "持股变动"], ["unlock", "限售解禁"], ["penalty", "监管处罚"],
  ["litigation", "诉讼仲裁"], ["guarantee", "对外担保"], ["pledge", "股份质押"],
] as const;

const EMPTY_PAGE: DisclosurePage = { items: [], total: 0, page: 1, page_size: 50, total_pages: 0 };

function timeText(value: number | null) {
  return value == null ? "来源未提供" : new Date(value * 1000).toLocaleString("zh-CN", { hour12: false });
}

function durationText(value: number | null) {
  if (value == null) return "未知";
  if (value < 60) return `${Math.max(0, value)} 秒`;
  if (value < 3600) return `${Math.floor(value / 60)} 分钟`;
  return `${Math.floor(value / 3600)} 小时`;
}

export function disclosurePageTokens(page: number, total: number): Array<number | "ellipsis"> {
  const pages = [...new Set([1, total, page - 1, page, page + 1])].filter((value) => value >= 1 && value <= total).sort((a, b) => a - b);
  const output: Array<number | "ellipsis"> = [];
  pages.forEach((value, index) => { if (index && value - pages[index - 1] > 1) output.push("ellipsis"); output.push(value); });
  return output;
}

export function disclosurePollDelay(running: boolean): number {
  return running ? 750 : 3000;
}

function SyncPanel({ snapshot, expanded, onToggle, onCancel }: { snapshot: DisclosureSyncSnapshot; expanded: boolean; onToggle: () => void; onCancel: () => void }) {
  const diagnosis = JSON.stringify(snapshot, null, 2);
  return (
    <section className="card shrink-0 overflow-hidden">
      <button type="button" className="flex w-full items-center gap-3 px-3 py-2 text-left" onClick={onToggle} aria-expanded={expanded}>
        <span className={`h-2.5 w-2.5 rounded-full ${snapshot.status === "failed" ? "bg-red-500" : snapshot.running ? "animate-pulse bg-blue-500" : snapshot.status === "completed" ? "bg-emerald-500" : "bg-slate-400"}`} />
        <div className="min-w-0 flex-1">
          <div className="flex items-center justify-between gap-3 text-xs"><b>{snapshot.phase}</b><span className="num">{snapshot.progress}%</span></div>
          <div className="mt-1 h-1.5 overflow-hidden rounded bg-slate-200 dark:bg-slate-800"><div className={`h-full transition-all ${snapshot.status === "failed" ? "bg-red-500" : "bg-blue-500"}`} style={{ width: `${snapshot.progress}%` }} /></div>
          <div className="muted mt-1 truncate text-[10px]">{snapshot.current_provider}{snapshot.current_item ? ` · ${snapshot.current_item}` : ""}{snapshot.running && snapshot.estimated_remaining_seconds != null ? ` · 预计还需约 ${durationText(snapshot.estimated_remaining_seconds)}` : ""}</div>
        </div>
        <span className="muted text-xs">{expanded ? "收起详情" : "展开工作详情"}</span>
      </button>
      {expanded && <div className="border-t border-slate-200 p-3 text-xs dark:border-slate-800">
        <div className="grid grid-cols-3 gap-2 lg:grid-cols-7">
          {[["发现", snapshot.discovered], ["已规范化", snapshot.normalized], ["新增", snapshot.inserted], ["去重", snapshot.deduplicated], ["正式原文核验", snapshot.primary_verified], ["待核验", snapshot.needs_review], ["失败", snapshot.failures]].map(([label, value]) =>
            <div key={String(label)} className="rounded bg-slate-50 p-2 dark:bg-slate-900"><div className="muted text-[10px]">{label}</div><b className="num text-sm">{value}</b></div>)}
        </div>
        {snapshot.error && <div className="mt-2 rounded border border-red-300 bg-red-50 p-2 text-red-700 dark:border-red-900 dark:bg-red-950/30 dark:text-red-300">{snapshot.error}</div>}
        <div className="mt-2 max-h-40 overflow-auto rounded bg-slate-950 p-2 font-mono text-[10px] leading-5 text-slate-300">
          {snapshot.recent_logs.length ? snapshot.recent_logs.map((line, index) => <div key={`${index}-${line}`}>{line}</div>) : <div>暂无工作日志</div>}
        </div>
        <div className="mt-2 flex gap-2">
          <button type="button" className="btn" onClick={() => navigator.clipboard.writeText(diagnosis)}>复制诊断信息</button>
          {snapshot.running && <button type="button" className="btn-danger" onClick={onCancel}>取消后台任务</button>}
        </div>
      </div>}
    </section>
  );
}

function ProviderPanel({ providers, onClose }: { providers: DisclosureProviderHealth[]; onClose: () => void }) {
  return <aside className="absolute inset-y-0 right-0 z-30 flex w-[520px] max-w-full flex-col border-l border-slate-200 bg-white shadow-2xl dark:border-slate-800 dark:bg-[#0d1524]">
    <div className="flex items-center justify-between border-b border-slate-200 px-3 py-2 dark:border-slate-800"><b>正式来源、频率与重试状态</b><button type="button" className="btn" onClick={onClose}>关闭</button></div>
    <div className="min-h-0 flex-1 space-y-2 overflow-auto p-3">
      {providers.map((provider) => <article key={provider.provider_id} className="rounded border border-slate-200 p-3 text-xs dark:border-slate-800">
        <div className="flex items-start justify-between gap-3"><div><b>{provider.provider_name}</b><div className="muted mt-0.5">{provider.authority_name}</div></div><span className={`rounded px-2 py-0.5 ${provider.consecutive_failures ? "bg-red-500/10 text-red-600" : "bg-emerald-500/10 text-emerald-600"}`}>{provider.consecutive_failures ? `连续失败 ${provider.consecutive_failures} 次` : "就绪"}</span></div>
        <div className="mt-2 grid grid-cols-2 gap-2"><div>目标发现延迟：≤ {durationText(provider.target_latency_secs)}</div><div>访问上限：{provider.rate_limit_per_minute} 次/分钟</div><div>最近成功：{timeText(provider.last_success_at)}</div><div>下次重试：{timeText(provider.retry_after)}</div></div>
        <p className="muted mt-2 leading-5">{provider.note}</p>
        {provider.last_error && <div className="mt-2 rounded bg-red-500/10 p-2 text-red-600 dark:text-red-300">{provider.last_error}</div>}
        {provider.public_index_url && <button type="button" className="mt-2 text-blue-600 underline" onClick={() => window.open(provider.public_index_url, "_blank")}>打开公开披露入口</button>}
      </article>)}
    </div>
  </aside>;
}

function DetailPanel({ detail, loading, onClose, onAgent, onExtract }: { detail: DisclosureDetail | null; loading: boolean; onClose: () => void; onAgent: (detail: DisclosureDetail) => void; onExtract: (detail: DisclosureDetail) => void }) {
  return <aside className="flex min-h-0 w-[520px] shrink-0 flex-col border-l border-slate-200 bg-white dark:border-slate-800 dark:bg-[#0d1524]">
    <div className="flex items-center justify-between border-b border-slate-200 px-3 py-2 dark:border-slate-800"><b>公告原文与证据详情</b><button type="button" className="btn" onClick={onClose}>关闭</button></div>
    <div className="min-h-0 flex-1 overflow-auto p-3 text-xs">{loading ? <Loading text="正在读取附件、修订和结构化事件…" /> : detail ? <div className="space-y-3">
      <div><div className="flex flex-wrap gap-1"><span className={`rounded px-2 py-0.5 ${detail.primary_verified ? "bg-emerald-500/10 text-emerald-700 dark:text-emerald-300" : "bg-amber-500/10 text-amber-700 dark:text-amber-300"}`}>{detail.primary_verified ? "正式原文已核验" : "仅发现，原文待核验"}</span><span className="rounded bg-slate-100 px-2 py-0.5 dark:bg-slate-800">{detail.category_name}</span><span className="rounded bg-violet-500/10 px-2 py-0.5 text-violet-600">{detail.status_name}</span></div><h2 className="mt-2 text-base font-semibold leading-6">{detail.title}</h2></div>
      <div className={`rounded border p-2 ${detail.primary_verified ? "border-emerald-200 bg-emerald-50 dark:border-emerald-900 dark:bg-emerald-950/30" : "border-amber-200 bg-amber-50 dark:border-amber-900 dark:bg-amber-950/30"}`}>{detail.verification_note}</div>
      <div className="grid grid-cols-2 gap-2 rounded bg-slate-50 p-2 dark:bg-slate-900"><div><span className="muted">发布时间</span><div>{timeText(detail.published_at)}</div></div><div><span className="muted">首次发现</span><div>{timeText(detail.first_seen_at)}</div></div><div><span className="muted">发现延迟</span><div>{durationText(detail.discovery_latency_secs)}</div></div><div><span className="muted">解析状态</span><div>{detail.extraction_status}</div></div></div>
      <div className="flex flex-wrap gap-2"><button type="button" className="btn-primary" onClick={() => onAgent(detail)}>交给智能助手核验并深入分析</button><button type="button" className="btn" disabled={!relationSourceVersion(detail)} onClick={() => onExtract(detail)}>{relationSourceVersion(detail) ? "后台抽取供应链关系" : "原文归档后可抽取关系"}</button></div>
      <section><h3 className="font-semibold">证券关联</h3><div className="mt-1 flex flex-wrap gap-1">{detail.securities.map((security) => <span key={security.code} className="chip">{security.name || "名称未知"} {security.code} · {security.market}</span>)}</div></section>
      <section><h3 className="font-semibold">披露入口（{detail.sources.length}）</h3><div className="mt-1 space-y-1">{detail.sources.map((source) => <div key={source.source_id} className="rounded border border-slate-200 p-2 dark:border-slate-800"><div className="flex justify-between"><b>{source.provider_name}</b><span>{source.authority_name}</span></div><button type="button" className="mt-1 max-w-full truncate text-left text-blue-600 underline" onClick={() => window.open(source.original_url, "_blank")}>{source.original_url}</button></div>)}</div></section>
      <section><h3 className="font-semibold">附件层级（{detail.attachments.length}）</h3>{detail.attachments.length ? <div className="mt-1 space-y-1">{detail.attachments.map((attachment) => <div key={attachment.attachment_id} className="rounded border border-slate-200 p-2 dark:border-slate-800"><b>{attachment.parent_attachment_id ? "└ 附件 · " : "原文 · "}{attachment.name}</b><div className="muted mt-1">{attachment.media_type} · {attachment.page_count == null ? "页数未知" : `${attachment.page_count} 页`} · {attachment.extraction_status}</div>{attachment.review_reason && <div className="mt-1 text-amber-600">{attachment.review_reason}</div>}</div>)}</div> : <p className="muted mt-1">索引尚未归档附件，不能据此推断正文内容。</p>}</section>
      <section><h3 className="font-semibold">结构化事件（{detail.events.length}）</h3>{detail.events.length ? <div className="mt-1 space-y-1">{detail.events.map((event) => <details key={event.event_id} className="rounded border border-slate-200 p-2 dark:border-slate-800"><summary className="cursor-pointer font-medium">{event.event_type} · 查看字段与证据</summary><pre className="mt-2 overflow-auto whitespace-pre-wrap text-[10px]">{JSON.stringify({ fields: event.fields, evidence: event.evidence, parser: event.parser_version }, null, 2)}</pre></details>)}</div> : <p className="muted mt-1">正式正文未解析前不生成结构化数字。</p>}</section>
      {detail.revisions.length > 0 && <section><h3 className="font-semibold">修订/撤回链</h3><div className="mt-1 space-y-1">{detail.revisions.map((revision) => <div key={revision.disclosure_id} className="rounded bg-violet-500/10 p-2">{revision.status_name} · {revision.title} · {timeText(revision.first_seen_at)}</div>)}</div></section>}
      {detail.review_reason && <div className="rounded bg-amber-500/10 p-2 text-amber-700 dark:text-amber-300">审核原因：{detail.review_reason}</div>}
    </div> : <div className="muted p-6 text-center">没有可显示的详情</div>}</div>
  </aside>;
}

export default function DisclosurePage() {
  const navigate = useNavigate();
  const setAgentInput = useAgentSession((state) => state.setInput);
  const [page, setPage] = useState<DisclosurePage>(EMPTY_PAGE);
  const [code, setCode] = useState(""); const [keyword, setKeyword] = useState("");
  const [category, setCategory] = useState("all"); const [status, setStatus] = useState("all");
  const [primaryOnly, setPrimaryOnly] = useState(false); const [pageNo, setPageNo] = useState(1);
  const [loading, setLoading] = useState(true); const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<DisclosureListItem | null>(null);
  const [detail, setDetail] = useState<DisclosureDetail | null>(null); const [detailLoading, setDetailLoading] = useState(false);
  const [sync, setSync] = useState<DisclosureSyncSnapshot | null>(null); const [syncExpanded, setSyncExpanded] = useState(false);
  const [providers, setProviders] = useState<DisclosureProviderHealth[]>([]); const [showProviders, setShowProviders] = useState(false);

  const load = useCallback(async () => {
    setLoading(true); setError(null);
    try { setPage(await queryDisclosures({ security_code: code || null, keyword: keyword || null, category, status, primary_only: primaryOnly, page: pageNo, page_size: 50 })); }
    catch (reason) { setError(errMsg(reason)); } finally { setLoading(false); }
  }, [code, keyword, category, status, primaryOnly, pageNo]);
  useEffect(() => { void load(); }, [load]);
  useEffect(() => {
    let active = true; let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try {
        const value = await disclosureSyncStatus();
        if (!active) return;
        setSync(value);
        // Keep a low-frequency watcher even while idle. A task can be started
        // after this effect's first read; stopping the watcher in that state
        // left the UI frozen on the start snapshot until the page was reopened.
        timer = setTimeout(poll, disclosurePollDelay(value.running));
      } catch {
        if (active) timer = setTimeout(poll, 3000);
      }
    };
    void poll(); return () => { active = false; clearTimeout(timer); };
  }, []);
  useEffect(() => { if (!sync?.running && sync?.status === "completed") void load(); }, [sync?.running, sync?.status, load]);

  const openDetail = async (item: DisclosureListItem) => { setSelected(item); setDetail(null); setDetailLoading(true); try { setDetail(await getDisclosureDetail(item.disclosure_id)); } catch (reason) { setError(errMsg(reason)); } finally { setDetailLoading(false); } };
  const startSync = async () => { setError(null); try { await disclosureSyncStart({ security_code: code || undefined, days: 365, max_pages: code ? 10 : 3 }); setSyncExpanded(true); setSync(await disclosureSyncStatus()); } catch (reason) { setError(errMsg(reason)); } };
  const openProviders = async () => { try { setProviders(await getDisclosureProviderHealth()); setShowProviders(true); } catch (reason) { setError(errMsg(reason)); } };
  const askAgent = (item: DisclosureDetail) => { setAgentInput(`请优先核验正式披露 ${item.disclosure_id} 的交易所/巨潮/公司原文与附件，再分析其对 ${item.securities.map((security) => security.code).join("、") || "相关公司"} 的影响。请逐项引用 source_version_id、PDF 页码/表格单元格；如果当前只有镜像发现记录，明确说明“原文未核验”，不要提高结论置信度。`); navigate("/agent"); };
  const extractRelations = async (item: DisclosureDetail) => { const version = relationSourceVersion(item); if (!version) return; setError(null); try { const task = await startRelationExtraction(version, disclosureRelationKind(item)); localStorage.setItem("astock_relation_job", task.job_id); navigate(`/graph?relation_job=${encodeURIComponent(task.job_id)}`); } catch (reason) { setError(errMsg(reason)); } };
  const summary = useMemo(() => `${page.total.toLocaleString("zh-CN")} 条正式披露记录`, [page.total]);

  return <div className="relative flex h-full min-w-0 flex-col overflow-hidden p-3">
    <div className="mb-3 flex shrink-0 flex-wrap items-center gap-2">
      <div className="mr-auto"><h1 className="text-base font-semibold">正式披露中心</h1><p className="muted mt-0.5 text-[11px]">交易所 / 巨潮 / 证监会 / 公司 IR 独立证据面 · 镜像只作发现</p></div>
      <button type="button" className="btn" onClick={openProviders}>来源频率与健康状态</button>
      <button type="button" className="btn-primary" disabled={sync?.running} onClick={startSync}>{sync?.running ? "后台同步中…" : "增量同步"}</button>
    </div>
    {sync && sync.status !== "idle" && <div className="mb-3"><SyncPanel snapshot={sync} expanded={syncExpanded} onToggle={() => setSyncExpanded((value) => !value)} onCancel={async () => { await disclosureSyncCancel(); setSync(await disclosureSyncStatus()); }} /></div>}
    <div className="card mb-3 flex shrink-0 flex-wrap items-end gap-2 p-3 text-xs">
      <label><span className="muted mb-1 block">证券代码</span><input className="input w-32" value={code} onChange={(event) => { setCode(event.target.value.replace(/\D/g, "").slice(0, 6)); setPageNo(1); }} placeholder="如 600519" /></label>
      <label><span className="muted mb-1 block">标题关键词</span><input className="input w-52" value={keyword} onChange={(event) => { setKeyword(event.target.value); setPageNo(1); }} placeholder="报告、回购、问询…" /></label>
      <label><span className="muted mb-1 block">公告类型</span><select className="input" value={category} onChange={(event) => { setCategory(event.target.value); setPageNo(1); }}>{CATEGORIES.map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select></label>
      <label><span className="muted mb-1 block">有效状态</span><select className="input" value={status} onChange={(event) => { setStatus(event.target.value); setPageNo(1); }}><option value="all">全部状态</option><option value="active">有效</option><option value="revised">修订版</option><option value="cancelled">已取消/撤回</option></select></label>
      <label className="flex h-8 items-center gap-2 rounded border border-slate-200 px-2 dark:border-slate-700"><input type="checkbox" checked={primaryOnly} onChange={(event) => { setPrimaryOnly(event.target.checked); setPageNo(1); }} />只看正式原文入口</label>
      <button type="button" className="btn" onClick={() => { setCode(""); setKeyword(""); setCategory("all"); setStatus("all"); setPrimaryOnly(false); setPageNo(1); }}>清空筛选</button>
    </div>
    {error && <div className="mb-3 shrink-0"><ErrorBox message={error} /></div>}
    <div className="card flex min-h-0 flex-1 overflow-hidden">
      <div className="flex min-w-0 flex-1 flex-col">
        <div className="muted flex shrink-0 items-center justify-between border-b border-slate-200 px-3 py-2 text-[11px] dark:border-slate-800"><span>{summary}</span><span>第 {page.page || 1} / {Math.max(1, page.total_pages)} 页 · 每页 {page.page_size} 条</span></div>
        <div className="min-h-0 flex-1 overflow-auto">{loading ? <Loading text="正在读取正式披露时间线…" /> : page.items.length ? <table className="w-full text-left text-xs"><thead className="sticky top-0 z-10 bg-slate-100 dark:bg-slate-900"><tr><th className="px-3 py-2">发布时间 / 证券</th><th className="px-3 py-2">公告标题</th><th className="px-3 py-2">类型与状态</th><th className="px-3 py-2">来源核验</th><th className="px-3 py-2">发现延迟</th></tr></thead><tbody>{page.items.map((item) => <tr key={item.disclosure_id} className={`cursor-pointer border-t border-slate-100 hover:bg-blue-50 dark:border-slate-800 dark:hover:bg-blue-950/20 ${selected?.disclosure_id === item.disclosure_id ? "bg-blue-50 dark:bg-blue-950/30" : ""}`} onClick={() => openDetail(item)}><td className="px-3 py-2 align-top"><div>{timeText(item.published_at)}</div><div className="muted mt-1">{item.securities.map((security) => `${security.name || ""} ${security.code}`).join("、") || "未关联证券"}</div></td><td className="max-w-xl px-3 py-2 align-top"><b className="leading-5">{item.title}</b>{item.review_reason && <div className="mt-1 line-clamp-1 text-amber-600">{item.review_reason}</div>}</td><td className="px-3 py-2 align-top"><span className="rounded bg-slate-100 px-2 py-0.5 dark:bg-slate-800">{item.category_name}</span><div className="mt-1">{item.status_name}</div></td><td className="px-3 py-2 align-top"><span className={`rounded px-2 py-0.5 ${item.primary_verified ? "bg-emerald-500/10 text-emerald-600" : "bg-amber-500/10 text-amber-600"}`}>{item.primary_verified ? "正式原文已归档" : "待正式原文核验"}</span><div className="muted mt-1">{item.sources.length} 个入口</div></td><td className="num px-3 py-2 align-top">{durationText(item.discovery_latency_secs)}</td></tr>)}</tbody></table> : <div className="muted flex h-full flex-col items-center justify-center gap-2"><b>当前筛选范围没有披露记录</b><span>可调整筛选或点击“增量同步”；长任务会在后台继续。</span></div>}</div>
        <div className="flex shrink-0 items-center justify-between border-t border-slate-200 px-3 py-2 text-xs dark:border-slate-800"><span className="muted">所有缺失均显示原因，不使用“——”掩盖数据状态</span><div className="flex gap-1">{disclosurePageTokens(pageNo, page.total_pages).map((token, index) => token === "ellipsis" ? <span key={`ellipsis-${index}`} className="px-1">…</span> : <button key={token} type="button" className={`chip ${token === pageNo ? "bg-blue-600 text-white" : ""}`} onClick={() => setPageNo(token)}>{token}</button>)}</div></div>
      </div>
      {selected && <DetailPanel detail={detail} loading={detailLoading} onClose={() => { setSelected(null); setDetail(null); }} onAgent={askAgent} onExtract={extractRelations} />}
    </div>
    {showProviders && <ProviderPanel providers={providers} onClose={() => setShowProviders(false)} />}
  </div>;
}

export function relationSourceVersion(detail: DisclosureDetail): string | null {
  return detail.source_version_id ?? detail.attachments.find((item) => item.source_version_id)?.source_version_id ?? null;
}

export function disclosureRelationKind(detail: Pick<DisclosureDetail, "title" | "category">): RelationDocumentKind {
  const title = detail.title;
  if (/招股|募集说明书/.test(title)) return "prospectus";
  if (/半年度|半年报/.test(title)) return "semi_annual_report";
  if (/年度报告|年报/.test(title)) return "annual_report";
  if (/调研|投资者关系|业绩说明会/.test(title)) return "investor_relations";
  if (/招标|投标|中标/.test(title)) return "tender";
  if (/合同|订单|协议/.test(title) || detail.category === "contract") return "major_contract";
  if (/专利/.test(title)) return "patent";
  if (/环评|产能|扩产|投产/.test(title)) return "capacity_eia";
  return "other";
}
