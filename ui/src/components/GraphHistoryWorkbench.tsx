import { useCallback, useEffect, useMemo, useState } from "react";
import {
  graphAsOf,
  graphEdgeTimeline,
  graphHistoryBounds,
  graphSnapshotDiff,
  type GraphEdgeRevision,
  type GraphHistoryBounds,
  type GraphSnapshot,
  type GraphSnapshotDiff,
} from "../lib/api";
import { errMsg } from "../lib/api";
import { ErrorBox, LoadBar } from "./ui";

const STATUS: Record<string, string> = {
  candidate: "候选",
  verified: "已核验",
  active: "有效",
  stale: "待复核",
  contradicted: "存在反方证据",
  expired: "业务已到期",
  revoked: "已撤销",
};

const SOURCE: Record<string, string> = {
  annual_report: "年报/半年报",
  prospectus: "招股书",
  investor_research: "投资者调研",
  tender: "招投标",
  contract: "重大合同",
  patent: "专利",
  regulatory_approval: "监管批文",
  capacity_disclosure: "产能披露",
  research: "研究材料",
  manual: "人工操作",
  legacy: "历史迁移",
};

const RELATION: Record<string, string> = {
  supplies: "供应",
  customer_of: "是其客户",
  competes: "竞争",
  substitutes: "替代",
  exposed_to: "风险暴露",
  belongs_to: "属于",
  produces: "生产",
  consumes: "消耗",
};

const dateTime = (value?: number | null) => {
  if (!value) return "未知";
  return new Date(value * 1000).toLocaleString("zh-CN", { hour12: false });
};

const day = (value: number) => new Date(value * 1000).toLocaleDateString("zh-CN");
const percent = (value?: number | null) => value == null ? "未披露" : `${(value * 100).toFixed(2)}%`;

function nodeName(snapshot: GraphSnapshot, id: string) {
  return snapshot.nodes.find((node) => node.id === id)?.name ?? id;
}

