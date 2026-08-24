import test from "node:test";
import assert from "node:assert/strict";

import { validateJoinQuantDaily } from "./lib/live-data-validation.mjs";

const context = {
  configured: true,
  symbol: "000725",
  start: "2026-04-26",
  end: "2026-08-24",
  source: "JoinQuant",
  retrieved_at: "2026-08-24T02:00:01Z",
};

const rows = [
  { date: "2026-08-20", open: 10, high: 10.6, low: 9.8, close: 10.4, volume: 1234, volume_unit: "Lots", amount: 1_240_000 },
  { date: "2026-08-21", open: 10.4, high: 10.8, low: 10.2, close: 10.7, volume: 1567, volume_unit: "Lots", amount: 1_670_000 },
];

function dataset(overrides = {}) {
  return {
    ok: true,
    rows: structuredClone(rows),
    total_rows: 2,
    truncated: false,
    source: "JoinQuant",
    fetched_at: "2026-08-24T02:00:00Z",
    ...overrides,
  };
}

test("audits the authenticated JoinQuant identity, window, units, freshness and digest", () => {
  const result = validateJoinQuantDaily(context, dataset(), "000725", context.start, context.end);
  assert.deepEqual({
    symbol: result.symbol,
    first_date: result.first_date,
    latest_date: result.latest_date,
    latest_lag_days: result.latest_lag_days,
    structural_rows_checked: result.structural_rows_checked,
    volume_unit: result.volume_unit,
    truncated: result.truncated,
  }, {
    symbol: "000725",
    first_date: "2026-08-20",
    latest_date: "2026-08-21",
    latest_lag_days: 3,
    structural_rows_checked: 2,
    volume_unit: "Lots",
    truncated: false,
  });
  assert.match(result.data_sha256, /^[a-f0-9]{64}$/);
});

test("rejects identity, ordering and pagination mismatches", () => {
  assert.throws(() => validateJoinQuantDaily({ ...context, symbol: "600519" }, dataset(), "000725", context.start, context.end), /identity\/window/);
  assert.throws(() => validateJoinQuantDaily(context, dataset({ total_rows: 3 }), "000725", context.start, context.end), /pagination\/source/);
  const duplicate = dataset();
  duplicate.rows[1].date = duplicate.rows[0].date;
  assert.throws(() => validateJoinQuantDaily(context, duplicate, "000725", context.start, context.end), /duplicate or unordered date/);
});

test("rejects invalid OHLC, wrong volume units and stale latest bars", () => {
  const badOhlc = dataset();
  badOhlc.rows[1].high = 10.1;
  assert.throws(() => validateJoinQuantDaily(context, badOhlc, "000725", context.start, context.end), /OHLC bounds/);
  const wrongUnit = dataset();
  wrongUnit.rows[1].volume_unit = "FundUnits";
  assert.throws(() => validateJoinQuantDaily(context, wrongUnit, "000725", context.start, context.end), /volume\/amount units/);
  const stale = dataset();
  stale.rows = stale.rows.map((row, index) => ({ ...row, date: `2026-07-${20 + index}` }));
  assert.throws(() => validateJoinQuantDaily(context, stale, "000725", context.start, context.end), /latest bar is stale/);
});
