# Dependency refresh: 2026-10-27

<!-- markdownlint-configure-file {"MD013": false} -->

The final check at `2026-10-27T04:04:15Z` exited 0: all 39 inputs were `CURRENT`.
No update was deferred, no freshness exception remains, and no direct dependency was added.
Exact pins remain in the [snapshot](../../../compatibility/dependency-status.json) and lockfiles.

| Input | Previous | Adopted |
| --- | --- | --- |
| cc | 1.9.0 | 1.10.0 |
| libsqlite3-sys | 0.39.0 | 0.39.1, bundling SQLite 3.54.1 |
| smallvec | 1.16.3 | 1.16.4 |
| google.golang.org/api | v0.308.0 | v0.309.0 |
| faraday | 2.15.0 | 2.15.1 |
| google-apis-storage_v1 | 0.69.0 | 0.70.0 |
| pstore | 0.2.1 | 0.3.0 |
| docker/buildkit-syft-scanner:stable-1 | sha256:ae4f3b554449e7e25548e7d8ccc029d17357348e30c6e3df01b92bc93654d6a9 | sha256:873a7458c3c8ac8e32c7b796c10e1600b07b963d2aa000a72a2d97aaaa2d6315 |

## Validation performed on 2026-10-27

| Command or check | Result |
| --- | --- |
| `cargo check --workspace --all-targets --locked` | PASS |
| `just check` | PASS: 988 tests, no failures/skips; formatting, native/cross-target clippy, ledger, shell and offline publication checks |
| `BUNDLE_FROZEN=true bundle check` | PASS with Ruby 4.0.9/Bundler 4.0.23 before the SDK suite |
| `just check-sdk` | PASS: Go vet, 52 top-level tests and 85 subtests without failures/skips; Ruby self-check, migration, permissive/strict operations and V4 signatures |
| ARM64 scanner local OCI build and SPDX inspection | PASS: SPDX-2.3 generated with Syft v1.51.0 |
| `just check-dependencies` | PASS: exit 0, all 39 inputs `CURRENT` |

Application-container checks were omitted because Dockerfile inputs were unchanged; AMD64 execution was omitted as requested.
No benchmark, performance measurement, real-cloud test, HTTPS-mode SDK rerun, remote CI, push, publication or release was performed.

## Refresh: 2026-10-29

The final check at `2026-10-29T00:14:38Z` exited 0: all 55 inputs were `CURRENT`.
No update was deferred, no freshness exception remains, and no direct dependency was added.

| Input | Previous | Adopted |
| --- | --- | --- |
| rusqlite | 0.41.0 | 0.41.1; libsqlite3-sys stays 0.39.1, bundling SQLite 3.54.1 |
| google.golang.org/api | v0.309.0 | v0.310.0 |
| github.com/googleapis/gax-go/v2 (indirect) | v2.26.2 | v2.27.0 |
| github.com/googleapis/enterprise-certificate-proxy (indirect) | v0.3.23 | v0.3.24 |
| go.opentelemetry.io/otel, metric, sdk, sdk/metric, trace (indirect) | v1.45.0 | v1.47.0; otel/log v1.47.0 added |
| curlimages/curl:8.23.0 | sha256:11e64e28c5f58c8d93ee95fcf22ac055f48b0bc440e0ce7d8013bbb5ad6aa8ac | sha256:3fb7e13e5d179a845c97d78204f3f1064d1907037f26a3ebf91710571151fd93 |

### Upstream changes reviewed on 2026-10-29

- rusqlite v0.41.1 skips the PRAGMA name check under SQLCipher and fixes a panic with a zero-argument deterministic scalar function. The catalog enables neither SQLCipher nor custom scalar functions, so no code change was needed.
- google-api-go-client v0.310.0 contains only regenerated Discovery clients. gax-go v2.27.0 adds a path expression helper. No harness migration was needed.
- The curl-container 8.23.0_2 rebuild moves the Alpine base from 3.24.2 to 3.24.3; curl stays 8.23.0 and `latest` resolves to the same index.

### Validation performed on 2026-10-29

