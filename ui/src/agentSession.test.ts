import { beforeEach, describe, expect, it } from "vitest";
import {
  appendAgentTurn,
  handleAgentEnvelope,
  resetAgentSession,
  useAgentSession,
} from "./agentSession";

describe("persistent Agent session channel", () => {
  beforeEach(() => resetAgentSession());

  it("keeps parallel tool progress matched by call id outside a mounted page", () => {
    appendAgentTurn("全面分析 300308");
    handleAgentEnvelope({
      run_id: "run-1",
      conversation_id: "conv-1",
      seq: 1,
      event: {
        type: "tool_call_started",
        call_id: "call-a",
        name: "get_quote",
        args: { symbol: "300308" },
        position: 1,
        total: 2,
        timeout_ms: 45_000,
      },
    });
    handleAgentEnvelope({
      run_id: "run-1",
      conversation_id: "conv-1",
      seq: 2,
      event: {
        type: "tool_call_started",
        call_id: "call-b",
        name: "get_quote",
        args: { symbol: "000001" },
        position: 2,
        total: 2,
        timeout_ms: 45_000,
      },
    });
    handleAgentEnvelope({
      run_id: "run-1",
      conversation_id: "conv-1",
      seq: 3,
      event: {
        type: "tool_call_progress",
        call_id: "call-b",
        name: "get_quote",
        elapsed_ms: 2_000,
        timeout_ms: 45_000,
        stage: "等待数据源返回并执行确定性计算",
      },
    });
    handleAgentEnvelope({
      run_id: "run-1",
      conversation_id: "conv-1",
      seq: 4,
      event: {
        type: "tool_call_finished",
        call_id: "call-a",
        name: "get_quote",
        cache_key: "quote:300308",
        elapsed_ms: 812,
        success: true,
        source: "tdx+eastmoney",
        fetched_at: "2026-08-22T10:00:00+08:00",
        error: null,
      },
    });

    const state = useAgentSession.getState();
    expect(state.taskId).toBe("run-1");
    expect(state.conversationId).toBe("conv-1");
    const tools = [...state.msgs].reverse().find((message) => message.role === "assistant")!.tools;
    expect(tools).toHaveLength(2);
    expect(tools[0]).toMatchObject({ callId: "call-a", done: true, source: "tdx+eastmoney" });
    expect(tools[1]).toMatchObject({
      callId: "call-b",
      done: false,
      elapsedMs: 2_000,
      timeoutMs: 45_000,
      stage: "等待数据源返回并执行确定性计算",
    });
  });
});
