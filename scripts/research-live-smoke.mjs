import { spawn } from "node:child_process";

const [engineExecutable, agentExecutable] = process.argv.slice(2);
if (!engineExecutable || !agentExecutable) {
  throw new Error("usage: node scripts/research-live-smoke.mjs <engine.exe> <agent-worker.exe>");
}

const MAX_FRAME_BYTES = 8 * 1024 * 1024;
const NEWS_SOURCE_GROUPS = [
  ["cls-telegraph", "cls-depth", "cls-hot", "jin10"],
  ["wallstreetcn-quick", "wallstreetcn-hot", "wallstreetcn-news", "mktnews-flash"],
  ["gelonghui", "fastbull-express", "fastbull-news", "xueqiu-hotstock"],
];

class FramedWorker {
  constructor(name, executable) {
    this.name = name;
    this.pending = new Map();
    this.buffer = Buffer.alloc(0);
    this.stderr = "";
    this.child = spawn(executable, [], { stdio: ["pipe", "pipe", "pipe"], windowsHide: true });
    this.child.stderr.setEncoding("utf8");
    this.child.stderr.on("data", (chunk) => { this.stderr = (this.stderr + chunk).slice(-4000); });
    this.child.stdout.on("data", (chunk) => { this.buffer = Buffer.concat([this.buffer, chunk]); this.drain(); });
    this.child.on("exit", (code) => {
      for (const waiter of this.pending.values()) waiter.reject(new Error(`${name} exited ${code}`));
      this.pending.clear();
    });
  }

  drain() {
    while (this.buffer.length >= 4) {
      const length = this.buffer.readUInt32LE(0);
      if (length <= 0 || length > MAX_FRAME_BYTES) throw new Error(`${this.name} invalid frame length ${length}`);
      if (this.buffer.length < length + 4) return;
      const body = this.buffer.subarray(4, length + 4);
      this.buffer = this.buffer.subarray(length + 4);
      const response = JSON.parse(body.toString("utf8"));
      const waiter = this.pending.get(response.request_id);
      if (!waiter) continue;
      clearTimeout(waiter.timer);
      this.pending.delete(response.request_id);
      waiter.resolve(response);
    }
  }

  request(kind, payload = {}, deadlineMs = 180_000) {
    const requestId = `${this.name}-${Date.now()}-${Math.random().toString(36).slice(2)}`;
    const body = Buffer.from(JSON.stringify({
      protocol_version: 1,
      request_id: requestId,
      kind,
      payload,
      deadline_ms: deadlineMs,
    }), "utf8");
    if (body.length > MAX_FRAME_BYTES) throw new Error(`${kind} request exceeds 8 MiB`);
    const header = Buffer.alloc(4);
    header.writeUInt32LE(body.length);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(requestId);
        reject(new Error(`${kind} timed out; stderr=${this.stderr}`));
      }, deadlineMs + 15_000);
      this.pending.set(requestId, { resolve, reject, timer });
      this.child.stdin.write(Buffer.concat([header, body]));
    }).then((response) => {
      if (!response.ok) throw new Error(`${kind}: ${response.error?.code}: ${response.error?.message}`);
      return response.payload;
    });
  }

  async close() {
    this.child.stdin.end();
    await new Promise((resolve) => setTimeout(resolve, 200));
    if (this.child.exitCode == null) this.child.kill();
  }
}

const engine = new FramedWorker("engine", engineExecutable);
const agent = new FramedWorker("agent", agentExecutable);
const startedAt = Date.now();

async function fetchNews(filters, perGroup, minimumItems = 10) {
  const batch = await engine.request("research.news", {
    ...filters,
    sources: NEWS_SOURCE_GROUPS.flat(),
    limit: perGroup,
  }, 120_000);
  if (batch.items.length < minimumItems) throw new Error(`insufficient news evidence (${batch.items.length}); ${(batch.errors ?? []).join("; ")}`);
  return {
    ...batch,
    successful_sources: [...new Set(batch.successful_sources ?? [])],
    successful_channels: [...new Set(batch.successful_channels ?? [])],
    stale_sources: [...new Set(batch.stale_sources ?? [])],
    errors: [...new Set(batch.errors ?? [])],
    requested_source_count: NEWS_SOURCE_GROUPS.flat().length,
  };
}

function asObject(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : null;
}

function tailRows(value, limit) {
  return Array.isArray(value) ? value.slice(-limit) : value;
}

