import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";
import type {
  AgentReasoningDepth,
  AgentReport,
  AgentResearchMode,
  AgentStreamEnvelope,
} from "./lib/api";
import {
  emptyClarificationDraft,
  hasClarification,
  type ClarificationDraft,
} from "./lib/agentClarification";

export const DEFAULT_AGENT_TOOLS = [
  "get_quote",
  "get_kline",
  "compute_indicators",
  "run_full_analysis",
  "run_chanlun",
  "get_fund_flow",
  "get_market_breadth",
  "get_market_regime",
  "search_stock",
  "compare_stocks",
  "scan_market",
  "get_watchlist",
  "get_cached_detail",
  "get_fundamentals",
  "run_valuation",
  "get_industry_chain",
  "run_supply_chain_shock",
  "build_relationship_graph",
  "run_backtest",
  "iterate_strategy",
  "run_joinquant_research",
  "search_web",
  "research_news",
] as const;

export interface ToolCallItem {
  key: number;
  callId?: string;
  name: string;
  args?: string;
  done: boolean;
  cacheKey?: string;
  elapsedMs?: number;
  source?: string;
  fetchedAt?: string;
  startedAt?: number;
  position?: number;
  total?: number;
  success?: boolean;
  error?: string;
  /** Typical duration for display only; never a cancellation deadline. */
  estimatedMs?: number;
  stage?: string;
  lastProgressAt?: number;
}

export interface ChatMsg {
  key: number;
  role: "user" | "assistant";
  /** User-visible text only; private reasoning is stripped before rendering. */
  raw: string;
  tools: ToolCallItem[];
  report?: AgentReport;
  suspendedAt?: number;
  failed?: string;
  done: boolean;
  /** Persisted form state for an Agent clarification card. */
  clarificationDraft?: ClarificationDraft;
  clarificationSubmitted?: boolean;
}

export type RunStatus =
  | "idle"
  | "running"
  | "waiting_input"
  | "suspended"
  | "completed"
  | "failed"
  | "cancelled";

export interface AgentProgress {
  phase: string;
  message: string;
  round: number;
  maxRounds: number;
  completed: number | null;
  total: number | null;
  updatedAt: number;
}

interface AgentSessionState {
  msgs: ChatMsg[];
  input: string;
  status: RunStatus;
  compactionCount: number;
  taskId: string | null;
  conversationId: string | null;
  lastSeq: number;
  err: string | null;
  progress: AgentProgress | null;
  /** Follow-up questions accepted while the current background run continues. */
  pendingQuestions: string[];
  researchMode: AgentResearchMode;
  reasoningDepth: AgentReasoningDepth;
  enabledTools: string[];
  autoResumeOnQuota: boolean;
  setInput: (input: string) => void;
  setStatus: (status: RunStatus) => void;
  setErr: (err: string | null) => void;
  setMsgs: (msgs: ChatMsg[]) => void;
  setResearchMode: (mode: AgentResearchMode) => void;
  setReasoningDepth: (depth: AgentReasoningDepth) => void;
  setEnabledTools: (tools: string[]) => void;
  setAutoResumeOnQuota: (enabled: boolean) => void;
}

let keySeq = Date.now();
export const nextAgentKey = () => keySeq++;

