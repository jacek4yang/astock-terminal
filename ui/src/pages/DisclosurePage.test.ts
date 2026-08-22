import { describe, expect, it } from "vitest";
import { disclosurePageTokens, disclosurePollDelay } from "./DisclosurePage";

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
});
