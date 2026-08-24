import { spawn } from "node:child_process";

const [engineExecutable, requestedSymbol = "000725"] = process.argv.slice(2);
if (!engineExecutable) {
  throw new Error("usage: node scripts/research-data-audit.mjs <engine.exe> [symbol]");
}

const MAX_FRAME_BYTES = 8 * 1024 * 1024;
const NEWS_SOURCE_GROUPS = [
  ["cls-telegraph", "cls-depth", "cls-hot", "jin10"],
  ["wallstreetcn-quick", "wallstreetcn-hot", "wallstreetcn-news", "mktnews-flash"],
  ["gelonghui", "fastbull-express", "fastbull-news", "xueqiu-hotstock"],
];

class EngineWorker {
  constructor(executable) {
    this.pending = new Map();
    this.buffer = Buffer.alloc(0);
    this.stderr = "";
    this.child = spawn(executable, [], { stdio: ["pipe", "pipe", "pipe"], windowsHide: true });
    this.child.stderr.setEncoding("utf8");
    this.child.stderr.on("data", (chunk) => { this.stderr = (this.stderr + chunk).slice(-4000); });
    this.child.stdout.on("data", (chunk) => { this.buffer = Buffer.concat([this.buffer, chunk]); this.drain(); });
  }

  drain() {
    while (this.buffer.length >= 4) {
      const length = this.buffer.readUInt32LE(0);
      if (length <= 0 || length > MAX_FRAME_BYTES) throw new Error(`invalid frame length ${length}`);
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

  request(kind, payload = {}, deadlineMs = 120_000) {
    const requestId = `audit-${Date.now()}-${Math.random().toString(36).slice(2)}`;
    const body = Buffer.from(JSON.stringify({
      protocol_version: 1,
      request_id: requestId,
      kind,
      payload,
      deadline_ms: deadlineMs,
    }), "utf8");
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

function sourceSummary(source) {
  return {
    provider: source.provider,
    ok: source.ok,
    fetched_at: source.fetched_at,
    source: source.source,
    error: source.error,
    price: source.quote?.price,
    volume: source.quote?.volume,
    amount: source.quote?.amount,
    timestamp: source.quote?.timestamp,
  };
}

const engine = new EngineWorker(engineExecutable);
const startedAt = Date.now();
const auditEnd = new Date().toISOString().slice(0, 10);
const auditStart = new Date(Date.now() - 365 * 86_400_000).toISOString().slice(0, 10);
try {
  const [handshake, credentials, reconciliation, marketContext, globalContext, securityEvents, candidates, joinquant, newsBatch] = await Promise.all([
    engine.request("system.handshake", { app_version: "research-data-audit", protocol_version: 1 }, 15_000),
    engine.request("credentials.status", {}, 15_000),
    engine.request("research.data_reconcile", { symbol: requestedSymbol }, 180_000),
    engine.request("research.market_context", {}, 180_000),
    engine.request("research.global_context", {}, 180_000),
    engine.request("research.security_events", { symbol: requestedSymbol }, 180_000),
    engine.request("research.market_candidates", { limit: 60, max_lot_cost: 16_000 }, 120_000),
    engine.request("research.joinquant_context", { symbol: requestedSymbol, benchmark: "000300", start: auditStart, end: auditEnd }, 180_000),
    engine.request("research.news", { sources: NEWS_SOURCE_GROUPS.flat(), limit: 60 }, 120_000)
      .then((value) => ({ ok: true, requested: NEWS_SOURCE_GROUPS.flat(), value }))
      .catch((error) => ({ ok: false, requested: NEWS_SOURCE_GROUPS.flat(), error: String(error) })),
  ]);
  const news = [newsBatch].map((batch) => batch.ok ? {
    requested: batch.requested,
    item_count: batch.value.items?.length ?? 0,
    successful_sources: batch.value.successful_sources ?? [],
    successful_channels: batch.value.successful_channels ?? [],
    stale_sources: batch.value.stale_sources ?? [],
    errors: (batch.value.errors ?? []).map((value) => String(value).slice(0, 300)),
    latest_items: (batch.value.items ?? []).slice(0, 3).map((item) => ({
      source_id: item.source_id,
      provider_id: item.provider_id,
      title: item.title,
      published_at: item.published_at,
      trust_tier: item.trust_tier,
    })),
  } : { requested: batch.requested, error: batch.error });
  console.log(JSON.stringify({
    ok: true,
    elapsed_ms: Date.now() - startedAt,
    engine_version: handshake.engine_version,
    credential_configured: credentials.providers,
    candidates: {
      count: candidates.items?.length ?? 0,
      source: candidates.source,
      industry_enrichment: candidates.industry_enrichment,
      standardized_name_count: (candidates.items ?? []).filter((item) => typeof item.name === "string" && item.name.length > 0 && !/\s/.test(item.name)).length,
      industry_count: (candidates.items ?? []).filter((item) => typeof item.industry === "string" && item.industry.length > 0).length,
      board_count: new Set((candidates.items ?? []).map((item) => item.board).filter(Boolean)).size,
      samples: (candidates.items ?? []).slice(0, 5).map(({ symbol, name, market, board, industry, lot_cost }) => ({ symbol, name, market, board, industry, lot_cost })),
    },
    joinquant: {
      configured: joinquant.configured,
      source: joinquant.source,
      datasets: Object.fromEntries(Object.entries(joinquant.datasets ?? {}).map(([key, value]) => [key, {
        ok: value.ok,
        total_rows: value.total_rows,
        source: value.source,
        error: value.error,
      }])),
    },
    market_context: {
      trade_date: marketContext.trade_date,
      datasets: Object.fromEntries(Object.entries(marketContext.datasets ?? {}).map(([key, value]) => [key, {
        ok: value.ok,
        total_rows: value.total_rows,
        returned_rows: value.rows?.length ?? 0,
        truncated: value.truncated,
        source: value.source,
        error: value.error,
      }])),
    },
    global_context: {
      datasets: Object.fromEntries(Object.entries(globalContext.datasets ?? {}).map(([key, value]) => [key, {
        ok: value.ok,
        total_rows: value.total_rows,
        returned_rows: value.rows?.length,
        source: value.source,
        error: value.error,
      }])),
    },
    security_events: {
      symbol: securityEvents.symbol,
      as_of: securityEvents.as_of,
      datasets: Object.fromEntries(Object.entries(securityEvents.datasets ?? {}).map(([key, value]) => [key, {
        ok: value.ok,
        total_rows: value.total_rows,
        returned_rows: value.rows?.length ?? 0,
        truncated: value.truncated,
        source: value.source,
        error: value.error,
      }])),
    },
    news,
    reconciliation: {
      symbol: reconciliation.symbol,
      blocking: reconciliation.blocking,
      quote_sources: (reconciliation.quote_sources ?? []).map(sourceSummary),
      quote_conflicts: (reconciliation.quote_checks ?? []).filter((row) => !row.consistent),
      kline_sources: reconciliation.kline_sources,
      kline_overlap_days: reconciliation.kline_overlap_days,
      kline_conflicts: (reconciliation.kline_close_checks ?? []).filter((row) => !row.consistent),
      policy: reconciliation.policy,
    },
  }, null, 2));
} finally {
  await engine.close();
}
