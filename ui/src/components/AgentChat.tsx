import { useCallback, useEffect, useRef, useState } from "react";
import { Link } from "react-router-dom";
import {
  agentAsk,
  agentCancel,
  agentConversationDelete,
  agentConversationLoad,
  agentConversations,
  agentResume,
  agentTasks,
  errKind,
  errMsg,
  isTauri,
  minimaxStatus,
  NOT_TAURI_MSG,
  type AgentConversation,
  type AgentMessage,
  type AgentTask,
} from "../lib/api";
import { onAgentEvent, type AgentEvent, type AgentReport } from "../lib/events";
import Markdown from "./Markdown";
import { Term } from "./ui";
import { useAppStore } from "../store";

// ==================== 数据模型 ====================

interface ToolCallItem {
  key: number;
  name: string;
  args?: string;
  done: boolean;
  cacheKey?: string;
  elapsedMs?: number;
  source?: string;
  fetchedAt?: string;
}

interface ChatMsg {
  key: number;
  role: "user" | "assistant";
  /** 流式原始文本(含 <think> 标签) */
  raw: string;
  tools: ToolCallItem[];
  report?: AgentReport;
  suspendedAt?: number;
  failed?: string;
  done: boolean;
}

type RunStatus = "idle" | "running" | "suspended" | "completed" | "failed";

let keySeq = 1;
const nextKey = () => keySeq++;

function fmtUnix(sec: number): string {
  return new Date(sec * 1000).toLocaleString("zh-CN", { hour12: false });
}

/** 拆分 <think>…</think> 思考块(兼容流式未闭合),返回正文与思考内容 */
function splitThink(raw: string): { text: string; think: string } {
  let text = "";
  let think = "";
  let rest = raw;
  for (;;) {
    const i = rest.indexOf("<think>");
    if (i < 0) {
      text += rest;
      break;
    }
    text += rest.slice(0, i);
    rest = rest.slice(i + "<think>".length);
    const j = rest.indexOf("</think>");
    if (j < 0) {
      think += rest;
      rest = "";
      break;
    }
    think += rest.slice(0, j);
    rest = rest.slice(j + "</think>".length);
  }
  // 流式中途可能残留半个标签,隐藏正文中未完整的 "<th…" 尾巴
  const partial = text.match(/<(?:t(?:h(?:i(?:n(?:k)?)?)?)?)?$/);
  if (partial) text = text.slice(0, text.length - partial[0].length);
  return { text, think };
}

// ==================== 子组件 ====================

const STATUS_LABEL: Record<string, { text: string; cls: string }> = {
  running: { text: "运行中", cls: "bg-blue-600/10 text-blue-600 dark:text-blue-400" },
  suspended: { text: "已挂起", cls: "bg-amber-500/10 text-amber-600 dark:text-amber-400" },
  completed: { text: "已完成", cls: "bg-down/10 text-down" },
  failed: { text: "失败", cls: "bg-up/10 text-up" },
};

function StatusBadge({ status }: { status: string }) {
  const s = STATUS_LABEL[status] ?? {
    text: status,
    cls: "bg-slate-200 text-slate-600 dark:bg-slate-800 dark:text-slate-300",
  };
  return <span className={"chip " + s.cls}>{s.text}</span>;
}

/** 单条工具调用时间线卡片 */
function ToolCard({ tool }: { tool: ToolCallItem }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="rounded border border-slate-200 bg-slate-50 px-2.5 py-1.5 dark:border-slate-800 dark:bg-slate-900/60">
      <div className="flex flex-wrap items-center gap-2 text-xs">
        <span
          className={
            "inline-block h-1.5 w-1.5 rounded-full " +
            (tool.done ? "bg-down" : "animate-pulse bg-blue-500")
          }
        />
        <span className="num font-medium">{tool.name}</span>
        {tool.done && tool.elapsedMs != null && (
          <span className="num muted">{(tool.elapsedMs / 1000).toFixed(1)}s</span>
        )}
        {!tool.done && <span className="muted">调用中…</span>}
        {tool.args && (
          <button className="muted underline decoration-dotted underline-offset-2" onClick={() => setOpen(!open)}>
            {open ? "收起参数" : "参数"}
          </button>
        )}
      </div>
      {open && tool.args && (
        <pre className="num muted mt-1 max-h-32 overflow-auto whitespace-pre-wrap break-all text-xs">
          {tool.args}
        </pre>
      )}
      {tool.done && (tool.source || tool.fetchedAt) && (
        <div className="muted num mt-1 text-xs">
          {tool.source && <span>数据源:{tool.source}</span>}
          {tool.fetchedAt && <span className="ml-2">时间:{tool.fetchedAt}</span>}
        </div>
      )}
    </div>
  );
}

