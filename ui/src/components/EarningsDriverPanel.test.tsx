import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import EarningsDriverPanel from "./EarningsDriverPanel";

const mocks = vi.hoisted(() => ({ tree: vi.fn(), shock: vi.fn() }));

vi.mock("../lib/api", () => ({
  getEarningsDriverTree: mocks.tree,
  runEarningsDriverShock: mocks.shock,
  errMsg: (value: unknown) => String(value),
}));

vi.mock("./Chart", () => ({ default: () => <div data-testid="driver-chart" /> }));

const parameter = (id: string, origin = "historical_fact") => ({
  id, name: id === "base_revenue" ? "历史营业收入" : "收入增长率", category: "revenue",
  value: id === "base_revenue" ? 1_000_000_000 : 0.1,
  low: id === "base_revenue" ? 1_000_000_000 : 0.05,
  high: id === "base_revenue" ? 1_000_000_000 : 0.2,
  unit: id === "base_revenue" ? "CNY" : "decimal", origin, report_period: "2025-12-31", confidence: 0.98,
  evidence: origin === "historical_fact" ? [{ source_version_id: "f10:income:v1", source_name: "公司年报结构化报表",
    report_period: "2025-12-31", announced_date: "2026-03-30", locator: "TOTAL_OPERATE_INCOME", unit: "CNY",
    confidence_low: 0.94, confidence_high: 0.99 }] : [], note: "可追溯参数",
});

const scenario = (name: string, scale: number) => ({ scenario: name, revenue: 1e9 * scale, gross_profit: 4e8 * scale,
  operating_profit: 2e8 * scale, tax: 5e7 * scale, minority_profit: 0, parent_net_profit: 1.5e8 * scale,
  eps: 1.5 * scale, operating_cash_flow: 2e8 * scale, capex: 5e7 * scale, free_cash_flow: 1.5e8 * scale });

const tree = {
  snapshot_id: "edt-stable-snapshot", parameter_snapshot_id: "fp-shared-parameters", model_version: "earnings-driver-v1",
  symbol: "300308", company_name: "中际旭创", industry: "电子制造", industry_template: "manufacturing",
  industry_template_label: "制造业", revenue_formula: "各产品销量×ASP，受产能×利用率约束",
  cost_formula: "材料耗用×采购价+能源+人工+折旧+运输", report_period: "2025-12-31", knowledge_time: 1_800_000_000,
  golden_template_reviewed: true,
  parameters: [parameter("base_revenue"), parameter("revenue_growth", "industry_prior"), ...Array.from({ length: 8 }, (_, i) => ({ ...parameter(`fact_${i}`), name: `参数${i}` }))],
  revenue_tree: { id: "revenue", label: "合并营业收入", dimension: "company_total", formula: "合并收入",
    status: "consolidated_fact", parameter_ids: ["base_revenue"], children: [{ id: "products", label: "分产品收入",
      dimension: "product", formula: "销量×ASP", status: "missing_disclosure", parameter_ids: [], children: [] }] },
  cost_tree: { id: "cost", label: "合并营业成本", dimension: "company_total", formula: "合并成本",
    status: "consolidated_fact", parameter_ids: [], children: [{ id: "materials", label: "主要材料",
      dimension: "input", formula: "产量×单耗×采购价", status: "missing_disclosure", parameter_ids: [], children: [] }] },
  formula_nodes: [{ id: "forecast_revenue", name: "预测收入", formula: "历史收入×(1+收入增长率)",
    parameter_ids: ["base_revenue", "revenue_growth"], unit: "CNY", historical_value: 1e9,
    forecast_low: 1.05e9, forecast_base: 1.1e9, forecast_high: 1.2e9 }],
  scenarios: [scenario("bear", 0.9), scenario("base", 1), scenario("bull", 1.2)],
  sensitivity: [{ revenue_growth: 0.05, gross_margin: 0.35, eps: 1.2, free_cash_flow: 1e8 }],
  monte_carlo: { samples: 1000, seed: 7, eps_p10: 1.1, eps_p50: 1.5, eps_p90: 2.0,
    fcf_p10: 1e8, fcf_p50: 1.5e8, fcf_p90: 2e8, method: "确定性区间抽样" },
  implied_assumption: { current_price: 20, implied_fcf_growth: 0.08, search_low: -0.5, search_high: 1,
    wacc: 0.09, terminal_growth: 0.025, explanation: "反向求解，不是预测" },
  quality: { exact_eps_available: false, model_completeness: 1, missing_core_drivers: ["分产品销量", "ASP"],
    refusal_reason: null, warnings: ["仅输出宽区间"] }, provenance_legend: {},
};

describe("盈利驱动树", () => {
  beforeEach(() => {
    mocks.tree.mockReset().mockResolvedValue(tree);
    mocks.shock.mockReset().mockResolvedValue({ base_snapshot_id: tree.snapshot_id, shocked_snapshot_id: "shock-v1", shocks: [],
      base: scenario("base", 1), shocked: scenario("base", 0.9), delta: { revenue: -1e8, gross_profit: -4e7,
        operating_profit: -2e7, parent_net_profit: -1.5e7, eps: -0.15, operating_cash_flow: -2e7, free_cash_flow: -1.5e7 },
      changed_parameters: [], warnings: [] });
  });
  afterEach(cleanup);

  it("明确展示行业公式、快照口径和拒绝精确 EPS 的原因", async () => {
    render(<EarningsDriverPanel symbol="300308" />);
    expect(await screen.findByText("制造业模型")).toBeInTheDocument();
    expect(screen.getByText(/各产品销量×ASP/)).toBeInTheDocument();
    expect(screen.getByText(/不输出精确 EPS/)).toHaveTextContent("分产品销量、ASP");
    expect(screen.getByText("预测收入")).toBeInTheDocument();
    expect(screen.getByText(/历史收入×\(1\+收入增长率\)/)).toBeInTheDocument();
  });

  it("支持情景、Monte Carlo、反向求解与分页证据下钻", async () => {
    render(<EarningsDriverPanel symbol="300308" />);
    await screen.findByText("制造业模型");
    fireEvent.click(screen.getByRole("button", { name: "情景与敏感性" }));
    expect(screen.getByText(/Monte Carlo 区间/)).toBeInTheDocument();
    expect(screen.getByText("现价反向求解")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "参数与证据" }));
    expect(screen.getByText("第 1 / 2 页")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "下一页" }));
    expect(screen.getByText("第 2 / 2 页")).toBeInTheDocument();
  });

  it("把用户冲击提交给财务桥接并显示逐项变化", async () => {
    render(<EarningsDriverPanel symbol="300308" />);
    await screen.findByText("制造业模型");
    fireEvent.click(screen.getByRole("button", { name: "冲击试算" }));
    fireEvent.click(screen.getByRole("button", { name: "计算冲击如何影响利润和现金流" }));
    await waitFor(() => expect(mocks.shock).toHaveBeenCalled());
    expect(await screen.findByText("相对基准情景的变化")).toBeInTheDocument();
    expect(screen.getByText(/shock-v1/)).toBeInTheDocument();
  });
});
