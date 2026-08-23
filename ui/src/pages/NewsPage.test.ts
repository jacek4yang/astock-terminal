import { describe, expect, it } from "vitest";
import { pageTokens, virtualRange } from "./NewsPage";

describe("资讯中心分页与虚拟列表", () => {
  it("十万条结果只计算当前可见窗口", () => {
    const result = virtualRange(83_000, 830, 100_000);
    expect(result.totalHeight).toBe(16_600_000);
    expect(result.start).toBe(496);
    expect(result.end - result.start).toBeLessThanOrEqual(14);
    expect(result.offset).toBe(result.start * 166);
  });

  it("用紧凑页码保留首页、当前页和末页", () => {
    expect(pageTokens(50, 100)).toEqual([1, "ellipsis", 49, 50, 51, "ellipsis", 100]);
    expect(pageTokens(1, 3)).toEqual([1, 2, 3]);
  });
});
