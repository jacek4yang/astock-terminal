import { useEffect, useMemo, useRef, useState } from "react";
import type { AgentPhase, ConversationSummary, TaskSpec } from "../bridge/generated";
import { isProton, requestNative, subscribeNativeEvent } from "../bridge";
import type { ClarificationDraft, ClarificationQuestion, ClarificationRequest } from "../lib/agentClarification";
import { emptyClarificationDraft } from "../lib/agentClarification";
import { createEventBatcher } from "../lib/eventBatcher";
import { consumeAgentDraft, subscribeAgentDraft } from "./agentDraft";
import { sanitizeAgentVisibleText } from "./agentVisibleText";
import { useResearchContext } from "./store";

type Depth = "fast" | "balanced" | "deep" | "exhaustive";
type ToolPolicy = "auto" | "market" | "evidence" | "full";
type MessageRole = "user" | "agent" | "system" | "tool";

type AgentMessage = { id: string; role: MessageRole; text: string; timestamp: string };
type TaskView = {
  task_id?: string;
  phase?: AgentPhase;
  accepted_seq?: number;
  missing_fields?: string[];
  pending_tool_count?: number;
  completed_tool_count?: number;
  evidence_ids?: string[];
  clarification?: ClarificationRequest | null;
  cache_requests?: number;
  cache_hits?: number;
  spec?: TaskSpec | null;
};
type AgentEffect = {
  kind?: string;
  fields?: string[];
  tool?: string;
  call_id?: string;
  title?: string;
  detail?: string;
  cache_hit?: boolean;
  evidence_count?: number;
  analysis_modules?: string[];
  module_activities?: AgentModuleActivity[];
  status?: "succeeded" | "failed" | "skipped";
};
type AgentModuleActivity = {
  module?: string;
  scope?: string;
  status?: "succeeded" | "failed" | "skipped";
  error?: string | null;
};
export type DeterministicVerification = {
  version?: string;
  distinct_citations?: number;
  numeric_claims_checked?: number;
  registry_facts?: number;
};
type TaskTransition = {
  accepted?: boolean;
  rejection?: string | null;
  state?: TaskView;
  effects?: AgentEffect[];
  activities?: AgentEffect[];
  clarification?: ClarificationRequest | null;
  checkpoint?: unknown;
  report?: string | null;
  verification_findings?: string[];
  verification?: DeterministicVerification;
};

type AgentSession = {
  sessionId?: string;
  title?: string;
  createdAt?: number;
  updatedAt?: number;
  input?: string;
  depth?: Depth;
  toolPolicy?: ToolPolicy;
  messages?: AgentMessage[];
  task?: TaskView | null;
  effects?: AgentEffect[];
  clarification?: ClarificationRequest | null;
  draft?: ClarificationDraft;
  checkpoint?: unknown;
  verification?: DeterministicVerification | null;
};

type StoredConversation = ConversationSummary & { session: AgentSession };

type DurableTask = {
  task: { accepted_seq: number; checkpoint?: unknown };
  events: Array<{ seq: number }>;
};

type WorkerProgress = {
  request_id?: string;
  round?: number;
  stage?: "checkpointed" | "tool_finished" | string;
  state?: TaskView | null;
  activities?: AgentEffect[];
  verification_findings?: string[];
  tool?: { call_id?: string; kind?: string } | null;
  tool_result?: { call_id?: string; ok?: boolean; cache_hit?: boolean } | null;
};

const MAX_HISTORY_ITEMS = 80;
const AGENT_RENDER_BATCH_MS = 110;

export function workerProgressMatchesTask(progress: WorkerProgress, taskId?: string) {
  return !progress.state?.task_id || Boolean(taskId && progress.state.task_id === taskId);
}

const AGENT_BEST = "__agent_best__";
const phaseLabel: Record<AgentPhase, string> = {
  idle: "就绪", preparing: "准备研究", waiting_for_user: "等待你的选择", reasoning: "分析中",
  awaiting_tools: "调用工具", reviewing: "核验证据", synthesizing: "撰写结论", verifying: "校验报告",
  suspended: "已暂停", completed: "已完成", verification_failed: "校验未通过", cancelled: "已取消", hard_failed: "执行失败",
};
const depthLabel: Record<Depth, string> = { fast: "快速", balanced: "标准", deep: "深入", exhaustive: "极深" };
const toolLabel: Record<ToolPolicy, string> = { auto: "自动选择工具", market: "仅行情与技术面", evidence: "行情 + 资料核验", full: "全部研究工具" };
const effectCopy: Record<string, [string, string]> = {
  persist_checkpoint: ["保存任务检查点", "任务事件与状态已进入可恢复队列"],
  request_clarification: ["生成澄清问题", "由模型根据目标和缺失边界动态提问"],
  ask_for_clarification: ["生成澄清问题", "正在请求模型生成与当前目标相关的问题"],
  prepare_research: ["建立研究计划", "整理证券范围、资料时点、工具与验证要求"],
  request_model: ["分析证据与下一步", "模型只接收可公开的任务上下文，不展示私有推理链"],
  review_evidence: ["核验证据", "检查来源、时点、冲突和结论覆盖情况"],
  synthesize_report: ["形成研究结论", "将工具结果整理为可审阅的研究报告"],
  verify_report: ["发布前校验", "逐行复现数字，并检查证据编号、来源时点、版本、质量状态和研究边界"],
  publish_report: ["发布报告", "报告已通过校验并保存"],
};
const moduleLabel: Record<string, string> = {
  earnings_driver: "盈利驱动树",
  industry_graph: "产业关系图谱",
  relationship: "跨证券关系",
  market_regime: "市场状态识别",
  historical_backtest: "历史回测",
};

