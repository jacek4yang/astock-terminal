import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import { useAgentSession } from "../agentSession";
import { ErrorBox, Loading } from "../components/ui";
import {
  errMsg,
  getGlobalGoldenChains,
  getGlobalProviderHealth,
  globalSyncCancel,
  globalSyncStart,
  globalSyncStatus,
  queryGlobalDocuments,
  type GlobalDocumentListItem,
  type GlobalDocumentPage,
  type GlobalGoldenChain,
  type GlobalProviderRuntime,
  type GlobalSyncSnapshot,
} from "../lib/api";

const EMPTY_PAGE: GlobalDocumentPage = { items: [], total: 0, page: 1, page_size: 50, total_pages: 0 };

const CATEGORY_LABELS: Record<string, string> = {
  company_disclosure: "海外公司正式披露",
  macro_policy: "宏观与政策原始数据",
  trade_regulation: "贸易与监管原文",
  energy_commodity: "能源、商品与持仓",
};

function timeText(value: number | null) {
  return value == null ? "尚无记录" : new Date(value * 1000).toLocaleString("zh-CN", { hour12: false });
}

function durationText(value: number | null) {
  if (value == null) return "未知";
  if (value < 60) return `${Math.max(0, value)} 秒`;
  if (value < 3600) return `${Math.ceil(value / 60)} 分钟`;
  return `${Math.ceil(value / 3600)} 小时`;
}

export function globalPageTokens(page: number, total: number): Array<number | "ellipsis"> {
  if (total <= 0) return [];
  const pages = [...new Set([1, total, page - 1, page, page + 1])]
    .filter((value) => value >= 1 && value <= total)
    .sort((a, b) => a - b);
  const output: Array<number | "ellipsis"> = [];
  pages.forEach((value, index) => {
    if (index && value - pages[index - 1] > 1) output.push("ellipsis");
    output.push(value);
  });
  return output;
}

export function globalPollDelay(running: boolean) {
  return running ? 750 : 3000;
}

function SyncPanel({ snapshot, expanded, onToggle, onCancel }: {
  snapshot: GlobalSyncSnapshot; expanded: boolean; onToggle: () => void; onCancel: () => void;
}) {
  const diagnosis = JSON.stringify(snapshot, null, 2);
  const success = snapshot.status === "completed";
  return <section className="card shrink-0 overflow-hidden">
    <button type="button" className="flex w-full items-center gap-3 px-3 py-2 text-left" onClick={onToggle} aria-expanded={expanded}>
      <span className={`h-2.5 w-2.5 rounded-full ${snapshot.status === "failed" ? "bg-red-500" : snapshot.running ? "animate-pulse bg-blue-500" : success ? "bg-emerald-500" : "bg-amber-500"}`} />
      <div className="min-w-0 flex-1">
        <div className="flex items-center justify-between gap-3 text-xs"><b>{snapshot.phase}</b><span className="num">{snapshot.progress}%</span></div>
        <div className="mt-1 h-1.5 overflow-hidden rounded bg-slate-200 dark:bg-slate-800"><div className={`h-full transition-all ${snapshot.status === "failed" ? "bg-red-500" : "bg-blue-500"}`} style={{ width: `${snapshot.progress}%` }} /></div>
        <div className="muted mt-1 truncate text-[10px]">{snapshot.current_provider}{snapshot.current_item ? ` · ${snapshot.current_item}` : ""}{snapshot.running && snapshot.estimated_remaining_seconds != null ? ` · 预计还需约 ${durationText(snapshot.estimated_remaining_seconds)}` : ""}</div>
      </div>
      <span className="muted text-xs">{expanded ? "收起详情" : "展开工作详情"}</span>
    </button>
    {expanded && <div className="border-t border-slate-200 p-3 text-xs dark:border-slate-800">
      <div className="grid grid-cols-4 gap-2 lg:grid-cols-8">
        {[["来源总数", snapshot.sources_total], ["可访问", snapshot.sources_ready], ["来源缺口", snapshot.source_gaps], ["发现文档", snapshot.documents_discovered], ["原文归档", snapshot.documents_archived], ["保存观测", snapshot.observations_saved], ["证据路径", snapshot.mapping_paths], ["失败", snapshot.failures]].map(([label, value]) =>
          <div key={String(label)} className="rounded bg-slate-50 p-2 dark:bg-slate-900"><div className="muted text-[10px]">{label}</div><b className="num text-sm">{value}</b></div>)}
      </div>
      {snapshot.error && <div className="mt-2 rounded border border-red-300 bg-red-50 p-2 text-red-700 dark:border-red-900 dark:bg-red-950/30 dark:text-red-300">{snapshot.error}</div>}
      <div className="mt-2 max-h-44 overflow-auto rounded bg-slate-950 p-2 font-mono text-[10px] leading-5 text-slate-300">
        {snapshot.recent_logs.length ? snapshot.recent_logs.map((line, index) => <div key={`${index}-${line}`}>{line}</div>) : <div>暂无工作日志</div>}
      </div>
      <div className="mt-2 flex gap-2"><button type="button" className="btn" onClick={() => navigator.clipboard.writeText(diagnosis)}>复制诊断信息</button>{snapshot.running && <button type="button" className="btn-danger" onClick={onCancel}>取消后台任务</button>}</div>
    </div>}
  </section>;
}

