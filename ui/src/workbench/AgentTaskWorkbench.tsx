import { useEffect, useMemo, useRef, useState } from "react";
import type { AgentPhase, TaskSpec } from "../bridge/generated";
import { isProton, requestNative } from "../bridge";
import type { ClarificationDraft, ClarificationQuestion, ClarificationRequest } from "../lib/agentClarification";
import { emptyClarificationDraft } from "../lib/agentClarification";
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
};
type TaskTransition = {
  accepted?: boolean;
  rejection?: string | null;
  state?: TaskView;
  effects?: AgentEffect[];
  activities?: AgentEffect[];
  clarification?: ClarificationRequest | null;
  checkpoint?: unknown;
  report?: string;
};

type ResearchCandidate = {
  symbol: string;
  name: string;
  market: string | null;
  board: string | null;
  industry: string | null;
  price: number | null;
  pct: number | null;
  amount: number | null;
  lot_cost: number | null;
};

type ResearchPlan = {
  symbols: string[];
  selection_summary: string;
  verification_focus: string[];
  rejected_risks: string[];
};

type ResearchNews = {
  items: Array<{ document_id?: string; revision_id?: string; url?: string; [key: string]: unknown }>;
  successful_sources: string[];
  successful_channels?: string[];
  stale_sources: string[];
  errors: string[];
  requested_source_count?: number;
  evidence_note?: string;
};

const NEWS_SOURCE_GROUPS = [
  ["cls-telegraph", "cls-depth", "cls-hot", "jin10"],
  ["wallstreetcn-quick", "wallstreetcn-hot", "wallstreetcn-news", "mktnews-flash"],
  ["gelonghui", "fastbull-express", "fastbull-news", "xueqiu-hotstock"],
];

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
};

type ConversationSummary = {
  conversation_id: string;
  title: string;
  phase: AgentPhase;
  message_count: number;
  evidence_count: number;
  parent_conversation_id?: string | null;
  branch_from_message_id?: string | null;
  created_at: number;
  updated_at: number;
};

type StoredConversation = ConversationSummary & { session: AgentSession };

type DurableEffect = {
  effect_id: string;
  effect_kind: string;
  status: "pending" | "succeeded" | "failed" | "cancelled";
  result?: unknown;
  idempotency_key: string;
};

type DurableTask = {
  task: { accepted_seq: number; checkpoint?: unknown };
  events: Array<{ seq: number }>;
};

const MAX_HISTORY_ITEMS = 80;

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
  verify_report: ["发布前校验", "检查证据完整性、计算引用和研究边界"],
  publish_report: ["发布报告", "报告已通过校验并保存"],
};

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

function validResearchSymbols(values?: string[] | null): string[] {
  return [...new Set((values ?? []).filter((value) => /^\d{6}$/.test(value)))].slice(0, 5);
}

type JsonObject = Record<string, unknown>;

function asObject(value: unknown): JsonObject | null {
  return value !== null && typeof value === "object" && !Array.isArray(value) ? value as JsonObject : null;
}

function tailRows(value: unknown, limit: number): unknown {
  return Array.isArray(value) ? value.slice(-limit) : value;
}

function compactDatasetRows(value: unknown, limit: number): unknown {
  const dataset = asObject(value);
  if (!dataset || !Array.isArray(dataset.rows)) return value;
  return { ...dataset, rows: dataset.rows.slice(0, limit), model_view_rows: Math.min(dataset.rows.length, limit) };
}

function compactResearchDatasets(value: unknown, limits: Record<string, number>, fallback: number): unknown {
  const envelope = asObject(value);
  const datasets = asObject(envelope?.datasets);
  if (!envelope || !datasets) return value;
  return {
    ...envelope,
    datasets: Object.fromEntries(Object.entries(datasets).map(([key, dataset]) => [
      key,
      compactDatasetRows(dataset, limits[key] ?? fallback),
    ])),
  };
}

function compactMarketContext(value: unknown, exhaustive: boolean): unknown {
  return compactResearchDatasets(value, {
    billboard_7d: exhaustive ? 120 : 80,
    margin_daily: exhaustive ? 40 : 30,
    industry_boards: exhaustive ? 60 : 40,
    concept_boards: exhaustive ? 60 : 40,
    previous_limit_up_pool: exhaustive ? 160 : 100,
    sub_new_pool: exhaustive ? 160 : 100,
  }, 200);
}

/**
 * Keep the Engine/database response complete at rest while giving each model
 * round a bounded, auditable research view. Missing/error/source fields are
 * deliberately retained; only repetitive time-series rows are windowed.
 */
