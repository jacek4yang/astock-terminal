import { useCallback, useEffect, useRef, useState } from "react";
import { Link } from "react-router-dom";
import {
  agentAsk,
  agentCancel,
  agentConversationDelete,
  agentConversationLoad,
  agentConversations,
  agentResume,
  agentTasks,
  errKind,
  errMsg,
  isTauri,
  minimaxStatus,
  NOT_TAURI_MSG,
  type AgentConversation,
  type AgentMessage,
  type AgentTask,
} from "../lib/api";
import Markdown from "./Markdown";
import { Term } from "./ui";
import { useAppStore } from "../store";
import {
  fetchedAtDisplay,
  sourceDisplayName,
  toolArgumentsDisplay,
  toolDisplayName,
} from "../lib/agentLabels";
import {
  appendAgentTurn,
  DEFAULT_AGENT_TOOLS,
  handleAgentEnvelope,
  nextAgentKey,
  patchClarificationDraft,
  patchLastAssistant,
  resetAgentSession,
  selectAgentTask,
  setAgentRunIdentity,
  useAgentSession,
  type AgentProgress,
  type ChatMsg,
  type RunStatus,
  type ToolCallItem,
} from "../agentSession";
import {
  clarificationIsComplete,
  emptyClarificationDraft,
  formatClarificationAnswer,
  hasClarification,
  parseClarification,
  type ClarificationDraft,
  type ClarificationRequest,
} from "../lib/agentClarification";

export type { ChatMsg } from "../agentSession";

const RESEARCH_MODES = [
  { id: "quick" as const, label: "快速", detail: "只取必要证据，适合行情快问" },
  { id: "deep" as const, label: "深度", detail: "多源验证、反方证据与情景分析" },
  { id: "plan" as const, label: "计划", detail: "先分批澄清需求，再列计划并执行" },
];

const REASONING_DEPTHS = [
  { id: "standard" as const, label: "标准" },
  { id: "deep" as const, label: "深入" },
  { id: "maximum" as const, label: "极深" },
];

const TOOL_GROUPS = [
  {
    label: "行情与技术",
    tools: [
      "get_quote",
      "get_kline",
      "compute_indicators",
      "run_full_analysis",
      "run_chanlun",
      "get_fund_flow",
      "get_market_breadth",
      "get_market_regime",
    ],
  },
  {
    label: "基本面与估值",
    tools: ["get_fundamentals", "run_valuation"],
  },
  {
    label: "扫描与横向比较",
    tools: ["search_stock", "compare_stocks", "scan_market", "get_watchlist", "get_cached_detail"],
  },
  {
    label: "产业链与关系",
    tools: ["get_industry_chain", "run_supply_chain_shock", "build_relationship_graph"],
  },
  {
    label: "策略实验",
    tools: ["run_backtest", "iterate_strategy"],
  },
  {
    label: "外部研究数据",
    tools: ["research_news", "search_web", "run_joinquant_research"],
  },
] as const;

// ==================== 数据模型 ====================

function fmtUnix(sec: number): string {
  return new Date(sec * 1000).toLocaleString("zh-CN", { hour12: false });
}

/** Reconcile the persisted UI state with the durable task record after reload. */
export function taskRunStatus(status: string): RunStatus {
  switch (status) {
    case "queued":
    case "starting":
    case "running":
      return "running";
    case "waiting":
    case "suspended":
    case "interrupted":
      return "suspended";
    case "completed":
    case "failed":
    case "cancelled":
      return status;
    default:
      return "idle";
  }
}

/** Strip provider-private reasoning blocks, including an unfinished stream. */
export function stripPrivateReasoning(raw: string): string {
  let text = "";
  let rest = raw;
  for (;;) {
    const i = rest.indexOf("<think>");
    if (i < 0) {
      text += rest;
      break;
    }
    text += rest.slice(0, i);
    rest = rest.slice(i + "<think>".length);
    const j = rest.indexOf("</think>");
    if (j < 0) {
      rest = "";
      break;
    }
    rest = rest.slice(j + "</think>".length);
  }
  // 流式中途可能残留半个标签,隐藏正文中未完整的 "<th…" 尾巴
  const partial = text.match(/<(?:t(?:h(?:i(?:n(?:k)?)?)?)?)?$/);
  if (partial) text = text.slice(0, text.length - partial[0].length);
  return text;
}

// ==================== 子组件 ====================

const STATUS_LABEL: Record<string, { text: string; cls: string }> = {
  running: { text: "运行中", cls: "bg-blue-600/10 text-blue-600 dark:text-blue-400" },
  waiting_input: { text: "等待回答", cls: "bg-violet-500/10 text-violet-600 dark:text-violet-400" },
  suspended: { text: "已挂起", cls: "bg-amber-500/10 text-amber-600 dark:text-amber-400" },
  completed: { text: "已完成", cls: "bg-down/10 text-down" },
  failed: { text: "失败", cls: "bg-up/10 text-up" },
  cancelled: { text: "已取消", cls: "bg-slate-200 text-slate-600 dark:bg-slate-800 dark:text-slate-300" },
  queued: { text: "排队中", cls: "bg-blue-600/10 text-blue-600 dark:text-blue-400" },
  starting: { text: "启动中", cls: "bg-blue-600/10 text-blue-600 dark:text-blue-400" },
  interrupted: { text: "待恢复", cls: "bg-amber-500/10 text-amber-600 dark:text-amber-400" },
};

function StatusBadge({ status }: { status: string }) {
  const s = STATUS_LABEL[status] ?? {
    text: status,
    cls: "bg-slate-200 text-slate-600 dark:bg-slate-800 dark:text-slate-300",
  };
  return <span className={"chip " + s.cls}>{s.text}</span>;
}

const SENSITIVE_DIAGNOSTIC_KEY = /(?:api[-_ ]?key|token|secret|password|authorization|cookie|credential)/i;

/** Remove credentials recursively before diagnostics are rendered or copied. */
export function redactDiagnosticValue(value: unknown, key = ""): unknown {
  if (SENSITIVE_DIAGNOSTIC_KEY.test(key)) return "[已隐藏敏感信息]";
  if (Array.isArray(value)) return value.map((item) => redactDiagnosticValue(item));
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>).map(([childKey, childValue]) => [
        childKey,
        redactDiagnosticValue(childValue, childKey),
      ]),
    );
  }
  if (typeof value === "string") {
    return value
      .replace(/Bearer\s+[A-Za-z0-9._~+\/-]+/gi, "Bearer [已隐藏敏感信息]")
      .replace(
        /((?:api[-_ ]?key|token|secret|password|authorization|cookie|credential)\s*[=:]\s*)[^\s,;]+/gi,
        "$1[已隐藏敏感信息]",
      );
  }
  return value;
}

function safeArguments(args?: string): unknown {
  if (!args) return null;
  try {
    return redactDiagnosticValue(JSON.parse(args));
  } catch {
    return redactDiagnosticValue(args);
  }
}

export function buildToolDiagnostic(tool: ToolCallItem, elapsedMs?: number): string {
  return JSON.stringify(
    {
      类型: "智能助手工具步骤诊断",
      工具: toolDisplayName(tool.name),
      工具标识: tool.name,
      调用编号: tool.callId ?? null,
      状态: tool.done ? (tool.success === false ? "失败" : "完成") : "运行中",
      批次位置: tool.position != null && tool.total != null ? `${tool.position}/${tool.total}` : null,
      参数: safeArguments(tool.args),
      当前阶段: tool.stage ?? null,
      已耗时毫秒: elapsedMs ?? tool.elapsedMs ?? null,
      预估毫秒: tool.estimatedMs ?? null,
      数据来源: tool.source ? sourceDisplayName(tool.source) : null,
      数据时间: tool.fetchedAt ?? null,
      缓存键: tool.cacheKey ?? null,
      错误: tool.error ? redactDiagnosticValue(tool.error) : null,
      最新详细进度: tool.progressDetail ?? null,
      时间线: (tool.timeline ?? []).map((entry) => ({
        时间: new Date(entry.at).toISOString(),
        类型: entry.kind,
        说明: redactDiagnosticValue(entry.message),
        已耗时毫秒: entry.elapsedMs ?? null,
        详细进度: entry.detail ?? null,
      })),
    },
    null,
    2,
  );
}

async function copyDiagnostic(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    return;
  } catch {
    // Tauri/WebView clipboard permissions vary. Keep a safe legacy fallback.
  }
  const textarea = document.createElement("textarea");
  textarea.value = text;
  textarea.style.position = "fixed";
  textarea.style.opacity = "0";
  document.body.appendChild(textarea);
  textarea.select();
  const copied = document.execCommand("copy");
  textarea.remove();
  if (!copied) throw new Error("系统剪贴板不可用");
}

