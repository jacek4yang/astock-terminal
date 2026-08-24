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
const engineSchema = fs.readFileSync(path.join(root, "protocol", "schema", "engine.schema.json"), "utf8");
const browserBridge = fs.readFileSync(path.join(root, "scripts", "browser-dev-bridge.mjs"), "utf8");
const releaseGate = fs.readFileSync(path.join(root, "scripts", "release-gate.ps1"), "utf8");
const releaseSigner = fs.readFileSync(path.join(root, "scripts", "sign-release.ps1"), "utf8");

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
if (!releaseGate.includes("sign-release.ps1") || !releaseGate.includes("ASTOCK_RFC3161_TIMESTAMP_URL")) {
  failures.push("release gate does not execute the RFC3161 signing pipeline");
}
for (const signingMarker of ["!uninstfinalize", "/fd SHA256", "/tr $TimestampUrl", "/td SHA256", "SHA256SUMS", "signed-artifacts.json"]) {
  if (!releaseSigner.includes(signingMarker)) failures.push(`release signing pipeline is missing ${signingMarker}`);
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
