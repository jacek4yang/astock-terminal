import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import QuantResearchWorkbench from "./QuantResearchWorkbench";

const mocks = vi.hoisted(() => ({
  start: vi.fn(),
  status: vi.fn(),
  cancel: vi.fn(),
  get: vi.fn(),
  list: vi.fn(),
}));

vi.mock("./Chart", () => ({ default: () => <div data-testid="chart" /> }));
vi.mock("../lib/api", async () => {
  const actual = await vi.importActual<Record<string, unknown>>("../lib/api");
  return {
    ...actual,
    quantResearchStart: mocks.start,
    quantResearchStatus: mocks.status,
    quantResearchCancel: mocks.cancel,
    quantResearchSnapshotGet: mocks.get,
    quantResearchSnapshotList: mocks.list,
    errMsg: (error: unknown) => String(error),
  };
});

const config = {
  symbols: ["300308", "600519"], metric: "pearson", value_mode: "log_return", frequency: "daily",
  start_date: null, end_date: null, adjust: "qfq", lookback_bars: 750, missing_policy: "drop",
  rolling_window: 60, max_lag: 5, controls: [], bootstrap_reps: 199, permutation_reps: 199,
  alpha: 0.05, fdr_method: "benjamini_hochberg", max_pairs: 2000,
  max_observations_per_pair: 500, seed: 42, oos_ratio: 0.3,
} as const;

const snapshot = {
  snapshot_id: "qrs-test", function_version: "astock-quant-research/v1", created_at: 1, config,
  data_versions: { "300308": "v1", "600519": "v1" },
  budget: { requested_pairs: 1, executed_pairs: 1, pair_sampling: false, max_observations_per_pair: 500, estimated_operations: 1000, complexity: "O(股票对数 × 样本数)", explanation: "全部配对均纳入" },
  warnings: [], causality_boundary: "相关和 Granger 预测关系都不能单独证明结构性因果。",
  results: [{
    left: "300308", right: "600519", directed: false, effect: 0.42, effect_name: "Pearson 相关系数",
    confidence_low: 0.2, confidence_high: 0.6, confidence_method: "bootstrap", p_value: 0.01,
    p_value_method: "置换检验", adjusted_p_value: 0.02, significant_raw: true,
    significant_after_correction: true, effective_n: 240, best_lag: null, controls_used: [],
    stability: { slice_count: 4, same_direction_rate: 0.75, min_effect: 0.1, max_effect: 0.6, train_effect: 0.4, out_of_sample_effect: 0.3, outlier_robust_effect: 0.39, assessment: "跨窗口方向一般" },
    stability_slices: [{ group: "年度", label: "2025", effect: 0.4, effective_n: 120 }],
    interpretation: "相关不等于因果", conclusion: "效应量 0.42，经 FDR 校正后显著。", warnings: [],
  }],
};

const completedJob = {
  job_id: "quant-1", running: false, status: "completed", phase: "研究完成", progress: 100,
  done_pairs: 1, total_pairs: 1, current_pair: null, effective_observations: 240,
  fetched_series: 2, total_series: 2, estimated_remaining_seconds: 0, recent_logs: ["快照已保存"],
  result: snapshot, error: null, started_at: 1, updated_at: 2,
};

describe("量化研究工作台", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.status.mockResolvedValue(null);
    mocks.list.mockResolvedValue([]);
    mocks.cancel.mockResolvedValue(true);
  });

  it("提供完整中文研究参数并把任务作为后台任务启动", async () => {
    mocks.start.mockResolvedValue({ ...completedJob, running: true, status: "running", progress: 5, result: null, recent_logs: ["无固定超时，可随时取消"] });
    render(<QuantResearchWorkbench />);
    fireEvent.click(screen.getByRole("button", { name: "高级参数" }));
    expect(screen.getByText("多重检验")).toBeInTheDocument();
    expect(screen.getByText("Bootstrap 次数")).toBeInTheDocument();
    expect(screen.getByText("控制变量代码")).toBeInTheDocument();
    fireEvent.change(screen.getByPlaceholderText("如 300308，600519，600036"), { target: { value: "300308, 600519" } });
    fireEvent.click(screen.getByRole("button", { name: "开始后台研究" }));
    await waitFor(() => expect(mocks.start).toHaveBeenCalled());
    expect(mocks.start.mock.calls[0][0]).toMatchObject({ symbols: ["300308", "600519"], fdr_method: "benjamini_hochberg", seed: 42 });
    expect(await screen.findByText(/无固定超时/)).toBeInTheDocument();
  });

  it("显示效应区间、FDR、有效样本、分页与因果边界", async () => {
    mocks.status.mockResolvedValue(completedJob);
    render(<QuantResearchWorkbench />);
    expect(await screen.findByText("全部关系检验")).toBeInTheDocument();
    // The effect interval is rendered in both the summary and the detail row,
    // so assert presence rather than uniqueness. `getByText` throws on multiple
    // matches, which made this fail intermittently in CI depending on how much
    // of the table had rendered when the heading resolved. The neighbouring
    // sample-size assertion already uses this form for the same reason.
    expect(screen.getAllByText("[0.2000, 0.6000]").length).toBeGreaterThan(0);
    expect(screen.getAllByText("0.0200 · 通过").length).toBeGreaterThan(0);
    expect(screen.getAllByText("240").length).toBeGreaterThan(0);
    expect(screen.getByText(/不能单独证明结构性因果/)).toBeInTheDocument();
    expect(screen.getByText(/第 1\/1 页/)).toBeInTheDocument();
  });
});
