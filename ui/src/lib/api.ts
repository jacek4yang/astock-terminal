/**
 * Tauri 命令层类型化封装。
 * 契约见 ../docs/command-contract.md;所有命令返回 JSON(snake_case),
 * 错误统一 { error: string, kind: string }。
 */
import { Channel, invoke } from "@tauri-apps/api/core";
import { isProton, requestNative } from "../bridge";

/** 是否在 Tauri 桌面环境(纯浏览器 dev 时为 false) */
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** 是否连接到当前 Proton/CEF 桌面宿主。 */
export function isDesktop(): boolean {
  return isProton() || isTauri();
}

export const NOT_TAURI_MSG = "需在桌面应用中运行(纯浏览器模式无行情数据)";

/** 后端统一错误结构 */
export interface ApiError {
  error: string;
  kind?: string;
}

/** 规范化 invoke 抛出的错误为可读中文文案 */
export function errMsg(e: unknown): string {
  if (!isDesktop()) return NOT_TAURI_MSG;
  if (e && typeof e === "object") {
    const obj = e as Record<string, unknown>;
    if (typeof obj.error === "string") return obj.error;
    if (typeof obj.message === "string") return obj.message;
  }
  return String(e);
}

interface NativePage<T> {
  items: T[];
  next_cursor: number | null;
  snapshot_id: string;
}

async function readAllSharePages<T>(): Promise<T> {
  const items: unknown[] = [];
  let cursor: number | null = 0;
  let snapshotId: string | undefined;
  while (cursor != null) {
    const page: NativePage<unknown> = await requestNative<NativePage<unknown>>(
      "engine",
      "market.shares.page",
      { cursor, limit: 500, ...(snapshotId ? { snapshot_id: snapshotId } : {}) },
      { deadlineMs: 60_000 },
    );
    items.push(...page.items);
    snapshotId = page.snapshot_id;
    cursor = page.next_cursor;
  }
  return items as T;
}

async function protonCommand<T>(name: string, args: Record<string, unknown> = {}): Promise<T> {
  switch (name) {
    case "search_stocks": {
      const result = await requestNative<{ items: unknown[] }>("engine", "market.search", args);
      return result.items as T;
    }
    case "get_quote": {
      const result = await requestNative<{ quote: unknown }>("engine", "market.quote", args);
      return result.quote as T;
    }
    case "get_kline":
      return requestNative<T>("engine", "market.kline", args);
    case "get_index_kline": {
      const result = await requestNative<{ bars: unknown[] }>("engine", "market.index_kline", args);
      return result.bars as T;
    }
    case "get_order_book":
      return requestNative<T>("engine", "market.order_book", args);
    case "get_minute":
      return requestNative<T>("engine", "market.minute", args);
    case "get_fund_flow":
      return requestNative<T>("engine", "market.fund_flow.daily", args);
    case "get_realtime_flow":
      return requestNative<T>("engine", "market.fund_flow.realtime", args);
    case "chanlun_minute":
      return requestNative<T>("engine", "analysis.chanlun.minute", args, { deadlineMs: 60_000 });
    case "get_pool":
      return requestNative<T>("engine", "research.market_pool", args, { deadlineMs: 60_000 });
    case "get_board_cons":
      return requestNative<T>(
        "engine",
        "research.board.constituents",
        { board_code: args.bk_code },
        { deadlineMs: 60_000 },
      );
    case "query_disclosures":
      return requestNative<T>(
        "engine",
        "research.disclosures.list",
        (args.query ?? {}) as Record<string, unknown>,
      );
    case "get_disclosure_detail":
      return requestNative<T>("engine", "research.disclosures.detail", args);
    case "get_disclosure_provider_health":
      return requestNative<T>("engine", "research.disclosures.providers", {});
    case "disclosure_sync_start":
      return requestNative<T>(
        "engine",
        "research.disclosures.sync.start",
        (args.request ?? {}) as Record<string, unknown>,
        { deadlineMs: 60_000 },
      );
    case "disclosure_sync_status":
      return requestNative<T>("engine", "research.disclosures.sync.status", {});
    case "disclosure_sync_cancel":
      return requestNative<T>("engine", "research.disclosures.sync.cancel", {});
    case "get_news_provider_health":
      return requestNative<T>("engine", "research.news.providers", {});
    case "query_news_center":
      return requestNative<T>(
        "engine",
        "research.news.center",
        (args.query ?? {}) as Record<string, unknown>,
      );
    case "refresh_news_center":
      return requestNative<T>("engine", "research.news", args, { deadlineMs: 90_000 });
    case "set_news_provider_enabled":
      return requestNative<T>("engine", "research.news.provider.set", args);
    case "get_news_archive_recent":
      return requestNative<T>("engine", "research.news.archive.recent", args);
    case "get_news_archive_revisions":
      return requestNative<T>("engine", "research.news.archive.revisions", args);
    case "check_news_archive_integrity":
      return requestNative<T>("engine", "research.news.archive.integrity", {});
    case "get_news_ingest_observations":
      return requestNative<T>("engine", "research.news.archive.observations", args);
    case "set_news_item_state":
      return requestNative<T>("engine", "research.news.user_state", args);
    case "get_news_event_clusters":
      return requestNative<T>("engine", "research.news.clusters.list", args);
    case "get_news_event_cluster_detail":
      return requestNative<T>("engine", "research.news.clusters.detail", args);
    case "merge_news_event_clusters":
      return requestNative<T>("engine", "research.news.clusters.merge", args);
    case "split_news_event_revision":
      return requestNative<T>("engine", "research.news.clusters.split", args);
    case "get_pending_news_evidence_reviews":
      return requestNative<T>("engine", "research.news.reviews.list", args);
    case "resolve_news_evidence_review":
      return requestNative<T>("engine", "research.news.reviews.resolve", args);
    case "get_news_entity_links":
      return requestNative<T>("engine", "research.entities.links", args, { deadlineMs: 60_000 });
    case "get_entity_link_reviews":
      return requestNative<T>("engine", "research.entities.reviews", args);
    case "resolve_entity_link_review":
      return requestNative<T>("engine", "research.entities.resolve", args);
    case "get_market_breadth": {
      const result = await requestNative<{ breadth: unknown }>("engine", "market.overview", {});
      return result.breadth as T;
    }
    case "get_provider_health": {
      const result = await requestNative<{ provider_health: unknown }>("engine", "market.overview", {});
      return result.provider_health as T;
    }
    case "get_data_quality_slo":
      return requestNative<T>("engine", "diagnostics.data_quality", {
        action: "slo",
        window_secs: args.window_secs,
      });
    case "get_data_quality_observations":
      return requestNative<T>("engine", "diagnostics.data_quality", {
        action: "observations",
        dataset: args.dataset,
        provider: args.provider,
        limit: args.limit,
      });
    case "get_field_lineage":
      return requestNative<T>("engine", "diagnostics.data_quality", {
        action: "lineage",
        dataset: args.dataset,
        entity_key: args.entity_key,
        limit: args.limit,
      });
    case "get_data_reconciliations":
      return requestNative<T>("engine", "diagnostics.data_quality", {
        action: "reconciliations",
        dataset: args.dataset,
        entity_key: args.entity_key,
        limit: args.limit,
      });
    case "get_data_health_report":
      return requestNative<T>("engine", "diagnostics.data_quality", {
        action: "health",
        window_secs: args.window_secs,
      });
    case "reconcile_quote_sources":
      return requestNative<T>("engine", "research.quote_reconcile", args, { deadlineMs: 90_000 });
    case "reconcile_valuation_sources":
      return requestNative<T>("engine", "research.valuation_reconcile", args, { deadlineMs: 90_000 });
    case "scan_start":
      return requestNative<T>("engine", "quant.scan.start", {});
    case "scan_status":
      return requestNative<T>("engine", "quant.scan.status", {});
    case "scan_cancel":
      return requestNative<T>("engine", "quant.scan.cancel", {});
    case "settings_get_provider_status": {
      const status = await requestNative<{
        providers: {
          joinquant: boolean;
          optional: Record<string, { configured: boolean }>;
        };
      }>("engine", "credentials.status", {});
      return {
        tushare_token: Boolean(status.providers.optional.tushare?.configured),
        iwencai_key: Boolean(status.providers.optional.iwencai?.configured),
        jq_user: status.providers.joinquant,
        jq_pwd: status.providers.joinquant,
        sec_user_agent: Boolean(status.providers.optional.sec_edgar?.configured),
        socks5: Boolean(status.providers.optional.socks5?.configured),
      } as T;
    }
    case "settings_set_provider_credentials": {
      for (const [field, provider] of [
        ["tushare_token", "tushare"],
        ["iwencai_key", "iwencai"],
        ["sec_user_agent", "sec_edgar"],
        ["socks5", "socks5"],
      ] as const) {
        if (!(field in args)) continue;
        const value = String(args[field] ?? "").trim();
        await requestNative("engine", value ? "credentials.provider.set" : "credentials.provider.delete", value ? { provider, value } : { provider });
      }
      if ("jq_user" in args || "jq_pwd" in args) {
        const username = String(args.jq_user ?? "").trim();
        const password = String(args.jq_pwd ?? "");
        await requestNative(
          "engine",
          username && password ? "credentials.joinquant.set" : "credentials.joinquant.delete",
          username && password ? { username, password } : {},
        );
      }
      return { status: await protonCommand<ProviderStatus>("settings_get_provider_status"), message: "凭据已写入 Windows Credential Manager；需重启的数据源已明确标记。" } as T;
    }
    case "settings_get_agent_model_routing":
      return requestNative<T>("engine", "settings.agent_models.get", {});
    case "settings_set_agent_model_routing": {
      const settings = args.settings as AgentModelRoutingSettings;
      const saved = await requestNative<T>("engine", "settings.agent_models.set", settings);
      await requestNative("agent", "agent.provider.configure", { routing: settings, validate: false });
      return saved;
    }
    case "get_all_a_shares":
      return readAllSharePages<T>();
    case "get_stock_bundle": {
      return requestNative<T>("engine", "market.security_snapshot", args, { deadlineMs: 60_000 });
    }
    case "get_earnings_driver_tree":
      return requestNative<T>("engine", "research.earnings_driver.tree", args, {
        deadlineMs: 60_000,
      });
    case "run_earnings_driver_shock":
      return requestNative<T>("engine", "research.earnings_driver.shock", args, {
        deadlineMs: 60_000,
      });
    case "get_earnings_driver_snapshot":
      return requestNative<T>("engine", "research.earnings_driver.snapshot", args);
    case "get_source_documents":
      return requestNative<T>("engine", "research.sources.list", args);
    case "get_source_document":
      return requestNative<T>("engine", "research.sources.get", args);
    case "fetch_source_document":
      return requestNative<T>("engine", "research.sources.fetch", args, { deadlineMs: 60_000 });
    case "compare_source_evidence":
      return requestNative<T>("engine", "research.sources.compare", args);
    case "global_sync_start":
      return requestNative<T>("engine", "research.global.sync.start", (args.request ?? {}) as Record<string, unknown>, { deadlineMs: 60_000 });
    case "global_sync_status":
      return requestNative<T>("engine", "research.global.sync.status", {});
    case "global_sync_cancel":
      return requestNative<T>("engine", "research.global.sync.cancel", {});
    case "get_global_provider_health":
      return requestNative<T>("engine", "research.global.providers", {});
    case "query_global_documents":
      return requestNative<T>("engine", "research.global.documents", (args.query ?? {}) as Record<string, unknown>);
    case "get_global_golden_chains":
      return requestNative<T>("engine", "research.global.chains", {});
    case "get_global_transmission_paths":
      return requestNative<T>("engine", "research.global.transmission", args);
    case "watchlist_list":
      return requestNative<T>("engine", "workspace.watchlist.list", { group: args.group ?? "默认" });
    case "watchlist_add":
      return requestNative<T>("engine", "workspace.watchlist.add", {
        symbol: args.code,
        group: args.group ?? "默认",
      });
    case "watchlist_remove":
      return requestNative<T>("engine", "workspace.watchlist.remove", {
        symbol: args.code,
        group: args.group ?? "默认",
      });
    case "watchlist_pin":
      return requestNative<T>("engine", "workspace.watchlist.pin", {
        symbol: args.code,
        group: args.group ?? "默认",
        pinned: args.pinned,
      });
    default:
      throw new Error(`该功能尚未迁移到新的本地 Engine：${name}`);
  }
}

