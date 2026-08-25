import fs from "node:fs";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");
const failures = [];

function quotedStrings(source) {
  return [...source.matchAll(/"([^"]+)"/g)].map((match) => match[1]);
}

function exactStringSet(label, actual, expected) {
  const normalizedActual = [...new Set(actual)].sort();
  const normalizedExpected = [...new Set(expected)].sort();
  if (JSON.stringify(normalizedActual) !== JSON.stringify(normalizedExpected)) {
    failures.push(`${label} drifted: expected ${normalizedExpected.join(", ")}; found ${normalizedActual.join(", ")}`);
  }
}

function requiredCapture(label, source, pattern) {
  const match = source.match(pattern);
  if (!match) {
    failures.push(`${label} cannot be parsed`);
    return "";
  }
  return match[1];
}

const cargo = fs.readFileSync(path.join(root, "Cargo.toml"), "utf8");
const ui = JSON.parse(fs.readFileSync(path.join(root, "ui", "package.json"), "utf8"));
const agentWorkbench = fs.readFileSync(path.join(root, "ui", "src", "workbench", "AgentTaskWorkbench.tsx"), "utf8");
const agentTaskService = fs.readFileSync(path.join(root, "ui", "src", "services", "agentTaskService.ts"), "utf8");
const agentClientSources = `${agentWorkbench}\n${agentTaskService}`;
const workspaceStore = fs.readFileSync(path.join(root, "ui", "src", "workbench", "store.ts"), "utf8");
const agentVisibleText = fs.readFileSync(path.join(root, "ui", "src", "workbench", "agentVisibleText.ts"), "utf8");
const moonAgent = fs.readFileSync(path.join(root, "app-moon", "agent_worker", "main.mbt"), "utf8");
const moonProvider = fs.readFileSync(path.join(root, "app-moon", "agent_worker", "provider.mbt"), "utf8");
const moonKernel = fs.readFileSync(path.join(root, "app-moon", "agent_core", "state.mbt"), "utf8");
const moonHost = fs.readFileSync(path.join(root, "desktop-moon", "backend", "host", "backend.mbt"), "utf8");
const moonWorkerClient = fs.readFileSync(path.join(root, "desktop-moon", "backend", "host", "worker_client.mbt"), "utf8");
const moonHostTests = fs.readFileSync(path.join(root, "desktop-moon", "backend", "host", "worker_supervision_wbtest.mbt"), "utf8");
const moonEntry = fs.readFileSync(path.join(root, "desktop-moon", "backend", "app", "main.mbt"), "utf8");
const engineSchema = fs.readFileSync(path.join(root, "protocol", "schema", "engine.schema.json"), "utf8");
const engineSchemaObject = JSON.parse(engineSchema);
const agentSchema = JSON.parse(fs.readFileSync(path.join(root, "protocol", "schema", "agent.schema.json"), "utf8"));
const hostSchema = JSON.parse(fs.readFileSync(path.join(root, "protocol", "schema", "host.schema.json"), "utf8"));
const browserBridge = fs.readFileSync(path.join(root, "scripts", "browser-dev-bridge.mjs"), "utf8");
const browserBridgeAuthSmoke = fs.readFileSync(path.join(root, "scripts", "browser-bridge-auth-smoke.ps1"), "utf8");
const browserAcceptancePreflight = fs.readFileSync(path.join(root, "scripts", "browser-acceptance-preflight.ps1"), "utf8");
const handshakeContract = fs.readFileSync(path.join(root, "scripts", "lib", "handshake-contract.mjs"), "utf8");
const rendererBridge = fs.readFileSync(path.join(root, "ui", "src", "bridge", "index.ts"), "utf8");
const acceptanceEvidence = fs.readFileSync(path.join(root, "scripts", "acceptance-evidence.mjs"), "utf8");
const releaseScenarios = fs.readFileSync(path.join(root, "scripts", "release-scenarios.mjs"), "utf8");
const researchDataGate = fs.readFileSync(path.join(root, "scripts", "research-data-release-gate.mjs"), "utf8");
const releaseGate = fs.readFileSync(path.join(root, "scripts", "release-gate.ps1"), "utf8");
const releaseSigner = fs.readFileSync(path.join(root, "scripts", "sign-release.ps1"), "utf8");
const releaseEvidenceValidator = fs.readFileSync(path.join(root, "scripts", "release-evidence-check.mjs"), "utf8");
const packageHardener = fs.readFileSync(path.join(root, "scripts", "harden-package.ps1"), "utf8");
const migrationEvidence = fs.readFileSync(path.join(root, "scripts", "migration-e2e.ps1"), "utf8");
const migrationEngineEvidence = fs.readFileSync(path.join(root, "scripts", "migration-engine-e2e.mjs"), "utf8");
const faultEvidence = fs.readFileSync(path.join(root, "scripts", "fault-injection-e2e.ps1"), "utf8");
const externalEvidence = fs.readFileSync(path.join(root, "scripts", "external-services-e2e.ps1"), "utf8");
const liveProviderRunner = fs.readFileSync(path.join(root, "scripts", "research-live-smoke.mjs"), "utf8");
const liveDataValidator = fs.readFileSync(path.join(root, "scripts", "lib", "live-data-validation.mjs"), "utf8");
const credentialEvidence = fs.readFileSync(path.join(root, "scripts", "record-credential-rotation.ps1"), "utf8");
const bootstrap = fs.readFileSync(path.join(root, "scripts", "bootstrap.ps1"), "utf8");
const performanceEvidence = fs.readFileSync(path.join(root, "scripts", "performance-e2e.ps1"), "utf8");
const performanceCdp = fs.readFileSync(path.join(root, "scripts", "performance-cdp.mjs"), "utf8");
const releasePublisher = fs.readFileSync(path.join(root, "scripts", "publish-v6.ps1"), "utf8");
const unsignedReleaseWorkflow = fs.readFileSync(path.join(root, ".github", "workflows", "release-unsigned.yml"), "utf8");
const unsignedReleaseStage = fs.readFileSync(path.join(root, "scripts", "stage-unsigned-release.ps1"), "utf8");
const moonbitCiBootstrap = fs.readFileSync(path.join(root, "scripts", "install-moonbit-ci.ps1"), "utf8");
const buildCommon = fs.readFileSync(path.join(root, "scripts", "Build.Common.ps1"), "utf8");
const moonFormalModel = fs.readFileSync(path.join(root, "app-moon", "agent_formal", "model.mbt"), "utf8");
const tlaLifecycleModel = fs.readFileSync(path.join(root, "formal", "AgentLifecycle.tla"), "utf8");
const tlaLifecycleConfig = fs.readFileSync(path.join(root, "formal", "AgentLifecycle.cfg"), "utf8");
const desktopCdpSession = fs.readFileSync(path.join(root, "scripts", "desktop-cdp-session.ps1"), "utf8");
const desktopRendererFault = fs.readFileSync(path.join(root, "scripts", "desktop-renderer-fault.mjs"), "utf8");
const desktopFaultEvidence = fs.readFileSync(path.join(root, "scripts", "fault-injection-desktop.ps1"), "utf8");
const desktopWindowProbe = fs.readFileSync(path.join(root, "scripts", "desktop-window-probe.ps1"), "utf8");
const desktopWindowEvidence = fs.readFileSync(path.join(root, "scripts", "desktop-window-e2e.ps1"), "utf8");
const rendererRecoveryPatch = fs.readFileSync(path.join(root, "patches", "proton-0.2.1-windows-renderer-recovery.patch"), "utf8");
const gpuPolicyPatch = fs.readFileSync(path.join(root, "patches", "proton-0.2.1-windows-gpu-policy.patch"), "utf8");
const qualityWorkflow = fs.readFileSync(path.join(root, ".github", "workflows", "quality.yml"), "utf8");
const engineCredentials = fs.readFileSync(path.join(root, "crates", "engine", "src", "credentials.rs"), "utf8");
const engineRuntime = fs.readFileSync(path.join(root, "crates", "engine", "src", "lib.rs"), "utf8");
const engineAgentContext = fs.readFileSync(path.join(root, "crates", "engine", "src", "agent_context.rs"), "utf8");
const engineEventStore = fs.readFileSync(path.join(root, "crates", "engine", "src", "event_store.rs"), "utf8");
const marketHub = fs.readFileSync(path.join(root, "crates", "market-data", "src", "hub.rs"), "utf8");
const marketHttp = fs.readFileSync(path.join(root, "crates", "market-data", "src", "http.rs"), "utf8");
const credentialRuntimeSources = [
  ["engine credentials", engineCredentials],
  ["market hub", marketHub],
  ["market proxy", fs.readFileSync(path.join(root, "crates", "market-data", "src", "proxy.rs"), "utf8")],
  ["Tushare provider", fs.readFileSync(path.join(root, "crates", "market-data", "src", "providers", "tushare.rs"), "utf8")],
  ["iWencai provider", fs.readFileSync(path.join(root, "crates", "market-data", "src", "providers", "iwencai_openapi.rs"), "utf8")],
  ["JoinQuant provider", fs.readFileSync(path.join(root, "crates", "market-data", "src", "providers", "joinquant.rs"), "utf8")],
  ["SEC provider", fs.readFileSync(path.join(root, "crates", "market-data", "src", "providers", "sec_edgar.rs"), "utf8")],
  ["global source catalog", fs.readFileSync(path.join(root, "crates", "global-intelligence", "src", "lib.rs"), "utf8")],
];
const activeRuntimeDocs = [
  "docs/agent-runtime-hardening.md",
  "docs/data-contracts.md",
  "docs/news-center.md",
  "docs/data-source-tushare.md",
  "docs/data-source-joinquant-v2.md",
].map((name) => [name, fs.readFileSync(path.join(root, ...name.split("/")), "utf8")]);

