export const BROWSER_CDP_SCENARIOS = Object.freeze([
  "market-overview",
  "stock-detail",
  "agent-task",
  "conversation-history",
  "conversation-branch",
  "dynamic-clarification",
  "tool-activity",
  "evidence-navigation",
  "settings",
  "responsive-1200",
  "responsive-900",
  "console-clean",
]);

export const BROWSER_CDP_ASSERTION_ANCHORS = Object.freeze({
  "market-overview": ["market-index-values-visible", "market-quality-state-visible"],
  "stock-detail": ["canonical-security-identity", "complete-stock-sections-visible"],
  "agent-task": ["durable-agent-task-started", "verified-report-or-explicit-blocker"],
  "conversation-history": ["history-survives-page-switch", "persisted-conversation-reloads"],
  "conversation-branch": ["branch-origin-recorded", "branch-research-restarts"],
  "dynamic-clarification": ["model-generated-question-visible", "agent-best-choice-available"],
  "tool-activity": ["tool-lifecycle-visible", "cache-evidence-metadata-visible"],
  "evidence-navigation": ["evidence-reference-opens", "source-version-visible"],
  settings: ["credential-secret-not-echoed", "provider-quota-cache-controls-visible"],
  "responsive-1200": ["no-overlap-at-1199", "adaptive-side-panel-at-1199"],
  "responsive-900": ["single-column-at-899", "no-horizontal-reading-overflow"],
  "console-clean": ["console-errors-empty", "console-warnings-empty"],
});

