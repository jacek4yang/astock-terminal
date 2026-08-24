# Data-source parity and release gate

Updated: 2026-08-24 (Asia/Shanghai)

This document treats the last Tauri application as a migration oracle, not as
the target architecture. The old application registers 127 UI commands. The
versioned Engine v1 contract currently exposes 57 coarse request kinds. A lower
command count is intentional, but a feature is not considered migrated merely
because its Rust crate still compiles.

## Status language

- `READY`: reachable through the Proton/Engine or MoonBit Agent path and tested.
- `ENRICHED`: migrated and strengthened with provenance, cross-checks or Agent use.
- `INTERNAL ONLY`: mature Rust implementation remains, but no new Engine service
  exposes it yet. This is a migration gap, not parity.
- `GAP`: no acceptable replacement path exists yet.
- `TRUSTED BOUNDARY`: correctness depends on an external provider, network,
  operating-system facility or user credential and cannot be proved locally.

The old Tauri baseline must not be removed while any release-required row is
`INTERNAL ONLY` or `GAP`.

`scripts/capability-parity-check.mjs` turns that rule into a build gate. It
freezes and classifies all 127 legacy handlers, all 14 concrete market-provider
modules and all 26 registered official global sources. Registry drift fails CI
until it is reviewed; deleting `src-tauri` while blockers remain also fails.
The classification currently exposes 54 legacy handlers through new coarse
services and keeps 73 as explicit migration blockers. This count is deliberately
conservative: partial coverage does not count as migrated. Even with all six
current sentiment pools present, the legacy
arbitrary-date pool query stays blocked until the coarse service has equivalent
historical semantics.

The 26-entry global catalog must not be confused with 26 working legacy
collectors. The old `global_sync_start` actively downloads World Bank and an
explicitly requested SEC CIK; the other entries primarily provide source,
license, timezone, credential and health metadata. The new contract therefore
does not claim those catalog rows as live data. World Bank is active, SGE/WGC
context is newly exposed, and SEC ingestion remains an explicit blocker.

## Provider inventory

| Source/provider | Data supplied | New path | Status | Accuracy and use policy |
| --- | --- | --- | --- | --- |
| TDX TCP | A-share quote, five levels, day/week/month K-line, full security list | `market.*`, `research.data_reconcile` | ENRICHED | Golden protocol fixtures plus live dual-source checks. Volume is normalized to lots. |
| Tencent | K-line failover | Rust market failover chain | READY | Provider provenance retained; not used as silent zero-value fallback. |
| Sina | K-line failover | Rust market failover chain | READY | Provider provenance retained. |
| EastMoney push2/push2his | Quote, K-line, minute, breadth, full market, fund flow | `market.*`, `research.data_reconcile` | ENRICHED / TRUSTED BOUNDARY | HTTPS only. TDX/EastMoney quotes and overlapping daily closes are compared; conflicts block high-confidence publication. The history CDN is intermittently unavailable, so its failure remains visible and TDX/Tencent/Sina must preserve the two-source gate. |
| EastMoney F10/datacenter | Profile, statements, indicators, dividends, valuation snapshot/history | `research.fundamentals` | ENRICHED | Missing numeric fields remain `null`; statement use is point-in-time gated by announcement date. |
| EastMoney datacenter reports | Limit pools, billboard, block trade, margin, surveys, holder count, earnings forecasts, unlocks, suspensions, notices, boards | `research.market_context`, `research.security_events` | ENRICHED | Ten market-level and nine security-level datasets are exposed as two bounded services. All six legacy sentiment pools are retained. Security filters execute upstream so full-market pagination cannot silently omit a low-frequency security; legitimate empty reports remain distinct from failures. |
| NewsNow public aggregator | 12 allowlisted finance channels | `research.news` | ENRICHED | Provider and logical channel are separate fields. Live entries without `revision_id` are discovery evidence only. |
| Durable news archive | Immutable document revisions, observations, clusters and entity links | merged by `research.news` | READY (read), INTERNAL ONLY (review UI) | Archived revisions remain available; cluster merge/split and evidence-review workflows are not yet exposed by the new contract. |
| EastMoney announcement mirror | A-share announcement discovery | `research.news` | READY | Treated as official-mirror discovery; material claims must follow the exchange/company source link. |
| CNInfo | Statutory disclosure index and PDF original links | `research.security_events.cninfo_disclosures_1y` | ENRICHED (research read), INTERNAL ONLY (bulk archive/review) | The new path resolves the mandatory `orgId` before querying; this fixes the legacy bare-code query that silently returned zero rows. Full cancellable bulk sync, PDF text extraction and review UI remain gated. |
| Source verifier | Fetch, version, compare webpage/PDF evidence | `research.sources.*` | READY | Content-addressed versions are retained; external source windows remain zero privilege. |
| JoinQuant | Adjusted daily, constituents, valuation, macro | `research.joinquant_context` + Credential Manager | READY / TRUSTED BOUNDARY | The Agent consumes one bounded explicit-call research package when configured. The current machine has no configured JoinQuant credential, so contract and missing-credential behavior are tested but no live upstream verification claim is permitted. |
| Tushare Pro | Raw daily, calendar, adjustment-factor cross-check, dividends, daily basics | Rust provider | INTERNAL ONLY | Token-gated; not in automatic failover. |
| iWencai | Natural-language screening / OpenAPI event and board data | Rust provider and screening crate | INTERNAL ONLY | Credential/captcha/rate-limit dependent. |
| World Bank / global asset providers | Official macro series, COMEX/SGE gold, World Gold Council/SGE primary publications | `research.global_context` | ENRICHED | Five independently failing datasets retain observation periods and source identities. Annual macro observations are context, never mislabeled as real-time trading signals. |
| SEC EDGAR | Overseas primary filings | Rust global-intelligence stack | INTERNAL ONLY | Required for issuer-specific overseas transmission research; not silently replaced by media summaries. |
| MiniMax Plus | Clarification, planning, evidence review, adversarial review, final synthesis | MoonBit Worker | ENRICHED | Secrets stay in Windows Credential Manager. Provider output cannot create facts; it may only select tools and synthesize supplied evidence. |

