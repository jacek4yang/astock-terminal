# Rust Agent recovery migration map

This document records the Phase 0 differential audit for the cross-platform
Agent recovery. The historical oracle is commit
`7aa0f03846dd56618421ce4f48a4f4e3b6cc3ccb` (`v5.0.3`). It is not a reset
target. The current v6 Engine and protocol remain the source for later data,
durability and verification behavior.

## Observed baseline

- Reference host: Debian 13 (trixie), Linux x86_64, native Linux rather than
  WSL; Rust/Cargo 1.96.0.
- The starting branch was clean `main` at `1f317a1`; recovery work uses
  `feat/rust-agent-recovery`.
- The first `cargo check --workspace --all-targets` failed before compilation
  because `.cargo/config.toml` globally selected
  `D:/astock-build/astock-terminal/cargo-target`. The colon entered Linux's
  `LD_LIBRARY_PATH` as a path separator. Removing the global target directory
  made the unchanged v6 Rust workspace pass on Linux.
- v5.0.3 contained `crates/agent` (15,491 Rust source lines), a Tauri v2
  adapter and React Agent screens. v6 removed both Rust Agent and Tauri,
  retained/expanded the deterministic domain crates, and introduced a Rust
  Engine plus durable Agent event/effect/conversation services behind the
  Proton/MoonBit process graph.
- The current Engine exposes bounded aggregate operations for market
  preparation, per-security evidence, deterministic report verification,
  sessions, effects, checkpoints, quant, graph, disclosures, news and other
  research services. Those APIs are the preferred port target; resurrecting
  direct v5 tool-to-domain dependencies would recreate the wrong dependency
  direction.

## KEEP

| Component | Reason |
| --- | --- |
| `crates/core`, `market-data`, `storage`, `technical`, `chanlun`, `fundamental`, `graph`, `quant`, `backtest` and intelligence/data crates | Current v6 deterministic data and research work is newer than the v5 oracle and already Linux-compiles. |
| `crates/engine` deterministic services | Provides the GUI-independent computation/persistence boundary and bounded research aggregates required by both products. |
| `crates/compute` bounded calculation language | Gives the Agent composable deterministic scalar/series calculations and a protected JoinQuant-data path without arbitrary code execution. |
| `crates/minimax` resilient client | Retains model discovery, quota, SSE parsing, pre-commit replay, idle watchdogs, SOCKS-capable HTTP dependencies and secret-redacting `SecretKey`. |
| `crates/protocol` framing and generated contracts | Preserves length-prefixed UTF-8 JSON, bounded frames/pages and existing durable Engine operations. Contract evolution must remain schema-first. |
| Engine Agent event/effect/conversation tables | Already persist input/effect intent before effects, enforce sequence/idempotency rules and support checkpoints/branches. The Rust runtime should consume these rather than create a parallel SQLite truth. |
| v6 deterministic report verifier and evidence registries | Stronger than a fluent-only v5 completion path; successful publication remains fail-closed. |
| MoonBit formal models | Retain as an archival/specification and verification asset while removing it from the normal CLI runtime path. |

## PORT

| v5 capability | Rust recovery destination |
| --- | --- |
| `backend::ChatBackend` provider seam | Provider-independent `ModelProvider` in `crates/agent-runtime`, with MiniMax as the first adapter. |
| `orchestrator::AgentEngine` streaming tool loop | Shared runtime with typed public events, bounded rounds, cooperative cancellation, persisted effects and bounded parallel read-only tools. |
| `tools::ToolRegistry` fail-closed typed registry | Runtime registry whose entries map only to allowlisted Engine operations and declare risk, timeout, cache and freshness policy. |
| Prompt discipline | A smaller stable financial-analyst system contract retaining evidence labels, counter-evidence, uncertainty, current-data rules and the manual-trading boundary. |
| v5 resumable tasks, history compaction and evidence reports | Port incrementally over v6 event/checkpoint/conversation storage and the v6 evidence verifier. Do not copy the old persistence schema wholesale. |
| Mature v5 built-in/deep tool behavior | Map tool-by-tool to v6 Engine services. Port only missing semantics after comparing the current deterministic implementation. |
| v5 Tauri commands/events | Later thin Tauri v2 adapter over the same runtime API used by `astock`; no Agent state machine in React. |

