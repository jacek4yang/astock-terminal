import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { RelationCandidate } from "../lib/api";
import RelationReviewWorkbench from "./RelationReviewWorkbench";

const candidate: RelationCandidate = {
  candidate_id: "relcand:1", run_id: "relrun:1", source_version_id: "srcver:annual-2025",
  document_kind: "annual_report", subject_text: "星海动力有限公司", object_text: "远航汽车股份有限公司",
  relation: "supplies", product_text: "动力电池", amount_text: "2亿元", share_bps: 1000,
  report_period: "2025年度", region: null, subject_entity_id: "sub:power", object_entity_id: "listed:far",
  subject_parent_entity_id: "listed:star", object_parent_entity_id: "listed:far", disclosure_mode: "named",
  confidence_bps: 8400, validation_status: "validated", review_status: "pending_review", confidential: false,
  non_inferable: false, candidate_version: 1, proposed_by_model: true, publication_status: null,
  eligible_for_agent: false, created_at: 1, updated_at: 1,
  validation: [{ field: "原文证据", passed: true, detail: "quote 与不可变段落完全一致" }, { field: "主体实体", passed: true, detail: "子公司已映射" }],
  evidence: [{ evidence_id: "relev:1", source_version_id: "srcver:annual-2025", segment_id: "seg:42",
    page_number: 42, paragraph_index: 10, span_start: 0, span_end: 35,
    quote_original: "星海动力与远航汽车签订动力电池供应合同", independent_group: "doc:annual", polarity: "supports" }],
};

const mocks = vi.hoisted(() => ({ queryRelationReviews: vi.fn(), reviewRelationCandidate: vi.fn() }));
vi.mock("../lib/api", () => ({
  queryRelationReviews: mocks.queryRelationReviews,
  reviewRelationCandidate: mocks.reviewRelationCandidate,
  startRelationExtraction: vi.fn(), getRelationExtractionStatus: vi.fn(), cancelRelationExtraction: vi.fn(),
  retractRelationCandidate: vi.fn(), errMsg: (value: unknown) => String(value),
}));

describe("供应链关系审核工作台", () => {
  beforeEach(() => { localStorage.clear(); mocks.queryRelationReviews.mockReset().mockResolvedValue({ items: [candidate], total: 1, page: 1, page_size: 20, total_pages: 1 }); mocks.reviewRelationCandidate.mockReset().mockResolvedValue({ candidate_id: candidate.candidate_id, publication_id: "relpub:1", projection_key: "a|b|supplies", status: "published", note: "已发布" }); window.history.replaceState({}, "", "/graph"); });
  afterEach(cleanup);

  it("逐层展示实体映射、原文页码、span 与确定性校验", async () => {
    render(<RelationReviewWorkbench />);
    await screen.findByText(/星海动力有限公司/);
    fireEvent.click(screen.getByText(/星海动力有限公司/));
    expect(screen.getByText("listed:star")).toBeInTheDocument();
    expect(screen.getByText(/第 42 页 · span 0–35/)).toBeInTheDocument();
    expect(screen.getByText(/quote 与不可变段落完全一致/)).toBeInTheDocument();
    expect(screen.getByText(/只有通过并发布后才允许进入 Agent 高置信结论/)).toBeInTheDocument();
  });

  it("没有审核理由时不会发布", async () => {
    render(<RelationReviewWorkbench />);
    await screen.findByText(/星海动力有限公司/);
    fireEvent.click(screen.getByText(/星海动力有限公司/));
    fireEvent.click(screen.getByRole("button", { name: "通过并发布" }));
    expect(screen.getByText(/请填写审核理由/)).toBeInTheDocument();
    await waitFor(() => expect(mocks.reviewRelationCandidate).not.toHaveBeenCalled());
  });
});