| Command or check | Result |
| --- | --- |
| `cargo check --all-targets --locked` | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS |
| `just check` | PASS: 45 test binaries and doctests, 1113 tests without failures or ignores; formatting, clippy, ledger, shell and offline publication checks |
| `just check-sdk` | PASS: Go vet, 82 top-level tests and 109 subtests without failures; the V2 signed URL test skips by design outside HTTPS mode; Ruby self-check, migration, permissive/strict operations and V2/V4 signatures |
| `docker pull` of the new curl digest | PASS: curl 8.23.0 on linux/arm64 |
| `bash scripts/smoke-containers.sh --image <local build> linux/arm64` | PASS: every linux/arm64 smoke step, including the peer-container curl steps |
| `just check-dependencies` | PASS: exit 0, all 55 inputs `CURRENT` |

AMD64 container execution was not run.
No benchmark, performance measurement, real-cloud test, HTTPS-mode SDK rerun, remote CI, push, publication or release was performed.

## Refresh: 2026-10-30

The final check at `2026-10-30T05:48:19Z` exited 1: 68 of 69 inputs were `CURRENT`.
The remaining `docker library/composer:latest` drift is the open `composer-riscv64-publication-lag` freshness exception described below; no direct dependency was added.

| Input | Previous | Adopted |
| --- | --- | --- |
| tonic, tonic-prost, tonic-prost-build (tonic-build transitively) | 0.14.8 | 0.14.9; no other Cargo.lock package changes |
| composer:2.10.3 image (PHP 8.5.12) | sha256:2cf275428757e5435a667bfc0d038b14520ca02be2b1b70d44a05c8c19e2de48 | composer:2.11.0, sha256:278dfc7bd35169965d03f87194816f43bf4e6b6ea61707c2114e52ad12fe24ce (PHP 8.5.12) |
| Python for `sdk-scorecard/python` and `sdk-tests/python` | 3.14.10, `requires-python >=3.14` | 3.15.2, `requires-python >=3.15`; both `uv.lock` files keep every package version and drop only the cp314 wheels |

### Upstream changes reviewed on 2026-10-30

- The grpc-rust 0.14.9 releases, published 2026-10-29 from revision `4c0f455c264fe2ec9f9ab5d0cc7fd2824d5daf70`, add h2 reset-stream settings to the transport `Endpoint` and apply hyper's header read timeout on tonic's HTTP/1 server connections. gcs-mimic enables only the `codegen` feature and drives connections through its own Hyper server, so neither change is reachable. tonic-prost relaxes codec `Default` bounds; the generated code does not depend on them.
- tonic 0.14.9 requires hyper `^1.10`, which the locked graph already satisfies. Minimum Rust stays 1.88.
- With protoc 36.2 and all 21 proto digests verified by `src/grpc/proto/SHA256SUMS`, `cargo run -p grpc-codegen --locked --offline` followed by `cargo fmt --all` regenerates byte-identical `src/grpc/generated` sources.
- tonic-prost and tonic-prost-build 0.14.9 still ship without a license text. The repository-root LICENSE at the new revision is byte-identical to the previously reviewed text (SHA-256 `5c686e0f7978412b49187ae0e9f77e9b52eea9d8d2e6c0de8694b35817d873b1`, also the LICENSE packaged in tonic 0.14.9), so the release notice exception moves to that revision and version; any other version still fails packaging.
- Composer 2.11.0, released 2026-10-29, is the newest stable release on <https://getcomposer.org/versions> and the composer/composer GitHub releases. It removes the temporary `source-fallback` option, adds a `cooldown` policy and fixes several security issues. Neither PHP manifest uses `source-fallback`, policies or scripts. The image runs PHP 8.5.12, unchanged from 2.10.3. `composer install --no-interaction --no-scripts --prefer-dist` installs both committed `composer.lock` files unchanged, and `composer validate` reports only the existing exact-pin warning.
- The 2.11.0 and 2.11 tags resolve to `sha256:278dfc7b…`; its linux/amd64 (`sha256:e86de4d2…`) and linux/arm64 (`sha256:f9a0252b…`) manifests are now recorded in the snapshot. The `latest` and `2` tags resolve to `sha256:c7f68260…`, which has the same amd64 and arm64 manifests but still lists the 2.10.3 linux/riscv64 image (`sha256:e6d3961e…`), because <https://hub.docker.com/r/riscv64/composer/tags> has not published 2.11.0. The version-consistent 2.11.0 index is pinned, and the `composer-riscv64-publication-lag` exception in the snapshot keeps the resulting `latest` drift visible until the next review on 2026-11-06. Republishing the tags with riscv64 will also change the 2.11.0 index digest, which then needs a re-pin.
- Python 3.15.2, released 2026-10-25, is the newest published non-prerelease release in the python.org release API. All 21 locked packages declare a `requires-python` range that includes 3.15; the compiled ones ship wheels for it: cffi 2.1.1, charset-normalizer 3.5.2 and google-crc32c 1.9.0 have cp315 wheels, cryptography 50.0.2 and protobuf 7.36.2 have abi3 wheels. No transitive update was needed, and `uv sync --locked` installs the graph without building from source. google-crc32c loads its C implementation.
- uv 0.13.6 is the first release that offers the managed CPython 3.15.2 build; the same release added 3.14.10, so the uv requirement is unchanged. Re-locking with it also moves the `sdk-tests/python` lock to lock revision 5, matching the scorecard lock.

