import { beforeEach, describe, expect, it, vi } from "vitest";

const { requestNative } = vi.hoisted(() => ({ requestNative: vi.fn() }));

vi.mock("../bridge", () => ({
  isProton: () => true,
  requestNative,
}));

import { deterministicVerificationSummary, durableCheckpointState, expandAgentActivities, requestDurableAgent, workerProgressMatchesTask } from "./AgentTaskWorkbench";

const spec = {
  objective: "分析两万元最新投资计划",
  security_universe: [],
  as_of: "",
  research_start: "",
  research_end: "",
  investment_horizon: "",
  comparison_benchmark: "",
  output_type: "manual_plan" as const,
  evidence_requirement: "strict" as const,
};

describe("Host-owned durable Agent bridge", () => {
  beforeEach(() => requestNative.mockReset());

  it("submits one public Agent call and cannot write Engine journal primitives", async () => {
    requestNative.mockResolvedValue({
      state: { task_id: "task-1", accepted_seq: 2, phase: "waiting_for_user" },
      checkpoint: { task_id: "task-1", accepted_seq: 2 },
    });

    const reply = await requestDurableAgent<{
      state?: { accepted_seq?: number };
      checkpoint?: unknown;
    }>(
      "agent.start",
      { task_id: "task-1", seq: 1, spec },
      120_000,
    );

    expect(reply.state?.accepted_seq).toBe(2);
    expect(requestNative).toHaveBeenCalledTimes(1);
    expect(requestNative).toHaveBeenCalledWith(
      "agent",
      "agent.start",
      { task_id: "task-1", seq: 1, spec },
      { deadlineMs: 120_000 },
    );
  });

  it("formats deterministic report verification without inventing totals", () => {
    expect(deterministicVerificationSummary({
      version: "engine-report-verifier-v1",
      distinct_citations: 12,
      numeric_claims_checked: 37,
      registry_facts: 6_000,
    })).toBe("复现 37 个数字 · 12 个不同证据引用 · 6000 条字段事实");
    expect(deterministicVerificationSummary({})).toBe("复现 0 个数字 · 0 个不同证据引用 · 0 条字段事实");
  });

  it("routes Worker progress by durable Agent task id, not conversation id", () => {
    expect(workerProgressMatchesTask({ state: { task_id: "task-1" } }, "task-1")).toBe(true);
    expect(workerProgressMatchesTask({ state: { task_id: "task-1" } }, "conversation-1")).toBe(false);
    expect(workerProgressMatchesTask({ stage: "diagnostic" }, "task-1")).toBe(true);
  });

  it("accepts recovery state only from a matching Engine journal checkpoint", () => {
    expect(durableCheckpointState({
      task: { accepted_seq: 4, checkpoint: { task_id: "task-1", accepted_seq: 4, phase: "suspended" } },
      events: [{ seq: 1 }, { seq: 4 }],
    }, "task-1")).toMatchObject({ task_id: "task-1", accepted_seq: 4, phase: "suspended" });

    expect(() => durableCheckpointState({
      task: { accepted_seq: 4, checkpoint: { task_id: "forged", accepted_seq: 4 } },
      events: [],
    }, "task-1")).toThrow("持久化检查点与任务不匹配");
    expect(() => durableCheckpointState({
      task: { accepted_seq: 4, checkpoint: { task_id: "task-1", accepted_seq: 3 } },
      events: [],
    }, "task-1")).toThrow("持久化检查点序列与任务日志不一致");
    expect(() => durableCheckpointState({ task: { accepted_seq: 4 }, events: [] }, "task-1"))
      .toThrow("持久化任务没有可恢复检查点");
  });

  it("expands advanced Engine modules into visible success, skip and failure activities", () => {
    const activities = expandAgentActivities([{
      kind: "execute_tool",
      call_id: "security-context-1",
      module_activities: [
        { module: "earnings_driver", scope: "300308", status: "succeeded", error: null },
        { module: "relationship", scope: "portfolio", status: "skipped", error: "relationship_requires_two_symbols" },
        { module: "industry_graph", scope: "300308", status: "failed", error: "provider_unavailable" },
      ],
    }]);
    expect(activities).toHaveLength(4);
    expect(activities.slice(1).map((item) => item.title)).toEqual([
      "盈利驱动树 完成",
      "跨证券关系 已跳过",
      "产业关系图谱 失败",
    ]);
    expect(activities[2]).toMatchObject({ status: "skipped", detail: expect.stringContaining("relationship_requires_two_symbols") });
    expect(activities[3]).toMatchObject({ status: "failed", detail: expect.stringContaining("provider_unavailable") });
  });
});