export const BROWSER_CDP_PROCEDURES = Object.freeze({
  "market-overview": {
    viewport: { width: 1440, height: 900 },
    actions: ["打开“今日市场”并等待 Engine ready", "刷新行情并检查指数卡、来源时间和数据质量状态"],
    expected: {
      "market-index-values-visible": "至少一个主要指数显示来自真实 Engine 的非占位数值；缺失值不得显示为 0",
      "market-quality-state-visible": "行情来源、刷新时间以及缺失、过期、冲突或降级状态可直接看到",
    },
  },
  "stock-detail": {
    viewport: { width: 1440, height: 900 },
    actions: ["在证券搜索输入 300308 并回车", "核对标准名称后依次检查图表、盘口、基本面、估值、资讯和证据区域"],
    expected: {
      "canonical-security-identity": "证券必须标准化显示为 300308 中际旭创，且市场身份在各区域一致",
      "complete-stock-sections-visible": "K线、盘口、趋势、资金、基本面、估值、资讯与证据入口均可访问；不支持的数据显式降级",
    },
  },
  "agent-task": {
    viewport: { width: 1440, height: 900 },
    actions: ["打开“Agent 智研”并新建完整研究任务", "观察任务编号、阶段和报告校验；证据不足时检查明确阻断"],
    expected: {
      "durable-agent-task-started": "任务具有 Engine 持久化 task_id 和可恢复检查点状态，不以 React localStorage 充当执行真相",
      "verified-report-or-explicit-blocker": "任务只发布通过 Engine 校验的人工研究报告；否则明确显示 WaitingForUser、Suspended 或 VerificationFailed",
    },
  },
  "conversation-history": {
    viewport: { width: 1440, height: 900 },
    actions: ["保存一条研究会话并切换到今日市场或配置", "返回 Agent，打开历史并重新载入该会话"],
    expected: {
      "history-survives-page-switch": "一级页面切换后当前会话文本、任务标识和进度仍完整",
      "persisted-conversation-reloads": "历史列表从 Engine 持久化记录重新载入同一会话，而非仅依赖页面内存",
    },
  },
  "conversation-branch": {
    viewport: { width: 1440, height: 900 },
    actions: ["在历史消息上选择“从此分支”", "确认新会话的来源节点并启动重新取数的研究"],
    expected: {
      "branch-origin-recorded": "新会话记录原会话、原消息或持久化检查点来源，且原会话保持不变",
      "branch-research-restarts": "分支创建新的任务并重新取得最新数据；旧结论只作为待核验线索",
    },
  },
  "dynamic-clarification": {
    viewport: { width: 1440, height: 900 },
    actions: ["使用轮换后的 MiniMax 提交缺少期限或风险边界的研究目标", "等待模型生成问题并选择“由 Agent 选择最优方案”"],
    expected: {
      "model-generated-question-visible": "澄清问题及选项来自本次模型响应并与缺失 TaskSpec 字段相关，不是前端固定问卷",
      "agent-best-choice-available": "每个可委托问题提供由 Agent 基于证据选择的选项，同时保留模型选项和自由输入",
    },
  },
  "tool-activity": {
    viewport: { width: 1440, height: 900 },
    actions: ["运行允许全部工具的研究任务", "在右侧检查至少一次工具开始、完成或失败以及缓存/证据元数据"],
    expected: {
      "tool-lifecycle-visible": "工具活动显示名称、输入摘要、状态、耗时和与任务对应关系，不显示私有推理链",
      "cache-evidence-metadata-visible": "活动显示真实缓存命中状态、来源版本和证据编号；无样本时显示未知而非伪造命中率",
    },
  },
  "evidence-navigation": {
    viewport: { width: 1440, height: 900 },
    actions: ["从工具活动或校验后的报告打开一个证据编号", "核对来源详情中的版本、发布时间、抓取时间和原文定位"],
    expected: {
      "evidence-reference-opens": "报告或工具中的证据编号可打开对应证据详情，且证券与结论上下文一致",
      "source-version-visible": "证据详情显示 source_version_id、来源、发布时间/数据截至时间和质量状态",
    },
  },
  settings: {
    viewport: { width: 1440, height: 900 },
    actions: ["打开“配置”并检查 Provider、额度、缓存和存储区域", "确认所有凭据输入为空或掩码且页面状态不回显秘密"],
    expected: {
      "credential-secret-not-echoed": "MiniMax、聚宽及可选 Provider 的秘密值不出现在 DOM、错误、日志或状态文本中",
      "provider-quota-cache-controls-visible": "模型连接/额度、缓存统计与安全清理、数据目录和诊断控制均可见且状态可辨识",
    },
  },
  "responsive-1200": {
    viewport: { width: 1199, height: 900 },
    actions: ["将内置浏览器视口精确设为 1199×900", "遍历今日市场、个股详情、Agent 和配置并检查侧栏适配"],
    expected: {
      "no-overlap-at-1199": "1199px 下标题、数值、表格、图表、输入框和按钮不重叠或裁掉关键内容",
      "adaptive-side-panel-at-1199": "侧栏按设计折叠或转为抽屉，主内容自然扩展且仍可访问全部信息",
    },
  },
  "responsive-900": {
    viewport: { width: 899, height: 900 },
    actions: ["将内置浏览器视口精确设为 899×900", "检查个股图表/分析与 Agent 历史/活动改为单列或标签切换"],
    expected: {
      "single-column-at-899": "899px 下复杂双栏区域改为单列或显式标签/抽屉，不保留不可读的压缩双栏",
      "no-horizontal-reading-overflow": "正文、表格可见列、工具记录和输入区无需横向滚动即可阅读核心内容",
    },
  },
  "console-clean": {
    viewport: { width: 1440, height: 900 },
    actions: ["完成其余 11 个浏览器场景并收集整段控制台事件", "筛查 error、warning、未处理 Promise、React 警告和 Bridge 协议错误"],
    expected: {
      "console-errors-empty": "完整浏览器验收期间控制台 error 和未处理异常数量为 0",
      "console-warnings-empty": "完整浏览器验收期间控制台 warning、React 警告和协议降级警告数量为 0",
    },
  },
});

export const NATIVE_WINDOW_SCENARIOS = Object.freeze([
  "packaged-launch",
  "window-drag",
  "window-double-click-maximize",
  "window-restore",
  "window-edge-resize",
  "window-minimize",
  "taskbar-icon-high-dpi",
  "native-context-menu",
]);

