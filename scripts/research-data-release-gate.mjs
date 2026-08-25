import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

const MAX_OUTPUT_BYTES = 16 * 1024 * 1024;

function invariant(condition, message) {
  if (!condition) throw new Error(message);
}

function successfulProviders(rows) {
  return new Set((rows ?? []).filter((row) => row?.ok === true).map((row) => row.provider));
}

function assertNoCredentialedAudit(result, symbol) {
  invariant(result?.joinquant?.tested === false, `${symbol}: public-data gate invoked a credentialed provider`);
}

function assertIdentity(result, symbol, expectedName) {
  const exact = (result?.identity_search?.items ?? []).filter((item) => item.code === symbol);
  invariant(exact.length === 1, `${symbol}: canonical identity search did not return exactly one record`);
  invariant(exact[0].name === expectedName, `${symbol}: canonical name mismatch (${exact[0].name ?? "missing"})`);
  invariant(!/\s/u.test(exact[0].name), `${symbol}: canonical name contains spacing noise`);
}

export function validateAuditResults({ standard, beijing, legacy }) {
  invariant(standard.exitCode === 0 && standard.data?.ok === true, "300308: public data audit did not pass");
  assertNoCredentialedAudit(standard.data, "300308");
  assertIdentity(standard.data, "300308", "中际旭创");
  invariant(standard.data.reconciliation?.blocking === false, "300308: reconciliation is blocking");
  invariant(successfulProviders(standard.data.reconciliation?.quote_sources).size >= 2,
    "300308: fewer than two quote providers succeeded");
  invariant(successfulProviders(standard.data.reconciliation?.kline_sources).size >= 2,
    "300308: fewer than two K-line providers succeeded");
  const quoteProviders = successfulProviders(standard.data.reconciliation?.quote_sources);
  const requiredConsensus = Math.max(2, Math.floor(quoteProviders.size / 2) + 1);
  const reportedRequired = standard.data.reconciliation?.quote_required_consensus;
  const consensusProviders = new Set(standard.data.reconciliation?.quote_consensus_providers ?? []);
  const outlierProviders = new Set(standard.data.reconciliation?.quote_outlier_providers ?? []);
  invariant(reportedRequired === requiredConsensus,
    "300308: quote majority requirement is missing or inconsistent");
  invariant(standard.data.reconciliation?.quote_consensus_sources === consensusProviders.size,
    "300308: quote consensus count contradicts provider identities");
  invariant(consensusProviders.size >= requiredConsensus,
    "300308: quote reconciliation lacks a strict majority");
  invariant([...consensusProviders, ...outlierProviders].every((provider) => quoteProviders.has(provider)) &&
    consensusProviders.size + outlierProviders.size === quoteProviders.size,
  "300308: quote consensus/outlier partition is incomplete");
  invariant((standard.data.reconciliation?.quote_conflicts ?? [])
    .every((conflict) => outlierProviders.has(conflict.provider)),
  "300308: a quote conflict exists inside the accepted consensus");
  invariant(outlierProviders.size === 0 || standard.data.degraded === true,
    "300308: quarantined quote outliers are not exposed as degraded data");
  invariant((standard.data.reconciliation?.kline_conflicts ?? []).length === 0,
    "300308: K-line reconciliation has conflicts");
  invariant((standard.data.candidates?.count ?? 0) >= 50,
    "300308: candidate universe is too small");
  invariant(standard.data.candidates?.standardized_name_count === standard.data.candidates?.count,
    "300308: candidate universe contains missing or non-standard names");
  invariant(typeof standard.data.candidates?.liquidity_incomplete === "boolean",
    "300308: candidate liquidity completeness is not explicit");
  const candidateLiquidity = standard.data.candidates?.liquidity_available_count ?? 0;
  invariant(candidateLiquidity >= 0 && candidateLiquidity <= standard.data.candidates.count,
    "300308: candidate liquidity coverage is invalid");
  invariant(standard.data.candidates.liquidity_incomplete ===
    (candidateLiquidity < standard.data.candidates.count),
  "300308: candidate liquidity completeness contradicts coverage");
  const standardNews = standard.data.news?.[0];
  invariant((standardNews?.item_count ?? 0) >= 20, "300308: fewer than 20 current news items were available");
  invariant(new Set(standardNews?.successful_channels ?? []).size >= 2,
    "300308: fewer than two news channels succeeded");

  assertNoCredentialedAudit(beijing.data, "920001");
  assertIdentity(beijing.data, "920001", "纬达光电");
  const beijingQuotes = successfulProviders(beijing.data.reconciliation?.quote_sources);
  const beijingKlines = successfulProviders(beijing.data.reconciliation?.kline_sources);
  invariant(beijingQuotes.size >= 1, "920001: no quote source succeeded");
  invariant(beijingKlines.size >= 1, "920001: no K-line source succeeded");
  if (beijing.data.reconciliation?.blocking === true) {
    invariant(beijing.exitCode !== 0 && beijing.data.ok === false,
      "920001: a blocking single-source result was reported as passing");
  } else {
    invariant(beijing.exitCode === 0 && beijing.data.ok === true,
      "920001: a non-blocking result did not pass");
    invariant(beijingQuotes.size >= 2 && beijingKlines.size >= 2,
      "920001: non-blocking status lacks two-source quote and K-line coverage");
  }

  invariant(legacy.exitCode !== 0 && legacy.data?.ok === false, "430002: legacy code was accepted as live data");
  assertNoCredentialedAudit(legacy.data, "430002");
  invariant((legacy.data.identity_search?.items ?? []).length === 0,
    "430002: legacy code leaked into active identity search");
  invariant(legacy.data.reconciliation?.blocking === true,
    "430002: legacy code was not marked blocking");
  invariant((legacy.data.reconciliation?.quote_sources ?? []).length === 0 &&
    (legacy.data.reconciliation?.kline_sources ?? []).length === 0,
  "430002: fabricated live data was attached to a legacy code");
  invariant((legacy.data.request_failures ?? []).some((failure) => failure.kind === "research.data_reconcile"),
    "430002: the live reconciliation rejection was not recorded");

  return {
    standard: {
      symbol: "300308",
      name: "中际旭创",
      quote_providers: [...successfulProviders(standard.data.reconciliation.quote_sources)],
      quote_consensus_providers: [...consensusProviders],
      quote_outlier_providers: [...outlierProviders],
      kline_providers: [...successfulProviders(standard.data.reconciliation.kline_sources)],
      news_items: standardNews.item_count,
      news_channels: [...new Set(standardNews.successful_channels)],
      candidate_count: standard.data.candidates.count,
      candidate_liquidity_count: candidateLiquidity,
      candidate_liquidity_incomplete: standard.data.candidates.liquidity_incomplete,
      degraded: standard.data.degraded,
    },
    beijing: {
      symbol: "920001",
      name: "纬达光电",
      blocking: beijing.data.reconciliation.blocking,
      quote_providers: [...beijingQuotes],
      kline_providers: [...beijingKlines],
      degraded: beijing.data.degraded,
    },
    legacy: {
      symbol: "430002",
      rejected: true,
      failed_operations: legacy.data.request_failures.map((failure) => failure.kind),
    },
  };
}

