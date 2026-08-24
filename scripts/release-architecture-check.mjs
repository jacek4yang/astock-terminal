import fs from "node:fs";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");
const failures = [];
const cargo = fs.readFileSync(path.join(root, "Cargo.toml"), "utf8");
const ui = JSON.parse(fs.readFileSync(path.join(root, "ui", "package.json"), "utf8"));
const agentWorkbench = fs.readFileSync(path.join(root, "ui", "src", "workbench", "AgentTaskWorkbench.tsx"), "utf8");
const moonAgent = fs.readFileSync(path.join(root, "app-moon", "agent_worker", "main.mbt"), "utf8");
const moonKernel = fs.readFileSync(path.join(root, "app-moon", "agent_core", "state.mbt"), "utf8");
const moonHost = fs.readFileSync(path.join(root, "desktop-moon", "backend", "host", "backend.mbt"), "utf8");
const moonEntry = fs.readFileSync(path.join(root, "desktop-moon", "backend", "app", "main.mbt"), "utf8");
const engineSchema = fs.readFileSync(path.join(root, "protocol", "schema", "engine.schema.json"), "utf8");
const browserBridge = fs.readFileSync(path.join(root, "scripts", "browser-dev-bridge.mjs"), "utf8");
const releaseGate = fs.readFileSync(path.join(root, "scripts", "release-gate.ps1"), "utf8");
const releaseSigner = fs.readFileSync(path.join(root, "scripts", "sign-release.ps1"), "utf8");
const packageHardener = fs.readFileSync(path.join(root, "scripts", "harden-package.ps1"), "utf8");
const migrationEvidence = fs.readFileSync(path.join(root, "scripts", "migration-e2e.ps1"), "utf8");
const faultEvidence = fs.readFileSync(path.join(root, "scripts", "fault-injection-e2e.ps1"), "utf8");
const buildCommon = fs.readFileSync(path.join(root, "scripts", "Build.Common.ps1"), "utf8");
const desktopCdpSession = fs.readFileSync(path.join(root, "scripts", "desktop-cdp-session.ps1"), "utf8");
const desktopRendererFault = fs.readFileSync(path.join(root, "scripts", "desktop-renderer-fault.mjs"), "utf8");
const desktopFaultEvidence = fs.readFileSync(path.join(root, "scripts", "fault-injection-desktop.ps1"), "utf8");
const rendererRecoveryPatch = fs.readFileSync(path.join(root, "patches", "proton-0.2.1-windows-renderer-recovery.patch"), "utf8");
const gpuPolicyPatch = fs.readFileSync(path.join(root, "patches", "proton-0.2.1-windows-gpu-policy.patch"), "utf8");

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
if (agentWorkbench.includes("requestDurableTool")) failures.push("React Agent workbench still owns Engine tool execution");
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
if (!moonAgent.includes('payload.stringify()')) failures.push("Agent tool cache identity does not include the complete Engine payload");
if (!agentWorkbench.includes('agent.task.load') || !agentWorkbench.includes('recoverLatestCheckpoint')) {
  failures.push("React does not restore the newest durable Agent checkpoint before resuming");
}
if (moonAgent.includes('"agent.research" =>') || moonAgent.includes('"agent.plan" =>')) {
  failures.push("MoonBit Agent still exposes renderer-supplied legacy research orchestration endpoints");
}
if (!moonHost.includes("execute_agent_effect") || !moonHost.includes('effect.target != "engine"')) {
  failures.push("MoonBit Host does not enforce the generic Engine-only Agent effect runner");
}
if (!moonHost.includes('effect.kind != "research.agent_prepare_context"') || !moonHost.includes('effect.kind != "research.agent_security_context"')) {
  failures.push("MoonBit Host cannot reconcile a persisted pending read-only research effect");
}
if (!browserBridge.includes("executeAgentEffect") || !browserBridge.includes('effect?.target !== "engine"')) {
  failures.push("browser test Bridge does not preserve the production Agent effect contract");
}
for (const kind of ["research.agent_prepare_context", "research.agent_security_context"]) {
  if (!engineSchema.includes(`\"${kind}\"`)) failures.push(`Engine protocol is missing ${kind}`);
}
for (const kind of ["storage.data_root.migrate", "storage.data_root.rollback"]) {
  if (!engineSchema.includes(`\"${kind}\"`)) failures.push(`Engine protocol is missing ${kind}`);
}
if (!releaseGate.includes("sign-release.ps1") || !releaseGate.includes("ASTOCK_RFC3161_TIMESTAMP_URL")) {
  failures.push("release gate does not execute the RFC3161 signing pipeline");
}
for (const signingMarker of ["!uninstfinalize", "/fd SHA256", "/tr $TimestampUrl", "/td SHA256", "SHA256SUMS", "signed-artifacts.json"]) {
  if (!releaseSigner.includes(signingMarker)) failures.push(`release signing pipeline is missing ${signingMarker}`);
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
if (releaseGate.indexOf("browser-cdp-evidence") > releaseGate.indexOf("package-proton-cef")) {
  failures.push("release gate can launch the packaged desktop before Codex browser evidence passes");
}
for (const dependencyMarker of [
  "'package-proton-cef' 'package' 'INTEGRATION TESTED' -Requires @('browser-cdp-evidence') -Action",
  "'fault-injection-desktop-evidence' 'reliability' 'FAULT-INJECTION TESTED' -Requires @('browser-cdp-evidence','package-proton-cef','fault-injection-core') -Action",
  "'authenticode' 'signing' 'ASSUMED/TRUSTED BOUNDARY' -Requires $productionSigningPrerequisites -Action",
  "'credential-rotation-evidence'",
  "'external-services-evidence'",
  "status = 'SKIPPED'",
]) {
  if (!releaseGate.includes(dependencyMarker)) failures.push(`release gate dependency control is missing ${dependencyMarker}`);
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