function cmd<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  if (isProton()) return protonCommand<T>(name, args);
  if (isTauri()) return invoke<T>(name, args);
  return Promise.reject(new Error(NOT_TAURI_MSG));
}

// ==================== 行情数据 ====================

export interface Quote {
  symbol: string;
  name: string;
  price: number;
  pct: number;
  change: number;
  high: number;
  low: number;
  open: number;
  pre_close: number;
  volume: number;
  amount: number;
  turnover: number | null;
  timestamp: string;
  field_provenance: Record<string, FieldProvenance>;
}

export interface FieldProvenance {
  source: string;
  as_of: string | null;
  fetched_at: string;
  stale: boolean;
  quality: "reported" | "reference" | "derived" | "missing";
  missing_reason: string | null;
}

export interface Bar {
  date: string;
  open: number;
  close: number;
  high: number;
  low: number;
  volume: number;
  amount: number | null;
  pct: number | null;
  turnover: number | null;
}

export type KlinePeriod = "day" | "week" | "month";
export type KlineAdjust = "qfq" | "hfq" | "none";

export interface KlineResult {
  bars: Bar[];
  source: string;
}

export interface MinutePoint {
  time: string;
  price: number;
  avg_price: number;
  volume: number;
}

export interface MinuteData {
  points: MinutePoint[];
  pre_close: number;
  name: string;
}

export interface SearchResult {
  code: string;
  name: string;
  classify: string;
}

export interface MarketBreadth {
  up: number;
  down: number;
  flat: number;
  total: number;
  breadth_ratio: number;
}

export interface AllShare {
  code: string;
  name: string;
  market: string;
  board: "main" | "chi_next" | "star" | "beijing" | "fund" | "other" | string;
  price: number | null;
  pct: number | null;
  amount: number | null;
  source: string;
  fetched_at: string;
}

export interface FundFlow {
  date: string;
  main_net: number;
  super_large_net: number;
  large_net: number;
  medium_net: number;
  small_net: number;
  main_pct: number;
}

export interface RealtimeFlowPoint {
  time: string;
  main_net: number;
  super_large_net: number;
  large_net: number;
  medium_net: number;
  small_net: number;
}

export interface RealtimeFlowSummary {
  main_net: number;
  super_large_net: number;
  large_net: number;
  medium_net: number;
  small_net: number;
}

export interface RealtimeFlow {
  points: RealtimeFlowPoint[];
  summary: RealtimeFlowSummary;
}

export const getQuote = (symbol: string) => cmd<Quote>("get_quote", { symbol });
export interface OrderBookLevel {
  level: number;
  price: number;
  volume: number;
}
export interface OrderBook {
  symbol: string;
  server_time: string;
  current_volume: number;
  inner_volume: number;
  outer_volume: number;
  bids: OrderBookLevel[];
  asks: OrderBookLevel[];
  source: string;
  fetched_at: string;
  transaction_detail_available: boolean;
  limitation: string;
}
export const getOrderBook = (symbol: string) => cmd<OrderBook>("get_order_book", { symbol });
export const getKline = (symbol: string, period: KlinePeriod, adjust: KlineAdjust, count: number) =>
  cmd<KlineResult>("get_kline", { symbol, period, adjust, count });
export const getMinute = (symbol: string) => cmd<MinuteData>("get_minute", { symbol });
export const searchStocks = (keyword: string) => cmd<SearchResult[]>("search_stocks", { keyword });
export const getMarketBreadth = () => cmd<MarketBreadth>("get_market_breadth");
export const getAllAShares = () => cmd<AllShare[]>("get_all_a_shares");
export const getFundFlow = (symbol: string, days: number) =>
  cmd<FundFlow[]>("get_fund_flow", { symbol, days });
export const getRealtimeFlow = (symbol: string) => cmd<RealtimeFlow>("get_realtime_flow", { symbol });
export const getIndexKline = (secid: string, count: number) =>
  cmd<Bar[]>("get_index_kline", { secid, count });

/** get_stock_bundle 返回:股票页一次取数,除 quote 外各分区可独立降级为 null */
export interface StockBundle {
  quote: Quote;
  kline: KlineResult | null;
  fund_flow_30d: FundFlow[] | null;
  analysis: SignalJson | null;
  chanlun_daily: ChanlunDailyJson | null;
  /** 降级为 null 的分区名(kline / fund_flow_30d / analysis / chanlun_daily) */
  missing: string[];
}

export const getStockBundle = (
  symbol: string,
  period: KlinePeriod,
  adjust: KlineAdjust,
  count: number,
) => cmd<StockBundle>("get_stock_bundle", { symbol, period, adjust, count });

/** get_provider_health 单项:某数据源熔断器快照 */
export interface ProviderHealthItem {
  name: string;
  /** closed=可用 / open=熔断中 / half_open=冷却结束试探中 */
  state: "closed" | "open" | "half_open" | string;
  /** 距下次试探的剩余秒数,仅 open 时有值 */
  cooldown_remaining_secs: number | null;
  /** 是否已配置(可选 provider 无 token 时为 false) */
  available: boolean;
}

export const getProviderHealth = () => cmd<ProviderHealthItem[]>("get_provider_health");

export type DatasetKind =
  | "realtime_quote"
  | "intraday_minute"
  | "daily_kline"
  | "weekly_kline"
  | "monthly_kline"
  | "fund_flow"
  | "fundamentals"
  | "valuation"
  | "announcement"
  | "news"
  | "knowledge_graph"
  | "macro"
  | "backtest"
  | "search_discovery"
  | "other";

export interface QualityFlag {
  code: string;
  severity: "info" | "warning" | "blocking";
  field: string | null;
  message: string;
}

export interface DataQualitySummary {
  dataset: DatasetKind;
  dataset_name: string;
  freshness: "fresh" | "stale" | "expired";
  age_secs: number;
  expected_cadence_secs: number;
  stale_after_secs: number;
  hard_expiry_secs: number;
  quality_flags: QualityFlag[];
  confidence_ceiling: "high" | "medium" | "low" | "blocked";
  allow_high_confidence: boolean;
  allow_deterministic_compute: boolean;
}

export interface QualityObservation {
  observation_id: number | null;
  dataset: DatasetKind;
  provider: string;
  entity_key: string | null;
  operation: string;
  success: boolean;
  latency_ms: number | null;
  summary: DataQualitySummary;
  missing_fields: number;
  conflicts: number;
  error_kind: string | null;
  recorded_at: number;
}

export interface DatasetSlo {
  dataset: DatasetKind;
  dataset_name: string;
  provider: string;
  observations: number;
  successes: number;
  error_rate: number;
  latency_p50_ms: number | null;
  latency_p95_ms: number | null;
  last_success_at: number | null;
  consecutive_stale: number;
  missing_fields: number;
  conflicts: number;
  current_freshness: "fresh" | "stale" | "expired";
  expected_cadence_secs: number;
  stale_after_secs: number;
  hard_expiry_secs: number;
  latest_quality_flags: QualityFlag[];
}

export interface FieldLineageRecord {
  lineage_id: number | null;
  dataset: DatasetKind;
  entity_key: string;
  field_path: string;
  source: string;
  source_url: string | null;
  event_time: number | null;
  as_of_time: number | null;
  publish_time: number | null;
  fetched_at: number;
  parser_version: string;
  schema_version: string;
  license: string;
  unit: string | null;
  currency: string | null;
  adjustment: string;
  revision: string | null;
  accounting_scope: string;
  quality_flags: QualityFlag[];
  created_at: number;
}

export interface NumericObservation {
  provider: string;
  field: string;
  value: number;
  unit: string;
  currency: string | null;
  adjustment: string;
  accounting_scope: string;
  as_of_time: string | null;
}

export interface ReconciliationResult {
  field: string;
  left: NumericObservation;
  right: NumericObservation;
  absolute_diff: number;
  relative_diff: number;
  tolerance: { absolute: number; relative: number };
  status: "matched" | "within_tolerance" | "conflict" | "incompatible_contract";
  explanation: string;
  quality_flags: QualityFlag[];
}

export interface ReconciliationAudit {
  reconciliation_id: number | null;
  dataset: DatasetKind;
  entity_key: string;
  result: ReconciliationResult;
  blocking: boolean;
  compared_at: number;
}

export interface QuoteReconciliationReport {
  symbol: string;
  compared_at: number;
  results: ReconciliationResult[];
  failures: { provider: string; error: string }[];
  blocking: boolean;
  comparable_sources: number;
  limitation: string | null;
}

export interface ValuationReconciliationReport {
  symbol: string;
  compared_at: number;
  results: ReconciliationResult[];
  failures: { provider: string; error: string }[];
  blocking: boolean;
  comparable_sources: number;
  limitation: string | null;
}

export interface DataHealthReport {
  generated_at: number;
  window_secs: number;
  actual_observations: number;
  coverage_start: number | null;
  coverage_end: number | null;
  coverage_secs: number;
  continuous_window_satisfied: boolean;
  rows: DatasetSlo[];
  markdown: string;
  limitation: string | null;
}

export const getDataQualitySlo = (windowSecs: number) =>
  cmd<DatasetSlo[]>("get_data_quality_slo", { window_secs: windowSecs });
export const getDataQualityObservations = (
  dataset: DatasetKind | null,
  provider: string | null,
  limit = 100,
) => cmd<QualityObservation[]>("get_data_quality_observations", { dataset, provider, limit });
export const getFieldLineage = (
  dataset: DatasetKind | null,
  entityKey: string | null,
  limit = 100,
) => cmd<FieldLineageRecord[]>("get_field_lineage", { dataset, entity_key: entityKey, limit });
export const getDataReconciliations = (
  dataset: DatasetKind | null,
  entityKey: string | null,
  limit = 100,
) => cmd<ReconciliationAudit[]>("get_data_reconciliations", { dataset, entity_key: entityKey, limit });
export const reconcileQuoteSources = (symbol: string) =>
  cmd<QuoteReconciliationReport>("reconcile_quote_sources", { symbol });
export const reconcileValuationSources = (symbol: string) =>
  cmd<ValuationReconciliationReport>("reconcile_valuation_sources", { symbol });
export const getDataHealthReport = (windowSecs: number) =>
  cmd<DataHealthReport>("get_data_health_report", { window_secs: windowSecs });

export type NewsTrustTier =
  | "first_party_disclosure"
  | "licensed_media"
  | "public_aggregator"
  | "search_lead";

export type NewsDeliveryMode = "push_stream" | "scheduled_index" | "published_incremental";

/** 可插拔资讯来源的能力、调度约束与运行状态。读取快照不会访问上游。 */
export interface NewsProviderHealthItem {
  provider_id: string;
  display_name: string;
  enabled: boolean;
  circuit_state: "closed" | "open" | string;
  trust_tier: NewsTrustTier;
  trust_tier_name: string;
  modes: NewsDeliveryMode[];
  license: string;
  endpoint: string;
  min_refresh_secs: number;
  rate_limit_per_minute: number;
  last_success_at: number | null;
  last_latency_ms: number | null;
  attempts: number;
  failures: number;
  failure_rate: number;
  stale: boolean;
  cursor_present: boolean;
  cooldown_remaining_secs: number | null;
  last_error_kind: string | null;
  archived_documents: number;
  archived_revisions: number;
  archive_last_observed_at: number | null;
  stale_age_secs: number | null;
}