if (fs.existsSync(path.join(root, "src-tauri"))) failures.push("src-tauri differential oracle has not been removed");
if (/"src-tauri"/.test(cargo)) failures.push("Cargo workspace still contains src-tauri");
if (/"crates\/agent"/.test(cargo)) failures.push("Cargo workspace still contains the legacy Rust Agent");
if (fs.existsSync(path.join(root, "crates", "agent"))) failures.push("legacy Rust Agent sources have not been removed");
for (const section of ["dependencies", "devDependencies"]) {
  for (const name of Object.keys(ui[section] ?? {})) {
    if (name.startsWith("@tauri-apps/")) failures.push(`ui ${section} still contains ${name}`);
  }
}
if (ui.scripts?.tauri) failures.push("ui scripts still expose the obsolete Tauri entrypoint");
for (const stableAgentModel of [
  "task_spec",
  "clarification_request",
  "agent_question",
  "conversation_summary",
  "task_checkpoint",
  "tool_activity",
  "evidence_ref",
  "verification_finding",
  "provider_quota",
]) {
  if (!agentSchema.$defs?.[stableAgentModel]) failures.push(`Agent protocol schema is missing stable model: ${stableAgentModel}`);
}
const requiredAgentServiceMethods = ["task.create", "task.list", "task.get", "task.branch", "task.resume", "task.cancel", "task.answer"];
const declaredAgentServiceMethods = new Set((agentSchema.properties?.service_methods?.prefixItems ?? []).map((item) => item.const));
for (const method of requiredAgentServiceMethods) {
  if (!declaredAgentServiceMethods.has(method)) failures.push(`Agent service contract is missing method: ${method}`);
}
if (ui.dependencies?.["dockview-react"] || ui.devDependencies?.["dockview-react"]) {
  failures.push("v6 three-page renderer still depends on the retired Dockview IDE shell");
}
for (const legacyRendererPath of [
  "ui/src/agentSession.ts",
  "ui/src/components/AgentChat.tsx",
  "ui/src/components/Layout.tsx",
  "ui/src/pages/AgentPage.tsx",
  "ui/src/workbench/panelRegistry.tsx",
  "ui/src/workbench/presets.ts",
]) {
  if (fs.existsSync(path.join(root, ...legacyRendererPath.split("/")))) {
    failures.push(`retired v5 renderer path is still present: ${legacyRendererPath}`);
  }
}
if (!workspaceStore.includes('export type WorkbenchPreset = "market" | "agent" | "settings"')) {
  failures.push("renderer navigation is not closed to the three v6 primary pages");
}
if (!workspaceStore.includes('name: "astock-workspace-v6"') || workspaceStore.includes('name: "astock-workspace-v5"')) {
  failures.push("v6 renderer can accidentally adopt the retired workspace layout schema");
}
for (const [name, source] of credentialRuntimeSources) {
  for (const forbiddenSecretPath of [
    "std::env::set_var",
    "std::env::remove_var",
    "std::env::var(",
    "Provider::from_env",
    "ProxyConfig::from_env",
    "JoinQuantProvider::from_env",
  ]) {
    if (source.includes(forbiddenSecretPath)) {
      failures.push(`${name} still moves provider credentials through process environment: ${forbiddenSecretPath}`);
    }
  }
}
for (const credentialBoundaryMarker of [
  "load_market_credentials",
  "MarketDataCredentials",
  "MarketDataCredentials::new",
  "MarketData::with_storage_and_credentials",
]) {
  if (!engineCredentials.includes(credentialBoundaryMarker) && !engineRuntime.includes(credentialBoundaryMarker) && !marketHub.includes(credentialBoundaryMarker)) {
    failures.push(`Credential Manager to MarketData memory boundary is missing ${credentialBoundaryMarker}`);
  }
}
for (const publicSecretField of ["pub tushare_token", "pub iwencai_key", "pub sec_edgar_user_agent", "pub socks5"]) {
  if (marketHub.includes(publicSecretField)) failures.push(`MarketData credential bundle exposes secret field: ${publicSecretField}`);
}
for (const proxyLeakMarker of ["debug!(proxy = %url", "warn!(proxy = %url", "error!(proxy = %url"]) {
  if (marketHttp.includes(proxyLeakMarker)) failures.push(`SOCKS5 credential can leak through HTTP diagnostics: ${proxyLeakMarker}`);
}
if (!marketHttp.includes("pub fn proxy_configured(&self) -> bool") || marketHttp.includes("pub fn proxy_config(&self)")) {
  failures.push("HTTP diagnostics must expose only SOCKS5 configured state, never the credential-bearing proxy object");
}
for (const staleWorkflowMarker of [
  "Tauri",
  "cargo check -p astock-app",
  "rust-toolchain@1.88.0",
  "hustcer/setup-moonbit@",
]) {
  if (qualityWorkflow.includes(staleWorkflowMarker)) {
    failures.push(`quality workflow still contains obsolete v5 marker: ${staleWorkflowMarker}`);
  }
}
for (const requiredWorkflowMarker of [
  "ASTOCK_BUILD_ROOT",
  "protocol/codegen.mjs --check",
  "capability-parity-check.mjs --release",
  "release-architecture-check.mjs",
  "acceptance-evidence.test.mjs",
  "live-data-validation.test.mjs",
  "research-data-release-gate.test.mjs",
  "https://cli.moonbitlang.com/binaries/latest/moonbit-linux-x86_64.tar.gz",
  "b8f9273653f9af49c447775a7ecc7d20a2784849a15fe489a03afd6718c75d0d",
  "moon version | grep -F 'moon 0.1.20260824 (dae026a'",
  "moon update",
  "moon test --target native",
  "cargo check --locked --workspace --all-targets --all-features",
]) {
  if (!qualityWorkflow.includes(requiredWorkflowMarker)) {
    failures.push(`quality workflow is missing v6 check: ${requiredWorkflowMarker}`);
  }
}
for (const marker of [
  'gate: "public-research-data"',
  'credentialed_providers_tested: false',
  'assertIdentity(standard.data, "300308", "中际旭创")',
  'assertIdentity(beijing.data, "920001", "纬达光电")',
  'legacy.data.reconciliation?.blocking === true',
]) {
  if (!researchDataGate.includes(marker)) failures.push(`public research-data gate is missing ${marker}`);
}
for (const [name, source] of activeRuntimeDocs) {
  for (const staleDocMarker of [
    "cargo check -p astock-app",
    "真实 Tauri 桌面进程",
    "稳定 Tauri 响应",
    "`AgentEngine` 外层监督",
    "Tokio `OnceCell`",
  ]) {
    if (source.includes(staleDocMarker)) failures.push(`${name} still instructs the obsolete v5 runtime: ${staleDocMarker}`);
  }
  for (const unsafeCredentialInstruction of [
    "复用 storage `kv` 表存 token",
    "所有凭证只存本地(与 Tushare token 同模式)",
  ]) {
    if (source.includes(unsafeCredentialInstruction)) {
      failures.push(`${name} still recommends obsolete credential storage: ${unsafeCredentialInstruction}`);
    }
  }
}
if (agentWorkbench.includes("requestDurableTool")) failures.push("React Agent workbench still owns Engine tool execution");
for (const visibleTextMarker of ["stripPrivateReasoning", "sanitizeAgentVisibleText", "Bearer [已隐藏敏感信息]"]) {
  if (!agentVisibleText.includes(visibleTextMarker)) failures.push(`v6 Agent visible-text boundary is missing ${visibleTextMarker}`);
}
if (!agentWorkbench.includes("sanitizeAgentVisibleText(message.text)")) {
  failures.push("persisted Agent history bypasses the v6 visible-text publication boundary");
}
for (const durableRecoveryMarker of ["durableCheckpointState", "agent.task.load", "持久化检查点与任务不匹配", "setDurableTaskReady(false)"]) {
  if (!agentClientSources.includes(durableRecoveryMarker)) failures.push(`React Agent recovery is missing Engine-truth guard: ${durableRecoveryMarker}`);
}
if (agentWorkbench.includes("locally saved checkpoint") || /recoverLatestCheckpoint\([^)]*fallback/.test(agentWorkbench)) {
  failures.push("React Agent can still use a presentation-session checkpoint as executable task truth");
}
for (const obsoleteCredentialWrapper of ["settings_set_provider_credentials", "jq_pwd", "ProviderCredentials"]) {
  if (agentWorkbench.includes(obsoleteCredentialWrapper) || fs.readFileSync(path.join(root, "ui", "src", "lib", "api.ts"), "utf8").includes(obsoleteCredentialWrapper)) {
    failures.push(`React still exposes the obsolete aggregate credential wrapper: ${obsoleteCredentialWrapper}`);
  }
}
for (const leakedTool of ["market.overview", "research.market_context", "research.market_candidates", "research.data_reconcile"]) {
  if (agentWorkbench.includes(`\"${leakedTool}\"`)) failures.push(`React Agent workbench still selects ${leakedTool}`);
}
if (!agentClientSources.includes("agent.research.workflow")) failures.push("React does not submit the single Agent research workflow request");
if (!moonAgent.includes('"agent.research.workflow"') || !moonAgent.includes('"host_effects"')) {
  failures.push("MoonBit Agent does not own the effect-driven research workflow");
}
for (const recoveryMarker of ["ReconcileInterruptedWorkflow", "ResearchPlanSelected", "ProviderSuspended"]) {
  if (!moonKernel.includes(recoveryMarker)) failures.push(`MoonBit reducer is missing ${recoveryMarker}`);
}
for (const publicationGateMarker of [
  'tool == "research.agent_report_verify"',
  'contains(state.evidence_ids, "engine-report-verifier-v1")',
  'state.pending_tools.is_empty()',
]) {
  if (!moonKernel.includes(publicationGateMarker)) {
    failures.push(`MoonBit reducer publication gate is missing ${publicationGateMarker}`);
  }
}
if (!moonAgent.includes('payload.stringify()')) failures.push("Agent tool cache identity does not include the complete Engine payload");
for (const capability of ["advanced_tool_planning", "closed_engine_effects"]) {
  if (!moonAgent.includes(`"${capability}"`)) failures.push(`MoonBit Agent handshake is missing ${capability}`);
}
if (!engineRuntime.includes('"agent_advanced_analysis_v1"')) {
  failures.push("Rust Engine handshake is missing agent_advanced_analysis_v1");
}
if (!agentClientSources.includes('agent.task.load') || !agentWorkbench.includes('recoverLatestCheckpoint')) {
  failures.push("React does not restore the newest durable Agent checkpoint before resuming");
}
for (const bridgeBootstrapMarker of ["consumeBootstrapToken", 'request.url === "/session"', "bridgeBootstrapToken = null", "uiUrl.hash = bootstrapFragment.toString()"]) {
  if (!browserBridge.includes(bridgeBootstrapMarker)) failures.push(`browser development Bridge is missing one-time bootstrap protection: ${bridgeBootstrapMarker}`);
}
for (const rendererBootstrapMarker of ["initializeBrowserTestConfig", "cleanUrl.hash = \"\"", "window.history.replaceState", "window.sessionStorage.setItem", "await browserTestSession()"]) {
  if (!rendererBridge.includes(rendererBootstrapMarker)) failures.push(`renderer development Bridge is missing bootstrap consumption/scrubbing: ${rendererBootstrapMarker}`);
}
for (const browserAuthMarker of ["replayStatus -ne 401", "healthStatus -ne 200", "wrongOriginStatus -ne 401", "UseProxy = $false"]) {
  if (!browserBridgeAuthSmoke.includes(browserAuthMarker)) failures.push(`browser Bridge authorization smoke is missing ${browserAuthMarker}`);
}
for (const preflightMarker of [
  "CheckNetIsolation LoopbackExempt -s",
  "proxy_bypass_ready",
  "codex_process_ancestor",
  "browser_navigation_tested = $false",
  "secrets_in_evidence = $false",
  "browser-environment-preflight.json",
]) {
  if (!browserAcceptancePreflight.includes(preflightMarker)) {
    failures.push(`Codex browser environment preflight is missing ${preflightMarker}`);
  }
}
for (const preflightEvidenceMarker of [
  "validateBrowserEnvironmentPreflight",
  "browser-environment-preflight",
  "proxy_bypass_ready",
  "codex_process_ancestor",
]) {
  if (!acceptanceEvidence.includes(preflightEvidenceMarker)) {
    failures.push(`browser acceptance evidence is not bound to environment preflight: ${preflightEvidenceMarker}`);
  }
}
if (!releaseGate.includes("Invoke-ReleaseGateStep 'browser-bridge-auth' 'security' 'INTEGRATION TESTED'") ||
    !releaseGate.includes("'browser-bridge-auth',")) {
  failures.push("one-time browser Bridge authorization is not a mandatory signing prerequisite");
}
for (const proofIsolationMarker of ["proofRunId", 'validCount -ne [int]$proof.summary.valid', "incomplete or contaminated"]) {
  if (!releaseGate.includes(proofIsolationMarker)) failures.push(`formal release proof isolation is missing ${proofIsolationMarker}`);
}
for (const secretScanMarker of ["secret-history-scan", "gitleaks-history.json", "--log-opts=--all", "17157e2ee8b76fc8b1d8bee607a250e34b8a8023c8bc81822d4b5ee4d78fcb7c"]) {
  if (!releaseGate.includes(secretScanMarker)) failures.push(`local release secret-history scan is missing ${secretScanMarker}`);
}
for (const bootstrapSecretScanMarker of ['gitleaks_${version}_windows_x64.zip', "d29144deff3a68aa93ced33dddf84b7fdc26070add4aa0f4513094c8332afc4e", "gitleaks.exe"]) {
  if (!bootstrap.includes(bootstrapSecretScanMarker)) failures.push(`D-drive Gitleaks bootstrap is missing ${bootstrapSecretScanMarker}`);
}
for (const workflowSecretScanMarker of ["fetch-depth: 0", "gitleaks_${version}_linux_x64.tar.gz", "551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb", "--log-opts=--all"]) {
  if (!qualityWorkflow.includes(workflowSecretScanMarker)) failures.push(`quality workflow secret-history scan is missing ${workflowSecretScanMarker}`);
}
if (!engineEventStore.includes('format!("sha256:{:x}", Sha256::digest(value.as_bytes()))') ||
    !moonHost.includes("durable_operation_effect_matches") ||
    !moonHost.includes("identity.payload == Some(payload)") ||
    !browserBridge.includes("isDeepStrictEqual(item.effect?.payload")) {
  failures.push("Agent Effect identities are not parameter-complete SHA-256-backed structured matches");
}
for (const internalJournalKind of [
  "agent.task.create",
  "agent.event.append",
  "agent.checkpoint.put",
  "agent.effect.begin",
  "agent.effect.complete",
  "agent.effect.list",
]) {
  if (agentClientSources.includes(`"${internalJournalKind}"`)) {
    failures.push(`React Agent workbench can write internal journal kind ${internalJournalKind}`);
  }
}
if (moonAgent.includes('"agent.research" =>') || moonAgent.includes('"agent.plan" =>')) {
  failures.push("MoonBit Agent still exposes renderer-supplied legacy research orchestration endpoints");
}
if (!moonHost.includes("execute_agent_effect") || !moonHost.includes('effect.target != "engine"')) {
  failures.push("MoonBit Host does not enforce the generic Engine-only Agent effect runner");
}
if (!moonHost.includes("if !permitted_agent_effect_kind(effect.kind)") ||
    !moonHostTests.includes('permitted_agent_effect_kind("credentials.provider.delete")')) {
  failures.push("MoonBit Host does not deny non-research Agent Engine effects before execution");
}
if (!moonHost.includes('effect.kind != "research.agent_prepare_context"') ||
    !moonHost.includes('effect.kind != "research.agent_security_context"') ||
    !moonHost.includes('effect.kind != "research.agent_report_verify"')) {
  failures.push("MoonBit Host cannot reconcile a persisted pending read-only research effect");
}
if (!browserBridge.includes("executeAgentEffect") || !browserBridge.includes('effect?.target !== "engine"')) {
  failures.push("browser test Bridge does not preserve the production Agent effect contract");
}
if (!browserBridge.includes("const permittedKinds = new Set") ||
    !browserBridge.includes("if (!permittedKinds.has(effect.kind))")) {
  failures.push("browser test Bridge does not enforce the production bounded Agent Effect allowlist");
}
const agentRendererKinds = agentSchema.properties.renderer_request_kinds.prefixItems.map((item) => item.const);
const hostRendererKinds = hostSchema.properties.renderer_request_kinds.prefixItems.map((item) => item.const);
const engineRendererKinds = engineSchemaObject.properties.renderer_request_kinds.prefixItems.map((item) => item.const);
const engineWorkerKinds = engineSchemaObject.properties.request_kinds.prefixItems.map((item) => item.const);
if (engineRendererKinds.length !== 113 ||
    !engineRendererKinds.every((kind) => engineWorkerKinds.includes(kind))) {
  failures.push("Engine renderer request allowlist is not the audited 113-kind subset of the Worker contract");
}
for (const internalEngineKind of [
  "system.handshake",
  "system.shutdown",
  "system.cancel",
  "research.agent_prepare_context",
  "research.agent_security_context",
  "research.agent_report_verify",
  "research.market_context",
  "research.market_candidates",
  "research.joinquant_context",
  "research.optional_sources",
  "agent.task.create",
  "agent.event.append",
  "agent.checkpoint.put",
  "agent.effect.begin",
  "agent.effect.complete",
  "agent.effect.list",
]) {
  if (engineRendererKinds.includes(internalEngineKind)) {
    failures.push(`internal Engine kind is exposed to the renderer: ${internalEngineKind}`);
  }
}
exactStringSet(
  "Agent renderer request contract",
  agentRendererKinds,
  [
    "diagnostics.status",
    "agent.provider.test",
    "agent.provider.configure",
    "agent.start",
    "agent.event",
    "agent.research.workflow",
  ],
);
for (const internalAgentKind of ["agent.restore", "agent.task.snapshot", "agent.research.workflow.continue"]) {
  if (agentRendererKinds.includes(internalAgentKind)) {
    failures.push(`internal Agent kind is exposed to the renderer: ${internalAgentKind}`);
  }
}
exactStringSet(
  "Host renderer request contract",
  hostRendererKinds,
  [
    "diagnostics.status",
    "window.state",
    "window.minimize",
    "window.toggle_maximize",
    "window.begin_drag",
    "window.system_menu",
  ],
);
for (const marker of [
  "renderer_engine_request_kind(envelope.kind)",
  "renderer_agent_request_kind(envelope.kind)",
  "renderer_host_request_kind(envelope.kind)",
]) {
  if (!moonHost.includes(marker)) failures.push(`Proton Host renderer request denial is missing ${marker}`);
}
if (!rendererBridge.includes("ENGINE_RENDERER_REQUEST_KINDS") ||
    !rendererBridge.includes("isRendererRequestKind(target, kind)") ||
    !rendererBridge.includes("Renderer 请求未在")) {
  failures.push("React typed Bridge does not fail closed on undeclared target request kinds");
}
if (!browserBridge.includes("RENDERER_REQUEST_KINDS") ||
    !browserBridge.includes("outside the protocol contract")) {
  failures.push("browser test Bridge does not enforce the generated renderer request contracts");
}
const expectedEngineStartupCapabilities = [
  "market",
  "research",
  "data_quality",
  "agent_advanced_analysis_v1",
  "storage",
  "credentials",
  "agent_event_store_v2",
];
const expectedAgentStartupCapabilities = [
  "pure_reducer",
  "replay",
  "evidence_gate",
  "advanced_tool_planning",
  "closed_engine_effects",
  "deterministic_report_verification",
  "sse_stream_recovery",
];
if (engineSchemaObject.properties.service_version.const !== "6.0.0" ||
    agentSchema.properties.service_version.const !== "6.0.0") {
  failures.push("Engine and Agent startup service versions are not both schema-pinned to 6.0.0");
}
exactStringSet(
  "Engine startup capability contract",
  engineSchemaObject.properties.startup_required_capabilities.prefixItems.map((item) => item.const),
  expectedEngineStartupCapabilities,
);
exactStringSet(
  "Agent startup capability contract",
  agentSchema.properties.startup_required_capabilities.prefixItems.map((item) => item.const),
  expectedAgentStartupCapabilities,
);
for (const marker of [
  "validate_worker_handshake(\"engine\", engine_request_id, engine_reply)",
  "validate_worker_handshake(\"agent\", agent_request_id, agent_reply)",
]) {
  if (!moonHost.includes(marker)) failures.push(`Proton Host startup compatibility check is missing ${marker}`);
}
for (const marker of [
  "payload.engine_version == protocol_release_version",
  "payload.agent_version == protocol_release_version",
  "payload.max_frame_bytes == MAX_FRAME_BYTES",
  "require_capabilities(",
  "validate_worker_handshake(self.name, handshake_request_id, reply)",
]) {
  if (!moonWorkerClient.includes(marker)) failures.push(`Proton Host Worker compatibility check is missing ${marker}`);
}
if (!browserBridge.includes("validateHandshakeResponse(engineHandshake") ||
    !browserBridge.includes("validateHandshakeResponse(agentHandshake") ||
    !handshakeContract.includes("startup_required_capabilities")) {
  failures.push("browser test Bridge does not fail closed on incompatible Worker startup handshakes");
}
for (const marker of [
  "route_durable_agent",
  "durable_agent_request_kind(envelope.kind)",
  "agent_durability_lock.acquire()",
  '"agent.restore"',
  "loaded.task.checkpoint",
  '"agent.effect.begin"',
  '"agent.effect.complete"',
]) {
  if (!moonHost.includes(marker)) failures.push(`Proton Host durable Agent boundary is missing ${marker}`);
}
if (!browserBridge.includes("routeDurableAgent") ||
    !browserBridge.includes("DURABLE_AGENT_KINDS") ||
    !browserBridge.includes("durableAgentTail") ||
    !browserBridge.includes('"agent.effect.begin"') ||
    !browserBridge.includes('"agent.effect.complete"')) {
  failures.push("browser test Bridge does not preserve Host-owned Agent durability");
}
const expectedAgentEffectKinds = [
  "research.agent_prepare_context",
  "research.agent_security_context",
  "research.agent_report_verify",
];
exactStringSet(
  "MoonBit Host Agent Effect allowlist",
  quotedStrings(requiredCapture(
    "MoonBit Host Agent Effect allowlist",
    moonHost,
    /fn permitted_agent_effect_kind\([^)]*\)[^{]*\{([\s\S]*?)\n\}/,
  )),
  expectedAgentEffectKinds,
);
exactStringSet(
  "browser Bridge Agent Effect allowlist",
  quotedStrings(requiredCapture(
    "browser Bridge Agent Effect allowlist",
    browserBridge,
    /const permittedKinds = new Set\(\[([\s\S]*?)\]\);/,
  )),
  expectedAgentEffectKinds,
);
const expectedAnalysisModules = [
  "earnings_driver",
  "industry_graph",
  "relationship",
  "market_regime",
  "historical_backtest",
];
exactStringSet(
  "MoonBit Agent advanced-analysis allowlist",
  quotedStrings(requiredCapture(
    "MoonBit Agent advanced-analysis allowlist",
    moonProvider,
    /fn available_analysis_modules\([^)]*\)[^{]*\{([\s\S]*?)\n\}/,
  )),
  expectedAnalysisModules,
);
exactStringSet(
  "Rust Engine advanced-analysis allowlist",
  quotedStrings(requiredCapture(
    "Rust Engine advanced-analysis allowlist",
    engineAgentContext,
    /const ADVANCED_ANALYSIS_MODULES[^=]*=\s*\[([\s\S]*?)\];/,
  )),
  expectedAnalysisModules,
);
for (const analysisModule of expectedAnalysisModules) {
  if (!moonProvider.includes(`\"${analysisModule}\"`) || !engineAgentContext.includes(`\"${analysisModule}\"`)) {
    failures.push(`Agent advanced analysis module is not planned and executed end-to-end: ${analysisModule}`);
  }
}
for (const toolBoundaryMarker of [
  "analysis_modules_for_policy",
  "model_selected_unknown_analysis_module",
  "analysis_module_policy_is_closed_and_never_silently_escalates",
  '"quality_blocking": true',
  '"tool_activities": tool_activities',
]) {
  if (!moonProvider.includes(toolBoundaryMarker) && !engineAgentContext.includes(toolBoundaryMarker)) {
    failures.push(`Agent advanced tool boundary is missing ${toolBoundaryMarker}`);
  }
}
for (const acceptanceMarker of [
  "codex-in-app-browser",
  "packaged-proton-cef",
  "interaction-trace",
  "screenshot",
  "observation contains credential or Bridge-token material",
  "flag: \"wx\"",
  "required assertion anchor is missing",
]) {
  if (!acceptanceEvidence.includes(acceptanceMarker)) failures.push(`interactive acceptance evidence recorder is missing ${acceptanceMarker}`);
}
for (const scenarioContractMarker of [
  "BROWSER_CDP_ASSERTION_ANCHORS",
  "DESKTOP_E2E_ASSERTION_ANCHORS",
  "model-generated-question-visible",
  "blocking-finding-prevents-publication",
  "drag-does-not-white-screen",
  "source-window-has-zero-app-permissions",
]) {
  if (!releaseScenarios.includes(scenarioContractMarker)) failures.push(`interactive scenario contract is missing ${scenarioContractMarker}`);
}
for (const evidenceAssertionMarker of [
  "missing required assertion anchor",
  "unapproved interactive scenario",
]) {
  if (!releaseEvidenceValidator.includes(evidenceAssertionMarker)) failures.push(`interactive evidence validator is missing ${evidenceAssertionMarker}`);
}
for (const kind of ["research.agent_prepare_context", "research.agent_security_context", "research.agent_report_verify"]) {
  if (!engineSchema.includes(`\"${kind}\"`)) failures.push(`Engine protocol is missing ${kind}`);
}
for (const kind of ["storage.data_root.migrate", "storage.data_root.rollback"]) {
  if (!engineSchema.includes(`\"${kind}\"`)) failures.push(`Engine protocol is missing ${kind}`);
}
if (!releaseGate.includes("sign-release.ps1") || !releaseGate.includes("ASTOCK_RFC3161_TIMESTAMP_URL")) {
  failures.push("release gate does not execute the RFC3161 signing pipeline");
}
for (const signingMarker of [
  "!uninstfinalize",
  "/fd SHA256",
  "/tr $TimestampUrl",
  "/td SHA256",
  "SHA256SUMS",
  "signed-artifacts.json",
  "packaged_pe_count = $peFiles.Count",
  "pe_inventory = $peInventory",
  "release-evidence-check.mjs",
]) {
  if (!releaseSigner.includes(signingMarker)) failures.push(`release signing pipeline is missing ${signingMarker}`);
}
for (const publicationMarker of [
  "ConfirmProductionRelease",
  "Assert-AStockCleanWorktree",
  "visibility -ne 'PRIVATE'",
  "Authenticode is not Valid",
  "-FilePath 'git' -Arguments @('tag', '-s'",
  "$releaseArguments = @('release', 'create'",
  "GitHub Actions: NOT VERIFIED — billing/spending restriction; release gates executed locally",
  "Manifest target escapes its base directory",
  "Local and remote immutable tag objects differ",
  "$signedEvidence.pe_inventory",
  "Get-ChildItem -LiteralPath $appDirectory -Recurse -File",
  "AStock-Terminal-v6.0.0-verification-bundle.zip",
]) {
  if (!releasePublisher.includes(publicationMarker)) failures.push(`v6 publication guard is missing ${publicationMarker}`);
}
for (const unsignedPublicationMarker of [
  "ConfirmUnsignedAttestedRelease",
  "visibility -ne 'PUBLIC'",
  "Required GitHub quality checks have not passed",
  "@('tag', '-a'",
  "release-unsigned.yml",
]) {
  if (!releasePublisher.includes(unsignedPublicationMarker)) failures.push(`unsigned v6 publication guard is missing ${unsignedPublicationMarker}`);
}
for (const unsignedWorkflowMarker of [
  "id-token: write",
  "attestations: write",
  "actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6",
  "stage-unsigned-release.ps1",
  "package.ps1 -SkipSpaceCheck",
  "inputs.publish",
  "Upload dry-run package for inspection",
  "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02",
  "if: env.PUBLISH_RELEASE == 'true'",
  "gh release create",
  "Require successful quality checks for the tagged commit",
]) {
  if (!unsignedReleaseWorkflow.includes(unsignedWorkflowMarker)) failures.push(`unsigned release workflow is missing ${unsignedWorkflowMarker}`);
}
for (const unsignedStageMarker of [
  "github-oidc-attested-unsigned",
  "authenticode = 'NOT PROVIDED'",
  "AStock-Terminal-v6.0.0-SHA256SUMS.txt",
  "credentials_embedded = $false",
  "Get-AuthenticodeSignature",
]) {
  if (!unsignedReleaseStage.includes(unsignedStageMarker)) failures.push(`unsigned release staging is missing ${unsignedStageMarker}`);
}
for (const moonbitBootstrapMarker of [
  "0.1.20260824",
  "dae026a",
  "915a560cc4950a124bfedf5302ec6bf0d0f98d8ea6b2ae7978e4680641281963",
  "ca33c246472d02ce3805f8fc96b20e1819bf530f2fca7fe6610f5c9a601ee6eb",
  "MoonBit CI toolchain identity mismatch",
]) {
  if (!moonbitCiBootstrap.includes(moonbitBootstrapMarker)) failures.push(`MoonBit CI bootstrap is missing ${moonbitBootstrapMarker}`);
}
for (const reportIntegrityMarker of [
  "Get-ChildItem -LiteralPath $reportDirectory -Recurse -File",
  "GetRelativePath($reportDirectory, $file.FullName)",
]) {
  if (!releaseGate.includes(reportIntegrityMarker)) failures.push(`release report manifest is missing ${reportIntegrityMarker}`);
}
for (const evidenceMarker of [
  "packaged-app-pe-plus-installer",
  "duplicate PE inventory path",
  "PE inventory must contain every packaged PE plus the installer",
]) {
  if (!releaseEvidenceValidator.includes(evidenceMarker)) failures.push(`signed PE evidence validation is missing ${evidenceMarker}`);
}
for (const packageMarker of ["RequestExecutionLevel user", "$LOCALAPPDATA\\Programs\\AStock Terminal", "/RELEASETEST=", "HKCU", "makensis"]) {
  if (!packageHardener.includes(packageMarker)) failures.push(`package hardening is missing ${packageMarker}`);
}
for (const migrationMarker of ["migration-engine-e2e.mjs", "uninstall-preserves-data", "release-evidence-check.mjs"]) {
  if (!migrationEvidence.includes(migrationMarker)) failures.push(`migration release harness is missing ${migrationMarker}`);
}
for (const migrationSemanticMarker of ["validateMigrationEvidence", "migration-trace", "payload_matches", "marker_sha256_before", "touched_production_data = $false"]) {
  if (!`${releaseEvidenceValidator}\n${migrationEvidence}\n${migrationEngineEvidence}`.includes(migrationSemanticMarker)) {
    failures.push(`migration semantic evidence is missing ${migrationSemanticMarker}`);
  }
}
if (!releaseGate.includes("migration-e2e.ps1")) failures.push("release gate does not execute isolated migration E2E");
for (const faultMarker of ["fault-injection-core.mjs", "provider-stream-break", "sqlite-lock"]) {
  if (!faultEvidence.includes(faultMarker)) failures.push(`core fault harness is missing ${faultMarker}`);
}
for (const streamMarker of ['"stream": true', 'decode_sse_chunks', 'minimax_sse_incomplete_stream', 'retry_complete_text']) {
  if (!moonProvider.includes(streamMarker)) failures.push(`MoonBit Provider SSE recovery is missing ${streamMarker}`);
}
for (const externalMarker of ["credential-rotation.json", "research-live-smoke.mjs", "minimax-stream-resume", "joinquant-minimal-data", "data_sha256", "latest_lag_days", "pending_effects", "verifier_effect_status", "secrets_in_evidence = $false"]) {
  if (!externalEvidence.includes(externalMarker)) failures.push(`external Provider evidence harness is missing ${externalMarker}`);
}
for (const liveRunnerMarker of ["validateJoinQuantDaily", "joinquantAudit", "completed Agent response does not reconcile", "engine.research.agent_report_verify"]) {
  if (!liveProviderRunner.includes(liveRunnerMarker)) failures.push(`live Provider runner is missing ${liveRunnerMarker}`);
}
for (const liveDataMarker of ["invalid, duplicate or unordered date", "violates OHLC bounds", "latest bar is stale", "data_sha256"]) {
  if (!liveDataValidator.includes(liveDataMarker)) failures.push(`live Provider data validator is missing ${liveDataMarker}`);
}
for (const liveDataMarker of ["primary-source citations are insufficient", "snapshot is not bound to this live run", "audited security identity is invalid", "latest qfq bar is stale", "audited row digest is missing", "durable Effect ledger is incomplete", "durable verifier Effect is missing"]) {
  if (!releaseEvidenceValidator.includes(liveDataMarker)) failures.push(`external Provider evidence validator is missing ${liveDataMarker}`);
}
for (const browserProcedureMarker of ["BROWSER_CDP_PROCEDURES", "DESKTOP_E2E_PROCEDURES", "versioned procedure", "procedure.json", "changed the versioned expected value"]) {
  if (!`${releaseScenarios}\n${acceptanceEvidence}\n${releaseEvidenceValidator}`.includes(browserProcedureMarker)) {
    failures.push(`browser acceptance procedures are missing ${browserProcedureMarker}`);
  }
}
for (const credentialMarker of ["ConfirmOldCredentialsRevoked", "credential-readback-smoke.mjs", "credential_manager_readback_verified = $true", "secrets_in_evidence = $false"]) {
  if (!credentialEvidence.includes(credentialMarker)) failures.push(`credential rotation evidence harness is missing ${credentialMarker}`);
}
for (const [name, source] of [
  ["core fault", faultEvidence],
  ["desktop fault", desktopFaultEvidence],
  ["native window", desktopWindowEvidence],
  ["migration", migrationEvidence],
  ["external Provider", externalEvidence],
  ["credential rotation", credentialEvidence],
  ["performance", performanceEvidence],
  ["signing", releaseSigner],
]) {
  if (!source.includes("Assert-AStockCleanWorktree")) failures.push(`${name} evidence does not reject a dirty worktree`);
}
if (!releaseGate.includes("fault-injection-e2e.ps1")) failures.push("release gate does not execute core fault injection");
if (!moonEntry.includes('ASTOCK_RELEASE_TEST_CDP') || !moonEntry.includes('PROTON_HEADLESS')) {
  failures.push("packaged desktop entry does not expose fail-closed headless CDP release measurement");
}
if (!buildCommon.includes('proton-0.2.1-explicit-remote-debug.patch') || !buildCommon.includes('AStock production hardening: choosing a port must never turn CDP on')) {
  failures.push("Proton runtime does not require explicit application permission before enabling CDP");
}
if (!buildCommon.includes('proton-0.2.1-windows-renderer-recovery.patch') || !buildCommon.includes('AStock production hardening: CEF does not recreate a crashed renderer')) {
  failures.push("Proton runtime does not provide bounded Windows renderer crash recovery");
}
for (const recoveryMarker of ["renderer_recovery_count < 3", "> 60000", "reload_ignore_cache", "renderer_recovery_exhausted"]) {
  if (!rendererRecoveryPatch.includes(recoveryMarker)) failures.push(`renderer recovery patch is missing ${recoveryMarker}`);
}
if (!buildCommon.includes('proton-0.2.1-windows-gpu-policy.patch')) failures.push("Windows GPU policy patch is not applied during bootstrap");
for (const gpuMarker of ["PROTON_DISABLE_GPU", "AStock production GPU policy", "disable-gpu-compositing"]) {
  if (!gpuPolicyPatch.includes(gpuMarker)) failures.push(`Windows GPU policy patch is missing ${gpuMarker}`);
}
if (!desktopCdpSession.includes("PROTON_DISABLE_GPU")) failures.push("headless CEF acceptance does not explicitly exercise software GPU fallback");
for (const cdpMarker of ["ASTOCK_RELEASE_TEST_CDP", "PROTON_REMOTE_DEBUGGING_PORT", "desktop-cdp-smoke.mjs", "ProcessStartInfo", "WindowStyle"]) {
  if (!desktopCdpSession.includes(cdpMarker)) failures.push(`packaged desktop CDP harness is missing ${cdpMarker}`);
}
for (const faultMarker of ["Page.crash", "renderer_fault_injected", "host_restart_required", "Browser.close"]) {
  if (!desktopRendererFault.includes(faultMarker)) failures.push(`packaged renderer fault harness is missing ${faultMarker}`);
}
for (const faultMarker of ["renderer-kill", "gpu-failure", "PROTON_DISABLE_GPU=1", "fault-injection.json", "production_data_touched = $false"]) {
  if (!desktopFaultEvidence.includes(faultMarker)) failures.push(`desktop fault evidence harness is missing ${faultMarker}`);
}
for (const probeMarker of ["ExpectedExecutablePath", "Refusing to control an unrelated process", "AllowInteractiveInput", "GetDpiForWindow", "HasLargeIcon", "TaskbarEligible"]) {
  if (!desktopWindowProbe.includes(probeMarker)) failures.push(`native desktop window probe is missing ${probeMarker}`);
}
for (const windowMarker of ["window-drag", "window-double-click-maximize", "window-edge-resize", "native-context-menu", "production_data_touched = $false", "desktop-window-native.json"]) {
  if (!desktopWindowEvidence.includes(windowMarker)) failures.push(`native desktop window evidence harness is missing ${windowMarker}`);
}
for (const nativeEvidenceMarker of ["validateNativeWindowEvidence", "win32-window-trace", "does not prove a bounded move", "process_alive"]) {
  if (!`${releaseEvidenceValidator}\n${desktopWindowEvidence}`.includes(nativeEvidenceMarker)) {
    failures.push(`native desktop window semantic evidence is missing ${nativeEvidenceMarker}`);
  }
}
if (releaseGate.indexOf("browser-cdp-evidence") > releaseGate.indexOf("package-proton-cef")) {
  failures.push("release gate can launch the packaged desktop before Codex browser evidence passes");
}
if (releaseGate.indexOf("credential-rotation-evidence") > releaseGate.indexOf("browser-cdp-evidence") ||
    !releaseGate.includes("'browser-cdp-evidence' 'renderer' 'INTEGRATION TESTED' -Requires @('credential-rotation-evidence') -Action") ||
    !releaseGate.includes("Browser acceptance predates credential rotation")) {
  failures.push("real-Provider browser acceptance is not bound to post-rotation credentials");
}
for (const dependencyMarker of [
  "'package-proton-cef' 'package' 'INTEGRATION TESTED' -Requires @('browser-cdp-evidence') -Action",
  "'external-services-evidence' 'providers' 'ASSUMED/TRUSTED BOUNDARY' -Requires @('credential-rotation-evidence') -Action",
  "'fault-injection-desktop-evidence' 'reliability' 'FAULT-INJECTION TESTED' -Requires @('browser-cdp-evidence','package-proton-cef','fault-injection-core') -Action",
  "'desktop-window-native-evidence' 'desktop' 'INTEGRATION TESTED' -Requires @('browser-cdp-evidence','package-proton-cef') -Action",
  "'desktop-e2e-evidence' 'desktop' 'INTEGRATION TESTED' -Requires @('browser-cdp-evidence','package-proton-cef','desktop-window-native-evidence') -Action",
  "'performance-evidence' 'performance' 'INTEGRATION TESTED' -Requires @('browser-cdp-evidence','package-proton-cef') -Action",
  "'authenticode' 'signing' 'ASSUMED/TRUSTED BOUNDARY' -Requires $productionSigningPrerequisites -Action",
  "'credential-rotation-evidence'",
  "'external-services-evidence'",
  "status = 'SKIPPED'",
  "ASTOCK_BROWSER_ACCEPTANCE_SESSION",
  "ASTOCK_DESKTOP_ACCEPTANCE_SESSION",
  "Complete-InteractiveEvidence -SessionDirectory $BrowserAcceptanceSession",
  "Complete-InteractiveEvidence -SessionDirectory $DesktopAcceptanceSession",
  "'public-research-data' 'data' 'INTEGRATION TESTED'",
  "research-data-release-gate.mjs",
]) {
  if (!releaseGate.includes(dependencyMarker)) failures.push(`release gate dependency control is missing ${dependencyMarker}`);
}
for (const marker of [
  "browser-cdp.json",
  "Assert-AStockCleanWorktree",
  "performance-cdp.mjs",
  "backend/skeleton",
  "logical_rows = [int]$raw.assertions.logical_rows",
  "application_package_sha256",
  "proton_skeleton_source_sha256",
  "release-evidence-check.mjs",
]) {
  if (!performanceEvidence.includes(marker)) failures.push(`packaged performance harness is missing ${marker}`);
}
for (const marker of [
  "100_000",
  "workspaceRestoreSamples",
  "commandFeedbackSamples",
  "scrollFpsSamples",
  "agentRenderSamples",
  "processTreeSamples",
  "skeleton_cold_start_ms",
]) {
  if (!performanceCdp.includes(marker)) failures.push(`packaged performance CDP runner is missing ${marker}`);
}

