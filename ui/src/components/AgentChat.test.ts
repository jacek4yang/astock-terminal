import { describe, expect, it } from "vitest";
import type { AgentMessage } from "../lib/api";
import {
  historyToMsgs,
  stripPrivateReasoning,
  taskRunStatus,
  type ChatMsg,
} from "./AgentChat";

function message(overrides: Partial<AgentMessage>): AgentMessage {
  return {
    id: "m1",
    role: "assistant",
    content: "answer",
    tool_calls: [],
    tool_call_id: null,
    created_at: 1,
    malformed: false,
    ...overrides,
  };
}

describe("Agent history safety", () => {
  it("loads a normalized malformed legacy record as safe text", () => {
    const out: ChatMsg[] = [];
    const rows = historyToMsgs(
      message({ content: "{bad legacy payload}", malformed: true }),
      out,
    );
    expect(rows).toHaveLength(1);
    expect(rows[0].raw).toBe("{bad legacy payload}");
    expect(rows[0].failed).toContain("安全加载");
  });

  it("never renders provider-private reasoning", () => {
    expect(stripPrivateReasoning("可见前<think>私有推理</think>可见后")).toBe("可见前可见后");
    expect(stripPrivateReasoning("回答<think>尚未结束")).toBe("回答");
  });

  it("reconciles durable background-task states after reopening the app", () => {
    expect(taskRunStatus("completed")).toBe("completed");
    expect(taskRunStatus("interrupted")).toBe("suspended");
    expect(taskRunStatus("running")).toBe("running");
    expect(taskRunStatus("unknown")).toBe("idle");
  });
});