function compactSecurityEvidence(
  bundle: { symbol: string; market: unknown; fundamentals: unknown; events: unknown; news: ResearchNews; reconciliation: unknown; joinquant: unknown },
  exhaustive: boolean,
) {
  const market = asObject(bundle.market);
  const kline = asObject(market?.kline);
  const fundamentals = asObject(bundle.fundamentals);
  const reconciliation = asObject(bundle.reconciliation);
  return {
    symbol: bundle.symbol,
    market: market ? {
      ...market,
      kline: kline ? { ...kline, bars: tailRows(kline.bars, exhaustive ? 250 : 180) } : market.kline,
      fund_flow_30d: tailRows(market.fund_flow_30d, 30),
    } : bundle.market,
    fundamentals: fundamentals ? {
      ...fundamentals,
      income: tailRows(fundamentals.income, 8),
      balance: tailRows(fundamentals.balance, 8),
      cashflow: tailRows(fundamentals.cashflow, 8),
      indicators: tailRows(fundamentals.indicators, 8),
      dividends: tailRows(fundamentals.dividends, 8),
      valuation_history: tailRows(fundamentals.valuation_history, exhaustive ? 252 : 180),
    } : bundle.fundamentals,
    events: compactResearchDatasets(bundle.events, {
      announcements_1y: exhaustive ? 160 : 100,
      cninfo_disclosures_1y: exhaustive ? 50 : 40,
      org_survey_2y: exhaustive ? 120 : 80,
      block_trade_1y: exhaustive ? 120 : 80,
    }, exhaustive ? 120 : 80),
    news: { ...bundle.news, items: bundle.news.items.slice(0, exhaustive ? 30 : 20) },
    reconciliation: reconciliation ? {
      ...reconciliation,
      kline_close_checks: tailRows(reconciliation.kline_close_checks, 20),
    } : bundle.reconciliation,
    joinquant: compactResearchDatasets(bundle.joinquant, { qfq_daily: exhaustive ? 500 : 250, benchmark_components: 500, macro_cpi: 24 }, 500),
  };
}

async function fetchResearchNews(
  filters: { symbol?: string; keyword?: string },
  limit: number,
  minimumItems = 10,
): Promise<ResearchNews> {
  const batch = await requestNative<ResearchNews>("engine", "research.news", {
    ...filters,
    sources: NEWS_SOURCE_GROUPS.flat(),
    limit,
  }, { deadlineMs: 120_000 });
  if (batch.items.length < minimumItems) {
    throw new Error(`有效资讯仅 ${batch.items.length} 条，低于研究发布门槛；${batch.errors.join("；")}`);
  }
  return {
    ...batch,
    successful_sources: [...new Set(batch.successful_sources ?? [])],
    successful_channels: [...new Set(batch.successful_channels ?? [])],
    stale_sources: [...new Set(batch.stale_sources ?? [])],
    errors: [...new Set(batch.errors ?? [])],
    requested_source_count: NEWS_SOURCE_GROUPS.flat().length,
    evidence_note: "一次有界采集覆盖12类频道，频道与采集Provider分层记录并按文档修订去重；重要判断仍须回链一级来源",
  };
}

/**
 * All stateful Agent Worker operations pass through the Engine journal. The
 * effect intent is committed before the Worker can contact a provider; the
 * provider result, reducer outcome events and full checkpoint are committed
 * before the renderer receives the reply.
 */