function ChainPanel({ chains }: { chains: GlobalGoldenChain[] }) {
  return <section className="card shrink-0 p-3">
    <div className="mb-2 flex items-center justify-between"><div><b className="text-xs">四条跨境黄金链路</b><span className="muted ml-2 text-[10px]">双侧原文证据齐全后才激活到具体 A 股</span></div></div>
    <div className="grid grid-cols-1 gap-2 xl:grid-cols-4">{chains.map((chain) => <article key={chain.chain_id} className="rounded border border-slate-200 p-2 text-[10px] dark:border-slate-800">
      <div className="flex items-start justify-between gap-2"><b className="text-xs">{chain.name}</b><span className="rounded bg-blue-500/10 px-1.5 py-0.5 text-blue-600">证据门禁</span></div>
      <div className="mt-2 flex flex-wrap items-center gap-1">{chain.nodes.map((node, index) => <span key={node} className="contents"><span className="rounded bg-slate-100 px-1.5 py-1 dark:bg-slate-800">{node}</span>{index < chain.nodes.length - 1 && <span className="text-blue-500">→</span>}</span>)}</div>
      <div className="muted mt-2">官方来源：{chain.global_sources.join("、")}</div><div className="mt-1 text-amber-600 dark:text-amber-300">{chain.activation_requirement}</div>
    </article>)}</div>
  </section>;
}

function ProviderPanel({ providers, onClose }: { providers: GlobalProviderRuntime[]; onClose: () => void }) {
  return <aside className="absolute inset-y-0 right-0 z-30 flex w-[600px] max-w-full flex-col border-l border-slate-200 bg-white shadow-2xl dark:border-slate-800 dark:bg-[#0d1524]">
    <div className="flex items-center justify-between border-b border-slate-200 px-3 py-2 dark:border-slate-800"><div><b>海外官方来源、许可与频率</b><div className="muted text-[10px]">禁用或失败均显示缺口，不使用媒体转载补位</div></div><button type="button" className="btn" onClick={onClose}>关闭</button></div>
    <div className="min-h-0 flex-1 space-y-2 overflow-auto p-3">{providers.map((provider) => <article key={provider.provider_id} className="rounded border border-slate-200 p-3 text-xs dark:border-slate-800">
      <div className="flex items-start justify-between gap-3"><div><b>{provider.provider_name}</b><div className="muted mt-0.5">{provider.region} · {CATEGORY_LABELS[provider.category] ?? provider.category} · 原时区 {provider.original_timezone}</div></div><span className={`rounded px-2 py-0.5 ${provider.enabled && !provider.consecutive_failures ? "bg-emerald-500/10 text-emerald-600" : "bg-amber-500/10 text-amber-600"}`}>{!provider.enabled ? "等待凭据" : provider.consecutive_failures ? `连续失败 ${provider.consecutive_failures} 次` : "就绪"}</span></div>
      <div className="mt-2 grid grid-cols-2 gap-2"><div>目标发现延迟：≤ {durationText(provider.target_latency_secs)}</div><div>访问上限：{provider.rate_limit_per_minute} 次/分钟</div><div>最近成功：{timeText(provider.last_success_at)}</div><div>下次重试：{timeText(provider.retry_after)}</div></div>
      {provider.credential_env && <div className="mt-2 rounded bg-slate-100 p-2 dark:bg-slate-900">Credential Manager 槽：{provider.credential_env}（凭据内容不会显示）</div>}
      <p className="muted mt-2 leading-5">许可策略：{provider.license_policy}</p>{provider.last_error && <div className="mt-2 rounded bg-red-500/10 p-2 text-red-600 dark:text-red-300">{provider.last_error}</div>}
      <button type="button" className="mt-2 text-blue-600 underline" onClick={() => window.open(provider.official_url, "_blank")}>打开官方入口</button>
    </article>)}</div>
  </aside>;
}

