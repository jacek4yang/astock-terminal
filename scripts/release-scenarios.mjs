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
