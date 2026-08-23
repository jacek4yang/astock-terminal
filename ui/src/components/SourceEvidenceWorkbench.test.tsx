import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import SourceEvidenceWorkbench from "./SourceEvidenceWorkbench";

const getSourceDocuments = vi.fn();
const getSourceDocument = vi.fn();

vi.mock("../lib/api", () => ({
  compareSourceEvidence: vi.fn(async () => []),
  errMsg: (error: unknown) => String(error),
  fetchSourceDocument: vi.fn(),
  getSourceDocument: (...args: unknown[]) => getSourceDocument(...args),
  getSourceDocuments: (...args: unknown[]) => getSourceDocuments(...args),
}));

describe("SourceEvidenceWorkbench", () => {
  beforeEach(() => {
    getSourceDocuments.mockReset();
    getSourceDocument.mockReset();
    getSourceDocuments.mockResolvedValue([
      {
        source_document_id: "srcdoc:official",
        canonical_url: "https://www.sse.com.cn/disclosure/a.html",
        current_version_id: "srcver:official",
        authority: "regulatory_exchange_government",
        authority_name: "监管/交易所/政府一级来源",
        is_primary_source: true,
        access_status: "verified",
        failure_kind: null,
        failure_message: null,
        first_fetched_at: 1_800_000_000,
        last_fetched_at: 1_800_000_000,
      },
      {
        source_document_id: "srcdoc:paywall",
        canonical_url: "https://example.com/paywall",
        current_version_id: null,
        authority: "unknown",
        authority_name: "未分类来源",
        is_primary_source: false,
        access_status: "unverified",
        failure_kind: "access_wall",
        failure_message: "页面要求登录",
        first_fetched_at: 1_800_000_000,
        last_fetched_at: 1_800_000_000,
      },
    ]);
    getSourceDocument.mockResolvedValue({
      document: {},
      version: {
        source_version_id: "srcver:official",
        source_document_id: "srcdoc:official",
        canonical_url: "https://www.sse.com.cn/disclosure/a.html",
        content_hash: "hash",
        extracted_hash: "text-hash",
        media_type: "text/html",
        title: "合同公告",
        published_at: 1_800_000_000,
        fetched_at: 1_800_000_010,
        parser_version: "source-evidence-v1",
        supersedes_version_id: null,
        scores: { reliability: 1, independence: 1, freshness: 1, note: "评分不替代证据" },
        authority: "regulatory_exchange_government",
        authority_name: "监管/交易所/政府一级来源",
        is_primary_source: true,
        prompt_injection_detected: false,
      },
      segments: [{
        segment_id: "segment:1",
        source_version_id: "srcver:official",
        page_number: null,
        paragraph_index: 0,
        selector: "p",
        span_start: 0,
        span_end: 12,
        text: "公司合同金额2亿元。",
        text_hash: "segment-hash",
      }],
      facts: [{
        fact_id: "fact:1",
        source_version_id: "srcver:official",
        segment_id: "segment:1",
        fact_type: "money",
        field_name: "合同金额",
        subject: "公司",
        raw_value: "2",
        normalized_value: 200_000_000,
        original_unit: "亿元",
        normalized_unit: "元",
        page_number: null,
        paragraph_index: 0,
        span_start: 0,
        span_end: 10,
      }],
      verification_note: "已读取原始来源",
    });
  });

  it("keeps unverified failures explicit and drills verified facts to exact evidence", async () => {
    const user = userEvent.setup();
    render(<SourceEvidenceWorkbench />);

    expect(await screen.findByText("原始来源核验与字段级证据")).toBeInTheDocument();
    expect(screen.getByPlaceholderText(/输入公告、监管页面/)).toBeInTheDocument();
    expect(await screen.findByText("原文未核验")).toBeInTheDocument();

    await user.click(screen.getByText("监管/交易所/政府一级来源"));
    expect(await screen.findByText("合同金额：2亿元")).toBeInTheDocument();
    expect(screen.getByText(/原文位置 0–10/)).toBeInTheDocument();
    expect(screen.getByText("事实编号：fact:1")).toBeInTheDocument();
  });
});
