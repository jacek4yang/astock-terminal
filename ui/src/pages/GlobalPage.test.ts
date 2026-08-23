import { describe, expect, it } from "vitest";
import { globalPageTokens, globalPollDelay } from "./GlobalPage";

describe("全球传导工作台", () => {
  it("分页保留首页、邻页和尾页", () => {
    expect(globalPageTokens(20, 40)).toEqual([1, "ellipsis", 19, 20, 21, "ellipsis", 40]);
  });

  it("空闲仍监听，后台运行时提高刷新频率", () => {
    expect(globalPollDelay(false)).toBe(3000);
    expect(globalPollDelay(true)).toBe(750);
  });
});
