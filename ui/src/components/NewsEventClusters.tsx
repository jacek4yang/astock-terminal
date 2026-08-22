import { useCallback, useEffect, useState } from "react";
import {
  errMsg,
  getEntityLinkReviews,
  getNewsEventClusterDetail,
  getNewsEventClusters,
  getNewsEntityLinks,
  getPendingNewsEvidenceReviews,
  mergeNewsEventClusters,
  resolveEntityLinkReview,
  resolveNewsEvidenceReview,
  splitNewsEventRevision,
  type AgentConclusionReview,
  type DocumentRelationship,
  type DocumentEntityLink,
  type EntityKind,
  type EntityLinkReview,
  type NewsEventCluster,
  type NewsEventClusterDetail,
} from "../lib/api";

const RELATIONSHIP_LABEL: Record<DocumentRelationship, string> = {
  first_publication: "首发",
  reprint: "转载",
  summary: "摘要",
  follow_up: "跟进",
  commentary: "评论/解读",
  correction: "更正",
  retraction: "撤回",
  duplicate_fetch: "重复抓取",
};

const ENTITY_KIND_LABEL: Record<EntityKind, string> = {
  legal_entity: "法人主体",
  listed_security: "上市证券",
  subsidiary: "子公司",
  brand: "品牌",
  person: "人物",
  product: "产品",
  industry: "行业",
  commodity: "商品",
  region: "地区",
  policy: "政策",
};

function dateTime(seconds: number | null): string {
  if (seconds == null) return "时间未知";
  return new Date(seconds * 1000).toLocaleString("zh-CN", { hour12: false });
}