export interface EvidenceTimestamp {
  utc: number | null;
  original: string | null;
}

export interface ArchivedNewsRevision {
  document_id: string;
  canonical_url: string;
  source_id: string;
  source_name: string;
  license: string;
  content_type: string;
  language: string;
  parser_version: string;
  content_hash: string;
  current_revision_id: string | null;
  document_first_seen_time_utc: number;
  last_observed_at: number;
  retention_class: string;
  revision_id: string;
  revision_hash: string;
  title: string;
  factual_summary: string;
  supersedes_revision_id: string | null;
  event_time: EvidenceTimestamp;
  publish_time: EvidenceTimestamp;
  first_seen_time_utc: number;
  revision_time: EvidenceTimestamp;
  raw_snapshot_hash: string | null;
}

export interface NewsUserState {
  document_id: string;
  is_read: boolean;
  pinned: boolean;
  favorite: boolean;
  ignored: boolean;
  updated_at: number;
}

export interface NewsCenterEventMeta {
  cluster_id: string;
  independent_sources: number;
  old_republication: boolean;
  conflict_fields: string[];
  status: string;
}

export type EffectiveSessionRole =
  | "same_day_premarket"
  | "intraday"
  | "next_trading_day"
  | "historical_only";

export type EffectiveMarketPhase =
  | "premarket"
  | "opening_auction"
  | "morning_trading"
  | "lunch_break"
  | "afternoon_trading"
  | "closing_auction"
  | "after_close"
  | "non_trading_day";

export interface EffectiveNewsSession {
  target_trading_date: string;
  role: EffectiveSessionRole;
  phase: EffectiveMarketPhase;
  effective_at_utc: number;
  effective_at_china: string;
  publication_precision: "exact_time" | "date_only" | "missing";
  time_uncertain: boolean;
  evidence_use: "decision_evidence" | "verification_lead" | "historical_context";
  can_increase_confidence: boolean;
  rationale: string;
  rules_version: string;
}

export interface NewsCenterItem {
  revision: ArchivedNewsRevision;
  user_state: NewsUserState;
  important: boolean;
  importance_reason: string;
  event_type: string;
  verification: string;
  verification_name: string;
  event: NewsCenterEventMeta | null;
  entity_links: DocumentEntityLink[];
  effective_session: EffectiveNewsSession;
}

export interface NewsCenterSourceFacet {
  source_id: string;
  source_name: string;
  count: number;
}

export interface NewsCenterPage {
  items: NewsCenterItem[];
  total: number;
  page: number;
  page_size: number;
  has_more: boolean;
  generated_at: number;
  newest_first_seen: number | null;
  newest_observed_at: number | null;
  archive_age_secs: number | null;
  source_facets: NewsCenterSourceFacet[];
}

export interface NewsCenterQuery {
  keyword: string;
  category: string;
  source_id: string;
  importance: string;
  entity_keywords: string[];
  event_type: string;
  language: string;
  verification: string;
  user_state: string;
  from_utc: number | null;
  to_utc: number | null;
  page: number;
  page_size: number;
}

export interface NewsRefreshResult {
  items: unknown[];
  successful_sources: string[];
  stale_sources: string[];
  errors: string[];
}

export interface NewsIngestObservation {
  observation_id: number;
  document_id: string | null;
  revision_id: string | null;
  provider_id: string;
  endpoint: string;
  fetched_at: number;
  http_status: number | null;
  etag: string | null;
  last_modified: string | null;
  latency_ms: number | null;
  parse_status: string;
  parse_error: string | null;
  raw_evidence_hash: string | null;
  raw_evidence_present: boolean;
}

export type DocumentRelationship =
  | "first_publication"
  | "reprint"
  | "summary"
  | "follow_up"
  | "commentary"
  | "correction"
  | "retraction"
  | "duplicate_fetch";

export interface SimilarityFeatures {
  same_url: boolean;
  same_content: boolean;
  title_exact: boolean;
  simhash_similarity: number;
  minhash_similarity: number;
  semantic_similarity: number;
  entity_overlap: number;
  action_overlap: number;
  time_proximity: number;
}

export interface ClusterExplanation {
  score: number;
  merge_threshold: number;
  reasons: string[];
  separation_reasons: string[];
  features: SimilarityFeatures;
}

export interface NewsEventCluster {
  cluster_id: string;
  canonical_title: string;
  event_time_utc: number | null;
  first_seen_time_utc: number;
  primary_revision_id: string;
  first_source_id: string;
  independent_sources: number;
  evidence_diversity: number;
  latest_revision_id: string;
  conflict_fields: string[];
  model_version: string;
  status: string;
  merged_into_cluster_id: string | null;
  created_at: number;
  updated_at: number;
}

export interface NewsEventClusterMember {
  cluster_id: string;
  revision_id: string;
  relationship: DocumentRelationship;
  merge_score: number;
  explanation: ClusterExplanation;
  old_republication: boolean;
  assigned_by: string;
  model_version: string;
  active: boolean;
  created_at: number;
}

export interface NewsEventConflict {
  cluster_id: string;
  field_name: string;
  values: string[];
  authoritative_revision_id: string | null;
  status: string;
}

export interface NewsEventClusterDetail {
  cluster: NewsEventCluster;
  members: NewsEventClusterMember[];
  revisions: ArchivedNewsRevision[];
  conflicts: NewsEventConflict[];
}

export interface EventEntityRef {
  entity_id: string;
  name: string;
  listed_code: string | null;
  role: string;
}

export interface EventFieldEvidence {
  evidence_id: string;
  event_id: string;
  field_name: string;
  provenance: string;
  source_revision_id: string | null;
  source_version_id: string | null;
  quote_original: string | null;
  quote_zh: string | null;
  location: unknown;
  observed_at: number;
  confidence_bps: number;
}

export interface StructuredEvent {
  event_id: string;
  source_revision_id: string;
  kind: string;
  title: string;
  subjects: EventEntityRef[];
  objects: EventEntityRef[];
  amount_text: string | null;
  quantity_text: string | null;
  unit_original: string | null;
  currency_original: string | null;
  baseline_period: string | null;
  starts_at: number | null;
  ends_at: number | null;
  region: string | null;
  conditions: string[];
  official_effective: boolean | null;
  reversibility: string;
  impact_horizon: string;
  lifecycle: string;
  catalyst_path: string[];
  validation_dates: number[];
  invalidation_conditions: string[];
  missing_fields: string[];
  evidence: EventFieldEvidence[];
  extraction_version: string;
  created_at: number;
  updated_at: number;
}

export interface EventMetricContribution {
  metric: string;
  available: boolean;
  value_bps: number | null;
  score_contribution: number;
  explanation: string;
}

export interface EventMarketAssessment {
  assessment_id: string;
  event_id: string;
  security_code: string;
  as_of_date: string;
  fundamental: { direction: string; impact_bps: number | null; quantifiable: boolean; rationale: string; provenance: string };
  market_opportunity: { price_in_state: string; opportunity: string; price_in_score: number | null; rationale: string; no_trade_directive: string };
  expectation_gap: { structured_impact_bps: number | null; consensus_impact_bps: number | null; gap_bps: number | null; quantifiable: boolean; rationale: string };
  diagnostics: {
    pre_stock_return_bps: number | null;
    pre_benchmark_return_bps: number | null;
    pre_abnormal_return_bps: number | null;
    sector_relative_bps: number | null;
    abnormal_volume_bps: number | null;
    valuation_change_bps: number | null;
    historical_median_post_bps: number | null;
    historical_sample_count: number;
    components: EventMetricContribution[];
  };
  missing_inputs: string[];
  data_versions: unknown;
  created_at: number;
}

export interface EventResearchBundle {
  event: StructuredEvent;
  timeline: Array<{ transition_id: string; event_id: string; from_status: string; to_status: string; reason: string; evidence_id: string | null; transitioned_at: number }>;
  assessment: EventMarketAssessment | null;
  calibration: { ontology_kind: string; sample_count: number; median_post_abnormal_return_bps: number | null; positive_sample_ratio_bps: number | null; data_versions: string[] };
}

export interface EventAnalysisSnapshot {
  job_id: string;
  revision_id: string;
  security_code: string | null;
  running: boolean;
  status: string;
  phase: string;
  progress: number;
  current_item: string;
  estimated_remaining_seconds: number | null;
  recent_logs: string[];
  result: EventResearchBundle | null;
  error: string | null;
  started_at: number;
  updated_at: number;
}

export interface EventAnalysisStartResponse {
  job_id: string;
  started: boolean;
  reused: boolean;
  estimated_seconds: number;
  note: string;
}

export interface AgentConclusionReview {
  task_id: string;
  conclusion_key: string;
  triggering_revision: string;
  trigger_relation: string;
  status: string;
  created_at: number;
  reviewed_at: number | null;
}

export type EntityKind =
  | "legal_entity"
  | "listed_security"
  | "subsidiary"
  | "brand"
  | "person"
  | "product"
  | "industry"
  | "commodity"
  | "region"
  | "policy";

export interface RelatedListedEntity {
  entity_id: string;
  code: string;
  name: string;
  relation_path: string[];
  confidence: number;
  eligible_for_agent: boolean;
}

export interface EntityLinkCandidate {
  entity_id: string;
  canonical_name: string;
  entity_kind: EntityKind;
  listed_code: string | null;
  matched_name_type: string;
  score: number;
  reasons: string[];
  related_listed: RelatedListedEntity[];
}

export interface DocumentEntityLink {
  link_id: string;
  revision_id: string;
  span_start: number;
  span_end: number;
  span_text: string;
  candidates: EntityLinkCandidate[];
  final_entity_id: string | null;
  final_entity_name: string | null;
  final_entity_kind: EntityKind | null;
  listed_code: string | null;
  confidence: number;
  reasons: string[];
  linker_version: string;
  evidence_revision_id: string;
  status: "accepted" | "pending_review" | "rejected";
  proposed_by_model: boolean;
  eligible_for_agent: boolean;
}

export interface EntityLinkReview {
  review_id: number;
  link: DocumentEntityLink;
  proposed_entity_id: string | null;
  decision: string;
  reason: string | null;
  created_at: number;
}

export type SourceAuthority =
  | "regulatory_exchange_government"
  | "company_disclosure"
  | "licensed_media"
  | "aggregator"
  | "social_lead"
  | "unknown";

export interface SourceDocumentSummary {
  source_document_id: string;
  canonical_url: string;
  current_version_id: string | null;
  authority: SourceAuthority;
  authority_name: string;
  is_primary_source: boolean;
  access_status: string;
  failure_kind: string | null;
  failure_message: string | null;
  first_fetched_at: number;
  last_fetched_at: number;
}

export interface SourceScores {
  reliability: number;
  independence: number;
  freshness: number;
  note: string;
}

export interface SourceVersion {
  source_version_id: string;
  source_document_id: string;
  canonical_url: string;
  content_hash: string;
  extracted_hash: string;
  media_type: string;
  title: string | null;
  published_at: number | null;
  fetched_at: number;
  parser_version: string;
  supersedes_version_id: string | null;
  scores: SourceScores;
  authority: SourceAuthority;
  authority_name: string;
  is_primary_source: boolean;
  prompt_injection_detected: boolean;
}

export interface SourceSegment {
  segment_id: string;
  source_version_id: string;
  page_number: number | null;
  paragraph_index: number;
  selector: string | null;
  span_start: number;
  span_end: number;
  text: string;
  text_hash: string;
}

export interface FactEvidence {
  fact_id: string;
  source_version_id: string;
  segment_id: string;
  fact_type: string;
  field_name: string;
  subject: string | null;
  raw_value: string;
  normalized_value: number | null;
  original_unit: string | null;
  normalized_unit: string | null;
  page_number: number | null;
  paragraph_index: number;
  span_start: number;
  span_end: number;
}

