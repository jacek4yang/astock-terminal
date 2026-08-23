import { describe, expect, it } from "vitest";
import {
  clarificationIsComplete,
  formatClarificationAnswer,
  parseClarification,
} from "./agentClarification";

describe("Agent clarification protocol", () => {
  it("parses a structured selection card and keeps surrounding prose", () => {
    const parsed = parseClarification(`先确认三个关键条件。\n\n\`\`\`astock-questions
{"title":"配置研究方案","questions":[{"id":"goal","question":"这笔资金的定位？","kind":"single","options":[{"id":"trial","label":"试探性建仓","recommended":true},{"id":"long","label":"长期底仓"}],"allow_other":true}]}
\`\`\`\n\n提交后继续。`);
    expect(parsed?.pending).toBe(false);
    expect(parsed?.displayText).toContain("先确认三个关键条件");
    expect(parsed?.displayText).toContain("提交后继续");
    expect(parsed?.request.questions[0].options[0]).toMatchObject({
      id: "trial",
      recommended: true,
    });
  });

  it("converts legacy numbered Markdown questions into interactive choices", () => {
    const parsed = parseClarification(`两万元的方案差异很大，请先确认：

**1. 这笔2万元的资金定位？**
- A. 试探性建仓
- B. 长期底仓/定投
- C. 中短期交易

**2. 风险承受？**
- 保守（最大回撤小于5%）
- 平衡（最大回撤小于15%）
- 激进（最大回撤可超过30%）

收到回复后继续分析。`);
    expect(parsed?.request.questions).toHaveLength(2);
    expect(parsed?.request.questions[0].options[0].label).toBe("试探性建仓");
    expect(parsed?.request.questions[1].options[2].label).toContain("激进");
    expect(parsed?.displayText).not.toContain("1. 这笔");
    expect(parsed?.displayText).toContain("收到回复后");
  });

  it("does not convert ordinary numbered research sections", () => {
    expect(parseClarification("1. 关键依据\n- 营收增长\n- 现金流改善\n2. 风险\n- 估值偏高")).toBeNull();
  });

  it("requires every answer and formats one durable continuation message", () => {
    const request = parseClarification(`\`\`\`astock-questions
{"questions":[{"id":"risk","question":"风险偏好？","options":["保守","平衡"]},{"id":"term","question":"计划期限？","options":["半年","三年"]}]}
\`\`\``)!.request;
    const incomplete = { selections: { risk: ["o2"] }, other: {} };
    expect(clarificationIsComplete(request, incomplete)).toBe(false);
    const complete = { selections: { risk: ["o2"], term: ["o2"] }, other: {} };
    expect(clarificationIsComplete(request, complete)).toBe(true);
    const answer = formatClarificationAnswer(request, complete);
    expect(answer).toContain("风险偏好？：平衡");
    expect(answer).toContain("计划期限？：三年");
    expect(answer).toContain("不要重复询问");
  });
});
