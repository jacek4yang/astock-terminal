# Testing

Normal CI is deterministic and does not consume paid Provider quota.

The Rust Agent vertical suite covers:

- prompt → streamed model turn → typed tool call → Engine-shaped evidence →
  synthesis → deterministic verifier → completed report;
- durable tool intent before tool execution and checkpoint after each event;
- real embedded Engine SQLite task projection with a temporary data root;
- bounded calculation AST math canaries, limits and deterministic SHA-256;
- Agent → calculation tool → embedded Engine → evidence registry flow;
- protected JoinQuant input injection that rejects model-forged OHLCV names;
- two durable conversation turns through the embedded Engine, reload/list,
  bounded prior-message rehydration into the next model request, and a
  checkpoint-verified branch whose executable task state is cleared;
- long-session extractive compaction that retains all stored messages while
  sending only a labeled summary and bounded recent window to the model;
- provider authentication failure → `Failed`, rate limit/idle timeout →
  `Suspended`, immediate cancellation of a pending stream and a cooperative
  Engine tool, and bounded oversized-tool failure returned to the next model
  round;
- Engine task-event and effect-ledger cursor pagination with pages capped at
  500, plus provider-independent visible-response limit enforcement;
- unknown tool fail-closed behavior;
- fragmented tool-call argument reconstruction;
- fragmented private-reasoning suppression;
- optional field omission on serialized runtime tasks.

Run it with:

```bash
cargo test -p astock-compute -p astock-engine -p astock-agent-runtime -p astock --all-targets
```

The cross-platform `rust-cli` GitHub Actions matrix runs calculation/runtime/
CLI tests, Clippy, build and a non-provider smoke command on Linux, Windows
and macOS.
Linux remains the target for the deeper future headless and fault-injection
suite beyond the deterministic cases already listed above.

Still required before CLI/TUI v1: deterministic clarification, multi-tool
rounds including partial failures, malformed/partial SSE framing, cancellation
during a cooperative Engine tool, SIGTERM and exact-checkpoint task recovery,
SQLite busy/corruption, context exhaustion/compaction, invalid upstream UTF-8,
TTY resize/input and command-level JSON/JSONL golden tests.

Live source tests prove only the observed service/date/environment. They do not
replace deterministic regression tests or establish future availability.
Live tests must use newly issued credentials entered locally through the
hidden prompt or loaded from the OS credential store. Values previously
exposed in chat, tickets or logs are never valid acceptance-test inputs.

TTY smoke coverage verifies that secret prompts disable echo and that
non-TTY missing-credential execution exits with code 2 instead of reading or
blocking on stdin. These are integration-tested terminal behaviors, not a
claim that an upstream account authenticated successfully.