/** 单条工具调用时间线卡片 */
function ToolCard({ tool }: { tool: ToolCallItem }) {
  const [open, setOpen] = useState(false);
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">("idle");
  const [now, setNow] = useState(Date.now());
  const businessName = toolDisplayName(tool.name);
  const label = tool.done ? `${businessName}已完成` : `正在${businessName}`;
  const argumentsToShow = toolArgumentsDisplay(tool.args);
  useEffect(() => {
    if (tool.done || tool.startedAt == null) return;
    const timer = window.setInterval(() => setNow(Date.now()), 500);
    return () => window.clearInterval(timer);
  }, [tool.done, tool.startedAt]);
  const elapsedMs = tool.done
    ? tool.elapsedMs
    : tool.startedAt == null
      ? undefined
      : now - tool.startedAt;
  const hasDetailedTotal = Boolean(tool.progressDetail && tool.progressDetail.total > 0);
  const toolPercent = tool.done
    ? 100
    : hasDetailedTotal
      ? Math.min(99, Math.max(2, (tool.progressDetail!.completed / tool.progressDetail!.total) * 100))
    : tool.estimatedMs && elapsedMs != null
      ? Math.min(94, Math.max(3, (94 * elapsedMs) / (elapsedMs + tool.estimatedMs * 0.6)))
      : 18;
  const estimatedSeconds = tool.estimatedMs ? Math.round(tool.estimatedMs / 1000) : null;
  const overEstimate = Boolean(
    !tool.done && tool.estimatedMs && elapsedMs != null && elapsedMs > tool.estimatedMs,
  );
  const argumentsJson = safeArguments(tool.args);
  const copy = async (text: string) => {
    try {
      await copyDiagnostic(text);
      setCopyState("copied");
    } catch {
      setCopyState("failed");
    }
    window.setTimeout(() => setCopyState("idle"), 2400);
  };
  return (
    <div className="rounded border border-slate-200 bg-slate-50 px-2.5 py-1.5 transition-colors hover:border-blue-300 dark:border-slate-800 dark:bg-slate-900/60 dark:hover:border-blue-800">
      <button
        type="button"
        className="flex w-full flex-wrap items-center gap-2 text-left text-xs"
        aria-expanded={open}
        title="点击查看调用编号、参数、时间线、数据来源和错误"
        onClick={() => setOpen((value) => !value)}
      >
          <span
            className={
              "inline-block h-1.5 w-1.5 rounded-full " +
              (tool.done
                ? tool.success === false
                  ? "bg-up"
                  : "bg-down"
                : "animate-pulse bg-blue-500")
            }
          />
          <span className="font-medium">{label}</span>
          {tool.position != null && tool.total != null && (
            <span className="num muted">{tool.position}/{tool.total}</span>
          )}
          {elapsedMs != null && <span className="num muted">{(elapsedMs / 1000).toFixed(1)}s</span>}
          {!tool.done && <span className="muted min-w-0 flex-1">{tool.stage ?? "正在后台分析，切换页面也会继续"}</span>}
          <span className="muted ml-auto shrink-0 underline decoration-dotted underline-offset-2">
            {open ? "收起任务详情" : "查看任务详情"}
          </span>
      </button>
      <div className="mt-1.5 h-1 overflow-hidden rounded-full bg-slate-200 dark:bg-slate-800">
        <div
          className={
            "h-full rounded-full transition-all duration-500 " +
            (tool.success === false ? "bg-up" : "bg-blue-500")
          }
          style={{ width: `${toolPercent}%` }}
        />
      </div>
      {tool.progressDetail && (
        <div className="muted mt-1 flex flex-wrap justify-between gap-x-3 gap-y-0.5 text-[10px]">
          <span className="num">已处理 {tool.progressDetail.completed} / {tool.progressDetail.total} 只</span>
          <span className="num">成功 {tool.progressDetail.succeeded} · 失败 {tool.progressDetail.failed} · 缓存命中 {tool.progressDetail.cache_hits}</span>
          <span className="num">本轮获取 {tool.progressDetail.records.toLocaleString("zh-CN")} 条上游数据</span>
          <span className="num">当前并行 {tool.progressDetail.active.length} 只</span>
        </div>
      )}
      {!tool.done && estimatedSeconds != null && (
        <div className="muted mt-1 flex flex-wrap justify-between gap-2 text-[10px]">
          <span>预计约 {estimatedSeconds} 秒</span>
          <span>{overEstimate ? "已超过预估，仍在后台继续，可随时取消" : "预估仅供参考，不会自动超时"}</span>
        </div>
      )}
      {open && (
        <div className="mt-2 space-y-2 border-t border-slate-200 pt-2 text-xs dark:border-slate-800">
          <div className="grid gap-x-4 gap-y-1 sm:grid-cols-2">
            <div><span className="muted">调用编号：</span><span className="num break-all">{tool.callId ?? "历史记录未保存编号"}</span></div>
            <div><span className="muted">当前状态：</span>{tool.done ? (tool.success === false ? "执行失败" : "执行完成") : "后台运行中"}</div>
            {tool.startedAt != null && <div><span className="muted">开始时间：</span><span className="num">{new Date(tool.startedAt).toLocaleString("zh-CN", { hour12: false })}</span></div>}
            {tool.estimatedMs != null && <div><span className="muted">参考预估：</span><span className="num">约 {(tool.estimatedMs / 1000).toFixed(0)} 秒（不作为超时限制）</span></div>}
            {tool.source && <div><span className="muted">数据来源：</span>{sourceDisplayName(tool.source)}</div>}
            {tool.fetchedAt && <div><span className="muted">数据时间：</span><span className="num">{fetchedAtDisplay(tool.fetchedAt)}</span></div>}
            {tool.cacheKey && <div className="sm:col-span-2"><span className="muted">缓存键：</span><span className="num break-all">{tool.cacheKey}</span></div>}
          </div>
          {argumentsToShow.length > 0 && (
            <dl className="grid gap-x-3 gap-y-0.5 rounded bg-slate-100 p-2 dark:bg-slate-950/60 sm:grid-cols-2">
              {argumentsToShow.map((item, index) => (
                <div key={`${item.label}-${index}`} className="flex min-w-0 gap-1">
                  <dt className="muted shrink-0">{item.label}：</dt>
                  <dd className="num min-w-0 break-all">{item.value}</dd>
                </div>
              ))}
            </dl>
          )}
          {tool.args && (
            <details>
              <summary className="muted cursor-pointer select-none">查看脱敏后的原始参数</summary>
              <pre className="mt-1 max-h-40 overflow-auto whitespace-pre-wrap break-all rounded bg-slate-950 p-2 text-[11px] text-slate-200">
                {typeof argumentsJson === "string" ? argumentsJson : JSON.stringify(argumentsJson, null, 2)}
              </pre>
            </details>
          )}
          {tool.progressDetail && (
            <section className="rounded border border-blue-200 bg-blue-50/70 p-2.5 dark:border-blue-900/70 dark:bg-blue-950/25">
              <div className="mb-2 flex items-center gap-2">
                <div className="micro-label text-blue-700 dark:text-blue-300">候选股处理明细</div>
                <span className="num ml-auto text-blue-700 dark:text-blue-300">{tool.progressDetail.completed} / {tool.progressDetail.total}</span>
              </div>
              <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">
                {[
                  ["成功", tool.progressDetail.succeeded],
                  ["失败", tool.progressDetail.failed],
                  ["缓存命中", tool.progressDetail.cache_hits],
                  ["新获取数据", `${tool.progressDetail.records.toLocaleString("zh-CN")} 条`],
                  ["当前并行", `${tool.progressDetail.active.length} 只`],
                  ["剩余", `${Math.max(0, tool.progressDetail.total - tool.progressDetail.completed)} 只`],
                ].map(([metricLabel, value]) => (
                  <div key={metricLabel} className="rounded bg-white/80 px-2 py-1.5 dark:bg-slate-950/50">
                    <div className="muted text-[10px]">{metricLabel}</div>
                    <div className="num mt-0.5 font-medium">{value}</div>
                  </div>
                ))}
              </div>
              {tool.progressDetail.active.length > 0 && (
                <details className="mt-2" open>
                  <summary className="cursor-pointer select-none font-medium">正在处理的股票（{tool.progressDetail.active.length}）</summary>
                  <ul className="mt-1.5 space-y-1">
                    {tool.progressDetail.active.map((item) => (
                      <li key={item.label} className="grid gap-1 rounded bg-white/80 px-2 py-1.5 dark:bg-slate-950/50 sm:grid-cols-[8rem_1fr]">
                        <span className="num font-medium">{item.label}</span>
                        <span className="muted">{item.stage}</span>
                      </li>
                    ))}
                  </ul>
                </details>
              )}
              {tool.progressDetail.recent_errors.length > 0 && (
                <details className="mt-2">
                  <summary className="cursor-pointer select-none font-medium text-red-600 dark:text-red-300">最近错误（{tool.progressDetail.recent_errors.length}）</summary>
                  <ul className="mt-1.5 space-y-1 text-red-700 dark:text-red-300">
                    {tool.progressDetail.recent_errors.map((error, index) => (
                      <li key={index} className="break-all rounded bg-red-50 px-2 py-1 dark:bg-red-950/40">{String(redactDiagnosticValue(error))}</li>
                    ))}
                  </ul>
                </details>
              )}
            </section>
          )}
          {(tool.timeline?.length ?? 0) > 0 && (
            <div>
              <div className="micro-label mb-1">工作时间线</div>
              <ol className="space-y-1 border-l border-slate-300 pl-3 dark:border-slate-700">
                {tool.timeline!.map((entry, index) => (
                  <li key={`${entry.at}-${index}`} className="grid gap-x-2 sm:grid-cols-[7.5rem_1fr_auto]">
                    <span className="num muted">{new Date(entry.at).toLocaleTimeString("zh-CN", { hour12: false })}</span>
                    <span className={entry.kind === "error" ? "text-red-600 dark:text-red-300" : ""}>{entry.message}</span>
                    {entry.elapsedMs != null && <span className="num muted">{(entry.elapsedMs / 1000).toFixed(1)}s</span>}
                  </li>
                ))}
              </ol>
            </div>
          )}
          <div className="flex flex-wrap items-center gap-2">
            <button type="button" className="btn" onClick={() => void copy(buildToolDiagnostic(tool, elapsedMs))}>
              复制完整诊断
            </button>
            {tool.error && (
              <button type="button" className="btn" onClick={() => void copy(String(redactDiagnosticValue(tool.error)))}>
                复制错误
              </button>
            )}
            {copyState === "copied" && <span className="text-emerald-600 dark:text-emerald-400">已复制，可直接粘贴用于诊断</span>}
            {copyState === "failed" && <span className="text-red-600 dark:text-red-300">剪贴板不可用，请展开参数后手动选择复制</span>}
            <span className="muted ml-auto">密钥、令牌和认证字段会自动隐藏</span>
          </div>
        </div>
      )}
      {tool.done && (tool.source || tool.fetchedAt) && (
        <div className="muted mt-1 text-xs">
          {tool.source && <span>数据来源：{sourceDisplayName(tool.source)}</span>}
          {tool.fetchedAt && <span className="num ml-2">更新时间：{fetchedAtDisplay(tool.fetchedAt)}</span>}
        </div>
      )}
      {tool.done && tool.success === false && tool.error && (
        <div className="mt-1 rounded bg-red-50 px-2 py-1 text-xs text-red-700 dark:bg-red-950/40 dark:text-red-300">
          本步骤未成功：{tool.error}
        </div>
      )}
    </div>
  );
}

