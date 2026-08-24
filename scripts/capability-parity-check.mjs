import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");
const releaseMode = process.argv.includes("--release");
const tauriRegistryPath = path.join(root, "src-tauri", "src", "lib.rs");
const providerDir = path.join(root, "crates", "market-data", "src", "providers");
const globalCatalogPath = path.join(root, "crates", "global-intelligence", "src", "lib.rs");

const expectedLegacyHandlerCount = 127;
const expectedLegacyHandlerHash = "b55ed6504d2c97ab3463274cf826e8b34b1f60257e8447a79b43baab26a8e700";

// Exact legacy capabilities that are reachable through the new coarse Engine
// contract. Everything else in the frozen 127-command registry is a release
// blocker and keeps src-tauri as a differential oracle. A handler may only be
// moved here after its new contract, consumer and test are all present.
const migratedHandlers = new Set([
  "market.get_quote",
  "market.get_order_book",
  "market.get_kline",
  "market.get_minute",
  "market.search_stocks",
  "market.get_market_breadth",
  "market.get_all_a_shares",
  "market.get_fund_flow",
  "market.get_realtime_flow",
  "market.get_index_kline",
  "market.get_provider_health",
  "news.query_news_center",
  "news.refresh_news_center",
  "source_evidence.fetch_source_document",
  "source_evidence.get_source_documents",
  "source_evidence.get_source_document",
  "source_evidence.compare_source_evidence",
  "disclosure.query_disclosures",
  "data_quality.reconcile_quote_sources",
  "data_quality.reconcile_valuation_sources",
  "data_quality.get_data_quality_slo",
  "data_quality.get_data_quality_observations",
  "data_quality.get_field_lineage",
  "data_quality.get_data_reconciliations",
  "data_quality.get_data_health_report",
  "bundle.get_stock_bundle",
  "analysis.analyze",
  "analysis.chanlun_daily",
  "fundamental.get_fundamentals",
  "fundamental.get_valuation",
  "fundamental.get_earnings_driver_tree",
  "fundamental.run_earnings_driver_shock",
  "fundamental.get_earnings_driver_snapshot",
  "datacenter.get_zt_pool",
  "datacenter.get_billboard",
  "datacenter.get_margin_daily",
  "datacenter.get_org_survey",
  "datacenter.get_holder_num",
  "datacenter.get_earnings_predict",
  "datacenter.get_lift_stage",
  "datacenter.get_suspensions",
  "datacenter.get_notices",
  "datacenter.get_boards",
  "watchlist.watchlist_list",
  "watchlist.watchlist_add",
  "watchlist.watchlist_remove",
  "watchlist.watchlist_pin",
  "settings.minimax_set_key",
  "settings.minimax_status",
  "settings.minimax_quota",
  "settings.cache_stats",
  "settings.cache_cleanup",
  "settings.get_data_dir",
  "agent.agent_ask",
  "agent.agent_conversation_delete",
  "agent.agent_conversation_load",
  "agent.agent_conversations",
  "agent.agent_resume",
  "agent.agent_tasks",
  "agent.agent_cancel",
  "scan.scan_start",
  "scan.scan_status",
  "scan.scan_cancel",
  "settings.settings_get_provider_status",
  "settings.settings_set_provider_credentials",
  "settings.settings_get_agent_model_routing",
  "settings.settings_set_agent_model_routing",
]);

const marketProviderStatus = {
  cninfo_disclosure: "ENRICHED",
  eastmoney: "ENRICHED",
  eastmoney_f10: "ENRICHED",
  em_datacenter: "ENRICHED",
  finance_news: "ENRICHED",
  global_assets: "ENRICHED",
  iwencai_openapi: "ENRICHED",
  joinquant: "READY",
  sec_edgar: "INTERNAL_ONLY",
  sina: "READY",
  tdx_adapter: "ENRICHED",
  tencent: "READY",
  tushare: "ENRICHED",
  world_bank: "ENRICHED",
};

const exposedGlobalSources = new Set([
  "world_bank",
  "sge_gold",
  "world_gold_council",
]);

