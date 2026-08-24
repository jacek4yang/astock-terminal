# Migration baseline

Captured before the Proton/MoonBit migration.

## Repository baseline

- Branch: `main`
- Baseline revision: `7aa0f03`
- UI: React 18 + Vite 6 + ECharts 5, route/page shell, direct Tauri API bridge.
- Native shell: Tauri 2 with more than one hundred command registrations.
- Agent: Rust orchestration with durable reports, evidence validation, recovery, quota suspension and deterministic context compaction.

## Verified baseline

- `npm --prefix ui test`: 18 files, 54 tests passed.
- `npm --prefix ui run build`: passed; the largest generated chunk was ECharts at about 1.04 MB.
- `cargo test --workspace` using the historical C: target stopped when Windows could not execute a missing `adjust_live` test binary; no Rust assertion failed before that point.
- Re-running that target with `CARGO_TARGET_DIR=D:\astock-build\astock-terminal\cargo-target` passed, confirming the external build volume is viable.

The first migration gate is a complete workspace run from D:. Existing Tauri and Rust Agent code remains a differential baseline until the replacement passes its subsystem gates.

The command/domain migration and provider non-regression gate is maintained in
[`data-source-parity.md`](data-source-parity.md). A compiling legacy crate does
not count as a migrated Proton/Engine capability.
