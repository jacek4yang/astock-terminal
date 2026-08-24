import fs from "node:fs";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");
const failures = [];
const cargo = fs.readFileSync(path.join(root, "Cargo.toml"), "utf8");
const ui = JSON.parse(fs.readFileSync(path.join(root, "ui", "package.json"), "utf8"));

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
  failures,
}, null, 2));
if (failures.length) process.exitCode = 1;
