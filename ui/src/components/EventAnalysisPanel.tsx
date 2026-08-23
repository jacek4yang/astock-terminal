import type { EventAnalysisSnapshot } from "../lib/api";

const KIND: Record<string, string> = {
  earnings: "业绩", guidance: "经营指引", order: "订单/中标", price_increase: "涨价",
  shutdown: "停产", capacity: "产能", policy: "政策", sanction: "制裁", tariff: "关税",
  accident: "事故", merger_acquisition: "并购重组", repurchase: "回购", share_reduction: "减持",
  unlock: "解禁", litigation: "诉讼", technology_breakthrough: "技术突破", macro_release: "宏观发布", other: "其他事件",
};
const STATUS: Record<string, string> = { rumor: "传闻", unverified: "待核验", confirmed: "已确认", effective: "已生效", completed: "已完成", cancelled: "已取消", revised: "已修订" };
const PROVENANCE: Record<string, string> = { observed_fact: "已观察事实", company_guidance: "公司指引", market_consensus: "市场一致预期", agent_assumption: "分析假设", scenario: "情景推演" };
const HORIZON: Record<string, string> = { intraday: "日内", days: "数日", quarter: "季度", year: "年度" };
const REVERSIBILITY: Record<string, string> = { reversible: "可逆", conditional: "条件可逆", irreversible: "不可逆", unknown: "未知" };
const DIRECTION: Record<string, string> = { positive: "正向", negative: "负向", neutral: "中性", unknown: "无法判断" };

export function eventBps(value: number | null | undefined): string {
  return typeof value === "number" && Number.isFinite(value)
    ? `${value >= 0 ? "+" : ""}${(value / 100).toFixed(2)}%`
    : "不可量化";
}

function dateText(value: number | null | undefined): string {
  return typeof value === "number" && Number.isFinite(value)
    ? new Date(value * 1000).toLocaleString("zh-CN", { hour12: false })
    : "来源未提供";
}

