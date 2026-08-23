import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import GraphHistoryWorkbench from "./GraphHistoryWorkbench";

const mocks = vi.hoisted(() => ({
  bounds: vi.fn(),
  asOf: vi.fn(),
  timeline: vi.fn(),
  diff: vi.fn(),
}));

vi.mock("../lib/api", () => ({
  graphHistoryBounds: mocks.bounds,
  graphAsOf: mocks.asOf,
  graphEdgeTimeline: mocks.timeline,
  graphSnapshotDiff: mocks.diff,
  errMsg: (value: unknown) => String(value),
}));

const snapshot = {
  snapshot_id: "graph-snapshot:stable",
  business_time: 1_700_000_000,
  knowledge_time: 1_700_100_000,
  center: null,
  hops: 2,
  nodes: [
    { id: "company:600001", kind: "company", name: "供应商甲", code: "600001" },
    { id: "company:600002", kind: "company", name: "客户乙", code: "600002" },
  ],
  edges: [{
    revision_id: "edge-rev:2", identity_id: "edge:1", revision_no: 2,
    src: "company:600001", original_src: "company:600001", dst: "company:600002", original_dst: "company:600002",
    relation: "supplies", product_scope: "动力电池", region_scope: "中国", weight: 0.35, disclosed_share: 0.35,
    confidence: 0.92, effective_confidence: 0.81, source_type: "annual_report", source_name: "2025 年报",
    source_url: "https://example.com/2025", evidence_version: "srcver:2025", status: "stale",
    valid_from: 1_700_000_000, valid_to: null, observed_at: 1_700_050_000, recorded_at: 1_700_100_000,
    revalidate_after: 1_710_000_000,
  }],
  revision_ids: ["edge-rev:2"], merge_ids: [], stale_count: 1, excluded_count: 2,
};

describe("双时间历史图谱工作台", () => {
  beforeEach(() => {
    mocks.bounds.mockReset().mockResolvedValue({
      business_min: 1_600_000_000, business_max: 1_700_000_000,
      knowledge_min: 1_650_000_000, knowledge_max: 1_700_100_000,
      revision_count: 12, revalidation_due_count: 3,
    });
    mocks.asOf.mockReset().mockResolvedValue(snapshot);
    mocks.timeline.mockReset().mockResolvedValue([
      { ...snapshot.edges[0], revision_id: "edge-rev:1", revision_no: 1, status: "active", disclosed_share: 0.2,
        superseded_at: 1_700_100_000, decay_half_life_days: 730, supersedes_revision_id: null, metadata: {} },
      { ...snapshot.edges[0], status: "contradicted", superseded_at: null, decay_half_life_days: 730,
        supersedes_revision_id: "edge-rev:1", metadata: { reason: "客户变更" } },
    ]);
    mocks.diff.mockReset().mockResolvedValue({
      left_snapshot_id: "graph-snapshot:then", right_snapshot_id: "graph-snapshot:stable",
      added_revision_ids: ["edge-rev:2"], removed_revision_ids: ["edge-rev:1"], changed_identity_ids: ["edge:1"],
    });
  });
  afterEach(cleanup);

  it("明确区分两种时间并可展开完整关系修订时间线", async () => {
    render(<GraphHistoryWorkbench />);
    expect(await screen.findByText("双时间历史图谱")).toBeInTheDocument();
    expect(screen.getByText(/业务时间回答/)).toBeInTheDocument();
    expect(screen.getByText(/12 个不可变修订/)).toBeInTheDocument();
    expect(await screen.findByText("供应商甲")).toBeInTheDocument();
    expect(screen.getByText(/35.00%/)).toBeInTheDocument();
    fireEvent.click(screen.getByText("供应商甲"));
    await waitFor(() => expect(mocks.timeline).toHaveBeenCalledWith("edge:1"));
    expect(await screen.findByText("第 1 次修订")).toBeInTheDocument();
    expect(screen.getByText("存在反方证据")).toBeInTheDocument();
    expect(screen.getAllByText(/半衰期 730 天/)).toHaveLength(2);
  });

  it("知识差异比较显式使用同一业务时间的两个知悉截面", async () => {
    render(<GraphHistoryWorkbench />);
    await screen.findByText("供应商甲");
    fireEvent.click(screen.getByRole("button", { name: "比较后来新增认知" }));
    await waitFor(() => expect(mocks.diff).toHaveBeenCalledWith(
      1_700_000_000,
      1_700_000_000,
      1_700_000_000,
      1_700_100_000,
    ));
    expect(await screen.findByText("知识时间差异")).toBeInTheDocument();
    expect(screen.getByText("后来新增")).toBeInTheDocument();
    expect(screen.getByText(/回测只能使用左侧/)).toBeInTheDocument();
  });
});
