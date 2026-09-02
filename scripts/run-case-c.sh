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

  python3 - "$run_jsonl" "$exit_code" "$elapsed" <<'PY'
import json, sys, collections
path, exit_code, elapsed = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
rounds = 0
tools = collections.Counter()
tool_fail = collections.Counter()
empty_turns = 0
verifications = 0
blocking = 0
findings = collections.Counter()
submit_attempts = 0
outcome = "no_terminal_event"
citations = 0
for line in open(path, encoding="utf-8", errors="replace"):
    line = line.strip()
    if not line:
        continue
    try:
        ev = json.loads(line)
    except json.JSONDecodeError:
        continue
    t = ev.get("type")
    if t == "model_started":
        rounds = max(rounds, ev.get("round", 0))
    elif t == "tool_scheduled":
        tools[ev.get("tool", "?")] += 1
        if ev.get("tool") == "submit_report":
            submit_attempts += 1
    elif t == "tool_failed":
        tool_fail[ev.get("tool", "?")] += 1
    elif t == "model_turn_empty":
        empty_turns += 1
    elif t == "verification_started":
        verifications += 1
    elif t == "verification_finding":
        f = ev.get("finding", {})
        findings[f.get("code", "?")] += 1
        if f.get("blocking"):
            blocking += 1
    elif t == "completed":
        outcome = "completed"
        citations = len(ev.get("evidence_ids", []))
    elif t == "failed":
        outcome = "failed"
    elif t == "suspended":
        outcome = "suspended"
    elif t == "cancelled":
        outcome = "cancelled"
summary = {
    "exit_code": exit_code,
    "elapsed_s": elapsed,
    "outcome": outcome,
    "model_rounds": rounds,
    "tool_calls_total": sum(tools.values()),
    "tool_calls": dict(sorted(tools.items())),
    "tool_failures": dict(sorted(tool_fail.items())),
    "empty_turns": empty_turns,
    "independent_verifications": verifications,
    "blocking_findings": blocking,
    "finding_codes": dict(sorted(findings.items())),
    "submit_report_attempts": submit_attempts,
    "citations_in_report": citations,
}
out = path.replace(".jsonl", ".summary.json")
json.dump(summary, open(out, "w"), ensure_ascii=False, indent=2)
print(json.dumps(summary, ensure_ascii=False))
PY
  echo
done

echo "=== ALL RUNS DONE $(date -Is); summaries in $OUT_DIR"
