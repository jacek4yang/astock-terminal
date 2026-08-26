# Architecture

## Cross-platform recovery target

```text
astock CLI / inline TUI ─┐
                         ├──> shared Rust Agent Runtime ──> Rust Engine
React + thin Tauri v2 ───┘                                  │
                                                            └──> domain/data/storage crates
```

The Rust Agent Runtime owns provider-independent orchestration, task state,
bounded retries/review, evidence/publication policy and public progress
events. The Engine owns deterministic financial algorithms, source adapters,
SQLite/Parquet persistence and evidence registries. CLI and Tauri are adapters
and may not create alternate Agent implementations.

The current recovery branch has the first CLI/runtime vertical slice while the
v6 Proton/MoonBit implementation remains preserved. The sections below record
that v6 production boundary as migration history; they are not the final
cross-platform target. MoonBit may remain for formal/specification work but is
not required by the `astock` binary. See `rust-agent-recovery.md` for the
KEEP/PORT/REWRITE/REMOVE/ARCHIVE audit and replacement gates.

## v6 preserved architecture

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

Before an existing SQLite schema changes, the storage layer creates and
integrity-checks an online backup. All pending schema migrations then commit in
one transaction. Explicit data-root migration and rollback validate SQLite and
the file manifest, atomically switch only the activation pointer, and never
delete either the retained source or migrated copy.

The research renderer submits one `agent.research.workflow` request containing
only user-selected depth/tool policy and the current symbol preference. The
MoonBit Agent emits a bounded, allowlisted `host_effects + continuation`
contract: it first requests the market/macro/news/candidate snapshot, reviews
that result, validates model-selected symbols and a closed advanced-analysis
set (earnings driver, industry graph, relationship, market regime and
historical backtest), then requests the selected securities' evidence snapshot
before three report review rounds. Non-`auto` tool policies deterministically
override model output; no module can add trading, credential or storage
mutation. The Host does not choose a symbol, data source, tool, parameter or
stop condition; it persists the Agent checkpoint/effect intent and accepts only
the three declared research aggregate kinds, including on the first attempt.
The browser Bridge enforces the same allowlist. Results are persisted before
they are returned to the continuation. The Rust aggregate services window
repetitive rows and reject any context that would approach the 8 MiB frame
boundary. `release-architecture-check.mjs` fails if Engine tool selection
returns to the React workbench or either Host allowlist weakens.

Renderer request permissions are generated from the same protocol schemas as
the Rust, MoonBit and TypeScript contracts. Engine, public Agent and Host kinds
are checked independently in React, Proton Host and the browser acceptance
Bridge. The internal `agent.research.workflow.continue` kind is Worker-to-Host
only and cannot be submitted by Renderer code; unknown Host kinds fail instead
of falling through to diagnostics. Renderer code can read a durable task for
recovery, but cannot create a task or append events, checkpoints and Effects.
For `agent.start`, `agent.event` and `agent.research.workflow`, Host (and only
the development acceptance Bridge outside production) journals the user input
and operation intent before calling Agent, persists intermediate checkpoints,
then stores the correlated terminal response. A repeated completed operation
is served from that immutable journal result instead of invoking Agent twice.
Operation and tool identities compare the full structured payload; Engine
stores only a `sha256:` digest as the unique SQLite key, so large parameters
neither overflow the identity column nor leak their contents through indexes.
Before a post-start event or workflow is sent, Host restores the newest Engine
checkpoint into Agent. A supervised Worker restart therefore resumes from
durable state instead of depending on React to notice and repair the process.
`agent.restore` and the in-memory task snapshot are internal Worker operations,
not Renderer permissions; the workbench can read only the Engine's durable task
projection.
The renderer-facing task service is fixed to
`task.create/list/get/branch/resume/cancel/answer`. It is a typed compatibility
facade rather than a second wire protocol: mutating transitions remain
Host-journaled Agent calls, while bounded history/task reads and message
branching remain deterministic Engine calls.
Checkpoint branching is fail-closed: Engine verifies the source conversation
snapshot against the current durable task and checkpoint sequence, records the
origin for audit, then clears executable task/effect/checkpoint state in the
new conversation. The branch starts a new task and reacquires current data;
historical results are evidence leads, never silently reused facts.
Stateful Agent operations are single-flight at the Host boundary. A concurrent
duplicate waits for the first operation, then reuses its committed result;
only an orphaned journal intent after a process loss can enter the bounded
read-only retry path.
An explicit durable cancel event is the only exception to waiting behind a
long Agent request: because the Agent channel is ordered and synchronous, Host
terminates that Worker, records the interrupted operation as failed, restarts
and re-handshakes, restores the last Engine checkpoint, and then journals and
reduces the cancel event. A timed-out Worker channel is likewise terminated so
a delayed frame can never be correlated with a later request.

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

The retired Dockview registry, v5 `AgentChat`, local Agent session store and
route/layout shell are also absent from production sources and dependencies.
News, disclosures and global-source views can hand a text draft to the single
v6 Agent composer, but that transient handoff cannot create or recover a task.
The workspace persistence key is v6-specific, so an obsolete IDE layout cannot
be silently adopted after upgrade.

## Contract and persistence

`protocol/schema` is the contract source. Frames use a four-byte little-endian
length followed by UTF-8 JSON, with an 8 MiB hard limit. Request IDs,
cancellation IDs, deadlines and protocol version are validated at every
boundary. Large datasets use bounded pages and stable snapshot/source version
identifiers.

Engine and Agent startup versions plus the minimum production capability sets
are also schema-pinned. The Host validates the correlated response envelope,
protocol v1, release 6.0.0, frame/page limits, reducer version and every required
capability at initial startup and after a supervised restart; extra future
capabilities are allowed. The browser development Bridge and IPC smoke use the
same schema-derived contract. A Worker that merely replies `ok` but is missing
one of these fields is terminated as incompatible instead of entering service.

Agent state changes and effect intent are committed by Host to the Engine
SQLite event store before Provider or Engine side effects run. An effect result
is committed before it is reduced. Conversation history, branches,
checkpoints, evidence and verification findings are durable; React local
storage is never the task truth source and has no journal write capability.
Secrets live only in Windows Credential Manager.

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
