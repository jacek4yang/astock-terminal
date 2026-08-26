# Configuration and credentials

Ordinary configuration uses platform-native directories through the Rust
`directories` crate. Run `astock config path` to see the authoritative path.

Typical locations:

| Platform | Configuration | Data | Cache |
| --- | --- | --- | --- |
| Linux | `~/.config/astock/config.toml` | `~/.local/share/astock` | `~/.cache/astock` |
| Windows | native roaming/local application directories | native application data | native cache |
| macOS | native Application Support/Preferences mapping | native Application Support | native cache |

`astock --config /absolute/or/relative/config.toml ...` selects a different
ordinary configuration file. The Engine receives an explicit native data path
from the adapter; it does not require adapters to mutate `ASTOCK_DATA_DIR`.

The accepted TOML sections are `[agent]`, `[provider.minimax]`, `[research]`,
`[network]` and `[tui]`. Unknown fields fail validation so misspellings do not
silently disable safeguards.

`provider.minimax.region` accepts `auto`, `cn` or `intl`; `auto` probes both
official services and caches the accepted region, while an explicit value
limits probing to that service. `provider.minimax.model = "auto"` probes the
bounded built-in preference chain. A concrete visible model ID replaces that
chain with exactly the configured model. Invalid region/model values fail
configuration validation before any provider request.

Secrets are intentionally absent from the schema. In a TTY, a missing
MiniMax key is requested with terminal echo disabled. An Agent run also asks
for optional JoinQuant credentials when no JoinQuant keyring entry is
available; Enter at the username prompt skips that provider. Prompted values
are session-only and flow directly into typed in-process adapters. They are
never put in command arguments, TOML, JSON/IPC, Agent events or SQLite.

When a usable OS credential backend exists, the MiniMax keyring slot is used
before prompting. The runtime wraps secret values in `SecretKey`, whose
`Debug`/`Display` are masked and whose Serde implementation refuses
serialization. Non-TTY invocations never try to read a credential prompt and
therefore require an OS credential-store entry. Credentials must not be
placed in environment variables.

Never store MiniMax keys, JoinQuant passwords, cookies or authenticated proxy
URLs in TOML. Proxy configuration accepts HTTP(S)/SOCKS endpoints and rejects
embedded `user:password@host` authority. The explicit proxy is applied to
buffered and streaming MiniMax clients.

Any credential pasted into chat, an issue, a commit or logs is considered
compromised and must be revoked before live acceptance testing.

The default `[agent]` policy is `tool_policy = "full"`. This exposes every
tool in the Runtime's fixed read-only registry to the model; it does not allow
unknown tools or execution/trading effects.
