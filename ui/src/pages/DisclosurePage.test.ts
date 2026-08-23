import { describe, expect, it } from "vitest";
import { disclosurePageTokens, disclosurePollDelay, disclosureRelationKind } from "./DisclosurePage";

describe("正式披露中心分页", () => {
  it("在大页数下保留首页、邻页和尾页", () => {
    expect(disclosurePageTokens(50, 100)).toEqual([1, "ellipsis", 49, 50, 51, "ellipsis", 100]);
  });

  it("空数据不生成无效页码", () => {
    expect(disclosurePageTokens(1, 0)).toEqual([]);
  });

  it("空闲后仍保留监听，运行中提高进度刷新频率", () => {
    expect(disclosurePollDelay(false)).toBe(3000);
    expect(disclosurePollDelay(true)).toBe(750);
  });

  it("把正式材料路由到对应关系抽取规则，而不是统一按新闻处理", () => {
    expect(disclosureRelationKind({ title: "2025年年度报告", category: "periodic_report" })).toBe("annual_report");
    expect(disclosureRelationKind({ title: "联合体中标重大项目公告", category: "contract" })).toBe("tender");
    expect(disclosureRelationKind({ title: "投资者关系活动记录表", category: "other" })).toBe("investor_relations");
  });
});
