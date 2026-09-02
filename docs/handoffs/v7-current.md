# v7 current state

Durable resume point for the v7.0.0 mission. Update after every milestone.
Facts here come from git/GitHub/tests, never from chat memory.

## Merged main

`1b4a5e7376037f12657845bbdf8658de99525d65` — docs(release): record the
measurement on the merged contract (#86). Workspace already converged at
`7.0.0` (astock-storage v7.0.0 per Cargo).

## Active branch / PR

- Branch: `feat/batched-evidence-discovery` (local = `origin` @
  `30b86c3f7325735db28c446c70d71d4a6a4fc75f`).
- PR #87 "Batch evidence discovery and calculation into one call each",
  state open, all 10 CI checks green (quality matrix, run 33146074873).
- Diff reviewed in full on 2026-09-02: batching (`search_evidence` queries[],
  `run_financial_calculation` programs[]), `compute_from_evidence`,
  all-missing-fields decode scan, engine payload paths via
  `serde_path_to_error`, worked scalar example on calc shape errors,
  calculation-tool withdrawal after 3 consecutive shape failures (offer filter
  + call refusal), observed numbers cannot cite non-numeric evidence.
  **Verifier untouched** (confirmed by diff inspection).
- Local deterministic validation on 2026-09-02: `cargo fmt --check` clean;
  `cargo test -p astock-agent-runtime` 107 unit + 138 integration tests all
  pass (decode_shape 5, finalization 25, report_contract 41, live_draft_replay
  1 replaying 49 real drafts).

## Completed invariants (merged on main)

- One Rust Agent runtime (`astock-agent-runtime`), Rust Engine, thin Tauri v2
  desktop (React), MoonBit/Proton retired (#83).
- Structured report contract; `submit_report`/`search_evidence` as Runtime
  tools; validate → render → verifier → publish; no free-form publication.
- Every printed figure backed (declared numeric_item or cited evidence);
  `{label}` references; free-text coverage hole closed (#84).
- Typed provider faults, one attempt budget, durable resume time (#85).
- Case A (simple factual) stable: 5/5 published, zero blocking findings
  across measured batches (see docs/releases/v7.0.0-live-acceptance.md).

## Current measured blocker

Case C (moderate: `简单分析紫金矿业当前估值、趋势和主要风险。`) publishes
~1/3 on the final code; failures exhaust the 32-round ceiling. Root causes
measured and addressed by PR #87: O(figures) search_evidence (42 calls),
O(figures) calc calls (12–24), one-field-per-round decode repair, AST shape
thrash after coverage, non-numeric evidence cited by observed numbers.
**Live re-measurement of Case C on the #87 branch has NOT been run** — the PR
says "continuing", no results recorded.

## Credentials

`astock credentials status` → minimax: not configured; joinquant: not
configured. Live testing blocked until the user runs the credential
checkpoint (`credentials set minimax` / `credentials set joinquant`).
Never ask for secret values in chat.

## Environment notes (this host)

- Repo is at `/home/jacek/src/astock-terminal` (NOT `/home/jacek/astock-terminal`).
- Network: everything runs through proxywrap shims + proxychains → socks5
  127.0.0.1:10808. The socks5 proxy ABORTS HTTP CONNECT, so plain
  `HTTPS_PROXY=http://127.0.0.1:10808` fails for curl/cargo. Working recipe
  for cargo (needed until crates are cached; `cargo fetch --locked` done
  2026-09-02, 269 crates were missing):
  `env -u HTTP_PROXY -u HTTPS_PROXY -u NO_PROXY HTTPS_PROXY=socks5h://127.0.0.1:10808 /home/jacek/.cargo/bin/cargo <cmd>`
- git uses SSH (`git@github.com:jacek4yang/astock-terminal.git`) and works.

## Tests run (latest)

- fmt --check: clean (branch).
- agent-runtime suite: all pass (branch).
- Full workspace suite: run 2026-09-02 — see next milestone update for the
  recorded totals.

## Live measurements

See docs/releases/v7.0.0-live-acceptance.md (authoritative). Case A stable;
Case C intermittent pre-#87; B and D–J unmeasured.

### Run 1 on the #87 branch (2026-09-02, live MiniMax M3 + market upstreams)

Fresh session, balanced, 601899. **Suspended at round 13** by a provider
network fault ("error decoding response body") after 631 s; the runtime
fail-closed with evidence intact. Metrics from the JSONL stream:

- Research rounds 1–7: get_quote/get_fundamentals/get_kline/get_market_regime/
  research_news in round 1 (parallel), 7 search_evidence calls, 3
  compute_from_evidence (43 registered calculation evidence ids each), 2
  run_financial_calculation shape failures (strings in scalar values) after
  which the model self-corrected to compute_from_evidence — **the #87
  fallback path worked live**. Pre-#87 baseline was 42 search_evidence calls
  and 12–24 calculation calls.
- Finalization rounds 8–13: submit attempts 1–2 failed at decode (no findings
  emitted), attempts 3–4 each returned 40 findings (35 figure_in_free_text,
  4 number_disagrees_with_evidence, 1 other); the model began a full rewrite
  at round 13 and the stream died mid-generation.
- Verdict: **#87's research-phase objective (round consumption) is fixed
  live**. The residual Case C blocker is finalization convergence:
  figure_in_free_text repair burden (~35/attempt) and decode friction.
- Resume of the suspended task started 2026-09-02 (tests durable resume +
  convergence within the remaining finalization budget); outcome recorded
  below when available.

## Next exact step

1. Finish deterministic gates on the #87 branch (workspace tests, clippy
   `-D warnings`, cargo deny, frontend npm test/build).
2. LIVE CREDENTIAL CHECKPOINT to the user (exact wording per mission §5).
3. After "configured": live Case C re-measurement on the branch, ≥5
   consecutive fresh-session publications target, zero blocking findings,
   metrics recorded (rounds, tool calls, search_evidence calls, calc calls,
   submit_report attempts, citations, elapsed).
4. Merge #87 only with that evidence; sync main; update this file.

## External blockers

- MiniMax + JoinQuant credentials not configured on this host (user action).
- MiniMax rolling quota windows (interval_status exhaustion observed
  historically); schedule live runs around quota, never sleep-wait.
