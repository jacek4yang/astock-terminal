import { describe, expect, it } from "vitest";
import { sourceDisplayName, toolArgumentsDisplay, toolDisplayName } from "./agentLabels";

describe("retail-facing agent labels", () => {
  it("never exposes internal tool identifiers for known or unknown tools", () => {
    expect(toolDisplayName("run_chanlun")).toBe("缠论结构分析");
    expect(toolDisplayName("get_market_regime")).toBe("市场环境识别");
    expect(toolDisplayName("future_internal_tool")).toBe("扩展分析步骤");
  });

  it("renders source and argument metadata in plain Chinese", () => {
    expect(sourceDisplayName("eastmoney_f10")).toBe("东方财富公司资料");
    expect(sourceDisplayName("tdx")).toBe("通达信行情数据");
    expect(toolArgumentsDisplay('{"symbol":"300308","period":"day","adjust":"qfq"}')).toEqual([
      { label: "股票代码", value: "300308" },
      { label: "K 线周期", value: "日线" },
      { label: "复权方式", value: "前复权" },
    ]);
  });
});