function DetailPanel({ item, onClose, onAgent }: { item: GlobalDocumentListItem; onClose: () => void; onAgent: () => void }) {
  return <aside className="flex min-h-0 w-[520px] shrink-0 flex-col border-l border-slate-200 bg-white dark:border-slate-800 dark:bg-[#0d1524]">
    <div className="flex items-center justify-between border-b border-slate-200 px-3 py-2 dark:border-slate-800"><b>海外原文与时间证据</b><button type="button" className="btn" onClick={onClose}>关闭</button></div>
    <div className="min-h-0 flex-1 space-y-3 overflow-auto p-3 text-xs">
      <div className="flex flex-wrap gap-1"><span className={`rounded px-2 py-0.5 ${item.primary_verified ? "bg-emerald-500/10 text-emerald-600" : "bg-amber-500/10 text-amber-600"}`}>{item.primary_verified ? "海外一级原文已归档" : "原文归档存在缺口"}</span><span className="chip">{item.provider_name}</span><span className="chip">{item.document_type}</span></div>
      <div><h2 className="text-base font-semibold leading-6">{item.title_zh || item.title_original}</h2>{item.title_zh && <p className="muted mt-1 leading-5">原文标题：{item.title_original}</p>}</div>
      <div className="grid grid-cols-2 gap-2 rounded bg-slate-50 p-2 dark:bg-slate-900"><div><span className="muted">原始发布时间</span><div>{item.published_local}</div></div><div><span className="muted">原时区</span><div>{item.published_timezone}</div></div><div><span className="muted">转换后北京时间</span><div>{timeText(item.published_at_utc)}</div></div><div><span className="muted">修订版本</span><div>第 {item.revision_no} 版</div></div></div>
      <div className="rounded border border-slate-200 p-2 leading-5 dark:border-slate-800"><b>翻译与数字保护</b><div className="muted mt-1">{item.translation_status}</div><div className="mt-1">原始数字、代码、公司法定名称、单位和币种不会被译文覆盖。</div></div>
      {item.source_version_id ? <div className="rounded bg-emerald-500/10 p-2 text-emerald-700 dark:text-emerald-300">证据版本：{item.source_version_id}</div> : <div className="rounded bg-amber-500/10 p-2 text-amber-700 dark:text-amber-300">证据版本尚未形成，当前记录不得提升 Agent 结论置信度。</div>}
      {item.gap_reason && <div className="rounded bg-amber-500/10 p-2 text-amber-700 dark:text-amber-300">已知缺口：{item.gap_reason}</div>}
      <div className="rounded border border-slate-200 p-2 leading-5 dark:border-slate-800"><b>许可策略</b><p className="muted mt-1">{item.license_policy}</p></div>
      <div className="flex gap-2"><button type="button" className="btn-primary" onClick={onAgent}>交给智能助手做传导核验</button><button type="button" className="btn" onClick={() => window.open(item.original_url, "_blank")}>打开官方原文</button></div>
    </div>
  </aside>;
}

