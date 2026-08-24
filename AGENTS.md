# AStock Terminal engineering rules

## Architecture boundaries

- React is a CEF renderer. Components call the typed bridge in `ui/src/bridge`; they never import Tauri or invoke native commands directly.
- The Proton/MoonBit desktop host owns windows, permissions, IPC routing and worker supervision only.
- The MoonBit Agent worker owns provider-independent orchestration, task state, retry/recovery policy, evidence policy and report publication rules.
- The Rust Engine owns deterministic financial/data algorithms, SQLite/Parquet persistence and Windows credential writes. It must not depend on a GUI framework.
- Do not add automatic order placement, broker execution, or background trading. Trading plans are research artifacts requiring human action.

## Contracts and durability

- `protocol/schema` is the source of truth. Generated Rust, MoonBit and TypeScript contracts must be refreshed together and `protocol-codegen --check` must pass.
- IPC uses 4-byte little-endian length-prefixed UTF-8 JSON. stdout is protocol-only; diagnostics use stderr JSONL.
- Persist Agent events and tool intent before performing effects. Process duplicate, stale and replayed input idempotently.
- Never move unbounded result sets over IPC. Use pages of at most 500 rows, snapshot IDs, cache handles or content-addressed references.
- Never publish a successful Agent report while blocking verification findings remain.

## MoonBit

- The Agent reducer must remain pure: `State + Event -> State + Effects`.
- Keep network, clock, random, storage, process and logging behavior in the imperative shell.
- Pin MoonBit dependencies. Verify current APIs from installed package sources before adopting examples.
- Every `proof_axiomatized` use must be recorded in `docs/formal-verification.md`; never add an axiom only to make CI pass.

## Proton and security

- Use Proton 0.2.1 with its pinned CEF runtime. CEF is mandatory; do not add WebView2/Tauri fallback code.
- Renderer permissions are deny-by-default. The main entry receives only the application bridge grant. External sources open in a zero-privilege window.
- Secrets belong in Windows Credential Manager. Never put a credential in command arguments, environment variables, JSON persistence, logs, React/Zustand state or IPC recordings.

## Build and verification

- On a local Windows workstation use `scripts/*.ps1`; build products and intermediates must remain under `ASTOCK_BUILD_ROOT` (default `D:\astock-build\astock-terminal`).
- Do not silently fall back to C:. CI must explicitly override build roots with runner temporary storage.
- Keep tests deterministic by default. Live provider tests stay ignored and require an explicit opt-in.
- Classify reliability claims accurately: formally proved, model checked, property tested, integration tested, fault-injection tested, trusted boundary, or not verified.
