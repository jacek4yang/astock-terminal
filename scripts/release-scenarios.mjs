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

function desktopProcedure(scenario, actions, expectedValues, viewport = { width: 1440, height: 900 }) {
  const anchors = DESKTOP_E2E_ASSERTION_ANCHORS[scenario];
  if (!anchors || expectedValues.length !== anchors.length) throw new Error(`desktop procedure ${scenario} does not match its assertion anchors`);
  return Object.freeze({
    viewport: Object.freeze({ ...viewport }),
    actions: Object.freeze([...actions]),
    expected: Object.freeze(Object.fromEntries(anchors.map((anchor, index) => [anchor, expectedValues[index]]))),
  });
}

export const DESKTOP_E2E_PROCEDURES = Object.freeze(Object.fromEntries([
  ["packaged-launch", desktopProcedure("packaged-launch",
    ["从隔离测试目录启动签名后的 AStock Terminal 安装包", "打开诊断页核对版本、提交和数据目录后退出并重新启动"],
    ["窗口与诊断页显示 AStock Terminal 6.0.0、正确产品身份和当前不可变提交", "行情、任务、缓存和日志全部写入本次验收隔离目录，未触碰生产数据目录"])],
  ["cef-react-render", desktopProcedure("cef-react-render",
    ["启动打包应用并等待首个 React 页面完成渲染", "打开诊断页读取 CEF 与 Chromium 版本并切换三个一级页面"],
    ["诊断页报告的 CEF 147.0.14 与锁定的 Chromium 147.0.7727.138 相符", "React 根节点完整渲染今日市场、Agent 智研和配置，切换后无白屏或残留页面"])],
  ["desktop-bridge", desktopProcedure("desktop-bridge",
    ["通过打包 renderer 调用声明过的诊断和市场 Bridge 服务", "从验收探针调用一个未声明命令并记录 Host 拒绝结果"],
    ["声明过的 typed Bridge 请求携带协议版本并得到与 request_id 对应的响应", "未声明命令在到达 Engine 或 Agent 前被 Host 明确拒绝且不泄露内部能力"])],
  ["engine-agent-handshake", desktopProcedure("engine-agent-handshake",
    ["冷启动应用并捕获 Host 与 Engine 的启动握手", "捕获 Host 与 Agent Worker 的握手、能力列表和心跳"],
    ["Engine 的应用、服务、协议版本和粗粒度能力与 6.0.0 合同兼容", "Agent 的 Provider、任务、恢复、工具和协议能力兼容且心跳进入 ready 状态"])],
  ["market-overview", desktopProcedure("market-overview",
    ["打开今日市场并刷新主要指数、涨跌分布和全市场列表", "断开一个次级来源后再次刷新并检查质量状态"],
    ["主要指数与证券列表显示真实 Engine 数值、来源时间和标准证券身份，缺失不补零", "来源失败时显示缺失、过期、冲突或降级状态，同时保留可验证的其他来源数据"])],
  ["stock-detail-data", desktopProcedure("stock-detail-data",
    ["从市场列表打开 300308 并核对代码、市场与标准名称", "遍历行情、盘口、图表、资金、基本面、估值、新闻、公告和证据区"],
    ["所有区域一致显示 300308 中际旭创及其规范市场身份，不被别名或脏名称覆盖", "个股关键区块均可访问；不可用字段带来源和质量说明而不是空白、零值或错位内容"])],
  ["kline-orderbook", desktopProcedure("kline-orderbook",
    ["在个股详情切换日线、周线、复权方式和技术指标", "打开五档盘口并在不支持盘口的市场检查降级显示"],
    ["K 线 OHLC、成交量、时区、交易日和复权标记完整渲染且缩放后保持一致", "盘口显示来源时间和单位；不支持时明确说明市场覆盖限制而不是伪造五档数据"])],
  ["fundamentals-valuation", desktopProcedure("fundamentals-valuation",
    ["打开基本面并切换至少三个报告期核对字段单位", "打开估值页检查估值日期、币种、口径、来源和可比样本"],
    ["财务指标按报告期有序显示，单位、币种、审计期和缺失状态可辨识且不串期", "每项估值展示数据截至时间、模型口径和来源版本，缺少输入时禁止输出确定性估值"])],
  ["news-pagination", desktopProcedure("news-pagination",
    ["打开个股资讯并连续翻页直到超过首个 500 行逻辑边界", "打开一个去重新闻簇并检查原文、发布时间与抓取时间"],
    ["每页不超过合同上限且使用稳定快照继续分页，不重复、跳行或把大结果塞进单帧", "新闻保留原文链接、来源修订、发布时间、抓取时间、实体和去重簇信息"])],
  ["graph", desktopProcedure("graph",
    ["从个股详情打开关系图并加载一个稳定快照", "选择一条关系边并打开其证据来源"],
    ["关系图按快照标识渲染节点与边，缩放和选择时不触发无关页面重载", "每条关键关系边可追溯到带来源版本和时间的证据，未知关系不被绘制为事实"])],
  ["quant-job", desktopProcedure("quant-job",
    ["创建一个使用固定数据截至时间的量化扫描作业", "切换页面后返回作业并打开持久化结果快照"],
    ["量化作业显示 queued、running 和终态进度，取消与错误状态可区分且 UI 保持响应", "结果绑定参数、证券范围、source_version_id 和快照标识，页面切换后仍能恢复"])],
  ["backtest", desktopProcedure("backtest",
    ["用固定区间和基准启动回测并等待确定性结果", "打开结果详情核对费用、滑点、复权、幸存者偏差和风险指标"],
    ["回测作业完成后结果与相同参数重放一致，并保留输入数据快照与计算标识", "报告显式展示交易费用、滑点、复权和数据边界，不将历史表现表述为未来保证"])],
  ["normal-agent-research", desktopProcedure("normal-agent-research",
    ["使用轮换后的 MiniMax 提交证据充分的标准股票研究任务", "等待多源工具、反方审查、证据校验和最终报告完成"],
    ["Agent 按计划经历取数、分析、复核、综合和验证阶段，任务可持久化恢复", "最终内容明确是人工审阅研究与投资计划，不出现账户连接、订单提交或自动交易能力"])],
  ["multiple-tool-calls", desktopProcedure("multiple-tool-calls",
    ["运行同时需要行情、基本面、资讯和量化工具的研究任务", "在工具活动中逐项核对请求摘要、状态、耗时、缓存和证据编号"],
    ["至少两个不同工具的开始、结果或失败事件按序持久化且可从任务历史重新载入", "每个工具结果与原 request_id、幂等键和任务步骤对应，重复响应不会形成第二次 Effect"])],
  ["long-agent-task", desktopProcedure("long-agent-task",
    ["启动包含多轮复核与压力情景的长时研究任务", "运行期间切换页面、滚动历史和调整面板后返回任务"],
    ["长任务按策略创建持久化检查点并可显示最近安全恢复位置，不丢失证据账本", "任务运行时输入、导航、滚动和状态刷新保持响应，事件批量渲染不阻塞无关图表"])],
  ["conversation-history", desktopProcedure("conversation-history",
    ["完成并重命名一个研究会话后切换页面并关闭应用", "重启后搜索历史并重新打开该会话"],
    ["会话标题、消息、TaskSpec、任务状态和证据引用由 Engine 持久化而非依赖 localStorage", "重启后的历史搜索返回同一会话，完整文本、工具活动和报告可重新加载"])],
  ["conversation-branch", desktopProcedure("conversation-branch",
    ["在一条历史用户消息或持久化检查点上选择从此开始研究", "修改研究截止时间并启动分支，随后比较原会话与新会话"],
    ["新会话记录父会话与来源消息或检查点，原会话保持只读不被覆盖", "分支建立新 task_id 并重新获取最新数据，旧报告仅作为待核验上下文而非事实缓存"])],
  ["dynamic-clarification", desktopProcedure("dynamic-clarification",
    ["提交缺少投资期限或风险边界的研究任务", "检查 MiniMax 动态问题后选择模型选项、自由输入或由 Agent 选择最优方案"],
    ["澄清问题与缺失 TaskSpec 字段相关并来自本轮 Provider 响应，不是前端预设问卷", "界面同时提供模型生成选项、自由输入和可审计的 Agent 最优选择入口"])],
  ["task-resume", desktopProcedure("task-resume",
    ["在工具结果持久化后暂停一个 Agent 任务", "从最近检查点恢复并等待下一个计划步骤"],
    ["恢复从持久化检查点继续且保留 TaskSpec、上下文摘要、工具和证据状态", "已完成的工具 Effect 通过幂等键重放或复用，不发生重复网络调用和双重完成"])],
  ["application-restart-task-resume", desktopProcedure("application-restart-task-resume",
    ["在 Agent 任务 AwaitingTools 或 Reviewing 阶段正常关闭应用", "重新启动应用并从历史打开运行中的任务执行恢复"],
    ["Host 重启 Worker 后识别未完成任务并恢复到最后安全检查点或明确 Suspended", "用户与 Agent 文本、问题答案、工具活动和证据记录在应用重启后保持完整"])],
  ["cancellation", desktopProcedure("cancellation",
    ["在存在待完成工具时取消 Agent 任务", "等待迟到工具结果并再次尝试恢复或发布任务"],
    ["取消事件清除或隔离全部 pending tools，迟到结果只入审计账本且不能推进 reducer", "Cancelled 保持吸收终态，恢复、重试或迟到响应不能产生报告或第二个终态"])],
  ["context-compaction", desktopProcedure("context-compaction",
    ["运行足够长的多轮研究以触发上下文压缩", "打开压缩前后检查点并核对任务规范与证据账本"],
    ["压缩后完整保留目标、证券范围、预算、数据截至时间、期限、基准和风险边界", "证据编号、工具结果、未决问题、校验发现和来源版本在压缩后仍可重放核对"])],
  ["verification-failure", desktopProcedure("verification-failure",
    ["注入单源重大数据冲突或缺少关键证据的研究结果", "让 Agent 进入发布校验并尝试打开最终报告"],
    ["阻断级校验发现阻止 Completed 与正式报告发布，不能被空值或模型措辞绕过", "任务明确显示 VerificationFailed、问题来源、缺失证据和可恢复操作而不是模糊失败"])],
  ["successful-verified-report", desktopProcedure("successful-verified-report",
    ["运行满足证据门槛的完整研究任务直到校验通过", "打开最终报告并逐项点击引用与校验摘要"],
    ["报告展示 verifier 版本、校验时间、可靠性分类和人工审阅边界且任务唯一完成", "关键结论的证据编号都能解析为来源、时间、版本和数据截至点一致的记录"])],
  ["evidence-navigation", desktopProcedure("evidence-navigation",
    ["从报告引用和工具活动各打开一个证据编号", "返回原位置后打开来源详情并检查版本和时间"],
    ["证据详情从两个入口均可打开，关闭后恢复原报告或工具滚动位置", "来源详情显示 source_version_id、原文链接、发布时间、抓取时间、证券实体和质量状态"])],
  ["provider-disconnect", desktopProcedure("provider-disconnect",
    ["在 MiniMax 流式响应中通过故障注入断开 Provider", "恢复连接后从已持久化 Provider 游标或安全检查点继续"],
    ["断流被显示为可区分的暂停或重试状态，已收到的 chunk 不被当作完成报告", "恢复使用有界重试并避免重复 chunk、工具 Effect 和消息，无法恢复时明确 Suspended"])],
  ["quota-suspension", desktopProcedure("quota-suspension",
    ["注入 Provider 额度耗尽响应并观察任务状态", "额度恢复后从配置页或任务页继续同一任务"],
    ["任务进入 Suspended 并显示真实额度状态、恢复条件和最近检查点，不静默切换模型", "额度恢复后沿同一 task_id 继续且不丢失证据、问题答案或重复已完成工具"])],
  ["agent-worker-recovery", desktopProcedure("agent-worker-recovery",
    ["在 Agent Worker 有 pending Effect 时终止该进程", "观察 Host 退避重启并等待任务账本 reconcile"],
    ["Host 在心跳阈值内检测 Agent 故障并显示重启次数、退避和最终状态", "Worker 恢复后按幂等键协调 pending 与 completed Effect，不重复发布或丢失任务"])],
  ["engine-worker-recovery", desktopProcedure("engine-worker-recovery",
    ["在 Engine 执行数据工具时终止该进程", "观察 Host 重启 Engine 并让 Agent 对未决工具结果进行协调"],
    ["Engine 故障被心跳监督检测，UI 显示数据能力不可用而不是返回伪造或缓存零值", "恢复后任务、快照和数据库保持一致，未决工具按幂等键重试或明确失败"])],
  ["renderer-crash-recovery", desktopProcedure("renderer-crash-recovery",
    ["通过验收故障注入触发 CEF renderer 崩溃", "等待 Host 受控恢复 renderer 并重新打开原任务"],
    ["Host 捕获 renderer 终止并提供有界恢复，不造成 Engine、Agent 或 Job Object 泄漏", "恢复后的页面从持久化状态加载任务和会话，不白屏、不重复提交输入且控制台干净"])],
  ["window-drag", desktopProcedure("window-drag",
    ["按住自绘标题栏空白区域连续拖动窗口跨越显示器区域", "拖动经过高频行情刷新和图表动画时观察 renderer 与窗口内容"],
    ["窗口跟随指针平滑移动，交互控件和文本选择区域不会误触发原生拖拽", "拖动期间和结束后 renderer 不出现白屏、黑块、尺寸跳变或输入卡死"])],
  ["window-double-click-maximize", desktopProcedure("window-double-click-maximize",
    ["双击自绘标题栏空白区域并记录窗口边界", "在最大化状态再次双击同一区域"],
    ["第一次双击通过原生窗口语义最大化并正确避开 Windows 工作区与任务栏", "第二次双击恢复到先前窗口边界，页面比例和面板状态保持不变"])],
  ["window-restore", desktopProcedure("window-restore",
    ["保存个股 62/38 分隔比例和 Agent 侧栏状态后最大化窗口", "恢复窗口并在三个一级页面间切换"],
    ["窗口恢复后自定义分隔比例、折叠状态和响应式断点正确复原且内容不重叠", "当前证券、Agent 会话、任务进度和输入草稿在最大化与恢复间保持完整"])],
  ["window-edge-resize", desktopProcedure("window-edge-resize",
    ["从窗口四边和四角逐步调整尺寸并跨越 1200 与 900 断点", "使用键盘调整个股和 Agent 分隔条后再次改变窗口尺寸"],
    ["每个原生边缘与角都能改变窗口边界，最小尺寸受到约束且没有拖拽白屏", "断点切换后文字、图表、表格和输入可读，键盘调整值被持久化并能恢复"])],
  ["window-minimize", desktopProcedure("window-minimize",
    ["在行情刷新和 Agent 任务运行时点击原生最小化按钮", "从 Windows 任务栏图标恢复窗口并检查后台进程状态"],
    ["窗口最小化到任务栏且不会隐藏为无入口后台窗口，Worker 生命周期保持受控", "从任务栏恢复后 renderer、行情和 Agent 任务状态正确，未重复启动 Host 或 Worker"])],
  ["taskbar-icon-high-dpi", desktopProcedure("taskbar-icon-high-dpi",
    ["在 100%、150% 和 200% 缩放下检查任务栏、Alt+Tab 和窗口图标", "固定与取消固定应用并重启后再次核对产品身份"],
    ["任务栏、Alt+Tab、窗口和安装包均使用趋势智研品牌图标而不是默认 CEF 或空白图标", "多档 DPI 下图标边缘清晰、尺寸正确且固定重启后仍关联同一应用身份"],
    { width: 1440, height: 900, device_scale_factor: 2 })],
  ["native-context-menu", desktopProcedure("native-context-menu",
    ["在 Agent 输入框和正文选择文本后打开右键菜单并执行复制粘贴", "在标题栏右键并操作 Windows 系统菜单的移动、大小和关闭入口"],
    ["可编辑文本保留撤销、剪切、复制、粘贴、删除和全选等原生语义且不泄露凭据", "标题栏系统菜单通过原生窗口命令工作，应用自定义菜单不会覆盖系统操作"])],
  ["responsive-layout-persistence", desktopProcedure("responsive-layout-persistence",
    ["将个股详情调为约 62/38 并设置 Agent 历史与证据栏宽度", "依次缩到 1199、899 再恢复 1440 宽并重启应用"],
    ["用户调整的合法面板比例按页面与工作区 schema 持久化，重启后在适用宽度恢复", "跨断点时侧栏变抽屉或标签，恢复宽屏后比例自然复原且无不可读横向溢出"])],
  ["external-source-isolation", desktopProcedure("external-source-isolation",
    ["从证据详情打开一个外部新闻或公告原文窗口", "尝试导航、弹窗、下载、媒体、文件和 Bridge 调用并记录策略结果"],
    ["外部来源窗口没有应用服务、进程、文件、数据库、凭据或 Worker 控制权限", "导航仅允许批准来源，弹窗下载证书媒体和未声明权限按零权限策略被拒绝并记录"])],
  ["release-no-debug-leakage-local-gate-disclosure", desktopProcedure("release-no-debug-leakage-local-gate-disclosure",
    ["检查正式包进程参数、监听端口、资源和诊断输出是否包含开发 Bridge 或 CDP", "打开版本与发布说明核对本地门禁和 GitHub Actions 状态披露"],
    ["生产包不启用远程调试、开发 Bridge、一次性 bootstrap 入口或任何测试令牌与调试菜单", "界面和发布元数据明确披露 GitHub Actions 未验证及本地门禁结果，不把计费失败写成 CI 通过"])],
]));

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

