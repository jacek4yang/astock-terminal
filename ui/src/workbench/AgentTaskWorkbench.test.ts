import { beforeEach, describe, expect, it, vi } from "vitest";

const { requestNative } = vi.hoisted(() => ({ requestNative: vi.fn() }));

vi.mock("../bridge", () => ({
  isProton: () => true,
  requestNative,
}));

import { deterministicVerificationSummary, expandAgentActivities, requestDurableAgent, workerProgressMatchesTask } from "./AgentTaskWorkbench";

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

describe("durable Agent operation journal", () => {
  beforeEach(() => requestNative.mockReset());

  it("commits intent before Worker and result plus checkpoint before returning", async () => {
    requestNative.mockImplementation(async (target: string, kind: string) => {
      if (kind === "agent.effect.list") return { items: [] };
      if (target === "agent" && kind === "agent.start") {
        return {
          state: { task_id: "task-1", accepted_seq: 2, phase: "waiting_for_user" },
          checkpoint: { task_id: "task-1", accepted_seq: 2 },
        };
      }
      if (kind === "agent.task.load") return { task: { accepted_seq: 0 }, events: [{ seq: 1 }] };
      return { inserted: true };
    });

    const reply = await requestDurableAgent<{
      state?: { accepted_seq?: number };
      checkpoint?: unknown;
    }>(
      "agent.start",
      { task_id: "task-1", seq: 1, spec },
      "task-1",
      0,
      120_000,
      spec,
    );

    expect(reply.state?.accepted_seq).toBe(2);
    expect(requestNative.mock.calls.map((call) => `${call[0]}:${call[1]}`)).toEqual([
      "engine:agent.task.create",
      "engine:agent.event.append",
      "engine:agent.effect.list",
      "engine:agent.effect.begin",
      "agent:agent.start",
      "engine:agent.effect.complete",
      "engine:agent.task.load",
      "engine:agent.event.append",
      "engine:agent.checkpoint.put",
    ]);
  });

  it("reconciles a pending research workflow through a journaled retry", async () => {
    requestNative.mockImplementation(async (_target: string, kind: string) => {
      if (kind === "agent.effect.list") {
        return {
          items: [{
            effect_id: "fx-existing",
            effect_kind: "agent.research.workflow",
            status: "pending",
            idempotency_key: "task-2:agent.research.workflow:3",
          }],
        };
      }
      return { inserted: true };
    });

    await expect(requestDurableAgent(
      "agent.research.workflow",
      { task_id: "task-2", context: {} },
      "task-2",
      3,
      900_000,
    )).resolves.toEqual({ inserted: true });
    expect(requestNative.mock.calls.some((call) => call[0] === "agent")).toBe(true);
    expect(requestNative.mock.calls).toContainEqual([
      "engine",
      "agent.effect.begin",
      expect.objectContaining({ idempotency_key: "task-2:agent.research.workflow:3:retry:1" }),
    ]);
  });

  it("backfills a missing checkpoint from an already completed durable result", async () => {
    const completedReply = {
      state: { task_id: "task-replay", accepted_seq: 3, phase: "preparing" },
      checkpoint: { task_id: "task-replay", accepted_seq: 3, phase: "preparing" },
    };
    requestNative.mockImplementation(async (target: string, kind: string) => {
      if (kind === "agent.effect.list") {
        return {
          items: [{
            effect_id: "fx-completed",
            effect_kind: "agent.event",
            status: "succeeded",
            idempotency_key: "task-replay:agent.event:2",
            result: completedReply,
          }],
        };
      }
      if (kind === "agent.task.load") return { task: { accepted_seq: 1 }, events: [{ seq: 1 }, { seq: 2 }] };
      if (target === "agent") throw new Error("completed operations must not call the Worker again");
      return { inserted: true };
    });

    await expect(requestDurableAgent(
      "agent.event",
      { task_id: "task-replay", seq: 2, event_kind: "resume" },
      "task-replay",
      1,
      120_000,
    )).resolves.toEqual(completedReply);
    expect(requestNative.mock.calls.map((call) => `${call[0]}:${call[1]}`)).toEqual([
      "engine:agent.event.append",
      "engine:agent.effect.list",
      "engine:agent.task.load",
      "engine:agent.event.append",
      "engine:agent.checkpoint.put",
    ]);
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