export interface SourceDocumentDetail {
  document: SourceDocumentSummary;
  version: SourceVersion | null;
  segments: SourceSegment[];
  facts: FactEvidence[];
  verification_note: string;
}

export interface EvidenceConflict {
  field_name: string;
  values: FactEvidence[];
  note: string;
}

export const getNewsProviderHealth = () =>
  cmd<NewsProviderHealthItem[]>("get_news_provider_health");

export const setNewsProviderEnabled = (providerId: string, enabled: boolean) =>
  cmd<void>("set_news_provider_enabled", { provider_id: providerId, enabled });

export const getNewsArchiveRecent = (limit = 100) =>
  cmd<ArchivedNewsRevision[]>("get_news_archive_recent", { limit });

export const getNewsArchiveRevisions = (documentId: string) =>
  cmd<ArchivedNewsRevision[]>("get_news_archive_revisions", { document_id: documentId });

export const checkNewsArchiveIntegrity = () => cmd<string>("check_news_archive_integrity");

export const getNewsIngestObservations = (providerId: string, limit = 10) =>
  cmd<NewsIngestObservation[]>("get_news_ingest_observations", {
    provider_id: providerId,
    limit,
  });

export const queryNewsCenter = (query: NewsCenterQuery) =>
  cmd<NewsCenterPage>("query_news_center", { query });

export const refreshNewsCenter = (
  sources: string[] = [],
  keyword: string | null = null,
  symbol: string | null = null,
  limit = 100,
) => cmd<NewsRefreshResult>("refresh_news_center", { sources, keyword, symbol, limit });

export const setNewsItemState = (
  documentId: string,
  action: "read" | "pinned" | "favorite" | "ignored",
  value: boolean,
) => cmd<NewsUserState>("set_news_item_state", {
  document_id: documentId,
  action,
  value,
});

export const getNewsEventClusters = (limit = 50) =>
  cmd<NewsEventCluster[]>("get_news_event_clusters", { limit });

export const getNewsEventClusterDetail = (clusterId: string) =>
  cmd<NewsEventClusterDetail>("get_news_event_cluster_detail", { cluster_id: clusterId });

export const mergeNewsEventClusters = (fromClusterId: string, toClusterId: string, reason: string) =>
  cmd<NewsEventClusterDetail>("merge_news_event_clusters", {
    from_cluster_id: fromClusterId,
    to_cluster_id: toClusterId,
    reason,
  });

export const splitNewsEventRevision = (revisionId: string, reason: string) =>
  cmd<NewsEventClusterDetail>("split_news_event_revision", { revision_id: revisionId, reason });

export const startEventAnalysis = (
  revisionId: string,
  securityCode: string | null = null,
  structuredImpactBps: number | null = null,
  consensusImpactBps: number | null = null,
) => cmd<EventAnalysisStartResponse>("event_analysis_start", {
  request: {
    revision_id: revisionId,
    security_code: securityCode,
    structured_impact_bps: structuredImpactBps,
    consensus_impact_bps: consensusImpactBps,
  },
});

export const getEventAnalysisStatus = (jobId: string) =>
  cmd<EventAnalysisSnapshot>("event_analysis_status", { job_id: jobId });

export const cancelEventAnalysis = (jobId: string) =>
  cmd<{ cancelled: boolean }>("event_analysis_cancel", { job_id: jobId });

export type RelationDocumentKind =
  | "annual_report" | "semi_annual_report" | "prospectus" | "investor_relations"
  | "product_manual" | "tender" | "major_contract" | "patent"
  | "regulatory_approval" | "capacity_eia" | "customs_industry" | "other";
export type SupplyRelationType =
  | "supplies" | "customer_of" | "produces" | "consumes" | "won_bid"
  | "contract_with" | "patent_for" | "approved_for" | "capacity_for";
export interface RelationEvidence {
  evidence_id: string; source_version_id: string; segment_id: string; page_number: number | null;
  paragraph_index: number; span_start: number; span_end: number; quote_original: string;
  independent_group: string; polarity: string;
}
export interface RelationValidationCheck { field: string; passed: boolean; detail: string }
export interface RelationCandidate {
  candidate_id: string; run_id: string; source_version_id: string; document_kind: RelationDocumentKind;
  subject_text: string; object_text: string; relation: SupplyRelationType; product_text: string | null;
  amount_text: string | null; share_bps: number | null; report_period: string | null; region: string | null;
  subject_entity_id: string | null; object_entity_id: string | null;
  subject_parent_entity_id: string | null; object_parent_entity_id: string | null;
  disclosure_mode: string; confidence_bps: number; validation_status: string; validation: RelationValidationCheck[];
  review_status: string; confidential: boolean; non_inferable: boolean; candidate_version: number;
  proposed_by_model: boolean; publication_status: string | null; eligible_for_agent: boolean;
  evidence: RelationEvidence[]; created_at: number; updated_at: number;
}
export interface RelationExtractionRun {
  run_id: string; source_version_id: string; document_kind: RelationDocumentKind; extractor_kind: string;
  model_id: string | null; model_version: string | null; schema_version: string; input_hash: string;
  status: string; candidate_count: number; validation_errors: number; started_at: number;
  completed_at: number | null; error: string | null;
}
export interface RelationExtractionRunDetail {
  run: RelationExtractionRun; source_title: string | null; source_url: string;
  candidates: RelationCandidate[]; diagnostics: string[];
}
export interface RelationExtractionSnapshot {
  job_id: string; source_version_id: string; running: boolean; status: string; phase: string;
  progress: number; current_item: string; segments_scanned: number; candidates_found: number;
  validated: number; needs_review: number; estimated_remaining_seconds: number | null;
  recent_logs: string[]; result: RelationExtractionRunDetail | null; error: string | null;
  started_at: number; updated_at: number;
}
export interface RelationReviewPage { items: RelationCandidate[]; total: number; page: number; page_size: number; total_pages: number }
export interface RelationReviewRequest {
  candidate_id: string; decision: "accepted" | "modified" | "rejected" | "confidential" | "non_inferable" | "merge_entity";
  reviewer: string; reason: string; subject_text: string | null; object_text: string | null;
  relation: SupplyRelationType | null; product_text: string | null; merged_entity_id: string | null;
  confidential: boolean; non_inferable: boolean; publish: boolean;
  dataset_split: "train" | "dev" | "test" | null; training_eligible: boolean;
}
export interface RelationPublicationResult { candidate_id: string; publication_id: string | null; projection_key: string | null; status: string; note: string }
export const startRelationExtraction = (sourceVersionId: string, documentKind: RelationDocumentKind) =>
  cmd<{ job_id: string; started: boolean; reused: boolean; estimated_seconds: number; note: string }>("relation_extraction_start", { request: { source_version_id: sourceVersionId, document_kind: documentKind, model_id: null, model_version: null, model_candidates: [] } });
export const getRelationExtractionStatus = (jobId: string) =>
  cmd<RelationExtractionSnapshot>("relation_extraction_status", { job_id: jobId });
export const cancelRelationExtraction = (jobId: string) => cmd<boolean>("relation_extraction_cancel", { job_id: jobId });
export const queryRelationReviews = (status: string, documentKind: RelationDocumentKind | null, minConfidenceBps: number, page: number, pageSize: number) =>
  cmd<RelationReviewPage>("query_relation_reviews", { status, document_kind: documentKind, min_confidence_bps: minConfidenceBps, page, page_size: pageSize });
export const reviewRelationCandidate = (request: RelationReviewRequest) =>
  cmd<RelationPublicationResult>("review_relation_candidate", { request });
export const retractRelationCandidate = (candidateId: string, reason: string) =>
  cmd<RelationPublicationResult>("retract_relation_candidate", { candidate_id: candidateId, reason });

export const getPendingNewsEvidenceReviews = (limit = 50) =>
  cmd<AgentConclusionReview[]>("get_pending_news_evidence_reviews", { limit });

export const resolveNewsEvidenceReview = (
  taskId: string,
  conclusionKey: string,
  triggeringRevision: string,
) => cmd<boolean>("resolve_news_evidence_review", {
  task_id: taskId,
  conclusion_key: conclusionKey,
  triggering_revision: triggeringRevision,
});

export const getNewsEntityLinks = (revisionIds: string[]) =>
  cmd<DocumentEntityLink[]>("get_news_entity_links", { revision_ids: revisionIds });

export const getEntityLinkReviews = (limit = 50) =>
  cmd<EntityLinkReview[]>("get_entity_link_reviews", { limit });

export const resolveEntityLinkReview = (
  linkId: string,
  entityId: string | null,
  accept: boolean,
  reason: string,
) => cmd<boolean>("resolve_entity_link_review", {
  link_id: linkId,
  entity_id: entityId,
  accept,
  reason,
});

export const fetchSourceDocument = (url: string) =>
  cmd<SourceDocumentDetail>("fetch_source_document", { url });

export const getSourceDocuments = (limit = 100) =>
  cmd<SourceDocumentSummary[]>("get_source_documents", { limit });

export const getSourceDocument = (sourceVersionId: string) =>
  cmd<SourceDocumentDetail>("get_source_document", { source_version_id: sourceVersionId });

export const compareSourceEvidence = (sourceVersionIds: string[]) =>
  cmd<EvidenceConflict[]>("compare_source_evidence", { source_version_ids: sourceVersionIds });

// ==================== 分析引擎 ====================

export interface TradePlan {
  action: string;
  entry_price: number | null;
  stop_loss: number | null;
  target_price: number | null;
  position_size: string;
  holding_period: string;
  risk_reward_ratio: number | null;
  max_loss_pct: number | null;
  notes: string;
}

export interface ManualScenario {
  name: string;
  condition: string;
  response: string;
  invalidation: string;
}

export interface ManualCheckpoint {
  phase: string;
  time_window: string;
  observe: string[];
  required_conditions: string[];
  action_if_confirmed: string;
  action_if_failed: string;
  next_checkpoint: string;
}

export interface ManualEvidence {
  label: string;
  value: string;
  source: string;
  as_of: string;
}

/** 仅供人工执行的条件化计划；软件不会据此自动下单。 */
export interface ManualTradingPlan {
  plan_id: string;
  symbol: string;
  name: string;
  generated_at: string;
  data_as_of: string;
  market_regime: string;
  thesis: string;
  counter_thesis: string;
  confidence: number;
  risk_budget_pct: number;
  entry_zone_low: number;
  entry_zone_high: number;
  stop_loss: number;
  target_price: number;
  risk_reward_ratio: number;
  stop_basis: string;
  target_basis: string;
  expected_holding_period: string;
  position_guidance: string;
  scenarios: ManualScenario[];
  checkpoints: ManualCheckpoint[];
  invalidation_conditions: string[];
  review_triggers: string[];
  constraints: string[];
  evidence: ManualEvidence[];
  disclaimer: string;
}

export interface TrendInfo {
  direction: string;
  strength: number;
  stage: string;
  ma_arrangement: string;
  ma_scores: Record<string, number>;
  trendline: unknown;
  signals: string[];
}

export interface PatternInfo {
  name: string;
  direction: string;
  confidence: number;
  status: string;
  target_price: number | null;
  key_levels: Record<string, number>;
  description: string;
}

export interface VolumePriceInfo {
  pattern: string;
  direction: string;
  confidence: number;
  volume_ratio: number;
  turnover: number;
  obv_trend: string;
  signals: string[];
  description: string;
}

export interface BreakoutInfo {
  system: string;
  signal: string;
  breakout_price: number | null;
  current_n: number | null;
  stop_loss: number | null;
  entry_price: number | null;
  position_units: number;
  exit_price: number | null;
  channel_high: number | null;
  channel_low: number | null;
  next_add_price: number | null;
  signals: string[];
  description: string;
}

