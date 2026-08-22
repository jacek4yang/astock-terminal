import { describe, expect, it } from "vitest";
import { pageTokens } from "./MarketPage";

describe("market pagination", () => {
  it("shows edge pages and a compact numbered window", () => {
    expect(pageTokens(8, 20)).toEqual([1, "ellipsis", 6, 7, 8, 9, 10, "ellipsis", 20]);
    expect(pageTokens(1, 3)).toEqual([1, 2, 3]);
  });
});
