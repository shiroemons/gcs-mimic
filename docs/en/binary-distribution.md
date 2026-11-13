# Native binary distribution

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

GitHub Releases from 0.8.0 publish native archives for the targets below.
Earlier releases were [withdrawn](release-policy.md#withdrawn-releases).
Do not infer platform support from the supported packaging targets alone;
only targets in the release matrix are built, tested and published.

## Target matrix

Each target builds and runs its Rust tests on the corresponding CPU, including
the Linux musl tests. Cross-compilation alone is insufficient for publication.
After validation, separate `test` and `build` matrices run in parallel alongside
Foundation checks. Publication requires all three to pass. Foundation covers fmt
and OS/ABI Clippy checks; release builds restore no Cargo cache.

Every published release ships all eight targets.
Directory synchronization remains unsupported on Windows;
`sync_dir` is rejected there with a diagnostic.

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
On macOS a blob and its directory entry are synced with `F_BARRIERFSYNC`, which
orders them before the catalog commit that references them. A full `F_FULLFSYNC`
device flush cost about 4 ms per sync on an Apple-silicon SSD, and SQLite's
commit fsync there stops at the device cache, so a power loss can drop the
latest acknowledged commits but cannot keep a commit whose blob was lost.
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
For a release candidate, retain the entire version, for example
`gcs-mimic-1.0.0-rc.1-<target>`. Candidates use the same eight targets, notices,
checksums and immutable assets as stable releases.
Cargo dependency notices cover the locked workspace graph, including build-only
and other-platform dependencies; this conservative superset is not an exact
runtime bill of materials. Original notices, multiple license alternatives and
required NOTICE files are preserved. The Rust standard library's copyright
document and the associated license texts are included; the toolchain-wide
copyright document is not, because no compiler or tool component ships.
Linux adds GCC runtime notices; musl archives
also include the pinned upstream musl source archive to retain individual source
file notices. That source archive supplies notices, not a claim that the shipped
runtime is an unpatched upstream build.

The release profile uses thin LTO with one codegen unit, trading build time for
size: in one measurement each, a clean macOS release build went from about 53 s
to about 130 s and the stripped binary shrank from about 17.6 MB to 13.9 MB.

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
The musl notice input is reviewed for Rust 1.100.0; a compiler upgrade must review
and advance that version guard along with the bundled runtime's notices.

Release assets include one archive per release-matrix target and `SHA256SUMS`.
Manual dispatch runs the publish job through asset checks and read-only
publication-flag calculation, so failures appear before a tag; only a tag push
creates the Release. Verify the selected
archive after downloading both from the same release:

```sh
sha256sum --ignore-missing --check SHA256SUMS
```

On macOS, use `shasum -a 256 --ignore-missing --check SHA256SUMS`; on
PowerShell, use `Get-FileHash <archive> -Algorithm SHA256` and compare the
corresponding entry.

`SHA256SUMS` uses LF line endings only;
the publish job rejects a file that contains a carriage return.
Checksums detect corruption; they are not code signatures or independent proof
of origin.

macOS archives are unsigned and not notarized. When a browser downloads one,
Gatekeeper blocks the extracted binary. After verifying the checksum, remove
the quarantine attribute before the first run:

```sh
xattr -d com.apple.quarantine gcs-mimic
```

Downloads through `curl` or `gh release download` carry no quarantine attribute.

## Verification and release flow

1. Update the root package version and Cargo.lock together according to the
   [release policy](release-policy.md). Reconcile the feature ledger, compatibility
   restrictions and migration instructions. In the same commit, set the default
   image of `examples/strict-local/compose.yaml`, the copy in its README and
   `examples/full-stack/compose.yaml` to the new version: the tag publishes those
   recipes as they are, and they use settings that only the new release has.
   `scripts/check-docs.sh` fails until they match the manifest.
   For a candidate, both recipes and the strict-local README copy must use that
   exact `-rc.N` image. Update explicit image, archive and binary references in
   public examples intended for this release to the same full version. Generic
   quick starts using `latest` continue to select the highest stable release.
   When preparing the final release, replace those candidate pins with the final
   stable version in the same files; retain historical candidate notes and data
   fixtures.
2. Write `docs/en/releases/<version>.md` with final changes, compatibility impact,
   tested platforms and known limitations. Confirm the first-release roadmap
   requirements manually; the existence of a notes file does not prove them.
   Retain the complete candidate suffix, such as `1.0.0-rc.1.md`.
   Use ordinary repository-relative links: `scripts/release-notes.sh` rewrites
   them to the tagged files for the Release body, and the workflow fails on a
   link whose target does not exist.
3. Run `just check-dependencies` and `just pre-push` on the final source. Commit
   the reviewed version, lockfile and notes, then push through the agreed process.
4. Run **Release binaries** using GitHub Actions' manual dispatch on that commit.
   It runs the release-input freshness check
   (`cargo xtask dependencies check --scope release`), the reusable Foundation
   checks and every release matrix target's native suite, uploads verification
   archives as workflow artifacts and runs the publish job through asset checks
   and read-only publication-flag calculation. The selected Latest/prerelease
   flags are displayed without creating a Release or changing registry tags.
   Manual dispatch never creates a public release. Inspect the results and
   archives. Do not tag
   until this dry run passes on the exact commit to tag, including its publish
   job; any change to the release matrix, workflows or release scripts after it
   needs a new dry run. A second manual dispatch with `reuse-dry-run` rehearses
   the tag's path: it skips the gates and runs the publish job from the dry run's
   archives, without publishing.
5. When publication is authorized, create and push a new `vX.Y.Z` or
   `vX.Y.Z-rc.N` tag on the tested commit. The tag must match Cargo's product
   version exactly, using the
   [accepted release grammar](release-policy.md#product-version-and-compatibility).
   A tag run
   checks the version, the committed release notes and release input freshness
   again, then publishes the archives of the newest dry run on its commit as a
   GitHub Release, without testing or building them again. A candidate is marked
   as a prerelease with `--prerelease --latest=false`, so it does not become the
   latest Release. Every release explicitly sets `--latest=true|false` from the
   shared container-tag policy: stable versions alone determine Latest, so
   publishing `0.17.2` after `1.0.0` does not roll it back. The publish job is
   serialized to keep concurrent tag releases from racing that selection. Dry
   runs use separate groups and cannot displace a waiting tag publication.
   It fails
   before publishing when no such run holds all eight archives, for example
   after they expire; then run the dry run and re-run the tag's failed job. Build
   jobs have read permissions; only the final publish job can write release
   contents.
6. The publish job verifies archive count, checksums, unchanged remote tag SHA and
   absence of an existing release for the tag. Inspect the published assets and
   download checksums. Never replace an existing version's tag or assets; fixes
   need a new version. A failed or interrupted publication needs inspection before
   retrying, especially if GitHub has already created the release.
7. Once a stable or candidate image is published, pin its index digest in
   `scripts/released-data-fixtures.sh`, generate its data directory fixture with
   that script, and add the version to `SUPPORTED_RELEASES` in
   `tests/it_import_seed/released_data_upgrade.rs`. Add it to the fixture script's
   default version list as well. Every candidate gets its own fixture; the final
   release must open them all. The xtask release-version tests reject a later
   version while an earlier published release lacks its fixture, and the
   integration tests require every fixture to be listed and preserve its resources.

Packaging is also available as `cargo xtask release prepare TARGET BINARY OUTPUT_DIR`
or `just prepare-binary TARGET BINARY OUTPUT_DIR`. Build first with the pinned
toolchain and the intended target; the command checks the executable's product
version and refuses to overwrite an output directory. Linux requires the native
notice inputs prepared by the workflow. Packaging does not build, publish, create
tags, rewrite examples or invent release notes. Recipe pins are updated manually
as part of release preparation. The
[GHCR workflow](container-distribution.md) follows successful tag-driven binary
Release creation; manual verification does not publish images.
For local redistribution, set `--remap-path-prefix` for the checkout and Cargo
cache before building, as the workflow does; staging an existing binary does not
remove embedded compiler paths.

## Candidate verification limits

Windows execution, GitHub's actual `gh release create` behavior and the complete
binary-to-GHCR-to-Docker-Hub publication chain have not been verified for an RC.
The Windows MSVC xtask target passes local cross-checking, but that cannot replace
native execution. Shell/workflow checks run only on Unix; Rust version cases run
on every target.

The tested run blocks remain inline in YAML rather than calling scripts through
single-line workflow steps. The offline harness extracts their literal bodies
and checks validation-step metadata, but it does not reproduce GitHub's runner,
permissions or expression evaluation. These blocks must stay compatible with
macOS Bash 3.2 for the local checks.

Before tagging the first candidate, run the manual dry run on the exact candidate
commit and confirm both Windows lanes, all eight archives, their complete RC
versions and checksums. Rehearse reuse of those artifacts with `reuse-dry-run`.
Record the current stable GitHub Latest and floating container digests before
authorizing the tag push. After that authorized publication, inspect the RC's
prerelease/non-latest flags and all three publication jobs; verify its exact
GHCR/Docker Hub tags, equal digest reports and attestations, and unchanged
floating tags and stable GitHub Latest. Capture the candidate's own pinned data
fixture after publication and require every next candidate/final build to open
it. Complete the required soak before the final release. A configured workflow
or local mocked check does not complete these remote gates.

## References

- [GitHub-hosted runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
- [GitHub CLI release creation](https://cli.github.com/manual/gh_release_create)
- [Rust target support](https://doc.rust-lang.org/rustc/platform-support.html)
- [Rust static C runtime selection](https://doc.rust-lang.org/reference/linkage.html#static-and-dynamic-c-runtimes)
- [Windows write-through move](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw)
- [Windows file flushing](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers)
- [SQLite file URI processing](https://www.sqlite.org/uri.html)