export interface CupHandle {
  pattern: string;
  cup_high: number;
  cup_low: number;
  handle_high: number;
  handle_low: number;
  cup_depth: number;
  handle_depth: number;
  breakout: boolean;
  buy_point: number | null;
  target: number | null;
}

export interface CanslimInfo {
  c_score: number;
  a_score: number;
  n_score: number;
  s_score: number;
  l_score: number;
  i_score: number;
  m_score: number;
  total: number;
  grade: string;
  signals: string[];
  cup_handle: CupHandle | null;
  description: string;
}

/** analyze 返回(与旧版 signal_to_dict 同形,见 fixtures/golden 下 outputs/signal) */
export interface SignalJson {
  action: string;
  score: number;
  confidence: number;
  risk_level: string;
  signal_strength: string;
  plain_summary: string;
  trade_plan: TradePlan;
  manual_plan?: ManualTradingPlan | null;
  module_scores: Record<string, number>;
  buy_signals: string[];
  sell_signals: string[];
  risk_warnings: string[];
  key_levels: Record<string, number>;
  description: string;
  trend: TrendInfo;
  patterns: PatternInfo[];
  volume_price: VolumePriceInfo;
  breakouts: BreakoutInfo[];
  canslim: CanslimInfo;
  optimized_action: string;
  original_action: string;
  position_advice: string;
  risk_notes: string[];
  risk_reward: number | null;
}

// ---- 缠论 ----

export interface ChanFractal {
  type: "top" | "bottom";
  type_name: string;
  price: number;
  date: string;
}

export interface ChanStroke {
  direction: "up" | "down";
  start_price: number;
  end_price: number;
  start_date: string;
  end_date: string;
  macd_area: number;
  has_divergence: boolean;
}

export interface ChanZhongshu {
  start_date: string;
  end_date: string;
  zg: number;
  zd: number;
  zz: number;
  is_broken: boolean;
  break_direction: string | null;
}

export interface ChanSignal {
  type: string;
  type_name: string;
  price: number;
  date: string;
  confidence: number;
  description: string;
}

/** 以下 chart_* 为后端预生成的 ECharts payload,可直接用于 markPoint/markArea/markLine */
export interface ChartSignalItem {
  coord: [string, number];
  symbol: string;
  symbolRotate?: number;
  symbolSize?: number;
  itemStyle?: Record<string, unknown>;
  label?: Record<string, unknown>;
  type_name: string;
  date: string;
  price: number;
  confidence: number;
  description: string;
}

export interface ChartFractalItem {
  coord: [string, number];
  symbol: string;
  symbolSize?: number;
  itemStyle?: Record<string, unknown>;
  fractal_type: string;
}

export interface ChartZhongshuItem {
  xAxis: [string, string];
  yAxis: [number, number];
  itemStyle?: Record<string, unknown>;
  broken: boolean;
  break_direction: string | null;
  zg: number;
  zd: number;
}

export interface ChartStrokeItem {
  coords: [[string, number], [string, number]];
  lineStyle?: Record<string, unknown>;
  has_divergence: boolean;
}

/** chanlun_daily 返回(与旧版 daily_result_to_dict 同形) */
export interface ChanlunDailyJson {
  kline_count: number;
  merged_count: number;
  fractal_count: number;
  stroke_count: number;
  zhongshu_count: number;
  fractals: ChanFractal[];
  strokes: ChanStroke[];
  zhongshus: ChanZhongshu[];
  signals: ChanSignal[];
  current_state: string;
  summary: string;
  description: string;
  chart_signals: ChartSignalItem[];
  chart_fractals: ChartFractalItem[];
  chart_zhongshus: ChartZhongshuItem[];
  chart_strokes: ChartStrokeItem[];
}

/** chanlun_minute 返回(字段以后端为准,前端仅展示概要) */
export interface ChanlunMinuteJson {
  signals?: ChanSignal[];
  summary?: string;
  description?: string;
  [key: string]: unknown;
}

export const analyze = (symbol: string, period: KlinePeriod) =>
  cmd<SignalJson>("analyze", { symbol, period });
export const chanlunDaily = (symbol: string, period: KlinePeriod, count: number) =>
  cmd<ChanlunDailyJson>("chanlun_daily", { symbol, period, count });
export const chanlunMinute = (symbol: string) => cmd<ChanlunMinuteJson>("chanlun_minute", { symbol });

// ==================== 基本面 / 估值 ====================

export interface FundamentalsProfile {
  name: string | null;
  industry: string | null;
  listing_date: string | null;
  total_shares: number | null;
  float_shares: number | null;
}

export interface FundamentalsPeriod {
  period_end: string | null;
  report_type: string | null;
  announced_date: string | null;
}

export interface Dupont {
  net_margin: number | null;
  asset_turnover: number | null;
  equity_multiplier: number | null;
}

/** 比率类字段单位均为百分数(如 18.5 表示 18.5%),金额单位为元 */
export interface FundamentalsMetrics {
  revenue: number | null;
  net_profit: number | null;
  revenue_yoy: number | null;
  profit_yoy: number | null;
  gross_margin: number | null;
  operating_margin: number | null;
  net_margin: number | null;
  roe: number | null;
  roa: number | null;
  roic: number | null;
  dupont: Dupont | null;
  fcf: number | null;
  cfo_to_net_income: number | null;
  ccc: number | null;
  current_ratio: number | null;
  debt_ratio: number | null;
}

export interface GrowthPoint {
  period_end: string | null;
  revenue: number | null;
  net_profit: number | null;
  revenue_yoy: number | null;
  profit_yoy: number | null;
  gross_margin: number | null;
  roe: number | null;
}

export interface PiotroskiCriterion {
  name: string;
  passed: boolean | null;
}

export interface PiotroskiScore {
  score: number | null;
  criteria: PiotroskiCriterion[];
}

export interface AltmanScore {
  z_classic: number | null;
  z_emerging: number | null;
  zone: string | null;
}

export interface BeneishScore {
  m_score: number | null;
  interpretation: string | null;
}

export interface FundamentalsScores {
  piotroski: PiotroskiScore | null;
  altman: AltmanScore | null;
  beneish: BeneishScore | null;
}

export interface FundamentalAnomaly {
  kind: string;
  severity: string;
  explanation: string;
  evidence: string | null;
}

export interface DividendPlan {
  year: string | number | null;
  plan: string | null;
}

/** get_fundamentals 返回;各 section 可能整体缺失,missing 列出不可用部分 */
export interface FundamentalsJson {
  profile: FundamentalsProfile | null;
  latest_period: FundamentalsPeriod | null;
  metrics: FundamentalsMetrics | null;
  growth_series: GrowthPoint[] | null;
  scores: FundamentalsScores | null;
  anomalies: FundamentalAnomaly[] | null;
  dividends: DividendPlan[] | null;
  missing: string[] | null;
}

export interface ValuationCurrent {
  price: number | null;
  pe_ttm: number | null;
  pe_static: number | null;
  pb: number | null;
  ps_ttm: number | null;
  pcf: number | null;
  market_cap: number | null;
}

/** 分位值域 [0,100],days 为历史样本天数 */
export interface ValuationPercentile {
  pe_ttm_pct: number | null;
  pb_pct: number | null;
  ps_pct: number | null;
  days: number | null;
}

/** wacc/growth 为小数(0.09 = 9%),values[i][j] 对应 wacc[i] × growth[j] */
export interface DcfSensitivity {
  wacc: number[] | null;
  growth: number[] | null;
  values: (number | null)[][] | null;
}

export interface DcfValuation {
  bear: number | null;
  base: number | null;
  bull: number | null;
  sensitivity: DcfSensitivity | null;
  caveat: string | null;
}

export interface ValuationHistoryPoint {
  date: string;
  pe_ttm: number | null;
  pb: number | null;
}

/** get_valuation 返回;各 section 可能整体缺失 */
export interface ValuationJson {
  parameter_snapshot_id: string;
  current: ValuationCurrent | null;
  percentile: ValuationPercentile | null;
  dcf: DcfValuation | null;
  history_series: ValuationHistoryPoint[] | null;
}

export const getFundamentals = (symbol: string) =>
  cmd<FundamentalsJson>("get_fundamentals", { symbol });
export const getValuation = (symbol: string) => cmd<ValuationJson>("get_valuation", { symbol });

export type DriverValueOrigin =
  | "historical_fact"
  | "management_guidance"
  | "market_consensus"
  | "user_assumption"
  | "agent_assumption"
  | "industry_prior";

export interface DriverEvidence {
  source_version_id: string;
  source_name: string;
  report_period: string | null;
  announced_date: string | null;
  locator: string;
  unit: string;
  confidence_low: number;
  confidence_high: number;
}

export interface DriverParameter {
  id: string;
  name: string;
  category: string;
  value: number | null;
  low: number | null;
  high: number | null;
  unit: string;
  origin: DriverValueOrigin;
  report_period: string | null;
  confidence: number;
  evidence: DriverEvidence[];
  note: string;
}

export interface DriverFormulaNode {
  id: string;
  name: string;
  formula: string;
  parameter_ids: string[];
  unit: string;
  historical_value: number | null;
  forecast_low: number | null;
  forecast_base: number | null;
  forecast_high: number | null;
}

export interface DriverBranch {
  id: string;
  label: string;
  dimension: string;
  formula: string;
  status: string;
  parameter_ids: string[];
  children: DriverBranch[];
}

export interface DriverScenario {
  scenario: "bear" | "base" | "bull" | string;
  revenue: number;
  gross_profit: number;
  operating_profit: number;
  tax: number;
  minority_profit: number;
  parent_net_profit: number;
  eps: number | null;
  operating_cash_flow: number;
  capex: number;
  free_cash_flow: number;
}

export interface DriverSensitivityCell {
  revenue_growth: number;
  gross_margin: number;
  eps: number | null;
  free_cash_flow: number;
}

export interface EarningsDriverTree {
  snapshot_id: string;
  parameter_snapshot_id: string;
  model_version: string;
  symbol: string;
  company_name: string | null;
  industry: string | null;
  industry_template: string;
  industry_template_label: string;
  revenue_formula: string;
  cost_formula: string;
  report_period: string | null;
  knowledge_time: number;
  golden_template_reviewed: boolean;
  parameters: DriverParameter[];
  revenue_tree: DriverBranch;
  cost_tree: DriverBranch;
  formula_nodes: DriverFormulaNode[];
  scenarios: DriverScenario[];
  sensitivity: DriverSensitivityCell[];
  monte_carlo: {
    samples: number;
    seed: number;
    eps_p10: number | null;
    eps_p50: number | null;
    eps_p90: number | null;
    fcf_p10: number;
    fcf_p50: number;
    fcf_p90: number;
    method: string;
  } | null;
  implied_assumption: {
    current_price: number | null;
    implied_fcf_growth: number | null;
    search_low: number;
    search_high: number;
    wacc: number;
    terminal_growth: number;
    explanation: string;
  };
  quality: {
    exact_eps_available: boolean;
    model_completeness: number;
    missing_core_drivers: string[];
    refusal_reason: string | null;
    warnings: string[];
  };
  provenance_legend: Record<string, string>;
}

export interface DriverShockInput {
  kind: string;
  magnitude: number;
  lag_months: number;
  pass_through?: number | null;
  evidence_version_id?: string | null;
  note?: string | null;
}

export interface EarningsShockBridge {
  base_snapshot_id: string;
  shocked_snapshot_id: string;
  shocks: DriverShockInput[];
  base: DriverScenario | null;
  shocked: DriverScenario | null;
  delta: {
    revenue: number;
    gross_profit: number;
    operating_profit: number;
    parent_net_profit: number;
    eps: number | null;
    operating_cash_flow: number;
    free_cash_flow: number;
  } | null;
  changed_parameters: DriverParameter[];
  warnings: string[];
}

