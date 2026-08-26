# Next release gate: current status

Assessed against `main` at `eede42c`. This records which release gates pass
today and which are blocked, with the evidence for each, so the decision to
publish is made on facts rather than optimism. A successful compile is not a
release gate.

## Version policy

`v6.0.0` and all published history are immutable. The workspace and `ui`
package are converged at `6.0.0`.

The production Agent runtime changed identity in this cycle: orchestration moved
from the MoonBit Agent Worker to `astock-agent-runtime`, the CLI's control plane
was replaced by a canonical intent layer, and the desktop host direction changed
from Proton/CEF to a thin Tauri v2 adapter. Those are breaking architectural
changes, so the next stable release is a major one, `v7.0.0`.

## Gate status

| Gate | State | Evidence |
| --- | --- | --- |
| `main` clean | pass | Working tree clean; remote branches are `main` only. |
| version converged | pass | `Cargo.toml` and `ui/package.json` both `6.0.0`; `release-version-check` runs in CI. |
| Rust workspace green | pass | `cargo test --locked --workspace`: 824 passed, 0 failed, 29 ignored. `fmt`, `check` and `clippy -D warnings` clean with `--locked --workspace --all-targets --all-features`. |
| Agent tests green | pass | 38 runtime unit, 13 mock-provider vertical, 8 architecture. |
| natural-language/slash equivalence | pass | 22 tests in `crates/agent-runtime/tests/intent_equivalence.rs`; also confirmed byte-identical adapter output through a pty. |
| frontend green | pass | `frontend` job green in CI. |
| security green | pass | `security-audit` job green (RustSec); `cargo deny check` reports advisories, bans, licenses and sources ok. |
| CLI cross-platform | pass | `rust-cli` green on `ubuntu-latest`, `macos-latest` and `windows-2022`; full workspace green on Windows via `rust-windows`. |
| **Tauri build green** | **blocked** | There is no Tauri v2 adapter in the tree. No `tauri.conf.json` exists, and no `tauri` dependency appears in any Rust manifest or in `ui/package.json`. The only desktop pipeline, `release-unsigned-attested`, builds the Proton/CEF plus MoonBit product, which is the superseded architecture. |
| **live MiniMax smoke** | **blocked** | No credential is available. `astock doctor` reports `MiniMax credential: not configured`. |
| **live JoinQuant smoke** | **blocked** | Same; `astock doctor` reports `JoinQuant credential: not configured`. |
| **installation smoke** | **blocked** | Depends on a desktop package that cannot be built, and on a Windows host for a genuine install rather than a cross-compile. |
| **upgrade/migration smoke** | **blocked** | Requires a prior release of this architecture to upgrade from. |
| release artifacts, SBOM, hashes, provenance | not started | Gated behind the blocked items above. |

## Why the tag has not been created

Section 37 of the release policy is explicit that the final tag is not created
until every gate passes, and that a successful compile alone is insufficient.
Four gates are blocked by missing inputs rather than by unfinished work in this
repository, so publishing `v7.0.0` now would mean either shipping untested
platform artifacts or describing unverified behaviour as verified. Both are
prohibited.

## What would unblock each item

**Live provider acceptance.** A MiniMax API key and a JoinQuant account
installed through the OS credential store, not pasted into conversation. Any
credential that has appeared in a chat, commit, issue or log is compromised and
must be revoked rather than reused; the credentials supplied earlier in this
project's history fall into that category and were deliberately not used.

**Desktop artifact.** The thin Tauri v2 adapter has to be written first: a
`tauri.conf.json`, a Tauri crate depending only on `astock-agent-runtime`, and a
generated typed bridge so React holds no orchestration. The architecture test
already encodes the constraint that this adapter must not reimplement Agent
logic. After that, a Windows host is needed to test the produced MSI/NSIS
package rather than merely produce it.

**Product policy decision.** Release policy currently makes the Windows desktop
application mandatory for a stable release. If a CLI-only release is acceptable
in the interim, that is a deliberate policy change and needs an explicit owner
decision; it is not something to assume. A CLI-only `v7.0.0-rc` covering Linux,
macOS and Windows is defensible on today's evidence, because those three targets
are green in CI, but it would still need its own SHA-256 sums, CycloneDX SBOM,
third-party notices, build metadata, verification report and provenance.

## Reliability labels for what is claimed

- Integration tested: intent resolution, clarification normalisation, plan
  mutation, mock-provider tool loops, cancellation, credential round-trip on the
  host platform, CLI JSON output, cross-platform CLI build and test.
- Property tested by manifest inspection: the architecture dependency edges.
- Trusted boundary, not verified: real MiniMax behaviour, real market upstreams,
  JoinQuant.
- Not verified: desktop packaging, install and upgrade paths, cross-reboot
  credential persistence on Linux, and any claim about live data correctness.
