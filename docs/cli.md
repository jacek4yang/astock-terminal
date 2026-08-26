# `astock` CLI

`astock` is the reference adapter for the shared Rust Agent Runtime. It is an
inline, scrollback-friendly terminal application and has no graphical runtime
dependency.

## Commands implemented in the first vertical slice

```text
astock
astock chat
astock ask [--symbol CODE] [--depth DEPTH] [--json|--jsonl] QUERY
astock resume [SESSION_ID]
astock sessions [--limit N] [--query TEXT] [--json]
astock history [SESSION_ID] [--json]
astock branch [SESSION_ID] [--message-id ID] [--title TITLE] [--json]
astock compact [SESSION_ID] [--json]
astock sources [--limit N] [--json]
astock cache [--json]
astock config path
astock config validate
astock doctor
astock tools
astock models
astock quota
astock version
```

`QUERY` may be `-`, in which case UTF-8 input is read to EOF from stdin.
Depth is `fast`, `balanced`, `deep` or `exhaustive`.

Every `ask` or interactive turn is stored as a durable conversation plus a
separate task. `resume` opens the latest or selected conversation and starts
future input as a new task with bounded prior messages in model context.
`sessions` and `history` are provider-free reads and therefore never request
API credentials. `branch` creates a new immutable-history branch at the latest
or selected message, verifies the latest durable task/checkpoint pair when
applicable, and clears executable task state in the branch. Interactive
commands include `/new`, `/resume [id]`, `/branch [message-id]`, `/sessions`,
`/history`, `/compact`, `/depth [level]`, `/tools`, `/sources`, `/cache`,
`/evidence`, `/context`, `/status`, `/clear` and `/exit`.

`sources` reads a bounded page (maximum 500) from the local versioned evidence
archive. `cache` reports deterministic local storage counters. Both are
read-only and provider-free; cache deletion is not implicitly authorized by a
status command.

Model context is automatically bounded to the latest 40 user/Agent messages
and 120,000 characters. When earlier messages fall outside that window, the
Runtime stores and supplies a deterministic extractive index capped at 30,000
characters. `compact` refreshes that index explicitly. Full durable messages
are not deleted, and the model is told that the index is historical context,
not current evidence.

This continuation boundary is deliberately narrower than exact task replay:
an interrupted/suspended task remains durable and inspectable, but resuming
its precise checkpoint/effect frontier, clarification, semantic/model-assisted
compaction and cache administration are still planned.

## Output contracts

- Interactive TTY: streamed visible text plus concise tool/evidence progress.
- Non-TTY plain mode: one final report, no ANSI escapes.
- `--json`: one final object with session ID, task ID, status, report and
  evidence IDs.
- `--jsonl`: one JSON object per typed Agent event, with session and task IDs.
- Diagnostics and structured tracing use stderr.

Exit status categories:

| Code | Meaning |
| --- | --- |
| 0 | Completed and passed the configured publication gate |
| 1 | Internal/model-loop failure |
| 2 | Invalid configuration or missing credential |
| 3 | Provider/auth/quota/network failure |
| 4 | Engine/tool/store failure |
| 5 | Blocking report verification findings |
| 130 | Cooperative cancellation |

The runtime persists a terminal `Suspended` event for retryable provider/quota
failure and a `Failed` event for non-retryable failure. Conversation
continuation is available; exact suspended-task checkpoint replay is a later
milestone.

## Terminal behavior

The current inline UI does not enter raw/alternate-screen mode, so panic or
process termination cannot leave the terminal in an alternate buffer. Ctrl+C
cancels the current runtime token; Ctrl+D exits an interactive input loop.
`/clear` emits ANSI only after both stdin and stdout have been confirmed as
TTYs.

Missing MiniMax and optional JoinQuant credentials are requested on stderr so
stdout JSON contracts remain intact. Secret fields disable terminal echo and
prompted values are held for the current process only. Non-TTY execution fails
with an actionable missing-credential message instead of blocking on stdin.

The default task policy is `full`: all tools shown by `astock tools` are made
available to the Agent. That registry is a fixed read-only allowlist, and
unknown model-requested tools fail closed.