export default function GraphHistoryWorkbench() {
  const [bounds, setBounds] = useState<GraphHistoryBounds | null>(null);
  const [businessTime, setBusinessTime] = useState(0);
  const [knowledgeTime, setKnowledgeTime] = useState(0);
  const [query, setQuery] = useState("");
  const [hops, setHops] = useState(2);
  const [snapshot, setSnapshot] = useState<GraphSnapshot | null>(null);
  const [timeline, setTimeline] = useState<GraphEdgeRevision[]>([]);
  const [selectedIdentity, setSelectedIdentity] = useState<string | null>(null);
  const [diff, setDiff] = useState<GraphSnapshotDiff | null>(null);
  const [loading, setLoading] = useState(true);
  const [timelineLoading, setTimelineLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    graphHistoryBounds()
      .then((value) => {
        if (!alive) return;
        setBounds(value);
        setBusinessTime(value.business_max);
        setKnowledgeTime(value.knowledge_max);
        return graphAsOf(value.business_max, value.knowledge_max, undefined, 2);
      })
      .then((value) => {
        if (alive && value) setSnapshot(value);
      })
      .catch((cause) => alive && setError(errMsg(cause)))
      .finally(() => alive && setLoading(false));
    return () => { alive = false; };
  }, []);

  const load = useCallback(async () => {
    if (!businessTime || !knowledgeTime) return;
    setLoading(true);
    setError(null);
    setDiff(null);
    setSelectedIdentity(null);
    setTimeline([]);
    try {
      setSnapshot(await graphAsOf(businessTime, knowledgeTime, query.trim() || undefined, hops));
    } catch (cause) {
      setError(errMsg(cause));
    } finally {
      setLoading(false);
    }
  }, [businessTime, knowledgeTime, query, hops]);

  const compareKnowledge = async () => {
    const initialKnowledge = Math.min(businessTime, knowledgeTime);
    setLoading(true);
    setError(null);
    try {
      setDiff(await graphSnapshotDiff(
        businessTime,
        initialKnowledge,
        businessTime,
        knowledgeTime,
      ));
    } catch (cause) {
      setError(errMsg(cause));
    } finally {
      setLoading(false);
    }
  };

  const openTimeline = async (identityId: string) => {
    setSelectedIdentity(identityId);
    setTimelineLoading(true);
    setError(null);
    try {
      setTimeline(await graphEdgeTimeline(identityId));
    } catch (cause) {
      setError(errMsg(cause));
    } finally {
      setTimelineLoading(false);
    }
  };

  const selected = useMemo(
    () => snapshot?.edges.find((edge) => edge.identity_id === selectedIdentity) ?? null,
    [snapshot, selectedIdentity],
  );

  if (loading && !bounds) {
    return <div className="card p-5"><LoadBar className="w-full" /><div className="muted mt-2 text-xs">正在读取历史图谱边界与修订索引…</div></div>;
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto pr-1">
      {error && <ErrorBox message={error} onRetry={load} />}
      <div className="card shrink-0 p-4">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div>
            <div className="text-sm font-semibold">双时间历史图谱</div>
            <div className="muted mt-1 text-xs leading-5">
              业务时间回答“当时关系是否真实有效”；系统知悉时间回答“当时软件已经知道什么”。两者分开可阻断未来信息泄漏。
            </div>
          </div>
          {bounds && (
            <div className="flex gap-2 text-xs">
              <span className="tag">{bounds.revision_count} 个不可变修订</span>
              <span className={bounds.revalidation_due_count ? "tag bg-amber-500/10 text-amber-600" : "tag"}>
                {bounds.revalidation_due_count} 条待复核
              </span>
            </div>
          )}
        </div>

        {bounds && (
          <div className="mt-4 grid gap-4 xl:grid-cols-2">
            <label className="block text-xs">
              <div className="mb-1 flex justify-between gap-3">
                <span className="font-medium">业务有效时间</span>
                <span className="num muted">{dateTime(businessTime)}</span>
              </div>
              <input
                aria-label="业务有效时间"
                type="range"
                min={bounds.business_min}
                max={bounds.business_max}
                step={86_400}
                value={businessTime}
                onChange={(event) => setBusinessTime(Number(event.target.value))}
                className="w-full accent-blue-600"
              />
              <div className="muted mt-1 flex justify-between"><span>{day(bounds.business_min)}</span><span>{day(bounds.business_max)}</span></div>
            </label>
            <label className="block text-xs">
              <div className="mb-1 flex justify-between gap-3">
                <span className="font-medium">系统知悉时间</span>
                <span className="num muted">{dateTime(knowledgeTime)}</span>
              </div>
              <input
                aria-label="系统知悉时间"
                type="range"
                min={bounds.knowledge_min}
                max={bounds.knowledge_max}
                step={86_400}
                value={knowledgeTime}
                onChange={(event) => setKnowledgeTime(Number(event.target.value))}
                className="w-full accent-blue-600"
              />
              <div className="muted mt-1 flex justify-between"><span>{day(bounds.knowledge_min)}</span><span>{day(bounds.knowledge_max)}</span></div>
            </label>
          </div>
        )}

        <div className="mt-4 flex flex-wrap items-center gap-2 border-t border-slate-200 pt-3 dark:border-slate-800">
          <input
            className="input w-52"
            placeholder="股票代码 / 公司 / 产品（可选）"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => event.key === "Enter" && load()}
          />
          <select className="input w-24" value={hops} onChange={(event) => setHops(Number(event.target.value))}>
            <option value={1}>1 层</option><option value={2}>2 层</option><option value={3}>3 层</option>
          </select>
          <button className="btn-primary" disabled={loading} onClick={load}>{loading ? "重建中…" : "重建历史图谱"}</button>
          <button className="btn" disabled={loading || !snapshot} onClick={compareKnowledge}>比较后来新增认知</button>
        </div>
      </div>

      {loading && <LoadBar className="w-full shrink-0" />}

      {snapshot && (
        <div className="grid min-h-0 gap-3 xl:grid-cols-[minmax(0,1fr)_23rem]">
          <div className="card min-w-0 overflow-hidden">
            <div className="flex flex-wrap items-center justify-between gap-2 border-b border-slate-200 px-4 py-3 dark:border-slate-800">
              <div>
                <div className="text-sm font-semibold">该截面可见关系</div>
                <div className="muted mt-0.5 text-xs">{snapshot.nodes.length} 个节点 · {snapshot.edges.length} 条有效关系 · {snapshot.stale_count} 条待复核 · {snapshot.excluded_count} 条被状态门禁排除</div>
              </div>
              <button
                className="num muted max-w-72 truncate text-xs underline decoration-dotted"
                title={snapshot.snapshot_id}
                onClick={() => navigator.clipboard?.writeText(snapshot.snapshot_id)}
              >
                快照：{snapshot.snapshot_id}
              </button>
            </div>
            {snapshot.edges.length === 0 ? (
              <div className="muted p-8 text-center text-sm">在所选业务时间和知悉时间下，没有可用关系。若知悉时间早于材料入库，这是正确的防穿越结果。</div>
            ) : (
              <div className="divide-y divide-slate-100 dark:divide-slate-800/70">
                {snapshot.edges.map((edge) => (
                  <button
                    key={edge.identity_id}
                    className={`block w-full px-4 py-3 text-left hover:bg-blue-50/50 dark:hover:bg-blue-950/20 ${selectedIdentity === edge.identity_id ? "bg-blue-50 dark:bg-blue-950/30" : ""}`}
                    onClick={() => openTimeline(edge.identity_id)}
                  >
                    <div className="flex flex-wrap items-center gap-2 text-sm">
                      <span className="font-medium">{nodeName(snapshot, edge.src)}</span>
                      <span className="tag">{RELATION[edge.relation] ?? edge.relation}</span>
                      <span className="font-medium">{nodeName(snapshot, edge.dst)}</span>
                      <span className={edge.status === "stale" ? "tag bg-amber-500/10 text-amber-600" : "tag bg-emerald-500/10 text-emerald-600"}>{STATUS[edge.status] ?? edge.status}</span>
                    </div>
                    <div className="muted mt-1.5 flex flex-wrap gap-x-4 gap-y-1 text-xs">
                      <span>产品：{edge.product_scope ?? "未限定"}</span>
                      <span>地区：{edge.region_scope ?? "未限定"}</span>
                      <span>披露占比：{percent(edge.disclosed_share)}</span>
                      <span>衰减后置信度：{percent(edge.effective_confidence)}</span>
                      <span>业务期：{dateTime(edge.valid_from)} → {dateTime(edge.valid_to)}</span>
                    </div>
                  </button>
                ))}
              </div>
            )}
          </div>

          <div className="flex min-h-0 flex-col gap-3">
            {diff && (
              <div className="card p-3 text-xs">
                <div className="font-semibold">知识时间差异</div>
                <div className="mt-2 grid grid-cols-3 gap-2 text-center">
                  <div className="rounded bg-emerald-500/10 p-2"><div className="num text-base">{diff.added_revision_ids.length}</div><div className="muted">后来新增</div></div>
                  <div className="rounded bg-red-500/10 p-2"><div className="num text-base">{diff.removed_revision_ids.length}</div><div className="muted">后来撤销</div></div>
                  <div className="rounded bg-blue-500/10 p-2"><div className="num text-base">{diff.changed_identity_ids.length}</div><div className="muted">关系变化</div></div>
                </div>
                <div className="muted mt-2 leading-5">左侧使用业务时点当时的知识，右侧使用当前选择的知悉时点；回测只能使用左侧或明确指定的历史快照。</div>
              </div>
            )}
            <div className="card min-h-0 overflow-y-auto p-3">
              <div className="flex items-center justify-between"><div className="text-sm font-semibold">关系变更时间线</div>{selected && <span className="tag">身份不变 · 修订 {selected.revision_no}</span>}</div>
              {!selectedIdentity ? (
                <div className="muted py-8 text-center text-xs">点击左侧任一关系，查看每次材料更新、回溯修订、冲突与撤销记录。</div>
              ) : timelineLoading ? <LoadBar className="mt-4 w-full" /> : (
                <div className="mt-3 space-y-3">
                  {timeline.map((revision, index) => (
                    <div key={revision.revision_id} className="relative border-l-2 border-blue-500/30 pl-3 text-xs">
                      <span className="absolute -left-[5px] top-1 h-2 w-2 rounded-full bg-blue-500" />
                      <div className="flex flex-wrap items-center gap-1.5"><span className="font-semibold">第 {revision.revision_no} 次修订</span><span className="tag">{STATUS[revision.status] ?? revision.status}</span><span className="tag">{SOURCE[revision.source_type] ?? revision.source_type}</span></div>
                      <div className="muted mt-1 leading-5">
                        <div>系统记录：{dateTime(revision.recorded_at)}{revision.superseded_at ? ` · 被替代：${dateTime(revision.superseded_at)}` : ""}</div>
                        <div>业务有效：{dateTime(revision.valid_from)} → {dateTime(revision.valid_to)}</div>
                        <div>占比：{percent(revision.disclosed_share)} · 原始置信度：{percent(revision.confidence)}</div>
                        <div>证据版本：<span className="num break-all">{revision.evidence_version}</span></div>
                        <div>来源：{revision.source_name}</div>
                        <div>下次复核：{dateTime(revision.revalidate_after)} · 半衰期 {revision.decay_half_life_days} 天</div>
                      </div>
                      {index === timeline.length - 1 && <div className="mt-1 text-[11px] text-blue-500">最新系统认知</div>}
                    </div>
                  ))}
                </div>
              )}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