### Validation performed on 2026-10-30

| Command or check | Result |
| --- | --- |
| `cargo check --workspace --all-targets --locked` | PASS |
| `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo xtask lint` | PASS; lint reports 0 production violations |
| `cargo test -p xtask --locked` | PASS: 63 tests, including the grpc-rust notice exception for 0.14.9 |
| `cargo xtask release prepare aarch64-apple-darwin` with a local debug build | PASS: tonic-prost and tonic-prost-build 0.14.9 notices contain the reviewed LICENSE |
| `cargo test --locked --test grpc_foundation`, `cargo test --locked --lib grpc` | PASS: 2 and 44 tests |
| `bash scripts/test-rust.sh` | PASS: 49 test binaries and doctests, 1343 tests without failures or ignores |
| `bash scripts/sdk-go.sh --with-python --with-php` (HTTP) | PASS: Go suite; Python 3.15.2 permissive and strict suites (50 tests); PHP 8.5.12 self-check, migration, permissive and strict suites in the Composer 2.11.0 image. The V2 signed URL Go test skips by design outside HTTPS mode |
| `GCS_MIMIC_SDK_SCHEME=https bash scripts/sdk-go.sh --with-python --with-php` | PASS: the same Go, Python and PHP suites over HTTPS |
| `composer install` and `composer validate` in the Composer 2.11.0 image | PASS: both committed `composer.lock` files install unchanged |
| `bash scripts/check-docs.sh` | PASS |
| `cargo xtask dependencies check` | Exit 1: 68 of 69 inputs `CURRENT`; `docker library/composer:latest` `UPDATE_AVAILABLE` under the open exception |

The SDK suites ran with uv 0.13.6. Container images, AMD64 execution and the SDK scorecard were not run.
No benchmark, performance measurement, real-cloud test, remote CI, push, publication or release was performed.

## Refresh: 2026-10-31

The coordinator's pre-update `cargo +1.99.1 xtask dependencies check` exited 1.
All fifteen declared SDK manifests were inspected; Rust 1.99.1 and the
non-Composer direct packages and runtimes were `CURRENT`. The fixed
fake-gcs-server source remained `PINNED_REFERENCE`. Composer had a new stable
patch, so the earlier publication-lag exception alone could not justify keeping
2.11.0. This Composer update changes no package or lockfile graph. The separate
XML bucket slice adds quick-xml 0.42.0, as recorded in the dependency snapshot;
its parser checks and package notices require separate verification. Legacy
header signing also promotes the already resolved httpdate package as described
below; that direct edge adds no resolved package or version.

### HTTP-date parser direct edge

