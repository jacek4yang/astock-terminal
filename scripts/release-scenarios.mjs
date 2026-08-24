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

for (const [name, scenarios, expected] of [
  ["browser", BROWSER_CDP_SCENARIOS, 12],
  ["desktop", DESKTOP_E2E_SCENARIOS, 40],
]) {
  if (scenarios.length !== expected || new Set(scenarios).size !== expected) {
    throw new Error(`${name} release scenario catalog must contain ${expected} unique cases`);
  }
}
