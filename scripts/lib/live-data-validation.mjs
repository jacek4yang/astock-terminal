import crypto from "node:crypto";

export function validateJoinQuantDaily(context, dataset, symbol, start, end) {
  if (context.symbol !== symbol || context.start !== start || context.end !== end || context.source !== "JoinQuant") {
    throw new Error("JoinQuant response identity/window does not match the authenticated request");
  }
  if (dataset.source !== "JoinQuant" || dataset.truncated !== false || dataset.total_rows !== dataset.rows.length) {
    throw new Error("JoinQuant qfq_daily pagination/source metadata is inconsistent");
  }
  const fetchedAt = Date.parse(dataset.fetched_at);
  const retrievedAt = Date.parse(context.retrieved_at);
  if (!Number.isFinite(fetchedAt) || !Number.isFinite(retrievedAt)) {
    throw new Error("JoinQuant qfq_daily fetch timestamps are invalid");
  }
  let previousDate = "";
  for (const [index, row] of dataset.rows.entries()) {
    if (!row || typeof row !== "object" || !/^\d{4}-\d{2}-\d{2}$/.test(row.date) ||
        row.date < start || row.date > end || (previousDate && row.date <= previousDate)) {
      throw new Error(`JoinQuant qfq_daily row ${index} has an invalid, duplicate or unordered date`);
    }
    previousDate = row.date;
    for (const field of ["open", "high", "low", "close"]) {
      if (!Number.isFinite(row[field]) || row[field] <= 0) {
        throw new Error(`JoinQuant qfq_daily row ${index} has invalid ${field}`);
      }
    }
    if (row.high < Math.max(row.open, row.close, row.low) ||
        row.low > Math.min(row.open, row.close, row.high) || row.close >= 10_000) {
      throw new Error(`JoinQuant qfq_daily row ${index} violates OHLC bounds`);
    }
    if (!Number.isFinite(row.volume) || row.volume < 0 || row.volume_unit !== "Lots" ||
        (row.amount != null && (!Number.isFinite(row.amount) || row.amount < 0))) {
      throw new Error(`JoinQuant qfq_daily row ${index} has invalid volume/amount units`);
    }
  }
  const firstDate = dataset.rows[0].date;
  const latestDate = dataset.rows.at(-1).date;
  const latestLagDays = Math.floor((Date.parse(`${end}T00:00:00Z`) - Date.parse(`${latestDate}T00:00:00Z`)) / 86_400_000);
  if (latestLagDays < 0 || latestLagDays > 14) {
    throw new Error(`JoinQuant qfq_daily latest bar is stale by ${latestLagDays} days`);
  }
  return {
    symbol,
    requested_start: start,
    requested_end: end,
    first_date: firstDate,
    latest_date: latestDate,
    latest_lag_days: latestLagDays,
    structural_rows_checked: dataset.rows.length,
    volume_unit: "Lots",
    truncated: false,
    data_sha256: crypto.createHash("sha256").update(JSON.stringify(dataset.rows), "utf8").digest("hex"),
  };
}