const RESEARCH_MODE_LABEL: Record<string, string> = {
  quick: "快速模式",
  deep: "深度模式",
  plan: "计划模式",
};
const REASONING_DEPTH_LABEL: Record<string, string> = {
  standard: "标准思考",
  deep: "深入思考",
  maximum: "极深思考",
};

export function buildTaskDiagnostic(
  task: AgentTask,
  tools: ToolCallItem[] = [],
  progress: AgentProgress | null = null,
): string {
  return JSON.stringify(
    redactDiagnosticValue({
      类型: "智能助手后台任务诊断",
      任务编号: task.id,
      会话编号: task.conversation_id,
      状态: STATUS_LABEL[task.status]?.text ?? task.status,
      创建时间: new Date(task.created_at * 1000).toISOString(),
      更新时间: new Date(task.updated_at * 1000).toISOString(),
      用户目标: task.prompt,
      研究模式: task.research_mode,
      思考深度: task.reasoning_depth,
      指定模型: task.model,
      当前轮次: task.round,
      轮次上限: task.max_rounds,
      可用工具: task.enabled_tools ?? "系统默认完整工具集",
      额度恢复后自动继续: task.auto_resume_on_quota,
      专家数量: task.specialist_count,
      已收集证据: task.evidence_count,
      上下文压缩次数: task.context_compactions,
      已完成多专家复核: task.multi_agent_reviewed,
      当前进度: progress,
      错误: task.last_error,
      工具步骤: tools.map((tool) => JSON.parse(buildToolDiagnostic(tool))),
    }),
    null,
    2,
  );
}

function TaskDetailDialog({
  task,
  tools,
  progress,
  isCurrent,
  onClose,
  onCancel,
  onResume,
  onOpenConversation,
  canOpenConversation,
}: {
  task: AgentTask;
  tools: ToolCallItem[];
  progress: AgentProgress | null;
  isCurrent: boolean;
  onClose: () => void;
  onCancel: () => void;
  onResume: () => void;
  onOpenConversation: () => void;
  canOpenConversation: boolean;
}) {
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">("idle");
  const duration = Math.max(0, (task.updated_at - task.created_at) * 1000);
  const canResume = task.status === "suspended" || task.status === "interrupted" || task.status === "waiting";
  const canCancel = task.status === "running" || task.status === "queued" || task.status === "starting";
  const copy = async (text: string) => {
    try {
      await copyDiagnostic(text);
      setCopyState("copied");
    } catch {
      setCopyState("failed");
    }
    window.setTimeout(() => setCopyState("idle"), 2400);
  };
  return (
    <div className="absolute inset-0 z-50 flex justify-end bg-slate-950/45" role="presentation" onMouseDown={onClose}>
      <section
        className="flex h-full w-full max-w-2xl flex-col border-l border-slate-200 bg-white shadow-2xl dark:border-slate-800 dark:bg-slate-950"
        role="dialog"
        aria-modal="true"
        aria-label="后台任务详情"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header className="flex items-center gap-2 border-b border-slate-200 px-4 py-3 dark:border-slate-800">
          <div>
            <h2 className="font-semibold">后台任务详情</h2>
            <div className="num muted mt-0.5 text-xs break-all">{task.id}</div>
          </div>
          <StatusBadge status={task.status} />
          {isCurrent && <span className="chip bg-blue-600/10 text-blue-600 dark:text-blue-400">当前对话</span>}
          <button type="button" className="btn ml-auto" onClick={onClose}>关闭</button>
        </header>
        <div className="min-h-0 flex-1 space-y-4 overflow-y-auto p-4 text-sm">
          {task.last_error && (
            <div className="rounded border border-red-300 bg-red-50 p-3 text-red-700 dark:border-red-900 dark:bg-red-950/40 dark:text-red-300">
              <div className="mb-1 font-semibold">检测到错误</div>
              <div className="whitespace-pre-wrap break-all text-xs">{String(redactDiagnosticValue(task.last_error))}</div>
              <button type="button" className="btn mt-2" onClick={() => void copy(String(redactDiagnosticValue(task.last_error)))}>复制错误</button>
            </div>
          )}
          {progress && isCurrent && <ResearchProgress progress={progress} />}
          <section className="rounded border border-slate-200 dark:border-slate-800">
            <div className="card-title">任务概况</div>
            <dl className="grid gap-x-4 gap-y-2 p-3 text-xs sm:grid-cols-2">
              <div><dt className="muted">创建时间</dt><dd className="num mt-0.5">{fmtUnix(task.created_at)}</dd></div>
              <div><dt className="muted">最近更新</dt><dd className="num mt-0.5">{fmtUnix(task.updated_at)}</dd></div>
              <div><dt className="muted">已记录耗时</dt><dd className="num mt-0.5">{(duration / 1000).toFixed(1)} 秒</dd></div>
              <div><dt className="muted">分析轮次</dt><dd className="num mt-0.5">{task.round}{task.max_rounds ? ` / ${task.max_rounds}` : ""}</dd></div>
              <div><dt className="muted">研究模式</dt><dd className="mt-0.5">{RESEARCH_MODE_LABEL[task.research_mode ?? ""] ?? "系统默认"}</dd></div>
              <div><dt className="muted">思考深度</dt><dd className="mt-0.5">{REASONING_DEPTH_LABEL[task.reasoning_depth ?? ""] ?? "系统默认"}</dd></div>
              <div><dt className="muted">底层模型</dt><dd className="mt-0.5">{task.model ?? "由系统按任务难度自动选择"}</dd></div>
              <div><dt className="muted">断点续跑</dt><dd className="mt-0.5">{task.auto_resume_on_quota ? "额度恢复后自动继续" : "未开启自动继续"}</dd></div>
              <div><dt className="muted">证据记录</dt><dd className="num mt-0.5">{task.evidence_count} 条</dd></div>
              <div><dt className="muted">上下文压缩</dt><dd className="num mt-0.5">{task.context_compactions} 次</dd></div>
              <div><dt className="muted">独立专家</dt><dd className="num mt-0.5">{task.specialist_count} 位</dd></div>
              <div><dt className="muted">专家复核</dt><dd className="mt-0.5">{task.multi_agent_reviewed ? "已完成" : task.specialist_count ? "尚未完成" : "本轮未配置"}</dd></div>
              <div className="sm:col-span-2"><dt className="muted">所属会话</dt><dd className="num mt-0.5 break-all">{task.conversation_id}</dd></div>
            </dl>
          </section>
          {task.prompt && (
            <section className="rounded border border-slate-200 p-3 dark:border-slate-800">
              <div className="micro-label mb-1">本轮用户目标</div>
              <div className="whitespace-pre-wrap break-words">{task.prompt}</div>
            </section>
          )}
          <section className="rounded border border-slate-200 p-3 dark:border-slate-800">
            <div className="mb-2 flex items-center gap-2">
              <div className="micro-label">本轮工具权限</div>
              <span className="num muted ml-auto">{task.enabled_tools?.length ?? DEFAULT_AGENT_TOOLS.length} 项</span>
            </div>
            <div className="flex flex-wrap gap-1.5">
              {(task.enabled_tools ?? [...DEFAULT_AGENT_TOOLS]).map((name) => (
                <span key={name} className="chip bg-slate-100 text-slate-600 dark:bg-slate-900 dark:text-slate-300">{toolDisplayName(name)}</span>
              ))}
              {task.enabled_tools?.length === 0 && <span className="muted text-xs">本轮关闭了全部工具，只进行文本分析。</span>}
            </div>
          </section>
          {isCurrent && tools.length > 0 && (
            <section>
              <div className="mb-2 flex items-center"><div className="micro-label">具体工作步骤</div><span className="num muted ml-auto">{tools.length} 项</span></div>
              <div className="space-y-2">{tools.map((tool) => <ToolCard key={tool.key} tool={tool} />)}</div>
            </section>
          )}
          {!isCurrent && (
            <div className="rounded bg-blue-50 p-3 text-xs text-blue-700 dark:bg-blue-950/30 dark:text-blue-300">
              点击“打开所属对话”可查看该任务保存的完整问答与工具结果。历史任务的高频心跳已限长保存，避免日志无限占用空间。
            </div>
          )}
        </div>
        <footer className="flex flex-wrap items-center gap-2 border-t border-slate-200 px-4 py-3 dark:border-slate-800">
          <button type="button" className="btn-primary" onClick={() => void copy(buildTaskDiagnostic(task, tools, progress))}>复制完整诊断</button>
          <button type="button" className="btn" disabled={!canOpenConversation} onClick={onOpenConversation}>
            {isCurrent ? "当前对话已打开" : "打开所属对话"}
          </button>
          {canResume && <button type="button" className="btn" onClick={onResume}>选择并继续任务</button>}
          {canCancel && <button type="button" className="btn text-red-600 dark:text-red-300" onClick={onCancel}>取消任务</button>}
          {copyState === "copied" && <span className="text-xs text-emerald-600 dark:text-emerald-400">诊断信息已复制并自动脱敏</span>}
          {copyState === "failed" && <span className="text-xs text-red-600 dark:text-red-300">剪贴板不可用</span>}
        </footer>
      </section>
    </div>
  );
}