All external providers and the explicitly configured user-managed SOCKS proxy are
`TRUSTED BOUNDARY`. The application records provider, channel, source time and
failure state so the boundary remains visible.

## Functional migration matrix

| Legacy domain | Old capability | New replacement | Gate |
| --- | --- | --- | --- |
| Market | quote, order book, K-line, minute, breadth, all shares, flow, index K-line | bounded `market.*` plus normalized security master | READY |
| Candidate discovery | liquid A-share universe with executable lot-cost filter | `research.market_candidates` plus independent EastMoney industry enrichment | ENRICHED |
| Composite stock analysis | bundle, technical signal, Chanlun daily | `market.security_snapshot` | READY |
| Intraday Chanlun | minute-structure analysis | `analysis.chanlun.minute` | READY |
| Fundamentals | statements, valuation | `research.fundamentals` | READY |
| Earnings driver | driver tree, shocks, snapshots | `research.earnings_driver.*` | READY |
| News retrieval | multi-channel live data plus archive fallback | `research.news` | ENRICHED |
| News operations | center refresh/state, cluster merge/split, evidence review, entity review | `research.news.*` and `research.entities.*` | READY |
| Disclosures | per-security CNInfo index and PDF original links | `research.security_events` | READY |
| Disclosures | cancellable bulk sync/status, PDF archival/extraction, detail/health and review UI | `research.disclosures.*` | READY |
| Source evidence | fetch/list/get/compare | `research.sources.*` | READY |
| Data quality | quote reconciliation | `research.data_reconcile` | ENRICHED |
| Data quality | SLO, lineage, observations, history, valuation reconciliation/health report | `diagnostics.data_quality`, `research.quote_reconcile`, `research.valuation_reconcile` | ENRICHED |
| EastMoney datacenter | pools, billboard, margin, boards | `research.market_context` | READY |
| EastMoney datacenter | survey, holders, forecast, unlock, suspension, block trade, billboard and announcement bundle | `research.security_events` | ENRICHED |
| Knowledge graph | as-of graph, history, timeline, snapshot/diff, supply-chain shock | `research.graph.*` and `research.market.relationship` | READY |
| Quant/backtest | research jobs, snapshots, strategies, persistent backtests, regime | `research.quant.*`, `research.backtest.*`, `research.market.regime` | ENRICHED |
| Global context | gold cross-market snapshot/primary publications and World Bank macro context | `research.global_context` | ENRICHED |
| Global/event/relation | SEC filing sync, golden chains, transmission, event analysis, relation review | `research.global.*`, `research.events.*`, `research.relations.*` | READY for implemented providers; catalog-only sources stay NOT VERIFIED |
| Scan | cancellable whole-market scan | `quant.scan.*` | READY |
| Watchlist | list/add/remove/pin | `workspace.watchlist.*` | READY |
| Credentials/cache | MiniMax, JoinQuant, quota, cache stats/cleanup | coarse Engine services + Credential Manager | READY for exposed providers |
| Data directory | adopt/migrate/transactional switch | `storage.data_root.migrate` with backup, manifest verification and atomic pointer switch | ENRICHED |
| Agent task core | dynamic clarification, Agent-best choice, checkpoints, cancel/recovery | MoonBit reducer + Engine event/checkpoint store | ENRICHED |
| Agent research | candidate plan and final answer | market/news/fundamental/security-event/reconciliation tools plus three model rounds | ENRICHED |
| Agent history | durable tasks/conversations, list/load/rename/soft-delete and branch-from-point | Engine SQLite event/conversation store + three-page Agent UI; renderer local storage is not a truth source | ENRICHED |

