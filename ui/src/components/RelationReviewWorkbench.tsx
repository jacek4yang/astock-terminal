import { useCallback, useEffect, useMemo, useState } from "react";
import {
  cancelRelationExtraction,
  errMsg,
  getRelationExtractionStatus,
  queryRelationReviews,
  retractRelationCandidate,
  reviewRelationCandidate,
  startRelationExtraction,
  type RelationCandidate,
  type RelationDocumentKind,
  type RelationExtractionSnapshot,
  type RelationReviewPage,
  type SupplyRelationType,
} from "../lib/api";
import { ErrorBox, Loading } from "./ui";

const KINDS: Array<[RelationDocumentKind, string]> = [
  ["annual_report", "年度报告"], ["semi_annual_report", "半年度报告"], ["prospectus", "招股/募集文件"],
  ["investor_relations", "机构调研/业绩说明会"], ["product_manual", "产品手册"], ["tender", "招投标/中标公告"],
  ["major_contract", "重大合同"], ["patent", "专利"], ["regulatory_approval", "监管审批"],
  ["capacity_eia", "产能/环评"], ["customs_industry", "海关/行业统计"], ["other", "其他正式材料"],
];
const RELATIONS: Array<[SupplyRelationType, string]> = [
  ["supplies", "供应"], ["customer_of", "客户"], ["produces", "生产"], ["consumes", "采购/消耗"],
  ["won_bid", "中标"], ["contract_with", "签约"], ["patent_for", "专利涉及"],
  ["approved_for", "获批用于"], ["capacity_for", "产能对应"],
];
const kindName = (value: string) => KINDS.find(([key]) => key === value)?.[1] ?? value;
const relationName = (value: string) => RELATIONS.find(([key]) => key === value)?.[1] ?? value;
const EMPTY: RelationReviewPage = { items: [], total: 0, page: 1, page_size: 20, total_pages: 0 };
const duration = (seconds: number | null) => seconds == null ? "未知" : seconds < 60 ? `约 ${seconds} 秒` : `约 ${Math.ceil(seconds / 60)} 分钟`;

interface Draft { subject: string; object: string; relation: SupplyRelationType; product: string; mergeEntity: string; reason: string }

function JobPanel({ snapshot, onCancel }: { snapshot: RelationExtractionSnapshot; onCancel: () => void }) {
  const [expanded, setExpanded] = useState(true);
  const diagnosis = JSON.stringify(snapshot, null, 2);
  return <section className="card overflow-hidden">
    <button type="button" className="flex w-full items-center gap-3 p-3 text-left" onClick={() => setExpanded((value) => !value)}>
      <span className={`h-2.5 w-2.5 rounded-full ${snapshot.status === "failed" ? "bg-red-500" : snapshot.running ? "animate-pulse bg-blue-500" : "bg-emerald-500"}`} />
      <div className="min-w-0 flex-1"><div className="flex justify-between gap-2 text-xs"><b>{snapshot.phase}</b><span className="num">{snapshot.progress}%</span></div>
        <div className="mt-1.5 h-1.5 overflow-hidden rounded bg-slate-200 dark:bg-slate-800"><div className="h-full bg-blue-500 transition-all" style={{ width: `${snapshot.progress}%` }} /></div>
        <div className="muted mt-1 truncate text-[10px]">{snapshot.current_item}{snapshot.running ? ` · 预计 ${duration(snapshot.estimated_remaining_seconds)}（仅供参考，不设超时）` : ""}</div></div>
      <span className="muted text-xs">{expanded ? "收起" : "展开最详细工作信息"}</span>
    </button>
    {expanded && <div className="border-t border-slate-200 p-3 text-xs dark:border-slate-800">
      <div className="grid grid-cols-2 gap-2 md:grid-cols-4">{[["已扫描证据段", snapshot.segments_scanned], ["候选关系", snapshot.candidates_found], ["规则校验通过", snapshot.validated], ["需要人工处理", snapshot.needs_review]].map(([label, value]) => <div key={String(label)} className="rounded bg-slate-50 p-2 dark:bg-slate-900"><div className="muted text-[10px]">{label}</div><b className="num text-base">{value}</b></div>)}</div>
      {snapshot.error && <div className="mt-2 rounded bg-red-500/10 p-2 text-red-600">{snapshot.error}</div>}
      <div className="mt-2 max-h-36 overflow-auto rounded bg-slate-950 p-2 font-mono text-[10px] leading-5 text-slate-300">{snapshot.recent_logs.map((line, index) => <div key={`${index}-${line}`}>{line}</div>)}</div>
      {snapshot.result && <div className="mt-2 space-y-1 rounded border border-slate-200 p-2 dark:border-slate-800">{snapshot.result.diagnostics.map((line) => <div key={line}>• {line}</div>)}</div>}
      <div className="mt-2 flex gap-2"><button type="button" className="btn" onClick={() => navigator.clipboard.writeText(diagnosis)}>复制完整诊断信息</button>{snapshot.running && <button type="button" className="btn-danger" onClick={onCancel}>取消后台任务</button>}</div>
    </div>}
  </section>;
}