// These are the v6 three-surface acceptance scenarios. They retain the
// applicable cases from the original workbench brief while replacing generic
// IDE docking/command-palette checks with the later user-approved adjustable,
// responsive 今日市场 / Agent 智研 / 配置 design.
export const DESKTOP_E2E_SCENARIOS = Object.freeze([
  "packaged-launch",
  "cef-react-render",
  "desktop-bridge",
  "engine-agent-handshake",
  "market-overview",
  "stock-detail-data",
  "kline-orderbook",
  "fundamentals-valuation",
  "news-pagination",
  "graph",
  "quant-job",
  "backtest",
  "normal-agent-research",
  "multiple-tool-calls",
  "long-agent-task",
  "conversation-history",
  "conversation-branch",
  "dynamic-clarification",
  "task-resume",
  "application-restart-task-resume",
  "cancellation",
  "context-compaction",
  "verification-failure",
  "successful-verified-report",
  "evidence-navigation",
  "provider-disconnect",
  "quota-suspension",
  "agent-worker-recovery",
  "engine-worker-recovery",
  "renderer-crash-recovery",
  "window-drag",
  "window-double-click-maximize",
  "window-restore",
  "window-edge-resize",
  "window-minimize",
  "taskbar-icon-high-dpi",
  "native-context-menu",
  "responsive-layout-persistence",
  "external-source-isolation",
  "release-no-debug-leakage-local-gate-disclosure",
]);

export const DESKTOP_E2E_ASSERTION_ANCHORS = Object.freeze({
  "packaged-launch": ["signed-package-identity-visible", "isolated-data-root-active"],
  "cef-react-render": ["cef-version-matches-release", "react-root-rendered"],
  "desktop-bridge": ["typed-bridge-authorized", "undeclared-command-rejected"],
  "engine-agent-handshake": ["engine-handshake-compatible", "agent-handshake-compatible"],
  "market-overview": ["desktop-market-values-visible", "desktop-market-quality-visible"],
  "stock-detail-data": ["desktop-security-identity-canonical", "desktop-stock-sections-complete"],
  "kline-orderbook": ["kline-bars-rendered", "orderbook-source-status-visible"],
  "fundamentals-valuation": ["fundamentals-periods-visible", "valuation-provenance-visible"],
  "news-pagination": ["news-page-bounded", "news-source-revisions-visible"],
  graph: ["graph-snapshot-rendered", "graph-edge-evidence-visible"],
  "quant-job": ["quant-job-progresses", "quant-snapshot-persists"],
  backtest: ["backtest-job-completes", "backtest-assumptions-visible"],
  "normal-agent-research": ["agent-research-completes", "agent-report-manual-only"],
  "multiple-tool-calls": ["multiple-tools-recorded", "tool-results-correlated"],
  "long-agent-task": ["long-task-checkpoints", "long-task-remains-responsive"],
  "conversation-history": ["desktop-history-persists", "desktop-history-reloads"],
  "conversation-branch": ["desktop-branch-origin-recorded", "desktop-branch-research-restarts"],
  "dynamic-clarification": ["desktop-model-question-visible", "desktop-agent-best-choice-available"],
  "task-resume": ["task-resumes-from-checkpoint", "resume-does-not-duplicate-effects"],
  "application-restart-task-resume": ["restart-restores-task", "restart-restores-transcript"],
  cancellation: ["cancel-clears-pending-tools", "cancel-remains-terminal"],
  "context-compaction": ["compaction-preserves-task-spec", "compaction-preserves-evidence-ledger"],
  "verification-failure": ["blocking-finding-prevents-publication", "verification-failure-visible"],
  "successful-verified-report": ["report-verifier-version-visible", "report-citations-resolve"],
  "evidence-navigation": ["desktop-evidence-reference-opens", "desktop-source-version-visible"],
  "provider-disconnect": ["provider-disconnect-visible", "provider-disconnect-recoverable"],
  "quota-suspension": ["quota-suspends-task", "quota-resume-preserves-checkpoint"],
  "agent-worker-recovery": ["agent-crash-detected", "agent-recovery-reconciles-effects"],
  "engine-worker-recovery": ["engine-crash-detected", "engine-recovery-preserves-task"],
  "renderer-crash-recovery": ["renderer-crash-detected", "renderer-recovery-bounded"],
  "window-drag": ["titlebar-drag-moves-window", "drag-does-not-white-screen"],
  "window-double-click-maximize": ["titlebar-double-click-maximizes", "second-double-click-restores"],
  "window-restore": ["window-restore-preserves-layout", "window-restore-preserves-task"],
  "window-edge-resize": ["edge-resize-changes-bounds", "edge-resize-keeps-content-readable"],
  "window-minimize": ["window-minimizes-to-taskbar", "window-restores-from-taskbar"],
  "taskbar-icon-high-dpi": ["taskbar-icon-is-branded", "high-dpi-icon-is-sharp"],
  "native-context-menu": ["text-context-menu-retained", "titlebar-system-menu-opens"],
  "responsive-layout-persistence": ["panel-ratios-persist", "responsive-breakpoints-recover"],
  "external-source-isolation": ["source-window-has-zero-app-permissions", "source-navigation-policy-enforced"],
  "release-no-debug-leakage-local-gate-disclosure": ["production-cdp-disabled", "local-gate-disclosure-visible"],
});