## Live audit evidence (2026-08-24)

The checked-in `scripts/research-data-audit.mjs` performs a secret-free framed
IPC audit. For `000725` after the compatibility fixes it observed:

- TDX and EastMoney quote price both `5.75` at approximately 15:04 China time.
- Repeated audits observed the EastMoney history cluster alternating between
  success and connection reset. In the latest recorded run TDX and Sina each
  returned 120 bars and Tencent returned 121; 119 pairwise overlap checks had
  zero conflicts under `max(0.01 CNY, 0.2%)` close-price tolerance. An earlier
  run also obtained 120 EastMoney bars and reached 179 conflict-free checks.
- 60 current finance items, seven populated logical channels, two provider
  layers, and no live retrieval error.
- Ten market-context datasets are now requested: all six legacy sentiment
  pools, seven-day billboard, margin series, and industry/concept boards.
- Five global-context datasets were successful: gold market snapshot, primary
  WGC/SGE publications, and World Bank China/US inflation, GDP-growth and
  current-account observations.
- Nine security-event datasets were successful. For `000725` they included 256
  mirrored announcements, 196 CNInfo statutory disclosures (50 bounded rows
  returned with PDF links), 47 surveys, nine forecasts, nine block trades,
  eight billboard rows and the latest holder count. Legitimate zero-row
  suspension/unlock snapshots were not mislabeled as failures.
- Total Engine audit latency was approximately 4.6 seconds before candidate
  industry and optional-provider coverage were added.

The post-parity audit also verified that all 60 bounded candidates had canonical
whitespace-free names and EastMoney industry tags across three listing-board
classes. Industry enrichment is independently fallible and never replaces the
liquid-universe result. The same audit observed JoinQuant as explicitly
unconfigured and returned no synthetic JoinQuant dataset. With the expanded
candidate and optional-source checks, total audit latency was approximately
10.1 seconds.

Classification: `INTEGRATION TESTED`. This is a time-specific observation, not
a guarantee that an upstream will remain available.

`crates/market-data/tests/eastmoney_kline_live.rs` captures the upstream
compatibility regression: commas in EastMoney's `fields1/fields2` grammar must
remain literal, the historical cluster must not receive the legacy manual
`Connection` header, and the request stays HTTPS-verified. The integrated audit
above reached all four K-line providers, but a subsequent isolated EastMoney
run encountered three upstream connection resets. Classification for
EastMoney-history availability is therefore `NOT VERIFIED`; failover and the
two-independent-source publication gate are `INTEGRATION TESTED`.

The checked-in `scripts/research-live-smoke.mjs` also completed a real,
credential-backed but secret-free 20,000 CNY research task on the final
expanded contract: 60 candidates with market/board/industry metadata, three
planned securities, 50 market-news items across six populated channels, ten
market-context datasets, five global-context datasets and nine security-event
datasets per security. Three model rounds produced a 12,317-character report
in approximately 394 seconds. The report preserved the capital and
manual-execution boundaries. Three fundamental sections were explicitly
missing; none was replaced by zero or a model guess. JoinQuant was explicitly
unconfigured and contributed no synthetic evidence. Classification:
`INTEGRATION TESTED`, not a claim that the plan's future investment outcome is
correct.

## Non-regression release rules

1. Do not delete `src-tauri` until every release-required `INTERNAL ONLY`/`GAP`
   row has a new contract, tests and UI/Agent consumer.
2. Provider success counts and logical channel counts must never be conflated.
3. Missing, stale, single-source and conflicting data remain explicit and may
   suspend a task; they are never converted to zero.
4. A final manual investment plan requires executable 100-share lots, fees and
   cash reserve, and cannot exceed the stated capital.
5. Important news-derived claims require an immutable revision or a primary
   announcement/company/exchange source. Aggregator-only claims stay low
   confidence.
6. A successful model response is not evidence that market data is correct;
   live provider and cross-source gates run independently.
7. Deep research has a 15-minute bounded client deadline with cancellation and
   checkpoints. A five-minute UI timeout is prohibited because three evidence
   rounds can legitimately exceed it; timeout pressure must never be used to
   justify skipping the adversarial review.
