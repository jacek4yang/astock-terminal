import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { FramedWorker, handshake } from "./lib/framed-worker.mjs";

const [engineExecutable, testRoot] = process.argv.slice(2);
if (!engineExecutable || !testRoot || !path.isAbsolute(testRoot)) {
  throw new Error("usage: node scripts/migration-engine-e2e.mjs <engine.exe> <absolute-test-root>");
}

const source = path.join(testRoot, "source");
const destination = path.join(testRoot, "destination");
const profile = path.join(testRoot, "profile");
const localAppData = path.join(profile, "Local");
const appData = path.join(profile, "Roaming");
const parquetRelative = "timeseries/600519/day/qfq.parquet";
const parquetBody = Buffer.from("AStock migration release evidence parquet fixture\n", "utf8");
const conversationId = "migration-evidence-conversation";
const cases = [];

function record(id, started, details) {
  cases.push({ id, status: "PASSED", duration_ms: Date.now() - started, details });
}

function worker(options = {}) {
  return new FramedWorker(engineExecutable, {
    name: "migration-engine",
    env: { LOCALAPPDATA: localAppData, APPDATA: appData, ...options.env },
    unsetEnv: options.unsetEnv ?? [],
  });
}

function session(id, marker) {
  return {
    sessionId: id,
    title: marker,
    messages: [{ id: `${id}-message`, role: "user", content: marker, createdAt: Date.now() }],
    task: { phase: "completed", evidence_ids: [marker] },
  };
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

async function saveConversation(engine, id, marker) {
  return engine.request("agent.conversation.save", {
    conversation_id: id,
    title: marker,
    session: session(id, marker),
  });
}

async function loadConversation(engine, id, marker) {
  const loaded = await engine.request("agent.conversation.load", { conversation_id: id });
  assert(loaded.session?.messages?.[0]?.content === marker, `conversation ${id} was not preserved`);
}

fs.mkdirSync(path.dirname(path.join(source, parquetRelative)), { recursive: true });
fs.mkdirSync(localAppData, { recursive: true });
fs.mkdirSync(appData, { recursive: true });
fs.writeFileSync(path.join(source, parquetRelative), parquetBody);

let engine = worker({ env: { ASTOCK_DATA_DIR: source } });
try {
  await handshake(engine, "migration-source");
  await saveConversation(engine, conversationId, "source-preserved");
  const started = Date.now();
  const outcome = await engine.request("storage.data_root.migrate", { destination }, { deadlineMs: 120_000 });
  assert(outcome.sqlite_integrity === "ok", "migration did not verify SQLite");
  assert(outcome.source_retained === true, "migration did not retain its source");
  assert(outcome.restart_required === true, "migration must activate only after restart");
  record("d-drive-migration", started, {
    source,
    destination,
    sqlite_integrity: outcome.sqlite_integrity,
    source_retained: outcome.source_retained,
    restart_required: outcome.restart_required,
  });
} finally {
  await engine.shutdown();
}

{
  const started = Date.now();
  const manifestPath = path.join(destination, "migration-manifest.json");
  const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
  assert(manifest.sqlite_integrity === "ok", "manifest SQLite integrity is not ok");
  assert(manifest.source_retained === true, "manifest does not attest source retention");
  assert(fs.existsSync(path.join(source, "meta.db")), "source database was removed");
  assert(fs.existsSync(path.join(destination, "meta.db")), "destination database is missing");
  record("sqlite-integrity", started, {
    manifest_sha256: crypto.createHash("sha256").update(fs.readFileSync(manifestPath)).digest("hex"),
    sqlite_integrity: manifest.sqlite_integrity,
    source_retained: manifest.source_retained,
    source_meta_sha256: crypto.createHash("sha256").update(fs.readFileSync(path.join(source, "meta.db"))).digest("hex"),
    destination_meta_sha256: crypto.createHash("sha256").update(fs.readFileSync(path.join(destination, "meta.db"))).digest("hex"),
  });

  const parquet = manifest.files.find((entry) => entry.relative_path === parquetRelative);
  assert(parquet, "Parquet file is missing from the migration manifest");
  const copied = fs.readFileSync(path.join(destination, parquetRelative));
  assert(parquet.bytes === copied.length, "Parquet manifest byte count differs");
  assert(parquet.sha256 === crypto.createHash("sha256").update(copied).digest("hex"), "Parquet hash differs");
  assert(copied.equals(parquetBody), "Parquet payload differs after migration");
  record("parquet-manifest", started, {
    relative_path: parquetRelative,
    manifest_bytes: parquet.bytes,
    copied_bytes: copied.length,
    manifest_sha256: parquet.sha256,
    copied_sha256: crypto.createHash("sha256").update(copied).digest("hex"),
    payload_matches: copied.equals(parquetBody),
  });
}

engine = worker({ unsetEnv: ["ASTOCK_DATA_DIR"] });
try {
  await handshake(engine, "migration-destination");
  const status = await engine.request("diagnostics.status");
  assert(status.data_root?.origin === "migrated_redirect", "restart did not adopt migrated redirect");
  assert(path.resolve(status.data_root?.path) === path.resolve(destination), "restart opened the wrong migrated root");
  await loadConversation(engine, conversationId, "source-preserved");
  const started = Date.now();
  const outcome = await engine.request("storage.data_root.rollback", {}, { deadlineMs: 120_000 });
  assert(outcome.source_sqlite_integrity === "ok", "rollback did not verify the retained source database");
  assert(outcome.source_retained && outcome.migrated_copy_retained, "rollback deleted a data copy");
  assert(outcome.restart_required, "rollback must activate after restart");
  record("rollback", started, {
    active_before_rollback: path.resolve(status.data_root?.path),
    source,
    destination,
    source_sqlite_integrity: outcome.source_sqlite_integrity,
    source_retained: outcome.source_retained,
    migrated_copy_retained: outcome.migrated_copy_retained,
    restart_required: outcome.restart_required,
  });
} finally {
  await engine.shutdown();
}

engine = worker({ unsetEnv: ["ASTOCK_DATA_DIR"] });
try {
  await handshake(engine, "migration-rollback-source");
  const status = await engine.request("diagnostics.status");
  assert(path.resolve(status.data_root?.path) === path.resolve(source), "rollback pointer did not reactivate source");
  await loadConversation(engine, conversationId, "source-preserved");
  assert(fs.existsSync(path.join(destination, "meta.db")), "rollback removed the migrated copy");
  const repeated = await engine.request("storage.data_root.rollback", {}, { deadlineMs: 120_000 });
  assert(path.resolve(repeated.data_dir) === path.resolve(source), "repeated rollback changed the source pointer");
  assert(path.resolve(repeated.migrated_copy) === path.resolve(destination), "repeated rollback lost the migrated-copy identity");
  assert(repeated.source_retained && repeated.migrated_copy_retained, "repeated rollback removed a data copy");
} finally {
  await engine.shutdown();
}

const legacyProfile = path.join(testRoot, "legacy-profile");
const legacyAppData = path.join(legacyProfile, "Roaming");
const legacyLocalAppData = path.join(legacyProfile, "Local");
const legacyRoot = path.join(legacyAppData, "astock-terminal");
fs.mkdirSync(legacyLocalAppData, { recursive: true });
engine = new FramedWorker(engineExecutable, {
  name: "legacy-bootstrap-engine",
  env: { ASTOCK_DATA_DIR: legacyRoot, APPDATA: legacyAppData, LOCALAPPDATA: legacyLocalAppData },
});
try {
  await handshake(engine, "legacy-bootstrap");
  await saveConversation(engine, "legacy-evidence-conversation", "legacy-adopted");
} finally {
  await engine.shutdown();
}

{
  const started = Date.now();
  engine = new FramedWorker(engineExecutable, {
    name: "legacy-adoption-engine",
    env: { APPDATA: legacyAppData, LOCALAPPDATA: legacyLocalAppData },
    unsetEnv: ["ASTOCK_DATA_DIR"],
  });
  try {
    await handshake(engine, "legacy-adoption");
    const status = await engine.request("diagnostics.status");
    assert(status.data_root?.origin === "legacy_adopted", `legacy origin was ${status.data_root?.origin}`);
    await loadConversation(engine, "legacy-evidence-conversation", "legacy-adopted");
    record("legacy-data-adoption", started, {
      origin: status.data_root?.origin,
      adopted_path: path.resolve(status.data_root?.path),
      expected_path: path.resolve(legacyRoot),
      conversation_reloaded: true,
    });
  } finally {
    await engine.shutdown();
  }
}

process.stdout.write(`${JSON.stringify({ ok: true, cases })}\n`);