export const getEarningsDriverTree = (symbol: string) =>
  cmd<EarningsDriverTree>("get_earnings_driver_tree", { symbol });
export const getEarningsDriverSnapshot = (snapshotId: string) =>
  cmd<EarningsDriverTree>("get_earnings_driver_snapshot", { snapshot_id: snapshotId });
export const runEarningsDriverShock = (symbol: string, shocks: DriverShockInput[]) =>
  cmd<EarningsShockBridge>("run_earnings_driver_shock", { symbol, shocks });

// ==================== 扫描 ====================

export interface ScanResultItem {
  symbol: string;
  name: string;
  score: number;
  action: string;
  confidence: number;
}

export interface ScanStatus {
  running: boolean;
  done: number;
  total: number;
  current_symbol: string;
  results: ScanResultItem[];
}

export const scanStart = () => cmd<{ started: boolean }>("scan_start");
export const scanStatus = () => cmd<ScanStatus>("scan_status");
export const scanCancel = () => cmd<{ cancelled: boolean }>("scan_cancel");

// ==================== 自选股 ====================

export interface WatchlistItem {
  group_name: string;
  code: string;
  name?: string | null;
  added_at: string;
  pinned: boolean;
}

export const watchlistList = () => cmd<WatchlistItem[]>("watchlist_list");
export const watchlistAdd = (code: string, group: string) =>
  cmd<unknown>("watchlist_add", { code, group });
export const watchlistRemove = (code: string, group: string) =>
  cmd<unknown>("watchlist_remove", { code, group });
export const watchlistPin = (code: string, group: string, pinned: boolean) =>
  cmd<unknown>("watchlist_pin", { code, group, pinned });

// ==================== 设置 / MiniMax / 缓存 ====================

export interface ModelQuotaStatus {
  model_name: string;
  start_time: number | null;
  end_time: number | null;
  remains_time: number | null;
  current_interval_total_count: number | null;
  current_interval_usage_count: number | null;
  current_weekly_total_count: number | null;
  current_weekly_usage_count: number | null;
  weekly_start_time: number | null;
  weekly_end_time: number | null;
  weekly_remains_time: number | null;
  current_interval_status: number | null;
  current_interval_remaining_percent: number | null;
  current_weekly_status: number | null;
  current_weekly_remaining_percent: number | null;
  [key: string]: unknown;
}

/** 官方 token_plan/remains 返回的按模型 5 小时/周窗口快照。 */
export interface QuotaStatus {
  models: ModelQuotaStatus[];
  fetched_at: number;
}

export interface MinimaxStatus {
  has_key: boolean;
  region?: string;
  api_host?: string;
  model?: string;
  quota?: QuotaStatus;
  available_models?: AvailableMinimaxModel[];
  model_routing?: AgentModelRoutingSettings;
}

export interface AvailableMinimaxModel {
  id: string;
  object: string;
  created: number;
  owned_by: string;
}

export interface AgentModelRoutingSettings {
  coordinator_model: string;
  fast_model: string;
  deep_model: string;
  verifier_model: string;
  multi_agent_enabled: boolean;
  max_parallel_agents: number;
}

/** minimax_set_key 返回的服务信息(key 永不回显) */
export interface ServiceInfo {
  has_key?: boolean;
  region?: string;
  api_host?: string;
  model?: string;
  quota?: QuotaStatus;
  [key: string]: unknown;
}

export interface CacheStats {
  kline_bytes: number;
  sqlite_bytes: number;
  tool_cache_bytes: number;
  chat_bytes: number;
  total_bytes: number;
  disk_free_bytes?: number;
}

export interface CacheCleanupResult {
  freed_bytes: number;
  removed_files: number;
}

export const minimaxSetKey = (key: string) => cmd<ServiceInfo>("minimax_set_key", { key });
export const minimaxStatus = () => cmd<MinimaxStatus>("minimax_status");
export const minimaxQuota = () => cmd<QuotaStatus>("minimax_quota");
export const settingsGetAgentModelRouting = () =>
  cmd<AgentModelRoutingSettings>("settings_get_agent_model_routing");
export const settingsSetAgentModelRouting = (settings: AgentModelRoutingSettings) =>
  cmd<AgentModelRoutingSettings>("settings_set_agent_model_routing", { settings });
export const cacheStats = () => cmd<CacheStats>("cache_stats");
export const cacheCleanup = (targetMb: number) =>
  // backend uses #[tauri::command(rename_all = "snake_case")] — keys must be snake_case
  cmd<CacheCleanupResult>("cache_cleanup", { target_mb: targetMb });
export const getDataDir = () => cmd<string>("get_data_dir");
export const setDataDir = (path: string) => cmd<unknown>("set_data_dir", { path });

// ---- 数据源凭证与代理(可选 provider;状态只回布尔,凭证本体绝不回显) ----

/** settings_get_provider_status 返回:各项是否已配置 */
export interface ProviderStatus {
  tushare_token: boolean;
  iwencai_key: boolean;
  jq_user: boolean;
  jq_pwd: boolean;
  sec_user_agent: boolean;
  socks5: boolean;
}

/** settings_set_provider_credentials 入参;空串/不传 = 清除该项 */
export interface ProviderCredentials {
  tushare_token?: string;
  iwencai_key?: string;
  jq_user?: string;
  jq_pwd?: string;
  socks5?: string;
}

export interface SetProviderCredentialsResult {
  status: ProviderStatus;
  message: string;
}

export const settingsGetProviderStatus = () =>
  cmd<ProviderStatus>("settings_get_provider_status");
export const settingsSetProviderCredentials = (creds: ProviderCredentials) =>
  cmd<SetProviderCredentialsResult>("settings_set_provider_credentials", { ...creds });

// ==================== AI Agent ====================

/** 提取后端统一错误结构的 kind(如 "no_key") */
export function errKind(e: unknown): string | null {
  if (e && typeof e === "object") {
    const k = (e as Record<string, unknown>).kind;
    if (typeof k === "string") return k;
  }
  return null;
}

export type AgentTaskStatus =
  | "queued"
  | "starting"
  | "running"
  | "waiting"
  | "suspended"
  | "completed"
  | "failed"
  | "cancelled";

export interface AgentTask {
  id: string;
  conversation_id: string;
  kind: string;
  status: AgentTaskStatus | string;
  /** unix 秒 */
  created_at: number;
  /** unix 秒 */
  updated_at: number;
  prompt: string | null;
  research_mode: string | null;
  reasoning_depth: string | null;
  model: string | null;
  round: number;
  max_rounds: number | null;
  /** null 表示使用系统默认的完整工具集。 */
  enabled_tools: string[] | null;
  auto_resume_on_quota: boolean;
  specialist_count: number;
  evidence_count: number;
  context_compactions: number;
  multi_agent_reviewed: boolean;
  last_error: string | null;
}

export interface AgentConversation {
  id: string;
  title: string | null;
  /** unix 秒 */
  created_at: number;
}

export interface AgentHistoryToolCall {
  id: string | null;
  name: string | null;
  arguments: string | null;
}

/** Rust 已规范化的历史消息；content 永远是字符串。 */
export interface AgentMessage {
  id: string;
  role: string;
  content: string;
  tool_calls: AgentHistoryToolCall[];
  tool_call_id: string | null;
  created_at: number;
  malformed: boolean;
}

export interface AgentEvidence {
  evidence_id: string;
  tool: string;
  cache_key: string;
  source: string;
  fetched_at: string;
  tool_version: string;
  data_version: string;
  source_tier: "primary" | "provider" | "engine" | "discovery_only" | string;
  freshness: "fresh" | "stale" | "expired" | "unknown" | string;
  blocking: boolean;
  fields: AgentEvidenceField[];
}

export interface AgentEvidenceField {
  evidence_id: string;
  field_path: string;
  value: unknown;
  unit: string | null;
  currency: string | null;
  as_of: string;
  freshness: string;
  source_tier: string;
  blocking: boolean;
  calculation_id: string | null;
}

export type AgentClaimType =
  | "fact"
  | "calculation"
  | "external"
  | "inference"
  | "assumption"
  | "unknown";

export interface AgentResearchClaim {
  claim_id: string;
  text: string;
  claim_type: AgentClaimType;
  evidence_ids: string[];
  calculation_ids: string[];
  as_of: string | null;
  confidence: "high" | "medium" | "low" | "blocked";
  assumptions: string[];
  counter_evidence: string[];
  invalidation: string[];
  unknowns: string[];
}

export interface AgentVerificationFinding {
  code: string;
  severity: "error" | "warning";
  claim_id: string | null;
  message: string;
}

export interface AgentResearchReport {
  schema_version: string;
  as_of: string | null;
  confidence: "high" | "medium" | "low" | "blocked";
  claims: AgentResearchClaim[];
  calculations: Array<{
    calculation_id: string;
    tool: string;
    field_path: string;
    value: unknown;
    unit: string | null;
    data_version: string;
  }>;
  assumptions: string[];
  counter_evidence: string[];
  invalidation: string[];
  unknowns: string[];
  verification: {
    status: "passed" | "failed" | "not_applicable";
    verifier_version: string;
    verified_at: number;
    findings: AgentVerificationFinding[];
  };
}

export interface AgentReport {
  task_id: string;
  answer: string;
  conclusions: unknown;
  evidence: AgentEvidence[];
  generated_at: number;
  /** Optional only for reports saved by pre-v5.1 builds. */
  research?: AgentResearchReport;
}

export interface AgentToolWorkItem {
  label: string;
  stage: string;
}

export interface AgentToolProgressDetail {
  completed: number;
  total: number;
  succeeded: number;
  failed: number;
  skipped: number;
  retries: number;
  cache_hits: number;
  records: number;
  active: AgentToolWorkItem[];
  recent_errors: string[];
}

export type AgentEvent =
  | {
      type: "progress";
      phase: "preparing" | "reasoning" | "tools" | "synthesizing" | string;
      message: string;
      round: number;
      max_rounds: number;
      completed: number | null;
      total: number | null;
    }
  | {
      type: "context_compacted";
      before_chars: number;
      after_chars: number;
      retained_messages: number;
    }
  | { type: "text_delta"; text: string }
  | { type: "text_reset"; message: string }
  | {
      type: "tool_call_started";
      call_id: string;
      name: string;
      args: unknown;
      position: number;
      total: number;
      estimated_ms: number;
    }
  | {
      type: "tool_call_progress";
      call_id: string;
      name: string;
      elapsed_ms: number;
      estimated_ms: number;
      stage: string;
      detail?: AgentToolProgressDetail;
    }
  | {
      type: "tool_call_finished";
      call_id: string;
      name: string;
      cache_key: string;
      elapsed_ms: number;
      success: boolean;
      source: string | null;
      fetched_at: string | null;
      error: string | null;
    }
  | {
      type: "suspended";
      reason: { kind: "quota_exhausted"; reset_at_unix: number | null };
    }
  | { type: "completed"; report: AgentReport }
  | { type: "failed"; error: string };

export interface AgentStreamEnvelope {
  run_id: string;
  conversation_id: string;
  seq: number;
  event: AgentEvent;
}

export type AgentResearchMode = "quick" | "deep" | "plan";
export type AgentReasoningDepth = "standard" | "deep" | "maximum";
export interface AgentRunOptions {
  research_mode: AgentResearchMode;
  reasoning_depth: AgentReasoningDepth;
  /** null = all current and future tools; [] = text only. */
  enabled_tools: string[] | null;
  auto_resume_on_quota: boolean;
}

function agentChannel(handler: (message: AgentStreamEnvelope) => void) {
  const channel = new Channel<AgentStreamEnvelope>();
  channel.onmessage = handler;
  return channel;
}

export const agentAsk = (
  question: string,
  conversationId: string | null,
  onEvent: (message: AgentStreamEnvelope) => void,
  options?: AgentRunOptions,
) =>
  cmd<{ task_id: string; conversation_id: string }>("agent_ask", {
    question,
    conversation_id: conversationId,
    options,
    on_event: agentChannel(onEvent),
  });
