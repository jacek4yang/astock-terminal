import { useCallback, useEffect, useState } from "react";
import {
  errMsg,
  getNewsEventClusterDetail,
  getNewsEventClusters,
  getPendingNewsEvidenceReviews,
  mergeNewsEventClusters,
  resolveNewsEvidenceReview,
  splitNewsEventRevision,
  type AgentConclusionReview,
  type DocumentRelationship,
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

function dateTime(seconds: number | null): string {
  if (seconds == null) return "时间未知";
  return new Date(seconds * 1000).toLocaleString("zh-CN", { hour12: false });
}

export default function NewsEventClusters() {
  const [clusters, setClusters] = useState<NewsEventCluster[]>([]);
  const [details, setDetails] = useState<Record<string, NewsEventClusterDetail>>({});
  const [reviews, setReviews] = useState<AgentConclusionReview[]>([]);
  const [targets, setTargets] = useState<Record<string, string>>({});
  const [reasons, setReasons] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [confirmAction, setConfirmAction] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const [clusterRows, reviewRows] = await Promise.all([
        getNewsEventClusters(50),
        getPendingNewsEvidenceReviews(50),
      ]);
      setClusters(clusterRows);
      setReviews(reviewRows);
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
      setDetails((current) => ({ ...current, [clusterId]: detail }));
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