export function expandAgentActivities(activities: AgentEffect[] = []): AgentEffect[] {
  return activities.flatMap((effect) => {
    const modules = Array.isArray(effect.module_activities) ? effect.module_activities : [];
    const expanded = modules.map((activity) => {
      const module = activity.module ?? "unknown";
      const status = activity.status ?? "failed";
      const scope = activity.scope === "portfolio" ? "组合" : activity.scope ?? "范围未知";
      const statusLabel = status === "succeeded" ? "完成" : status === "skipped" ? "已跳过" : "失败";
      const failure = activity.error ? ` · ${activity.error}` : "";
      return {
        kind: "advanced_module",
        tool: module,
        call_id: `${effect.call_id ?? "advanced"}:${module}:${activity.scope ?? "unknown"}`,
        title: `${moduleLabel[module] ?? module} ${statusLabel}`,
        detail: `${scope} · 高级研究模块${statusLabel}${failure}`,
        status,
      } satisfies AgentEffect;
    });
    return [effect, ...expanded];
  });
}

function timeNow(): string {
  return new Intl.DateTimeFormat("zh-CN", { hour: "2-digit", minute: "2-digit" }).format(new Date());
}

function questionComplete(question: ClarificationQuestion, draft: ClarificationDraft): boolean {
  return Boolean(draft.selections[question.id]?.length || draft.other[question.id]?.trim());
}

function withAgentChoice(question: ClarificationQuestion) {
  return [...question.options, {
    id: AGENT_BEST,
    label: "由 Agent 选择最优项",
    description: "你明确授权后，Agent 才会基于已取得的证据作出选择，并在报告中记录依据。",
    recommended: false,
  }];
}

function normalizeClarification(value: ClarificationRequest): ClarificationRequest {
  return {
    ...value,
    description: value.description ?? undefined,
    questions: value.questions.map((question) => {
      const wire = question as ClarificationQuestion & { allow_other?: boolean };
      return {
        ...question,
        header: question.header ?? undefined,
        allowOther: question.allowOther ?? wire.allow_other ?? false,
        options: question.options.map((option) => ({ ...option, description: option.description ?? undefined })),
      };
    }),
  };
}

function sessionTitle(messages: AgentMessage[], fallback = "新的投资研究"): string {
  const objective = messages.find((message) => message.role === "user")?.text.trim() ?? "";
  if (!objective) return fallback;
  return objective.length > 38 ? `${objective.slice(0, 38)}…` : objective;
}

function historyTime(value?: number): string {
  if (!value) return "时间未知";
  const milliseconds = value < 10_000_000_000 ? value * 1000 : value;
  return new Intl.DateTimeFormat("zh-CN", {
    month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false,
  }).format(new Date(milliseconds));
}

function capitalFromObjective(objective = ""): number | null {
  const compact = objective.replace(/[，,\s]/g, "");
  const wan = compact.match(/(\d+(?:\.\d+)?)万(?:元)?/);
  if (wan) return Number(wan[1]) * 10_000;
  const yuan = compact.match(/(?:资金|本金|预算|投入)?(?:为|是|约)?(\d{4,9})(?:元|块)/);
  return yuan ? Number(yuan[1]) : null;
}

/**
 * Stateful Agent operations are one public bridge call. The Proton Host (and
 * browser acceptance Bridge) owns event/effect/checkpoint durability; the
 * renderer cannot write those internal Engine records or claim a result.
 */
export async function requestDurableAgent<T>(
  kind: "agent.start" | "agent.event" | "agent.research.workflow",
  payload: Record<string, unknown>,
  deadlineMs: number,
): Promise<T> {
  return requestNative<T>("agent", kind, payload, { deadlineMs });
}

export function deterministicVerificationSummary(verification: DeterministicVerification): string {
  const claims = Math.max(0, verification.numeric_claims_checked ?? 0);
  const citations = Math.max(0, verification.distinct_citations ?? 0);
  const facts = Math.max(0, verification.registry_facts ?? 0);
  return `复现 ${claims} 个数字 · ${citations} 个不同证据引用 · ${facts} 条字段事实`;
}

export function durableCheckpointState(durable: DurableTask, expectedTaskId: string): TaskView {
  const acceptedSeq = durable.task.accepted_seq;
  const checkpoint = durable.task.checkpoint;
  if (!Number.isInteger(acceptedSeq) || acceptedSeq < 1) throw new Error("持久化任务序列无效");
  if (!checkpoint || typeof checkpoint !== "object" || Array.isArray(checkpoint)) throw new Error("持久化任务没有可恢复检查点");
  const state = checkpoint as TaskView;
  if (state.task_id !== expectedTaskId) throw new Error("持久化检查点与任务不匹配");
  if (state.accepted_seq !== acceptedSeq) throw new Error("持久化检查点序列与任务日志不一致");
  return state;
}