function ResearchProgress({ progress }: { progress: AgentProgress }) {
  const determinate = progress.total != null && progress.total > 0;
  const percent = determinate
    ? Math.min(100, Math.max(0, ((progress.completed ?? 0) / progress.total!) * 100))
    : null;
  const currentPhase = Math.max(0, PROGRESS_PHASE_ORDER.indexOf(progress.phase));
  return (
    <div className="rounded-lg border border-blue-200 bg-blue-50/80 px-3 py-2.5 text-xs dark:border-blue-900/70 dark:bg-blue-950/30">
      <div className="mb-2 grid grid-cols-5 gap-1">
        {PROGRESS_PHASE_ORDER.map((phase, index) => (
          <div
            key={phase}
            className={
              "rounded px-1.5 py-1 text-center transition-colors " +
              (index < currentPhase
                ? "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400"
                : index === currentPhase
                  ? "bg-blue-600 text-white"
                  : "bg-slate-200/70 text-slate-500 dark:bg-slate-800 dark:text-slate-400")
            }
          >
            {index < currentPhase ? "✓ " : index === currentPhase ? "● " : "○ "}
            {PROGRESS_PHASE_LABELS[phase]}
          </div>
        ))}
      </div>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <span className="font-medium text-blue-700 dark:text-blue-300">{progress.message}</span>
        <span className="num muted ml-auto">
          分析轮次 {progress.round}
          {progress.maxRounds > 0 ? ` / 安全上限 ${progress.maxRounds}` : ""}
        </span>
      </div>
      <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-blue-100 dark:bg-blue-950">
        <div
          className={
            "h-full rounded-full bg-blue-500 transition-all duration-500 " +
            (percent == null ? "w-2/5 animate-pulse" : "")
          }
          style={percent == null ? undefined : { width: `${percent}%` }}
        />
      </div>
      <div className="muted mt-1.5 flex justify-between">
        <span>阶段：{PROGRESS_PHASE_LABELS[progress.phase] ?? progress.phase}</span>
        {determinate && <span className="num">{progress.completed ?? 0} / {progress.total}</span>}
      </div>
      <details className="mt-2 border-t border-blue-200/70 pt-1.5 dark:border-blue-900/60">
        <summary className="muted cursor-pointer select-none text-[11px]">系统优化详情</summary>
        <div className="muted mt-1 grid gap-1 text-[11px] sm:grid-cols-2">
          <span>· 相同数据自动复用缓存，避免重复访问上游</span>
          <span>· 独立分析最多 6 项并行，完成一项更新一项</span>
          <span>· 工具无固定超时，显示预估与已耗时，可由用户随时取消</span>
          <span>· 对话、证据与任务状态持续保存，切换页面不中断</span>
          <span>· 深度任务可由独立专家并行复核，主分析师统一综合</span>
        </div>
      </details>
    </div>
  );
}

export function ClarificationCard({
  request,
  draft,
  submitted,
  onChange,
  onSubmit,
}: {
  request: ClarificationRequest;
  draft: ClarificationDraft;
  submitted: boolean;
  onChange: (draft: ClarificationDraft) => void;
  onSubmit: () => void;
}) {
  const choose = (questionId: string, optionId: string, multiple: boolean) => {
    const current = draft.selections[questionId] ?? [];
    const next = multiple
      ? current.includes(optionId)
        ? current.filter((id) => id !== optionId)
        : [...current, optionId]
      : [optionId];
    onChange({ ...draft, selections: { ...draft.selections, [questionId]: next } });
  };
  const complete = clarificationIsComplete(request, draft);
  return (
    <section className="mt-3 overflow-hidden rounded-lg border border-violet-300 bg-violet-50/70 dark:border-violet-900/80 dark:bg-violet-950/25">
      <div className="border-b border-violet-200 px-3 py-2.5 dark:border-violet-900/70">
        <div className="flex items-center gap-2">
          <span className="inline-flex h-6 w-6 items-center justify-center rounded-full bg-violet-600 text-xs font-bold text-white">?</span>
          <div>
            <div className="text-sm font-semibold text-violet-900 dark:text-violet-100">{request.title}</div>
            {request.description && <div className="muted mt-0.5 text-xs">{request.description}</div>}
          </div>
          <span className="chip ml-auto bg-violet-600/10 text-violet-700 dark:text-violet-300">
            {submitted ? "已回答" : `共 ${request.questions.length} 项`}
          </span>
        </div>
      </div>
      <div className="space-y-4 p-3">
        {request.questions.map((question, questionIndex) => {
          const selected = draft.selections[question.id] ?? [];
          return (
            <fieldset key={question.id} disabled={submitted} className="space-y-2">
              <legend className="mb-2 text-sm font-semibold">
                <span className="mr-1.5 text-violet-600 dark:text-violet-400">{questionIndex + 1}.</span>
                {question.question}
                {question.kind === "multiple" && <span className="muted ml-2 text-xs font-normal">可多选</span>}
              </legend>
              <div className="grid gap-2 sm:grid-cols-2">
                {question.options.map((option) => {
                  const checked = selected.includes(option.id);
                  return (
                    <label
                      key={option.id}
                      className={
                        "cursor-pointer rounded-md border px-3 py-2 transition-colors " +
                        (checked
                          ? "border-violet-500 bg-violet-100 ring-1 ring-violet-400 dark:bg-violet-900/45"
                          : "border-slate-200 bg-white hover:border-violet-300 dark:border-slate-700 dark:bg-slate-900/70") +
                        (submitted ? " cursor-default opacity-80" : "")
                      }
                    >
                      <span className="flex items-start gap-2">
                        <input
                          className="mt-0.5 accent-violet-600"
                          type={question.kind === "multiple" ? "checkbox" : "radio"}
                          name={`clarification-${question.id}`}
                          checked={checked}
                          onChange={() => choose(question.id, option.id, question.kind === "multiple")}
                        />
                        <span className="min-w-0">
                          <span className="text-sm font-medium">{option.label}</span>
                          {option.recommended && (
                            <span className="ml-1.5 rounded bg-emerald-500/10 px-1.5 py-0.5 text-[10px] text-emerald-700 dark:text-emerald-400">建议</span>
                          )}
                          {option.description && <span className="muted mt-0.5 block text-xs">{option.description}</span>}
                        </span>
                      </span>
                    </label>
                  );
                })}
              </div>
              {question.allowOther && (
                <input
                  className="input mt-2 w-full"
                  value={draft.other[question.id] ?? ""}
                  disabled={submitted}
                  placeholder="其他情况或补充说明（可直接填写）"
                  onChange={(event) =>
                    onChange({
                      ...draft,
                      other: { ...draft.other, [question.id]: event.target.value },
                    })
                  }
                />
              )}
            </fieldset>
          );
        })}
        {!submitted ? (
          <div className="flex flex-wrap items-center gap-2 border-t border-violet-200 pt-3 dark:border-violet-900/60">
            <button className="btn-primary" disabled={!complete} onClick={onSubmit}>
              提交选择并继续分析
            </button>
            <span className="muted text-xs">
              {complete ? "提交后，Agent 会把这些选择作为本会话的已确认条件。" : "请为每个问题选择一项，或填写补充说明。"}
            </span>
          </div>
        ) : (
          <div className="rounded bg-emerald-500/10 px-2.5 py-2 text-xs text-emerald-700 dark:text-emerald-400">
            已提交，Agent 正在沿用这些条件继续研究；返回本页面时选择记录仍会保留。
          </div>
        )}
      </div>
    </section>
  );
}

