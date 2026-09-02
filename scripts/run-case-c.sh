#!/usr/bin/env bash
# Live Case C repeatability measurement.
#
# Runs fresh-session Case C asks, one at a time, capturing the typed JSONL event
# stream per run, then extracting the convergence metrics the v7 acceptance
# record tracks: rounds, tool calls by name, submit_report attempts, blocking
# findings, citations, outcome, elapsed.
#
# Usage: run-case-c.sh <run-count> <data-dir> <binary>
set -uo pipefail

COUNT="${1:-5}"
DATA_DIR="${2:-/tmp/astock-live-c}"
BIN="${3:-./target/debug/astock}"
OUT_DIR="$DATA_DIR/measurements"
mkdir -p "$OUT_DIR"

QUERY='简单分析紫金矿业当前估值、趋势和主要风险。'

for i in $(seq 1 "$COUNT"); do
  stamp=$(date +%Y%m%d-%H%M%S)
  run_jsonl="$OUT_DIR/case-c-run${i}-${stamp}.jsonl"
  echo "=== RUN $i/$COUNT start $(date -Is) -> $run_jsonl"
  start=$(date +%s)
  timeout 2400 "$BIN" ask \
    --data-dir "$DATA_DIR" \
    --depth balanced \
    --symbol 601899 \
    --jsonl \
    "$QUERY" > "$run_jsonl" 2>"$OUT_DIR/case-c-run${i}-${stamp}.err"
  exit_code=$?
  elapsed=$(( $(date +%s) - start ))
  echo "=== RUN $i/$COUNT exit=$exit_code elapsed=${elapsed}s"

  python3 "$(dirname "$0")/analyze-case-c.py" "$run_jsonl" | python3 -c "
import json, sys
summary = json.load(sys.stdin)
summary['exit_code'] = $exit_code
summary['elapsed_s'] = $elapsed
out = '$run_jsonl'.replace('.jsonl', '.summary.json')
json.dump(summary, open(out, 'w'), ensure_ascii=False, indent=2)
print(json.dumps(summary, ensure_ascii=False))
"
  echo
done

echo "=== ALL RUNS DONE $(date -Is); summaries in $OUT_DIR"
