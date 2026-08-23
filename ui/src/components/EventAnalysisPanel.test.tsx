import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { EventAnalysisSnapshot } from "../lib/api";
import { EventAnalysisPanel, eventBps } from "./EventAnalysisPanel";

const snapshot: EventAnalysisSnapshot = {
  job_id: "event-test",
  revision_id: "revision:test",
  security_code: "600000",
  running: false,
  status: "completed",
  phase: "结构化事件与市场定价分析已完成",
  progress: 100,
  current_item: "基本面结论与股价机会已分开输出",
  estimated_remaining_seconds: 0,
  recent_logs: ["已读取 180 条个股行情", "缺失项保持为空"],
  error: null,
  started_at: 1_800_000_000,
  updated_at: 1_800_000_010,
  result: {
    event: {
      event_id: "event:1", source_revision_id: "revision:test", kind: "order", title: "重大合同",
      subjects: [{ entity_id: "entity:1", name: "测试公司", listed_code: "600000", role: "subject" }], objects: [],
      amount_text: "2 亿元", quantity_text: null, unit_original: "亿元", currency_original: "人民币", baseline_period: null,
      starts_at: null, ends_at: null, region: null, conditions: [], official_effective: true,
      reversibility: "conditional", impact_horizon: "quarter", lifecycle: "confirmed",
      catalyst_path: ["核验合同生效", "跟踪季度收入确认"], validation_dates: [], invalidation_conditions: ["合同取消或重大修订"],
      missing_fields: ["数量"], extraction_version: "fixture-v1", created_at: 1_800_000_000, updated_at: 1_800_000_000,
      evidence: [{ evidence_id: "evidence:1", event_id: "event:1", field_name: "金额", provenance: "observed_fact", source_revision_id: "revision:test", source_version_id: null, quote_original: "合同金额2亿元", quote_zh: null, location: {}, observed_at: 1_800_000_000, confidence_bps: 9500 }],
    },
    timeline: [],
    assessment: {
      assessment_id: "assessment:1", event_id: "event:1", security_code: "600000", as_of_date: "2026-08-22", created_at: 1_800_000_010,
      fundamental: { direction: "positive", impact_bps: 800, quantifiable: true, rationale: "经营影响偏正。", provenance: "observed_fact" },
      market_opportunity: { price_in_state: "mostly_priced", opportunity: "机会偏中性", price_in_score: 76, rationale: "多数预期可能已计入价格。", no_trade_directive: "不生成买入/卖出指令。" },
      expectation_gap: { structured_impact_bps: 800, consensus_impact_bps: 700, gap_bps: 100, quantifiable: true, rationale: "经营影响高于一致预期。" },
      diagnostics: { pre_stock_return_bps: 500, pre_benchmark_return_bps: 100, pre_abnormal_return_bps: 400, sector_relative_bps: null, abnormal_volume_bps: 2000, valuation_change_bps: null, historical_median_post_bps: null, historical_sample_count: 0, components: [{ metric: "事件前异常收益", available: true, value_bps: 400, score_contribution: 16, explanation: "个股减市场基准。" }, { metric: "板块相对表现", available: false, value_bps: null, score_contribution: 0, explanation: "板块缓存不足。" }] },
      missing_inputs: ["板块历史序列", "历史估值序列"], data_versions: {},
    },
    calibration: { ontology_kind: "order", sample_count: 0, median_post_abnormal_return_bps: null, positive_sample_ratio_bps: null, data_versions: [] },
  },
};

describe("结构化事件与市场定价面板", () => {
  it("把基本面利好与已经交易的市场机会分开显示", () => {
    render(<EventAnalysisPanel snapshot={snapshot} error={null} onRetry={vi.fn()} onCancel={vi.fn()} />);
    expect(screen.getByText("基本面影响")).toBeInTheDocument();
    expect(screen.getByText("市场机会（独立判断）")).toBeInTheDocument();
    expect(screen.getByText(/正向 · \+8.00%/)).toBeInTheDocument();
    expect(screen.getByText(/机会偏中性 · 已交易评分 76\/100/)).toBeInTheDocument();
    expect(screen.getAllByText(/数据缺失|来源未提供/).length).toBeGreaterThan(0);
  });

  it("任何非数字都不会触发小数格式化崩溃", () => {
    expect(eventBps(null)).toBe("不可量化");
    expect(eventBps(Number.NaN)).toBe("不可量化");
    expect(eventBps(125)).toBe("+1.25%");
  });
});
