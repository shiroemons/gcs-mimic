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

### Upstream changes reviewed

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

### Upstream changes reviewed

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
