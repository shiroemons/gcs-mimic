# Native binary distribution foundation verification

Scope: `DIST-001` and `DIST-002`, verified locally on 2026-10-22. The host is macOS
ARM64. The eight-target native workflow is configured; remote execution and
public releases have not occurred. These requirements remain PARTIAL.

## Executed checks

- `cargo test --package xtask --lib release::tests --locked --offline`: PASS,
  six packaging tests. Unsupported target/output overwrite, version mismatch,
  notice retention, missing/escaping notices and native input requirements are
  covered, including the reviewed crc32c notice exception and a guard requiring
  musl runtime notice review when upgrading the Rust toolchain.
- `actionlint .github/workflows/ci.yaml .github/workflows/binaries.yaml .github/workflows/dependency-check.yaml`:
  PASS. This validates workflow syntax, not remote runtime behavior.
- Targeted catalog tests: PASS, six tests, including immutable URI serialization
  for Windows drive/UNC/extended paths and actual read-only opening with Unicode
  and URI-reserved filename characters on the local host.
- Targeted object storage tests: PASS, 21 tests, preserving local publication,
  reader/reclamation and recovery behavior.
- Targeted HTTP/foundation tests: PASS, 37 tests. Shutdown emits `stopped`, exits
  successfully and releases the directory for reopening. Windows-specific console
  events are compiled/executed only by the configured Windows lanes.
- `cargo fmt --all --check` and
  `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`: PASS.
- `just check`: PASS, full Rust/shell/generated-doc checks. After the final
  packaging changes, the xtask library suite passed all 17 tests and workspace
  clippy passed again. Runtime source did not change after this full check.
- `just check-sdk`: PASS, complete Go vet and uncached official SDK suite.
- `just check-containers`: PASS, native Linux ARM64 and emulated x86-64 build,
  health/network checks, bucket/object CRUD, persistence and SIGINT/SIGTERM.
  Each architecture ran once on the final runtime source and refreshed base.
- `just check-dependencies`: PASS, exit 0, all checked inputs CURRENT.
- Shellcheck and Justfile format checks: PASS.
- Markdownlint: PASS with MD013 disabled for existing long tables/commands and
  MD024 restricted to sibling headings because generated matrices repeat category
  names under different feature kinds.

The three Justfile check recipes are the same checks required by `just pre-push`;
they were invoked separately here. No push was performed.

## Local archive verification

A stripped, path-remapped `aarch64-apple-darwin` release build was staged with
`cargo xtask release prepare`, archived with normalized root ownership and
verified with `shasum -a 256 --check`. Result: PASS. The development archive is
2,349,676 compressed bytes with SHA-256
`b93babe2e1060caf271c7a194c183b61f45bcaa31b68cc9da7c3d69c147f3fd5`.
It is a local uncommitted-tree verification artifact, not a public release.

Archive inspection verified executable permissions, byte identity with the built
binary, both project licenses, Rust notices, all 55 registry package notice sets,
SQLite's public-domain notice, r-efi's AUTHORS terms and crc32c's Apache/zlib
notices. All members use UID/GID 0 and root names; no AppleDouble metadata,
absolute/traversing archive paths or machine-specific path prefixes were found
in file names or contents, including the executable. The packaged `--version`
returned `gcs-mimic 0.1.0`. BUILD-INFO explicitly marks the tree dirty and describes
the packaging context rather than claiming independently verified binary origin.

The published crc32c crate omits license bodies; its fixed upstream revision also
omits them. Its README and adapted zlib source are preserved, the declared
Apache-2.0 alternative is supplied as standard text, and the original zlib terms
are included. Changing the crate version requires notice review instead of
silently reusing this exception.

## Input refresh

The slice-start freshness check detected a new Distroless index after the prior
commit. The Dockerfile and dependency snapshot advance the runtime to
`sha256:83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf`,
with official amd64/arm64 manifests recorded in the snapshot. Historical size
measurements in `distroless.md` describe the earlier index, not this rebuild.

Windows bindings use Microsoft-maintained `windows-sys 0.61.2` (already locked
transitively); only Windows builds enable them. Upload/download artifact Actions
use verified release commit SHAs and join the existing daily freshness check.
musl attribution input is the official 1.2.5 source tarball, 1,080,786 bytes,
SHA-256 `a9a118bbe84d8764da0ea0d28b3ab3fae8477fc7e4085d90102b8596fc7c75e4`.
Rust 1.99.1's upstream build script identifies musl 1.2.5 plus security patches;
the source archive retains notices and is not represented as the patched source.

## Remaining limits

Windows native compilation, CRT import inspection, process-control events,
filesystem recovery and ARM64 execution have not run locally. macOS Intel and
native Linux GNU/musl publication artifacts also require their configured CI.
Windows namespace persistence after OS crash/power loss is not guaranteed; Unix
power loss remains unverified. Signing/notarization, origin attestations,
byte-identical rebuilds, installers and container registry publication are not
implemented by this slice. No tag, push or real release was created.
