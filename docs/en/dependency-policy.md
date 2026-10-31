# Dependency and Rust version policy

<!-- markdownlint-configure-file {"MD013": false} -->

Status: project policy. The project has compiler pins, Cargo.lock, a bounded HTTP/TLS runtime, digest-pinned Docker inputs, a dated [dependency snapshot](../../compatibility/dependency-status.json), and Rust/Go/Ruby/Node.js/PHP/Python daily detection tooling. The official Go JSON bucket SDK harness has its own pinned module graph and CI lane; Ruby HTTPS verification has a pinned gem graph. The official Node.js, Python and PHP SDK suites under `sdk-tests/` have their own exact pins and lockfiles; `just check-sdk` runs them with Node.js 24 or newer, with uv and the pinned `.python-version`, and with PHP inside the digest-pinned Composer image, so Docker is the only PHP prerequisite. Scheduled/remote CI execution is not verified; per-requirement progress is recorded only in the feature ledger.

## User requirement

The gRPC runtime uses tonic/tonic-prost and prost/prost-types; the workspace's
`grpc-codegen` tool adds tonic-prost-build only for explicit regeneration.
Daily detection enumerates every workspace member's direct dependencies from
locked Cargo metadata, so both runtime and generation dependencies are checked.
Ordinary application/Docker builds use committed code without protoc. Exact
versions, licenses, source sizes and provenance are recorded in the
[foundation contract](slices/grpc-v2-foundation.md).

Use the latest stable Rust and the latest suitable non-prerelease library releases at implementation start, and keep them current throughout development. A previous planning document's example versions must never silently become the dependency baseline.

This policy applies to runtime, build and test dependencies, official SDK example projects, compiler tooling and Docker base images. English is the canonical documentation language. User-facing implementation prompts can be Japanese; code, identifiers and commits remain English.

## Latest at a defined verification time

At bootstrap, at the beginning of each implementation slice, and immediately before a release, verify official upstream release information and package registries. Record the UTC check time and actual resolved versions. A development slice uses that verified snapshot; newly arriving releases become tracked updates, rather than changing a build midway through its execution.

The research baseline on 2026-10-21 includes the official announcement of **Rust 1.99.1**, released 2026-10-15. Re-check the stable channel when implementation begins. This dated fact must not be treated as an eternal version requirement or a prediction of a future release.

Official release source: <https://blog.rust-lang.org/2026/10/15/Rust-1.99.1/>

For each direct dependency, use its publisher-controlled release notes/repository and the official package registry metadata. Check package identity, stable/prerelease status, supported targets and compiler requirements. Do not infer the latest version from an old tutorial, a search snippet, or a model's memory.

## Rust toolchain, edition and supported compiler

Use the newest verified stable channel release. Nightly and beta are opt-in experimental CI lanes; they must not become a production requirement merely to use an unstable update command.

After verification, write the exact stable release number into rust-toolchain.toml, including the project's required components and targets. Record rustc -Vv, cargo -V and rustup show in build evidence. The checked-in toolchain file contains a concrete version, not a floating stable token.

Choose the newest edition supported by the selected stable toolchain and record it explicitly in Cargo.toml. Rust compiler versions and language editions are separate concepts: updating a compiler does not automatically migrate an existing crate's edition.

For this application, the default support policy is **current stable Rust only**. Set Cargo.toml rust-version to the oldest compiler that the project actually promises and verifies. Under the current-stable-only policy, advance that value with the verified toolchain baseline; do not preserve an older MSRV at the cost of holding back requested library upgrades. If a future separately published library needs a broader compiler support window, document and test that as a distinct product decision.

Sources:

- <https://doc.rust-lang.org/stable/book/appendix-07-nightly-rust.html>
- <https://rust-lang.github.io/rustup/overrides.html>
- <https://doc.rust-lang.org/edition-guide/editions/>
- <https://doc.rust-lang.org/cargo/reference/rust-version.html>

## Library selection and major-version upgrades

