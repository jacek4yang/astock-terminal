import fs from "node:fs";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");
const expected = process.argv[2] ?? "6.0.0";
const errors = [];

function read(relative) {
  return fs.readFileSync(path.join(root, relative), "utf8");
}

function requireMatch(relative, expression, description) {
  const source = read(relative);
  const match = source.match(expression);
  if (!match || match[1] !== expected) {
    errors.push(`${relative}: ${description}; actual=${match?.[1] ?? "missing"}, expected=${expected}`);
  }
}

for (const file of ["ui/package.json", "ui/package-lock.json"]) {
  const value = JSON.parse(read(file));
  if (value.version !== expected) errors.push(`${file}: version=${value.version}`);
  if (file.endsWith("package-lock.json") && value.packages?.[""]?.version !== expected) {
    errors.push(`${file}: root package version=${value.packages?.[""]?.version}`);
  }
}

requireMatch("Cargo.toml", /\[workspace\.package\][\s\S]*?version\s*=\s*"([^"]+)"/, "workspace version mismatch");
for (const file of [
  "app-moon/moon.mod",
  "desktop-moon/backend/moon.mod",
  "desktop-moon/shared/moon.mod",
  "packaging-moon/moon.mod",
]) {
  requireMatch(file, /\bversion\s*=\s*"([^"]+)"/, "MoonBit version mismatch");
}

const proton = JSON.parse(read("desktop-moon/proton.project.json"));
if (proton.package?.version !== expected) errors.push(`desktop-moon/proton.project.json: version=${proton.package?.version}`);
requireMatch("packaging-moon/packager/main.mbt", /PackageSpec::new\([\s\S]*?"(\d+\.\d+\.\d+)",\s*\[App/, "packager version mismatch");
requireMatch("desktop-moon/backend/host/backend.mbt", /"app_version"\s*:\s*"([^"]+)"/, "Host diagnostic version mismatch");

const envelope = JSON.parse(read("protocol/schema/envelope.schema.json"));
const protocolVersion = envelope?.$defs?.request?.properties?.protocol_version?.const;
if (protocolVersion !== 1) errors.push(`protocol version changed unexpectedly: ${protocolVersion}`);

console.log(JSON.stringify({
  ok: errors.length === 0,
  application_version: expected,
  protocol_version: protocolVersion,
  errors,
}, null, 2));
if (errors.length) process.exitCode = 1;
