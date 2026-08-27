# Releases

The current immutable `v6.0.0` release remains the historical Proton/CEF
desktop product. Its tag must not move and its Authenticode/unsigned attested
claims must not be rewritten.

The cross-platform recovery will eventually publish two product families:

- `astock` CLI archives for Linux x86_64/aarch64, Windows x86_64 and macOS
  x86_64/arm64;
- AStock Terminal installers/packages built from the thin Tauri v2 adapter.

No platform artifact is promised until its build and minimum runtime
acceptance pass in GitHub Actions. Release assets require `SHA256SUMS`, an
SBOM and the applicable provenance/attestation. Authenticode must be described
as `NOT PROVIDED` unless every shipped PE has independently verified `Valid`
status.

The first recovery branch is not a publishable cross-platform release: TUI
hardening, exact interrupted-task replay, remaining fault coverage, platform
CI evidence and the shared Tauri adapter remain incomplete. Durable
conversation continuation, branching and bounded context compaction are
integration tested but do not by themselves satisfy the release gate. The old
The v6 publication entry (`scripts/publish-v6.ps1`) was retired from `main` with
the rest of the Proton/CEF/MoonBit suite and is recoverable from the immutable
`v6.0.0` tag. It remains the record for the immutable v6 product
until a separately reviewed next-version release workflow replaces it.
