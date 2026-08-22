import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import Markdown, { fixMarkdown } from "./Markdown";

afterEach(cleanup);

describe("Markdown 标准渲染", () => {
  it("将粘连的标题与表头分开且不会显示分隔线数据行", () => {
    const src = [
      "## 仓位金额明细 | 项 | 股数 | 金额（元） | 占比 |",
      "| --- | ---: | ---: | ---: |",
      "| 紫金矿业 | 200 | 6,948 | 34.7% |",
    ].join("\n");

    render(<Markdown src={src} />);

    expect(screen.getByRole("heading", { name: "仓位金额明细" })).toBeInTheDocument();
    const table = screen.getByRole("table");
    expect(within(table).getAllByRole("columnheader")).toHaveLength(4);
    expect(within(table).queryByText(/^---$/)).not.toBeInTheDocument();
    expect(screen.getByText("紫金矿业")).toBeInTheDocument();
  });

  it("使用 GFM 正确渲染中文粗体及常见模型转义", () => {
    const { container } = render(
      <Markdown src={"建议：\\*\\*2万全仓黄金（紫金+山金）\\*\\*的方案风险过高"} />,
    );

    const strong = container.querySelector("strong");
    expect(strong).toHaveTextContent("2万全仓黄金（紫金+山金）");
    expect(container).toHaveTextContent("的方案风险过高");
    expect(container.innerHTML).not.toContain("&lt;!-- --&gt;");
  });

  it("不把孤立或尚未完成的竖线文本猜成表格", () => {
    const src = "估值口径 A | 估值口径 B";
    expect(fixMarkdown(src)).toBe(src);

    render(<Markdown src={src} />);
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });

  it("只在三行列数一致时修复缺少分隔行的表格", () => {
    const src = ["名称 | 收益率", "策略甲 | 12%", "策略乙 | 8%"].join("\n");
    const fixed = fixMarkdown(src);

    expect(fixed).toContain("| --- | --- |");
    expect(fixMarkdown(fixed)).toBe(fixed);
  });

  it("补齐流式输出中尚未闭合的代码围栏", () => {
    expect(fixMarkdown("```python\nprint('ok')")).toBe("```python\nprint('ok')\n```");
  });
});