Select the latest non-prerelease release of each direct dependency that can satisfy the project's contract on its supported targets. A maintained 0.x release can be the correct current release; do not equate “stable selection” with “version 1.0 or greater.”

When the latest release is a breaking major, perform its required migration and relevant conformance/performance verification. Do not hide it behind an old caret range and claim the project is up to date. Review compatibility as a graph: HTTP, TLS, protobuf/gRPC, database bindings and their shared types sometimes require coordinated upgrades.

A normal cargo update changes the lockfile within the manifest's allowed requirements; it does not prove every library is at the newest major release. Update Cargo.toml constraints deliberately when adopting a breaking release. Do not depend on nightly-only cargo update --breaking for this stable-toolchain workflow.

Resolve transitive dependencies to the newest versions allowed by the selected dependency graph. If upstream requirements prevent a newer transitive major, do not force an incompatible override and call it a valid update. Prefer upgrading or replacing the direct dependency responsible, and record remaining constraints precisely.

Use only necessary crate features and dependencies. “Latest” does not mean enable every optional feature, install extra infrastructure or replace a smaller suitable dependency without a demonstrated reason.

The legacy signature adapter promotes the already resolved `httpdate` 1.0.3 to
a direct dependency for HTTP-date parsing. This adds no resolved package or
version and avoids a second date grammar. Parsing may normalize an accepted
UTC spelling, but the verifier signs the original date value. The dated
[dependency evidence](evidence/dependency-refresh.md#http-date-parser-direct-edge)
records the license, package digest, size and scoped checks; daily detection
discovers this actual manifest edge through locked Cargo metadata.

Sources:

- <https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html>
- <https://doc.rust-lang.org/cargo/commands/cargo-update.html>

## Lockfiles and reproducibility

Commit Cargo.lock for the application and generate it with Cargo. Never manually edit resolved checksums or dependency records. Commit lockfiles for SDK example applications where their package manager supports them.

Normal CI and release builds use the committed lockfile, for example cargo build --locked and cargo test --locked. A drift/update job resolves newer versions in an isolated working copy, proposes the resulting manifest/lockfile change, and runs validation.

The exact Rust toolchain, complete dependency lockfile, source commit, build settings and base-image digests together define a reproducible release input. Reproducibility does not require leaving those inputs old: update them in reviewed, tested changes and preserve the resulting history.

Source: <https://doc.rust-lang.org/cargo/guide/cargo-toml-vs-cargo-lock.html>

## Go SDK toolchain and module graph

The [Go bucket harness](../../sdk-tests/go/README.md) pins the current stable Go
patch with a `toolchain` directive and its language series with an exact
`go MAJOR.MINOR.0` directive. Keep both: Go removes a redundant toolchain directive
when both specify the same patch. `actions/setup-go` reads the toolchain from
`sdk-tests/go/go.mod`; local Go can select/download it with `GOTOOLCHAIN=auto`.
The verified 2026-10-29 snapshot uses Go 1.27.3, Storage v1.70.0 and API client
v0.310.0. These are dated inputs, not permanent version requirements.

Commit `go.mod` and `go.sum`. Go's minimum version selection uses the manifest's
direct and indirect requirements; `go.sum` authenticates module content rather
than selecting versions. Ordinary CI uses that graph. The API client is a large
official generated module needed for options, iterators and error types; it also
requires an auth pseudo-version transitively. Do not replace upstream graph
constraints with incompatible overrides to remove that transitive pin.

Freshness detection reads every direct `require` from the manifest, including
single-line and grouped requirements. It checks the exact Go toolchain against
the latest stable official download metadata and the language baseline against
that release's major/minor series. It selects numeric stable module versions from
`proxy.golang.org` and excludes single versions and inclusive ranges retracted in
the highest release's `go.mod`, including self-retractions. Direct dependencies
are checked on the current module path; new majors are discovered through successive
`/v2`, `/v3`, etc. module paths (starting after an existing major suffix). A 404/410
ends discovery; other HTTP failures and transport/parse failures fail closed.
Pre-release-only paths do not count as stable updates. A direct requirement may
pin a pseudo-version only when its module publishes no tagged releases, as
`google.golang.org/genproto` does for the legacy Storage v1 stubs; it is then
compared with the proxy's `@latest` pseudo-version, and the module's first stable
release becomes the update target. Indirect requirements
remain graph constraints. Unsupported manifest directives fail explicitly.
The checker uses the same bounded curl path and exit statuses as Rust and needs
no Go installation. Unrecognized SDK ecosystems fail instead of being ignored.

Official sources checked on 2026-10-21:

- <https://go.dev/dl/?mode=json>
- <https://go.dev/doc/toolchain>
- <https://go.dev/ref/mod>
- <https://proxy.golang.org/cloud.google.com/go/storage/@latest>
- <https://proxy.golang.org/google.golang.org/api/@latest>
- <https://github.com/actions/setup-go/releases/tag/v7.0.0>

## Official Ruby SDK example dependencies

The local HTTPS scope adds `sdk-tests/ruby/Gemfile`, `Gemfile.lock` and
`.ruby-version`. The verified 2026-10-27 inputs are Ruby 4.0.9, Bundler 4.0.23
and `google-cloud-storage` 1.62.1. Commit the complete lockfile, including
checksums and `BUNDLED WITH`; do not infer the active Bundler version from the
host default. Run the isolated client workflow from
[`sdk-tests/ruby/README.md`](../../sdk-tests/ruby/README.md).
Rails Active Storage remains a separate compatibility scope.

The daily detector reads the exact Ruby pin, direct Gemfile requirements,
resolved lockfile versions and Bundler pin. It compares Ruby with the official
downloads page's stable marker and gems with the official RubyGems versions
API, selecting numeric stable releases across major versions and excluding
prereleases and yanked entries. The versions API lists active releases; the
initial selection also checks the version-specific API's `yanked` field.
The supported manifest uses the official RubyGems source, exact gem pins and
`ruby file: ".ruby-version"`; unsupported directives or lockfile mismatches
fail explicitly. Metadata failures do not report `CURRENT`.
The checker uses the existing bounded curl path and needs no Ruby installation.

Official sources checked on 2026-10-23:

- <https://www.ruby-lang.org/en/downloads.md>
- <https://rubygems.org/api/v1/versions/bundler.json>
- <https://rubygems.org/api/v2/rubygems/bundler/versions/4.0.23.json>
- <https://rubygems.org/api/v1/versions/google-cloud-storage.json>
- <https://rubygems.org/api/v2/rubygems/google-cloud-storage/versions/1.62.1.json>
- <https://guides.rubygems.org/rubygems-org-api/>

## Daily update detection and bounded upgrade work

The source checkout adds the separate `go/go.mod` facade and
`go/sourceexport/go.mod` capture modules, standalone Rust SDK Cargo graph,
Java Maven dependencies/plugins and Temurin/Maven pins, .NET NuGet/global.json,
and Node Testcontainers manifest. All fifteen SDK manifest snapshots are read
by the same daily detector; an omitted declared manifest fails. Locks are checked
against actual direct pins, and standalone Cargo metadata remains locked and
offline after `cargo fetch --locked --manifest-path sdk-tests/rust/Cargo.toml`.
The capture's fake-gcs-server dependency is the fixed migration baseline: the
detector prints PINNED_REFERENCE plus the latest available version, rather than
silently upgrading the contract or labeling it an unexamined CURRENT input.

Maven package/plugin pins use literal supported XML and a separate dependency
graph lock. NuGet discovery follows the official semver2 registration resource,
checks listed stable releases and fails on invalid/paged metadata. Full Temurin
release names retain hotfix build components; the shorter API openjdk_version
field is insufficient. .NET uses the newest supported stable SDK across release
channels. Registry bodies, including gzip responses, retain the 8 MiB cap.
CI installs the exact Java/Maven archive digests and .NET global pin; local
syntax/compile results do not establish remote CI or runtime SDK success.

The optional [SDK scorecard](../../sdk-scorecard/README.md) and the official
[Node.js](../../sdk-tests/node/README.md), [Python](../../sdk-tests/python/README.md)
and [PHP](../../sdk-tests/php/README.md) SDK suites add exact npm, PyPI and
Composer direct pins with committed package-lock.json, uv.lock and
composer.lock files. Each manifest keeps its own lockfile even where the pins
agree, so each is declared separately in the dependency snapshot. The daily
checker accepts only the manifests in its fixed table, reads every direct pin
from them, verifies the declared snapshot and checks stable upstream releases,
including new majors; PyPI files marked yanked and prereleases do not qualify.
It fails when a table manifest has no snapshot entry, so a new suite cannot be
skipped silently. Its intentionally small Python manifest reader requires a
single JSON-compatible dependency array of exact `name==X.Y.Z` pins and fails
on unsupported syntax. Each Python manifest's `.python-version` pin is compared
with the newest published stable release from the python.org release API,
including new minor versions. The Node.js `engines` field is a lower bound, not
a pin, and is not checked. PHP/Composer use a digest-pinned official Composer
container referenced by `sdk-scorecard/run.sh` and `sdk-tests/php/run.sh`; both
scripts are read when the checker verifies container pins.

Current command: `cargo xtask dependencies check`. Fetch all locked packages first (`cargo fetch --locked`), including foreign-target packages used by offline Cargo metadata. The Rust checker invokes curl for HTTPS with user curl configuration disabled, a 30-second timeout and an 8 MiB response cap; no Python or extra HTTP client dependency is required. It contacts official Rust, Cargo, Go, module proxy, Ruby, RubyGems, Python, PyPI, npm, Packagist, GitHub, Debian and Docker metadata. It discovers actual direct registry dependencies through offline Cargo metadata and compares all non-yanked stable releases, including new majors. It checks the declared Go, Ruby, npm, PyPI and Composer SDK scopes, Action pins (including setup-go), Docker index/platform digests, replacement tags, the Rust image's floating stable tag and Debian's current stable codename. Debian stable/trixie aliases have different APT suite contents/digests, so codename freshness is checked separately from selected-tag digest freshness. Network/parse failures exit 2; updates exit 1. Logs are actionable drift records; nothing is published automatically. `.github/workflows/dependency-check.yaml` schedules checks every calendar day at `03:17 UTC` without cooldown, fetching the locked Cargo graph before running the same detector. This includes the TLS and Ruby inputs automatically; scheduled remote execution has not been observed.

Cargo version selection follows workspace members' resolved direct dependency
edges. An older same-named transitive package remains part of the upstream graph;
it does not make the direct package's resolution ambiguous or exempt that direct
package from checking.

Run it at every slice start and before release. Review failed runs and `compatibility/dependency-status.json` exceptions at least weekly. Update toolchain, manifests, lockfiles and snapshot together after validation. One freshness exception is open, `composer-riscv64-publication-lag` (review by 2026-11-06): the official `composer:latest` index still carries the 2.10.3 riscv64 image, so it differs from the pinned Composer 2.11.1 index. The supported amd64 and arm64 manifests match latest after adopting 2.11.1, but both changed from the old 2.11.0 pin and require their own verification; see the [2026-10-31 refresh](evidence/dependency-refresh.md#refresh-2026-10-31).

Configure daily dependency drift detection. The selected updater must cover Cargo, Rust toolchain, Docker tags/digests, GitHub Actions and the SDK example ecosystems actually present. The currently checked official Dependabot reference includes cargo, rust-toolchain, docker and other supported ecosystems; verify support again when configuring the repository.

Daily means every calendar day, including weekends, and freshness checks must not intentionally delay discovery of a new stable release. The checked Dependabot reference defines `daily` as weekdays and a default version-update cooldown of three days. If choosing Dependabot, explicitly configure an every-day cron schedule and override release delays (for example, a supported zero cooldown or equivalent immediate-detection workflow). Validate the resulting behavior; do not assume the default configuration satisfies this policy. A failing candidate remains visible until fixed or documented as an exception.

Daily checks create concrete upgrade changes or actionable drift records. They do not silently modify protected branches, mutate the build input during a test, or publish a release.

Add explicit checks at bootstrap, at slice start and before release even when daily automation exists. Use a weekly maintenance review to ensure no failed upgrade or ignored major release remains unattended; the weekly review supplements daily detection.

Patch/minor upgrades may be grouped when the groups remain reviewable. Breaking majors and toolchain/edition migrations should be independently assessable. Automatic merge, if enabled by the repository owner, is restricted to changes that satisfy the repository's actual required checks; a bot discovering a release is not evidence of compatibility.

Sources:

- <https://docs.github.com/en/code-security/reference/supply-chain-security/dependabot-options-reference>
- <https://docs.docker.com/build/building/best-practices/>

## Temporary incompatibilities must be visible

If the newest release fails required functionality, first investigate and migrate or fix the integration. If an upstream regression blocks adoption, record an explicit temporary exception containing package/current/latest version, official upstream issue or release reference, failing test evidence, impact, owner, next action and review/expiry date.

Never describe a project with an unresolved freshness exception as “all dependencies current.” Release notes and the dependency status page must surface it. Re-test the exception on new upstream releases and during daily checks; do not leave broad ignore rules indefinitely.

A real incompatibility is not a reason to fake a passing test, remove a permission check or weaken persistence semantics. A temporary pin is a documented deviation from the user's freshness target, not satisfaction of it.

## Docker base images and released artifacts

Verify the newest suitable supported builder/runtime image from its official publisher. Pin actual digests in Dockerfiles; tags alone are mutable. For a multi-architecture image, record an appropriate manifest-list/index digest or separately verified platform digests and ensure both amd64 and arm64 resolve correctly.

Do not invent a `rust:<version>` tag before confirming that image exists. If the official Rust image has not yet published the new compiler, use a verified supported builder and install the exact official toolchain through a documented process, or record a visible publication-lag exception. Do not silently use the previous compiler.

The runtime uses digest-pinned `gcr.io/distroless/cc-debian13:nonroot`. It supplies
glibc/libgcc for the existing GNU Rust binary; the container build strips symbols
without changing panic handling, checksums or persistence settings. Numeric
UID/GID 10001 are retained so existing data volumes remain writable. Runtime
containers contain no shell or package manager; the application supplies probes.

The freshness checker supports Docker Hub and public `gcr.io` manifests. GCR
requests send no Docker Hub token. Unsupported registries fail explicitly.
Distroless tag/index/platform digests and Debian stable codename are checked
independently, preserving detection when a new Debian generation becomes stable.

The current workflow reports tag/digest drift; upgrade submission and latest-dependency candidate testing remain future work. Rebuild runtime images when accepted base-image updates change them; editing a Dockerfile does not update a published image. No image has been published yet.

Use immutable release version tags and publish the resolved image digest in release metadata. A convenience latest tag may exist after releases are actually published, but installation and reproduction examples should also show an explicit release/digest. Before the first release, use a locally built lowercase gcs-mimic:dev image.

Source: <https://docs.docker.com/build/building/best-practices/>

## CI lanes

| Lane | Toolchain/dependencies | Purpose |
| --- | --- | --- |
| Required PR/release | Exact pinned stable, committed lockfiles and digests | Reproduce the release inputs and validate supported targets. |
| Current stable compatibility | Freshly updated stable toolchain, committed lockfile | Detect compiler drift immediately; passing alone does not update the committed baseline. |
| Official Go JSON SDK | Go toolchain from go.mod, committed Go graph, pinned Rust | Build an isolated emulator and run bucket lifecycle/pagination via scripts/sdk-go.sh. |
| Latest-dependency candidate | Verified current stable, proposed manifests/lockfiles | Exercise dependency upgrades, including deliberate major migrations. |
| Optional experimental | Explicit beta/nightly | Early warning only; never substitutes for passing stable checks. |

Release gates require a successful check against the currently verified stable compiler, adoption of the latest direct dependencies or explicit visible exceptions, successful target-platform container smoke tests, and recorded version provenance.

The freshness part of the release gate covers release inputs only: inputs that build or ship release artifacts. `cargo xtask dependencies check --scope release`, which the **Release binaries** `validate` job runs, compares the Rust toolchain, direct Cargo dependencies, GitHub Action pins and the Docker inputs marked `"release_input": true` in the [dependency snapshot](../../compatibility/dependency-status.json) (the Dockerfile builder, frontend and runtime, and the GHCR builder and SBOM generator), plus the Debian stable codename. It skips the SDK suite ecosystems and test-only images such as the Composer and peer curl images, because they ship nothing. Every Docker input must declare `release_input`; a unit test requires it to be true exactly for the images pinned in the Dockerfile or the GHCR workflow. The daily check and `just check-dependencies` keep the default `--scope all`, so drift in development-only inputs stays visible and must still be adopted or recorded as an exception; it just no longer blocks publishing artifacts that do not contain it.

## Dependency status evidence

The 2026-10-29 SDK integration adds flate2 1.1.10 with default features disabled
and only rust_backend enabled for bounded JSON media gzip expansion. The official
Cargo index verifies non-yanked latest stable status and Rust 1.67.0 minimum;
the 80,244-byte crate is MIT OR Apache-2.0 and its rust-lang repository remains
active. Cargo.lock additionally resolves adler2 2.0.1, crc32fast 1.5.3,
miniz_oxide 0.9.1 and simd-adler32 0.3.10. No native zlib build is added.
The existing daily checker automatically includes the new direct edge and
reported all inputs CURRENT. Exact provenance and the decoding resource limits
are in the [integration contract](slices/sdk-scorecard-integration.md).

The original rusqlite update adopted 0.41.0 with defaults disabled and only
`bundled`, resolving libsqlite3-sys 0.39.0 and SQLite 3.54.0. No application API
or catalog schema change is required. The registry and distributed crate declare
Rust 1.89.0 despite the release notes mentioning 1.88.0; the tested project pin
remains 1.99.1. Package provenance, build changes and old-catalog compatibility
are recorded in the [migration evidence](evidence/rusqlite-update.md).
Its [historical freshness check](evidence/rusqlite-dependencies.txt) exited 1
for a distroless digest update adopted later. The 2026-10-27 refresh retains
rusqlite 0.41.0 and advances the binding to 0.39.1, bundling SQLite 3.54.1;
see the current [refresh evidence](evidence/dependency-refresh.md).
On 2026-10-29 rusqlite also enables its `cache` feature so request paths reuse
prepared statements through `prepare_cached`; the connection cache holds at most
64 statements. Cargo.lock adds hashlink 0.12.2 (MIT OR Apache-2.0, 33,361
compressed bytes, Rust 1.85) and foldhash 0.2.0 for the already locked hashbrown
0.17.1 (Zlib, 23,329 compressed bytes, Rust 1.60). The official Cargo index
confirms both as the latest non-yanked stable releases, their publisher
repositories are not archived, and both packages ship their license texts.
Digests are in the [dependency snapshot](../../compatibility/dependency-status.json).

The V4 signed-download slice promotes the already locked TLS transitive dependency
`ring` 0.17.14 to a direct dependency for RSASSA-PKCS1-v1_5 SHA-256 signing,
verification and canonical-request SHA-256. No second crypto implementation is
added and its resolved version is unchanged. The official Cargo index confirms
non-yanked stable status and Rust 1.66.0 minimum; the package is 1,502,610
compressed bytes, licensed `Apache-2.0 AND ISC`. Default `alloc` and
`dev_urandom_fallback` features match the existing graph. The publisher repository
is not archived and reports a 2026-07-23 push. Version, license, source checksum
and package size are recorded in the [dependency snapshot](../../compatibility/dependency-status.json).
The [slice-start check](evidence/signed-download-dependencies.txt) reports all
35 pre-existing inputs CURRENT; the appended post-promotion check includes ring.
Daily detection discovers it from the real direct Cargo edge automatically.

The local HTTPS slice adds `tokio-rustls` 0.26.6, published 2026-09-28 and
verified on 2026-10-23 as the newest non-yanked stable release. Its license is
`MIT OR Apache-2.0`, minimum Rust is 1.71, and the official compressed crate is
29,259 bytes. Default features are disabled; `ring` and `tls12` provide the
required TLS 1.2/1.3 transport without AWS-LC, certificate compression or another
HTTP framework. The lockfile pins Rustls 0.23.46, rustls-pki-types 1.15.1 and
ring 0.17.14. PEM parsing reuses the re-exported pki-types implementation.
Certificate generation is an explicit test-fixture OpenSSL command rather
than a new runtime dependency.

The Ruby SDK gem is Apache-2.0 and 111,616 compressed bytes; Bundler is MIT and
471,040 compressed bytes. Their official version-specific metadata, package
SHA-256 values and exact toolchain/lockfile inputs are in the
[snapshot](../../compatibility/dependency-status.json). These package sizes do
not measure application binary or container growth. Existing Rust/Docker pins
and the daily schedule remain in effect.
The [local TLS freshness check](evidence/local-tls-dependencies.txt) completed on
2026-10-23 with exit 0 and all 35 inputs `CURRENT`, including the new TLS and
Ruby inputs. The Go HTTPS harness promotes the already-locked OAuth2 v0.37.0
to a direct requirement; its version and remaining module graph are unchanged.

The object media slice adds four direct crates, verified as the latest non-yanked stable
releases on 2026-10-21. Compressed sizes describe registry packages, not binary-size growth.

| Crate | Version | License | Compressed bytes | Reason |
| --- | --- | --- | --- | --- |
| bytes | 1.12.1 | MIT | 75,667 | Share bounded byte chunks with the existing Hyper/Tokio graph |
| md-5 | 0.11.0 | MIT OR Apache-2.0 | 17,404 | Incremental object checksum compatibility; no authentication use |
| crc32c | 0.6.8 | Apache-2.0/MIT | 9,647 | Incremental Castagnoli CRC32C |
| getrandom | 0.4.3 | MIT OR Apache-2.0 | 52,437 | Opaque random blob identifiers |

All four publisher repositories were active and not archived at selection. Exact registry,
publisher, feature and compiler references are in the
[snapshot](../../compatibility/dependency-status.json). Tokio enables `fs`/`io-util` for
short asynchronous file jobs; http-body-util enables its existing `channel` feature for
bounded download bodies. No new runtime framework or general codec dependency is added.
The Rust builder index is updated to
`sha256:e6cd1d294a300601eaaa4a294b337e5f0826f427fb2d481e9f76e69e7981e244`;
its amd64/arm64 platform manifests are unchanged. The final online
[freshness check](evidence/object-media-dependencies.txt) exits 0 with every input CURRENT.

Maintain a machine-readable dependency status record containing checked_at_utc, toolchain channel/exact version, edition, declared rust-version, package current/latest version, source URL, resolution constraints, Docker digest per platform and any exception reference. Store actual command/test evidence with the change.

The [2026-10-27 refresh](evidence/dependency-refresh.md) updates the Go API
client, compatible Cargo/Ruby transitive dependencies and the workflow's SBOM
scanner digest. The [snapshot](../../compatibility/dependency-status.json)
and committed lockfiles record these inputs; no direct dependency is added.

Report the difference between “no update found,” “update available,” “candidate tests failing,” and “update merged.” Never mark a dependency current merely because the scheduled job ran.

## Definition of done

All policy acceptance rows begin NOT_STARTED. They become implemented only after the corresponding files, update workflow, CI lanes and evidence exist and have been executed. This research document itself is not evidence that the project has a current compiler, latest crates, committed lockfiles or a running daily update job.
