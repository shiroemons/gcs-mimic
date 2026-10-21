# GHCR publication foundation verification

Scope: `DIST-003`, on 2026-10-22. Publication remains PARTIAL. No registry writes,
new release tags or GitHub Releases were performed during this slice.

## Pushed baseline

The prior native distribution work was committed on main as `24c9392` (Windows
runtime support) and `f7ac025` (distribution infrastructure), then pushed.
[Foundation run 39637777841](https://github.com/shiroemons/gcs-mimic/actions/runs/39637777841)
completed successfully at `f7ac025285055404492ecbad2515669fbfd4af8a`:

- pinned Rust checks: success;
- current-stable Rust checks: success;
- Go SDK checks: success;
- native amd64 container smoke on `ubuntu-24.04`: success;
- native arm64 container smoke on `ubuntu-24.04-arm`: success.

The eight native archive lanes were not dispatched. This run establishes the
prior baseline, not remote execution of the current GHCR changes.

## Executed policy and notice checks

`cargo test --package xtask --lib --locked --offline`: PASS, 20 tests. New cases
verify numeric alias ordering, rejection of malformed/prerelease versions, and
notice extraction without Git or an executable. The existing packaging and
dependency tests remain enabled.

`just check`: PASS on the final Rust and shell sources: shared-check fail-fast
tests, formatting, Clippy with warnings denied, workspace tests, generated-ledger
validation, shell syntax and offline publication checks. `just check-sdk`: PASS,
including the pinned Go SDK bucket/object lifecycle cases.

`bash scripts/test-container-release.sh`: PASS without registry or GitHub writes.
Cases cover invalid/extra digest inputs, API failures, private visibility, moved
source tags, incorrect platforms, absent attestations, SLSA v1/v0.2 predicates,
immutable-version and
digest-asset mismatches, identical-input retry, aliases, and anonymous-access
failure/recovery. Mocked external commands exercise local control flow; they do
not establish the behavior of the real services.

The fixed Distroless arm64 manifest's 21 layers were read and SHA-256 verified.
They contain Debian OS package copyright files and common license texts;
libgcc/libstdc++/libgomp notice symlinks resolve to the retained gcc-14-base text.
The image keeps these OS files and adds Cargo/Rust/native notices separately.

BuildKit `v0.34.1` and the official `stable-1` SBOM generator are pinned by index
and amd64/arm64 digests in the dependency snapshot. The online freshness check
passed with all inputs CURRENT, including their floating update selectors.

## Executed local image checks

The final Docker source built with pinned BuildKit `v0.34.1` and Buildx `v0.37.1`.
Both `linux/arm64` and `linux/amd64` images passed the existing complete
`bash scripts/smoke-containers.sh --image "$image" "$platform"` suite: isolated
health checks, host/DNS access, bucket/object media CRUD, restart and recreated
container persistence, deliberate volume reset, resource limits, SIGINT and
SIGTERM. The prebuilt-image option tests the image that will be exported rather
than building a separate image for verification.

For each CPU, the same builder and cache exported an OCI archive using
`--provenance=mode=max` and the pinned SBOM generator. Archive inspection passed:

- The runtime manifest's config digest equals the SHA-256 of the raw config
  saved from the tested Docker image. A containerd image store can report a
  manifest digest as `.Id`; the workflow therefore hashes the saved config and
  reads the pushed runtime manifest rather than assuming metadata contains a
  config digest.
- Both project licenses, the 55-package Cargo notice index, Rust/native notices,
  Debian libc/GCC copyright files and common license texts are retained.
- The platform index includes an SPDX document and SLSA provenance attestation.

Temporary builders and loaded verification images were removed after success.
This verifies local OCI export and behavior, not the registry export path or
new native-runner executions of these sources.

`actionlint` on all workflows, `shellcheck` on the changed scripts, Markdown
lint, `git diff --check`, and the local-path scan: PASS. Markdown lint disables
MD013 for existing long tables/commands and restricts MD024 to sibling headings
because generated matrices repeat headings in separate categories.

## Limits

Actual GHCR permissions, initial package visibility, remote multi-platform
publication, anonymous access, registry recovery and GitHub Release digest-asset
attachment remain unverified. The configured retry comparisons and tests are not
a claim that these remote operations have run. Signing, vulnerability scanning
and byte-identical independent rebuilds are outside this slice. The local host
is macOS ARM64; local amd64 container execution uses emulation.