function runAudit(engineExecutable, symbol, testRoot) {
  return new Promise((resolve, reject) => {
    const symbolRoot = path.join(testRoot, symbol);
    fs.mkdirSync(symbolRoot, { recursive: true });
    const env = {
      ...process.env,
      ASTOCK_DATA_DIR: path.join(symbolRoot, "data"),
      LOCALAPPDATA: path.join(symbolRoot, "local"),
      APPDATA: path.join(symbolRoot, "roaming"),
    };
    for (const name of ["MINIMAX_API_KEY", "JOINQUANT_USERNAME", "JOINQUANT_PASSWORD"]) delete env[name];
    const child = spawn(process.execPath, [
      path.join(import.meta.dirname, "research-data-audit.mjs"),
      engineExecutable,
      symbol,
    ], { env, windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = Buffer.alloc(0);
    let stderr = Buffer.alloc(0);
    const append = (current, chunk) => {
      const next = Buffer.concat([current, chunk]);
      if (next.length > MAX_OUTPUT_BYTES) {
        child.kill();
        throw new Error(`${symbol}: audit output exceeded 16 MiB`);
      }
      return next;
    };
    child.stdout.on("data", (chunk) => {
      try { stdout = append(stdout, chunk); } catch (error) { reject(error); }
    });
    child.stderr.on("data", (chunk) => {
      try { stderr = append(stderr, chunk); } catch (error) { reject(error); }
    });
    child.on("error", reject);
    child.on("close", (exitCode) => {
      try {
        const text = stdout.toString("utf8").trim();
        invariant(text.length > 0, `${symbol}: audit returned no JSON; ${stderr.toString("utf8").slice(0, 500)}`);
        const data = JSON.parse(text);
        const result = { exitCode: exitCode ?? -1, data };
        fs.writeFileSync(
          path.join(symbolRoot, "audit-result.json"),
          `${JSON.stringify({
            schema_version: 1,
            symbol,
            exit_code: result.exitCode,
            credentialed_providers_tested: false,
            data,
          }, null, 2)}\n`,
          "utf8",
        );
        resolve(result);
      } catch (error) {
        reject(error);
      }
    });
  });
}

async function main() {
  const [engineExecutable, rawTestRoot, commit, rawBuildRoot] = process.argv.slice(2);
  invariant(engineExecutable && fs.statSync(engineExecutable).isFile(),
    "usage: node scripts/research-data-release-gate.mjs <engine.exe> <test-root> <commit> <build-root>");
  invariant(/^[a-f0-9]{40}$/i.test(commit ?? ""), "release data gate requires a full Git commit");
  const testRoot = path.resolve(rawTestRoot ?? "");
  const buildRoot = path.resolve(rawBuildRoot ?? "");
  const relative = path.relative(buildRoot, testRoot);
  invariant(relative && !relative.startsWith("..") && !path.isAbsolute(relative),
    "release data gate root must be a child of ASTOCK_BUILD_ROOT");
  fs.mkdirSync(testRoot, { recursive: true });
  // Run sequentially so a release audit never fans out three complete market,
  // news and reconciliation batches against the same public providers.
  const standard = await runAudit(path.resolve(engineExecutable), "300308", testRoot);
  const beijing = await runAudit(path.resolve(engineExecutable), "920001", testRoot);
  const legacy = await runAudit(path.resolve(engineExecutable), "430002", testRoot);
  const summary = validateAuditResults({ standard, beijing, legacy });
  process.stdout.write(`${JSON.stringify({
    schema_version: 1,
    gate: "public-research-data",
    status: "PASSED",
    commit,
    completed_at_utc: new Date().toISOString(),
    credentialed_providers_tested: false,
    isolated_test_root: testRoot,
    summary,
  }, null, 2)}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await main();
}
