import { describe, expect, it } from "vitest";
import { sanitizeAgentVisibleText, stripPrivateReasoning } from "./agentVisibleText";

describe("v6 Agent visible-text boundary", () => {
  it("never displays complete, incomplete or split private reasoning", () => {
    expect(stripPrivateReasoning("结论前<think>私有推理</think>结论后")).toBe("结论前结论后");
    expect(stripPrivateReasoning("结论<think>未结束的推理")).toBe("结论");
    expect(stripPrivateReasoning("结论<thi")).toBe("结论");
  });

  it("redacts echoed secrets while preserving evidence identity", () => {
    const visible = sanitizeAgentVisibleText(
      "证据 evf_price，source_version_id=sv_42；authorization=Bearer abc.def；api_key=must-not-leak",
    );
    expect(visible).toContain("evf_price");
    expect(visible).toContain("source_version_id=sv_42");
    expect(visible).not.toContain("abc.def");
    expect(visible).not.toContain("must-not-leak");
    expect(visible).toContain("[已隐藏敏感信息]");
  });
});
