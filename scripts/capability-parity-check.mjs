import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");
const releaseMode = process.argv.includes("--release");
const tauriRegistryPath = path.join(root, "src-tauri", "src", "lib.rs");
const providerDir = path.join(root, "crates", "market-data", "src", "providers");
const globalCatalogPath = path.join(root, "crates", "global-intelligence", "src", "lib.rs");
const engineSchemaPath = path.join(root, "protocol", "schema", "engine.schema.json");
const engineDispatchPath = path.join(root, "crates", "engine", "src", "lib.rs");
const engineFramingPath = path.join(root, "crates", "engine", "src", "main.rs");
const agentDispatchPath = path.join(root, "app-moon", "agent_worker", "main.mbt");
const legacyCapabilityMapPath = path.join(root, "protocol", "legacy-capability-map.json");

const expectedLegacyHandlerCount = 127;
const expectedLegacyHandlerHash = "b55ed6504d2c97ab3463274cf826e8b34b1f60257e8447a79b43baab26a8e700";
const expectedLegacyMappingHash = "97f5ee6a6a198e296202d4c55bf14865e295b8a85047778363c6592900613c13";

// Exact legacy capabilities that are reachable through the new coarse Engine
// contract. Before cutover, everything else in the frozen 127-command registry
// was a release blocker. A handler was only moved here after its new contract,
// consumer and test were all present. After cutover this set, its count and its
// immutable hash preserve the reviewed differential oracle without legacy code.
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
  "market.get_news_provider_health",
  "market.set_news_provider_enabled",
  "market.get_news_archive_recent",
  "market.get_news_archive_revisions",
  "market.check_news_archive_integrity",
  "market.get_news_ingest_observations",
  "news.query_news_center",
  "news.refresh_news_center",
  "news.set_news_item_state",
  "news.get_news_event_clusters",
  "news.get_news_event_cluster_detail",
  "news.merge_news_event_clusters",
  "news.split_news_event_revision",
  "news.get_pending_news_evidence_reviews",
  "news.resolve_news_evidence_review",
  "entities.get_news_entity_links",
  "entities.get_entity_link_reviews",
  "entities.resolve_entity_link_review",
  "event.event_analysis_start",
  "event.event_analysis_status",
  "event.event_analysis_cancel",
  "relations.query_relation_reviews",
  "relations.relation_extraction_cancel",
  "relations.relation_extraction_start",
  "relations.relation_extraction_status",
  "relations.retract_relation_candidate",
  "relations.review_relation_candidate",
  "deep.graph_as_of",
  "deep.graph_edge_timeline",
  "deep.graph_history_bounds",
  "deep.graph_snapshot_diff",
  "deep.graph_snapshot_get",
  "deep.graph_subgraph",
  "deep.relationship_graph",
  "deep.supply_chain_shock",
  "deep.quant_research_cancel",
  "deep.quant_research_snapshot_get",
  "deep.quant_research_snapshot_list",
  "deep.quant_research_start",
  "deep.quant_research_status",
  "deep.backtest_cancel",
  "deep.backtest_start",
  "deep.backtest_status",
  "deep.get_market_regime",
  "deep.list_strategies",
  "deep.run_backtest",
  "source_evidence.fetch_source_document",
  "source_evidence.get_source_documents",
  "source_evidence.get_source_document",
  "source_evidence.compare_source_evidence",
  "disclosure.query_disclosures",
  "disclosure.get_disclosure_detail",
  "disclosure.disclosure_sync_start",
  "disclosure.disclosure_sync_status",
  "disclosure.disclosure_sync_cancel",
  "disclosure.get_disclosure_provider_health",
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
  "analysis.chanlun_minute",
  "fundamental.get_fundamentals",
  "fundamental.get_valuation",
  "fundamental.get_earnings_driver_tree",
  "fundamental.run_earnings_driver_shock",
  "fundamental.get_earnings_driver_snapshot",
  "datacenter.get_zt_pool",
  "datacenter.get_pool",
  "datacenter.get_billboard",
  "datacenter.get_margin_daily",
  "datacenter.get_org_survey",
  "datacenter.get_holder_num",
  "datacenter.get_earnings_predict",
  "datacenter.get_lift_stage",
  "datacenter.get_suspensions",
  "datacenter.get_notices",
  "datacenter.get_boards",
  "datacenter.get_board_cons",
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
  "settings.set_data_dir",
  "global.global_sync_start",
  "global.global_sync_status",
  "global.global_sync_cancel",
  "global.get_global_provider_health",
  "global.query_global_documents",
  "global.get_global_golden_chains",
  "global.get_global_transmission_paths",
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
  sec_edgar: "READY",
  sina: "READY",
  tdx_adapter: "ENRICHED",
  tencent: "READY",
  tushare: "ENRICHED",
  world_bank: "ENRICHED",
};