function compactResearchDatasets(value, limits = {}, fallback = 80) {
  const envelope = asObject(value);
  const datasets = asObject(envelope?.datasets);
  if (!envelope || !datasets) return value;
  return {
    ...envelope,
    datasets: Object.fromEntries(Object.entries(datasets).map(([key, datasetValue]) => {
      const dataset = asObject(datasetValue);
      if (!dataset || !Array.isArray(dataset.rows)) return [key, datasetValue];
      const limit = limits[key] ?? fallback;
      return [key, { ...dataset, rows: dataset.rows.slice(0, limit), model_view_rows: Math.min(dataset.rows.length, limit) }];
    })),
  };
}

function compactSecurityEvidence(bundle) {
  const market = asObject(bundle.market);
  const kline = asObject(market?.kline);
  const fundamentals = asObject(bundle.fundamentals);
  const reconciliation = asObject(bundle.reconciliation);
  return {
    symbol: bundle.symbol,
    market: market ? {
      ...market,
      kline: kline ? { ...kline, bars: tailRows(kline.bars, 180) } : market.kline,
      fund_flow_30d: tailRows(market.fund_flow_30d, 30),
    } : bundle.market,
    fundamentals: fundamentals ? {
      ...fundamentals,
      income: tailRows(fundamentals.income, 8),
      balance: tailRows(fundamentals.balance, 8),
      cashflow: tailRows(fundamentals.cashflow, 8),
      indicators: tailRows(fundamentals.indicators, 8),
      dividends: tailRows(fundamentals.dividends, 8),
      valuation_history: tailRows(fundamentals.valuation_history, 180),
    } : bundle.fundamentals,
    events: compactResearchDatasets(bundle.events, { announcements_1y: 100, cninfo_disclosures_1y: 40, org_survey_2y: 80, block_trade_1y: 80 }, 80),
    news: { ...bundle.news, items: bundle.news.items.slice(0, 20) },
    reconciliation: reconciliation ? {
      ...reconciliation,
      kline_close_checks: tailRows(reconciliation.kline_close_checks, 20),
    } : bundle.reconciliation,
    joinquant: compactResearchDatasets(bundle.joinquant, { qfq_daily: 250, benchmark_components: 500, macro_cpi: 24 }, 500),
  };
}
try {
  await Promise.all([
    engine.request("system.handshake", { app_version: "research-live-smoke", protocol_version: 1 }, 15_000),
    agent.request("system.handshake", { app_version: "research-live-smoke", protocol_version: 1 }, 15_000),
  ]);
  const taskId = crypto.randomUUID();
  const taskSpec = {
    objective: "基于截至当前可取得的最新数据，为2万元资金生成仅供人工执行的A股投资计划；反复核验新闻、行情、资金、财务与估值，证据不足就暂停或保留现金",
    security_universe: ["AGENT_BEST_AFTER_EVIDENCE"],
    as_of: new Date().toISOString(),
    research_start: "2025-08-24",
    research_end: "2026-08-24",
    investment_horizon: "1至3个月",
    comparison_benchmark: "000300",
    output_type: "manual_plan",
    evidence_requirement: "primary_sources",
  };
  const started = await agent.request("agent.start", { task_id: taskId, seq: 1, spec: taskSpec }, 30_000);
  if (started.state?.phase !== "preparing") throw new Error(`unexpected start phase ${started.state?.phase}`);

  const [marketOverview, marketContext, globalContext, marketNews, candidates] = await Promise.all([
    engine.request("market.overview", {}, 120_000),
    engine.request("research.market_context", {}, 180_000),
    engine.request("research.global_context", {}, 180_000),
    fetchNews({}, 50),
    engine.request("research.market_candidates", { limit: 60, max_lot_cost: 16_000 }, 120_000),
  ]);
  if (!Array.isArray(candidates.items) || candidates.items.length < 5) throw new Error("candidate pool is too small");
  if (!Array.isArray(marketNews.items) || marketNews.items.length < 10) throw new Error("multi-source news evidence is insufficient");

  const planned = await agent.request("agent.plan", {
    task_id: taskId,
    candidates: candidates.items,
    market_context: marketOverview,
  }, 180_000);
  const symbols = (planned.plan?.symbols ?? []).slice(0, 3);
  if (!symbols.length || symbols.length > 3) throw new Error("invalid planned symbol count");

  const rawSecurities = await Promise.all(symbols.map(async (symbol) => {
    const [market, fundamentals, events, news, reconciliation, joinquant] = await Promise.all([
      engine.request("market.security_snapshot", { symbol, period: "day", adjust: "qfq", count: 500 }, 180_000),
      engine.request("research.fundamentals", { symbol }, 240_000),
      engine.request("research.security_events", { symbol }, 180_000),
      fetchNews({ symbol, keyword: symbol }, 25, 0),
      engine.request("research.data_reconcile", { symbol }, 180_000),
      engine.request("research.joinquant_context", {
        symbol,
        benchmark: taskSpec.comparison_benchmark,
        start: taskSpec.research_start,
        end: taskSpec.research_end,
      }, 180_000),
    ]);
    return { symbol, market, fundamentals, events, news, reconciliation, joinquant };
  }));
  const securities = rawSecurities.map(compactSecurityEvidence);

  const researched = await agent.request("agent.research", {
    task_id: taskId,
    context: {
      source: "desktop_engine",
      retrieved_at: new Date().toISOString(),
      research_plan: planned.plan,
      market_overview: marketOverview,
      market_context: compactResearchDatasets(marketContext, { billboard_7d: 80, margin_daily: 30, industry_boards: 40, concept_boards: 40, previous_limit_up_pool: 100, sub_new_pool: 100 }, 200),
      global_context: globalContext,
      market_news: { ...marketNews, items: marketNews.items.slice(0, 90) },
      securities,
      evidence_inventory: {
        requested_symbols: symbols,
        dimensions: ["quote", "kline", "technical_analysis", "fund_flow", "market_pools", "previous_limit_up", "sub_new", "billboard", "margin", "boards", "global_gold", "primary_gold_news", "macro_inflation", "macro_growth", "macro_current_account", "financial_statements", "valuation_history", "org_survey", "holder_count", "earnings_forecast", "unlocks", "suspensions", "block_trade", "announcements", "cninfo_disclosures", "multi_source_news", "cross_provider_reconciliation", "optional_joinquant_daily_valuation_benchmark_macro"],
        review_rounds: 3,
      },
    },
  }, 900_000);
  if (researched.state?.phase !== "completed") throw new Error(`unexpected final phase ${researched.state?.phase}`);
  if (researched.state?.model_rounds !== 3) throw new Error(`expected 3 model rounds, got ${researched.state?.model_rounds}`);
  if (typeof researched.report !== "string" || researched.report.length < 800) throw new Error("final report is missing or too short");
  if (researched.report.includes("<think>")) throw new Error("private reasoning leaked into final report");
  if (!/(2万元|20000|20,000)/.test(researched.report)) throw new Error("final report lost the 20,000 CNY capital constraint");

  console.log(JSON.stringify({
    ok: true,
    elapsed_ms: Date.now() - startedAt,
    candidate_pool: candidates.items.length,
    planned_symbols: symbols,
    market_news_items: marketNews.items.length,
    successful_news_sources: marketNews.successful_sources?.length ?? 0,
    successful_news_channels: marketNews.successful_channels?.length ?? 0,
    market_context_datasets: Object.keys(marketContext.datasets ?? {}).length,
    market_context_failures: Object.entries(marketContext.datasets ?? {}).filter(([, value]) => !value.ok).map(([key]) => key),
    global_context_datasets: Object.keys(globalContext.datasets ?? {}).length,
    global_context_failures: Object.entries(globalContext.datasets ?? {}).filter(([, value]) => !value.ok).map(([key]) => key),
    detailed_security_bundles: securities.length,
    security_event_failures: securities.flatMap((row) => Object.entries(row.events?.datasets ?? {}).filter(([, value]) => !value.ok).map(([key]) => `${row.symbol}:${key}`)),
    fundamental_missing_sections: securities.reduce((total, row) => total + (row.fundamentals.missing_sections?.length ?? 0), 0),
    reconciliation_blocking: securities.filter((row) => row.reconciliation.blocking).map((row) => row.symbol),
    joinquant_configured: securities.some((row) => row.joinquant?.configured === true),
    joinquant_failures: securities.flatMap((row) => Object.entries(row.joinquant?.datasets ?? {}).filter(([, value]) => !value.ok).map(([key]) => `${row.symbol}:${key}`)),
    model_rounds: researched.state.model_rounds,
    report_chars: researched.report.length,
    phase: researched.state.phase,
  }));
} finally {
  await Promise.all([engine.close(), agent.close()]);
}
