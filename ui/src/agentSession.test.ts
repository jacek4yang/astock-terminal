import { beforeEach, describe, expect, it } from "vitest";
import {
  appendToolTimeline,
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
        estimated_ms: 45_000,
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
        estimated_ms: 45_000,
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
        estimated_ms: 45_000,
        stage: "等待数据源返回并执行确定性计算",
        detail: {
          completed: 1,
          total: 50,
          succeeded: 1,
          failed: 0,
          cache_hits: 0,
          records: 250,
          active: [{ label: "300308 中际旭创", stage: "获取250根日K并计算指标" }],
          recent_errors: [],
        },
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
    expect(tools[0].timeline?.map((entry) => entry.kind)).toEqual(["started", "success"]);
    expect(tools[1]).toMatchObject({
      callId: "call-b",
      done: false,
      elapsedMs: 2_000,
      estimatedMs: 45_000,
      stage: "等待数据源返回并执行确定性计算",
      progressDetail: {
        completed: 1,
        total: 50,
        succeeded: 1,
        failed: 0,
        cache_hits: 0,
        records: 250,
        active: [{ label: "300308 中际旭创", stage: "获取250根日K并计算指标" }],
        recent_errors: [],
      },
    });
    expect(tools[1].timeline?.map((entry) => entry.kind)).toEqual(["started", "progress"]);
  });

  it("compacts repeated heartbeat diagnostics while retaining the first entry", () => {
    let timeline = appendToolTimeline(undefined, {
      at: 1,
      kind: "started",
      message: "开始",
    });
    timeline = appendToolTimeline(timeline, {
      at: 2,
      kind: "progress",
      message: "等待上游",
      elapsedMs: 1_000,
    });
    timeline = appendToolTimeline(timeline, {
      at: 3,
      kind: "progress",
      message: "等待上游",
      elapsedMs: 2_000,
    });
    expect(timeline).toHaveLength(2);
    expect(timeline[0]).toMatchObject({ kind: "started", message: "开始" });
    expect(timeline[1]).toMatchObject({ at: 3, elapsedMs: 2_000 });
  });

  it("persists a completed clarification as waiting for user input", () => {
    appendAgentTurn("帮我制定两万元计划");
    const answer = `\`\`\`astock-questions
{"title":"请确认","questions":[{"id":"risk","question":"风险偏好？","options":["保守","平衡"]}]}
\`\`\``;
    handleAgentEnvelope({
      run_id: "run-input",
      conversation_id: "conv-input",
      seq: 1,
      event: {
        type: "completed",
        report: {
          task_id: "run-input",
          answer,
          conclusions: [],
          evidence: [],
          generated_at: 1_777_000_000,
        },
      },
    });
    const state = useAgentSession.getState();
    expect(state.status).toBe("waiting_input");
    expect(state.msgs.at(-1)?.clarificationDraft).toEqual({ selections: {}, other: {} });
  });
});
