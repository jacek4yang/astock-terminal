import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { ReactNode } from "react";
import AgentChart, { splitAgentContent } from "./AgentChart";

/* ==================== fixMarkdown:校验与修正预处理 ====================
 * 纯函数,负责把模型输出的“半吊子” Markdown 规整为可稳定渲染的形式:
 * 1. 全角管道 `｜` 规整为半角 `|`(表格行内),全角连字符在分隔行规整为 `-`
 * 2. 表头后缺失 `|---|` 分隔行时自动插入
 * 3. 数据行管道符与表头列数不齐时补齐(空单元格)/截断(多余并入最后一列)
 * 4. 表格前若不是空行则补空行(GFM 要求表格独占块)
 * 5. 去掉孤立 `###` 碎片行(只有井号没有内容)
 * 6. 代码围栏 ``` 未闭合时补收尾
 */

/** 拆一行表格为单元格数组(去掉首尾的管道符再切分) */
function splitRow(line: string): string[] {
  let l = line.trim();
  if (l.startsWith("|")) l = l.slice(1);
  if (l.endsWith("|")) l = l.slice(1);
  return l.split("|").map((s) => s.trim());
}

/** 是否是表格分隔行,如 | --- | :---: | ---: | */
function isSeparatorRow(line: string): boolean {
  const cells = splitRow(line);
  if (cells.length === 0) return false;
  return cells.every((c) => /^:?-{1,}:?$/.test(c.replace(/\s/g, "")));
}

/** 该行是否像表格行(至少含一个管道符,且切得出 ≥2 列) */
function isTableRow(line: string): boolean {
  return line.includes("|") && splitRow(line).length >= 2;
}

/** 用列数重建一行:不足补空,超出并入最后一列 */
function normalizeRow(cells: string[], cols: number): string {
  const out = cells.slice(0, cols);
  if (cells.length > cols) {
    // 多余单元格并入最后一列,避免丢数据
    out[cols - 1] = [out[cols - 1], ...cells.slice(cols)].filter(Boolean).join(" ");
  }
  while (out.length < cols) out.push("");
  return "| " + out.join(" | ") + " |";
}

function makeSeparator(cols: number): string {
  return "| " + Array.from({ length: cols }, () => "---").join(" | ") + " |";
}

export function fixMarkdown(src: string): string {
  const rawLines = src.split("\n");
  const lines: string[] = [];
  let inFence = false;

  // 第一遍:围栏状态 + 全角字符规整 + 孤立标题碎片
  for (const raw of rawLines) {
    const line = raw.replace(/\r$/, "");
    if (/^\s*```/.test(line)) {
      inFence = !inFence;
      lines.push(line);
      continue;
    }
    if (inFence) {
      lines.push(line);
      continue;
    }
    // 孤立 ### 碎片(只有井号和空白)
    if (/^\s*#{1,6}\s*$/.test(line)) continue;
    // 全角管道规整(仅在像表格行的行内)
    let fixed = line;
    if (fixed.includes("｜")) fixed = fixed.replace(/｜/g, "|");
    // 全角/长连字符在“分隔行样式”的行里规整为 -
    if (/^[\s|:：—–-]+$/.test(fixed) && /[—–]/.test(fixed)) {
      fixed = fixed.replace(/[—–]/g, "-").replace(/:/g, ":");
    }
    lines.push(fixed);
  }
  // 围栏未闭合:补齐
  if (inFence) lines.push("```");

  // 第二遍:表格修正(围栏外)
  const out: string[] = [];
  inFence = false;
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (/^\s*```/.test(line)) {
      inFence = !inFence;
      out.push(line);
      i++;
      continue;
    }
    if (inFence || !isTableRow(line)) {
      out.push(line);
      i++;
      continue;
    }
    // 收集连续表格行块
    const block: string[] = [];
    while (i < lines.length && isTableRow(lines[i]) && !/^\s*```/.test(lines[i])) {
      block.push(lines[i]);
      i++;
    }
    // 单行且不像表头(下一行不是分隔行也无法构成表格):按原文输出
    if (block.length === 1 && !isSeparatorRow(block[0])) {
      // 孤立的一行含管道,可能是不完整表头:补分隔行让它至少能成表
      const cols = splitRow(block[0]).length;
      if (out.length > 0 && out[out.length - 1].trim() !== "") out.push("");
      out.push(normalizeRow(splitRow(block[0]), cols));
      out.push(makeSeparator(cols));
      continue;
    }
    const headerCells = splitRow(block[0]);
    const cols = headerCells.length;
    // 表格前需要空行(GFM 块级要求)
    if (out.length > 0 && out[out.length - 1].trim() !== "") out.push("");
    out.push(normalizeRow(headerCells, cols));
    let bodyStart = 1;
    if (block.length > 1 && isSeparatorRow(block[1])) {
      out.push(makeSeparator(cols));
      bodyStart = 2;
    } else {
      // 缺失分隔行:插入
      out.push(makeSeparator(cols));
    }
    for (let r = bodyStart; r < block.length; r++) {
      if (isSeparatorRow(block[r])) continue; // 多余分隔行丢弃
      out.push(normalizeRow(splitRow(block[r]), cols));
    }
  }

  return out.join("\n");
}

/* ==================== 渲染 ==================== */

/** 判断文本是否像数值(用于右对齐 + tabular-nums) */
function looksNumeric(text: string): boolean {
  const t = text.trim();
  if (t === "") return false;
  // 数字、千分位、小数、百分号、正负号、货币/单位后缀
  return /^[+-]?[\d,]+(\.\d+)?\s*[%亿万千元倍]?$/.test(t) || /^[+-]?\d+(\.\d+)?%?$/.test(t);
}

function textOf(node: ReactNode): string {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textOf).join("");
  if (typeof node === "object" && "props" in (node as object)) {
    return textOf((node as { props: { children?: ReactNode } }).props.children);
  }
  return "";
}