// These sources currently return research data. The complete official catalog
// is separately exposed by research.global.providers with explicit disabled /
// NOT VERIFIED state for entries that have no collector. Catalog visibility
// must never be confused with successful data collection.
const dataBearingGlobalSources = new Set([
  "sec_edgar",
  "world_bank",
  "sge_gold",
  "world_gold_council",
]);

function fail(message) {
  console.error(`[capability-parity] ${message}`);
  process.exitCode = 1;
}

let legacyHandlers;
if (fs.existsSync(tauriRegistryPath)) {
  const tauriSource = fs.readFileSync(tauriRegistryPath, "utf8");
  const handlerBody = tauriSource.match(/generate_handler!\[([\s\S]*?)\]\)/)?.[1];
  if (!handlerBody) {
    fail("could not locate the frozen Tauri handler registry");
    process.exit();
  }
  legacyHandlers = [...handlerBody.matchAll(/commands::([a-z_]+)::([a-z_]+)/g)]
    .map((match) => `${match[1]}.${match[2]}`)
    .sort();
} else {
  // After the cutover the exact frozen registry is represented by the
  // migrated set itself and still checked against the immutable count/hash.
  legacyHandlers = [...migratedHandlers].sort();
}
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
const unknownDataBearing = [...dataBearingGlobalSources].filter((name) => !globalProviders.includes(name));
if (unknownDataBearing.length) {
  fail(`data-bearing global-source declarations are stale: ${unknownDataBearing.join(", ")}`);
}
const engineSchema = fs.readFileSync(engineSchemaPath, "utf8");
const engineDispatch = fs.readFileSync(engineDispatchPath, "utf8");
const engineFraming = fs.readFileSync(engineFramingPath, "utf8");
const agentDispatch = fs.readFileSync(agentDispatchPath, "utf8");
const engineKinds = new Set(
  JSON.parse(engineSchema).properties.request_kinds.prefixItems.map((item) => item.const),
);
const legacyCapabilityMap = JSON.parse(fs.readFileSync(legacyCapabilityMapPath, "utf8"));
const capabilityRows = Array.isArray(legacyCapabilityMap.capabilities)
  ? legacyCapabilityMap.capabilities
  : [];
const legacyMappingHash = crypto.createHash("sha256").update(
  capabilityRows
    .map((row) => JSON.stringify(row))
    .sort()
    .join("\n"),
).digest("hex");
const mappedLegacy = new Set();
const mappedReplacements = new Set();
const statusCounts = { READY: 0, ENRICHED: 0 };

