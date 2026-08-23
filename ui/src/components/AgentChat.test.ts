import { createElement, useState } from "react";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { AgentMessage, AgentReport } from "../lib/api";
import { emptyClarificationDraft, formatClarificationAnswer, type ClarificationDraft } from "../lib/agentClarification";
import {
  buildToolDiagnostic,
  ClarificationCard,
  ResearchVerificationPanel,
  historyToMsgs,
  hasUnansweredClarification,
  isNearScrollBottom,
  redactDiagnosticValue,
  sanitizeAgentVisibleText,
  stripPrivateReasoning,
  taskRunStatus,
  type ChatMsg,
} from "./AgentChat";

function message(overrides: Partial<AgentMessage>): AgentMessage {
  return {
    id: "m1",
    role: "assistant",
    content: "answer",
    tool_calls: [],
    tool_call_id: null,
    created_at: 1,
    malformed: false,
    ...overrides,
  };
}

describe("Agent history safety", () => {
  it("loads a normalized malformed legacy record as safe text", () => {
    const out: ChatMsg[] = [];
    const rows = historyToMsgs(
      message({ content: "{bad legacy payload}", malformed: true }),
      out,
    );
    expect(rows).toHaveLength(1);
    expect(rows[0].raw).toBe("{bad legacy payload}");
    expect(rows[0].failed).toContain("安全加载");
  });

  it("never renders provider-private reasoning", () => {
    expect(stripPrivateReasoning("可见前<think>私有推理</think>可见后")).toBe("可见前可见后");
    expect(stripPrivateReasoning("回答<think>尚未结束")).toBe("回答");
  });

  it("never renders internal evidence, tool or credential identifiers", () => {
    const visible = sanitizeAgentVisibleText(
      "金价偏强〔证据:evf_5e298283〕；`research_news` 为 status=no_match，research_global_transmission 为 total_documents=0，请配置 BLS_API_KEY 并核对 source_version_id。",
    );
    expect(visible).toContain("金价偏强");
    expect(visible).toContain("财经新闻检索");
    expect(visible).toContain("海外一手信息检索");
    expect(visible).toContain("相应数据源配置");
    for (const internal of [
      "evf_",
      "research_news",
      "research_global_transmission",
      "status=no_match",
      "total_documents=0",
      "BLS_API_KEY",
      "source_version_id",
    ]) {
      expect(visible).not.toContain(internal);
    }
  });

  it("reconciles durable background-task states after reopening the app", () => {
    expect(taskRunStatus("completed")).toBe("completed");
    expect(taskRunStatus("interrupted")).toBe("suspended");
    expect(taskRunStatus("running")).toBe("running");
    expect(taskRunStatus("verification_failed")).toBe("failed");
    expect(taskRunStatus("unknown")).toBe("idle");
  });

  it("only follows streaming output while the reader remains near the bottom", () => {
    expect(isNearScrollBottom({ scrollHeight: 2_000, scrollTop: 1_420, clientHeight: 500 })).toBe(true);
    expect(isNearScrollBottom({ scrollHeight: 2_000, scrollTop: 900, clientHeight: 500 })).toBe(false);
  });

  it("redacts credentials from nested and plain-text diagnostics", () => {
    expect(
      redactDiagnosticValue({
        symbol: "300308",
        api_key: "top-secret",
        nested: { authorization: "Bearer abc.def" },
      }),
    ).toEqual({
      symbol: "300308",
      api_key: "[已隐藏敏感信息]",
      nested: { authorization: "[已隐藏敏感信息]" },
    });
    const diagnostic = buildToolDiagnostic({
      key: 1,
      callId: "call-1",
      name: "get_quote",
      args: JSON.stringify({ symbol: "300308", token: "must-not-leak" }),
      done: false,
      error: "authorization=must-not-leak",
    });
    expect(diagnostic).toContain("300308");
    expect(diagnostic).not.toContain("must-not-leak");
    expect(diagnostic).toContain("[已隐藏敏感信息]");
  });

  it("pairs a persisted tool result with its call instead of duplicating the row", () => {
    const out: ChatMsg[] = [];
    out.push(
      ...historyToMsgs(
        message({
          id: "run-1-2",
          role: "assistant",
          content: "",
          created_at: 10,
          tool_calls: [{ id: "call-1", name: "get_quote", arguments: "{\"symbol\":\"300308\"}" }],
        }),
        out,
      ),
    );
    out.push(
      ...historyToMsgs(
        message({
          id: "run-1-3",
          role: "tool",
          tool_call_id: "call-1",
          created_at: 11,
          content: JSON.stringify({
            tool: "get_quote",
            cache_key: "quote:300308",
            source: "tdx",
            fetched_at: "2026-08-23T09:00:00+08:00",
            summary: { price: 12.3 },
          }),
        }),
        out,
      ),
    );
    expect(out[0].tools).toHaveLength(1);
    expect(out[0].tools[0]).toMatchObject({
      callId: "call-1",
      done: true,
      success: true,
      source: "tdx",
      cacheKey: "quote:300308",
    });
  });

  it("keeps the last clarification waiting until a following user answer exists", () => {
    const content = `\`\`\`astock-questions
{"questions":[{"id":"risk","question":"风险偏好？","options":["保守","平衡"]}]}
\`\`\``;
    const out: ChatMsg[] = [];
    out.push(...historyToMsgs(message({ role: "assistant", content }), out));
    expect(hasUnansweredClarification(out)).toBe(true);
    out.push(...historyToMsgs(message({ role: "user", content: "我选择平衡" }), out));
    expect(out[0].clarificationSubmitted).toBe(true);
    expect(hasUnansweredClarification(out)).toBe(false);
  });

  it("lets the user select every card and submit one continuation answer", async () => {
    const request = {
      title: "请确认研究条件",
      questions: [
        {
          id: "goal",
          question: "资金定位？",
          kind: "single" as const,
          allowOther: true,
          options: [
            { id: "trial", label: "试探性建仓", recommended: true },
            { id: "long", label: "长期底仓" },
          ],
        },
        {
          id: "risk",
          question: "风险偏好？",
          kind: "single" as const,
          allowOther: true,
          options: [
            { id: "safe", label: "保守" },
            { id: "balanced", label: "平衡" },
          ],
        },
      ],
    };
    const submitted = vi.fn();
    function Harness() {
      const [draft, setDraft] = useState<ClarificationDraft>(emptyClarificationDraft());
      return createElement(ClarificationCard, {
        request,
        draft,
        submitted: false,
        onChange: setDraft,
        onSubmit: () => submitted(formatClarificationAnswer(request, draft)),
      });
    }
    render(createElement(Harness));
    const submit = screen.getByRole("button", { name: "提交选择并继续分析" });
    expect(submit).toBeDisabled();
    await userEvent.click(screen.getByText("试探性建仓"));
    await userEvent.click(screen.getByText("平衡"));
    expect(submit).toBeEnabled();
    await userEvent.click(submit);
    expect(submitted).toHaveBeenCalledTimes(1);
    expect(submitted.mock.calls[0][0]).toContain("资金定位？：试探性建仓");
    expect(submitted.mock.calls[0][0]).toContain("风险偏好？：平衡");
  });

  it("lazily expands a blocked claim into copyable field-level diagnostics", async () => {
    const report: AgentReport = {
      task_id: "run-1",
      answer: "报告未通过证据校验",
      conclusions: [],
      generated_at: 1,
      evidence: [{
        evidence_id: "ev_1",
        tool: "get_quote",
        cache_key: "get_quote:300308",
        source: "tdx",
        fetched_at: "2026-08-23T09:00:00+08:00",
        tool_version: "v2",
        data_version: "data_1",
        source_tier: "provider",
        freshness: "stale",
        blocking: false,
        fields: [{
          evidence_id: "evf_price",
          field_path: "/price",
          value: 12.3,
          unit: "cny",
          currency: "CNY",
          as_of: "2026-08-23T09:00:00+08:00",
          freshness: "stale",
          source_tier: "provider",
          blocking: false,
          calculation_id: null,
        }],
      }],
      research: {
        schema_version: "astock-research-report/v1",
        as_of: "2026-08-23T09:00:00+08:00",
        confidence: "blocked",
        claims: [{
          claim_id: "claim_1",
          text: "最新价为12.3元",
          claim_type: "fact",
          evidence_ids: ["evf_price"],
          calculation_ids: [],
          as_of: "2026-08-23T09:00:00+08:00",
          confidence: "blocked",
          assumptions: [],
          counter_evidence: [],
          invalidation: [],
          unknowns: [],
        }],
        calculations: [],
        assumptions: [],
        counter_evidence: [],
        invalidation: [],
        unknowns: [],
        verification: {
          status: "failed",
          verifier_version: "report-verifier/v1",
          verified_at: 1,
          findings: [{
            code: "stale_price",
            severity: "error",
            claim_id: "claim_1",
            message: "价格字段已陈旧，必须重新取行情",
          }],
        },
      },
    };
    render(createElement(ResearchVerificationPanel, { report }));
    expect(screen.getByText("报告已被证据校验阻断")).toBeInTheDocument();
    expect(screen.queryByText(/最新价为12.3元/)).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: /报告已被证据校验阻断/ }));
    expect(screen.getByText(/最新价为12.3元/)).toBeInTheDocument();
    expect(screen.getByText(/价格字段已陈旧/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "复制当前校验摘要" })).toBeInTheDocument();
  });

  it("shows clarification as waiting input instead of a failed report", () => {
    const report: AgentReport = {
      task_id: "run-waiting",
      answer: "请先选择你的资金定位。",
      conclusions: [],
      generated_at: 1,
      evidence: [],
      research: {
        schema_version: "astock-research-report/v1",
        as_of: "2026-08-23T09:00:00+08:00",
        confidence: "low",
        claims: [],
        calculations: [],
        assumptions: [],
        counter_evidence: [],
        invalidation: [],
        unknowns: [],
        verification: {
          status: "not_applicable",
          verifier_version: "report-verifier/v1",
          verified_at: 1,
          findings: [],
        },
      },
    };
    const { container } = render(createElement(ResearchVerificationPanel, { report }));
    expect(container).toHaveTextContent("正在等待你的选择，尚未发布投资结论");
    expect(container).not.toHaveTextContent("报告已被证据校验阻断");
  });
});
