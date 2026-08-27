# Agent verification boundary

`AgentLifecycle.tla` model-checks the bounded lifecycle used for request,
dynamic clarification, checkpoint, tool result, retry, bounded crash/restart,
cancellation and publication. Its invariants cover pending/completed
separation, one durable result per tool, bounded retries, cancellation/failure
cleanup and evidence-gated publication. Temporal properties additionally check
monotonic sequence numbers, terminal-state absorption and finite-range progress
to a terminal state or the explicit sequence bound.

> **v7 note.** The MoonBit implementation trees were retired from `main` with the
> Proton/CEF architecture and are recoverable from the immutable `v6.0.0` tag. The
> TLA+ specifications in this directory stay: a language-independent specification
> is not an implementation of a retired runtime.

`app-moon/agent_formal` contained executable MoonBit functions with real
`proof_require`/`proof_ensure` obligations. They separately name sequence and
round bounds, pending correspondence, one-time completion/result uniqueness,
cancellation, evidence-gated publication, terminal rejection, deterministic
replay, reconcile idempotence and compression structure completeness. `moon
prove` must report non-zero valid goals, and the release gate requires every
obligation to succeed independently in Why3 sessions containing Z3 and cvc5
results.

These results do not prove the complete desktop application. Serialization,
SQLite durability, Windows process primitives, Provider HTTP behavior and the
mapping between the production reducer and the abstract formal model remain
`ASSUMED/TRUSTED BOUNDARY`. Runtime/property/fault-injection gates must cover
those boundaries separately.