export default function GlobalPage() {
  const navigate = useNavigate();
  const [page, setPage] = useState<GlobalDocumentPage>(EMPTY_PAGE);
  const [providers, setProviders] = useState<GlobalProviderRuntime[]>([]);
  const [chains, setChains] = useState<GlobalGoldenChain[]>([]);
  const [sync, setSync] = useState<GlobalSyncSnapshot | null>(null);
  const [syncExpanded, setSyncExpanded] = useState(false);
  const [showProviders, setShowProviders] = useState(false);
  const [selected, setSelected] = useState<GlobalDocumentListItem | null>(null);
  const [cik, setCik] = useState(""); const [keyword, setKeyword] = useState("");
  const [providerId, setProviderId] = useState("all"); const [primaryOnly, setPrimaryOnly] = useState(false);
  const [pageNo, setPageNo] = useState(1); const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true); setError(null);
    try { setPage(await queryGlobalDocuments({ provider_id: providerId, keyword: keyword || null, primary_only: primaryOnly, page: pageNo, page_size: 50 })); }
    catch (reason) { setError(errMsg(reason)); } finally { setLoading(false); }
  }, [providerId, keyword, primaryOnly, pageNo]);

  useEffect(() => { void load(); }, [load]);
  useEffect(() => { void Promise.all([getGlobalProviderHealth(), getGlobalGoldenChains()]).then(([sourceRows, chainRows]) => { setProviders(sourceRows); setChains(chainRows); }).catch((reason) => setError(errMsg(reason))); }, []);
  useEffect(() => {
    let active = true; let timer: ReturnType<typeof setTimeout>;
    const poll = async () => { try { const value = await globalSyncStatus(); if (!active) return; setSync(value); timer = setTimeout(poll, globalPollDelay(value.running)); } catch { if (active) timer = setTimeout(poll, 3000); } };
    void poll(); return () => { active = false; clearTimeout(timer); };
  }, []);
  useEffect(() => { if (!sync?.running && sync?.status.startsWith("completed")) { void load(); void getGlobalProviderHealth().then(setProviders); } }, [sync?.running, sync?.status, load]);

  const startSync = async () => { setError(null); try { await globalSyncStart({ sec_cik: cik || undefined, include_world_bank: true, max_sec_filings: 20 }); setSyncExpanded(true); setSync(await globalSyncStatus()); } catch (reason) { setError(errMsg(reason)); } };
  const askAgent = (item: GlobalDocumentListItem) => { useAgentSession.getState().setInput(`请核验海外一级来源文档 ${item.document_id}（${item.provider_name}，证据版本 ${item.source_version_id ?? "尚未归档"}），先保留原始时区、数字、单位和币种，再分析它可能通过哪些有证据的客户/供应商/产品/商品路径影响 A 股。逐边给出 source_version_id、原文位置、置信度和失效条件；没有双侧正式证据的路径不得补全。`); navigate("/agent"); };
  const ready = useMemo(() => providers.filter((provider) => provider.enabled).length, [providers]);

  return <div className="relative flex h-full min-w-0 flex-col gap-3 overflow-hidden p-3">
    <div className="flex shrink-0 flex-wrap items-end gap-2"><div className="mr-auto"><h1 className="text-base font-semibold">全球事件 → A 股传导</h1><p className="muted mt-0.5 text-[11px]">海外监管/公司/宏观/商品一级来源 · 原时区与修订 · 双侧关系证据 · 不做海外交易</p></div><label className="text-xs"><span className="muted mb-1 block">按公司同步 SEC CIK（可选）</span><input className="input w-48" value={cik} onChange={(event) => setCik(event.target.value.replace(/\D/g, "").slice(0, 10))} placeholder="如 Apple 0000320193" /></label><button type="button" className="btn" onClick={() => setShowProviders(true)}>来源状态 {ready}/{providers.length || 21}</button><button type="button" className="btn-primary" disabled={sync?.running} onClick={startSync}>{sync?.running ? "后台同步中…" : "同步官方数据"}</button></div>
    {sync && sync.status !== "idle" && <SyncPanel snapshot={sync} expanded={syncExpanded} onToggle={() => setSyncExpanded((value) => !value)} onCancel={async () => { await globalSyncCancel(); setSync(await globalSyncStatus()); }} />}
    <ChainPanel chains={chains} />
    <div className="card flex shrink-0 flex-wrap items-end gap-2 p-3 text-xs"><label><span className="muted mb-1 block">标题关键词</span><input className="input w-60" value={keyword} onChange={(event) => { setKeyword(event.target.value); setPageNo(1); }} placeholder="公司、指标、政策…" /></label><label><span className="muted mb-1 block">官方来源</span><select className="input w-56" value={providerId} onChange={(event) => { setProviderId(event.target.value); setPageNo(1); }}><option value="all">全部官方来源</option>{providers.map((provider) => <option key={provider.provider_id} value={provider.provider_id}>{provider.provider_name}</option>)}</select></label><label className="flex h-8 items-center gap-2 rounded border border-slate-200 px-2 dark:border-slate-700"><input type="checkbox" checked={primaryOnly} onChange={(event) => { setPrimaryOnly(event.target.checked); setPageNo(1); }} />只看已归档一级原文</label><button type="button" className="btn" onClick={() => { setKeyword(""); setProviderId("all"); setPrimaryOnly(false); setPageNo(1); }}>清空筛选</button></div>
    {error && <div className="shrink-0"><ErrorBox message={error} /></div>}
    <div className="card flex min-h-0 flex-1 overflow-hidden"><div className="flex min-w-0 flex-1 flex-col"><div className="muted flex shrink-0 items-center justify-between border-b border-slate-200 px-3 py-2 text-[11px] dark:border-slate-800"><span>{page.total.toLocaleString("zh-CN")} 条海外一级来源记录</span><span>第 {page.page || 1} / {Math.max(1, page.total_pages)} 页 · 每页 {page.page_size} 条</span></div><div className="min-h-0 flex-1 overflow-auto">{loading ? <Loading text="正在读取海外原文、修订与时间证据…" /> : page.items.length ? <table className="w-full text-left text-xs"><thead className="sticky top-0 z-10 bg-slate-100 dark:bg-slate-900"><tr><th className="px-3 py-2">原始时间 / 时区</th><th className="px-3 py-2">文档</th><th className="px-3 py-2">来源 / 类型</th><th className="px-3 py-2">原文核验</th><th className="px-3 py-2">翻译状态</th></tr></thead><tbody>{page.items.map((item) => <tr key={item.document_id} className={`cursor-pointer border-t border-slate-100 hover:bg-blue-50 dark:border-slate-800 dark:hover:bg-blue-950/20 ${selected?.document_id === item.document_id ? "bg-blue-50 dark:bg-blue-950/30" : ""}`} onClick={() => setSelected(item)}><td className="px-3 py-2 align-top"><div>{item.published_local}</div><div className="muted mt-1">{item.published_timezone}</div></td><td className="max-w-xl px-3 py-2 align-top"><b className="leading-5">{item.title_zh || item.title_original}</b>{item.title_zh && <div className="muted mt-1 line-clamp-1">{item.title_original}</div>}{item.gap_reason && <div className="mt-1 line-clamp-1 text-amber-600">{item.gap_reason}</div>}</td><td className="px-3 py-2 align-top"><div>{item.provider_name}</div><div className="muted mt-1">{item.document_type} · 第 {item.revision_no} 版</div></td><td className="px-3 py-2 align-top"><span className={`rounded px-2 py-0.5 ${item.primary_verified ? "bg-emerald-500/10 text-emerald-600" : "bg-amber-500/10 text-amber-600"}`}>{item.primary_verified ? "一级原文已归档" : "原文存在缺口"}</span></td><td className="px-3 py-2 align-top">{item.translation_status}</td></tr>)}</tbody></table> : <div className="muted flex h-full flex-col items-center justify-center gap-2"><b>当前筛选范围没有海外一级来源记录</b><span>可直接同步 World Bank，或输入 SEC CIK 按公司同步；缺口不会被媒体转载填充。</span></div>}</div><div className="flex shrink-0 items-center justify-between border-t border-slate-200 px-3 py-2 text-xs dark:border-slate-800"><span className="muted">原时区、原单位、原币种和修订版本始终保留</span><div className="flex gap-1">{globalPageTokens(pageNo, page.total_pages).map((token, index) => token === "ellipsis" ? <span key={`ellipsis-${index}`} className="px-1">…</span> : <button key={token} type="button" className={`chip ${token === pageNo ? "bg-blue-600 text-white" : ""}`} onClick={() => setPageNo(token)}>{token}</button>)}</div></div></div>{selected && <DetailPanel item={selected} onClose={() => setSelected(null)} onAgent={() => askAgent(selected)} />}</div>
    {showProviders && <ProviderPanel providers={providers} onClose={() => setShowProviders(false)} />}
  </div>;
}