export async function requestDurableAgent<T>(
  kind: "agent.start" | "agent.event" | "agent.plan" | "agent.research",
  payload: Record<string, unknown>,
  taskId: string,
  acceptedSeq: number,
  deadlineMs: number,
  taskSpec?: TaskSpec,
): Promise<T> {
  if (taskSpec) {
    await requestNative("engine", "agent.task.create", {
      task_id: taskId,
      reducer_version: "moonbit-agent-kernel-v1",
      task_spec: taskSpec,
      phase: "idle",
    });
  }

  const inputSeq = typeof payload.seq === "number" ? payload.seq : null;
  if (inputSeq != null) {
    await requestNative("engine", "agent.event.append", {
      task_id: taskId,
      seq: inputSeq,
      event_id: `input:${taskId}:${inputSeq}`,
      event_kind: kind === "agent.start" ? "start" : String(payload.event_kind ?? "agent_event"),
      event: { worker_request_kind: kind, payload },
    });
  }

  const baseKey = `${taskId}:${kind}:${inputSeq ?? acceptedSeq}`;
  const effects = await requestNative<{ items: DurableEffect[] }>(
    "engine",
    "agent.effect.list",
    { task_id: taskId },
  );
  const prior = effects.items.filter((item) => item.idempotency_key === baseKey || item.idempotency_key.startsWith(`${baseKey}:retry:`));
  const completed = prior.find((item) => item.status === "succeeded" && item.result != null);
  if (completed) return completed.result as T;
  if (prior.some((item) => item.status === "pending")) {
    throw new Error("检测到同一 Agent 操作仍为 pending；任务已保留，请先恢复或等待本地 Worker 完成，避免重复调用模型。 ");
  }
  const retry = prior.length;
  const idempotencyKey = retry === 0 ? baseKey : `${baseKey}:retry:${retry}`;
  const effectId = `fx:${taskId}:${kind.replaceAll(".", "-")}:${inputSeq ?? acceptedSeq}:${retry}`;
  await requestNative("engine", "agent.effect.begin", {
    effect_id: effectId,
    task_id: taskId,
    caused_by_seq: inputSeq ?? acceptedSeq,
    effect_kind: kind,
    effect: { worker_request_kind: kind, payload },
    idempotency_key: idempotencyKey,
  });

  let reply: T;
  try {
    reply = await requestNative<T>("agent", kind, payload, { deadlineMs });
  } catch (cause) {
    await requestNative("engine", "agent.effect.complete", {
      effect_id: effectId,
      status: "failed",
      result: { error: cause instanceof Error ? cause.message : String(cause) },
    }).catch(() => undefined);
    throw cause;
  }

  await requestNative("engine", "agent.effect.complete", {
    effect_id: effectId,
    status: "succeeded",
    result: reply,
  });

  const transition = reply as T & { state?: TaskView; checkpoint?: unknown };
  const finalSeq = transition.state?.accepted_seq;
  if (typeof finalSeq === "number" && transition.checkpoint !== undefined) {
    const durable = await requestNative<DurableTask>("engine", "agent.task.load", { task_id: taskId });
    let durableMax = durable.events.reduce((maximum, event) => Math.max(maximum, event.seq), 0);
    while (durableMax < finalSeq) {
      durableMax += 1;
      await requestNative("engine", "agent.event.append", {
        task_id: taskId,
        seq: durableMax,
        event_id: `result:${taskId}:${durableMax}`,
        event_kind: durableMax === finalSeq ? `${kind}.result` : `${kind}.transition`,
        event: durableMax === finalSeq ? { effect_id: effectId, state: transition.state } : { effect_id: effectId },
      });
    }
    await requestNative("engine", "agent.checkpoint.put", {
      task_id: taskId,
      accepted_seq: finalSeq,
      phase: transition.state?.phase ?? "idle",
      checkpoint: transition.checkpoint,
    });
  }
  return reply;
}