export const useAgentSession = create<AgentSessionState>()(
  persist(
    (set) => ({
      msgs: [],
      input: "",
      status: "idle",
      compactionCount: 0,
      taskId: null,
      conversationId: null,
      lastSeq: 0,
      err: null,
      progress: null,
      pendingQuestions: [],
      researchMode: "deep",
      reasoningDepth: "deep",
      enabledTools: [...DEFAULT_AGENT_TOOLS],
      autoResumeOnQuota: true,
      setInput: (input) => set({ input }),
      setStatus: (status) => set({ status }),
      setErr: (err) => set({ err }),
      setMsgs: (msgs) => set({ msgs }),
      setResearchMode: (researchMode) => set({ researchMode }),
      setReasoningDepth: (reasoningDepth) => set({ reasoningDepth }),
      setEnabledTools: (enabledTools) => set({ enabledTools }),
      setAutoResumeOnQuota: (autoResumeOnQuota) => set({ autoResumeOnQuota }),
    }),
    {
      name: "astock-agent-session-v2",
      version: 4,
      storage: createJSONStorage(() => window.localStorage),
      migrate: (persisted, version) => {
        const state = persisted as Partial<AgentSessionState>;
        const enabled = Array.isArray(state.enabledTools) ? state.enabledTools : [];
        const legacyDefaults = DEFAULT_AGENT_TOOLS.slice(0, 20);
        // Existing users who had the former complete tool set enabled should
        // automatically receive newly installed research capabilities. A
        // customized allowlist remains untouched.
        const hadAllLegacyTools = legacyDefaults.every((tool) => enabled.includes(tool));
        return {
          ...state,
          enabledTools:
            version < 3 && hadAllLegacyTools ? [...DEFAULT_AGENT_TOOLS] : enabled,
        } as AgentSessionState;
      },
      partialize: (state) => ({
        msgs: state.msgs,
        input: state.input,
        status: state.status,
        compactionCount: state.compactionCount,
        taskId: state.taskId,
        conversationId: state.conversationId,
        lastSeq: state.lastSeq,
        err: state.err,
        progress: state.progress,
        pendingQuestions: state.pendingQuestions,
        researchMode: state.researchMode,
        reasoningDepth: state.reasoningDepth,
        enabledTools: state.enabledTools,
        autoResumeOnQuota: state.autoResumeOnQuota,
      }),
    },
  ),
);

export function patchLastAssistant(patch: (message: ChatMsg) => ChatMsg) {
  const state = useAgentSession.getState();
  const messages = state.msgs;
  for (let index = messages.length - 1; index >= 0; index--) {
    if (messages[index].role === "assistant") {
      const next = [...messages];
      next[index] = patch(next[index]);
      useAgentSession.setState({ msgs: next });
      return;
    }
  }
}

/** Update one persisted clarification card even while its route is unmounted. */
export function patchClarificationDraft(
  messageKey: number,
  patch: (draft: ClarificationDraft) => ClarificationDraft,
) {
  const state = useAgentSession.getState();
  useAgentSession.setState({
    msgs: state.msgs.map((message) =>
      message.key === messageKey
        ? {
            ...message,
            clarificationDraft: patch(
              message.clarificationDraft ?? emptyClarificationDraft(),
            ),
          }
        : message,
    ),
  });
}

