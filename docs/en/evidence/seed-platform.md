# Linux and Compose seed verification

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

Verified on **2026-10-22** for the native initial-seed contract. The runtime,
tests, dependency locks, Dockerfile and Compose configuration are unchanged.
All earlier evidence files remain unchanged, including the historical
[native seed results](seed.md) and [range/seed integration](seed-range.md).
This evidence closes their previously pending Linux and Compose checks.

## Environment and results

The host is macOS ARM64 with Docker Desktop's Linux ARM64 engine. Docker client
and engine are 29.8.1; Compose is 5.5.1. ARM64 containers execute on the native
CPU architecture; AMD64 containers use emulation. These are local container
results, not a native AMD64 or remote-CI result.

| Check | Result | Evidence |
| --- | --- | --- |
| Linux ARM64, network disabled: `cargo +1.99.1 test --locked --offline --test seed` | PASS: 12 tests, including invalid-byte fixture rejection | [Linux output](seed-platform-linux.txt) |
| `bash scripts/smoke-containers.sh linux/arm64 linux/amd64` | PASS: both source-built images and both seeded Compose journeys | [Container output](seed-platform-containers.txt) |
| `just check` | PASS: fmt, clippy, 240 Rust tests, ledger and shell checks | [Native check output](seed-platform-checks.txt) |
| `just check-dependencies` | PASS: all checked inputs CURRENT | [Freshness output](seed-platform-dependencies.txt) |
| Fixture preservation | PASS: checked-in filenames and bytes equal the starting checkout | Source mounted read-only; independent post-run comparison |

The existing shell script's seeded section is named `container-seed-lifecycle`
in the ledger. This is an acceptance-group identifier, not a Rust test function.
For each architecture its assertions verify:

1. `up --wait` reaches healthy readiness with the fixture available.
2. The fixture bind mount has `RW=false`, and imported bytes match the source.
3. User-overwritten bytes survive stop/start of the same container.
4. Those bytes also survive container recreation with the existing volume.
5. Deleting the object and every fixture bucket survives recreation without
   replaying the fixture tree.
6. Deliberately removing only the isolated test volume restores the original
   fixture on the next startup.

The script removes its own containers, networks and volumes. Existing developer
projects and data volumes are not used. Fixture names and bytes were compared
with the starting checkout after the run and were unchanged.

The same run also passes existing bucket/media, host/peer connectivity,
configured resource-limit and signal-shutdown assertions. It makes no measured
RSS, CPU, throughput or power-loss claim.

## Reproducing the Linux test

Use the exact builder image already pinned by the Dockerfile and a populated
host Cargo registry. The checkout and registry are read-only; compilation and
invalid-byte fixtures live in the container filesystem. Test execution has no
external network. Image acquisition is a separate prerequisite.

```sh
seed_builder=$(sed -n 's/^FROM \(rust:[^ ]*\) AS build$/\1/p' Dockerfile)
test -n "$seed_builder"
docker run --rm --platform linux/arm64 --network none --memory 2g --cpus 2 \
  --mount "type=bind,source=$PWD,target=/checkout,readonly" \
  --volume "$HOME/.cargo/registry:/usr/local/cargo/registry:ro" \
  --env CARGO_TARGET_DIR=/seed-target --workdir /checkout \
  "$seed_builder" sh -ec '
    printf "#!/bin/sh\nkill \"\$@\"\n" >/bin/kill
    chmod +x /bin/kill
    uname -sm
    rustc +1.99.1 -V
    cargo +1.99.1 -V
    cargo +1.99.1 test --locked --offline --test seed
  '
```

The slim builder lacks an external `kill` executable. The initial environment
attempt reported `3 passed; 9 failed`, with `No such file or directory` at
`tests/common/mod.rs:65` while stopping child servers. The wrapper above exposes
the standard shell's signal operation; the final unmodified suite passes all
12 tests. No test, assertion or negative case was skipped or weakened.
Explicit toolchain selection also avoids rustup component synchronization during
the offline test.

## Completion and remaining scope

- **UX-002 is IMPLEMENTED:** all 14 declared acceptance cases pass, including
  Linux invalid-byte names and seeded Compose. Existing native evidence stays
  attached; fixed octet-stream MIME and stable read-only input remain the scope.
- **UX-009 is IMPLEMENTED:** all declared stop/reset cases pass. Reset is
  explicit isolated-volume removal; no admin reset API is implied.
- **DESIGN-062 is PARTIAL:** directory import and read-only lifecycle are
  evidenced; the pre-existing manifest-metadata requirement remains pending.
- **DESIGN-063 remains PARTIAL:** its seed/reset case passes, while the existing
  power-loss and native AMD64 verification limits remain visible.
- **UX-008 remains PARTIAL:** seed replay prevention is verified; persisted
  permissions and broader durability remain outside the completed scope.

No existing source-format or Google API gap was closed by these tests. Source
mutation protection, atomic whole-directory import, legacy seed/default parity,
metadata manifests, Windows execution and host power-loss recovery were not
verified. No SDK or real-cloud differential suite was rerun, and no resource
benchmark was performed. The next implementation slice is bucket IAM policies.

Raw logs retain their original progress output, including trailing spaces;
only local path prefixes are replaced as stated in each log's first line.
