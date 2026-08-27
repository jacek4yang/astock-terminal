# AStock engineering rules

## Product identity

AStock is an evidence-driven A-share financial research Agent for human
decision support. It is not an automatic trading system. It does not log in to
brokers, route orders or buy/sell. A trading plan is a research artifact that
requires human review and manual execution. Do not add automatic order
placement, broker execution or background trading.

## Production architecture

There is exactly one production Agent implementation. Both user-facing
adapters drive the same Rust runtime:

```text
astock (terminal / SSH / CLI)  ─┐
                                ├──> astock-agent-runtime
astock-terminal (React + thin   │            │
Tauri v2 desktop adapter)      ─┘            ├──> astock-engine
                                             │       └──> domain/data crates
                                             ├──> provider adapters (astock-minimax)
                                             └──> astock-compute
```

- `astock-agent-runtime` owns conversational orchestration, canonical user
  intent, clarification, plan/TODO state, provider interaction, tool loops,
  retries, cancellation, context management, evidence review, report
  synthesis, the publication gate and task recovery.
- `astock-engine` owns deterministic market/data retrieval abstractions,
  financial and quantitative computation, evidence records, storage, graph,
  disclosures, news/event data, backtesting and verification services. It must
  not depend on a GUI framework.
- Adapters are presentation and input only. Neither the terminal nor React may
  reimplement orchestration, own task state or hold a second Agent loop.

Allowed and forbidden dependency edges are enforced by an architecture test,
not by convention alone:

```text
agent-runtime -> engine                 allowed
astock (CLI)  -> agent-runtime          allowed
Tauri adapter -> agent-runtime          allowed

agent-runtime -> Tauri / React / Ratatui   forbidden
engine        -> Tauri / React / Ratatui   forbidden
CLI           -> domain crates directly    forbidden
React         -> native calls outside the generated bridge   forbidden
```

### The Proton/MoonBit path is retired, and Git is its archive

The v6 Proton/CEF host and the MoonBit Agent worker are no longer the production
runtime, and they are no longer in the active tree. `app-moon`, `desktop-moon`,
`packaging-moon`, the v6 Proton/CEF release workflows and the v6 Windows
PowerShell build suite were removed from `main`.

- Every v6 source remains recoverable from the immutable `v6.0.0` tag and from
  Git history. Git is the archive; dead implementation code is not kept in the
  working tree to serve as one.
- Nothing in the v7 production path may require MoonBit, Proton or CEF — not to
  build, test, generate protocols, verify architecture, release or pass CI.
  `scripts/release-architecture-check.mjs` enforces this in both directions: the
  retired trees must not reappear, and no workflow may require the retired
  toolchain.
- The TLA+ `formal/` specifications and `docs/formal-verification.md` stay:
  language-independent specification is not an implementation of a retired
  runtime. Keep recording every `proof_axiomatized` use; never add an axiom only
  to make CI pass.
- The Agent capability surface is projected from the Rust registry into
  `protocol/agent-tool-manifest.json`, and a Rust test fails if it drifts. A
  retired implementation must never be the oracle for what the product can do.
- Tag `v6.0.0` and all published history are immutable. Never move a released
  tag, rewrite published history or force-push `main`.

Do not maintain two permanent production Agent runtimes.

## Interaction rules

Natural language is the primary interface. The product must not feel like a
CLI that happens to embed a model.

- A user who runs `astock` and types ordinary Chinese must get useful research
  without learning any command syntax.
- Slash commands are convenience aliases for experienced users. A slash
  command must not introduce a separate semantic execution path when the same
  action can be requested conversationally.
- Slash input and natural language must both resolve into the same canonical
  `UserIntent`, which is the only thing the runtime acts on. Do not write two
  handlers that can drift.
- Every research-oriented slash command needs a natural-language equivalent,
  and equivalence must be covered by tests.
- Purely local adapter operations, such as clearing the visible screen, may
  stay adapter-local but must never affect durable Agent truth.

Clarification follows the Codex pattern. When input is materially ambiguous,
ask one compact structured question with selectable options, mark a
`Recommended` option only when real context justifies it, normally offer
`Let Agent choose`, and normally offer `Other...` free text. Accept the answer
in any reasonable form: option letter, number, label text, a Chinese synonym,
an ordinal phrase, a delegation phrase or free text. Do not interrogate the
user: infer defaults that follow from conversation history, security context,
portfolio context, data availability or ordinary research practice, and ask
only when the answer materially changes the result.

