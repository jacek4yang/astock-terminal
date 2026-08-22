import { createElement, useState } from "react";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { AgentMessage } from "../lib/api";
import { emptyClarificationDraft, formatClarificationAnswer, type ClarificationDraft } from "../lib/agentClarification";
import {
  buildToolDiagnostic,
  ClarificationCard,
  historyToMsgs,
  hasUnansweredClarification,
  redactDiagnosticValue,
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

  it("reconciles durable background-task states after reopening the app", () => {
    expect(taskRunStatus("completed")).toBe("completed");
    expect(taskRunStatus("interrupted")).toBe("suspended");
    expect(taskRunStatus("running")).toBe("running");
    expect(taskRunStatus("unknown")).toBe("idle");
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
});
