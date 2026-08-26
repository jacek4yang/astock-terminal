# Shared Rust Agent Runtime

`crates/agent-runtime` is the provider-independent product core used by CLI
and future Tauri adapters. Its public seams are:

- `ModelProvider`: model selection, optional quota and streamed public model
  chunks;
- `ToolRegistry`: closed typed tools with Engine operation, schema, risk,
  timeout, cache and freshness metadata;
- `ToolExecutor`: execution boundary implemented by the embedded Rust Engine;
- `AgentStore`: durable task event/checkpoint/effect operations;
- `AgentRuntime`: task validation, model/tool loop, bounded parallel tools,
  cancellation, evidence collection and publication verification;
- `TaskStream`: typed public events plus cooperative cancellation and a joined
  outcome;
- `RuntimeSession` / `SessionManager`: bounded multi-turn messages and Engine-
  backed save, list, load, latest-session and immutable branch operations.

The runtime does not depend on Tauri, React, Ratatui, Proton, CEF or MoonBit.
It currently depends on the Engine, generated protocol models and the MiniMax
adapter crate. Future provider adapters implement the same model seam.

## Durability order

For every transition the runtime:

1. appends the typed event;
2. writes a checkpoint accepting that sequence;
3. emits the event to the UI adapter.

Before a Provider or Engine effect it writes `EffectIntent` with an
idempotency key. It persists the effect result before reducing/publishing the
corresponding completion event. v6 Engine SQLite services enforce monotonic
sequences, duplicate identity consistency and terminal effect consistency.

The first slice checkpoints every public event. Later compaction may batch
high-frequency text deltas only if crash/replay semantics remain explicit and
tested.

Task recovery reads are backward-paged at no more than 500 events using
`next_before_seq`. Effect-ledger reads are forward-paged at no more than 500
entries using the stable `(caused_by_seq, effect_id)` cursor. No task or effect
history is moved as an unbounded IPC result.

## Model and tool safety

Only registered function names are accepted. Names contain ASCII
alphanumeric/underscore characters and map to fixed Engine request kinds.
Unknown tools and malformed argument JSON fail closed. No shell, generic
filesystem mutation, process spawning or broker action exists in the model
tool surface.

Complex model-requested calculations use the closed
`astock-finance-calc/v1` JSON AST. The Runtime exposes local and JoinQuant-data
calculation tools, while the Engine validates, fuel-meters, fingerprints and
executes them. Neither tool accepts source-code strings or grants a process,
filesystem or arbitrary-network capability. See
[compute-language.md](compute-language.md).

Independent calls are admitted concurrently up to `max_parallel_tools`.
Every call has a deadline and a child cancellation token. Tool results have a
hard serialized-byte budget; an oversized result is reported as failed rather
than silently truncated or moved unboundedly into model context.

The provider-independent loop additionally caps each model round at 10,000
chunks, 120,000 visible characters, 32 tool calls and 256,000 tool-argument
characters. Crossing a limit closes the persisted provider effect as failed
and emits a non-retryable malformed-response terminal state.

## Public reasoning and verification

The runtime emits plans, tool actions, evidence IDs, visible text and
verification findings. It does not emit separated provider reasoning content.
The MiniMax adapter requests separated reasoning and also has a fragmented
`<think>` visibility filter as defense in depth.

Final model text is sent to `research.agent_report_verify` with the original
task contract and collected Engine evidence registries. Blocking findings
trigger a bounded revision round. Exhausting revision rounds returns exit code
5 and never emits a successful `Completed` event.

## Recovery boundary

Conversation messages now survive process exit. The CLI can list/load them and
continue a conversation as a fresh durable task; the Runtime passes at most 40
recent user/Agent messages and 120,000 characters back to the model. Older
messages remain intact in storage and are represented by a deterministic
extractive index capped at 30,000 characters, explicitly labeled as historical
context rather than evidence. Session rows are saved before task start, at
non-text transitions and terminal state, while every task event remains in the
event log.

Branching is wired through the existing Engine operation: the Engine verifies
the source conversation and, for a latest-message branch, the durable task and
checkpoint sequence before creating a new conversation with task state
cleared. Exact replay from an interrupted task's checkpoint/effect frontier,
clarification and semantic/model-assisted summary compaction are not yet wired
into this Rust facade. They will reuse the current Engine stores rather than
create a second database.
