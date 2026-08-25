import test from "node:test";
import assert from "node:assert/strict";
import { validateAuditResults } from "./research-data-release-gate.mjs";

function base(symbol, name, { blocking = false, ok = true } = {}) {
  return {
    exitCode: ok ? 0 : 1,
    data: {
      ok,
      degraded: blocking,
      identity_search: { items: name ? [{ code: symbol, name }] : [] },
      joinquant: { tested: false },
      candidates: {
        count: 60,
        standardized_name_count: 60,
        liquidity_available_count: 60,
        liquidity_incomplete: false,
      },
      news: [{ item_count: 40, successful_channels: ["official-a", "independent-b"] }],
      request_failures: [],
      reconciliation: {
        blocking,
        quote_sources: [{ provider: "tdx", ok: true }, { provider: "eastmoney", ok: true }],
        quote_consensus_sources: 2,
        quote_required_consensus: 2,
        quote_consensus_providers: ["tdx", "eastmoney"],
        quote_outlier_providers: [],
        kline_sources: [{ provider: "tdx", ok: true }, { provider: "sina", ok: true }],
        quote_conflicts: [],
        kline_conflicts: [],
      },
    },
  };
}

function fixture() {
  const standard = base("300308", "中际旭创");
  const beijing = base("920001", "纬达光电", { blocking: true, ok: false });
  beijing.data.reconciliation.quote_sources = [{ provider: "eastmoney", ok: true }];
  beijing.data.reconciliation.kline_sources = [{ provider: "sina", ok: true }];
  const legacy = base("430002", null, { blocking: true, ok: false });
  legacy.data.reconciliation.quote_sources = [];
  legacy.data.reconciliation.kline_sources = [];
  legacy.data.request_failures = [{ kind: "research.data_reconcile" }];
  return { standard, beijing, legacy };
}

test("accepts cross-source standard data and an explicit BSE coverage block", () => {
  const summary = validateAuditResults(fixture());
  assert.equal(summary.standard.name, "中际旭创");
  assert.equal(summary.beijing.blocking, true);
  assert.equal(summary.legacy.rejected, true);
});

test("rejects an empty canonical name and credentialed audit calls", () => {
  const empty = fixture();
  empty.beijing.data.identity_search.items[0].name = "";
  assert.throws(() => validateAuditResults(empty), /canonical name mismatch/);

  const credentialed = fixture();
  credentialed.standard.data.joinquant.tested = true;
  assert.throws(() => validateAuditResults(credentialed), /credentialed provider/);
});

test("rejects a legacy code carrying fabricated market data", () => {
  const value = fixture();
  value.legacy.data.reconciliation.quote_sources = [{ provider: "eastmoney", ok: true }];
  assert.throws(() => validateAuditResults(value), /fabricated live data/);
});

test("accepts one quarantined live quote outlier but rejects a two-by-two split", () => {
  const majority = fixture();
  majority.standard.data.degraded = true;
  majority.standard.data.reconciliation.quote_sources.push(
    { provider: "tencent", ok: true },
    { provider: "sina", ok: true },
  );
  majority.standard.data.reconciliation.quote_consensus_sources = 3;
  majority.standard.data.reconciliation.quote_required_consensus = 3;
  majority.standard.data.reconciliation.quote_consensus_providers = ["tdx", "tencent", "sina"];
  majority.standard.data.reconciliation.quote_outlier_providers = ["eastmoney"];
  majority.standard.data.reconciliation.quote_conflicts = [{ provider: "eastmoney", field: "amount" }];
  assert.deepEqual(validateAuditResults(majority).standard.quote_outlier_providers, ["eastmoney"]);

  const split = structuredClone(majority);
  split.standard.data.reconciliation.blocking = true;
  split.standard.data.reconciliation.quote_consensus_sources = 2;
  split.standard.data.reconciliation.quote_consensus_providers = ["tdx", "tencent"];
  split.standard.data.reconciliation.quote_outlier_providers = ["eastmoney", "sina"];
  assert.throws(() => validateAuditResults(split), /public data audit did not pass|reconciliation is blocking/);
});