for (const [name, scenarios, anchors, procedures] of [
  ["browser", BROWSER_CDP_SCENARIOS, BROWSER_CDP_ASSERTION_ANCHORS, BROWSER_CDP_PROCEDURES],
  ["desktop", DESKTOP_E2E_SCENARIOS, DESKTOP_E2E_ASSERTION_ANCHORS, DESKTOP_E2E_PROCEDURES],
]) {
  if (Object.keys(procedures).length !== scenarios.length) {
    throw new Error(`${name} procedure catalog must exactly match its scenarios`);
  }
  for (const scenario of scenarios) {
    const procedure = procedures[scenario];
    if (!procedure || !Array.isArray(procedure.actions) || procedure.actions.length < 2 ||
        procedure.actions.some((action) => typeof action !== "string" || action.length < 8) ||
        !procedure.viewport || !Number.isInteger(procedure.viewport.width) || !Number.isInteger(procedure.viewport.height) ||
        JSON.stringify(Object.keys(procedure.expected).sort()) !== JSON.stringify([...anchors[scenario]].sort()) ||
        Object.values(procedure.expected).some((expected) => typeof expected !== "string" || expected.length < 20)) {
      throw new Error(`${name} scenario ${scenario} has an incomplete executable procedure`);
    }
  }
}
