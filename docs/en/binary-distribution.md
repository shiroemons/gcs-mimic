# Native binary distribution

The distribution workflow is infrastructure for future releases. No public
binary release has been created or verified across all platforms yet. The first
release still requires the [roadmap's P1 and essential P2 scope](roadmap.md#first-useful-release).
Do not infer platform support from a configured matrix alone.

## Target matrix

Each target builds and runs its Rust tests on the corresponding CPU, including
the Linux musl tests. Cross-compilation alone is insufficient for publication.

**0.1.0 publishes only the four Linux targets.** The macOS and Windows native
test suites do not pass yet: macOS TLS tests and several Windows catalog tests
fail on GitHub runners. Those four rows stay supported by
`cargo xtask release prepare` but are removed from the release workflow matrix
until Foundation CI's `portability` job passes on them.

| OS / ABI | CPU | Rust target | Native runner | Archive |
| --- | --- | --- | --- | --- |
| macOS | Apple Silicon | `aarch64-apple-darwin` | `macos-15` | `.tar.gz` |
| macOS | Intel x86-64 | `x86_64-apple-darwin` | `macos-15-intel` | `.tar.gz` |
| Windows / MSVC | ARM64 | `aarch64-pc-windows-msvc` | `windows-11-arm` | `.zip` |
| Windows / MSVC | x86-64 | `x86_64-pc-windows-msvc` | `windows-2025` | `.zip` |
| Linux / GNU | ARM64 | `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | `.tar.gz` |
| Linux / GNU | x86-64 | `x86_64-unknown-linux-gnu` | `ubuntu-24.04` | `.tar.gz` |
| Linux / musl | ARM64 | `aarch64-unknown-linux-musl` | `ubuntu-24.04-arm` | `.tar.gz` |
| Linux / musl | x86-64 | `x86_64-unknown-linux-musl` | `ubuntu-24.04` | `.tar.gz` |

These runner OS versions are the initial verification environments, not a promise
that every older OS supported by Rust can run this application. GNU Linux builds
use Ubuntu 24.04's glibc baseline (2.39); older distributions should use the musl
variant. The exact required glibc symbols must be inspected in actual release
artifacts before promising a lower minimum. musl binaries use Rust's static
runtime and require no installed glibc. They still depend on Linux kernel and
filesystem behavior. macOS archives are separate binaries, not universal bundles.

Windows uses the static MSVC C runtime, including bundled SQLite. Native CI must
verify the executable's DLL imports before declaring that no Visual C++
Redistributable installation is required. Static runtime security fixes require
rebuilding and releasing a new application version. Signing, macOS notarization
and installer/package-manager integration are outside this foundation; unsigned
downloads can trigger OS trust prompts.

## Persistence and shutdown

Unix keeps file flushes, directory fsync and SQLite WAL with FULL synchronization.
Windows keeps file flushes, same-volume write-through blob publication, SQLite
WAL/FULL, exclusive directory ownership, and startup journal/orphan recovery.
Windows does not emulate a successful Unix directory fsync: directory creation
and deletion use Windows filesystem operations without that durability contract.

The Windows recovery scope is normal restart and process termination. Namespace
durability after an OS crash or power loss is not guaranteed. Unix power-loss
behavior also remains unverified. An acknowledged catalog entry whose blob is
missing prevents readiness; it is never silently discarded. Keep backups when
persistent development data matters. Both Windows Ctrl-C/Ctrl-Break and Unix
SIGINT/SIGTERM request graceful shutdown; the native suites verify shutdown and
reopening the same data directory.

## Artifacts and inputs

An archive is named `gcs-mimic-X.Y.Z-<target>` and contains the executable,
`LICENSE-MIT`, `LICENSE-APACHE`, third-party notices and `BUILD-INFO.json`.
Cargo dependency notices cover the locked workspace graph, including build-only
and other-platform dependencies; this conservative superset is not an exact
runtime bill of materials. Original notices, multiple license alternatives and
required NOTICE files are preserved. Rust's compiler/library copyright documents
and license texts are included. Linux adds GCC runtime notices; musl archives
also include the pinned upstream musl source archive to retain individual source
file notices. That source archive supplies notices, not a claim that the shipped
runtime is an unpatched upstream build.

Rust and Cargo.lock are pinned and Actions use commit SHAs. Builds remap checkout
and Cargo cache paths, and Unix archives normalize ownership rather than shipping
the builder's account name. BUILD-INFO records the packaging checkout, dirty-tree
state, requested target, compiler and runner information. Its version check alone
does not prove an arbitrary input binary's source or CPU/ABI; release CI builds
the exact target immediately before staging its fixed output path. Hosted runner
images and apt packages can change even under fixed runner labels. The current
workflow therefore supports traceable locked builds, not byte-identical rebuilds.
Refresh native runtime notices whenever compiler or system runtime inputs change.
Missing notice inputs stop packaging.
The musl notice input is reviewed for Rust 1.99.1; a compiler upgrade must review
and advance that version guard along with the bundled runtime's notices.

Release assets include one archive per release-matrix target (four Linux
archives in 0.1.0) and `SHA256SUMS`. Verify the selected
archive after downloading both from the same release:

```sh
sha256sum --ignore-missing --check SHA256SUMS
```

On macOS, use `shasum -a 256 <archive>`; on PowerShell, use
`Get-FileHash <archive> -Algorithm SHA256` and compare the corresponding entry.
Checksums detect corruption; they are not code signatures or independent proof
of origin.

## Verification and release flow

1. Update the root package version and Cargo.lock together according to the
   [release policy](release-policy.md). Reconcile the feature ledger, compatibility
   restrictions and migration instructions.
2. Write `docs/en/releases/X.Y.Z.md` with final changes, compatibility impact,
   tested platforms and known limitations. Confirm the first-release roadmap
   requirements manually; the existence of a notes file does not prove them.
3. Run `just check-dependencies` and `just pre-push` on the final source. Commit
   the reviewed version, lockfile and notes, then push through the agreed process.
4. Run **Release binaries** using GitHub Actions' manual dispatch on that commit.
   It runs freshness checks, the reusable Foundation checks and every release
   matrix target's native suite, then uploads verification archives as workflow artifacts. Manual
   dispatch never creates a public release. Inspect the results and archives.
5. When publication is authorized, create and push a new `vX.Y.Z` tag on the tested
   commit. The tag must match Cargo's stable `X.Y.Z` exactly. Tag builds repeat all
   gates and require the committed release notes before creating a normal GitHub
   Release. Build jobs have read permissions; only the final publish job can
   write release contents.
6. The publish job verifies archive count, checksums, unchanged remote tag SHA and
   absence of an existing release for the tag. Inspect the published assets and
   download checksums. Never replace an existing version's tag or assets; fixes
   need a new version. A failed or interrupted publication needs inspection before
   retrying, especially if GitHub has already created the release.

Packaging is also available as `cargo xtask release prepare TARGET BINARY OUTPUT_DIR`
or `just prepare-binary TARGET BINARY OUTPUT_DIR`. Build first with the pinned
toolchain and the intended target; the command checks the executable's product
version and refuses to overwrite an output directory. Linux requires the native
notice inputs prepared by the workflow. Packaging does not build, publish, create
tags or invent release notes. The
[GHCR workflow](container-distribution.md) follows successful tag-driven binary
Release creation; manual verification does not publish images.
For local redistribution, set `--remap-path-prefix` for the checkout and Cargo
cache before building, as the workflow does; staging an existing binary does not
remove embedded compiler paths.

## References

- [GitHub-hosted runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
- [Rust target support](https://doc.rust-lang.org/rustc/platform-support.html)
- [Rust static C runtime selection](https://doc.rust-lang.org/reference/linkage.html#static-and-dynamic-c-runtimes)
- [Windows write-through move](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw)
- [Windows file flushing](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers)
- [SQLite file URI processing](https://www.sqlite.org/uri.html)
