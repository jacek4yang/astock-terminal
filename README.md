# AStock

AStock is an evidence-driven financial research Agent platform for A-share
research and human decision support. The product is being consolidated around
one Rust Agent Runtime and one deterministic Rust financial Engine:

- `astock`: cross-platform CLI and inline terminal Agent; Linux is the
  reference development platform.
- `astock-terminal`: desktop research terminal. The current v6 Proton/MoonBit
  implementation remains preserved while a thin Tauri v2 adapter is restored
  over the shared Rust runtime.

This is not an automatic trading system. AStock does not log in to brokers,
route orders or buy/sell automatically. Any trading plan is a research
artifact that requires review and manual execution.

## Current Rust CLI milestone

The recovery branch contains the first native Rust vertical slice:

- provider-independent streamed model interface with MiniMax as the first
  adapter;
- closed typed financial-tool registry backed by the existing Rust Engine;
- durable Agent events, checkpoints and effect intent/result ordering;
- bounded parallel read-only tools, cooperative cancellation and deadlines;
- deterministic report verification before a successful completion event;
- a fuel-metered financial calculation language, including a protected
  JoinQuant-data calculation path with no arbitrary code execution;
- durable multi-turn conversations with bounded model history, offline
  session listing/history inspection and interactive continuation;
- plain text, JSON and JSONL single-shot modes;
- XDG/native paths, hidden session credential prompts and an inline TTY mode;
- deterministic mock-provider vertical tests that do not consume paid APIs.

Exact interrupted-task replay, semantic model-generated compaction, deeper
tool-by-tool v5 parity, broader fault-injection coverage, the polished TUI and
shared Tauri GUI are active migration work. See [the recovery
map](docs/rust-agent-recovery.md) for the audited boundary and phase plan.

## Build on Linux

The CLI does not require Proton, CEF, Tauri, React, Node.js, MoonBit or a
graphical session.

```bash
cargo build --release -p astock
./target/release/astock version
./target/release/astock doctor
```

Run the deterministic Rust gates:

```bash
cargo fmt --all -- --check
cargo test -p astock-agent-runtime -p astock --all-targets
cargo clippy -p astock-agent-runtime -p astock --all-targets -- -D warnings
```

The full workspace remains buildable with:

```bash
cargo test --workspace
```

Windows product scripts continue to put build intermediates below
`ASTOCK_BUILD_ROOT`; shared Cargo configuration no longer imposes a Windows
drive on Linux or macOS.

## Configure MiniMax safely

Ordinary settings are read from the platform-native config path shown by:

```bash
astock config path
astock config validate
```

On Linux this normally resolves to `~/.config/astock/config.toml`; data and
cache use `~/.local/share/astock` and `~/.cache/astock`. A custom file can be
selected with `--config /path/to/config.toml`.

Never put API keys, passwords, authenticated proxy URLs or cookies in the
TOML file, command arguments or environment variables. When `astock`, `astock
chat`, `astock ask`, `astock models` or `astock quota` needs a missing MiniMax
key in a terminal, it asks for the key with echo disabled. Agent runs also
offer an optional hidden JoinQuant session prompt. Prompted values stay in
that process and are not persisted. Non-interactive use requires a credential
already installed in the OS credential store.

Do not paste live credentials into issues, commits, chat or logs. A credential
that has appeared in any of those places must be revoked rather than reused.
The value is wrapped in a non-serializable redacted type and is never written
to Agent events, SQLite, JSON output or tool arguments.

Example non-secret configuration:

```toml
[agent]
profile = "senior-analyst"
depth = "deep"
tool_policy = "full"
language = "zh-CN"
max_parallel_tools = 4

[provider.minimax]
region = "auto"
model = "auto"
timeout_secs = 120

[research]
strict_evidence = true
cross_source_check = true
verify_numeric_claims = true
counter_evidence = true
allow_backtest = true

[network]
proxy = "socks5h://127.0.0.1:1080"

[tui]
show_tools = true
show_evidence = true
stream = true
```

Proxy URLs with embedded credentials are rejected.

## Use the CLI

```bash
astock ask '分析紫金矿业当前投资价值'
astock ask --symbol 601899 --depth deep '分析目前风险收益比'
astock ask --json '分析沪深300市场状态'
astock ask --jsonl '研究最近一个月AI产业链变化'
printf '%s\n' '分析沪深300市场状态' | astock ask -
astock
astock sessions
astock history --json
astock resume
astock branch
astock compact
astock sources
astock cache
```

When stdout is not a TTY, AStock does not emit ANSI control sequences or open
a fullscreen interface. Diagnostics go to stderr. `--json` emits one final
object; `--jsonl` emits typed progress events. The default tool policy is
`full`, so every tool registered in the Rust Runtime is offered to the Agent;
the allowlist remains closed and read-only. `astock ask` creates a durable
conversation. `astock resume [SESSION_ID]` continues the latest or selected
conversation as a new task with bounded prior user/Agent messages in model
context; it does not yet restart an interrupted task from its exact effect
checkpoint.

`astock branch [SESSION_ID]` creates a new conversation at the latest message;
`--message-id` selects an earlier point. The Engine verifies the latest source
checkpoint when applicable, retains the original conversation unchanged and
clears executable task state in the new branch before any fresh research.
Long conversations keep every original message in SQLite while model context
is bounded to the latest 40 user/Agent messages and 120,000 characters.
`astock compact [SESSION_ID]` (or `/compact`) refreshes a deterministic,
extractive index of older messages without deleting them; the prompt marks
that index as historical context rather than current evidence.

Useful non-provider commands:

```bash
astock doctor
astock tools
astock sources
astock cache
astock version
```

`astock models` and `astock quota` are provider-backed account queries and
therefore require the MiniMax credential path.

## Data and evidence discipline

Security identity, source, timestamp, adjustment mode, units, currency,
quality and missing/conflicting observations remain explicit. Large datasets
are normalized and bounded by the Engine before model ingestion. The model may
plan, interpret and synthesize; deterministic Rust computes financial and
quantitative results.

A report with blocking verification findings is not emitted as a successful
completion. Partial upstream failure is surfaced as degraded coverage rather
than silently converted to success or zero. Current/latest questions require
current configured sources and exposed data timestamps.

Public financial upstreams can time out, rate-limit, change schema or return
incorrect data. Provenance and cross-source checks reduce but cannot eliminate
that risk. Historical backtests do not predict future returns.

## Architecture and development

- [Rust Agent recovery map](docs/rust-agent-recovery.md)
- [Bounded financial calculation language](docs/compute-language.md)
- [Current architecture](docs/architecture.md)
- [Data sources](docs/data-sources.md)
- [Data contracts](docs/data-contracts.md)
- [Agent protocol](docs/agent-protocol.md)
- [Runtime hardening history](docs/agent-runtime-hardening.md)
- [Quant methodology](docs/quant-methodology.md)

The v5.0.3 Rust Agent is used only as a differential oracle. The current v6
Engine, evidence registry, report verifier and durable event/effect store are
preserved and consumed by the new runtime rather than replaced wholesale.