`httpdate` 1.0.3 is reused for legacy GOOG1/AWS header signature time checks.
The [official registry index](https://index.crates.io/ht/tp/httpdate) identifies
it as the latest non-yanked stable, and the
[publisher's package documentation](https://docs.rs/crate/httpdate/1.0.3)
records its 2023-08-13 release. The packaged manifest requires Rust 1.56,
declares `MIT OR Apache-2.0` and has no runtime dependencies. Both license
texts are packaged. Its compressed archive is **10,639 bytes**, with SHA-256
`df3b46402a9d5adb4c86a0cf463f42e19994e3ee891101b1841f30a545cb49a9`;
the documentation's 32.9 kB figure describes unpacked source, not that archive.

The manifest and root lockfile add the application's direct edge to the
existing Hyper dependency. The resolved package version and checksum do not
change. `cargo metadata --locked --offline` and
`CARGO_INCREMENTAL=0 cargo check -p xtask --all-targets --locked --offline`
pass after the edge is recorded. An accepted `+0000` date spelling is normalized
only for parsing; the canonical signature message retains the original bytes.
This package check does not establish complete legacy signature acceptance,
release-notice output or a post-update full freshness run.

### Composer inputs verified

The [official download page](https://getcomposer.org/download/) identifies
2.11.1, released 2026-10-30, as current stable. Its
[changelog](https://getcomposer.org/changelog/2.11.1) fixes GitLab registry
authentication, invalid platform-extension names, preservation of empty
manifest keys and invalid custom-header configuration. This repository uses
neither GitLab job-token authentication nor manifest-changing Composer commands.
The Composer license remains MIT; the official container source is
[revision 2af913cf6c428737e8eec2a3e9297d12d96eeac6](https://github.com/composer/docker/tree/2af913cf6c428737e8eec2a3e9297d12d96eeac6).

| Input | Verified digest |
| --- | --- |
| Adopted `composer:2.11.1` index | `sha256:537fb03057ed901ae06bde7ef25af348903485ae1acc7fc789b80d8bac666f35` |
| `composer:latest` index | `sha256:4ed71c38c6d788eb4e4e3747b5332f5b40d5b5a6fcadd5062a1a70986a118e0a` |
| Both indexes, linux/amd64 | `sha256:22844d23aacc7140fccf24992829ae98bc2c92e0240ccca3b35716bf5d82ed70` |
| Both indexes, linux/arm64 | `sha256:9c383fb6f07d2b2cbbf0f21133789b335a55996c4c857c0f2775d16f31922872` |
| Only `latest`, linux/riscv64 | `sha256:e6d3961eaef409a8425cb992665e1d48202c4a8f1cb967ac5e226131cc211db0` |

`docker buildx imagetools inspect composer:2.11.1` and the raw `latest` index
confirm these platform mappings. The exact-version index omits riscv64;
`latest` still points its riscv64 entry at Composer 2.10.3. The supported amd64
and arm64 manifests match each other across the two new indexes, but both
changed from the old 2.11.0 pin. The three PHP runner pins and the dependency
snapshot now use the exact 2.11.1 index. The existing
`composer-riscv64-publication-lag` exception retains its **2026-11-06** review
deadline; the exception does not hide the supported-platform update or extend
its deadline.

### Validation performed on 2026-10-31

All runtime commands below use
`composer:2.11.1@sha256:537fb03057ed901ae06bde7ef25af348903485ae1acc7fc789b80d8bac666f35`.
The host executes linux/arm64; amd64 was inspected, not executed.

| Command or check | Result |
| --- | --- |
| `cargo check -p xtask --all-targets --locked --offline` | PASS |
| `bash -n sdk-scorecard/run.sh sdk-scorecard/test-tls.sh sdk-tests/php/run.sh` | PASS |
| `docker pull` of the adopted index | PASS |
| `docker run --rm --network none IMAGE --version` | PASS: Composer 2.11.1, build time 2026-10-30 14:52:10 |
| `docker run --rm --network none IMAGE php --version` | PASS: PHP 8.5.12, Zend Engine 4.5.12 |
| `composer install --no-interaction --no-scripts --prefer-dist --no-progress` in both PHP projects | PASS: the locked platform graph is accepted; existing vendor trees need no package changes; autoload files regenerate |
| `docker run --rm --network none` with the read-only checkout and `php suite.php --self-check` | PASS: lifecycle-clear adapter wire check and local-origin rejection check, no emulator or external connection |

Lockfile SHA-256 values remain
`92736800893dff84c8baa4c4087f496441b9a1f55984dadaa2c7b6200195ab34`
for `sdk-scorecard/php/composer.lock` and
`68d4658fdcc5346a4a3200c60946a18de6d944fe715f1f88950f87cd2d26d2d1`
for `sdk-tests/php/composer.lock`.
Composer reports the existing root-package-version fallback and funding notices;
neither is suppressed. Historical 2.11.0 results above remain unchanged.

Full PHP API/TLS suites, a clean-vendor install, amd64 execution and a post-update
complete freshness run have not been executed at this checkpoint. The
coordinator owns final SDK/container checks for the accepted source. No
application image build, benchmark, real-cloud test, publication or release was
performed for this runtime update.
