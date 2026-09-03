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

### Run 2 on the #87 branch (2026-09-02, live MiniMax M3 + market upstreams)

Fresh session, balanced, 601899. **Failed at round 19** — finalization budget
(8 attempts) exhausted, 3 residual problems, 65 blocking findings total across
submissions, 631→458 s elapsed class. Trajectory 30 → decode → 23 → 3 → 3 → 3
→ exhausted. Tool profile: 6 data/research tools in rounds 1–3, 6
run_financial_calculation (3 shape failures), 6 search_evidence (4 sequential
single-query rounds — batch form only partially adopted), 7 submit_report.

Root causes extracted from the durable draft record (agent_effects_v2 holds
the full submitted arguments):

1. Attempt 1 was a skeleton (1 claim defined, 14 referenced by sections) —
   the model submitted early; 30 findings.
2. Attempt 2 died at decode: `kind: "calculated"` (right name:
   `deterministic_calculation`) — serde names it in one round.
3. The persistent `conflicting_evidence×1`: claim_market_regime cited
   evf_50e6…/evf_1ed2…/evf_2fd6… but disclosed conflicts for three
   different registrations of the same facts. The repair target named no
   identifiers, so the model guessed wrong ids for four straight attempts.
4. Whack-a-mole on claim_valuation_summary (kind=inference cannot carry
   numbers): unknown_number_reference×2 → unsupported_observed_number×2 →
   figure_in_free_text×2 across attempts 5–7.

Fixes landed on the branch (commits b79acf2, fe61d0d), both verified by
deterministic tests:

- Refusal events name the stage (decode error / validation histogram) — the
  run-2 trajectory above was read from the new events.
- Repair targets carry `conflicting_evidence_ids` (the exact cited ids to
  disclose) and the action says to copy them verbatim.
- The 7 run-2 drafts joined the offline replay fixture (56 total).

Run 3 (with both fixes) started 2026-09-02; outcome recorded below.

### Runs 3–5 + stream-liveness correction (2026-09-02/03)

- Run 3 failed: 5 of 8 finalization attempts died at **decode/shape**
  (claims-as-map, text in an f64, missing statement) while research took
  only 5 rounds. Fixes landed (commit 3131969): report size bounded by
  depth in the prompt (fast ≈2-4 / balanced ≈5-9 / deep ≈10-16 claims), a
  one-pass draft self-check required before submission, and the
  decode-failure instruction lists the exact shape rules plus the claim-kind
  enum names. Static prompt ratchet moved 3000→3200 once, documented.
- Run 4 suspended at round 2, run 5 at round 4 — both on
  `error decoding response body` / idle-timeout faults. A first fix
  (Runtime-level mid-stream replay, commit 684a509, pushed as 684a509) was
  measured live and turned out WRONG: run 5 burned the whole new budget on
  repeated idle timeouts. Root cause: MiniMaxClient already owns raw-SSE
  watchdogs + bounded pre-commit reconnects, and the adapter hides private
  reasoning, so a Runtime watchdog timing only visible chunks produces
  false idles and multiplies the provider's attempts.
- **Corrected fix (commit 6cf9a21, force-pushed):** `ModelProvider::
  manages_stream_liveness()` — MiniMax declares ownership; Runtime applies
  its watchdog only for simple providers. Deterministic tests pin all three
  boundaries (plain idle still suspends; self-managed activity outlives the
  outer watchdog; an exhausted provider fault suspends after exactly one
  Runtime stream call). The superseded 684a509 content no longer exists on
  the branch.
- **Case C run 1 on the corrected build PUBLISHED** (2026-09-03, exit 0,
  331 s): 21 rounds, 25 tool calls (8 search_evidence, 4 calc, 8
  submit_report), independent verifier passed with zero blocking findings
  on the final draft, 4,481 registered citations in the task. The run
  repeatedly crossed the previous 120 s stall point without a false idle.
  Repair trajectory 27 → 17 → 6 → 2 → 1 → publish.