export function hasUnansweredClarification(messages: ChatMsg[]): boolean {
  for (let index = messages.length - 1; index >= 0; index--) {
    const message = messages[index];
    if (message.role === "user") return false;
    if (
      message.role === "assistant" &&
      hasClarification(message.report?.answer ?? message.raw)
    ) {
      return message.clarificationSubmitted !== true && message.clarificationDraft?.submitted !== true;
    }
  }
  return false;
}

/** 助手消息气泡 */
function AssistantMsg({
  msg,
  onClarificationSubmit,
}: {
  msg: ChatMsg;
  onClarificationSubmit: (request: ClarificationRequest, draft: ClarificationDraft) => void;
}) {
  const answer = stripPrivateReasoning(msg.report ? msg.report.answer : msg.raw);
  const clarification = parseClarification(answer);
  const visibleAnswer = clarification ? clarification.displayText : answer;
  const draft = msg.clarificationDraft ?? emptyClarificationDraft();
  return (
    <div className="card anim-fade-up mr-auto w-full max-w-3xl px-3 py-2.5">
      {msg.tools.length > 0 && (
        <div className="mb-2 space-y-1.5 border-l-2 border-slate-200 pl-2.5 dark:border-slate-700">
          {msg.tools.map((t) => (
            <ToolCard key={t.key} tool={t} />
          ))}
        </div>
      )}
      {visibleAnswer.trim() ? (
        <Markdown src={visibleAnswer} />
      ) : (
        !msg.failed && !msg.done && !clarification?.pending && <div className="muted text-sm">正在思考…</div>
      )}
      {clarification?.pending && (
        <div className="mt-2 rounded border border-violet-200 bg-violet-50 px-3 py-2 text-sm text-violet-700 dark:border-violet-900/70 dark:bg-violet-950/30 dark:text-violet-300">
          正在整理需要你确认的选项…
        </div>
      )}
      {clarification && !clarification.pending && clarification.request.questions.length > 0 && (
        <ClarificationCard
          request={clarification.request}
          draft={draft}
          submitted={msg.clarificationSubmitted === true || draft.submitted === true}
          onChange={(next) => patchClarificationDraft(msg.key, () => next)}
          onSubmit={() => onClarificationSubmit(clarification.request, draft)}
        />
      )}
      {msg.report && msg.report.evidence.length > 0 && (
        <div className="mt-3 rounded border border-slate-200 dark:border-slate-800">
          <div className="muted border-b border-slate-200 px-2.5 py-1.5 text-xs dark:border-slate-800">
            <Term label="证据清单" tip="本回答引用的工具数据快照,可按缓存键溯源复核" />(
            {msg.report.evidence.length})
          </div>
          <ul className="divide-y divide-slate-100 dark:divide-slate-800/60">
            {msg.report.evidence.map((ev, i) => (
              <li key={i} className="num flex flex-wrap gap-x-3 px-2.5 py-1.5 text-xs">
                <span className="font-medium">{toolDisplayName(ev.tool)}</span>
                <span className="muted">数据来源：{sourceDisplayName(ev.source)}</span>
                <span className="muted">更新时间：{fetchedAtDisplay(ev.fetched_at)}</span>
              </li>
            ))}
          </ul>
        </div>
      )}
      {msg.report && !clarification && (
        <div className="muted mt-3 rounded border border-amber-200 bg-amber-50 px-2.5 py-2 text-xs dark:border-amber-900/60 dark:bg-amber-950/30">
          免责声明:以上内容由 AI 基于公开数据生成,仅供参考,不构成投资建议。市场有风险,决策需独立。
        </div>
      )}
      {msg.failed && (
        <div className="mt-1 rounded border border-red-300 bg-red-50 px-2.5 py-1.5 text-xs text-red-700 dark:border-red-900 dark:bg-red-950/40 dark:text-red-300">
          {msg.failed}
        </div>
      )}
    </div>
  );
}

// ==================== 对话组件(页面版 / 抽屉版复用) ====================