export default function NewsEventClusters() {
  const [clusters, setClusters] = useState<NewsEventCluster[]>([]);
  const [details, setDetails] = useState<Record<string, NewsEventClusterDetail>>({});
  const [reviews, setReviews] = useState<AgentConclusionReview[]>([]);
  const [entityReviews, setEntityReviews] = useState<EntityLinkReview[]>([]);
  const [entityLinks, setEntityLinks] = useState<Record<string, DocumentEntityLink[]>>({});
  const [reviewEntities, setReviewEntities] = useState<Record<string, string>>({});
  const [reviewReasons, setReviewReasons] = useState<Record<string, string>>({});
  const [targets, setTargets] = useState<Record<string, string>>({});
  const [reasons, setReasons] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [confirmAction, setConfirmAction] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const [clusterRows, reviewRows, entityReviewRows] = await Promise.all([
        getNewsEventClusters(50),
        getPendingNewsEvidenceReviews(50),
        getEntityLinkReviews(50),
      ]);
      setClusters(clusterRows);
      setReviews(reviewRows);
      setEntityReviews(entityReviewRows);
      setError(null);
    } catch (loadError) {
      setError(errMsg(loadError));
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const loadDetail = async (clusterId: string) => {
    if (details[clusterId]) return;
    setBusy(`detail:${clusterId}`);
    try {
      const detail = await getNewsEventClusterDetail(clusterId);
      const links = await getNewsEntityLinks(detail.members.map((member) => member.revision_id));
      setDetails((current) => ({ ...current, [clusterId]: detail }));
      setEntityLinks((current) => {
        const next = { ...current };
        for (const revision of detail.members) {
          next[revision.revision_id] = links.filter((link) => link.revision_id === revision.revision_id);
        }
        return next;
      });
    } catch (loadError) {
      setError(errMsg(loadError));
    } finally {
      setBusy(null);
    }
  };

  const merge = async (from: string) => {
    const target = targets[from];
    const reason = reasons[from]?.trim();
    if (!target || !reason) return;
    const key = `merge:${from}:${target}`;
    if (confirmAction !== key) {
      setConfirmAction(key);
      return;
    }
    setBusy(key);
    try {
      await mergeNewsEventClusters(from, target, reason);
      setDetails({});
      setConfirmAction(null);
      await load();
    } catch (mergeError) {
      setError(errMsg(mergeError));
    } finally {
      setBusy(null);
    }
  };

  const split = async (clusterId: string, revisionId: string) => {
    const reason = reasons[clusterId]?.trim();
    if (!reason) return;
    const key = `split:${revisionId}`;
    if (confirmAction !== key) {
      setConfirmAction(key);
      return;
    }
    setBusy(key);
    try {
      await splitNewsEventRevision(revisionId, reason);
      setDetails({});
      setConfirmAction(null);
      await load();
    } catch (splitError) {
      setError(errMsg(splitError));
    } finally {
      setBusy(null);
    }
  };

  const resolveReview = async (review: AgentConclusionReview) => {
    const key = `review:${review.task_id}:${review.triggering_revision}`;
    if (confirmAction !== key) {
      setConfirmAction(key);
      return;
    }
    setBusy(key);
    try {
      await resolveNewsEvidenceReview(
        review.task_id,
        review.conclusion_key,
        review.triggering_revision,
      );
      setConfirmAction(null);
      await load();
    } catch (reviewError) {
      setError(errMsg(reviewError));
    } finally {
      setBusy(null);
    }
  };

  const resolveEntityReview = async (review: EntityLinkReview, accept: boolean) => {
    const entityId = reviewEntities[review.link.link_id] ?? review.proposed_entity_id;
    const reason = reviewReasons[review.link.link_id]?.trim();
    if (!reason || (accept && !entityId)) return;
    const key = `entity-review:${review.link.link_id}:${accept ? "accept" : "reject"}`;
    if (confirmAction !== key) {
      setConfirmAction(key);
      return;
    }
    setBusy(key);
    try {
      await resolveEntityLinkReview(review.link.link_id, accept ? entityId : null, accept, reason);
      setConfirmAction(null);
      await load();
    } catch (reviewError) {
      setError(errMsg(reviewError));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="card">
      <div className="card-title flex items-center justify-between gap-2">
        <span>财经事件归并与更正追踪</span>
        <button className="btn !px-2 !py-0.5 text-xs" onClick={load}>刷新</button>
      </div>
      <div className="space-y-3 p-4 text-xs">
        <div className="muted leading-relaxed">
          相同事件的转载只计一次，但保留独立来源数量。点击事件可查看合并原因、相似度、修订和冲突；人工调整会追加审计记录，不会覆盖历史。
        </div>
        {error && <div className="rounded bg-red-50 px-2 py-1.5 text-up dark:bg-red-950/30">{error}</div>}

        {reviews.length > 0 && (
          <div className="space-y-2 rounded-lg border border-amber-300 bg-amber-50 p-3 dark:border-amber-800 dark:bg-amber-950/30">
            <div className="font-medium text-amber-700 dark:text-amber-300">有 {reviews.length} 条 Agent 结论因更正或撤回需要复核</div>
            {reviews.map((review) => {
              const key = `review:${review.task_id}:${review.triggering_revision}`;
              return (
                <div key={key} className="flex flex-wrap items-center gap-2 rounded border border-amber-200 p-2 dark:border-amber-900">
                  <span>任务 <span className="num">{review.task_id}</span></span>
                  <span>触发关系：{review.trigger_relation === "correction" ? "更正" : "撤回"}</span>
                  <span className="num break-all">修订 {review.triggering_revision}</span>
                  <button className="btn ml-auto" disabled={busy === key} onClick={() => resolveReview(review)}>
                    {busy === key ? "保存中…" : confirmAction === key ? "再次确认：已人工复核" : "标记为已人工复核"}
                  </button>
                </div>
              );
            })}
          </div>
        )}

        {entityReviews.length > 0 && (
          <div className="space-y-2 rounded-lg border border-amber-300 bg-amber-50 p-3 dark:border-amber-800 dark:bg-amber-950/30">
            <div className="font-medium text-amber-700 dark:text-amber-300">
              有 {entityReviews.length} 个低置信实体关联等待人工审核；审核前不会进入 Agent 投资结论
            </div>
            {entityReviews.map((review) => {
              const acceptKey = `entity-review:${review.link.link_id}:accept`;
              const rejectKey = `entity-review:${review.link.link_id}:reject`;
              return (
                <details key={review.link.link_id} className="rounded border border-amber-200 p-2 dark:border-amber-900">
                  <summary className="cursor-pointer">
                    原文“{review.link.span_text}”存在歧义 · 置信度 {(review.link.confidence * 100).toFixed(1)}% · 展开选择
                  </summary>
                  <div className="mt-2 space-y-2">
                    <select
                      className="input w-full"
                      value={reviewEntities[review.link.link_id] ?? review.proposed_entity_id ?? ""}
                      onChange={(event) => setReviewEntities((current) => ({ ...current, [review.link.link_id]: event.target.value }))}
                    >
                      <option value="">选择正确主体</option>
                      {review.link.candidates.map((candidate) => (
                        <option key={candidate.entity_id} value={candidate.entity_id}>
                          {candidate.canonical_name}（{ENTITY_KIND_LABEL[candidate.entity_kind]}，候选分 {(candidate.score * 100).toFixed(1)}%）
                        </option>
                      ))}
                    </select>
                    <input
                      className="input w-full"
                      placeholder="填写审核依据；将永久写入审计记录"
                      value={reviewReasons[review.link.link_id] ?? ""}
                      onChange={(event) => setReviewReasons((current) => ({ ...current, [review.link.link_id]: event.target.value }))}
                    />
                    <div className="flex gap-2">
                      <button className="btn" disabled={!reviewReasons[review.link.link_id]?.trim()} onClick={() => resolveEntityReview(review, true)}>
                        {busy === acceptKey ? "保存中…" : confirmAction === acceptKey ? "再次确认采用该主体" : "采用所选主体"}
                      </button>
                      <button className="btn-danger" disabled={!reviewReasons[review.link.link_id]?.trim()} onClick={() => resolveEntityReview(review, false)}>
                        {busy === rejectKey ? "保存中…" : confirmAction === rejectKey ? "再次确认拒绝关联" : "拒绝全部候选"}
                      </button>
                    </div>
                  </div>
                </details>
              );
            })}
          </div>
        )}

        {clusters.length === 0 ? (
          <div className="muted">尚无已归档事件；运行财经资讯研究后会自动建立。</div>
        ) : clusters.map((cluster) => {
          const detail = details[cluster.cluster_id];
          const activeTargets = clusters.filter((candidate) =>
            candidate.cluster_id !== cluster.cluster_id && candidate.status === "active"
          );
          return (
            <details
              key={cluster.cluster_id}
              className="rounded-lg border border-slate-200 dark:border-slate-800"
              onToggle={(event) => event.currentTarget.open && loadDetail(cluster.cluster_id)}
            >
              <summary className="flex cursor-pointer list-none flex-wrap items-center gap-x-2 gap-y-1 p-2.5">
                <span className="font-medium">{cluster.canonical_title}</span>
                <span className="rounded bg-slate-100 px-1.5 py-0.5 dark:bg-slate-800">{cluster.independent_sources} 个独立来源</span>
                <span className="num muted">证据多样性 {(cluster.evidence_diversity * 100).toFixed(0)}%</span>
                {cluster.conflict_fields.length > 0 && <span className="text-up">{cluster.conflict_fields.length} 项事实冲突</span>}
                <span className="muted ml-auto">{cluster.status === "merged" ? "已并入其他事件" : "展开详情"}</span>
              </summary>
              <div className="space-y-3 border-t border-slate-200 p-3 dark:border-slate-800">
                <div className="grid gap-2 sm:grid-cols-2">
                  <div><span className="muted">事件编号：</span><span className="num break-all">{cluster.cluster_id}</span></div>
                  <div><span className="muted">聚类版本：</span><span className="num">{cluster.model_version}</span></div>
                  <div><span className="muted">事件时间：</span><span className="num">{dateTime(cluster.event_time_utc)}</span></div>
                  <div><span className="muted">首次发现：</span><span className="num">{dateTime(cluster.first_seen_time_utc)}</span></div>
                </div>
                {!detail ? (
                  <div className="muted">{busy === `detail:${cluster.cluster_id}` ? "读取事件证据中…" : "展开后读取详情"}</div>
                ) : (
                  <>
                    {detail.conflicts.length > 0 && (
                      <div className="space-y-1 rounded bg-red-50 p-2 text-up dark:bg-red-950/30">
                        <div className="font-medium">字段级冲突（转载数量不会覆盖一手来源）</div>
                        {detail.conflicts.map((conflict) => (
                          <div key={conflict.field_name}>
                            {conflict.field_name}：<span className="num">{conflict.values.join(" / ")}</span>
                            {conflict.authoritative_revision_id && <span className="muted">；优先核对 {conflict.authoritative_revision_id}</span>}
                          </div>
                        ))}
                      </div>
                    )}
                    <div className="space-y-2">
                      {detail.members.map((member) => {
                        const revision = detail.revisions.find((row) => row.revision_id === member.revision_id);
                        const linkedEntities = entityLinks[member.revision_id] ?? [];
                        const splitKey = `split:${member.revision_id}`;
                        return (
                          <div key={member.revision_id} className="rounded border border-slate-200 p-2.5 dark:border-slate-800">
                            <div className="flex flex-wrap items-center gap-2">
                              <span className="font-medium">{revision?.title ?? member.revision_id}</span>
                              <span className="rounded bg-slate-100 px-1.5 py-0.5 dark:bg-slate-800">{RELATIONSHIP_LABEL[member.relationship]}</span>
                              <span className="num muted">合并分 {(member.merge_score * 100).toFixed(1)}</span>
                              <span className="muted">{revision?.source_name}</span>
                              {member.old_republication && <span className="text-amber-600 dark:text-amber-300">旧闻重发</span>}
                            </div>
                            <div className="muted mt-1">{member.explanation.reasons.join("；") || "由历史规则归并"}</div>
                            {member.explanation.separation_reasons.length > 0 && (
                              <div className="muted mt-1">保留分离的反向信号：{member.explanation.separation_reasons.join("；")}</div>
                            )}
                            <div className="mt-1 flex flex-wrap gap-x-3 text-[11px]">
                              <span>语义 {(member.explanation.features.semantic_similarity * 100).toFixed(0)}%</span>
                              <span>主体 {(member.explanation.features.entity_overlap * 100).toFixed(0)}%</span>
                              <span>动作 {(member.explanation.features.action_overlap * 100).toFixed(0)}%</span>
                              <span>时间 {(member.explanation.features.time_proximity * 100).toFixed(0)}%</span>
                              <span className="num break-all">{member.revision_id}</span>
                            </div>
                            {linkedEntities.length > 0 && (
                              <details className="mt-2 rounded bg-slate-50 p-2 dark:bg-slate-900/70">
                                <summary className="cursor-pointer font-medium">
                                  为什么关联到这些股票/实体（{linkedEntities.length}）
                                </summary>
                                <div className="mt-2 space-y-2">
                                  {linkedEntities.map((link) => (
                                    <div key={link.link_id} className="rounded border border-slate-200 p-2 dark:border-slate-800">
                                      <div className="flex flex-wrap gap-2">
                                        <span>原文：<mark className="rounded bg-amber-100 px-1 dark:bg-amber-900/50">{link.span_text}</mark></span>
                                        <span>→ {link.final_entity_name ?? "尚未确定"}</span>
                                        {link.final_entity_kind && <span className="muted">{ENTITY_KIND_LABEL[link.final_entity_kind]}</span>}
                                        {link.listed_code && <span className="num">{link.listed_code}</span>}
                                        <span className="num">置信度 {(link.confidence * 100).toFixed(1)}%</span>
                                        <span className={link.eligible_for_agent ? "text-emerald-600" : "text-amber-600"}>
                                          {link.eligible_for_agent ? "可供 Agent 使用" : "等待审核，Agent 已隔离"}
                                        </span>
                                      </div>
                                      <div className="muted mt-1">{link.reasons.join("；")}</div>
                                      {link.candidates.map((candidate) => (
                                        <div key={candidate.entity_id} className="mt-1 text-[11px]">
                                          候选：{candidate.canonical_name} · {(candidate.score * 100).toFixed(1)}%
                                          {candidate.related_listed.map((related) => (
                                            <span key={related.entity_id} className="ml-2">
                                              关联上市主体 {related.name} {related.code}（{related.relation_path.join(" → ")}，{(related.confidence * 100).toFixed(0)}%）
                                            </span>
                                          ))}
                                        </div>
                                      ))}
                                      <div className="num muted mt-1 break-all">证据修订：{link.evidence_revision_id} · 规则：{link.linker_version}</div>
                                    </div>
                                  ))}
                                </div>
                              </details>
                            )}
                            {detail.members.length > 1 && cluster.status === "active" && (
                              <button
                                className="btn mt-2"
                                disabled={!reasons[cluster.cluster_id]?.trim() || busy === splitKey}
                                onClick={() => split(cluster.cluster_id, member.revision_id)}
                              >
                                {busy === splitKey ? "拆分中…" : confirmAction === splitKey ? "再次确认拆为独立事件" : "把此文档拆为独立事件"}
                              </button>
                            )}
                          </div>
                        );
                      })}
                    </div>
                  </>
                )}

                {cluster.status === "active" && (
                  <div className="space-y-2 rounded bg-slate-50 p-2.5 dark:bg-slate-900/70">
                    <div className="font-medium">人工校正（需要填写理由并二次确认）</div>
                    <input
                      className="input w-full"
                      placeholder="说明为什么需要合并或拆分，理由会永久进入审计记录"
                      value={reasons[cluster.cluster_id] ?? ""}
                      onChange={(event) => setReasons((current) => ({ ...current, [cluster.cluster_id]: event.target.value }))}
                    />
                    <div className="flex flex-wrap items-center gap-2">
                      <select
                        className="input min-w-64 flex-1"
                        value={targets[cluster.cluster_id] ?? ""}
                        onChange={(event) => setTargets((current) => ({ ...current, [cluster.cluster_id]: event.target.value }))}
                      >
                        <option value="">选择要并入的目标事件</option>
                        {activeTargets.map((target) => <option key={target.cluster_id} value={target.cluster_id}>{target.canonical_title}</option>)}
                      </select>
                      <button
                        className="btn-danger"
                        disabled={!targets[cluster.cluster_id] || !reasons[cluster.cluster_id]?.trim() || busy?.startsWith("merge:")}
                        onClick={() => merge(cluster.cluster_id)}
                      >
                        {confirmAction?.startsWith(`merge:${cluster.cluster_id}:`) ? "再次确认合并" : "合并事件"}
                      </button>
                    </div>
                  </div>
                )}
              </div>
            </details>
          );
        })}
      </div>
    </div>
  );
}