/** Stable channel callback: it lives outside any route component. */
export function handleAgentEnvelope(message: AgentStreamEnvelope) {
  const state = useAgentSession.getState();
  if (message.seq <= state.lastSeq && state.taskId === message.run_id) return;
  useAgentSession.setState({
    taskId: message.run_id,
    conversationId: message.conversation_id,
    lastSeq: message.seq,
  });

  const event = message.event;
  switch (event.type) {
    case "progress":
      useAgentSession.setState({
        status: "running",
        progress: {
          phase: event.phase,
          message: event.message,
          round: event.round,
          maxRounds: event.max_rounds,
          completed: event.completed,
          total: event.total,
          updatedAt: Date.now(),
        },
      });
      break;
    case "context_compacted":
      useAgentSession.setState({ compactionCount: state.compactionCount + 1 });
      break;
    case "text_delta":
      useAgentSession.setState({ status: "running" });
      patchLastAssistant((item) => ({
        ...item,
        raw: item.raw + event.text,
        suspendedAt: undefined,
      }));
      break;
    case "text_reset":
      patchLastAssistant((item) => ({
        ...item,
        raw: "",
        failed: undefined,
      }));
      break;
    case "tool_call_started":
      patchLastAssistant((item) => ({
        ...item,
        tools: [
          ...item.tools,
          {
            key: nextAgentKey(),
            callId: event.call_id,
            name: event.name,
            args:
              event.args == null
                ? undefined
                : typeof event.args === "string"
                  ? event.args
                  : JSON.stringify(event.args),
            done: false,
            startedAt: Date.now(),
            position: event.position,
            total: event.total,
            estimatedMs: event.estimated_ms,
            stage: "检查本地缓存并选择可用数据源",
          },
        ],
      }));
      break;
    case "tool_call_progress":
      patchLastAssistant((item) => ({
        ...item,
        tools: item.tools.map((tool) =>
          tool.callId === event.call_id
            ? {
                ...tool,
                elapsedMs: event.elapsed_ms,
                estimatedMs: event.estimated_ms,
                stage: event.stage,
                lastProgressAt: Date.now(),
              }
            : tool,
        ),
      }));
      break;
    case "tool_call_finished":
      patchLastAssistant((item) => {
        const tools = [...item.tools];
        for (let index = tools.length - 1; index >= 0; index--) {
          if (
            !tools[index].done &&
            (tools[index].callId === event.call_id ||
              (!tools[index].callId && tools[index].name === event.name))
          ) {
            tools[index] = {
              ...tools[index],
              done: true,
              cacheKey: event.cache_key,
              elapsedMs: event.elapsed_ms,
              success: event.success,
              source: event.source ?? undefined,
              fetchedAt: event.fetched_at ?? undefined,
              error: event.error ?? undefined,
            };
            break;
          }
        }
        return { ...item, tools };
      });
      break;
    case "suspended":
      useAgentSession.setState({ status: "suspended" });
      patchLastAssistant((item) => ({
        ...item,
        suspendedAt: event.reason.reset_at_unix ?? undefined,
      }));
      break;
    case "completed":
      useAgentSession.setState({
        status: hasClarification(event.report.answer) ? "waiting_input" : "completed",
        progress: null,
      });
      patchLastAssistant((item) => {
        const evidenceByCache = new Map(
          event.report.evidence.map((evidence) => [evidence.cache_key, evidence]),
        );
        return {
          ...item,
          report: event.report,
          clarificationDraft: hasClarification(event.report.answer)
            ? item.clarificationDraft ?? emptyClarificationDraft()
            : item.clarificationDraft,
          suspendedAt: undefined,
          done: true,
          tools: item.tools.map((tool) => {
            const evidence = tool.cacheKey ? evidenceByCache.get(tool.cacheKey) : undefined;
            return evidence
              ? { ...tool, source: evidence.source, fetchedAt: evidence.fetched_at }
              : tool;
          }),
        };
      });
      break;
    case "failed":
      useAgentSession.setState({ status: "failed", progress: null });
      patchLastAssistant((item) => ({ ...item, failed: event.error, done: true }));
      break;
  }
}

export function appendAgentTurn(question: string) {
  const state = useAgentSession.getState();
  const maxRounds =
    state.reasoningDepth === "maximum"
      ? 48
      : state.researchMode === "quick"
        ? 12
        : state.researchMode === "plan"
          ? 40
          : 32;
  useAgentSession.setState({
    input: "",
    err: null,
    status: "running",
    lastSeq: 0,
    progress: {
      phase: "preparing",
      message: "正在创建后台研究任务",
      round: 1,
      maxRounds,
      completed: null,
      total: null,
      updatedAt: Date.now(),
    },
    msgs: [
      ...state.msgs,
      { key: nextAgentKey(), role: "user", raw: question, tools: [], done: true },
      { key: nextAgentKey(), role: "assistant", raw: "", tools: [], done: false },
    ],
  });
}

export function setAgentRunIdentity(taskId: string, conversationId: string) {
  useAgentSession.setState({ taskId, conversationId, status: "running" });
}

export function selectAgentTask(taskId: string, conversationId: string, status: RunStatus) {
  useAgentSession.setState({ taskId, conversationId, status, lastSeq: 0 });
}

export function resetAgentSession() {
  useAgentSession.setState({
    msgs: [],
    input: "",
    status: "idle",
    compactionCount: 0,
    taskId: null,
    conversationId: null,
    lastSeq: 0,
    err: null,
    progress: null,
    pendingQuestions: [],
  });
}