export default function AgentTaskWorkbench() {
  const [sessionId, setSessionId] = useState<string>(() => crypto.randomUUID());
  const [title, setTitle] = useState("");
  const [createdAt, setCreatedAt] = useState(Date.now);
  const [history, setHistory] = useState<ConversationSummary[]>([]);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [historyLoading, setHistoryLoading] = useState(false);
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
  const [busy, setBusy] = useState(false);
  const [busyStage, setBusyStage] = useState("");
  const [error, setError] = useState<string | null>(null);
  const composerRef = useRef<HTMLTextAreaElement | null>(null);
  const restoredTaskRef = useRef<string | null>(null);

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
    if (!hydrated || !isProton() || !messages.length) return;
    const updatedAt = Date.now();
    const resolvedTitle = title || sessionTitle(messages);
    const session: AgentSession = { sessionId, title: resolvedTitle, createdAt, updatedAt, input, depth, toolPolicy, messages, task, effects, clarification, draft, checkpoint };
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
  }, [checkpoint, clarification, createdAt, depth, draft, effects, hydrated, input, messages, sessionId, task, title, toolPolicy]);

  useEffect(() => {
    if (!historyOpen) return;
    const close = (event: KeyboardEvent) => { if (event.key === "Escape") setHistoryOpen(false); };
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [historyOpen]);

  useEffect(() => {
    if (!isProton() || !checkpoint || !task?.task_id || restoredTaskRef.current === task.task_id) return;
    restoredTaskRef.current = task.task_id;
    void requestNative("agent", "agent.restore", { state: checkpoint }, { deadlineMs: 20_000 }).catch((cause) => {
      restoredTaskRef.current = null;
      setError(`恢复 Agent 任务失败：${cause instanceof Error ? cause.message : String(cause)}`);
    });
  }, [checkpoint, task?.task_id]);

  const cacheRequests = task?.cache_requests ?? 0;
  const cacheHits = task?.cache_hits ?? 0;
  const completedClarification = useMemo(() => clarification?.questions.every((question) => questionComplete(question, draft)) ?? false, [clarification, draft]);

  const append = (role: MessageRole, text: string) => setMessages((current) => [
    ...current,
    { id: crypto.randomUUID(), role, text, timestamp: timeNow() },
  ]);

  const applyTransition = (reply: TaskTransition) => {
    const next = reply.state ?? {};
    setTask(next);
    setEffects(reply.activities ?? reply.effects ?? []);
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
    setBusy(true);
    setError(null);
    try {
      const fetchActivities: AgentEffect[] = [];
      setBusyStage("正在读取市场宽度和全部资讯频道…");
      const [marketOverview, marketContext, globalContext, marketNews] = await Promise.all([
        requestNative<unknown>("engine", "market.overview", {}, { deadlineMs: 120_000 }),
        requestNative<unknown>("engine", "research.market_context", {}, { deadlineMs: 180_000 }),
        requestNative<unknown>("engine", "research.global_context", {}, { deadlineMs: 180_000 }),
        fetchResearchNews({}, depth === "exhaustive" ? 60 : 45),
      ]);
      fetchActivities.push(
        { kind: "execute_tool", tool: "市场宽度", detail: "读取涨跌家数、数据时点与行情源", evidence_count: 1 },
        { kind: "execute_tool", tool: "市场环境", detail: "六类涨跌停情绪池、龙虎榜、两融与行业概念板块", evidence_count: 10 },
        { kind: "execute_tool", tool: "全球与宏观背景", detail: "黄金跨市场行情/一手资讯与中美通胀、GDP、经常账户；年度数据不冒充实时信号", evidence_count: 5 },
        { kind: "execute_tool", tool: "多源资讯", detail: "聚合全部12类财经频道；重要结论仍需回链公告或原始来源", evidence_count: 1 },
      );
      setEffects([...fetchActivities]);

      const capital = capitalFromObjective(state.spec?.objective);
      const maximumResearchSymbols = capital !== null && capital <= 50_000 ? 3 : 5;
      let symbols = validResearchSymbols([
        ...(contextSymbol ? [contextSymbol] : []),
        ...(state.spec?.security_universe ?? []),
      ]).slice(0, maximumResearchSymbols);
      let researchPlan: ResearchPlan | null = null;
      if (!symbols.length) {
        setBusyStage("正在建立满足资金与一手约束的候选池…");
        const candidatePool = await requestNative<{ items: ResearchCandidate[]; source: string; fetched_at: string }>(
          "engine",
          "research.market_candidates",
          { limit: depth === "exhaustive" ? 80 : 50, ...(capital ? { max_lot_cost: capital * 0.8 } : {}) },
          { deadlineMs: 120_000 },
        );
        fetchActivities.push({
          kind: "execute_tool",
          tool: "全市场候选筛选",
          detail: `从真实行情中按流动性、风险名称和${capital ? "资金/一手" : "可成交"}约束筛出 ${candidatePool.items.length} 个候选`,
          evidence_count: candidatePool.items.length,
        });
        setEffects([...fetchActivities]);
        setBusyStage("MiniMax Plus 正在规划深度取证对象；此时尚不形成投资结论…");
        const planned = await requestDurableAgent<{ plan: ResearchPlan; activities?: AgentEffect[] }>("agent.plan", {
          task_id: state.task_id,
          candidates: candidatePool.items,
          market_context: marketOverview,
        }, state.task_id, state.accepted_seq ?? 0, 180_000);
        researchPlan = planned.plan;
        symbols = validResearchSymbols(researchPlan.symbols).slice(0, maximumResearchSymbols);
        if (!symbols.length) throw new Error("Agent 研究计划没有返回候选池内的有效证券代码");
        fetchActivities.push(...(planned.activities ?? []));
      }

      setBusyStage(`正在并行核验 ${symbols.length} 只证券的行情、资金、财务、估值、公告与新闻…`);
      const rawSecurities = await Promise.all(symbols.map(async (symbol) => {
        const [market, fundamentals, events, news, reconciliation, joinquant] = await Promise.all([
          requestNative<unknown>("engine", "market.security_snapshot", { symbol, period: "day", adjust: "qfq", count: depth === "exhaustive" ? 500 : 250 }, { deadlineMs: 180_000 }),
          requestNative<unknown>("engine", "research.fundamentals", { symbol }, { deadlineMs: 240_000 }),
          requestNative<unknown>("engine", "research.security_events", { symbol }, { deadlineMs: 180_000 }),
          fetchResearchNews({ symbol, keyword: symbol }, depth === "exhaustive" ? 30 : 20, 0),
          requestNative<unknown>("engine", "research.data_reconcile", { symbol }, { deadlineMs: 180_000 }),
          requestNative<unknown>("engine", "research.joinquant_context", {
            symbol,
            benchmark: state.spec?.comparison_benchmark ?? "000300",
            start: state.spec?.research_start ?? new Date(Date.now() - 365 * 86_400_000).toISOString().slice(0, 10),
            end: state.spec?.research_end ?? new Date().toISOString().slice(0, 10),
          }, { deadlineMs: 180_000 }),
        ]);
        return { symbol, market, fundamentals, events, news, reconciliation, joinquant };
      }));
      const securities = rawSecurities.map((bundle) => compactSecurityEvidence(bundle, depth === "exhaustive"));
      for (const symbol of symbols) {
        fetchActivities.push({
          kind: "execute_tool",
          tool: `${symbol} 综合研究包`,
          detail: "行情/K线/资金流 + 财务三表/估值历史 + 调研/股东/预告/解禁/榜单/公告 + 多源新闻 + 跨源核验 + 可选聚宽研究包；缺失项原样保留",
          evidence_count: 6,
        });
      }
      setEffects([...fetchActivities]);
      setBusyStage("MiniMax Plus 正在进行证据评估、独立反证和最终综合（共三轮）…");
      const reply = await requestDurableAgent<TaskTransition>("agent.research", {
        task_id: state.task_id,
        context: {
          source: "desktop_engine",
          retrieved_at: new Date().toISOString(),
          run_options: { depth, tool_policy: toolPolicy },
          research_plan: researchPlan,
          market_overview: marketOverview,
          market_context: compactMarketContext(marketContext, depth === "exhaustive"),
          global_context: globalContext,
          market_news: { ...marketNews, items: marketNews.items.slice(0, depth === "exhaustive" ? 120 : 90) },
          securities,
          evidence_inventory: {
            requested_symbols: symbols,
            dimensions: ["quote", "kline", "technical_analysis", "fund_flow", "market_pools", "previous_limit_up", "sub_new", "billboard", "margin", "boards", "global_gold", "primary_gold_news", "macro_inflation", "macro_growth", "macro_current_account", "financial_statements", "valuation_history", "org_survey", "holder_count", "earnings_forecast", "unlocks", "suspensions", "block_trade", "announcements", "cninfo_disclosures", "multi_source_news", "cross_provider_reconciliation", "optional_joinquant_daily_valuation_benchmark_macro"],
            review_rounds: 3,
          },
        },
      }, state.task_id, state.accepted_seq ?? 0, 900_000);
      const next = applyTransition(reply);
      setEffects([...fetchActivities, ...(reply.activities ?? reply.effects ?? [])]);
      if (reply.report?.trim()) append("agent", reply.report.trim());
      else append("system", `研究执行完成，但 Worker 未返回报告正文。当前状态：${phaseLabel[next.phase ?? "idle"]}。`);
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(message);
      append("system", `研究暂停：${message}。任务检查点已保留，可直接重试。`);
    } finally {
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
    setCheckpoint(undefined);
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
      }, taskId, 0, 120_000, initialSpec);
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
      }, task.task_id, task.accepted_seq ?? 1, 120_000);
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
    setClarification(null);
    setDraft(emptyClarificationDraft());
    setCheckpoint(undefined);
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
    setMessages(saved.messages ?? []);
    setTask(saved.task ?? null);
    setEffects(saved.effects ?? []);
    setClarification(saved.clarification ? normalizeClarification(saved.clarification) : null);
    setDraft(saved.draft ?? emptyClarificationDraft());
    setCheckpoint(saved.checkpoint);
    setError(null);
    restoredTaskRef.current = null;
    setHistoryOpen(false);
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
      if (saved.conversation_id === sessionId) setTitle(nextTitle);
    } catch (cause) {
      setError(`重命名失败：${cause instanceof Error ? cause.message : String(cause)}`);
    }
  };

  const deleteHistory = async (saved: ConversationSummary) => {
    try {
      await requestNative("engine", "agent.conversation.delete", { conversation_id: saved.conversation_id });
      setHistory((current) => current.filter((item) => item.conversation_id !== saved.conversation_id));
      if (saved.conversation_id === sessionId) newResearch();
    } catch (cause) {
      setError(`删除历史研究失败：${cause instanceof Error ? cause.message : String(cause)}`);
    }
  };

  const status = task?.phase ?? "idle";
  const activity = effects.filter((effect) => effect.kind !== "persist_checkpoint");

  return <div className="agent-console agent-golden-layout">
    <header className="agent-console-header">
      <div><span className="agent-orb" /><strong>AStock Agent</strong><span className={`status-pill phase-${status}`}>{phaseLabel[status]}</span></div>
      <div className="agent-run-metrics"><span>工具 {task?.completed_tool_count ?? 0}/{(task?.completed_tool_count ?? 0) + (task?.pending_tool_count ?? 0)}</span><span>证据 {task?.evidence_ids?.length ?? 0}</span><span title="仅展示 Worker 返回的真实缓存统计">缓存命中 {cacheRequests ? `${Math.round(cacheHits / cacheRequests * 100)}% (${cacheHits}/${cacheRequests})` : "— 暂无样本"}</span><button onClick={() => setHistoryOpen(true)} disabled={busy}>历史 {history.length || ""}</button><button onClick={newResearch} disabled={busy}>＋ 新对话</button></div>
    </header>

    {historyOpen && <div className="agent-history-backdrop" role="presentation" onMouseDown={(event) => { if (event.currentTarget === event.target) setHistoryOpen(false); }}>
      <section className="agent-history-panel" role="dialog" aria-modal="true" aria-label="Agent 历史记录">
        <header><div><span className="eyebrow">RESEARCH HISTORY</span><h2>历史研究</h2><p>打开可查看完整记录；“基于此继续”会新建任务并重新取得最新数据。</p></div><button aria-label="关闭历史记录" onClick={() => setHistoryOpen(false)}>×</button></header>
        <div className="agent-history-list">{history.length ? history.map((saved) => <article className={saved.conversation_id === sessionId ? "current" : ""} key={saved.conversation_id}>
          <button className="history-main" disabled={historyLoading} onClick={() => void openHistory(saved)}><b>{saved.title}</b><span>{historyTime(saved.updated_at)} · {phaseLabel[saved.phase ?? "idle"]}</span><small>{saved.message_count} 条记录 · {saved.evidence_count} 条证据{saved.parent_conversation_id ? " · 研究分支" : ""}</small></button>
          <div><button disabled={historyLoading} onClick={() => void continueHistory(saved)}>基于此继续</button><button onClick={() => void renameHistory(saved)}>重命名</button><button className="danger" aria-label={`删除 ${saved.title || "历史研究"}`} onClick={() => void deleteHistory(saved)}>删除</button></div>
        </article>) : <div className="agent-history-empty"><b>还没有历史研究</b><p>发送第一条研究任务后会自动保存在本机，切换页面或重启桌面应用也不会丢失。</p></div>}</div>
        <footer><span>最多保留最近 {MAX_HISTORY_ITEMS} 项 · 不保存 API Key</span><button onClick={newResearch}>开始新对话</button></footer>
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
          return <article key={`${effect.call_id ?? effect.kind}-${index}`}><i className={effect.cache_hit ? "cache" : "running"} /><div><b>{title}</b><p>{detail}</p><small>{effect.call_id && `调用 ${effect.call_id}`}{effect.cache_hit && " · 命中缓存"}{effect.evidence_count ? ` · ${effect.evidence_count} 条证据` : ""}</small></div></article>;
        }) : <div className="activity-empty"><b>等待任务</b><p>开始研究后，这里会按顺序展示计划、工具调用、缓存、证据与报告校验，不显示模型私有推理链。</p></div>}
      </div>
      <footer><span>已取得证据</span><b>{task?.evidence_ids?.length ?? 0}</b><span>完成工具</span><b>{task?.completed_tool_count ?? 0}</b></footer>
    </aside>

    <footer className="agent-composer-wrap">
      {busyStage && <div className="agent-busy-stage"><span className="send-spinner" />{busyStage}</div>}
      {error && <div className="agent-error"><span>{error}</span><div>{task?.phase === "preparing" && <button onClick={() => void executeResearch(task)} disabled={busy}>重试研究</button>}<button onClick={() => setError(null)}>×</button></div></div>}
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
