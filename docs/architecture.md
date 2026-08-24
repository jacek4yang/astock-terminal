# v6 architecture

## Production process boundary

```text
React renderer (CEF)
        │ typed protocol v1 bridge
MoonBit Proton 0.2.1 Host
        ├── MoonBit Agent Worker
        └── Rust financial Engine
```

The renderer has three primary surfaces only: 今日市场, Agent 智研 and 配置.
It owns presentation state, never credentials or durable Agent truth. The Host
owns the native window, deny-by-default permissions, framed IPC, Worker
supervision and diagnostics. The Agent Worker owns clarification, planning,
tool coordination, review and publication decisions. Rust owns deterministic
market, research, quant, storage and credential services.

The research renderer submits one `agent.research.workflow` request containing
only user-selected depth/tool policy and the current symbol preference. The
MoonBit Agent emits a bounded, allowlisted `host_effects + continuation`
contract: it first requests the market/macro/news/candidate snapshot, reviews
that result (and invokes MiniMax for candidate selection when necessary), then
requests the selected securities' evidence snapshot before three report review
rounds. The Host does not choose a symbol, data source, tool, parameter or stop
condition; it persists the Agent checkpoint/effect intent, executes only
`target=engine`, persists the result, and returns it to the continuation. The
Rust aggregate services window repetitive rows and reject any context that
would approach the 8 MiB frame boundary. `release-architecture-check.mjs`
fails if Engine tool selection returns to the React workbench.

Every reducer call id remains unique, while an Engine snapshot cache key is
derived from the task, tool kind and full JSON payload. If Host/Agent/renderer
dies after an effect intent or result is persisted, the pure
`ReconcileInterruptedWorkflow` event moves pending calls to a separate
reconciled audit list (never to the completed list), retains the selected
security set, and replays only the two allowlisted read-only aggregate
services. A succeeded matching result is reused; an orphaned pending read is
retried under a journaled suffix. Mutating or unknown effects remain
fail-closed. Provider credentials, quota, rate limits or temporary
availability move the task to `Suspended`; after recovery, the same workflow
continues from the newest Engine checkpoint and parameter-addressed cache.

The frozen 127-command v5 registry reached 127/127 mapped capabilities before
the cutover. `src-tauri`, the old Rust Agent crate and all Tauri/WebView2
dependencies have now been removed. `scripts/capability-parity-check.mjs`
preserves the exact count and SHA-256 of that reviewed registry as the
differential oracle, while `scripts/release-architecture-check.mjs` prevents
either legacy runtime from returning. There is no browser or Tauri production
fallback.

## Contract and persistence

`protocol/schema` is the contract source. Frames use a four-byte little-endian
length followed by UTF-8 JSON, with an 8 MiB hard limit. Request IDs,
cancellation IDs, deadlines and protocol version are validated at every
boundary. Large datasets use bounded pages and stable snapshot/source version
identifiers.

Agent state changes and effect intent are committed to the Engine SQLite event
store before Provider or Engine side effects run. An effect result is committed
before it is reduced. Conversation history, branches, checkpoints, evidence
and verification findings are durable; React local storage is never the task
truth source. Secrets live only in Windows Credential Manager.

## Data correctness

Security Master resolves code, market, security type, canonical name and
aliases before any source query. Price, currency, volume unit, time zone,
adjustment mode, trading state and source timestamps remain explicit. Missing,
stale, single-source or conflicting observations are not converted to zero.
Material cross-source conflicts block Agent publication.

## Reliability classification

Release conclusions use only these labels: `FORMALLY PROVED`, `MODEL CHECKED`,
`PROPERTY TESTED`, `INTEGRATION TESTED`, `FAULT-INJECTION TESTED`,
`ASSUMED/TRUSTED BOUNDARY` and `NOT VERIFIED`. The formal model and proof
boundary is documented in `formal/README.md`. Provider behavior, Windows
facilities and the abstract-model/runtime refinement remain trusted boundaries
even when their integration tests pass.
