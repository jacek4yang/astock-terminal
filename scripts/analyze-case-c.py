#!/usr/bin/env python3
"""Extract Case C convergence metrics from a typed JSONL event stream.

The CLI's --jsonl wraps each typed Agent event as
{"event": {...}, "session_id": ..., "task_id": ...}; this reads that shape and
emits the metrics the v7 live acceptance record tracks.

Usage: analyze-case-c.py <run.jsonl> [more.jsonl ...]
"""
import json
import sys
from collections import Counter


def analyze(path: str) -> dict:
    rounds = 0
    model = None
    tools = Counter()
    tool_failures = Counter()
    empty_turns = 0
    verifications = 0
    blocking = 0
    finding_codes = Counter()
    submit_attempts = 0
    submit_failures = 0
    outcome = "no_terminal_event"
    citations = 0
    evidence_added = 0
    clarifications = 0
    for line in open(path, encoding="utf-8", errors="replace"):
        line = line.strip()
        if not line:
            continue
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        ev = record.get("event", record)
        t = ev.get("type")
        if t == "model_started":
            rounds = max(rounds, ev.get("round", 0))
            model = ev.get("model", model)
        elif t == "tool_scheduled":
            tools[ev.get("tool", "?")] += 1
            if ev.get("tool") == "submit_report":
                submit_attempts += 1
        elif t == "tool_failed":
            tool_failures[ev.get("tool", "?")] += 1
            if ev.get("tool") == "submit_report":
                submit_failures += 1
        elif t == "model_turn_empty":
            empty_turns += 1
        elif t == "verification_started":
            verifications += 1
        elif t == "verification_finding":
            finding = ev.get("finding", {})
            finding_codes[finding.get("code", "?")] += 1
            if finding.get("blocking"):
                blocking += 1
        elif t == "evidence_added":
            evidence_added += len(ev.get("evidence_ids", []))
        elif t == "clarification_requested":
            clarifications += 1
        elif t == "completed":
            outcome = "completed"
            citations = len(ev.get("evidence_ids", []))
        elif t == "failed":
            outcome = "failed"
        elif t == "suspended":
            outcome = "suspended"
        elif t == "cancelled":
            outcome = "cancelled"
    return {
        "file": path.split("/")[-1],
        "model": model,
        "outcome": outcome,
        "model_rounds": rounds,
        "tool_calls_total": sum(tools.values()),
        "tool_calls": dict(sorted(tools.items())),
        "tool_failures": dict(sorted(tool_failures.items())),
        "empty_turns": empty_turns,
        "independent_verifications": verifications,
        "blocking_findings": blocking,
        "finding_codes": dict(sorted(finding_codes.items())),
        "submit_report_attempts": submit_attempts,
        "submit_report_failures": submit_failures,
        "evidence_ids_added": evidence_added,
        "clarifications": clarifications,
        "citations_in_report": citations,
    }


def main() -> None:
    for path in sys.argv[1:]:
        print(json.dumps(analyze(path), ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