function MarkdownBody({ src }: { src: string }) {
  return (
    <div className="md space-y-2 text-sm leading-relaxed">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          h1: ({ children }) => (
            <h1 className="border-b border-slate-200 pb-1 pt-2 text-base font-bold dark:border-slate-800">
              {children}
            </h1>
          ),
          h2: ({ children }) => (
            <h2 className="border-b border-slate-200 pb-1 pt-2 text-[15px] font-bold dark:border-slate-800">
              {children}
            </h2>
          ),
          h3: ({ children }) => (
            <h3 className="pt-1.5 text-sm font-semibold text-slate-800 dark:text-slate-100">
              {children}
            </h3>
          ),
          h4: ({ children }) => (
            <h4 className="pt-1 text-sm font-semibold text-slate-700 dark:text-slate-200">
              {children}
            </h4>
          ),
          p: ({ children }) => <p className="whitespace-pre-wrap">{children}</p>,
          strong: ({ children }) => (
            <strong className="font-semibold text-slate-900 dark:text-slate-50">{children}</strong>
          ),
          ul: ({ children }) => <ul className="list-disc space-y-1 pl-5">{children}</ul>,
          ol: ({ children }) => <ol className="list-decimal space-y-1 pl-5">{children}</ol>,
          li: ({ children }) => <li className="leading-relaxed">{children}</li>,
          code: ({ className, children }) => {
            const isBlock = /language-/.test(className ?? "");
            if (isBlock) return <code className={className}>{children}</code>;
            return (
              <code className="num rounded bg-slate-100 px-1 py-0.5 text-xs text-slate-700 dark:bg-slate-800 dark:text-slate-300">
                {children}
              </code>
            );
          },
          pre: ({ children }) => (
            <pre className="num overflow-x-auto rounded bg-slate-100 p-2.5 text-xs leading-relaxed dark:bg-slate-800/70">
              {children}
            </pre>
          ),
          blockquote: ({ children }) => (
            <blockquote className="muted border-l-[3px] border-slate-300 pl-3 dark:border-slate-600">
              {children}
            </blockquote>
          ),
          hr: () => <hr className="border-slate-200 dark:border-slate-800" />,
          // 应用内不做外链跳转:链接渲染为带 title 的文本
          a: ({ href, children }) => (
            <span
              className="font-medium text-blue-600 decoration-dotted dark:text-blue-400"
              title={href}
            >
              {children}
            </span>
          ),
          table: ({ children }) => (
            <div className="max-h-96 overflow-auto rounded border border-slate-200 dark:border-slate-800">
              <table className="w-full border-collapse text-xs">{children}</table>
            </div>
          ),
          thead: ({ children }) => (
            <thead className="sticky top-0 z-10 bg-slate-50 dark:bg-slate-800/95">{children}</thead>
          ),
          tbody: ({ children }) => (
            <tbody className="[&>tr:nth-child(even)]:bg-slate-50/60 dark:[&>tr:nth-child(even)]:bg-slate-800/30">
              {children}
            </tbody>
          ),
          tr: ({ children }) => (
            <tr className="border-b border-slate-100 transition-colors hover:bg-blue-50/50 dark:border-slate-800/60 dark:hover:bg-slate-700/30">
              {children}
            </tr>
          ),
          th: ({ children }) => {
            const numeric = looksNumeric(textOf(children));
            return (
              <th
                className={
                  "whitespace-nowrap px-2.5 py-2 text-[11px] font-semibold text-slate-500 dark:text-slate-400 " +
                  (numeric ? "num text-right" : "text-left")
                }
              >
                {children}
              </th>
            );
          },
          td: ({ children }) => {
            const numeric = looksNumeric(textOf(children));
            return (
              <td
                className={
                  "whitespace-nowrap px-2.5 py-[7px] " +
                  (numeric ? "num text-right tabular-nums" : "")
                }
              >
                {children}
              </td>
            );
          },
        }}
      >
        {fixMarkdown(src)}
      </ReactMarkdown>
    </div>
  );
}

/** Markdown plus a safe, declarative chart block chosen by the research agent. */
export default function Markdown({ src }: { src: string }) {
  const blocks = splitAgentContent(src);
  return (
    <div className="space-y-3">
      {blocks.map((block, index) =>
        block.type === "text" ? (
          block.content.trim() ? <MarkdownBody key={index} src={block.content} /> : null
        ) : block.spec ? (
          <AgentChart key={index} spec={block.spec} />
        ) : (
          <div key={index} className="rounded border border-amber-200 bg-amber-50 px-2.5 py-2 text-xs text-amber-700 dark:border-amber-900/60 dark:bg-amber-950/30 dark:text-amber-300">
            智能助手生成的图表数据格式不完整，已安全跳过；文字结论不受影响。
          </div>
        ),
      )}
    </div>
  );
}
