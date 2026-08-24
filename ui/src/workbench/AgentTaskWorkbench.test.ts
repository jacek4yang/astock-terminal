import { beforeEach, describe, expect, it, vi } from "vitest";

const { requestNative } = vi.hoisted(() => ({ requestNative: vi.fn() }));

vi.mock("../bridge", () => ({
  isProton: () => true,
  requestNative,
}));

import { compactSecurityEvidence, requestDurableAgent, requestDurableTool } from "./AgentTaskWorkbench";

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

  it("does not duplicate a provider call while the same intent is pending", async () => {
    requestNative.mockImplementation(async (_target: string, kind: string) => {
      if (kind === "agent.effect.list") {
        return {
          items: [{
            effect_id: "fx-existing",
            effect_kind: "agent.research",
            status: "pending",
            idempotency_key: "task-2:agent.research:3",
          }],
        };
      }
      return { inserted: true };
    });

    await expect(requestDurableAgent(
      "agent.research",
      { task_id: "task-2", context: {} },
      "task-2",
      3,
      900_000,
    )).rejects.toThrow("pending");
    expect(requestNative.mock.calls.some((call) => call[0] === "agent")).toBe(false);
  });

  it("journals Engine tool intent and reuses its verified result", async () => {
    const quote = { quote: { symbol: "603927", price: 18.6 }, source: "TDX" };
    requestNative.mockImplementation(async (_target: string, kind: string) => {
      if (kind === "agent.effect.list") return { items: [] };
      if (kind === "market.quote") return quote;
      return { inserted: true };
    });
    await expect(requestDurableTool(
      "task-3",
      2,
      "603927-quote",
      "market.quote",
      { symbol: "603927" },
      60_000,
    )).resolves.toEqual(quote);
    expect(requestNative.mock.calls.map((call) => call[1])).toEqual([
      "agent.effect.list",
      "agent.effect.begin",
      "market.quote",
      "agent.effect.complete",
    ]);

    requestNative.mockReset();
    requestNative.mockResolvedValue({
      items: [{
        effect_id: "tool-existing",
        effect_kind: "engine.market.quote",
        status: "succeeded",
        result: quote,
        idempotency_key: "task-3:tool:603927-quote",
      }],
    });
    await expect(requestDurableTool(
      "task-3",
      2,
      "603927-quote",
      "market.quote",
      { symbol: "603927" },
      60_000,
    )).resolves.toEqual(quote);
    expect(requestNative).toHaveBeenCalledTimes(1);
  });
});

describe("Agent optional-source evidence", () => {
  it("keeps capability failures and bounds only repetitive provider rows", () => {
    const rows = Array.from({ length: 300 }, (_, index) => ({ date: `day-${index}`, close: index }));
    const compacted = compactSecurityEvidence({
      symbol: "600519",
      market: {},
      fundamentals: {},
      events: {},
      news: { items: [], successful_sources: [], stale_sources: [], errors: [] },
      reconciliation: {},
      joinquant: {},
      optionalSources: {
        configured: { tushare: true, iwencai: false, sec_edgar: false },
        capabilities: { tushare_raw_daily: true, tushare_pro: false },
        datasets: {
          tushare_raw_daily: { ok: true, rows, total_rows: 300, source: "Tushare" },
          tushare_daily_basic: { ok: false, rows: [], error: "积分不足2000" },
          iwencai_stock_events: { ok: false, data: null, error: "未配置" },
        },
      },
    }, false) as Record<string, any>;

    expect(compacted.optional_sources.datasets.tushare_raw_daily.rows).toHaveLength(250);
    expect(compacted.optional_sources.datasets.tushare_raw_daily.total_rows).toBe(300);
    expect(compacted.optional_sources.datasets.tushare_daily_basic.error).toContain("积分不足");
    expect(compacted.optional_sources.datasets.iwencai_stock_events.error).toBe("未配置");
    expect(compacted.optional_sources.capabilities.tushare_pro).toBe(false);
  });
});
