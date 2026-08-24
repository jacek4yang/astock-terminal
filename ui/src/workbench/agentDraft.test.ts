import { beforeEach, describe, expect, it, vi } from "vitest";
import { consumeAgentDraft, queueAgentDraft, subscribeAgentDraft } from "./agentDraft";

describe("v6 Agent draft handoff", () => {
  beforeEach(() => consumeAgentDraft());

  it("moves presentation text once without persisting task state", () => {
    expect(queueAgentDraft("  分析 300308 的最新风险  ")).toBe(true);
    expect(consumeAgentDraft()).toBe("分析 300308 的最新风险");
    expect(consumeAgentDraft()).toBeNull();
    expect(queueAgentDraft("   ")).toBe(false);
  });

  it("notifies a mounted Agent composer and supports clean unsubscribe", () => {
    const listener = vi.fn();
    const unsubscribe = subscribeAgentDraft(listener);
    queueAgentDraft("核验公告证据");
    expect(listener).toHaveBeenCalledWith("核验公告证据");
    unsubscribe();
    queueAgentDraft("不应再次通知");
    expect(listener).toHaveBeenCalledTimes(1);
  });
});