export default function AgentTaskWorkbench() {
  const [sessionId, setSessionId] = useState<string>(() => crypto.randomUUID());
  const [title, setTitle] = useState("");
  const [createdAt, setCreatedAt] = useState(Date.now);
  const [history, setHistory] = useState<ConversationSummary[]>([]);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [historyQuery, setHistoryQuery] = useState("");
  const [historySearchResults, setHistorySearchResults] = useState<ConversationSummary[] | null>(null);
  const [historySearching, setHistorySearching] = useState(false);
  const [hydrated, setHydrated] = useState(false);
  const contextSymbol = useResearchContext((state) => state.symbol);
  const [input, setInput] = useState("");
  const [depth, setDepth] = useState<Depth>("deep");
  const [toolPolicy, setToolPolicy] = useState<ToolPolicy>("auto");
  const [messages, setMessages] = useState<AgentMessage[]>([]);
  const [task, setTask] = useState<TaskView | null>(null);
  const [effects, setEffects] = useState<AgentEffect[]>([]);
  const [clarification, setClarification] = useState<ClarificationRequest | null>(null);
  const [draft, setDraft] = useState<ClarificationDraft>(emptyClarificationDraft);
  const [checkpoint, setCheckpoint] = useState<unknown>();
  const [durableTaskReady, setDurableTaskReady] = useState(true);
  const [verification, setVerification] = useState<DeterministicVerification | null>(null);
  const [busy, setBusy] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [busyStage, setBusyStage] = useState("");
  const [error, setError] = useState<string | null>(null);
  const composerRef = useRef<HTMLTextAreaElement | null>(null);
  const restoredTaskRef = useRef<string | null>(null);
  const cancelRequestedTaskRef = useRef<string | null>(null);

  useEffect(() => {
    const applyDraft = (prompt: string) => {
      consumeAgentDraft();
      setInput(prompt);
      setError(null);
      requestAnimationFrame(() => composerRef.current?.focus());
    };
    const pending = consumeAgentDraft();
    if (pending) applyDraft(pending);
    return subscribeAgentDraft(applyDraft);
  }, []);

  useEffect(() => {
    if (!isProton()) {
      setHydrated(true);
      return;
    }
    let cancelled = false;
    void (async () => {
      try {
        const result = await requestNative<{ items: ConversationSummary[] }>("engine", "agent.conversation.list", { limit: MAX_HISTORY_ITEMS });
        if (cancelled) return;
        const items = result.items ?? [];
        setHistory(items);
        const latest = items[0];
        if (latest) {
          const stored = await requestNative<StoredConversation>("engine", "agent.conversation.load", { conversation_id: latest.conversation_id });
          if (cancelled) return;
          restoreSession(stored.session, stored.title);
        }
      } catch (cause) {
        if (!cancelled) setError(`读取 Agent 历史失败：${cause instanceof Error ? cause.message : String(cause)}`);
      } finally {
        if (!cancelled) setHydrated(true);
      }
    })();
    return () => { cancelled = true; };
    // 首次挂载只从 Engine 恢复一次；restoreSession 使用当前组件的稳定 setter。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!isProton()) return;
    const activeTaskId = task?.task_id;
    const batcher = createEventBatcher<WorkerProgress>(AGENT_RENDER_BATCH_MS, (batch) => {
      if (!batch.length) return;
      const relevant = batch.filter((item) => workerProgressMatchesTask(item, activeTaskId));
      if (!relevant.length) return;
      const latestState = [...relevant].reverse().find((item) => item.state)?.state;
      if (latestState) {
        setTask((current) => ({ ...(current ?? {}), ...latestState }));
        if (latestState.phase) setBusyStage(phaseLabel[latestState.phase]);
      }
      const streamed = relevant.flatMap((item) => {
        const activities = Array.isArray(item.activities) ? item.activities : [];
        const tool = item.stage === "tool_finished" && item.tool
          ? [{
              kind: "execute_tool",
              tool: item.tool.kind,
              call_id: item.tool.call_id,
              cache_hit: item.tool_result?.cache_hit ?? false,
              title: item.tool_result?.ok === false ? "工具执行失败" : "工具执行完成",
              detail: item.tool_result?.ok === false
                ? "工具结果已安全持久化；任务不会基于失败结果发布结论"
                : "工具结果与缓存状态已持久化，正在进入下一轮复核",
            } satisfies AgentEffect]
          : [];
        return expandAgentActivities([...activities, ...tool]);
      });
      if (streamed.length) {
        setEffects((current) => {
          const merged = [...current];
          for (const item of streamed) {
            const key = `${item.kind ?? "activity"}:${item.call_id ?? item.tool ?? item.title ?? item.detail ?? ""}`;
            const index = merged.findIndex((existing) =>
              `${existing.kind ?? "activity"}:${existing.call_id ?? existing.tool ?? existing.title ?? existing.detail ?? ""}` === key);
            if (index >= 0) merged[index] = { ...merged[index], ...item };
            else merged.push(item);
          }
          return merged.slice(-80);
        });
      }
      window.dispatchEvent(new CustomEvent("astock:agent-render-batch", {
        detail: { at: performance.now(), event_count: relevant.length },
      }));
    });
    const enqueue = (raw: unknown) => {
      if (!raw || typeof raw !== "object") return;
      batcher.push(raw as WorkerProgress);
    };
    const unsubscribe = subscribeNativeEvent("worker_event", enqueue);
    return () => {
      unsubscribe();
      batcher.dispose();
    };
  }, [task?.task_id]);

  useEffect(() => {
    if (!hydrated || !isProton() || !messages.length) return;
    const updatedAt = Date.now();
    const resolvedTitle = title || sessionTitle(messages);
    const session: AgentSession = { sessionId, title: resolvedTitle, createdAt, updatedAt, input, depth, toolPolicy, messages, task, effects, clarification, draft, checkpoint, verification };
    const timer = window.setTimeout(() => {
      void requestNative<StoredConversation>("engine", "agent.conversation.save", {
        conversation_id: sessionId,
        title: resolvedTitle,
        session,
      }).then((stored) => {
        setHistory((current) => [{
          conversation_id: stored.conversation_id,
          title: stored.title,
          phase: stored.session.task?.phase ?? "idle",
          message_count: stored.session.messages?.length ?? 0,
          evidence_count: stored.session.task?.evidence_ids?.length ?? 0,
          parent_conversation_id: stored.parent_conversation_id,
          branch_from_message_id: stored.branch_from_message_id,
          created_at: stored.created_at,
          updated_at: stored.updated_at,
        }, ...current.filter((item) => item.conversation_id !== stored.conversation_id)].slice(0, MAX_HISTORY_ITEMS));
      }).catch((cause) => setError(`保存 Agent 会话失败：${cause instanceof Error ? cause.message : String(cause)}`));
    }, 450);
    return () => window.clearTimeout(timer);
  }, [checkpoint, clarification, createdAt, depth, draft, effects, hydrated, input, messages, sessionId, task, title, toolPolicy, verification]);

  useEffect(() => {
    if (!historyOpen) return;
    const close = (event: KeyboardEvent) => { if (event.key === "Escape") setHistoryOpen(false); };
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [historyOpen]);

  useEffect(() => {
    if (!historyOpen || !isProton()) return;
    const query = historyQuery.trim();
    if (!query) {
      setHistorySearchResults(null);
      setHistorySearching(false);
      return;
    }
    let cancelled = false;
    setHistorySearchResults(null);
    setHistorySearching(true);
    const timer = window.setTimeout(() => {
      void requestNative<{ items: ConversationSummary[] }>("engine", "agent.conversation.list", {
        limit: MAX_HISTORY_ITEMS,
        query,
      }).then((result) => {
        if (!cancelled) setHistorySearchResults(result.items ?? []);
      }).catch((cause) => {
        if (!cancelled) setError(`搜索 Agent 历史失败：${cause instanceof Error ? cause.message : String(cause)}`);
      }).finally(() => {
        if (!cancelled) setHistorySearching(false);
      });
    }, 220);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [historyOpen, historyQuery]);

  const recoverLatestCheckpoint = async (taskId: string) => {
    setDurableTaskReady(false);
    const durable = await requestNative<DurableTask>("engine", "agent.task.load", { task_id: taskId });
    const recovered = durableCheckpointState(durable, taskId);
    setCheckpoint(durable.task.checkpoint);
    setTask(recovered);
    setDurableTaskReady(true);
    return recovered;
  };

  useEffect(() => {
    if (!isProton() || !task?.task_id || restoredTaskRef.current === task.task_id) return;
    restoredTaskRef.current = task.task_id;
    void recoverLatestCheckpoint(task.task_id).catch((cause) => {
      restoredTaskRef.current = null;
      setError(`恢复 Agent 任务失败：${cause instanceof Error ? cause.message : String(cause)}`);
    });
  }, [task?.task_id]);

  const cacheRequests = task?.cache_requests ?? 0;
  const cacheHits = task?.cache_hits ?? 0;
  const completedClarification = useMemo(() => clarification?.questions.every((question) => questionComplete(question, draft)) ?? false, [clarification, draft]);

  const append = (role: MessageRole, text: string) => {
    const visible = role === "user" ? text : sanitizeAgentVisibleText(text);
    if (!visible.trim()) return;
    setMessages((current) => [
      ...current,
      { id: crypto.randomUUID(), role, text: visible, timestamp: timeNow() },
    ]);
  };

  const applyTransition = (reply: TaskTransition) => {
    const next = reply.state ?? {};
    restoredTaskRef.current = next.task_id ?? null;
    setDurableTaskReady(true);
    setTask(next);
    setEffects(expandAgentActivities(reply.activities ?? reply.effects ?? []));
    if (reply.verification) setVerification(reply.verification);
    if (reply.checkpoint !== undefined) setCheckpoint(reply.checkpoint);
    const generated = reply.clarification ?? next.clarification ?? null;
    if (generated) {
      setClarification(normalizeClarification(generated));
      setDraft(emptyClarificationDraft());
    }
    return next;
  };

  const executeResearch = async (state: TaskView) => {
    if (!state.task_id) return;
    if (!durableTaskReady) {
      setError("任务尚未从 Engine 持久化日志完成核验，暂不能继续执行");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      setBusyStage("MoonBit Agent 正在规划工具、核验多源资料并执行三轮审查…");
      const reply = await requestDurableAgent<TaskTransition>("agent.research.workflow", {
        task_id: state.task_id,
        depth,
        tool_policy: toolPolicy,
        preferred_symbols: contextSymbol ? [contextSymbol] : [],
        capital: capitalFromObjective(state.spec?.objective),
      }, 900_000);
      const next = applyTransition(reply);
      if (reply.report?.trim()) append("agent", reply.report.trim());
      else if (next.phase === "suspended") {
        const reason = reply.verification_findings?.[0] ?? "模型服务暂不可用，任务与工具结果已安全保存";
        setError(reason);
        append("system", `${reason}。配置或额度恢复后可从同一检查点继续，不会重复采集已命中的资料。`);
      } else if (next.phase === "verification_failed") {
        append("system", `报告校验未通过，未发布不完整结论：${reply.verification_findings?.join("；") || "请查看证据与诊断"}。`);
      } else append("system", `研究执行完成，但 Worker 未返回报告正文。当前状态：${phaseLabel[next.phase ?? "idle"]}。`);
    } catch (cause) {
      if (cancelRequestedTaskRef.current === state.task_id) return;
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(message);
      append("system", `研究暂停：${message}。任务检查点已保留，可直接重试。`);
      try {
        const recovered = await recoverLatestCheckpoint(state.task_id);
        append("system", `已从持久化日志恢复到“${phaseLabel[recovered.phase ?? "idle"]}”，重试会先对账未完成工具。`);
      } catch {
        // Keep the original failure visible; recovery diagnostics are exposed
        // separately and must not hide the root cause.
      }
    } finally {
      if (cancelRequestedTaskRef.current !== state.task_id) {
        setBusy(false);
        setBusyStage("");
      }
    }
  };

  const stopResearch = async () => {
    const active = task;
    if (!busy || stopping || !active?.task_id) return;
    cancelRequestedTaskRef.current = active.task_id;
    setStopping(true);
    setBusyStage("正在安全停止研究并保存最后一个已提交检查点…");
    setError(null);
    try {
      const reply = await requestDurableAgent<TaskTransition>("agent.event", {
        task_id: active.task_id,
        seq: Math.max(1, active.accepted_seq ?? 0) + 1,
        event_kind: "cancel",
      }, 120_000);
      const next = applyTransition(reply);
      if (next.phase !== "cancelled") throw new Error(`停止请求返回了意外状态：${phaseLabel[next.phase ?? "idle"]}`);
      append("system", "研究已按你的要求停止。已完成的工具结果和证据仍保存在任务日志中，不会发布未校验结论。");
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(`停止研究失败：${message}`);
      try {
        await recoverLatestCheckpoint(active.task_id);
      } catch {
        // The cancellation error remains the primary diagnostic.
      }
    } finally {
      cancelRequestedTaskRef.current = null;
      setStopping(false);
      setBusy(false);
      setBusyStage("");
    }
  };

  const start = async () => {
    const objective = input.trim();
    if (!objective || busy) return;
    if (!isProton()) {
      setError("Agent 只在 AStock 桌面应用中运行；浏览器预览没有行情、资料和本地 Worker。请从桌面程序进入。");
      return;
    }
    const taskId = crypto.randomUUID();
    const initialSpec: TaskSpec = {
      objective,
      security_universe: contextSymbol ? [contextSymbol] : [],
      as_of: "",
      research_start: "",
      research_end: "",
      investment_horizon: "",
      comparison_benchmark: "",
      output_type: /投资计划|交易计划|操作计划/.test(objective) ? "manual_plan" : "research_report",
      evidence_requirement: depth === "exhaustive" ? "primary_sources" : depth === "deep" ? "strict" : "standard",
    };
    setBusy(true);
    setBusyStage("正在理解目标并检查缺失的研究边界…");
    setError(null);
    setInput("");
    setTask(null);
    setEffects([]);
    setVerification(null);
    setCheckpoint(undefined);
    setDurableTaskReady(true);
    restoredTaskRef.current = null;
    setClarification(null);
    setDraft(emptyClarificationDraft());
    append("user", objective);
    try {
      const reply = await requestDurableAgent<TaskTransition>("agent.start", {
        task_id: taskId,
        seq: 1,
        spec: initialSpec,
        run_options: { depth, tool_policy: toolPolicy },
      }, 120_000);
      const next = applyTransition(reply);
      if (next.phase === "waiting_for_user") append("agent", "我正在结合你的研究目标生成必要的澄清问题。问题和候选项由模型动态产生，前端不会套用预设问卷。");
      else {
        append("agent", "任务边界完整，正在制定资料计划并准备调用研究工具。所有结论只用于人工审阅，不会自动下单。");
        if (next.phase === "preparing") await executeResearch(next);
      }
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      setInput(objective);
      setError(message);
      append("system", `任务尚未启动：${message}。你的问题已放回输入框，可在检查模型服务后直接重试。`);
    } finally {
      setBusy(false);
      setBusyStage("");
    }
  };

  const selectOption = (question: ClarificationQuestion, optionId: string) => {
    setDraft((current) => {
      const selected = current.selections[question.id] ?? [];
      const next = question.kind === "multiple"
        ? selected.includes(optionId) ? selected.filter((id) => id !== optionId) : [...selected, optionId]
        : [optionId];
      return { ...current, selections: { ...current.selections, [question.id]: next } };
    });
  };

  const letAgentChooseAll = () => {
    if (!clarification) return;
    setDraft({
      selections: Object.fromEntries(clarification.questions.map((question) => [question.id, [AGENT_BEST]])),
      other: {},
    });
  };

  const submitClarification = async () => {
    if (!task?.task_id || !clarification || !completedClarification || busy) return;
    const answers = clarification.questions.map((question) => {
      const optionIds = draft.selections[question.id] ?? [];
      const agentBest = optionIds.includes(AGENT_BEST);
      const labels = question.options.filter((option) => optionIds.includes(option.id)).map((option) => option.label);
      const other = draft.other[question.id]?.trim() || null;
      return {
        question_id: question.id,
        option_ids: optionIds.filter((id) => id !== AGENT_BEST),
        answer: agentBest ? null : [...labels, ...(other ? [other] : [])].join("；"),
        decision_mode: agentBest ? "agent_best_with_evidence" : "user_selected",
      };
    });
    setBusy(true);
    setBusyStage("正在让模型解析你的选择并补全研究边界…");
    setError(null);
    try {
      const reply = await requestDurableAgent<TaskTransition>("agent.event", {
        task_id: task.task_id,
        seq: (task.accepted_seq ?? 1) + 1,
        event_kind: "clarification_answered",
        clarification_response: { title: clarification.title, answers },
        run_options: { depth, tool_policy: toolPolicy },
      }, 120_000);
      const autoCount = answers.filter((answer) => answer.decision_mode === "agent_best_with_evidence").length;
      append("user", autoCount ? `已提交研究边界；其中 ${autoCount} 项明确授权 Agent 在取得证据后选择，并要求记录依据。` : "已提交模型提出的研究边界，继续执行。" );
      setClarification(null);
      const next = applyTransition(reply);
      append("agent", next.phase === "preparing" ? "研究边界已确认。正在建立资料计划；每次工具调用、缓存命中、证据与校验状态都会记录在右侧。" : `当前状态：${phaseLabel[next.phase ?? "idle"]}。`);
      if (next.phase === "preparing") await executeResearch(next);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
      setBusyStage("");
    }
  };

  const newResearch = () => {
    if (busy) return;
    setInput("");
    setTitle("");
    setMessages([]);
    setTask(null);
    setEffects([]);
    setVerification(null);
    setClarification(null);
    setDraft(emptyClarificationDraft());
    setCheckpoint(undefined);
    setDurableTaskReady(true);
    setError(null);
    setHistoryOpen(false);
    setSessionId(crypto.randomUUID());
    setCreatedAt(Date.now());
    restoredTaskRef.current = null;
    composerRef.current?.focus();
  };

  const restoreSession = (saved: AgentSession, storedTitle = "") => {
    if (!saved.sessionId) return;
    setSessionId(saved.sessionId);
    setTitle(storedTitle || saved.title || "");
    setCreatedAt(saved.createdAt ?? Date.now());
    setInput(saved.input ?? "");
    setDepth(saved.depth ?? "deep");
    setToolPolicy(saved.toolPolicy ?? "auto");
    setMessages((saved.messages ?? []).map((message) => ({
      ...message,
      text: message.role === "user" ? message.text : sanitizeAgentVisibleText(message.text),
    })).filter((message) => message.text.trim()));
    setTask(saved.task ?? null);
    setEffects(expandAgentActivities(saved.effects ?? []));
    setVerification(saved.verification ?? null);
    setClarification(saved.clarification ? normalizeClarification(saved.clarification) : null);
    setDraft(saved.draft ?? emptyClarificationDraft());
    setCheckpoint(saved.checkpoint);
    setDurableTaskReady(!saved.task?.task_id);
    setError(null);
    restoredTaskRef.current = null;
    setHistoryOpen(false);
    setHistoryQuery("");
    setHistorySearchResults(null);
  };

  const openHistory = async (saved: ConversationSummary) => {
    if (busy) return;
    setHistoryLoading(true);
    setError(null);
    try {
      const stored = await requestNative<StoredConversation>("engine", "agent.conversation.load", { conversation_id: saved.conversation_id });
      restoreSession(stored.session, stored.title);
    } catch (cause) {
      setError(`打开历史研究失败：${cause instanceof Error ? cause.message : String(cause)}`);
    } finally {
      setHistoryLoading(false);
    }
  };

  const branchFromMessage = async (messageId: string, sourceConversationId = sessionId) => {
    if (busy || historyLoading) return;
    const newConversationId = crypto.randomUUID();
    setHistoryLoading(true);
    setError(null);
    try {
      const stored = await requestNative<StoredConversation>("engine", "agent.conversation.branch", {
        source_conversation_id: sourceConversationId,
        new_conversation_id: newConversationId,
        message_id: messageId,
        title: `${title || sessionTitle(messages)} · 分支`,
      });
      restoreSession(stored.session, stored.title);
      setInput("基于以上节点重新取得最新数据并继续研究；旧结论只能作为待核验线索，不得直接沿用。 ");
      setHistory((current) => [{
        conversation_id: stored.conversation_id,
        title: stored.title,
        phase: "idle" as AgentPhase,
        message_count: stored.session.messages?.length ?? 0,
        evidence_count: 0,
        parent_conversation_id: stored.parent_conversation_id,
        branch_from_message_id: stored.branch_from_message_id,
        created_at: stored.created_at,
        updated_at: stored.updated_at,
      }, ...current].slice(0, MAX_HISTORY_ITEMS));
      requestAnimationFrame(() => composerRef.current?.focus());
    } catch (cause) {
      setError(`创建研究分支失败：${cause instanceof Error ? cause.message : String(cause)}`);
    } finally {
      setHistoryLoading(false);
    }
  };

  const continueHistory = async (saved: ConversationSummary) => {
    if (busy || historyLoading) return;
    setHistoryLoading(true);
    try {
      const stored = await requestNative<StoredConversation>("engine", "agent.conversation.load", { conversation_id: saved.conversation_id });
      const last = stored.session.messages?.at(-1);
      if (!last) throw new Error("该历史研究没有可分支的消息");
      restoreSession(stored.session, stored.title);
      setHistoryLoading(false);
      await branchFromMessage(last.id, saved.conversation_id);
    } catch (cause) {
      setHistoryLoading(false);
      setError(`继续历史研究失败：${cause instanceof Error ? cause.message : String(cause)}`);
    }
  };

  const renameHistory = async (saved: ConversationSummary) => {
    const nextTitle = window.prompt("重命名研究会话", saved.title)?.trim();
    if (!nextTitle || nextTitle === saved.title) return;
    try {
      await requestNative("engine", "agent.conversation.rename", { conversation_id: saved.conversation_id, title: nextTitle });
      setHistory((current) => current.map((item) => item.conversation_id === saved.conversation_id ? { ...item, title: nextTitle } : item));
      setHistorySearchResults((current) => current?.map((item) => item.conversation_id === saved.conversation_id ? { ...item, title: nextTitle } : item) ?? null);
      if (saved.conversation_id === sessionId) setTitle(nextTitle);
    } catch (cause) {
      setError(`重命名失败：${cause instanceof Error ? cause.message : String(cause)}`);
    }
  };

  const deleteHistory = async (saved: ConversationSummary) => {
    try {
      await requestNative("engine", "agent.conversation.delete", { conversation_id: saved.conversation_id });
      setHistory((current) => current.filter((item) => item.conversation_id !== saved.conversation_id));
      setHistorySearchResults((current) => current?.filter((item) => item.conversation_id !== saved.conversation_id) ?? null);
      if (saved.conversation_id === sessionId) newResearch();
    } catch (cause) {
      setError(`删除历史研究失败：${cause instanceof Error ? cause.message : String(cause)}`);
    }
  };

  const status = task?.phase ?? "idle";
  const activity = effects.filter((effect) => effect.kind !== "persist_checkpoint");
  const finalVerificationActivity = activity.reduce((last, effect, index) => effect.kind === "verify_report" ? index : last, -1);
  const displayedHistory = historyQuery.trim() ? historySearchResults ?? [] : history;

  return <div className="agent-console agent-golden-layout">
    <header className="agent-console-header">
      <div><span className="agent-orb" /><strong>AStock Agent</strong><span className={`status-pill phase-${status}`}>{phaseLabel[status]}</span></div>
      <div className="agent-run-metrics"><span>工具 {task?.completed_tool_count ?? 0}/{(task?.completed_tool_count ?? 0) + (task?.pending_tool_count ?? 0)}</span><span>证据 {task?.evidence_ids?.length ?? 0}</span><span title="仅展示 Worker 返回的真实缓存统计">缓存命中 {cacheRequests ? `${Math.round(cacheHits / cacheRequests * 100)}% (${cacheHits}/${cacheRequests})` : "— 暂无样本"}</span>{task?.task_id && <span title="历史会话只负责展示；此状态表示执行检查点已从 Engine 日志核验">任务日志 {durableTaskReady ? "已核验" : "核验中"}</span>}{busy && task?.task_id && <button className="agent-stop" onClick={() => void stopResearch()} disabled={stopping}>{stopping ? "停止中…" : "■ 停止研究"}</button>}<button onClick={() => setHistoryOpen(true)} disabled={busy}>历史 {history.length || ""}</button><button onClick={newResearch} disabled={busy}>＋ 新对话</button></div>
    </header>

    {historyOpen && <div className="agent-history-backdrop" role="presentation" onMouseDown={(event) => { if (event.currentTarget === event.target) setHistoryOpen(false); }}>
      <section className="agent-history-panel" role="dialog" aria-modal="true" aria-label="Agent 历史记录">
        <header><div><span className="eyebrow">RESEARCH HISTORY</span><h2>历史研究</h2><p>打开可查看完整记录；“基于此继续”会新建任务并重新取得最新数据。</p></div><button aria-label="关闭历史记录" onClick={() => setHistoryOpen(false)}>×</button></header>
        <label className="agent-history-search"><span>搜索</span><input type="search" value={historyQuery} onChange={(event) => setHistoryQuery(event.target.value)} placeholder="标题或会话编号" aria-label="搜索 Agent 历史" /><em>{historySearching ? "查询中…" : historyQuery.trim() ? `${displayedHistory.length} 项` : "最近记录"}</em></label>
        <div className="agent-history-list">{historySearching ? <div className="agent-history-empty"><b>正在搜索历史研究</b><p>从本机持久化记录中查询，不会向模型或外部服务发送关键词。</p></div> : displayedHistory.length ? displayedHistory.map((saved) => <article className={saved.conversation_id === sessionId ? "current" : ""} key={saved.conversation_id}>
          <button className="history-main" disabled={historyLoading} onClick={() => void openHistory(saved)}><b>{saved.title}</b><span>{historyTime(saved.updated_at)} · {phaseLabel[saved.phase ?? "idle"]}</span><small>{saved.message_count} 条记录 · {saved.evidence_count} 条证据{saved.parent_conversation_id ? " · 研究分支" : ""}</small></button>
          <div><button disabled={historyLoading} onClick={() => void continueHistory(saved)}>基于此继续</button><button onClick={() => void renameHistory(saved)}>重命名</button><button className="danger" aria-label={`删除 ${saved.title || "历史研究"}`} onClick={() => void deleteHistory(saved)}>删除</button></div>
        </article>) : <div className="agent-history-empty"><b>{historyQuery.trim() ? "没有匹配的历史研究" : "还没有历史研究"}</b><p>{historyQuery.trim() ? "尝试输入目标、证券名称或会话编号中的其他关键词。" : "发送第一条研究任务后会自动保存在本机，切换页面或重启桌面应用也不会丢失。"}</p></div>}</div>
        <footer><span>当前最多显示 {MAX_HISTORY_ITEMS} 项 · 历史保存在本机 · 不保存 API Key</span><button onClick={newResearch}>开始新对话</button></footer>
      </section>
    </div>}

    <main className="agent-transcript" aria-live="polite">
      {messages.length === 0 ? <section className="agent-welcome">
        <span className="agent-orb large" />
        <h2>今天想研究什么？</h2>
        <p>像使用 Codex 一样直接描述复杂问题。Agent 会制定计划，调用行情、公告、新闻、基本面、量化与证据工具，并在资料不足时动态向你提问。</p>
        <div className="agent-suggestions">
          <button onClick={() => setInput(`为${contextSymbol ? ` ${contextSymbol}` : "目标股票"}生成一份仅供人工执行的投资计划：综合行情趋势、估值、基本面、资金、事件与风险，给出观察条件、触发条件、失效条件和复核清单`)}>生成投资计划</button>
          <button onClick={() => setInput(`分析${contextSymbol ? ` ${contextSymbol}` : "目标股票"}当前行情：覆盖趋势、量价、关键价位、资金行为、市场环境与可能的反向情景`)}>分析股票行情</button>
          <button onClick={() => setInput(`${contextSymbol ? `检查 ${contextSymbol}` : "检查目标公司"}的利润增长质量、估值与主要风险，并逐项给出可核验证据和数据缺口`)}>证据与风险清单</button>
        </div>
      </section> : <div className="message-column">
        {messages.map((message) => <article className={`agent-message role-${message.role}`} key={message.id}>
          <div className="message-meta"><b>{message.role === "user" ? "你" : message.role === "agent" ? "AStock Agent" : message.role === "tool" ? "工具" : "系统"}</b><span><button title="从这条消息创建新研究分支" disabled={busy || historyLoading} onClick={() => void branchFromMessage(message.id)}>从此分支</button><time>{message.timestamp}</time></span></div>
          <p>{message.text}</p>
        </article>)}

        {task?.phase === "waiting_for_user" && !clarification && <section className="clarification-loading"><span className="send-spinner" /><div><b>模型正在生成澄清问题</b><p>只询问会实质影响研究结论的缺失信息，不使用前端预设模板。</p></div></section>}

        {clarification && <section className="clarification-card clarification-model-card">
          <header><div><span>模型生成的问题</span><h3>{clarification.title}</h3>{clarification.description && <p>{clarification.description}</p>}</div><button onClick={letAgentChooseAll}>全部由 Agent 基于证据选择</button></header>
          <div className="clarification-question-list">{clarification.questions.map((question, index) => <fieldset key={question.id}>
            <legend><small>{question.header ?? `问题 ${index + 1}`}</small>{question.question}</legend>
            <div className="clarification-options">{withAgentChoice(question).map((option) => {
              const selected = draft.selections[question.id]?.includes(option.id);
              return <button className={`${selected ? "selected" : ""} ${option.id === AGENT_BEST ? "agent-best" : ""}`} disabled={busy} key={option.id} onClick={() => selectOption(question, option.id)}><b>{option.label}{option.recommended && <em>推荐</em>}</b>{option.description && <small>{option.description}</small>}</button>;
            })}</div>
            {question.allowOther && <input value={draft.other[question.id] ?? ""} onChange={(event) => setDraft((current) => ({ ...current, other: { ...current.other, [question.id]: event.target.value } }))} placeholder="也可以输入自己的答案" />}
          </fieldset>)}</div>
          <footer><span>“由 Agent 选择”属于你的明确授权，最终选择、依据和证据会写入任务记录。</span><button disabled={!completedClarification || busy} onClick={() => void submitClarification()}>{busy ? "提交中…" : "确认并继续"}</button></footer>
        </section>}
      </div>}
    </main>

    <aside className="agent-activity" aria-label="Agent 工具活动">
      <header><div><b>研究进度与工具</b><small>真实状态 · 可恢复记录</small></div><span>{phaseLabel[status]}</span></header>
      <ol className="research-phase-strip"><li className={status !== "idle" ? "done" : "active"}>理解任务</li><li className={["preparing", "reasoning", "awaiting_tools", "reviewing", "synthesizing", "verifying", "completed"].includes(status) ? "active" : ""}>规划取数</li><li className={["awaiting_tools", "reviewing", "synthesizing", "verifying", "completed"].includes(status) ? "active" : ""}>工具与证据</li><li className={["reviewing", "synthesizing", "verifying", "completed"].includes(status) ? "active" : ""}>核验综合</li><li className={status === "completed" ? "active" : ""}>完成</li></ol>
      <div className="activity-feed">
        {activity.length ? activity.map((effect, index) => {
          const [title, detail] = effectCopy[effect.kind ?? ""] ?? [effect.title ?? effect.tool ?? "任务活动", effect.detail ?? "Worker 已记录此项活动"];
          const showVerification = effect.kind === "verify_report" && index === finalVerificationActivity && verification;
          const activityClass = effect.status === "failed" ? "failed" : effect.status === "skipped" ? "skipped" : effect.cache_hit || showVerification && status === "completed" || effect.status === "succeeded" ? "cache" : "running";
          return <article key={`${effect.call_id ?? effect.kind}-${index}`}><i className={activityClass} /><div><b>{title}</b><p>{detail}</p><small>{effect.call_id && `调用 ${effect.call_id}`}{effect.cache_hit && " · 命中缓存"}{effect.evidence_count ? ` · ${effect.evidence_count} 条证据` : ""}{showVerification && ` · ${deterministicVerificationSummary(verification)}`}</small>{showVerification && <small className="block break-all">校验器 {verification.version ?? "版本未知"}</small>}</div></article>;
        }) : <div className="activity-empty"><b>等待任务</b><p>开始研究后，这里会按顺序展示计划、工具调用、缓存、证据与报告校验，不显示模型私有推理链。</p></div>}
      </div>
      <footer><span>已取得证据</span><b>{task?.evidence_ids?.length ?? 0}</b><span>完成工具</span><b>{task?.completed_tool_count ?? 0}</b></footer>
    </aside>

    <footer className="agent-composer-wrap">
      {busyStage && <div className="agent-busy-stage"><span className="send-spinner" />{busyStage}</div>}
      {error && <div className="agent-error"><span>{error}</span><div>{task && ["preparing", "awaiting_tools", "suspended"].includes(task.phase ?? "") && <button onClick={() => void executeResearch(task)} disabled={busy || !durableTaskReady}>{task.phase === "suspended" ? "继续研究" : "重试研究"}</button>}<button onClick={() => setError(null)}>×</button></div></div>}
      <div className="agent-composer">
        <textarea ref={composerRef} value={input} disabled={busy || status === "waiting_for_user"} onChange={(event) => setInput(event.target.value)} placeholder={status === "waiting_for_user" ? "请先回答上方模型生成的问题…" : "向 AStock Agent 描述你的研究问题…"} rows={3} onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void start(); } }} />
        <div className="composer-toolbar">
          <label title="控制研究轮次、证据强度与验证深度"><span>分析深度</span><select value={depth} onChange={(event) => setDepth(event.target.value as Depth)}>{Object.entries(depthLabel).map(([value, label]) => <option value={value} key={value}>{label}</option>)}</select></label>
          <label title="限制本次任务允许调用的研究工具"><span>工具</span><select value={toolPolicy} onChange={(event) => setToolPolicy(event.target.value as ToolPolicy)}>{Object.entries(toolLabel).map(([value, label]) => <option value={value} key={value}>{label}</option>)}</select></label>
          <span className="composer-note">纯文本 · 不展示私有推理链 · 不自动交易</span>
          <button className="agent-send" aria-label="发送" disabled={!input.trim() || busy || status === "waiting_for_user"} onClick={() => void start()}>{busy ? <span className="send-spinner" /> : "↑"}</button>
        </div>
      </div>
      <p>Enter 发送 · Shift+Enter 换行。Agent 输出是研究信息，不构成投资建议。</p>
    </footer>
  </div>;
}
