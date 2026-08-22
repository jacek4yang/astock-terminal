/**
 * Tauri 命令层类型化封装。
 * 契约见 ../docs/command-contract.md;所有命令返回 JSON(snake_case),
 * 错误统一 { error: string, kind: string }。
 */
import { Channel, invoke } from "@tauri-apps/api/core";

/** 是否在 Tauri 桌面环境(纯浏览器 dev 时为 false) */
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export const NOT_TAURI_MSG = "需在桌面应用中运行(纯浏览器模式无行情数据)";

/** 后端统一错误结构 */
export interface ApiError {
  error: string;
  kind?: string;
}

/** 规范化 invoke 抛出的错误为可读中文文案 */
export function errMsg(e: unknown): string {
  if (!isTauri()) return NOT_TAURI_MSG;
  if (e && typeof e === "object") {
    const obj = e as Record<string, unknown>;
    if (typeof obj.error === "string") return obj.error;
    if (typeof obj.message === "string") return obj.message;
  }
  return String(e);
}

function cmd<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) return Promise.reject(new Error(NOT_TAURI_MSG));
  return invoke<T>(name, args);
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
  current: ValuationCurrent | null;
  percentile: ValuationPercentile | null;
  dcf: DcfValuation | null;
  history_series: ValuationHistoryPoint[] | null;
}

export const getFundamentals = (symbol: string) =>
  cmd<FundamentalsJson>("get_fundamentals", { symbol });
export const getValuation = (symbol: string) => cmd<ValuationJson>("get_valuation", { symbol });

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
  tool: string;
  cache_key: string;
  source: string;
  fetched_at: string;
}

export interface AgentReport {
  task_id: string;
  answer: string;
  conclusions: unknown;
  evidence: AgentEvidence[];
  generated_at: string;
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
  enabled_tools: string[];
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
export const supplyChainShock = (subject: string, direction: "up" | "down", magnitudePct?: number) =>
  cmd<ShockJson>("supply_chain_shock", { subject, direction, magnitude_pct: magnitudePct });
export const relationshipGraph = (symbols: string[], windowDays?: number) =>
  cmd<RelationshipGraph>("relationship_graph", { symbols, window_days: windowDays });

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
