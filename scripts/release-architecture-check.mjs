import fs from "node:fs";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");
const failures = [];
const cargo = fs.readFileSync(path.join(root, "Cargo.toml"), "utf8");
const ui = JSON.parse(fs.readFileSync(path.join(root, "ui", "package.json"), "utf8"));
const agentWorkbench = fs.readFileSync(path.join(root, "ui", "src", "workbench", "AgentTaskWorkbench.tsx"), "utf8");
const moonAgent = fs.readFileSync(path.join(root, "app-moon", "agent_worker", "main.mbt"), "utf8");
const moonProvider = fs.readFileSync(path.join(root, "app-moon", "agent_worker", "provider.mbt"), "utf8");
const moonKernel = fs.readFileSync(path.join(root, "app-moon", "agent_core", "state.mbt"), "utf8");
const moonHost = fs.readFileSync(path.join(root, "desktop-moon", "backend", "host", "backend.mbt"), "utf8");
const moonEntry = fs.readFileSync(path.join(root, "desktop-moon", "backend", "app", "main.mbt"), "utf8");
const engineSchema = fs.readFileSync(path.join(root, "protocol", "schema", "engine.schema.json"), "utf8");
const browserBridge = fs.readFileSync(path.join(root, "scripts", "browser-dev-bridge.mjs"), "utf8");
const acceptanceEvidence = fs.readFileSync(path.join(root, "scripts", "acceptance-evidence.mjs"), "utf8");
const releaseGate = fs.readFileSync(path.join(root, "scripts", "release-gate.ps1"), "utf8");
const releaseSigner = fs.readFileSync(path.join(root, "scripts", "sign-release.ps1"), "utf8");
const releaseEvidenceValidator = fs.readFileSync(path.join(root, "scripts", "release-evidence-check.mjs"), "utf8");
const packageHardener = fs.readFileSync(path.join(root, "scripts", "harden-package.ps1"), "utf8");
const migrationEvidence = fs.readFileSync(path.join(root, "scripts", "migration-e2e.ps1"), "utf8");
const faultEvidence = fs.readFileSync(path.join(root, "scripts", "fault-injection-e2e.ps1"), "utf8");
const externalEvidence = fs.readFileSync(path.join(root, "scripts", "external-services-e2e.ps1"), "utf8");
const credentialEvidence = fs.readFileSync(path.join(root, "scripts", "record-credential-rotation.ps1"), "utf8");
const performanceEvidence = fs.readFileSync(path.join(root, "scripts", "performance-e2e.ps1"), "utf8");
const performanceCdp = fs.readFileSync(path.join(root, "scripts", "performance-cdp.mjs"), "utf8");
const releasePublisher = fs.readFileSync(path.join(root, "scripts", "publish-v6.ps1"), "utf8");
const buildCommon = fs.readFileSync(path.join(root, "scripts", "Build.Common.ps1"), "utf8");
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
  "https://cli.moonbitlang.com/binaries/0.1.20260819/moonbit-linux-x86_64.tar.gz",
  "moon version | grep -F 'moon 0.1.20260819'",
  "moon test --target native",
  "cargo check --locked --workspace --all-targets --all-features",
]) {
  if (!qualityWorkflow.includes(requiredWorkflowMarker)) {
    failures.push(`quality workflow is missing v6 check: ${requiredWorkflowMarker}`);
  }
}
for (const [name, source] of activeRuntimeDocs) {
  for (const staleDocMarker of ["cargo check -p astock-app", "真实 Tauri 桌面进程", "稳定 Tauri 响应"]) {
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
for (const obsoleteCredentialWrapper of ["settings_set_provider_credentials", "jq_pwd", "ProviderCredentials"]) {
  if (agentWorkbench.includes(obsoleteCredentialWrapper) || fs.readFileSync(path.join(root, "ui", "src", "lib", "api.ts"), "utf8").includes(obsoleteCredentialWrapper)) {
    failures.push(`React still exposes the obsolete aggregate credential wrapper: ${obsoleteCredentialWrapper}`);
  }
}
for (const leakedTool of ["market.overview", "research.market_context", "research.market_candidates", "research.data_reconcile"]) {
  if (agentWorkbench.includes(`\"${leakedTool}\"`)) failures.push(`React Agent workbench still selects ${leakedTool}`);
}
if (!agentWorkbench.includes("agent.research.workflow")) failures.push("React does not submit the single Agent research workflow request");
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
if (!agentWorkbench.includes('agent.task.load') || !agentWorkbench.includes('recoverLatestCheckpoint')) {
  failures.push("React does not restore the newest durable Agent checkpoint before resuming");
}
if (!agentWorkbench.includes("persistDurableTransition") || !agentWorkbench.includes("completed.result")) {
  failures.push("React cannot backfill a missing checkpoint from an already completed durable Agent effect");
}
if (moonAgent.includes('"agent.research" =>') || moonAgent.includes('"agent.plan" =>')) {
  failures.push("MoonBit Agent still exposes renderer-supplied legacy research orchestration endpoints");
}
if (!moonHost.includes("execute_agent_effect") || !moonHost.includes('effect.target != "engine"')) {
  failures.push("MoonBit Host does not enforce the generic Engine-only Agent effect runner");
}
if (!moonHost.includes('effect.kind != "research.agent_prepare_context"') ||
    !moonHost.includes('effect.kind != "research.agent_security_context"') ||
    !moonHost.includes('effect.kind != "research.agent_report_verify"')) {
  failures.push("MoonBit Host cannot reconcile a persisted pending read-only research effect");
}
if (!browserBridge.includes("executeAgentEffect") || !browserBridge.includes('effect?.target !== "engine"')) {
  failures.push("browser test Bridge does not preserve the production Agent effect contract");
}
for (const acceptanceMarker of [
  "codex-in-app-browser",
  "packaged-proton-cef",
  "interaction-trace",
  "screenshot",
  "observation contains credential or Bridge-token material",
  "flag: \"wx\"",
]) {
  if (!acceptanceEvidence.includes(acceptanceMarker)) failures.push(`interactive acceptance evidence recorder is missing ${acceptanceMarker}`);
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
if (!releaseGate.includes("migration-e2e.ps1")) failures.push("release gate does not execute isolated migration E2E");
for (const faultMarker of ["fault-injection-core.mjs", "provider-stream-break", "sqlite-lock"]) {
  if (!faultEvidence.includes(faultMarker)) failures.push(`core fault harness is missing ${faultMarker}`);
}
for (const streamMarker of ['"stream": true', 'decode_sse_chunks', 'minimax_sse_incomplete_stream', 'retry_complete_text']) {
  if (!moonProvider.includes(streamMarker)) failures.push(`MoonBit Provider SSE recovery is missing ${streamMarker}`);
}
for (const externalMarker of ["credential-rotation.json", "research-live-smoke.mjs", "minimax-stream-resume", "joinquant-minimal-data", "secrets_in_evidence = $false"]) {
  if (!externalEvidence.includes(externalMarker)) failures.push(`external Provider evidence harness is missing ${externalMarker}`);
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
if (releaseGate.indexOf("browser-cdp-evidence") > releaseGate.indexOf("package-proton-cef")) {
  failures.push("release gate can launch the packaged desktop before Codex browser evidence passes");
}
for (const dependencyMarker of [
  "'package-proton-cef' 'package' 'INTEGRATION TESTED' -Requires @('browser-cdp-evidence') -Action",
  "'fault-injection-desktop-evidence' 'reliability' 'FAULT-INJECTION TESTED' -Requires @('browser-cdp-evidence','package-proton-cef','fault-injection-core') -Action",
  "'desktop-window-native-evidence' 'desktop' 'INTEGRATION TESTED' -Requires @('browser-cdp-evidence','package-proton-cef') -Action",
  "'desktop-e2e-evidence' 'desktop' 'INTEGRATION TESTED' -Requires @('browser-cdp-evidence','package-proton-cef','desktop-window-native-evidence') -Action",
  "'performance-evidence' 'performance' 'INTEGRATION TESTED' -Requires @('browser-cdp-evidence','package-proton-cef') -Action",
  "'authenticode' 'signing' 'ASSUMED/TRUSTED BOUNDARY' -Requires $productionSigningPrerequisites -Action",
  "'credential-rotation-evidence'",
  "'external-services-evidence'",
  "status = 'SKIPPED'",
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
