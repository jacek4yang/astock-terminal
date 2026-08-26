# Development

Linux is the reference Agent platform. Detect the host before assuming a
distribution or WSL behavior. Common Rust code must not depend on Windows
drive letters, `/mnt/c`, PowerShell, a display server or desktop DBus.

```bash
rustc --version
cargo --version
cargo check --workspace --all-targets
cargo test -p astock-agent-runtime -p astock --all-targets
cargo build --release -p astock
```

The repository-level Cargo config is platform-neutral. Windows packaging
scripts set `CARGO_TARGET_DIR` under `ASTOCK_BUILD_ROOT`; CI sets a runner-temp
target root. Heavy Linux builds use Cargo's native default or an explicit
Linux path supplied by the caller.

Use deterministic scripted Providers for normal development. Live MiniMax and
market-provider tests stay ignored/secret-gated and must never receive a key
through a command argument. A newly rotated local key may be supplied through
the launching shell for a manual acceptance run.

The v5.0.3 Agent can be inspected with `git show` and `git diff`, but files are
ported by behavior to the current Engine boundary. Do not reset the worktree
or bulk-restore `crates/agent`/`src-tauri`.

All source edits should preserve the dependency graph documented in
`rust-agent-recovery.md`. Domain computation belongs in Engine/domain crates;
orchestration belongs in Agent Runtime; terminal and desktop behavior belongs
in adapters.
