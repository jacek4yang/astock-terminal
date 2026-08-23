export type ClarificationKind = "single" | "multiple";

export interface ClarificationOption {
  id: string;
  label: string;
  description?: string;
  recommended?: boolean;
}

export interface ClarificationQuestion {
  id: string;
  header?: string;
  question: string;
  kind: ClarificationKind;
  options: ClarificationOption[];
  allowOther: boolean;
}

export interface ClarificationRequest {
  title: string;
  description?: string;
  questions: ClarificationQuestion[];
}

export interface ParsedClarification {
  request: ClarificationRequest;
  /** Ordinary prose outside the interactive question payload. */
  displayText: string;
  /** True while a streamed structured block has not received its closing fence. */
  pending: boolean;
}

export interface ClarificationDraft {
  selections: Record<string, string[]>;
  other: Record<string, string>;
  submitted?: boolean;
}

const OPEN_FENCE = /```astock-questions\s*/i;
const COMPLETE_FENCE = /```astock-questions\s*([\s\S]*?)```/i;

function cleanText(value: unknown, max = 240): string {
  return typeof value === "string" ? value.trim().slice(0, max) : "";
}

function safeId(value: unknown, fallback: string): string {
  const id = cleanText(value, 64).replace(/[^a-zA-Z0-9_-]/g, "_");
  return id || fallback;
}

function parseStructured(raw: string): ClarificationRequest | null {
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!value || typeof value !== "object") return null;
  const root = value as Record<string, unknown>;
  if (!Array.isArray(root.questions) || root.questions.length < 1 || root.questions.length > 3) {
    return null;
  }
  const usedQuestionIds = new Set<string>();
  const questions: ClarificationQuestion[] = [];
  for (let questionIndex = 0; questionIndex < root.questions.length; questionIndex++) {
    const entry = root.questions[questionIndex];
    if (!entry || typeof entry !== "object") return null;
    const item = entry as Record<string, unknown>;
    const question = cleanText(item.question);
    if (!question || !Array.isArray(item.options) || item.options.length < 2 || item.options.length > 6) {
      return null;
    }
    let questionId = safeId(item.id, `q${questionIndex + 1}`);
    if (usedQuestionIds.has(questionId)) questionId = `q${questionIndex + 1}`;
    usedQuestionIds.add(questionId);
    const usedOptionIds = new Set<string>();
    const options: ClarificationOption[] = [];
    for (let optionIndex = 0; optionIndex < item.options.length; optionIndex++) {
      const rawOption = item.options[optionIndex];
      const option =
        typeof rawOption === "string"
          ? ({ label: rawOption } as Record<string, unknown>)
          : rawOption && typeof rawOption === "object"
            ? (rawOption as Record<string, unknown>)
            : null;
      if (!option) return null;
      const label = cleanText(option.label, 120);
      if (!label) return null;
      let optionId = safeId(option.id, `o${optionIndex + 1}`);
      if (usedOptionIds.has(optionId)) optionId = `o${optionIndex + 1}`;
      usedOptionIds.add(optionId);
      options.push({
        id: optionId,
        label,
        description: cleanText(option.description) || undefined,
        recommended: option.recommended === true,
      });
    }
    questions.push({
      id: questionId,
      header: cleanText(item.header, 32) || undefined,
      question,
      kind: item.kind === "multiple" ? "multiple" : "single",
      options,
      allowOther: item.allow_other !== false,
    });
  }
  return {
    title: cleanText(root.title, 80) || "请确认研究条件",
    description: cleanText(root.description, 300) || undefined,
    questions,
  };
}

function plainMarkdown(line: string): string {
  return line
    .trim()
    .replace(/^#{1,6}\s+/, "")
    .replace(/\*\*/g, "")
    .replace(/__/g, "")
    .trim();
}

function legacyQuestion(line: string): string | null {
  const text = plainMarkdown(line);
  const match = text.match(/^\d{1,2}[.、)]\s*(.+[?？])\s*$/);
  return match?.[1]?.trim() || null;
}

function legacyOption(line: string): { id?: string; label: string } | null {
  const text = line.trim();
  const bullet = text.match(/^(?:[-+*•]|\d+[.)])\s+(.+)$/);
  if (!bullet) return null;
  const cleaned = plainMarkdown(bullet[1]);
  const lettered = cleaned.match(/^([A-Z])[.、)]\s*(.+)$/i);
  const label = (lettered?.[2] ?? cleaned).trim();
  if (!label) return null;
  return { id: lettered?.[1]?.toLowerCase(), label };
}

/**
 * Compatibility parser for older MiniMax answers that used numbered
 * Markdown questions and bullet options. It deliberately requires question
 * marks and 2-6 choices for every question to avoid turning research lists
 * into forms.
 */
function parseLegacy(src: string): ParsedClarification | null {
  const lines = src.split(/\r?\n/);
  const headers = lines
    .map((line, index) => ({ index, question: legacyQuestion(line) }))
    .filter((item): item is { index: number; question: string } => Boolean(item.question));
  if (headers.length < 1 || headers.length > 3) return null;

  const questions: ClarificationQuestion[] = [];
  let lastOptionLine = -1;
  for (let index = 0; index < headers.length; index++) {
    const start = headers[index].index + 1;
    const end = headers[index + 1]?.index ?? lines.length;
    const options: ClarificationOption[] = [];
    for (let lineIndex = start; lineIndex < end; lineIndex++) {
      const parsed = legacyOption(lines[lineIndex]);
      if (!parsed) continue;
      options.push({
        id: parsed.id ?? `o${options.length + 1}`,
        label: parsed.label,
      });
      lastOptionLine = lineIndex;
    }
    if (options.length < 2 || options.length > 6) return null;
    questions.push({
      id: `q${index + 1}`,
      question: headers[index].question,
      kind: "single",
      options,
      allowOther: true,
    });
  }

  const before = lines.slice(0, headers[0].index).join("\n").trim();
  const after = lines.slice(lastOptionLine + 1).join("\n").trim();
  return {
    request: {
      title: "请确认后继续分析",
      description: "选择最符合你的选项；不确定时可以补充说明。",
      questions,
    },
    displayText: [before, after].filter(Boolean).join("\n\n"),
    pending: false,
  };
}

export function parseClarification(src: string): ParsedClarification | null {
  const complete = src.match(COMPLETE_FENCE);
  if (complete) {
    const request = parseStructured(complete[1].trim());
    if (!request) return null;
    return {
      request,
      displayText: src.replace(complete[0], "").trim(),
      pending: false,
    };
  }
  if (OPEN_FENCE.test(src)) {
    return {
      request: { title: "正在准备确认选项", questions: [] },
      displayText: src.slice(0, src.search(OPEN_FENCE)).trim(),
      pending: true,
    };
  }
  return parseLegacy(src);
}

export function hasClarification(src: string): boolean {
  const parsed = parseClarification(src);
  return Boolean(parsed && !parsed.pending && parsed.request.questions.length > 0);
}

export function emptyClarificationDraft(): ClarificationDraft {
  return { selections: {}, other: {} };
}

export function clarificationIsComplete(
  request: ClarificationRequest,
  draft: ClarificationDraft,
): boolean {
  return request.questions.every(
    (question) =>
      (draft.selections[question.id]?.length ?? 0) > 0 ||
      Boolean(draft.other[question.id]?.trim()),
  );
}

export function formatClarificationAnswer(
  request: ClarificationRequest,
  draft: ClarificationDraft,
): string {
  const lines = ["关于你刚才提出的澄清问题，我的确认如下："];
  request.questions.forEach((question, index) => {
    const selectedIds = new Set(draft.selections[question.id] ?? []);
    const selected = question.options
      .filter((option) => selectedIds.has(option.id))
      .map((option) => option.label);
    const other = draft.other[question.id]?.trim();
    const answer = [...selected, ...(other ? [`补充：${other}`] : [])].join("；");
    lines.push(`${index + 1}. ${question.question}：${answer}`);
  });
  lines.push("请把这些条件作为本会话已确认前提，继续制定研究计划并执行；不要重复询问已经确认的事项。");
  return lines.join("\n");
}