for (const [name, scenarios, expected] of [
  ["browser", BROWSER_CDP_SCENARIOS, 12],
  ["native-window", NATIVE_WINDOW_SCENARIOS, 8],
  ["desktop", DESKTOP_E2E_SCENARIOS, 40],
]) {
  if (scenarios.length !== expected || new Set(scenarios).size !== expected) {
    throw new Error(`${name} release scenario catalog must contain ${expected} unique cases`);
  }
}

for (const [name, scenarios, anchors] of [
  ["browser", BROWSER_CDP_SCENARIOS, BROWSER_CDP_ASSERTION_ANCHORS],
  ["desktop", DESKTOP_E2E_SCENARIOS, DESKTOP_E2E_ASSERTION_ANCHORS],
]) {
  const scenarioSet = new Set(scenarios);
  const anchorKeys = Object.keys(anchors);
  if (anchorKeys.length !== scenarios.length || anchorKeys.some((scenario) => !scenarioSet.has(scenario))) {
    throw new Error(`${name} assertion-anchor catalog must exactly match its scenario catalog`);
  }
  for (const scenario of scenarios) {
    const required = anchors[scenario];
    if (!Array.isArray(required) || required.length < 2 || new Set(required).size !== required.length ||
        required.some((id) => typeof id !== "string" || !id.trim())) {
      throw new Error(`${name} scenario ${scenario} must declare at least two unique assertion anchors`);
    }
  }
}

if (Object.keys(BROWSER_CDP_PROCEDURES).length !== BROWSER_CDP_SCENARIOS.length) {
  throw new Error("browser procedure catalog must exactly match the browser scenarios");
}
for (const scenario of BROWSER_CDP_SCENARIOS) {
  const procedure = BROWSER_CDP_PROCEDURES[scenario];
  const anchors = BROWSER_CDP_ASSERTION_ANCHORS[scenario];
  if (!procedure || !Array.isArray(procedure.actions) || procedure.actions.length < 2 ||
      procedure.actions.some((action) => typeof action !== "string" || action.length < 8) ||
      !procedure.viewport || !Number.isInteger(procedure.viewport.width) || !Number.isInteger(procedure.viewport.height) ||
      JSON.stringify(Object.keys(procedure.expected).sort()) !== JSON.stringify([...anchors].sort()) ||
      Object.values(procedure.expected).some((expected) => typeof expected !== "string" || expected.length < 20)) {
    throw new Error(`browser scenario ${scenario} has an incomplete executable procedure`);
  }
}