/** 助手消息气泡 */
function AssistantMsg({ msg }: { msg: ChatMsg }) {
  const [thinkOpen, setThinkOpen] = useState(false);
  const { text, think } = splitThink(msg.report ? msg.report.answer : msg.raw);
  const answer = msg.report ? msg.report.answer : text;
  return (
    <div className="card anim-fade-up mr-auto w-full max-w-3xl px-3 py-2.5">
      {think.trim() && (
        <div className="mb-2">
          <button
            className="muted flex items-center gap-1 text-xs underline decoration-dotted underline-offset-2"
            onClick={() => setThinkOpen(!thinkOpen)}
          >
            <span className={"inline-block transition-transform " + (thinkOpen ? "rotate-90" : "")}>
              ▸
            </span>
            思考过程
          </button>
          {thinkOpen && (
            <div className="muted mt-1 whitespace-pre-wrap rounded bg-slate-100 px-2.5 py-2 text-xs leading-relaxed dark:bg-slate-800/60">
              {think.trim()}
            </div>
          )}
        </div>
      )}
      {msg.tools.length > 0 && (
        <div className="mb-2 space-y-1.5 border-l-2 border-slate-200 pl-2.5 dark:border-slate-700">
          {msg.tools.map((t) => (
            <ToolCard key={t.key} tool={t} />
          ))}
        </div>
      )}
      {answer.trim() ? (
        <Markdown src={answer} />
      ) : (
        !msg.failed && !msg.done && <div className="muted text-sm">正在思考…</div>
      )}
      {msg.report && msg.report.evidence.length > 0 && (
        <div className="mt-3 rounded border border-slate-200 dark:border-slate-800">
          <div className="muted border-b border-slate-200 px-2.5 py-1.5 text-xs dark:border-slate-800">
            <Term label="证据清单" tip="本回答引用的工具数据快照,可按缓存键溯源复核" />(
            {msg.report.evidence.length})
          </div>
          <ul className="divide-y divide-slate-100 dark:divide-slate-800/60">
            {msg.report.evidence.map((ev, i) => (
              <li key={i} className="num flex flex-wrap gap-x-3 px-2.5 py-1.5 text-xs">
                <span className="font-medium">{ev.tool}</span>
                <span className="muted">来源:{ev.source}</span>
                <span className="muted">时间:{ev.fetched_at}</span>
              </li>
            ))}
          </ul>
        </div>
      )}
      {msg.report && (
        <div className="muted mt-3 rounded border border-amber-200 bg-amber-50 px-2.5 py-2 text-xs dark:border-amber-900/60 dark:bg-amber-950/30">
          免责声明:以上内容由 AI 基于公开数据生成,仅供参考,不构成投资建议。市场有风险,决策需独立。
        </div>
      )}
      {msg.failed && (
        <div className="mt-1 rounded border border-red-300 bg-red-50 px-2.5 py-1.5 text-xs text-red-700 dark:border-red-900 dark:bg-red-950/40 dark:text-red-300">
          {msg.failed}
        </div>
      )}
    </div>
  );
}

// ==================== 对话组件(页面版 / 抽屉版复用) ====================

