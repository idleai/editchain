# Packaging and releases

Reusable crates are published as `.crate` assets in this repository's GitHub
Releases. The `cargo-index` branch serves the sparse Cargo index with immutable
archive checksums; [Cargo configuration](../.cargo/config.toml) registers it.

Normal checks use the committed lockfile and need only this repository's source.

## Automatic releases

The [Release workflow](../.github/workflows/release.yml) starts after successful
`main` CI:

1. Release-plz calculates versions and changelogs from commit messages and Rust
   API checks, then stages the release metadata on `ci/release/<source>`.
2. The Linux CI workflow checks that version commit before promotion to `main`.
3. Crate archives, Cargo index entries and native bundles publish from that
   verified commit.

Promotion uses a normal push. If newer work reaches `main` before the version
commit, the old run finishes successfully with publication skipped. It creates
no release tags, archives, index entries or native bundles. This also covers a
concurrent update between the final fetch and push. A rejected push with no
concurrent update still fails so authentication or server errors remain visible.

Use **Re-run failed jobs** to finish publication of a verified version commit
that already reached `main`, including after later work was merged. A superseded
candidate that never reached `main` stays skipped. Dispatch **Release** on `main`
to prepare the current source. Published versions and archives are immutable.

Declare breaking changes and consumer minimum versions in the feature PR.
Documentation changes with no effect on packaged contents do not create a new
package version.

The [native release workflow](../.github/workflows/native-release.yml) automatically
builds Linux x64 in an Ubuntu 22.04 container on the local runner pool, then
publishes the native draft. [native-release.json](../native-release.json) defines
the producer's binaries and test support. macOS and Windows bundles are added
through the manual workflow below.

## CI platforms and manual builds

Automatic pull-request, main, scheduled, and release runs select Linux only.
Trusted PR jobs use the local PR pool; other Linux release work uses
`IDLE_BACKGROUND_RUNNER` (default `idle-background-linux-x64`). Fork PRs use
GitHub-hosted Ubuntu. macOS and Windows native builds run only from the manual entrypoint.

Run [Full platform CI](../.github/workflows/full-platform.yml) from Actions with an
existing release tag, or use:

```sh
gh workflow run full-platform.yml --repo idleai/editchain --ref main -f tag=EXISTING_RELEASE_TAG
```

The run checks out that tag and uses its committed Cargo lockfile. It does not
prepare a new version commit. Complete this producer run before manually building
`host-tools` or other consumers that depend on its native bundles.

The manual run builds Linux x64, macOS x64/arm64, and Windows x64 bundles, then
attaches missing archives and checksums to the same release. Existing assets are
never overwritten. Set `publish` to `false` for validation with CI artifacts only.

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