if (legacyCapabilityMap.schema_version !== 1 ||
    legacyCapabilityMap.frozen_legacy_count !== expectedLegacyHandlerCount ||
    legacyCapabilityMap.frozen_legacy_sha256 !== expectedLegacyHandlerHash ||
    legacyCapabilityMap.frozen_mapping_sha256 !== expectedLegacyMappingHash ||
    legacyMappingHash !== expectedLegacyMappingHash ||
    !Array.isArray(legacyCapabilityMap.capabilities)) {
  fail("legacy capability map metadata does not match the frozen v5 registry");
}
for (const row of capabilityRows) {
  if (!Array.isArray(row) || row.length !== 3 || typeof row[0] !== "string" ||
      !Array.isArray(row[1]) || row[1].length === 0 || !["READY", "ENRICHED"].includes(row[2])) {
    fail(`invalid legacy capability map row: ${JSON.stringify(row)}`);
    continue;
  }
  const [legacy, replacements, status] = row;
  if (mappedLegacy.has(legacy)) fail(`duplicate legacy capability map row: ${legacy}`);
  mappedLegacy.add(legacy);
  statusCounts[status] += 1;
  for (const replacement of replacements) {
    if (typeof replacement !== "string" || replacement.length === 0) {
      fail(`invalid replacement request kind for ${legacy}`);
      continue;
    }
    const engineReachable = engineKinds.has(replacement) &&
      (engineDispatch.includes(`"${replacement}"`) || engineFraming.includes(`"${replacement}"`));
    const agentReachable = agentDispatch.includes(`"${replacement}" =>`);
    if (!engineReachable && !agentReachable) {
      fail(`${legacy} maps to unreachable request kind ${replacement}`);
    }
    mappedReplacements.add(replacement);
  }
}
const missingCapabilityRows = legacyHandlers.filter((name) => !mappedLegacy.has(name));
const unknownCapabilityRows = [...mappedLegacy].filter((name) => !migratedHandlers.has(name));
if (missingCapabilityRows.length || unknownCapabilityRows.length ||
    mappedLegacy.size !== expectedLegacyHandlerCount) {
  fail(`legacy capability map drifted; missing=${missingCapabilityRows.join(",")}; unknown=${unknownCapabilityRows.join(",")}`);
}
const globalCatalogVisible = engineSchema.includes('"research.global.providers"')
  && engineDispatch.includes('"research.global.providers" => global_sync::provider_health');
const catalogVisibilityBlockers = globalCatalogVisible ? [] : globalProviders;
const globalSourceNotVerified = globalProviders.filter((name) => !dataBearingGlobalSources.has(name));

const marketProviderBlockers = Object.entries(marketProviderStatus)
  .filter(([, status]) => status === "INTERNAL_ONLY" || status === "GAP")
  .map(([name]) => name);

if (releaseMode && blockers.length) {
  fail(`${blockers.length} legacy handlers are not READY/ENRICHED for the Proton architecture`);
}
if (releaseMode && marketProviderBlockers.length) {
  fail(`market providers are not renderer/Agent reachable: ${marketProviderBlockers.join(", ")}`);
}
if (releaseMode && catalogVisibilityBlockers.length) {
  fail(`${catalogVisibilityBlockers.length} official global sources are hidden from Engine provider health`);
}

console.log(JSON.stringify({
  ok: process.exitCode !== 1,
  mode: releaseMode ? "release" : "diagnostic",
  legacy_handlers: legacyHandlers.length,
  migrated_handlers: migratedHandlers.size,
  mapped_legacy_handlers: mappedLegacy.size,
  legacy_mapping_sha256: legacyMappingHash,
  reachable_replacement_request_kinds: mappedReplacements.size,
  mapped_statuses: statusCounts,
  legacy_handler_blockers: blockers.length,
  legacy_blockers: blockers,
  market_provider_modules: actualMarketProviders.length,
  market_provider_blockers: marketProviderBlockers,
  official_global_sources: globalProviders.length,
  catalog_visible_global_sources: globalCatalogVisible ? globalProviders.length : 0,
  data_bearing_global_sources: dataBearingGlobalSources.size,
  global_source_blockers: catalogVisibilityBlockers.length,
  global_source_not_verified: globalSourceNotVerified,
  enriched_extra_sources: ["yahoo_finance_comex_gold", "newsnow_finance_channels"],
}, null, 2));