export default function AgentChat({
  variant = "page",
  onClose,
}: {
  /** page:全页(历史抽屉在左侧);drawer:全局呼出抽屉(历史为覆盖层) */
  variant?: "page" | "drawer";
  onClose?: () => void;
}) {
  const currentSymbol = useAppStore((s) => s.currentSymbol);
  const currentName = useAppStore((s) => s.currentName);
  const [hasKey, setHasKey] = useState<boolean | null>(null);
  const [msgs, setMsgs] = useState<ChatMsg[]>([]);
  const [input, setInput] = useState("");
  const [status, setStatus] = useState<RunStatus>("idle");
  const [tasks, setTasks] = useState<AgentTask[]>([]);
  const [convs, setConvs] = useState<AgentConversation[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const [historyOpen, setHistoryOpen] = useState(false);

  const taskIdRef = useRef<string | null>(null);
  const convIdRef = useRef<string | null>(null);
  const unlistenRef = useRef<(() => void) | undefined>(undefined);
  const scrollRef = useRef<HTMLDivElement>(null);

  const running = status === "running";

  // 滚动到底部
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [msgs, status]);

  const patchLastAssistant = useCallback((fn: (m: ChatMsg) => ChatMsg) => {
    setMsgs((prev) => {
      for (let i = prev.length - 1; i >= 0; i--) {
        if (prev[i].role === "assistant") {
          const next = [...prev];
          next[i] = fn(next[i]);
          return next;
        }
      }
      return prev;
    });
  }, []);

  const handleEvent = useCallback(
    (ev: AgentEvent) => {
      switch (ev.type) {
        case "text_delta":
          setStatus("running");
          patchLastAssistant((m) => ({ ...m, raw: m.raw + ev.text, suspendedAt: undefined }));
          break;
        case "tool_call_started":
          patchLastAssistant((m) => ({
            ...m,
            tools: [
              ...m.tools,
              {
                key: nextKey(),
                name: ev.name,
                args:
                  ev.args == null
                    ? undefined
                    : typeof ev.args === "string"
                      ? ev.args
                      : JSON.stringify(ev.args),
                done: false,
              },
            ],
          }));
          break;
        case "tool_call_finished":
          patchLastAssistant((m) => {
            const tools = [...m.tools];
            for (let i = tools.length - 1; i >= 0; i--) {
              if (!tools[i].done && tools[i].name === ev.name) {
                tools[i] = {
                  ...tools[i],
                  done: true,
                  cacheKey: ev.cache_key,
                  elapsedMs: ev.elapsed_ms,
                };
                break;
              }
            }
            return { ...m, tools };
          });
          break;
        case "suspended":
          setStatus("suspended");
          patchLastAssistant((m) => ({ ...m, suspendedAt: ev.reset_at_unix }));
          break;
        case "completed":
          setStatus("completed");
          patchLastAssistant((m) => {
            const byCache = new Map(ev.report.evidence.map((e) => [e.cache_key, e]));
            return {
              ...m,
              report: ev.report,
              suspendedAt: undefined,
              done: true,
              tools: m.tools.map((t) => {
                const evd = t.cacheKey ? byCache.get(t.cacheKey) : undefined;
                return evd ? { ...t, source: evd.source, fetchedAt: evd.fetched_at } : t;
              }),
            };
          });
          break;
        case "failed":
          setStatus("failed");
          patchLastAssistant((m) => ({ ...m, failed: ev.error, done: true }));
          break;
      }
    },
    [patchLastAssistant],
  );

  const subscribeTask = useCallback(
    (taskId: string) => {
      unlistenRef.current?.();
      taskIdRef.current = taskId;
      onAgentEvent(taskId, handleEvent).then((u) => {
        if (taskIdRef.current === taskId) unlistenRef.current = u;
        else u();
      });
    },
    [handleEvent],
  );

  useEffect(() => () => unlistenRef.current?.(), []);

  const refreshTasks = useCallback(async () => {
    try {
      setTasks(await agentTasks());
    } catch {
      /* 轮询失败忽略 */
    }
  }, []);

  const refreshConvs = useCallback(async () => {
    try {
      setConvs(await agentConversations());
    } catch {
      /* 忽略 */
    }
  }, []);

  // 初始化:key 检测 + 历史列表 + 刷新后恢复未结束任务
  useEffect(() => {
    if (!isTauri()) return;
    minimaxStatus()
      .then((s) => setHasKey(s.has_key))
      .catch(() => setHasKey(null));
    refreshTasks();
    refreshConvs();
    agentTasks()
      .then((list) => {
        const pending = list
          .filter((t) => t.status === "running" || t.status === "suspended")
          .sort((a, b) => b.updated_at - a.updated_at)[0];
        if (pending && !taskIdRef.current) {
          setStatus(pending.status === "suspended" ? "suspended" : "running");
          setMsgs((prev) =>
            prev.length === 0
              ? [
                  {
                    key: nextKey(),
                    role: "assistant",
                    raw:
                      pending.status === "suspended"
                        ? "检测到上次未完成的任务(配额挂起)。配额恢复后点击下方「继续分析」即可接着运行。"
                        : "已恢复上次运行中的任务,正在等待后续输出…",
                    tools: [],
                    suspendedAt: pending.status === "suspended" ? pending.updated_at : undefined,
                    done: false,
                  },
                ]
              : prev,
          );
          subscribeTask(pending.id);
        }
      })
      .catch(() => {});
  }, [refreshTasks, refreshConvs, subscribeTask]);

  // 任务列表轮询(挂起/运行状态刷新)
  useEffect(() => {
    const t = setInterval(refreshTasks, 5000);
    return () => clearInterval(t);
  }, [refreshTasks]);

  const send = async (question: string) => {
    const q = question.trim();
    if (!q || running) return;
    setErr(null);
    setInput("");
    setMsgs((prev) => [
      ...prev,
      { key: nextKey(), role: "user", raw: q, tools: [], done: true },
      { key: nextKey(), role: "assistant", raw: "", tools: [], done: false },
    ]);
    // 新会话首轮:把当前查看的股票作为上下文前置(气泡仍只显示用户原文)
    let payload = q;
    if (convIdRef.current === null && currentSymbol) {
      payload = `【上下文】用户在查看 ${currentSymbol} ${currentName ?? ""}\n`.replace(/\s+\n/, "\n") + q;
    }
    try {
      const r = await agentAsk(payload, convIdRef.current);
      convIdRef.current = r.conversation_id;
      setStatus("running");
      subscribeTask(r.task_id);
      refreshTasks();
      refreshConvs();
    } catch (e) {
      if (errKind(e) === "no_key") {
        setHasKey(false);
        patchLastAssistant((m) => ({
          ...m,
          failed: "尚未配置 MiniMax API Key,请先到「设置」页填写后再提问。",
          done: true,
        }));
      } else {
        patchLastAssistant((m) => ({ ...m, failed: errMsg(e), done: true }));
      }
      setStatus("failed");
    }
  };

  const resume = async () => {
    const id = taskIdRef.current;
    if (!id) return;
    setErr(null);
    try {
      await agentResume(id);
      setStatus("running");
      patchLastAssistant((m) => ({ ...m, suspendedAt: undefined }));
      subscribeTask(id);
    } catch (e) {
      setErr(errMsg(e));
    }
  };

  const cancel = async () => {
    const id = taskIdRef.current;
    if (!id) return;
    try {
      await agentCancel(id);
      setStatus("failed");
      patchLastAssistant((m) => ({ ...m, failed: "已手动取消。", done: true }));
      refreshTasks();
    } catch (e) {
      setErr(errMsg(e));
    }
  };

  /** 从任务列表恢复某个挂起任务 */
  const resumeTask = (t: AgentTask) => {
    setStatus("suspended");
    setMsgs((prev) => [
      ...prev,
      {
        key: nextKey(),
        role: "assistant",
        raw: "已选中挂起的任务。配额恢复后点击下方「继续分析」。",
        tools: [],
        suspendedAt: t.updated_at,
        done: false,
      },
    ]);
    subscribeTask(t.id);
  };

  /** 加载历史会话 */
  const loadConversation = async (c: AgentConversation) => {
    setErr(null);
    try {
      const history = await agentConversationLoad(c.id);
      convIdRef.current = c.id;
      const out: ChatMsg[] = [];
      for (const m of history) {
        out.push(...historyToMsgs(m, out));
      }
      setMsgs(out);
      setStatus("idle");
      setHistoryOpen(false);
    } catch (e) {
      setErr(errMsg(e));
    }
  };

  /** 新对话:清空当前上下文,下一次提问开启新 conversation */
  const newChat = () => {
    if (running) return;
    convIdRef.current = null;
    taskIdRef.current = null;
    unlistenRef.current?.();
    unlistenRef.current = undefined;
    setMsgs([]);
    setStatus("idle");
    setErr(null);
    setHistoryOpen(false);
  };

  /** 删除历史会话(确认后);删当前会话则开新对话 */
  const deleteConversation = async (c: AgentConversation) => {
    if (!window.confirm(`删除会话「${c.title || "未命名会话"}」?该操作不可恢复。`)) return;
    setErr(null);
    try {
      await agentConversationDelete(c.id);
      if (convIdRef.current === c.id) newChat();
      await refreshConvs();
    } catch (e) {
      setErr(errMsg(e));
    }
  };

  const chips = [
    currentSymbol ? `全面分析当前股票 ${currentSymbol}` : "全面分析当前股票",
    "为什么最近跑输行业",
    "扫描今日强势信号股",
    "我的自选股有风险吗",
  ];

  const suspendedMsg = [...msgs].reverse().find((m) => m.role === "assistant" && m.suspendedAt);

  const historyPanel = (
    <aside
      className={
        "anim-fade-in flex shrink-0 flex-col border-slate-200 bg-white dark:border-slate-800 dark:bg-slate-900 " +
        (variant === "page"
          ? "w-64 border-r"
          : "absolute inset-y-0 left-0 z-30 w-60 border-r shadow-xl")
      }
    >
      <div className="card-title">
        历史会话
        <button className="btn-primary ml-auto !px-2 !py-0.5" onClick={newChat}>
          新对话
        </button>
        <button className="muted text-xs font-normal" onClick={() => setHistoryOpen(false)}>
          收起
        </button>
      </div>
      <div className="min-h-0 flex-1 space-y-3 overflow-y-auto p-2">
        <div>
          <div className="micro-label px-1 pb-1">会话</div>
          {convs.length === 0 ? (
            <div className="muted px-1 text-xs">暂无历史会话</div>
          ) : (
            convs.map((c) => (
              <div
                key={c.id}
                className="group flex items-center gap-1 rounded px-2 py-1.5 text-xs hover:bg-slate-100 dark:hover:bg-slate-800"
              >
                <button className="min-w-0 flex-1 text-left" onClick={() => loadConversation(c)}>
                  <div className="truncate">{c.title || "未命名会话"}</div>
                  <div className="num muted mt-0.5">{fmtUnix(c.created_at)}</div>
                </button>
                <button
                  className="muted shrink-0 rounded px-1 opacity-0 transition-opacity hover:text-red-500 group-hover:opacity-100"
                  title="删除该会话"
                  onClick={() => deleteConversation(c)}
                >
                  ✕
                </button>
              </div>
            ))
          )}
        </div>
        <div>
          <div className="micro-label px-1 pb-1">任务</div>
          {tasks.length === 0 ? (
            <div className="muted px-1 text-xs">暂无任务记录</div>
          ) : (
            tasks.map((t) => (
              <div
                key={t.id}
                className="flex items-center gap-2 rounded px-2 py-1.5 text-xs hover:bg-slate-100 dark:hover:bg-slate-800"
              >
                <StatusBadge status={t.status} />
                <span className="num muted truncate">{fmtUnix(t.created_at)}</span>
                {t.status === "suspended" && (
                  <button className="btn ml-auto shrink-0" onClick={() => resumeTask(t)}>
                    继续
                  </button>
                )}
              </div>
            ))
          )}
        </div>
      </div>
    </aside>
  );

  return (
    <div className="relative flex h-full min-h-0 overflow-hidden">
      {historyOpen && historyPanel}

      {/* 对话主区 */}
      <section className="flex min-w-0 flex-1 flex-col">
        <div className="flex items-center gap-3 border-b border-slate-200 px-4 py-2.5 dark:border-slate-800">
          <h1 className="text-sm font-semibold text-slate-700 dark:text-slate-300">AI 助手</h1>
          <span className="muted text-xs">
            <Term label="投研 Agent" tip="调用行情/分析工具取数后由大模型生成解读,过程与证据均可溯源" />
          </span>
          {currentSymbol && (
            <span className="chip bg-blue-600/10 text-blue-600 dark:text-blue-400">
              上下文:{currentName ?? currentSymbol}
            </span>
          )}
          <div className="ml-auto flex items-center gap-2">
            <button className="btn-primary" onClick={newChat} disabled={running}>
              新对话
            </button>
            {!historyOpen && (
              <button className="btn" onClick={() => setHistoryOpen(true)}>
                历史
              </button>
            )}
            {onClose && (
              <button className="btn" title="关闭" onClick={onClose}>
                ✕
              </button>
            )}
          </div>
        </div>

        {hasKey === false && (
          <div className="flex items-center gap-2 border-b border-amber-200 bg-amber-50 px-4 py-2 text-xs text-amber-800 dark:border-amber-900/60 dark:bg-amber-950/40 dark:text-amber-300">
            <span>尚未配置 MiniMax API Key,AI 助手不可用。</span>
            <Link to="/settings" className="font-medium underline underline-offset-2">
              前往设置页配置
            </Link>
          </div>
        )}
        {!isTauri() && (
          <div className="border-b border-red-200 bg-red-50 px-4 py-2 text-xs text-red-700 dark:border-red-900 dark:bg-red-950/40 dark:text-red-300">
            {NOT_TAURI_MSG}
          </div>
        )}

        {/* 消息区 */}
        <div ref={scrollRef} className="min-h-0 flex-1 space-y-3 overflow-y-auto p-4">
          {msgs.length === 0 && (
            <div className="muted py-10 text-center text-sm">
              向 AI 助手提问,或点击下方快捷模板开始
            </div>
          )}
          {msgs.map((m) =>
            m.role === "user" ? (
              <div key={m.key} className="anim-fade-up flex justify-end">
                <div className="max-w-2xl whitespace-pre-wrap rounded bg-blue-600 px-3 py-2 text-sm text-white">
                  {m.raw}
                </div>
              </div>
            ) : (
              <AssistantMsg key={m.key} msg={m} />
            ),
          )}

          {status === "suspended" && (
            <div className="rounded border border-amber-300 bg-amber-50 px-3 py-2.5 text-sm text-amber-800 dark:border-amber-900/60 dark:bg-amber-950/40 dark:text-amber-300">
              <div className="flex flex-wrap items-center gap-3">
                <span>
                  配额已用尽,任务已保存。
                  {suspendedMsg?.suspendedAt && (
                    <>
                      恢复时间:
                      <span className="num font-medium">{fmtUnix(suspendedMsg.suspendedAt)}</span>。
                    </>
                  )}
                  配额恢复后点击继续。
                </span>
                <button className="btn-primary" onClick={resume}>
                  继续分析
                </button>
              </div>
            </div>
          )}
          {err && (
            <div className="rounded border border-red-300 bg-red-50 px-3 py-2 text-xs text-red-700 dark:border-red-900 dark:bg-red-950/40 dark:text-red-300">
              {err}
            </div>
          )}
        </div>

        {/* 输入区 */}
        <div className="shrink-0 border-t border-slate-200 p-3 dark:border-slate-800">
          <div className="mb-2 flex flex-wrap gap-1.5">
            {chips.map((c) => (
              <button
                key={c}
                className="tag bg-slate-200 text-slate-600 transition-colors hover:bg-slate-300 dark:bg-slate-800 dark:text-slate-300 dark:hover:bg-slate-700"
                onClick={() => setInput(c)}
              >
                {c}
              </button>
            ))}
          </div>
          <div className="flex items-end gap-2">
            <textarea
              className="input max-h-32 min-h-[38px] flex-1 resize-y"
              placeholder="输入问题,Enter 发送(Shift+Enter 换行)"
              value={input}
              disabled={hasKey === false}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  send(input);
                }
              }}
            />
            {running ? (
              <button className="btn-danger" onClick={cancel}>
                取消
              </button>
            ) : (
              <button
                className="btn-primary"
                disabled={!input.trim() || hasKey === false}
                onClick={() => send(input)}
              >
                发送
              </button>
            )}
          </div>
        </div>
      </section>
    </div>
  );
}

/** 历史消息(role/content JSON)转换为气泡:工具消息折叠进上一条助手消息的轨迹 */
function historyToMsgs(m: AgentMessage, out: ChatMsg[]): ChatMsg[] {
  if (m.role === "user") {
    return [{ key: nextKey(), role: "user", raw: m.content, tools: [], done: true }];
  }
  if (m.role === "assistant") {
    return [
      {
        key: nextKey(),
        role: "assistant",
        raw: m.content,
        tools: [],
        done: true,
      },
    ];
  }
  // tool 等其他角色:折叠为上一条助手消息的工具轨迹
  const last = out[out.length - 1];
  if (last && last.role === "assistant") {
    let name = "tool";
    try {
      const j = JSON.parse(m.content) as Record<string, unknown>;
      if (typeof j.name === "string") name = j.name;
      else if (typeof j.tool === "string") name = j.tool;
    } catch {
      /* content 非 JSON 时保持默认名 */
    }
    last.tools = [
      ...last.tools,
      { key: nextKey(), name, args: m.content, done: true },
    ];
    return [];
  }
  return [];
}