function CandidateCard({ candidate, onChanged }: { candidate: RelationCandidate; onChanged: () => void }) {
  const [busy, setBusy] = useState(false); const [error, setError] = useState<string | null>(null);
  const [draft, setDraft] = useState<Draft>({ subject: candidate.subject_text, object: candidate.object_text, relation: candidate.relation, product: candidate.product_text ?? "", mergeEntity: "", reason: "" });
  const act = async (decision: "accepted" | "modified" | "rejected" | "confidential" | "non_inferable" | "merge_entity", publish: boolean) => {
    if (!draft.reason.trim()) { setError("请填写审核理由，便于回放和问题诊断"); return; }
    setBusy(true); setError(null);
    try { await reviewRelationCandidate({ candidate_id: candidate.candidate_id, decision, reviewer: "本机审核用户", reason: draft.reason,
      subject_text: decision === "modified" ? draft.subject : null, object_text: decision === "modified" ? draft.object : null,
      relation: decision === "modified" ? draft.relation : null, product_text: decision === "modified" ? draft.product || null : null,
      merged_entity_id: decision === "merge_entity" ? draft.mergeEntity || null : null,
      confidential: decision === "confidential", non_inferable: decision === "non_inferable", publish,
      dataset_split: "dev", training_eligible: decision !== "rejected" }); onChanged(); }
    catch (reason) { setError(errMsg(reason)); } finally { setBusy(false); }
  };
  const retract = async () => { if (!draft.reason.trim()) { setError("撤回也必须填写理由"); return; } setBusy(true); try { await retractRelationCandidate(candidate.candidate_id, draft.reason); onChanged(); } catch (reason) { setError(errMsg(reason)); } finally { setBusy(false); } };
  const diagnosis = JSON.stringify(candidate, null, 2);
  return <details className="rounded border border-slate-200 bg-white dark:border-slate-800 dark:bg-slate-950/30">
    <summary className="cursor-pointer list-none p-3"><div className="flex flex-wrap items-center gap-2 text-xs"><span className={`h-2 w-2 rounded-full ${candidate.publication_status === "published" ? "bg-emerald-500" : candidate.validation_status === "validated" ? "bg-blue-500" : "bg-amber-500"}`} /><b>{candidate.subject_text} → {candidate.object_text}</b><span className="chip">{relationName(candidate.relation)}</span><span className="num">置信度 {(candidate.confidence_bps / 100).toFixed(0)}%</span><span className="muted">{candidate.publication_status === "published" ? "已审核并发布" : candidate.review_status === "pending_review" ? "等待人工审核" : candidate.review_status}</span></div></summary>
    <div className="space-y-3 border-t border-slate-200 p-3 text-xs dark:border-slate-800">
      <div className="grid grid-cols-2 gap-2 lg:grid-cols-4"><div><span className="muted">实际主体实体</span><div className="break-all">{candidate.subject_entity_id ?? "尚未唯一映射"}</div></div><div><span className="muted">上市母公司映射</span><div className="break-all">{candidate.subject_parent_entity_id ?? "无"}</div></div><div><span className="muted">实际对象实体</span><div className="break-all">{candidate.object_entity_id ?? "尚未唯一映射"}</div></div><div><span className="muted">对象上市母公司</span><div className="break-all">{candidate.object_parent_entity_id ?? "无"}</div></div></div>
      <div className="flex flex-wrap gap-2">{candidate.product_text && <span className="chip">产品：{candidate.product_text}</span>}{candidate.amount_text && <span className="chip">金额：{candidate.amount_text}</span>}{candidate.share_bps != null && <span className="chip">占比：{(candidate.share_bps / 100).toFixed(2)}%</span>}{candidate.report_period && <span className="chip">报告期：{candidate.report_period}</span>}<span className="chip">披露方式：{candidate.disclosure_mode}</span></div>
      <section><b>确定性校验</b><div className="mt-1 grid gap-1 md:grid-cols-2">{candidate.validation.map((check) => <div key={check.field} className={`rounded p-2 ${check.passed ? "bg-emerald-500/10" : "bg-amber-500/10"}`}><b>{check.passed ? "通过" : "需处理"} · {check.field}</b><div className="muted mt-0.5">{check.detail}</div></div>)}</div></section>
      <section><b>原文证据（{candidate.evidence.length}）</b><div className="mt-1 space-y-1">{candidate.evidence.map((evidence) => <div key={evidence.evidence_id} className="rounded bg-slate-50 p-2 dark:bg-slate-900"><div className="leading-5">“{evidence.quote_original}”</div><div className="num muted mt-1 break-all">来源 {evidence.source_version_id} · {evidence.page_number == null ? `第 ${evidence.paragraph_index + 1} 段` : `第 ${evidence.page_number} 页`} · span {evidence.span_start}–{evidence.span_end} · {evidence.polarity === "supports" ? "支持" : "冲突"}</div></div>)}</div></section>
      <section className="rounded border border-slate-200 p-2 dark:border-slate-800"><b>修改或合并实体</b><div className="mt-2 grid gap-2 md:grid-cols-2"><input className="input" value={draft.subject} onChange={(event) => setDraft({ ...draft, subject: event.target.value })} placeholder="实际主体" /><input className="input" value={draft.object} onChange={(event) => setDraft({ ...draft, object: event.target.value })} placeholder="实际对象" /><select className="input" value={draft.relation} onChange={(event) => setDraft({ ...draft, relation: event.target.value as SupplyRelationType })}>{RELATIONS.map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select><input className="input" value={draft.product} onChange={(event) => setDraft({ ...draft, product: event.target.value })} placeholder="产品（可选）" /><input className="input md:col-span-2" value={draft.mergeEntity} onChange={(event) => setDraft({ ...draft, mergeEntity: event.target.value })} placeholder="需要合并时填写目标实体 ID" /></div></section>
      <textarea className="input min-h-16 w-full" value={draft.reason} onChange={(event) => setDraft({ ...draft, reason: event.target.value })} placeholder="必填：核对了什么、为何通过/修改/拒绝/撤回" />
      {error && <ErrorBox message={error} />}
      <div className="flex flex-wrap gap-2"><button type="button" className="btn-primary" disabled={busy || candidate.publication_status === "published"} onClick={() => act("accepted", true)}>通过并发布</button><button type="button" className="btn" disabled={busy} onClick={() => act("modified", true)}>保存修改并发布</button><button type="button" className="btn" disabled={busy || !draft.mergeEntity} onClick={() => act("merge_entity", true)}>合并实体并发布</button><button type="button" className="btn" disabled={busy} onClick={() => act("confidential", false)}>标记保密</button><button type="button" className="btn" disabled={busy} onClick={() => act("non_inferable", false)}>标记不可推断</button><button type="button" className="btn-danger" disabled={busy} onClick={() => act("rejected", false)}>拒绝</button>{candidate.publication_status === "published" && <button type="button" className="btn-danger" disabled={busy} onClick={retract}>撤回图谱关系</button>}<button type="button" className="btn" onClick={() => navigator.clipboard.writeText(diagnosis)}>复制候选诊断</button></div>
    </div>
  </details>;
}

export default function RelationReviewWorkbench() {
  const [sourceVersion, setSourceVersion] = useState(""); const [kind, setKind] = useState<RelationDocumentKind>("annual_report");
  const [jobId, setJobId] = useState(() => new URLSearchParams(window.location.search).get("relation_job") ?? localStorage.getItem("astock_relation_job") ?? "");
  const [job, setJob] = useState<RelationExtractionSnapshot | null>(null); const [status, setStatus] = useState("all");
  const [filterKind, setFilterKind] = useState<RelationDocumentKind | "all">("all"); const [minimum, setMinimum] = useState(0);
  const [pageNo, setPageNo] = useState(1); const [page, setPage] = useState<RelationReviewPage>(EMPTY); const [loading, setLoading] = useState(true); const [error, setError] = useState<string | null>(null);
  const load = useCallback(async () => { setLoading(true); setError(null); try { setPage(await queryRelationReviews(status, filterKind === "all" ? null : filterKind, minimum, pageNo, 20)); } catch (reason) { setError(errMsg(reason)); } finally { setLoading(false); } }, [status, filterKind, minimum, pageNo]);
  useEffect(() => { void load(); }, [load]);
  useEffect(() => { if (!jobId) return; let active = true; let timer: ReturnType<typeof setTimeout>; const poll = async () => { try { const value = await getRelationExtractionStatus(jobId); if (!active) return; setJob(value); if (!value.running) void load(); timer = setTimeout(poll, value.running ? 700 : 4000); } catch { if (active) timer = setTimeout(poll, 3000); } }; void poll(); return () => { active = false; clearTimeout(timer); }; }, [jobId, load]);
  const start = async () => { if (!sourceVersion.trim()) { setError("请填写正式原文的 source_version_id"); return; } setError(null); try { const result = await startRelationExtraction(sourceVersion.trim(), kind); localStorage.setItem("astock_relation_job", result.job_id); setJobId(result.job_id); setJob(await getRelationExtractionStatus(result.job_id)); } catch (reason) { setError(errMsg(reason)); } };
  const pages = useMemo(() => Array.from({ length: Math.min(7, Math.max(1, page.total_pages)) }, (_, index) => Math.min(Math.max(1, pageNo - 3) + index, Math.max(1, page.total_pages))).filter((value, index, values) => values.indexOf(value) === index), [page.total_pages, pageNo]);
  return <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-auto">
    <section className="card p-3 text-xs"><div className="flex flex-wrap items-end gap-2"><label className="min-w-64 flex-1"><span className="muted mb-1 block">正式原文版本 ID</span><input className="input w-full" value={sourceVersion} onChange={(event) => setSourceVersion(event.target.value)} placeholder="srcver:…（必须已归档且可定位原文）" /></label><label><span className="muted mb-1 block">材料类型</span><select className="input" value={kind} onChange={(event) => setKind(event.target.value as RelationDocumentKind)}>{KINDS.map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select></label><button type="button" className="btn-primary" onClick={start}>后台抽取关系</button></div><p className="muted mt-2">模型只能提交结构化候选；系统重新核对原文 span、证券主数据、子公司层级、单位和报告期。模型升级会新建批次，不会覆盖旧审核结果。</p></section>
    {job && <JobPanel snapshot={job} onCancel={async () => { await cancelRelationExtraction(job.job_id); setJob(await getRelationExtractionStatus(job.job_id)); }} />}
    <section className="card flex min-h-[420px] flex-col overflow-hidden"><div className="flex flex-wrap items-end gap-2 border-b border-slate-200 p-3 text-xs dark:border-slate-800"><div className="mr-auto"><b>人工关系审核队列</b><div className="muted mt-1">{page.total} 条 · 只有通过并发布后才允许进入 Agent 高置信结论</div></div><label><span className="muted mb-1 block">审核状态</span><select className="input" value={status} onChange={(event) => { setStatus(event.target.value); setPageNo(1); }}><option value="all">全部</option><option value="pending_review">等待审核</option><option value="accepted">已通过</option><option value="modified">已修改</option><option value="rejected">已拒绝</option><option value="confidential">保密</option><option value="non_inferable">不可推断</option></select></label><label><span className="muted mb-1 block">材料类型</span><select className="input" value={filterKind} onChange={(event) => { setFilterKind(event.target.value as RelationDocumentKind | "all"); setPageNo(1); }}><option value="all">全部</option>{KINDS.map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select></label><label><span className="muted mb-1 block">最低置信度</span><select className="input" value={minimum} onChange={(event) => { setMinimum(Number(event.target.value)); setPageNo(1); }}><option value={0}>不限</option><option value={7000}>70%</option><option value={8500}>85%</option><option value={9500}>95%</option></select></label><button type="button" className="btn" onClick={load}>刷新</button></div>
      {error && <div className="p-3"><ErrorBox message={error} /></div>}<div className="min-h-0 flex-1 space-y-2 overflow-auto p-3">{loading ? <Loading text="正在读取候选、原文证据和审核历史…" /> : page.items.length ? page.items.map((candidate) => <CandidateCard key={`${candidate.candidate_id}-${candidate.updated_at}`} candidate={candidate} onChanged={load} />) : <div className="muted flex h-full items-center justify-center">当前筛选范围没有关系候选</div>}</div>
      <div className="flex items-center justify-between border-t border-slate-200 p-3 text-xs dark:border-slate-800"><span className="muted">第 {page.page} / {Math.max(1, page.total_pages)} 页 · 每页 {page.page_size} 条</span><div className="flex gap-1">{pages.map((value) => <button key={value} type="button" className={`chip ${value === pageNo ? "bg-blue-600 text-white" : ""}`} onClick={() => setPageNo(value)}>{value}</button>)}</div></div>
    </section>
  </div>;
}

export function relationKindName(value: string) { return kindName(value); }