export default function AgentChat({
  variant = "page",
  onClose,
}: {
  /** page:全页(历史抽屉在左侧);drawer:全局呼出抽屉(历史为覆盖层) */
  variant?: "page" | "drawer";
  onClose?: () => void;
}) {
  const currentSymbol = useAppStore((s) => s.currentSymbol);
  const currentName = useAppStore((s) => s.currentName);
  const [hasKey, setHasKey] = useState<boolean | null>(null);
  const msgs = useAgentSession((s) => s.msgs);
  const input = useAgentSession((s) => s.input);
  const status = useAgentSession((s) => s.status);
  const compactionCount = useAgentSession((s) => s.compactionCount);
  const taskId = useAgentSession((s) => s.taskId);
  const conversationId = useAgentSession((s) => s.conversationId);
  const err = useAgentSession((s) => s.err);
  const progress = useAgentSession((s) => s.progress);
  const pendingQuestions = useAgentSession((s) => s.pendingQuestions ?? []);
  const researchMode = useAgentSession((s) => s.researchMode ?? "deep");
  const reasoningDepth = useAgentSession((s) => s.reasoningDepth ?? "deep");
  const enabledTools = useAgentSession((s) => s.enabledTools ?? [...DEFAULT_AGENT_TOOLS]);
  const autoResumeOnQuota = useAgentSession((s) => s.autoResumeOnQuota ?? true);
  const setInput = useAgentSession((s) => s.setInput);
  const setStatus = useAgentSession((s) => s.setStatus);
  const setErr = useAgentSession((s) => s.setErr);
  const setResearchMode = useAgentSession((s) => s.setResearchMode);
  const setReasoningDepth = useAgentSession((s) => s.setReasoningDepth);
  const setEnabledTools = useAgentSession((s) => s.setEnabledTools);
  const setAutoResumeOnQuota = useAgentSession((s) => s.setAutoResumeOnQuota);
  const [tasks, setTasks] = useState<AgentTask[]>([]);
  const [convs, setConvs] = useState<AgentConversation[]>([]);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [taskDetail, setTaskDetail] = useState<AgentTask | null>(null);

  const scrollRef = useRef<HTMLDivElement>(null);

  const running = status === "running";
  const waitingForInput = status === "waiting_input";

  // 滚动到底部
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [msgs, status]);

  const refreshTasks = useCallback(async () => {
    try {
      setTasks(await agentTasks());
    } catch {
      /* 轮询失败忽略 */
    }
  }, []);

  const refreshConvs = useCallback(async () => {
    try {
      setConvs(await agentConversations());
    } catch {
      /* 忽略 */
    }
  }, []);

  // 初始化:key 检测 + 历史列表 + 刷新后恢复未结束任务
  useEffect(() => {
    if (!isTauri()) return;
    minimaxStatus()
      .then((s) => setHasKey(s.has_key))
      .catch((e) => {
        setHasKey(null);
        setErr(`智能助手服务暂不可用：${errMsg(e)}`);
      });
    refreshTasks();
    refreshConvs();
    agentTasks()
      .then((list) => {
        const session = useAgentSession.getState();
        const current = session.taskId
          ? list.find((task) => task.id === session.taskId)
          : undefined;
        if (current) {
          const durableStatus = taskRunStatus(current.status);
          const effectiveStatus =
            durableStatus === "completed" && hasUnansweredClarification(session.msgs)
              ? "waiting_input"
              : durableStatus;
          if (effectiveStatus !== session.status) {
            useAgentSession.setState({
              status: effectiveStatus,
              progress: effectiveStatus === "running" ? session.progress : null,
            });
          }
          return;
        }
        const pending = list
          .filter((t) => t.status === "running" || t.status === "suspended" || t.status === "interrupted")
          .sort((a, b) => b.updated_at - a.updated_at)[0];
        if (pending && !session.taskId) {
          selectAgentTask(pending.id, pending.conversation_id, "suspended");
          if (session.msgs.length === 0) {
            useAgentSession.setState({
              msgs: [
                {
                  key: nextAgentKey(),
                  role: "assistant",
                  raw:
                    pending.status === "suspended"
                      ? "检测到上次因配额挂起的任务，可在配额恢复后继续。"
                      : "检测到应用退出时被中断的后台任务，可从持久化状态继续运行。",
                  tools: [],
                  suspendedAt: pending.status === "suspended" ? pending.updated_at : undefined,
                  done: false,
                },
              ],
            });
          }
        }
      })
      .catch(() => {});
  }, [refreshTasks, refreshConvs]);

  // 任务列表轮询(挂起/运行状态刷新)
  useEffect(() => {
    const t = setInterval(refreshTasks, 5000);
    return () => clearInterval(t);
  }, [refreshTasks]);

  const send = async (question: string) => {
    const q = question.trim();
    if (!q || running) return;
    const session = useAgentSession.getState();
    appendAgentTurn(q);
    // 新会话首轮:把当前查看的股票作为上下文前置(气泡仍只显示用户原文)
    let payload = q;
    if (session.conversationId === null && currentSymbol) {
      payload = `【上下文】用户在查看 ${currentSymbol} ${currentName ?? ""}\n`.replace(/\s+\n/, "\n") + q;
    }
    try {
      const r = await agentAsk(
        payload,
        session.conversationId,
        handleAgentEnvelope,
        {
          research_mode: session.researchMode ?? "deep",
          reasoning_depth: session.reasoningDepth ?? "deep",
          enabled_tools: session.enabledTools ?? [...DEFAULT_AGENT_TOOLS],
          auto_resume_on_quota: session.autoResumeOnQuota ?? true,
        },
      );
      setAgentRunIdentity(r.task_id, r.conversation_id);
      refreshTasks();
      refreshConvs();
    } catch (e) {
      if (errKind(e) === "no_key") {
        setHasKey(false);
        patchLastAssistant((m) => ({
          ...m,
          failed: "尚未配置 MiniMax 访问密钥，请先到「设置」页填写后再提问。",
          done: true,
        }));
      } else {
        patchLastAssistant((m) => ({ ...m, failed: errMsg(e), done: true }));
      }
      setStatus("failed");
    }
  };

  const submitClarification = (
    messageKey: number,
    request: ClarificationRequest,
    draft: ClarificationDraft,
  ) => {
    if (!clarificationIsComplete(request, draft) || running) return;
    patchClarificationDraft(messageKey, (current) => ({ ...current, submitted: true }));
    useAgentSession.setState({ status: "completed" });
    void send(formatClarificationAnswer(request, draft));
  };

  const queueFollowUp = (question: string) => {
    const q = question.trim();
    if (!q) return;
    const session = useAgentSession.getState();
    useAgentSession.setState({
      input: "",
      pendingQuestions: [...(session.pendingQuestions ?? []), q],
    });
  };

  // The user can keep talking while a long analysis runs. Follow-ups are
  // dispatched in order against the same persisted conversation.
  useEffect(() => {
    if (
      status !== "completed" ||
      hasKey === false ||
      pendingQuestions.length === 0 ||
      hasUnansweredClarification(msgs)
    ) return;
    const [next, ...rest] = pendingQuestions;
    useAgentSession.setState({ pendingQuestions: rest });
    void send(next);
  }, [status, hasKey, pendingQuestions, msgs]);

  const resume = async () => {
    const id = useAgentSession.getState().taskId;
    if (!id) return;
    setErr(null);
    try {
      useAgentSession.setState({
        lastSeq: 0,
        status: "running",
        progress: {
          phase: "preparing",
          message: "正在校验中断点并恢复后台任务",
          round: 1,
          maxRounds: 32,
          completed: null,
          total: null,
          updatedAt: Date.now(),
        },
      });
      await agentResume(id, handleAgentEnvelope);
      patchLastAssistant((m) => ({ ...m, suspendedAt: undefined }));
    } catch (e) {
      setErr(errMsg(e));
      setStatus("failed");
    }
  };

  const cancel = async () => {
    const id = useAgentSession.getState().taskId;
    if (!id) return;
    try {
      await agentCancel(id);
      useAgentSession.setState({ status: "cancelled", progress: null });
      patchLastAssistant((m) => ({ ...m, failed: "已手动取消。", done: true }));
      refreshTasks();
    } catch (e) {
      setErr(errMsg(e));
    }
  };

  /** 从任务列表恢复某个挂起任务 */
  const resumeTask = (t: AgentTask) => {
    selectAgentTask(t.id, t.conversation_id, "suspended");
    const session = useAgentSession.getState();
    useAgentSession.setState({
      msgs: [
        ...session.msgs,
        {
          key: nextAgentKey(),
          role: "assistant",
          raw: "已选中中断任务。点击下方「继续分析」后，将先修复工具调用链再恢复。",
          tools: [],
          suspendedAt: t.updated_at,
          done: false,
        },
      ],
    });
  };

  /** 加载历史会话 */
  const loadConversation = async (c: AgentConversation) => {
    setErr(null);
    try {
      const history = await agentConversationLoad(c.id);
      const out: ChatMsg[] = [];
      for (const m of history) {
        out.push(...historyToMsgs(m, out));
      }
      const loadedAt = Date.now();
      const normalized = out.map((message) => ({
        ...message,
        tools: message.tools.map((tool) =>
          tool.done
            ? tool
            : {
                ...tool,
                done: true,
                success: false,
                error: "历史记录中未找到对应工具结果，可能在应用退出时被中断。",
                timeline: [
                  ...(tool.timeline ?? []),
                  {
                    at: loadedAt,
                    kind: "error" as const,
                    message: "加载历史时未找到对应结果",
                  },
                ],
              },
        ),
      }));
      useAgentSession.setState({
        msgs: normalized,
        conversationId: c.id,
        taskId: null,
        lastSeq: 0,
        status: hasUnansweredClarification(normalized) ? "waiting_input" : "idle",
        err: null,
        progress: null,
      });
      setHistoryOpen(false);
    } catch (e) {
      setErr(errMsg(e));
    }
  };

  /** 新对话:清空当前上下文,下一次提问开启新 conversation */
  const newChat = () => {
    if (running) return;
    resetAgentSession();
    setHistoryOpen(false);
  };

  /** 删除历史会话(确认后);删当前会话则开新对话 */
  const deleteConversation = async (c: AgentConversation) => {
    if (!window.confirm(`删除会话「${c.title || "未命名会话"}」?该操作不可恢复。`)) return;
    setErr(null);
    try {
      await agentConversationDelete(c.id);
      if (useAgentSession.getState().conversationId === c.id) newChat();
      await refreshConvs();
    } catch (e) {
      setErr(errMsg(e));
    }
  };

  const showCurrentTaskDetail = () => {
    if (!taskId) return;
    const known = tasks.find((task) => task.id === taskId);
    if (known) {
      setTaskDetail(known);
      return;
    }
    const now = Math.floor(Date.now() / 1000);
    const lastUser = [...msgs].reverse().find((message) => message.role === "user");
    setTaskDetail({
      id: taskId,
      conversation_id: conversationId ?? taskId,
      kind: "chat",
      status,
      created_at: now,
      updated_at: now,
      prompt: lastUser?.raw ?? null,
      research_mode: researchMode,
      reasoning_depth: reasoningDepth,
      model: null,
      round: progress?.round ?? 0,
      max_rounds: progress?.maxRounds ?? null,
      enabled_tools: enabledTools,
      auto_resume_on_quota: autoResumeOnQuota,
      specialist_count: 0,
      evidence_count: msgs.reduce((count, message) => count + (message.report?.evidence.length ?? 0), 0),
      context_compactions: compactionCount,
      multi_agent_reviewed: false,
      last_error: err,
    });
  };

  const cancelTaskFromDetail = async () => {
    const selected = taskDetail;
    if (!selected) return;
    setErr(null);
    try {
      await agentCancel(selected.id);
      if (selected.id === taskId) {
        useAgentSession.setState({ status: "cancelled", progress: null });
        patchLastAssistant((message) => ({ ...message, failed: "已手动取消。", done: true }));
      }
      setTaskDetail({ ...selected, status: "cancelled", updated_at: Math.floor(Date.now() / 1000) });
      await refreshTasks();
    } catch (error) {
      setErr(errMsg(error));
    }
  };

  const openTaskConversation = async () => {
    const selected = taskDetail;
    if (!selected || selected.id === taskId || running) return;
    await loadConversation({
      id: selected.conversation_id,
      title: selected.prompt?.slice(0, 40) ?? "智能助手任务",
      created_at: selected.created_at,
    });
    setTaskDetail(null);
  };

  const chips = [
    currentSymbol ? `全面分析当前股票 ${currentSymbol}` : "全面分析当前股票",
    "为什么最近跑输行业",
    "扫描今日强势信号股",
    "我的自选股有风险吗",
    currentSymbol ? `迭代 ${currentSymbol} 的策略并做稳健性检验` : "迭代一个策略并做稳健性检验",
  ];

  const suspendedMsg = [...msgs].reverse().find((m) => m.role === "assistant" && m.suspendedAt);

  const historyPanel = (
    <aside
      className={
        "anim-fade-in flex shrink-0 flex-col border-slate-200 bg-white dark:border-slate-800 dark:bg-slate-900 " +
        (variant === "page"
          ? "w-64 border-r"
          : "absolute inset-y-0 left-0 z-30 w-60 border-r shadow-xl")
      }
    >
      <div className="card-title">
        历史会话
        <button className="btn-primary ml-auto !px-2 !py-0.5" onClick={newChat}>
          新对话
        </button>
        <button className="muted text-xs font-normal" onClick={() => setHistoryOpen(false)}>
          收起
        </button>
      </div>
      <div className="min-h-0 flex-1 space-y-3 overflow-y-auto p-2">
        <div>
          <div className="micro-label px-1 pb-1">会话</div>
          {convs.length === 0 ? (
            <div className="muted px-1 text-xs">暂无历史会话</div>
          ) : (
            convs.map((c) => (
              <div
                key={c.id}
                className="group flex items-center gap-1 rounded px-2 py-1.5 text-xs hover:bg-slate-100 dark:hover:bg-slate-800"
              >
                <button className="min-w-0 flex-1 text-left" onClick={() => loadConversation(c)}>
                  <div className="truncate">{c.title || "未命名会话"}</div>
                  <div className="num muted mt-0.5">{fmtUnix(c.created_at)}</div>
                </button>
                <button
                  className="muted shrink-0 rounded px-1 opacity-0 transition-opacity hover:text-red-500 group-hover:opacity-100"
                  title="删除该会话"
                  onClick={() => deleteConversation(c)}
                >
                  ✕
                </button>
              </div>
            ))
          )}
        </div>
        <div>
          <div className="micro-label px-1 pb-1">任务</div>
          {tasks.length === 0 ? (
            <div className="muted px-1 text-xs">暂无任务记录</div>
          ) : (
            tasks.map((t) => (
              <button
                type="button"
                key={t.id}
                className="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-xs hover:bg-slate-100 dark:hover:bg-slate-800"
                title="点击查看任务详情和诊断信息"
                onClick={() => setTaskDetail(t)}
              >
                <StatusBadge status={t.status} />
                <span className="num muted truncate">{fmtUnix(t.created_at)}</span>
                <span className="muted ml-auto shrink-0 underline decoration-dotted underline-offset-2">详情</span>
                {(t.status === "suspended" || t.status === "interrupted") && (
                  <span
                    className="btn shrink-0"
                    role="button"
                    tabIndex={0}
                    onClick={(event) => { event.stopPropagation(); resumeTask(t); }}
                    onKeyDown={(event) => {
                      if (event.key === "Enter" || event.key === " ") {
                        event.preventDefault();
                        event.stopPropagation();
                        resumeTask(t);
                      }
                    }}
                  >
                    继续
                  </span>
                )}
              </button>
            ))
          )}
        </div>
      </div>
    </aside>
  );

  return (
    <div className="relative flex h-full min-h-0 overflow-hidden">
      {historyOpen && historyPanel}
      {taskDetail && (
        <TaskDetailDialog
          task={taskDetail}
          tools={taskDetail.id === taskId ? msgs.flatMap((message) => message.tools) : []}
          progress={taskDetail.id === taskId ? progress : null}
          isCurrent={taskDetail.id === taskId}
          canOpenConversation={taskDetail.id !== taskId && !running}
          onClose={() => setTaskDetail(null)}
          onCancel={() => void cancelTaskFromDetail()}
          onResume={() => { resumeTask(taskDetail); setTaskDetail(null); }}
          onOpenConversation={() => void openTaskConversation()}
        />
      )}

      {/* 对话主区 */}
      <section className="flex min-w-0 flex-1 flex-col">
        <div className="flex items-center gap-3 border-b border-slate-200 px-4 py-2.5 dark:border-slate-800">
          <h1 className="text-sm font-semibold text-slate-700 dark:text-slate-300">智能助手</h1>
          <span className="muted text-xs">
            <Term label="智能投研" tip="调用行情与分析能力取得数据后生成解读，过程与证据均可追溯" />
          </span>
          {currentSymbol && (
            <span className="chip bg-blue-600/10 text-blue-600 dark:text-blue-400">
              上下文:{currentName ?? currentSymbol}
            </span>
          )}
          <span
            className="chip bg-violet-500/10 text-violet-600 dark:text-violet-400"
            title="超出预算时会将旧上下文压缩成确定性的工作状态快照，保留目标、证据键、完成工具与最近消息"
          >
            自动上下文压缩{compactionCount ? ` · ${compactionCount}` : ""}
          </span>
          <span className="chip bg-blue-600/10 text-blue-600 dark:text-blue-400">
            {RESEARCH_MODES.find((mode) => mode.id === researchMode)?.label ?? "深度"}模式
          </span>
          <span className="chip bg-amber-500/10 text-amber-700 dark:text-amber-300">
            {REASONING_DEPTHS.find((depth) => depth.id === reasoningDepth)?.label ?? "深入"}思考
          </span>
          {taskId && status !== "idle" && (
            <button
              type="button"
              className="chip num bg-slate-100 text-slate-500 hover:ring-1 hover:ring-blue-400 dark:bg-slate-800 dark:text-slate-300"
              title="点击查看当前后台任务详情"
              onClick={showCurrentTaskDetail}
            >
              {{
                running: "后台任务运行中",
                waiting_input: "等待你确认研究条件",
                suspended: "后台任务待恢复",
                completed: "本轮分析已完成",
                failed: "本轮分析未完成",
                cancelled: "本轮分析已取消",
              }[status] ?? "后台任务"}
              <span className="ml-1 font-sans underline decoration-dotted underline-offset-2">详情</span>
            </button>
          )}
          <div className="ml-auto flex items-center gap-2">
            <button className="btn" onClick={() => setSettingsOpen((open) => !open)}>
              研究设置
            </button>
            <button className="btn-primary" onClick={newChat} disabled={running}>
              新对话
            </button>
            {!historyOpen && (
              <button className="btn" onClick={() => setHistoryOpen(true)}>
                历史
              </button>
            )}
            {onClose && (
              <button className="btn" title="关闭" onClick={onClose}>
                ✕
              </button>
            )}
          </div>
        </div>

        {settingsOpen && (
          <div className="border-b border-slate-200 bg-slate-50/80 px-4 py-3 text-xs dark:border-slate-800 dark:bg-slate-900/70">
            <div className="grid gap-4 xl:grid-cols-[1fr_0.75fr_2fr]">
              <fieldset disabled={running}>
                <legend className="micro-label mb-1.5">研究流程</legend>
                <div className="grid gap-1.5">
                  {RESEARCH_MODES.map((mode) => (
                    <button
                      key={mode.id}
                      type="button"
                      className={
                        "rounded border px-2.5 py-2 text-left transition-colors " +
                        (researchMode === mode.id
                          ? "border-blue-500 bg-blue-50 text-blue-700 dark:bg-blue-950/40 dark:text-blue-300"
                          : "border-slate-200 bg-white hover:border-slate-300 dark:border-slate-700 dark:bg-slate-900")
                      }
                      onClick={() => setResearchMode(mode.id)}
                    >
                      <span className="font-medium">{mode.label}模式</span>
                      <span className="muted ml-2">{mode.detail}</span>
                    </button>
                  ))}
                </div>
              </fieldset>

              <fieldset disabled={running}>
                <legend className="micro-label mb-1.5">思考深度</legend>
                <div className="flex gap-1.5">
                  {REASONING_DEPTHS.map((depth) => (
                    <button
                      key={depth.id}
                      type="button"
                      className={reasoningDepth === depth.id ? "btn-primary flex-1" : "btn flex-1"}
                      onClick={() => setReasoningDepth(depth.id)}
                    >
                      {depth.label}
                    </button>
                  ))}
                </div>
                <p className="muted mt-2 leading-relaxed">
                  极深模式会提高证据核验、反例、压力情景和稳健性检查上限，耗时与额度也会增加。
                </p>
                <label className="mt-3 flex cursor-pointer items-start gap-2 rounded border border-slate-200 bg-white p-2 dark:border-slate-700 dark:bg-slate-900">
                  <input
                    className="mt-0.5"
                    type="checkbox"
                    checked={autoResumeOnQuota}
                    onChange={(event) => setAutoResumeOnQuota(event.target.checked)}
                  />
                  <span>
                    <span className="block font-medium">额度恢复后自动继续</span>
                    <span className="muted mt-0.5 block leading-relaxed">
                      保存当前计划、证据和步骤，等待下一额度窗口后从断点续跑。
                    </span>
                  </span>
                </label>
              </fieldset>

              <fieldset disabled={running}>
                <div className="mb-1.5 flex items-center gap-2">
                  <legend className="micro-label">本轮可用工具</legend>
                  <span className="num muted">{enabledTools.length} / {DEFAULT_AGENT_TOOLS.length}</span>
                  <button className="btn ml-auto" type="button" onClick={() => setEnabledTools([...DEFAULT_AGENT_TOOLS])}>
                    全部开启
                  </button>
                  <button className="btn" type="button" onClick={() => setEnabledTools([])}>
                    全部关闭
                  </button>
                </div>
                <div className="grid gap-2 md:grid-cols-2 xl:grid-cols-3">
                  {TOOL_GROUPS.map((group) => {
                    const allOn = group.tools.every((tool) => enabledTools.includes(tool));
                    return (
                      <div key={group.label} className="rounded border border-slate-200 bg-white p-2 dark:border-slate-700 dark:bg-slate-900">
                        <label className="mb-1.5 flex cursor-pointer items-center gap-2 font-medium">
                          <input
                            type="checkbox"
                            checked={allOn}
                            onChange={(event) => {
                              const groupSet = new Set<string>(group.tools);
                              setEnabledTools(
                                event.target.checked
                                  ? [...new Set([...enabledTools, ...group.tools])]
                                  : enabledTools.filter((tool) => !groupSet.has(tool)),
                              );
                            }}
                          />
                          {group.label}
                        </label>
                        <div className="grid gap-1">
                          {group.tools.map((tool) => (
                            <label key={tool} className="muted flex cursor-pointer items-center gap-1.5">
                              <input
                                type="checkbox"
                                checked={enabledTools.includes(tool)}
                                onChange={(event) =>
                                  setEnabledTools(
                                    event.target.checked
                                      ? [...new Set([...enabledTools, tool])]
                                      : enabledTools.filter((item) => item !== tool),
                                  )
                                }
                              />
                              {toolDisplayName(tool)}
                            </label>
                          ))}
                        </div>
                      </div>
                    );
                  })}
                </div>
              </fieldset>
            </div>
            {running && <div className="muted mt-2">当前后台任务已锁定本轮设置；新设置会用于下一轮提问。</div>}
          </div>
        )}

        {hasKey === false && (
          <div className="flex items-center gap-2 border-b border-amber-200 bg-amber-50 px-4 py-2 text-xs text-amber-800 dark:border-amber-900/60 dark:bg-amber-950/40 dark:text-amber-300">
            <span>尚未配置 MiniMax 访问密钥，智能助手暂不可用。</span>
            <Link to="/settings" className="font-medium underline underline-offset-2">
              前往设置页配置
            </Link>
          </div>
        )}
        {!isTauri() && (
          <div className="border-b border-red-200 bg-red-50 px-4 py-2 text-xs text-red-700 dark:border-red-900 dark:bg-red-950/40 dark:text-red-300">
            {NOT_TAURI_MSG}
          </div>
        )}

        {/* 消息区 */}
        <div ref={scrollRef} className="min-h-0 flex-1 space-y-3 overflow-y-auto p-4">
          {running && progress && <ResearchProgress progress={progress} />}
          {msgs.length === 0 && (
            <div className="muted py-10 text-center text-sm">
              向智能助手提问，或点击下方快捷模板开始
            </div>
          )}
          {msgs.map((m) =>
            m.role === "user" ? (
              <div key={m.key} className="anim-fade-up flex justify-end">
                <div className="max-w-2xl whitespace-pre-wrap rounded bg-blue-600 px-3 py-2 text-sm text-white">
                  {m.raw}
                </div>
              </div>
            ) : (
              <AssistantMsg
                key={m.key}
                msg={m}
                onClarificationSubmit={(request, draft) =>
                  submitClarification(m.key, request, draft)
                }
              />
            ),
          )}

          {status === "suspended" && (
            <div className="rounded border border-amber-300 bg-amber-50 px-3 py-2.5 text-sm text-amber-800 dark:border-amber-900/60 dark:bg-amber-950/40 dark:text-amber-300">
              <div className="flex flex-wrap items-center gap-3">
                <span>
                  任务已保存，可从中断点继续。
                  {suspendedMsg?.suspendedAt && (
                    <>
                      建议恢复时间:
                      <span className="num font-medium">{fmtUnix(suspendedMsg.suspendedAt)}</span>。
                    </>
                  )}
                  系统会自动补齐应用退出时未返回的分析结果，避免工具链不匹配错误（错误码 2013）。
                  {autoResumeOnQuota && " 已开启额度恢复后自动续跑；也可立即手动尝试。"}
                </span>
                <button className="btn-primary" onClick={resume}>
                  继续分析
                </button>
              </div>
            </div>
          )}
          {err && (
            <div className="rounded border border-red-300 bg-red-50 px-3 py-2 text-xs text-red-700 dark:border-red-900 dark:bg-red-950/40 dark:text-red-300">
              {err}
            </div>
          )}
        </div>

        {/* 输入区 */}
        <div className="shrink-0 border-t border-slate-200 p-3 dark:border-slate-800">
          {pendingQuestions.length > 0 && (
            <div className="mb-2 flex flex-wrap items-center gap-1.5 text-xs">
              <span className="muted">等待继续分析：</span>
              {pendingQuestions.map((question, index) => (
                <span key={`${question}-${index}`} className="tag max-w-72 truncate" title={question}>
                  {index + 1}. {question}
                </span>
              ))}
            </div>
          )}
          <div className="mb-2 flex flex-wrap gap-1.5">
            {chips.map((c) => (
              <button
                key={c}
                className="tag bg-slate-200 text-slate-600 transition-colors hover:bg-slate-300 dark:bg-slate-800 dark:text-slate-300 dark:hover:bg-slate-700"
                disabled={waitingForInput}
                onClick={() => setInput(c)}
              >
                {c}
              </button>
            ))}
          </div>
          <div className="flex items-end gap-2">
            <textarea
              className="input max-h-32 min-h-[38px] flex-1 resize-y"
              placeholder={
                waitingForInput
                  ? "请先完成上方选择并点击“提交选择并继续分析”"
                  : "输入问题,Enter 发送(Shift+Enter 换行)"
              }
              value={input}
              disabled={hasKey === false || waitingForInput}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  send(input);
                }
              }}
            />
            {waitingForInput ? (
              <button className="btn-primary" disabled>
                等待提交选择
              </button>
            ) : running ? (
              <>
                <button className="btn-primary" disabled={!input.trim()} onClick={() => queueFollowUp(input)}>
                  加入追问
                </button>
                <button className="btn-danger" onClick={cancel}>
                  取消当前分析
                </button>
              </>
            ) : (
              <button
                className="btn-primary"
                disabled={!input.trim() || hasKey === false}
                onClick={() => send(input)}
              >
                发送
              </button>
            )}
          </div>
        </div>
      </section>
    </div>
  );
}

