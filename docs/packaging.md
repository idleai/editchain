# Packaging and releases

Reusable crates are published as `.crate` assets in this repository's GitHub
Releases. The `cargo-index` branch serves the sparse Cargo index with immutable
archive checksums; [Cargo configuration](../.cargo/config.toml) registers it.

Normal checks use the committed lockfile and need only this repository's source.

## Automatic releases

The [Release workflow](../.github/workflows/release.yml) starts after successful
`main` CI:

1. Release-plz calculates versions and changelogs from commit messages and Rust
   API checks, then commits the release metadata to `main`.
2. The full CI workflow checks that version commit.
3. Crate archives, Cargo index entries and native bundles publish from that
   verified commit.

Release preparation uses a normal push, so a concurrent change to `main` cannot
be overwritten. Declare breaking changes and consumer minimum versions in the
feature PR. Documentation changes with no effect on packaged contents do not
create a new package version.

Use **Re-run failed jobs** on the original Release run to finish a failed
publication with its verified commit, even if `main` has advanced. Dispatch
**Release** on `main` to prepare current changes or resume its version commit.
Retries can finish draft releases; published versions and archives are immutable.

The [native release workflow](../.github/workflows/native-release.yml) builds
Linux x64, macOS x64/arm64 and Windows x64 bundles. It publishes the native draft
after all platform builds complete. [native-release.json](../native-release.json)
defines the producer's binaries and test support.

## Unpublished integration

For local Rust work, pass a temporary registry patch with
`cargo --config /absolute/path/local.toml ...`. Keep these overrides out of
committed manifests and lockfiles.

For a full check, use
[memos/scripts/check-integration.py](https://github.com/idleai/memos/blob/v1/scripts/check-integration.py)
with explicit `--producer` and `--consumer` checkout paths. It patches Cargo,
builds candidate native bundles when needed, runs the consumer's normal check
script and restores dependency files. The manual
[Unpublished package integration workflow](https://github.com/idleai/memos/blob/v1/.github/workflows/integration.yml)
runs the same check for selected branches.

## Registry maintenance

Dependabot version updates are paused. Its custom Cargo registry configuration
still requires the Dependabot secret `PUBLIC_CARGO_REGISTRY_TOKEN` set to the
literal value `anonymous`. This public marker is not an access token; the Cargo
indexes remain anonymously readable.