function fail(message) {
  console.error(`[capability-parity] ${message}`);
  process.exitCode = 1;
}

if (!fs.existsSync(tauriRegistryPath)) {
  fail("src-tauri migration oracle is missing while legacy blockers remain");
  process.exit();
}

const tauriSource = fs.readFileSync(tauriRegistryPath, "utf8");
const handlerBody = tauriSource.match(/generate_handler!\[([\s\S]*?)\]\)/)?.[1];
if (!handlerBody) {
  fail("could not locate the frozen Tauri handler registry");
  process.exit();
}

const legacyHandlers = [...handlerBody.matchAll(/commands::([a-z_]+)::([a-z_]+)/g)]
  .map((match) => `${match[1]}.${match[2]}`)
  .sort();
const handlerHash = crypto.createHash("sha256").update(legacyHandlers.join("\n")).digest("hex");
if (legacyHandlers.length !== expectedLegacyHandlerCount || handlerHash !== expectedLegacyHandlerHash) {
  fail(`legacy registry drifted: count=${legacyHandlers.length}, sha256=${handlerHash}; review and classify every change`);
}

const unknownMigrated = [...migratedHandlers].filter((name) => !legacyHandlers.includes(name));
if (unknownMigrated.length) {
  fail(`migrated-handler declarations are stale: ${unknownMigrated.join(", ")}`);
}
const blockers = legacyHandlers.filter((name) => !migratedHandlers.has(name));

const actualMarketProviders = fs.readdirSync(providerDir)
  .filter((name) => name.endsWith(".rs") && !["mod.rs", "news_ingest.rs"].includes(name))
  .map((name) => path.basename(name, ".rs"))
  .sort();
const declaredMarketProviders = Object.keys(marketProviderStatus).sort();
if (JSON.stringify(actualMarketProviders) !== JSON.stringify(declaredMarketProviders)) {
  fail(`market provider inventory drifted; actual=${actualMarketProviders.join(",")}; declared=${declaredMarketProviders.join(",")}`);
}

const globalSource = fs.readFileSync(globalCatalogPath, "utf8");
const catalogBody = globalSource.match(/pub fn official_global_sources\(\)[\s\S]*?vec!\[([\s\S]*?)\r?\n    \]\r?\n}/)?.[1];
if (!catalogBody) {
  fail("could not parse official_global_sources catalog");
  process.exit();
}
const globalProviders = [...catalogBody.matchAll(/provider_id:\s*"([^"]+)"/g)]
  .map((match) => match[1]);
const unknownExposed = [...exposedGlobalSources].filter((name) => !globalProviders.includes(name));
if (unknownExposed.length) {
  fail(`exposed global-source declarations are stale: ${unknownExposed.join(", ")}`);
}
const globalBlockers = globalProviders.filter((name) => !exposedGlobalSources.has(name));

const marketProviderBlockers = Object.entries(marketProviderStatus)
  .filter(([, status]) => status === "INTERNAL_ONLY" || status === "GAP")
  .map(([name]) => name);

if (releaseMode && blockers.length) {
  fail(`${blockers.length} legacy handlers are not READY/ENRICHED for the Proton architecture`);
}
if (releaseMode && marketProviderBlockers.length) {
  fail(`market providers are not renderer/Agent reachable: ${marketProviderBlockers.join(", ")}`);
}
if (releaseMode && globalBlockers.length) {
  fail(`${globalBlockers.length} official global sources are not exposed through the Engine contract`);
}

console.log(JSON.stringify({
  ok: process.exitCode !== 1,
  mode: releaseMode ? "release" : "diagnostic",
  legacy_handlers: legacyHandlers.length,
  migrated_handlers: migratedHandlers.size,
  legacy_handler_blockers: blockers.length,
  legacy_blockers: blockers,
  market_provider_modules: actualMarketProviders.length,
  market_provider_blockers: marketProviderBlockers,
  official_global_sources: globalProviders.length,
  exposed_global_sources: exposedGlobalSources.size,
  global_source_blockers: globalBlockers.length,
  enriched_extra_sources: ["yahoo_finance_comex_gold", "newsnow_finance_channels"],
}, null, 2));