export const agentResume = (
  taskId: string,
  onEvent: (message: AgentStreamEnvelope) => void,
) =>
  cmd<{ resumed: boolean }>("agent_resume", {
    task_id: taskId,
    on_event: agentChannel(onEvent),
  });
export const agentCancel = (taskId: string) =>
  cmd<{ cancelled: boolean }>("agent_cancel", { task_id: taskId });
export const agentTasks = () => cmd<AgentTask[]>("agent_tasks");
export const agentConversations = () => cmd<AgentConversation[]>("agent_conversations");
export const agentConversationLoad = (conversationId: string) =>
  cmd<AgentMessage[]>("agent_conversation_load", { conversation_id: conversationId });
export const agentConversationDelete = (conversationId: string) =>
  cmd<{ cancelled: boolean }>("agent_conversation_delete", { conversation_id: conversationId });

// ==================== 深度分析(图谱) ====================

/** graph_subgraph 节点:kind 为 company/product/segment/material/commodity/industry/region/policy */
export interface GraphNode {
  id: string;
  kind: string;
  name: string;
  code?: string | null;
  meta?: Record<string, unknown>;
}

/** graph_subgraph 边:relation 为 supplies/customer_of/competes/substitutes/exposed_to/belongs_to/produces/consumes */
export interface GraphEdge {
  id?: number | null;
  src: string;
  dst: string;
  relation: string;
  weight: number;
  source_name: string;
  source_url: string;
  confidence: number;
  valid_from?: number;
  valid_to?: number | null;
}

export interface SubgraphResult {
  center: string;
  hops: number;
  coverage: "identity_only" | "sourced_relations";
  coverage_note: string;
  nodes: GraphNode[];
  edges: GraphEdge[];
}

export interface GraphSnapshotEdge {
  revision_id: string;
  identity_id: string;
  revision_no: number;
  src: string;
  original_src: string;
  dst: string;
  original_dst: string;
  relation: string;
  product_scope?: string | null;
  region_scope?: string | null;
  weight: number;
  disclosed_share?: number | null;
  confidence: number;
  effective_confidence: number;
  source_type: string;
  source_name: string;
  source_url: string;
  evidence_version: string;
  status: string;
  valid_from: number;
  valid_to?: number | null;
  observed_at: number;
  recorded_at: number;
  revalidate_after: number;
}

export interface GraphSnapshot {
  snapshot_id: string;
  business_time: number;
  knowledge_time: number;
  center?: string | null;
  hops?: number;
  nodes: GraphNode[];
  edges: GraphSnapshotEdge[];
  revision_ids: string[];
  merge_ids: string[];
  stale_count: number;
  excluded_count: number;
}

export interface GraphEdgeRevision {
  revision_id: string;
  identity_id: string;
  revision_no: number;
  src: string;
  dst: string;
  relation: string;
  product_scope?: string | null;
  region_scope?: string | null;
  weight: number;
  confidence: number;
  disclosed_share?: number | null;
  source_type: string;
  source_name: string;
  source_url: string;
  evidence_version: string;
  status: string;
  valid_from: number;
  valid_to?: number | null;
  observed_at: number;
  recorded_at: number;
  superseded_at?: number | null;
  revalidate_after: number;
  decay_half_life_days: number;
  supersedes_revision_id?: string | null;
  metadata: Record<string, unknown> | null;
}

export interface GraphHistoryBounds {
  business_min: number;
  business_max: number;
  knowledge_min: number;
  knowledge_max: number;
  revision_count: number;
  revalidation_due_count: number;
}

export interface GraphSnapshotDiff {
  left_snapshot_id: string;
  right_snapshot_id: string;
  added_revision_ids: string[];
  removed_revision_ids: string[];
  changed_identity_ids: string[];
}

/** supply_chain_shock 单条传导结果 */
export interface ShockEntry {
  node_id: string;
  code: string | null;
  name: string;
  direction: string;
  hop: number;
  logic_chain: string;
  expected_lag_days: number | null;
  magnitude_estimate_pct: number | null;
  confidence: number;
  provenance: { source: string; url: string }[];
}

export interface ShockJson {
  event_title: string;
  subject: { id: string; name: string; kind: string };
  summary: string;
  primary_benefit: ShockEntry[];
  primary_harm: ShockEntry[];
  secondary_benefit: ShockEntry[];
  secondary_harm: ShockEntry[];
  potential: ShockEntry[];
  disclaimer: string;
}

/** relationship_graph 单条相关边 */
export interface RelationshipEdge {
  pair: [string, string];
  pearson: number;
  best_lag: number;
  lag_corr: number;
  p_value: number | null;
  significant: boolean;
  leader: string | null;
}

export interface RelationshipGraph {
  window_days: number;
  aligned_bars: number;
  period: { start: string | null; end: string | null };
  nodes: { symbol: string }[];
  edges: RelationshipEdge[];
  matrix: { labels: string[]; pearson: number[][] };
  method: string;
  note: string;
  errors: string[];
}

export const graphSubgraph = (symbolOrNode: string, hops?: number) =>
  cmd<SubgraphResult>("graph_subgraph", { symbol_or_node: symbolOrNode, hops });
export const graphAsOf = (businessTime: number, knowledgeTime: number, symbolOrNode?: string, hops?: number) =>
  cmd<GraphSnapshot>("graph_as_of", {
    business_time: businessTime,
    knowledge_time: knowledgeTime,
    symbol_or_node: symbolOrNode,
    hops,
  });
export const graphHistoryBounds = () => cmd<GraphHistoryBounds>("graph_history_bounds");
export const graphEdgeTimeline = (identityId: string) =>
  cmd<GraphEdgeRevision[]>("graph_edge_timeline", { identity_id: identityId });
export const graphSnapshotGet = (snapshotId: string) =>
  cmd<GraphSnapshot | null>("graph_snapshot_get", { snapshot_id: snapshotId });
export const graphSnapshotDiff = (
  leftBusinessTime: number,
  leftKnowledgeTime: number,
  rightBusinessTime: number,
  rightKnowledgeTime: number,
) => cmd<GraphSnapshotDiff>("graph_snapshot_diff", {
  left_business_time: leftBusinessTime,
  left_knowledge_time: leftKnowledgeTime,
  right_business_time: rightBusinessTime,
  right_knowledge_time: rightKnowledgeTime,
});
export const supplyChainShock = (subject: string, direction: "up" | "down", magnitudePct?: number) =>
  cmd<ShockJson>("supply_chain_shock", { subject, direction, magnitude_pct: magnitudePct });
export const relationshipGraph = (symbols: string[], windowDays?: number) =>
  cmd<RelationshipGraph>("relationship_graph", { symbols, window_days: windowDays });

// ==================== 可复现量化研究工作台 ====================

export type QuantMetric =
  | "pearson"
  | "spearman"
  | "kendall"
  | "distance_correlation"
  | "mutual_information"
  | "lead_lag"
  | "granger";

export interface QuantResearchConfig {
  symbols: string[];
  metric: QuantMetric;
  value_mode: "price_level" | "arithmetic_return" | "log_return";
  frequency: "daily" | "weekly" | "monthly";
  start_date: string | null;
  end_date: string | null;
  adjust: "qfq" | "hfq" | "none";
  lookback_bars: number;
  missing_policy: "drop" | "forward_fill" | "zero";
  rolling_window: number;
  max_lag: number;
  controls: string[];
  bootstrap_reps: number;
  permutation_reps: number;
  alpha: number;
  fdr_method: "benjamini_hochberg" | "bonferroni" | "none";
  max_pairs: number;
  max_observations_per_pair: number;
  seed: number;
  oos_ratio: number;
}

export interface QuantStabilitySlice {
  group: string;
  label: string;
  effect: number;
  effective_n: number;
}

export interface QuantStabilitySummary {
  slice_count: number;
  same_direction_rate: number | null;
  min_effect: number | null;
  max_effect: number | null;
  train_effect: number | null;
  out_of_sample_effect: number | null;
  outlier_robust_effect: number | null;
  assessment: string;
}

export interface QuantPairInference {
  left: string;
  right: string;
  directed: boolean;
  effect: number;
  effect_name: string;
  confidence_low: number | null;
  confidence_high: number | null;
  confidence_method: string;
  p_value: number | null;
  p_value_method: string;
  adjusted_p_value: number | null;
  significant_raw: boolean | null;
  significant_after_correction: boolean | null;
  effective_n: number;
  best_lag: number | null;
  controls_used: string[];
  stability: QuantStabilitySummary;
  stability_slices: QuantStabilitySlice[];
  interpretation: string;
  conclusion: string;
  warnings: string[];
}

export interface QuantResearchSnapshot {
  snapshot_id: string;
  function_version: string;
  created_at: number;
  config: QuantResearchConfig;
  data_versions: Record<string, string>;
  budget: {
    requested_pairs: number;
    executed_pairs: number;
    pair_sampling: boolean;
    max_observations_per_pair: number;
    estimated_operations: number;
    complexity: string;
    explanation: string;
  };
  results: QuantPairInference[];
  warnings: string[];
  causality_boundary: string;
}

export interface QuantResearchJob {
  job_id: string;
  running: boolean;
  status: "running" | "completed" | "cancelled" | "failed" | string;
  phase: string;
  progress: number;
  done_pairs: number;
  total_pairs: number;
  current_pair: [string, string] | null;
  effective_observations: number;
  fetched_series: number;
  total_series: number;
  estimated_remaining_seconds: number | null;
  recent_logs: string[];
  result: QuantResearchSnapshot | null;
  error: string | null;
  started_at: number;
  updated_at: number;
}

export interface QuantSnapshotListItem {
  snapshot_id: string;
  function_version: string;
  metric: string;
  symbols: string[];
  data_versions: Record<string, string>;
  config: QuantResearchConfig;
  created_at: number;
}

export const quantResearchStart = (config: QuantResearchConfig) =>
  cmd<QuantResearchJob>("quant_research_start", { config });
export const quantResearchStatus = (jobId?: string | null) =>
  cmd<QuantResearchJob | null>("quant_research_status", { job_id: jobId ?? null });
export const quantResearchCancel = (jobId: string) =>
  cmd<boolean>("quant_research_cancel", { job_id: jobId });
export const quantResearchSnapshotGet = (snapshotId: string) =>
  cmd<QuantResearchSnapshot | null>("quant_research_snapshot_get", { snapshot_id: snapshotId });
export const quantResearchSnapshotList = (limit = 20) =>
  cmd<QuantSnapshotListItem[]>("quant_research_snapshot_list", { limit });

// ==================== 东财数据中心 ====================

/** 数据中心统一返回:{rows, count, source, fetched_at(RFC3339)} */
export interface DcResult<T> {
  rows: T[];
  count: number;
  source: string;
  fetched_at: string;
}

/** 涨停统计:近 days 天内涨停 ct 次 */
export interface LimitStat {
  days: number;
  ct: number;
}

/** get_zt_pool 行(价格/金额:元,比率:%) */
export interface ZtPoolRow {
  code: string;
  name: string;
  price: number | null;
  pct: number | null;
  amount: number | null;
  float_market_cap: number | null;
  total_market_cap: number | null;
  turnover: number | null;
  limit_times: number | null;
  first_lock_time: string;
  last_lock_time: string;
  lock_fund: number | null;
  break_times: number | null;
  limit_stat: LimitStat | null;
  industry: string;
}

