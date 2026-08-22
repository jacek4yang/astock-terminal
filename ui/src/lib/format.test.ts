import { describe, expect, it } from "vitest";
import { EMPTY_DISPLAY, finiteNumber, fmtNum, fmtPct, fmtText, fmtYiWan } from "./format";

describe("runtime-safe financial formatting", () => {
  it("normalizes finite decimal strings returned by upstream payloads", () => {
    expect(finiteNumber("19.539")).toBe(19.539);
    expect(fmtNum("19.539", 2)).toBe("19.54");
    expect(fmtPct("3.25")).toBe("+3.25%");
    expect(fmtYiWan("125000000")).toBe("1.25亿");
  });

  it("does not throw or fabricate values for malformed input", () => {
    for (const value of [null, undefined, "", "--", false, Number.NaN, Number.POSITIVE_INFINITY]) {
      expect(finiteNumber(value)).toBeNull();
      expect(fmtNum(value)).toBe(EMPTY_DISPLAY);
    }
  });

  it("renders upstream dash placeholders as readable Chinese text", () => {
    for (const value of [null, "", "-", "--", "------", "—", "N/A"]) {
      expect(fmtText(value)).toBe("暂无");
    }
    expect(fmtText(" 已披露 ")).toBe("已披露");
  });
});