- Residual false-refusal found in that run and fixed (commit 30fa858):
  `876.13亿元` citing calculation evidence valued exactly `876.13`
  (亿-denominated) was refused because the shared rule scaled ×1e8.
  `supported_by` now also accepts same-denomination evidence (mirroring its
  `%` convention); different magnitudes still refuse.

State: Case C publishes but not yet at ≥5 consecutive. Runs 2..5 on the
fully corrected build (denomination fix included) are the repeatability
measurement; CI on the branch was green through 6cf9a21.

### Runs 6–8 + the network root cause (2026-09-03)

- Run 6 failed at budget exhaustion: `unsupported_observed_number` stuck at
  exactly 9 across three attempts — an inference claim carrying observed
  balance-sheet figures (capex/lt_debt/bonds/st_debt) refused every time.
  Fixed (commit 0e3182a): `ClaimKind::permits` now allows Inference and
  Scenario claims to carry Observed/Calculated figures (same concession
  DeterministicCalculation already had); Unknown stays number-free.
- Run 5 (direct-transport attempt) failed in the endgame: validation
  converged 49→22→14→2→1, then the model deleted its figures and the
  verifier refused the emptied report twice. Fixed (commit f965d40):
  validation raises the verifier's own
  `report_contains_no_verifiable_numeric_claims` at draft time under the
  same code (one round earlier, restore-the-figures instruction);
  max_finalization_attempts 8→10 on measured trajectories.
- Runs 4/7/8 suspended on `error decoding response body` / MiniMax idle
  watchdog, runs 4–8 each losing 1–2 network-fault rounds.
- **Root cause found (host-level, not MiniMax):** every `cargo`/`astock`
  process on this host ran under proxywrap → LD_PRELOAD
  `libproxychains.so.4` → the flaky local socks 127.0.0.1:10808 —
  intercepting ALL connect() including loopback. This caused: the
  intermittent MiniMax mid-stream breaks, the false idle-timeout
  suspensions (runs 7/8's "120 s MiniMax watchdog" was proxychains
  killing the connection), eastmoney 502s, and all 16
  `astock-minimax` http tests failing locally (reqwest→loopback was
  proxied). Proof: `env LD_PRELOAD=libproxychains.so.4 <testbin>` fails,
  without it passes; `which cargo` resolves to the proxywrap shim which
  re-injects the preload even under `env -u LD_PRELOAD`.
- **Remediation:** run cargo via the real binary
  `/home/jacek/.cargo/bin/cargo` (all 16 minimax http tests now pass
  locally); run `astock` with an explicit stable proxy in
  `~/.config/astock/config.toml`:
  `[network] proxy = "socks5h://192.168.31.105:10808"` (LAN proxy,
  verified fast/stable for api.minimaxi.com and market upstreams). A
  temporary StreamPolicy raise (150/240 s) was applied and then reverted —
  the suspensions it targeted were proxychains, not MiniMax.
- CI on the branch: green through 0e3182a (last checks observed).

Next: Case C repeatability series on the clean network path (in flight),
target ≥5 consecutive publications.

## Next exact step

1. Complete ≥5 consecutive fresh-session Case C publications on commit
   ≥30fa858 (run 2 of the series is in flight; harness
   `scripts/run-case-c.sh`, analyzer `scripts/analyze-case-c.py`).
2. Record the series in docs/releases/v7.0.0-live-acceptance.md; run
   remaining gates (workspace suite, clippy, ui) on the final branch state.
3. Merge #87 with that evidence; sync main; continue to Case A repeats and
   JoinQuant live acceptance (E), then the post-#87 PR sequence
   (Context Compiler/memory, fault orchestrator remainder, workers,
   freshness, opportunity research, release pipeline).

## External blockers

- MiniMax + JoinQuant credentials not configured on this host (user action).
- MiniMax rolling quota windows (interval_status exhaustion observed
  historically); schedule live runs around quota, never sleep-wait.
