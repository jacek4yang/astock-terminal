import { requestNative } from "../bridge";
import { AGENT_SERVICE_METHODS, type ConversationSummary } from "../bridge/generated";

export type DurableAgentKind = "agent.start" | "agent.event" | "agent.research.workflow";

export type DurableTaskView = {
  task: { accepted_seq: number; checkpoint?: unknown };
  events: Array<{ seq: number }>;
};

export type StoredAgentConversation<TSession = unknown> = ConversationSummary & {
  session: TSession;
};

export const publicAgentTaskMethods = new Set<string>(AGENT_SERVICE_METHODS);

/**
 * Stable renderer service facade. The wire remains protocol v1 compatible:
 * Host owns stateful Agent journaling, while durable history reads stay in the
 * Engine. Callers never receive event/effect/checkpoint write primitives.
 */
export const agentTaskService = {
  create<T>(payload: Record<string, unknown>, deadlineMs = 120_000): Promise<T> {
    return requestNative<T>("agent", "agent.start", payload, { deadlineMs });
  },

  list(limit: number, query?: string): Promise<{ items: ConversationSummary[] }> {
    return requestNative("engine", "agent.conversation.list", { limit, ...(query ? { query } : {}) });
  },

  get(taskId: string): Promise<DurableTaskView> {
    return requestNative("engine", "agent.task.load", { task_id: taskId });
  },

  getConversation<TSession = unknown>(conversationId: string): Promise<StoredAgentConversation<TSession>> {
    return requestNative("engine", "agent.conversation.load", { conversation_id: conversationId });
  },

  branch<TSession = unknown>(payload: {
    source_conversation_id: string;
    new_conversation_id: string;
    message_id: string;
    title: string;
  }): Promise<StoredAgentConversation<TSession>> {
    return requestNative("engine", "agent.conversation.branch", payload);
  },

  resume<T>(payload: Record<string, unknown>, deadlineMs = 900_000): Promise<T> {
    return requestNative<T>("agent", "agent.research.workflow", payload, { deadlineMs });
  },

  cancel<T>(taskId: string, acceptedSeq: number, deadlineMs = 120_000): Promise<T> {
    return requestNative<T>("agent", "agent.event", {
      task_id: taskId,
      seq: Math.max(1, acceptedSeq) + 1,
      event_kind: "cancel",
    }, { deadlineMs });
  },

  answer<T>(payload: Record<string, unknown>, deadlineMs = 120_000): Promise<T> {
    return requestNative<T>("agent", "agent.event", {
      ...payload,
      event_kind: "clarification_answered",
    }, { deadlineMs });
  },

  transition<T>(kind: DurableAgentKind, payload: Record<string, unknown>, deadlineMs: number): Promise<T> {
    return requestNative<T>("agent", kind, payload, { deadlineMs });
  },
};
