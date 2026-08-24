# Agent verification boundary

`AgentLifecycle.tla` model-checks the bounded lifecycle used for request,
checkpoint, tool result, retry, crash/recovery, cancellation and publication.
Its invariants cover pending/completed separation, one durable result per tool,
bounded retries, cancellation/failure cleanup and evidence-gated publication.

`app-moon/agent_formal` contains executable MoonBit functions with real
`proof_require`/`proof_ensure` obligations. `moon prove` must report non-zero
valid goals, and the release gate requires successful Why3 sessions containing
both Z3 and cvc5 results.

These results do not prove the complete desktop application. Serialization,
SQLite durability, Windows process primitives, Provider HTTP behavior and the
mapping between the production reducer and the abstract formal model remain
`ASSUMED/TRUSTED BOUNDARY`. Runtime/property/fault-injection gates must cover
those boundaries separately.