/** 历史消息(role/content JSON)转换为气泡:工具消息折叠进上一条助手消息的轨迹 */
export function historyToMsgs(m: AgentMessage, out: ChatMsg[]): ChatMsg[] {
  if (m.role === "system") return [];
  if (m.role === "user") {
    const last = out[out.length - 1];
    if (last?.role === "assistant" && hasClarification(last.raw)) {
      last.clarificationSubmitted = true;
    }
    return [{ key: nextAgentKey(), role: "user", raw: m.content, tools: [], done: true }];
  }
  if (m.role === "assistant") {
    const tools = m.tool_calls.map((call) => ({
      key: nextAgentKey(),
      callId: call.id ?? undefined,
      name: call.name || "tool",
      args: call.arguments ?? undefined,
      done: false,
      startedAt: m.created_at * 1000,
      stage: "已从历史记录恢复工具调用",
      timeline: [
        {
          at: m.created_at * 1000,
          kind: "started" as const,
          message: "工具调用已保存",
        },
      ],
    }));
    return [
      {
        key: nextAgentKey(),
        role: "assistant",
        raw: m.content,
        tools,
        failed: m.malformed ? "该历史消息格式异常，已按纯文本安全加载。" : undefined,
        clarificationDraft: hasClarification(m.content) ? emptyClarificationDraft() : undefined,
        done: true,
      },
    ];
  }
  // tool 等其他角色:折叠为上一条助手消息的工具轨迹
  const last = out[out.length - 1];
  if (last && last.role === "assistant") {
    let name = "tool";
    let cacheKey: string | undefined;
    let source: string | undefined;
    let fetchedAt: string | undefined;
    let error: string | undefined;
    try {
      const j = JSON.parse(m.content) as Record<string, unknown>;
      if (typeof j.name === "string") name = j.name;
      else if (typeof j.tool === "string") name = j.tool;
      if (typeof j.cache_key === "string") cacheKey = j.cache_key;
      if (typeof j.source === "string") source = j.source;
      if (typeof j.fetched_at === "string") fetchedAt = j.fetched_at;
      if (typeof j.error === "string") error = j.error;
    } catch {
      /* content 非 JSON 时保持默认名 */
    }
    const matchedIndex = last.tools.findIndex(
      (tool) => !tool.done && m.tool_call_id != null && tool.callId === m.tool_call_id,
    );
    const finishedAt = m.created_at * 1000;
    const finished = {
      callId: m.tool_call_id ?? undefined,
      name,
      done: true,
      success: error == null,
      cacheKey,
      source,
      fetchedAt,
      error,
      stage: error == null ? "历史工具结果已加载" : "历史工具执行失败",
    };
    if (matchedIndex >= 0) {
      const existing = last.tools[matchedIndex];
      last.tools = last.tools.map((tool, index) =>
        index === matchedIndex
          ? {
              ...existing,
              ...finished,
              name: existing.name || name,
              timeline: [
                ...(existing.timeline ?? []),
                {
                  at: finishedAt,
                  kind: error == null ? ("success" as const) : ("error" as const),
                  message: error ?? "工具结果已保存",
                },
              ],
            }
          : tool,
      );
    } else {
      last.tools = [
        ...last.tools,
        {
          key: nextAgentKey(),
          args: m.content,
          ...finished,
          timeline: [
            {
              at: finishedAt,
              kind: error == null ? ("success" as const) : ("error" as const),
              message: error ?? "历史工具结果已保存",
            },
          ],
        },
      ];
    }
    return [];
  }
  return [];
}

const PROGRESS_PHASE_LABELS: Record<string, string> = {
  preparing: "准备任务",
  reasoning: "理解与规划",
  tools: "取数与计算",
  reviewing: "多专家复核",
  synthesizing: "图表与结论",
};

const PROGRESS_PHASE_ORDER = ["preparing", "reasoning", "tools", "reviewing", "synthesizing"];