## REWRITE

| Component | New rule |
| --- | --- |
| CLI/TUI | First-class `astock` Rust binary. Inline scrollback-friendly mode is the reliability baseline; non-TTY modes never emit ANSI or launch fullscreen UI. |
| Shared Agent facade | `AgentRuntime` owns planning, model/tool rounds, public events, verification and recovery policy. It depends on the Engine boundary, never Tauri, React or Ratatui. |
| Configuration/paths | Use OS-native directories. Linux follows XDG; Windows build scripts may still set `CARGO_TARGET_DIR` below `ASTOCK_BUILD_ROOT` without imposing that path on other platforms. |
| Credentials | Interactive adapters use no-echo, session-only prompts and may read an OS credential store. Values never enter config, environment variables, command arguments, task state, JSON, logs or tool arguments. Non-TTY use requires a preinstalled OS credential-store entry. |
| CI/release | Replace the Windows-only product authority incrementally with separate cross-platform Rust quality/CLI jobs, then Tauri builds and dual product release artifacts. Existing v6 publication evidence remains historical until its replacement is proven. |
| React bridge | Eventually regenerate a thin Tauri-facing typed API; React owns presentation only. |

## REMOVE (only after replacement gates pass)

- Proton/CEF and MoonBit Worker requirements from the normal production
  runtime, build and publication path.
- v6 architecture checks that deliberately reject Tauri or require the
  MoonBit Agent as the production orchestrator.
- Windows-only defaults in shared Cargo/Vite/common development paths.
- Renderer-side orchestration or duplicated task state machines.
- Obsolete v5 direct Tauri command implementations after equivalent shared
  runtime/Engine behavior has vertical tests.

No major subsystem is deleted during the first Rust CLI milestones. Removal
requires replacement evidence and preserved Git history.

## ARCHIVE

- Tag `v6.0.0` and its existing Git history are the immutable Proton/MoonBit
  production snapshot. A separate archival branch/tag is optional; no reset,
  tag move or history rewrite is needed to preserve it.
- `app-moon`, `desktop-moon`, Proton patches and v6 release scripts remain in
  tree until the Rust CLI and later Tauri paths cover their useful behavior.
- v5.0.3 remains the differential oracle for prompts, clarification, tool
  descriptions, provider behavior and user flows—not a source tree to restore
  wholesale.

## Target dependency graph

```text
astock (clap + inline terminal adapter) ─┐
                                        ├──> astock-agent-runtime
astock-terminal (React + thin Tauri v2) ┘          │
                                                   ├──> astock-engine
                                                   ├──> provider adapters
                                                   │       └──> astock-minimax
                                                   └──> generated protocol models

astock-engine ──> core/data/research/storage crates
              └─> astock-compute (pure bounded evaluator)
```

Forbidden edges are enforced by manifest review and will receive an
architecture test:

```text
agent-runtime -X-> Tauri / React / Ratatui
engine        -X-> Tauri / React / Ratatui
React         -X-> native invocation outside the generated bridge
```

The CLI may depend on terminal/UI crates and `agent-runtime`, but must not
reach domain crates directly. Tauri and CLI are adapters, not alternate Agent
implementations.

## Incremental proof plan

1. Linux workspace check from a clean tree.
2. Shared runtime unit tests with a scripted model provider, mock Engine tool
   executor and durable-store spy, including malformed streams and effect
   ordering.
3. Headless `astock ask` with deterministic JSON/plain/JSONL behavior and
   cooperative cancellation.
4. Real Engine aggregate integration using a temporary data root and mock
   model provider.
5. Complete exact-checkpoint resume, semantic compaction and broader
   fault-injection coverage (durable multi-turn continuation,
   checkpoint-verified immutable-history branching and deterministic
   extractive context compaction are implemented).
6. Linux release build and manual secret-gated MiniMax acceptance.
7. Windows/macOS CLI build and smoke matrix.
8. Thin Tauri restoration over the proven shared runtime, followed by a real
   GUI vertical test.

Reliability labels remain scoped: a Linux `cargo check` proves compilation,
not runtime data correctness; scripted vertical tests are integration tests,
not live-provider verification; external Provider and upstream behavior
remain trusted boundaries until explicit opt-in tests run.
