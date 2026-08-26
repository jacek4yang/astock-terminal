# Bounded financial calculation language

`astock-finance-calc/v1` is the Agent's built-in, deterministic calculation
system. It is a typed JSON AST executed by `astock-compute` inside the Rust
Engine. It is intentionally not Python, JavaScript, shell, SQL or `eval`.

The language has ordered `bindings` (similar to `let` declarations), named
numeric-series `inputs` and one or more `outputs`. Expressions may compose:

- scalar/series `add`, `sub`, `mul`, `div`, `neg`, `abs` and `clip`;
- `lag`, `diff`, arithmetic/log `returns` and `cumulative_return`;
- `sma`, `ema`, `rolling_std`, rolling `zscore` and `rsi`;
- `tail`, `mean`, population `std`, `sum`, `min`, `max`, `last`, `count`;
- pairwise `correlation` and `max_drawdown`.

Example:

```json
{
  "version": 1,
  "inputs": { "close": [100.0, 110.0, 99.0] },
  "bindings": [
    {
      "name": "ret",
      "expr": {
        "op": "returns",
        "input": { "op": "var", "name": "close" }
      }
    }
  ],
  "outputs": {
    "mean_return": {
      "op": "mean",
      "input": { "op": "var", "name": "ret" }
    },
    "max_drawdown": {
      "op": "max_drawdown",
      "input": { "op": "var", "name": "close" }
    }
  }
}
```

The evaluator rejects unknown JSON fields and variables, duplicate names,
non-finite inputs, oversized series, excessive AST depth/node count and work
beyond its fuel budget. Division by zero and mathematically undefined results
become explicit `null`. Programs and results are bounded; each accepted
program receives a stable SHA-256 fingerprint.

## JoinQuant calculation path

`run_joinquant_calculation` accepts a symbol, an explicit research date range
of at most five years and the same program AST. The Engine obtains a bounded
前复权 daily series through the existing allowlisted JoinQuant adapter, then
injects these protected variables:

```text
open high low close volume amount turnover pct
```

The program is forbidden from defining those names itself. Dates, source,
retrieval time, adjustment mode and program fingerprint are returned with the
execution, and the result is registered as field-level evidence. Model-authored
code is never sent to the JoinQuant kernel: only existing typed data queries
run remotely, while calculations run locally in Rust.

This provides useful Agent autonomy without granting access to files, local
processes, arbitrary network destinations, clocks, randomness, dynamic
libraries or remote code execution. Strategy creation and backtesting remain
separate typed capabilities, and neither path can place an order.