Non-trivial research maintains a user-visible dynamic plan. The plan is an
execution artifact, not private chain-of-thought. The Agent may add, remove,
reorder, split, retry, block or degrade steps as evidence changes, and both
adapters consume the same plan events.

## Contracts and durability

- `protocol/schema` is the source of truth. Generated Rust and TypeScript
  contracts are refreshed together and `node protocol/codegen.mjs --check` must
  pass. There are no MoonBit outputs.
- IPC uses 4-byte little-endian length-prefixed UTF-8 JSON. stdout is
  protocol-only; diagnostics use stderr JSONL.
- Persist Agent events and tool intent before performing effects. Process
  duplicate, stale and replayed input idempotently. Never repeat a completed
  external effect unnecessarily after a crash.
- Never move unbounded result sets across a boundary. Use pages of at most 500
  rows, snapshot IDs, cache handles or content-addressed references.
- Never publish a successful Agent report while blocking verification findings
  remain.
- Semantic compaction preserves the full immutable original conversation,
  evidence IDs, important calculations, decisions, unresolved questions, user
  constraints and prior assumptions. A summary is historical context and must
  never silently become current market evidence.

## Financial data integrity

- Preserve security identity, source, timestamp, publication and retrieval
  time, revision, unit, currency, adjustment mode, freshness and quality for
  every important observation, with an evidence ID.
- Missing data stays missing. Never convert a missing value into zero, and
  never convert partial upstream failure into silent success.
- Prefer primary disclosures for company facts; use independent sources when
  cross-checking adds value. Route by capability rather than calling every
  provider blindly.
- Questions about current/latest/today state require established freshness. If
  only stale data exists, say so explicitly with its timestamp.
- Distinguish fact, calculation, inference, scenario and uncertainty in
  reports. Link important claims to evidence.
- Use ranges rather than fake precision. Never present an in-sample or
  research backtest as proof of future profitability.
- Classify reliability accurately: formally proved, model checked, property
  tested, integration tested, fault-injection tested, trusted boundary, or not
  verified.

## Rust

- Keep the deterministic core free of I/O where practical; confine network,
  clock, random, storage, process and logging behavior to the outer shell.
- Expose Engine capability through bounded typed tools. Never expose arbitrary
  filesystem access, process execution or model-generated code. The
  calculation language stays fuel-metered with no arbitrary execution.
- Keep tests deterministic by default. Live provider tests stay ignored and
  require explicit opt-in.

## Security and credentials

- Secrets belong in an OS credential store: Windows Credential Manager, or the
  platform keyring elsewhere. Interactive adapters use no-echo, session-only
  prompts.
- A credential must never enter Git, issues, PRs, logs, JSON output, durable
  Agent events, ordinary config, command arguments, React/Zustand state or IPC
  recordings. Proxy URLs with embedded credentials are rejected.
- Never ask a user to paste a live credential into ordinary Agent conversation
  when a secure mechanism exists. Any credential that has appeared in chat, a
  commit, an issue or a log is compromised and must be revoked rather than
  reused.
- Renderer permissions are deny-by-default. The main entry receives only the
  application bridge grant. External sources open in a zero-privilege window.
- Never commit `.kiro/`, Agent transcripts, `.env` secrets, `target/`,
  provider dumps or logs containing secrets. Run a non-printing credential
  scan before committing.

## Build and verification

- Linux is the reference development platform for the CLI and requires no
  Tauri, React, Node.js or graphical session.
- Windows and macOS build through plain `cargo` and, for the desktop adapter,
  the Tauri CLI. CI overrides build roots with runner temporary storage via
  `CARGO_TARGET_DIR`. Shared Cargo configuration must not impose a Windows drive
  on Linux or macOS. The v6 `scripts/*.ps1` Proton build suite is retired; it is
  recoverable from `v6.0.0` if v6 ever needs rebuilding.
- Rust gates:

```bash
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
```

- A successful compile is not a release gate. Do not ship untested platform
  artifacts, and never describe an unsigned binary as Authenticode-signed. If
  Authenticode is unavailable, label it `NOT PROVIDED` and use the explicit
  unsigned release policy.
- The v6 publication evidence stays valid for v6 and is recoverable from
  `v6.0.0`. The v7 release pipeline is v7-native and must produce its own
  equivalent evidence: SHA-256 sums, CycloneDX SBOM, third-party notices, build
  metadata, verification report and OIDC provenance/attestations. It must not
  reuse the Proton/CEF/MoonBit pipeline.