/** get_billboard 行(金额:元,比率:%) */
export interface BillboardRow {
  code: string;
  secucode: string;
  name: string;
  trade_date: string | null;
  close_price: number | null;
  change_rate: number | null;
  net_amt: number | null;
  buy_amt: number | null;
  sell_amt: number | null;
  deal_amt: number | null;
  accum_amount: number | null;
  deal_net_ratio: number | null;
  deal_amount_ratio: number | null;
  turnover_rate: number | null;
  free_market_cap: number | null;
  explanation: string;
  d1_change: number | null;
  d2_change: number | null;
  d5_change: number | null;
  d10_change: number | null;
}

/** get_margin_daily 行(余额/买入额单位:亿元,字段以 _yi 结尾) */
export interface MarginDailyRow {
  statistics_date: string | null;
  fin_balance_yi: number | null;
  loan_balance_yi: number | null;
  margin_balance_yi: number | null;
  fin_buy_amt_yi: number | null;
  loan_sell_amt_yi: number | null;
  security_org_num: number | null;
  operatedept_num: number | null;
  personal_investor_num: number | null;
  org_investor_num: number | null;
  investor_num: number | null;
  marginliab_investor_num: number | null;
  total_guarantee_yi: number | null;
  avg_guarantee_ratio: number | null;
}

/** get_org_survey 行 */
export interface OrgSurveyRow {
  code: string;
  name: string;
  notice_date: string | null;
  receive_start_date: string | null;
  receive_end_date: string | null;
  org_count: number | null;
  receive_way_explain: string;
  receive_place: string;
  receptionist: string;
  receive_object: string;
  close_price: number | null;
  change_rate: number | null;
}

/** get_holder_num 行(户数:户,市值:元) */
export interface HolderNumRow {
  code: string;
  name: string;
  end_date: string | null;
  pre_end_date: string | null;
  hold_notice_date: string | null;
  holder_num: number | null;
  pre_holder_num: number | null;
  holder_num_change: number | null;
  holder_num_ratio: number | null;
  interval_change: number | null;
  avg_market_cap: number | null;
  avg_hold_num: number | null;
  total_market_cap: number | null;
  total_a_shares: number | null;
}

/** get_earnings_predict 行(金额:元,幅度:%) */
export interface EarningsPredictRow {
  code: string;
  name: string;
  notice_date: string | null;
  report_date: string | null;
  predict_finance: string;
  predict_type: string;
  predict_content: string;
  change_reason: string;
  predict_amt_lower: number | null;
  predict_amt_upper: number | null;
  add_amp_lower: number | null;
  add_amp_upper: number | null;
  preyear_same_period: number | null;
}

/** get_lift_stage 行(数量:股,市值:元,free_ratio 为小数比率) */
export interface LiftStageRow {
  code: string;
  name: string;
  free_date: string | null;
  current_free_shares: number | null;
  able_free_shares: number | null;
  lift_market_cap: number | null;
  free_ratio: number | null;
  pre_close: number | null;
  b20_change: number | null;
  a20_change: number | null;
  free_shares_type: string;
}

/** get_notices 行 */
export interface NoticeRow {
  art_code: string;
  title: string;
  notice_date: string | null;
  display_time: string;
  column_name: string;
  stock_code: string;
  stock_name: string;
  url: string;
}

export const getZtPool = (date?: string) =>
  cmd<DcResult<ZtPoolRow>>("get_zt_pool", date ? { date } : {});
/** 情绪池统一入口:zt 涨停 / prev 昨日涨停 / strong 强势 / sub_new 次新 / broken 炸板 / dt 跌停。 */
export type PoolKind = "zt" | "prev" | "strong" | "sub_new" | "broken" | "dt";
export type PoolRow = Record<string, unknown>;
export const getPool = (pool: PoolKind, date?: string) =>
  cmd<DcResult<PoolRow>>("get_pool", date ? { pool, date } : { pool });
export interface BoardConsRow {
  code: string;
  name: string;
  price: number | null;
  pct: number | null;
  pe: number | null;
  total_market_cap: number | null;
  float_market_cap: number | null;
}
export const getBoardConstituents = (boardCode: string) =>
  cmd<DcResult<BoardConsRow>>("get_board_cons", { bk_code: boardCode });
export const getBillboard = (days?: number) =>
  cmd<DcResult<BillboardRow>>("get_billboard", days != null ? { days } : {});
export const getMarginDaily = () => cmd<DcResult<MarginDailyRow>>("get_margin_daily");
export const getOrgSurvey = (days?: number) =>
  cmd<DcResult<OrgSurveyRow>>("get_org_survey", days != null ? { days } : {});
export const getHolderNum = (code?: string) =>
  cmd<DcResult<HolderNumRow>>("get_holder_num", code ? { code } : {});
export const getEarningsPredict = (reportDate?: string) =>
  cmd<DcResult<EarningsPredictRow>>(
    "get_earnings_predict",
    reportDate ? { report_date: reportDate } : {},
  );
export const getLiftStage = (start: string, end: string) =>
  cmd<DcResult<LiftStageRow>>("get_lift_stage", { start, end });
export const getNotices = (code: string, days?: number) =>
  cmd<DcResult<NoticeRow>>("get_notices", days != null ? { code, days } : { code });

// ==================== 正式披露中心 ====================

export interface DisclosureSecurity { code: string; name: string; market: string }
export interface DisclosureSource {
  source_id: string; provider_id: string; provider_name: string; authority: string;
  authority_name: string; entry_kind: string; upstream_id: string | null;
  original_url: string; discovered_at: number; latency_ms: number | null; is_primary: boolean;
}
export interface DisclosureAttachment {
  attachment_id: string; parent_attachment_id: string | null; name: string; original_url: string;
  media_type: string; byte_size: number | null; content_hash: string | null;
  source_version_id: string | null; extraction_status: string; page_count: number | null;
  parser_version: string; review_reason: string | null;
}
export interface DisclosureEvent {
  event_id: string; event_type: string; fields: Record<string, unknown>;
  evidence: Record<string, unknown>; parser_version: string;
}
export interface DisclosureListItem {
  disclosure_id: string; title: string; category: string; category_name: string;
  status: string; status_name: string; published_at: number | null;
  publication_precision: string; first_seen_at: number; discovery_latency_secs: number | null;
  revision_of: string | null; cancelled_by: string | null; source_version_id: string | null;
  extraction_status: string; review_reason: string | null; securities: DisclosureSecurity[];
  sources: DisclosureSource[]; primary_verified: boolean;
}
export interface DisclosureDetail extends DisclosureListItem {
  attachments: DisclosureAttachment[]; events: DisclosureEvent[];
  revisions: DisclosureListItem[]; verification_note: string;
}
export interface DisclosureQuery {
  security_code?: string | null; keyword?: string | null; category?: string | null;
  status?: string | null; primary_only?: boolean; from_utc?: number | null; to_utc?: number | null;
  page: number; page_size: number;
}
export interface DisclosurePage {
  items: DisclosureListItem[]; total: number; page: number; page_size: number; total_pages: number;
}
export interface DisclosureSyncSnapshot {
  job_id: string | null; running: boolean; status: string; phase: string; progress: number;
  current_provider: string; current_item: string; discovered: number; normalized: number;
  inserted: number; deduplicated: number; primary_verified: number; needs_review: number;
  failures: number; estimated_remaining_seconds: number | null; recent_logs: string[];
  started_at: number | null; updated_at: number; error: string | null;
}
export interface DisclosureProviderHealth {
  provider_id: string; provider_name: string; authority: string; authority_name: string;
  enabled: boolean; public_index_url: string; target_latency_secs: number;
  rate_limit_per_minute: number; last_attempt_at: number | null; last_success_at: number | null;
  consecutive_failures: number; retry_after: number | null; last_error: string | null; note: string;
}
export const queryDisclosures = (query: DisclosureQuery) => cmd<DisclosurePage>("query_disclosures", { query });
export const getDisclosureDetail = (disclosureId: string) => cmd<DisclosureDetail>("get_disclosure_detail", { disclosure_id: disclosureId });
export const disclosureSyncStart = (request: { security_code?: string; days?: number; max_pages?: number }) =>
  cmd<{ started: boolean; job_id: string; estimated_seconds: number; note: string }>("disclosure_sync_start", { request });
export const disclosureSyncStatus = () => cmd<DisclosureSyncSnapshot>("disclosure_sync_status");
export const disclosureSyncCancel = () => cmd<{ cancelled: boolean }>("disclosure_sync_cancel");
export const getDisclosureProviderHealth = () => cmd<DisclosureProviderHealth[]>("get_disclosure_provider_health");

// ==================== 全球一级来源与 A 股传导 ====================

export interface GlobalSyncSnapshot {
  job_id: string | null; running: boolean; status: string; phase: string; progress: number;
  current_provider: string; current_item: string; sources_total: number; sources_ready: number;
  source_gaps: number; documents_discovered: number; documents_archived: number;
  observations_saved: number; mapping_paths: number; failures: number;
  estimated_remaining_seconds: number | null; recent_logs: string[];
  started_at: number | null; updated_at: number; error: string | null;
}
export interface GlobalProviderRuntime {
  provider_id: string; provider_name: string; region: string; category: string;
  official_url: string; original_timezone: string; license_policy: string;
  credential_env: string | null; enabled: boolean; target_latency_secs: number;
  rate_limit_per_minute: number; last_attempt_at: number | null; last_success_at: number | null;
  consecutive_failures: number; retry_after: number | null; last_error: string | null;
}
export interface GlobalDocumentListItem {
  document_id: string; provider_id: string; provider_name: string; document_type: string;
  title_original: string; title_zh: string | null; original_language: string; original_url: string;
  source_version_id: string | null; published_at_utc: number; published_local: string;
  published_timezone: string; revision_no: number; primary_verified: boolean;
  translation_status: string; gap_reason: string | null; license_policy: string;
}
export interface GlobalDocumentQuery {
  provider_id?: string | null; keyword?: string | null; primary_only: boolean;
  page: number; page_size: number;
}
export interface GlobalDocumentPage {
  items: GlobalDocumentListItem[]; total: number; page: number; page_size: number; total_pages: number;
}
export interface GlobalGoldenChain {
  chain_id: string; name: string; global_sources: string[]; nodes: string[];
  activation_requirement: string;
}
export interface GlobalEntity {
  entity_id: string; entity_type: string; legal_name: string; name_zh: string | null;
  jurisdiction: string; identifiers: Record<string, unknown>; aliases: string[];
  translation_status: string;
}
export interface GlobalRelation {
  relation_id: string; src_entity_id: string; dst_entity_id: string; relation_type: string;
  direction: string; confidence_bps: number; evidence_document_id: string;
  evidence_source_version_id: string; evidence_quote_original: string;
  evidence_quote_zh: string | null; evidence_location: Record<string, unknown>;
  observed_at: number; valid_from: number; valid_to: number | null;
}
export interface GlobalTransmissionPath {
  path_id: string; entities: GlobalEntity[]; relations: GlobalRelation[];
  path_confidence_bps: number; target_a_share_code: string;
}
export const globalSyncStart = (request: { sec_cik?: string; include_world_bank?: boolean; max_sec_filings?: number }) =>
  cmd<{ started: boolean; job_id: string; estimated_seconds: number; note: string }>("global_sync_start", { request });
export const globalSyncStatus = () => cmd<GlobalSyncSnapshot>("global_sync_status");
export const globalSyncCancel = () => cmd<{ cancelled: boolean }>("global_sync_cancel");
export const getGlobalProviderHealth = () => cmd<GlobalProviderRuntime[]>("get_global_provider_health");
export const queryGlobalDocuments = (query: GlobalDocumentQuery) => cmd<GlobalDocumentPage>("query_global_documents", { query });
export const getGlobalGoldenChains = () => cmd<GlobalGoldenChain[]>("get_global_golden_chains");
export const getGlobalTransmissionPaths = (rootEntityId: string, asOf?: number, maxDepth?: number) =>
  cmd<GlobalTransmissionPath[]>("get_global_transmission_paths", { root_entity_id: rootEntityId, as_of: asOf, max_depth: maxDepth });