export function EventAnalysisPanel({ snapshot, error, onRetry, onCancel }: {
  snapshot: EventAnalysisSnapshot | null;
  error: string | null;
  onRetry: () => void;
  onCancel: () => void;
}) {
  if (!snapshot && !error) {
    return <section className="rounded border border-blue-200 bg-blue-50/60 p-2 dark:border-blue-900 dark:bg-blue-950/20"><b>结构化事件与市场定价</b><div className="muted mt-1">正在创建可恢复的后台分析任务…</div></section>;
  }
  if (error && !snapshot) {
    return <section className="rounded border border-red-200 bg-red-50 p-2 text-red-700 dark:border-red-900 dark:bg-red-950/30 dark:text-red-300"><b>事件分析未启动</b><div className="mt-1 break-all">{error}</div><div className="mt-2 flex gap-2"><button type="button" className="btn" onClick={onRetry}>重新分析</button><button type="button" className="btn" onClick={() => void navigator.clipboard.writeText(error)}>复制错误</button></div></section>;
  }
  if (!snapshot) return null;
  const bundle = snapshot.result;
  const event = bundle?.event;
  const assessment = bundle?.assessment;
  const fields = event ? [
    ["金额", event.amount_text], ["数量", event.quantity_text], ["原始单位", event.unit_original],
    ["原始币种", event.currency_original], ["对比基线", event.baseline_period], ["开始时间", event.starts_at ? dateText(event.starts_at) : null],
    ["结束时间", event.ends_at ? dateText(event.ends_at) : null], ["影响地区", event.region],
    ["正式生效", event.official_effective == null ? null : event.official_effective ? "是" : "否"], ["成立条件", event.conditions.join("；") || null],
  ] : [];
  return <section className="rounded border border-blue-200 bg-blue-50/40 p-2 dark:border-blue-900 dark:bg-blue-950/20">
    <div className="flex items-center justify-between gap-2"><h3 className="font-semibold">结构化事件与市场定价</h3><span className="muted num">{snapshot.progress}%</span></div>
    <div className="mt-2 h-1.5 overflow-hidden rounded bg-slate-200 dark:bg-slate-800"><div className={`h-full ${snapshot.status === "failed" ? "bg-red-500" : snapshot.running ? "bg-blue-500" : "bg-emerald-500"}`} style={{ width: `${Math.max(0, Math.min(100, snapshot.progress))}%` }} /></div>
    <div className="mt-1.5 flex flex-wrap gap-x-3 gap-y-1"><b>{snapshot.phase}</b><span>当前：{snapshot.current_item || "整理结果"}</span>{snapshot.running && snapshot.estimated_remaining_seconds != null && <span className="muted">预计约 {snapshot.estimated_remaining_seconds} 秒，仅供参考、不设超时</span>}</div>
    {(snapshot.running || snapshot.error) && <div className="mt-2 flex gap-2">{snapshot.running && <button type="button" className="btn" onClick={onCancel}>安全停止</button>}{snapshot.error && <button type="button" className="btn" onClick={() => void navigator.clipboard.writeText(snapshot.error!)}>复制错误</button>}<button type="button" className="btn" onClick={onRetry}>重新读取/分析</button></div>}
    {(snapshot.error || error) && <div className="mt-2 rounded bg-red-500/10 p-2 text-red-700 dark:text-red-300">{snapshot.error || error}</div>}
    <details className="mt-2 rounded border border-slate-200 p-2 dark:border-slate-800" open={snapshot.running}>
      <summary className="cursor-pointer font-medium">详细工作记录（{snapshot.recent_logs.length} 条）</summary>
      <ol className="mt-2 space-y-1 text-[11px]">{snapshot.recent_logs.map((line, index) => <li key={`${index}-${line}`} className="break-all"><span className="muted num mr-1">{index + 1}.</span>{line}</li>)}</ol>
      <button type="button" className="btn mt-2" onClick={() => void navigator.clipboard.writeText(JSON.stringify(snapshot, null, 2))}>复制任务诊断</button>
    </details>
    {event && <div className="mt-3 space-y-3">
      <div className="grid grid-cols-2 gap-2 rounded bg-white/70 p-2 dark:bg-slate-950/40">
        <div><span className="muted">事件类型</span><div>{KIND[event.kind] ?? event.kind}</div></div><div><span className="muted">当前状态</span><div>{STATUS[event.lifecycle] ?? event.lifecycle}</div></div>
        <div><span className="muted">影响期限</span><div>{HORIZON[event.impact_horizon] ?? event.impact_horizon}</div></div><div><span className="muted">可逆性</span><div>{REVERSIBILITY[event.reversibility] ?? event.reversibility}</div></div>
      </div>
      <div><b>影响主体与对象</b><div className="mt-1 flex flex-wrap gap-1">{[...event.subjects, ...event.objects].length ? [...event.subjects, ...event.objects].map((entity) => <span key={`${entity.role}-${entity.entity_id}`} className="rounded bg-blue-500/10 px-1.5 py-0.5">{entity.name}{entity.listed_code ? ` ${entity.listed_code}` : ""} · {entity.role === "subject" ? "主体" : "对象"}</span>) : <span className="muted">尚无经过核验的实体映射</span>}</div></div>
      <div><b>逐字段事实</b><div className="mt-1 grid grid-cols-2 gap-px overflow-hidden rounded border border-slate-200 bg-slate-200 dark:border-slate-800 dark:bg-slate-800">{fields.map(([label, value]) => <div key={label} className="bg-white p-2 dark:bg-[#0d1524]"><span className="muted">{label}</span><div className={!value ? "text-amber-600" : ""}>{value || "来源未提供"}</div></div>)}</div></div>
      <div><b>催化路径与验证时间</b><ol className="mt-1 space-y-1">{event.catalyst_path.map((step, index) => <li key={`${index}-${step}`}>{index + 1}. {step}</li>)}</ol><div className="muted mt-1">验证日期：{event.validation_dates.length ? event.validation_dates.map(dateText).join("；") : "尚无确定日期"}</div></div>
      <div><b>事件状态时间线</b>{bundle.timeline.length ? <ol className="mt-1 space-y-1">{bundle.timeline.map((row) => <li key={row.transition_id}>{dateText(row.transitioned_at)} · {STATUS[row.from_status] ?? row.from_status} → {STATUS[row.to_status] ?? row.to_status} · {row.reason}</li>)}</ol> : <div className="muted mt-1">当前只有初始状态，后续确认、生效、完成、取消或修订都会保留迁移记录。</div>}</div>
      {assessment ? <div className="grid grid-cols-2 gap-2">
        <div className="rounded border border-emerald-200 bg-emerald-50 p-2 dark:border-emerald-900 dark:bg-emerald-950/30"><b>基本面影响</b><div className="mt-1 text-sm font-semibold">{DIRECTION[assessment.fundamental.direction] ?? assessment.fundamental.direction} · {eventBps(assessment.fundamental.impact_bps)}</div><div className="mt-1 leading-5">{assessment.fundamental.rationale}</div><div className="muted mt-1">依据类型：{PROVENANCE[assessment.fundamental.provenance] ?? assessment.fundamental.provenance}</div></div>
        <div className="rounded border border-amber-200 bg-amber-50 p-2 dark:border-amber-900 dark:bg-amber-950/30"><b>市场机会（独立判断）</b><div className="mt-1 text-sm font-semibold">{assessment.market_opportunity.opportunity}{assessment.market_opportunity.price_in_score != null ? ` · 已交易评分 ${assessment.market_opportunity.price_in_score}/100` : ""}</div><div className="mt-1 leading-5">{assessment.market_opportunity.rationale}</div><div className="muted mt-1">{assessment.market_opportunity.no_trade_directive}</div></div>
      </div> : <div className="rounded border border-amber-200 bg-amber-50 p-2 dark:border-amber-900 dark:bg-amber-950/30">当前事件没有经过核验的上市公司代码，因此只建立事实与证据，暂不计算市场定价。</div>}
      {assessment && <><div className="rounded border border-slate-200 p-2 dark:border-slate-800"><b>预期差</b><div className="mt-1 text-sm font-semibold">{assessment.expectation_gap.quantifiable ? eventBps(assessment.expectation_gap.gap_bps) : "不可量化"}</div><div className="muted mt-1">{assessment.expectation_gap.rationale}</div></div>
        <details className="rounded border border-slate-200 p-2 dark:border-slate-800" open><summary className="cursor-pointer font-semibold">市场定价逐项依据</summary><div className="mt-2 space-y-1.5">{assessment.diagnostics.components.map((row) => <div key={row.metric} className="rounded bg-white/70 p-2 dark:bg-slate-950/40"><div className="flex justify-between gap-2"><span>{row.metric}</span><b className={row.available ? "" : "text-amber-600"}>{row.available ? eventBps(row.value_bps) : "数据缺失"}</b></div><div className="muted mt-1">{row.explanation}{row.available ? ` · 评分贡献 ${row.score_contribution >= 0 ? "+" : ""}${row.score_contribution}` : ""}</div></div>)}</div></details></>}
      {(event.missing_fields.length > 0 || assessment?.missing_inputs.length) && <div className="rounded border border-amber-200 bg-amber-50 p-2 dark:border-amber-900 dark:bg-amber-950/30"><b>当前缺口</b><div className="mt-1">{[...event.missing_fields, ...(assessment?.missing_inputs ?? [])].join("；")}</div><div className="muted mt-1">缺失项保持为空，不会由模型猜测补齐。</div></div>}
      <div><b>失效条件</b><ul className="mt-1 list-disc space-y-1 pl-5">{event.invalidation_conditions.map((item) => <li key={item}>{item}</li>)}</ul></div>
      <details className="rounded border border-slate-200 p-2 dark:border-slate-800"><summary className="cursor-pointer font-semibold">字段证据与来源（{event.evidence.length}）</summary><div className="mt-2 space-y-2">{event.evidence.map((evidence) => <div key={evidence.evidence_id} className="rounded bg-white/70 p-2 dark:bg-slate-950/40"><div>{evidence.field_name} · {PROVENANCE[evidence.provenance] ?? evidence.provenance} · 置信度 {(evidence.confidence_bps / 100).toFixed(0)}%</div><div className="mt-1 leading-5">{evidence.quote_zh ?? evidence.quote_original ?? "该项为明确标记的分析假设/情景，不是来源事实"}</div><div className="num muted mt-1 break-all">{evidence.source_revision_id ?? "无来源修订（非事实字段）"}</div></div>)}</div></details>
      <div className="muted">历史同类校准：{bundle.calibration.sample_count} 个样本{bundle.calibration.median_post_abnormal_return_bps != null ? ` · 事后异常收益中位数 ${eventBps(bundle.calibration.median_post_abnormal_return_bps)}` : " · 样本不足，暂不量化"}</div>
    </div>}
  </section>;
}
