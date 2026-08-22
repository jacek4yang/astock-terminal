import { describe, expect, it } from "vitest";
import { parseAgentChart, splitAgentContent } from "./AgentChart";

describe("safe agent chart protocol", () => {
  it("accepts bounded line and bar data and extracts it from an answer", () => {
    const raw = '{"title":"收盘价走势","unit":"元","x":["周一","周二"],"series":[{"name":"收盘价","type":"line","data":[10,11.2]}]}';
    expect(parseAgentChart(raw)?.series[0].data).toEqual([10, 11.2]);
    const blocks = splitAgentContent(`结论\n\n\`\`\`astock-chart\n${raw}\n\`\`\`\n继续分析`);
    expect(blocks.map((block) => block.type)).toEqual(["text", "chart", "text"]);
    expect(
      parseAgentChart('{"title":"默认折线","x":["1","2"],"series":[{"name":"收盘","data":[10,11]}]}')
        ?.series[0].type,
    ).toBe("line");
  });

  it("rejects arbitrary chart options and mismatched or non-finite data", () => {
    expect(parseAgentChart('{"title":"x","x":["a","b"],"series":[{"name":"x","type":"pie","data":[1,2]}]}')).toBeNull();
    expect(parseAgentChart('{"title":"x","x":["a","b"],"series":[{"name":"x","type":"line","data":[1]}]}')).toBeNull();
    expect(parseAgentChart('{"title":"x","x":["a","b"],"series":[{"name":"x","type":"line","data":[1,"bad"]}]}')).toBeNull();
  });
});