for (const proofFunction of [
  "accept_sequence",
  "next_bounded_round",
  "consume_pending",
  "pending_correspondence",
  "complete_once",
  "unique_result_count",
  "cancel_pending",
  "publication_allowed",
  "event_accepted",
  "replay_deterministic",
  "reconcile_idempotent",
  "compression_structure_complete",
]) {
  if (!moonFormalModel.includes(`pub fn ${proofFunction}(`)) {
    failures.push(`MoonBit formal model is missing named obligation: ${proofFunction}`);
  }
}
for (const temporalMarker of [
  "MaxCrashes",
  "NeedClarification ==",
  "Crash ==",
  "Restart ==",
  "ProgressSpec ==",
  "WF_vars(ForwardStep)",
  "WF_vars(Restart)",
  "SeqMonotonic ==",
  "TerminalAbsorbing ==",
  "FiniteRangeLiveness ==",
]) {
  if (!tlaLifecycleModel.includes(temporalMarker)) {
    failures.push(`TLA+ Agent lifecycle is missing bounded recovery/liveness marker: ${temporalMarker}`);
  }
}
for (const configMarker of [
  "SPECIFICATION ProgressSpec",
  "MaxCrashes = 2",
  "PROPERTIES",
  "SeqMonotonic",
  "TerminalAbsorbing",
  "FiniteRangeLiveness",
]) {
  if (!tlaLifecycleConfig.includes(configMarker)) {
    failures.push(`TLC release configuration is missing temporal check: ${configMarker}`);
  }
}

const moon = fs.readFileSync(path.join(root, "desktop-moon", "backend", "moon.mod"), "utf8");
for (const dependency of [
  "moonbit-community/proton@0.2.1",
  "moonbit-community/proton_contract@0.2.1",
]) {
  if (!moon.includes(dependency)) failures.push(`desktop Host is not pinned to ${dependency}`);
}

console.log(JSON.stringify({
  ok: failures.length === 0,
  desktop_entry: "Proton 0.2.1 + CEF",
  legacy_tauri_present: fs.existsSync(path.join(root, "src-tauri")),
  legacy_rust_agent_present: fs.existsSync(path.join(root, "crates", "agent")),
  renderer_agent_tool_orchestration_present: agentWorkbench.includes("requestDurableTool"),
  moonbit_agent_effect_workflow_present: moonAgent.includes('"host_effects"'),
  failures,
}, null, 2));
if (failures.length) process.exitCode = 1;
