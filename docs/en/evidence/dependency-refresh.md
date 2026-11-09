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

## Refresh: 2026-11-01

`cargo xtask dependencies check` reported `docker library/composer:latest:
UPDATE_AVAILABLE` (`sha256:4ed71c38c6d788eb4e4e3747b5332f5b40d5b5a6fcadd5062a1a70986a118e0a`
against the pinned `composer:2.11.1` index
`sha256:537fb03057ed901ae06bde7ef25af348903485ae1acc7fc789b80d8bac666f35`).
Inspection with `docker buildx imagetools inspect` showed no new Composer
release. The `2.11.1` tag index is unchanged; the `latest` index differs only by
a `linux/riscv64` entry that still carries Composer 2.10.3. The linux/amd64
(`sha256:22844d23aacc7140fccf24992829ae98bc2c92e0240ccca3b35716bf5d82ed70`) and
linux/arm64
(`sha256:9c383fb6f07d2b2cbbf0f21133789b335a55996c4c857c0f2775d16f31922872`)
manifests are identical in both indexes.

No single digest can equal both tag indexes, so the exact-index comparison could
never pass. The checker now compares a distinct moving tag only on the platforms
declared in the input's `platforms` map and prints platform differences outside
that set as `INFO` without failing; see the Docker platform scope paragraph in
the [dependency policy](../dependency-policy.md). Composer is used only by
development scripts that run on amd64 and arm64, both declared, so the
`composer-riscv64-publication-lag` exception no longer describes a freshness gap
and was removed. The pins in the three PHP runners and the snapshot are
unchanged. `docker/dockerfile` declares no platforms and keeps the exact
comparison because its tag and moving tag are the same.

The full `cargo xtask dependencies check` afterwards exited 0: Composer
`2.11.1` was `CURRENT`, Composer `latest` was `CURRENT on declared platforms
linux/amd64, linux/arm64` with `INFO: latest differs only on linux/riscv64`, and
the Rust, distroless, curl, buildkit and syft-scanner inputs were unchanged
`CURRENT` results.

## Refresh: 2026-11-01 distroless

The scheduled check at `2026-11-01T10:33Z` reported `docker
distroless/cc-debian13:nonroot: UPDATE_AVAILABLE`. `docker buildx imagetools
inspect gcr.io/distroless/cc-debian13:nonroot` showed a rebuilt index in which
both shipped runtime manifests changed; the tag still tracks Debian 13 and the
Debian stable codename is still `trixie`.

| Digest | Previous | Adopted |
| --- | --- | --- |
| Index | `sha256:3ee00291cf386ec52a552e6a858d555debd74776eb4bfa53fa3df2cd6677515e` | `sha256:08476c244888bfb425bae1ae4dd7c17f093e972aab8426883d97f3c6f280f7c1` |
| linux/amd64 | `sha256:732d2fe38bb1996cb47f8218b48dc840967684d73c5483894ff2ed638068c1b6` | `sha256:65af2e9e44e71de2df28245f6814301775d3ef04436b7d563f240d86fedf24aa` |
| linux/arm64 | `sha256:91e80685018de611a20450bed6b011c3d3242b3d9cc4452208121f342703ada0` | `sha256:1026089ebb266c264cfa1828860332f25160079615ee146d159a7d1f3a701bd3` |

The `Dockerfile` runtime stage and the
[snapshot](../../../compatibility/dependency-status.json) pin the adopted index.

### Validation performed on 2026-11-01

- `cargo xtask dependencies check` at `2026-11-01T13:33:30Z` exited 0: 118
  inputs were `CURRENT`, fake-gcs-server v1.56.1 stayed `PINNED_REFERENCE`, and
  Composer `latest` printed `INFO: latest differs only on linux/riscv64`.
- `just check-container linux/arm64` and `just check-container linux/amd64`
  exited 0: both images built from the adopted runtime and passed every smoke
  case.

## Refresh: 2026-11-03

The scheduled check at `2026-11-03T10:57Z` reported four `UPDATE_AVAILABLE`
inputs; a local rerun at `2026-11-03T13:02Z` also found the new BuildKit
release.

| Input | Previous | Adopted | Change |
| --- | --- | --- | --- |
| crc32c | 0.6.8 | 0.6.9 | SPDX license metadata, bundled license files, inlined x86_64 SSE 4.2 loops |
| actions/setup-dotnet | v6.0.0 `a98b56852c35b8e3190ac28c8c2271da59106c68` | v6.1.0 `0f4a354dd5547a28c70b7853dbd95d5b67886e73` | New optional inputs and dependency updates |
| moby/buildkit | v0.34.1 `sha256:99368f3e46177c6405b9740e4461d91cee004128daeecca78d4ceac50ec1da5c` | v0.34.2 `sha256:c5f8862f52499773af85bc07695b2379ef08e8002352b0df2c4864c91d78dabe` | Fixes GHSA-2fqp-xg6f-v7gj and GHSA-w26x-xg27-7qjc |
| composer:2.11.1 | `sha256:537fb03057ed901ae06bde7ef25af348903485ae1acc7fc789b80d8bac666f35` | `sha256:5190d9b445add2fa26090108273cb1ef1e1af98175201c737b85b3c4f90fbf7c` | linux/riscv64 manifest added; amd64 and arm64 unchanged |
| rust:1.99.1-slim-trixie | `sha256:b8e062b1c3a80183df83d9ba187cadd2b5ab84bc78d453dbfeb2fccd5f982247` | `sha256:9daf2b244ccc30523e55cafe32edf8465671ba567d0101620e5612ccb76ea1a7` | Only linux/riscv64 rebuilt; amd64 and arm64 unchanged |

BuildKit v0.34.2 platform digests are `sha256:97719ccb2ac86fb44fd36f330a6d302f63dddd5326f3ada24f9c05ccac0d9098`
(linux/amd64) and `sha256:92f93f25966c544549b88144406ce8cd9dcdf5df128250711031a4acd9149a8e`
(linux/arm64). Composer `latest` now resolves to the same index as `2.11.1`, so
the riscv64-only `INFO` line no longer appears. Because the Rust image's amd64
and arm64 manifests are unchanged, the socat Debian snapshot timestamp stays
`20261027T000000Z`.

crc32c 0.6.9 was published from upstream revision
`3098a6edd7b1cb28198e3048889907b7160043e4` (tag `v0.6.9`). It now ships
`LICENSE-MIT` and `LICENSE-APACHE`, and `src/combine.rs` is byte-identical to
0.6.8, including the zlib attribution. The release notice exception was
re-reviewed for 0.6.9: the crate's own license texts are retained instead of
supplying the project's Apache-2.0 text, `src/combine.rs` and `ZLIB.txt`
(now under `distribution/notices/crc32c-0.6.9/`) keep the zlib terms, and
0.6.8 or any other version fails closed. The SDK lockfiles that resolve
crc32c 0.6.8 transitively through google-cloud-storage are unchanged.

### Validation performed on 2026-11-03

- `cargo xtask dependencies check` at `2026-11-03T13:08:40Z` exited 0: 118
  inputs were `CURRENT` and fake-gcs-server v1.56.1 stayed `PINNED_REFERENCE`.
- `just check` exited 0, including the production lint gate and every test
  binary (`xtask` 176 passed).
- `just check-container linux/arm64` exited 0: the image built from the adopted
  Rust index, generated release notices from crc32c 0.6.9 and passed every
  smoke case. linux/amd64 was not run locally; CI builds and smoke-tests it.
- The PHP suites were not rerun because the Composer amd64 and arm64 manifests
  are unchanged. The BuildKit pin is exercised only by the GHCR publication
  workflow.

## Refresh: 2026-11-04

A local check at `2026-11-04T10:19:23Z` reported four `UPDATE_AVAILABLE`
inputs. Two were adopted; the Rails pair is deferred under a visible exception.

| Input | Previous | Latest | Result |
| --- | --- | --- | --- |
| rust:1.99.1-slim-trixie | `sha256:9daf2b244ccc30523e55cafe32edf8465671ba567d0101620e5612ccb76ea1a7` | `sha256:ea0999ff67355d958b8799b6751c879a6d770076d26047dca66f6e51e77930d3` | Adopted; linux/amd64 and linux/arm64 rebuilt |
| google-auth-library | 11.3.2 | 11.3.3 | Adopted in the Node.js suite, scorecard and Testcontainers locks |
| activestorage | 8.1.4 | 8.1.4.1 | Deferred: `rails-8-1-4-1-gcs-direct-upload-content-length-range` |
| railties | 8.1.4 | 8.1.4.1 | Deferred with activestorage |

The rebuilt Rust index (docker-rust revision
`f6c59bee4a6e34a154c2122895ecd7833507f705`, created 2026-11-03) has new
platform digests `sha256:be49948243566aa0ae465ae2786c4bab8cb5597084873b01d3db50dcebc70b18`
(linux/amd64) and `sha256:a5b16f6cc783db8240559f7d2fe38a30e3885bed801db49e5e37d6b1223b68bf`
(linux/arm64). Both images still report `rustc 1.99.1 (58cf8f9c3 2026-10-13)`
and the Debian snapshot `20261027T000000Z` in
`/etc/apt/sources.list.d/debian.sources`, so the socat snapshot timestamp in
`sdk-scorecard/install-socat.sh` is unchanged. The Dockerfile,
`sdk-scorecard/run.sh` and `sdk-scorecard/test-tls.sh` pin the new index.

google-auth-library 11.3.3 (published 2026-11-03, Apache-2.0, 784,370 bytes
unpacked, Node.js >=22) replaces 11.3.2 as the suite's exact direct pin. The
scorecard and Testcontainers locks resolve it transitively through
`@google-cloud/storage`; each lock changes only this package's version,
tarball and integrity, and the suite and scorecard locks still resolve the
same package set.

### Rails 8.1.4.1 is deferred

Rails 8.1.4.1 is a security release. Two of its Active Storage changes alter
the GCS direct-upload contract
([release notes](https://github.com/rails/rails/releases/tag/v8.1.4.1)):

- GHSA-hfr4-8rjc-97m2: `url_for_direct_upload` and `headers_for_direct_upload`
  require `content_length:` and sign and send
  `x-goog-content-length-range: <size>,<size>`.
- GHSA-3w93-6hmw-whh4: direct-upload URLs are always signed with V2, including
  when `cache_control` is set.

With the suite's lockfile resolved to 8.1.4.1 (the lock also moved
nokogiri 1.19.4 to 1.19.7, loofah 2.25.2 to 2.25.3 and rails-html-sanitizer
1.7.1 to 1.7.2), `just check-active-storage` exited 1:

```text
Rails 8.1.4.1; googleauth 1.17.6
AS-DIRECT-UPLOAD: FAIL ArgumentError: missing keyword: :content_length
Summary: 1 PASS, 5 PASS_WITH_WORKAROUND, 2 FAIL
AS-EMULATOR-DIRECT-UPLOAD: FAIL RuntimeError: direct PUT failed: 400
AS-EMULATOR-DIRECT-UPLOAD-CORS: FAIL RuntimeError: preflight did not allow http://localhost:3000
AS-EMULATOR-V4-DIRECT-UPLOAD: FAIL RuntimeError: direct upload URL is not V4
Summary: 7 PASS, 3 FAIL
```

The direct PUT returned `400 InvalidArgument`: "gcs-mimic does not support XML
API x-goog-content-length-range". The same server binary with the committed
8.1.4 lockfile reported `1 PASS, 6 PASS_WITH_WORKAROUND, 1 FAIL` (the known
AS-DEFAULT-AUTH failure) and `10 PASS, 0 FAIL`. Adopting 8.1.4.1 therefore
needs emulator support for `x-goog-content-length-range` on XML uploads and
suite changes for the new keyword and V2-only signing, which is a feature
slice rather than a pin update. The exception in
[the snapshot](../../../compatibility/dependency-status.json) records the next
action and a 2026-11-11 review date.

### Validation performed on 2026-11-04

- `cargo xtask dependencies check` at `2026-11-04T10:26:17Z` exited 1: 116
  inputs were `CURRENT`, fake-gcs-server v1.56.1 stayed `PINNED_REFERENCE`, and
  only activestorage and railties reported `UPDATE_AVAILABLE` 8.1.4 -> 8.1.4.1.
- `just check-container linux/arm64` exited 0: the image built from the adopted
  Rust index and passed every smoke case. linux/amd64 was not run locally; CI
  builds and smoke-tests it.
- `bash scripts/sdk-go.sh --with-node` exited 0 over HTTP and over HTTPS
  (`GCS_MIMIC_SDK_SCHEME=https`), both run through
  `scripts/with-test-tmpdir.sh`. The Node.js files reported 7, 2, 24 and 27
  tests with 0 failures; the skips are the HTTPS-only cases over HTTP and the
  strict-only case on the permissive server.
- The Rails results above come from `just check-active-storage`. The strict
  Rails recipe was not run because the suite's pins are unchanged.

### Rails 8.1.4.1 adopted

XML object PUT now enforces `x-goog-content-length-range`
([slice](../slices/xml-content-length-range.md)), so the Rails Active Storage
suite adopts activestorage and railties 8.1.4.1 and the
`rails-8-1-4-1-gcs-direct-upload-content-length-range` exception is closed.
The lockfile resolves the same transitive changes as the deferred attempt:
nokogiri 1.19.4 to 1.19.7, loofah 2.25.2 to 2.25.3 and rails-html-sanitizer
1.7.1 to 1.7.2.

The suite follows the two Rails 8.1.4.1 contract changes:

- `scenario.rb` passes `content_length:` to `headers_for_direct_upload`. Blob
  methods already pass the blob's byte size.
- AS-EMULATOR-V4-DIRECT-UPLOAD becomes AS-EMULATOR-CACHE-CONTROL-DIRECT-UPLOAD:
  the `cache_control` entry now yields a V2 URL with an unsigned
  Cache-Control header.
- The new AS-EMULATOR-DIRECT-UPLOAD-SIZE sends the signed URL a larger and a
  smaller body and expects `EntityTooLarge` and `EntityTooSmall` with no object.
- `buckets.json` and the strict scenario's CORS rule allow
  `x-goog-content-length-range`, as the Rails guide at v8.1.4.1 does.
- The explicit `EmulatorGcs` V4 adapter requires `content_length:` and signs
  the range; the signing self-check fails when the range changes.

Validation on 2026-11-04:

- `cargo xtask dependencies check` at `2026-11-04T13:22:16Z` exited 0 with 118
  `CURRENT` inputs, including activestorage and railties 8.1.4.1;
  fake-gcs-server v1.56.1 stayed `PINNED_REFERENCE`.
- `just check-active-storage` reported `Rails 8.1.4.1`, scenario
  `1 PASS, 6 PASS_WITH_WORKAROUND, 1 FAIL` (the known AS-DEFAULT-AUTH failure)
  and storage.yml recipe `11 PASS, 0 FAIL`, with AS-EMULATOR-PRODUCTION-NO-KEY
  passing. The command exits 1 because of AS-DEFAULT-AUTH, as before.
- `bash sdk-tests/ruby/active_storage/run-strict.sh` reported the signing
  self-check PASS and `3 PASS, 0 FAIL`.

## Refresh: 2026-11-05

The release-scope check on 2026-11-05 reported `UPDATE_AVAILABLE` for
`rust:1.99.1-slim-trixie`.

| Input | Previous | Adopted | Change |
| --- | --- | --- | --- |
| rust:1.99.1-slim-trixie | `sha256:ea0999ff67355d958b8799b6751c879a6d770076d26047dca66f6e51e77930d3` | `sha256:59f72d5265cfcf281fcfa4d990e75f09c53f911def9913c47a4a89dca690a887` | Only linux/riscv64 and its attestation rebuilt; amd64 and arm64 unchanged |

`slim-trixie` resolves to the same index. The linux/amd64
(`sha256:be49948243566aa0ae465ae2786c4bab8cb5597084873b01d3db50dcebc70b18`)
and linux/arm64
(`sha256:a5b16f6cc783db8240559f7d2fe38a30e3885bed801db49e5e37d6b1223b68bf`)
manifests are byte-identical to the previous pin, so the snapshot timestamp
`20261027T000000Z` in `scripts/install-musl-tools.sh` and
`sdk-scorecard/install-socat.sh` is unchanged. The Dockerfile,
`sdk-scorecard/run.sh` and `sdk-scorecard/test-tls.sh` pin the new index.

### Validation performed on 2026-11-05

- `cargo xtask dependencies check` at `2026-11-05T06:31:32Z` exited 1. Both Rust
  image tags, the Debian stable codename and the musl and socat packages were
  `CURRENT`, with 113 `CURRENT` lines in total and fake-gcs-server v1.56.1
  `PINNED_REFERENCE`. Three SDK-suite inputs released upstream the same day
  reported `UPDATE_AVAILABLE`: google.golang.org/api v0.310.0 -> v0.311.0
  (two manifests), google.golang.org/genproto
  v0.0.0-20261026165753-2ffab32bce7e -> v0.0.0-20261104224025-d1e00e9f33ab and
  bundler 4.0.23 -> 4.0.24 (two manifests). They are not adopted in this
  refresh.
- No container was rebuilt for this pin: the declared platform manifests the
  builds use are unchanged, and both architectures were built and smoke-tested
  from them the same day ([scratch runtime](scratch-runtime.md)).

## Refresh: 2026-11-05 SDK inputs

The three SDK-suite updates reported by the same check are adopted.

| Input | Previous | Adopted | Manifests |
| --- | --- | --- | --- |
| google.golang.org/api | v0.310.0 | v0.311.0 (published 2026-11-04, revision `2a8925bc33b378b7172830a4b2d154b7952055bf`) | `go/go.mod`, `sdk-tests/go/go.mod` |
| google.golang.org/genproto | v0.0.0-20261026165753-2ffab32bce7e | v0.0.0-20261104224025-d1e00e9f33ab | `sdk-tests/go/go.mod` |
| bundler | 4.0.23 | 4.0.24 (published 2026-11-05, SHA-256 `16ca64e20816a0cf151a948fb141d788602996d2b4d73577b74388fe84fa98bc`) | `sdk-tests/ruby/Gemfile.lock`, `sdk-tests/ruby/active_storage/Gemfile.lock` |

The API client release notes list regenerated Discovery clients only, and its
BSD-3-Clause LICENSE is byte-identical to v0.310.0; the module still requires
Go 1.26.0. `go get` and `go mod tidy` with Go 1.27.3 also raise, as indirect
requirements, cloud.google.com/go/auth/oauth2adapt to v0.3.1, golang.org/x/net
to v0.62.0 and genproto/googleapis/rpc to v0.0.0-20261026165753-2ffab32bce7e in
both modules. In `go/go.mod` they bring cel.dev/expr v0.25.3, the Envoy
pseudo-version and grpc v1.85.0, which `sdk-tests/go/go.mod` already resolved.
Bundler 4.0.24 fixes `bundle exec` paths with spaces, gemspec dependency groups
and platforms, lock checksums after platform changes and file locking; each
lockfile changes only its Bundler checksum and `BUNDLED WITH`.

`examples/go`, `examples/ruby`, `sdk-scorecard/go` and `sdk-scorecard/ruby` use
the same previous versions but are not tracked by the freshness check, so they
are unchanged here.

### Validation performed on 2026-11-05 for SDK inputs

- `cargo xtask dependencies check` at `2026-11-05T06:41:59Z` exited 0 with 118
  `CURRENT` inputs; fake-gcs-server v1.56.1 stayed `PINNED_REFERENCE`.
- `bash scripts/sdk-go.sh --with-ruby`, run through
  `scripts/with-test-tmpdir.sh`, exited 0: Go vet and 88 passing Go tests (the
  V2 signed URL test skips by design outside HTTPS mode) and every Ruby
  permissive and strict journey.
- `go test -race -tags integration ./...` in `go` with `GCS_MIMIC_BINARY` set
  as `scripts/sdk-go.sh` sets it, and `go test -race ./...` in
  `go/sourceexport`, passed.
- `just check-active-storage` reported `Rails 8.1.4.1`, scenario
  `1 PASS, 6 PASS_WITH_WORKAROUND, 1 FAIL` (the known AS-DEFAULT-AUTH failure)
  and storage.yml recipe `11 PASS, 0 FAIL`; it exits 1 because of
  AS-DEFAULT-AUTH, as before. `bash sdk-tests/ruby/active_storage/run-strict.sh`
  reported the signing self-check PASS and `3 PASS, 0 FAIL`.

## Refresh: 2026-11-06

The scheduled check on 2026-11-05 reported `UPDATE_AVAILABLE` for
`rust:1.99.1-slim-trixie`, and the check on 2026-11-06 also reported Composer and
the Java SDK inputs.

| Input | Previous | Adopted | Change |
| --- | --- | --- | --- |
| rust:1.99.1-slim-trixie | `sha256:59f72d5265cfcf281fcfa4d990e75f09c53f911def9913c47a4a89dca690a887` | `sha256:7eab7ce19273ce785e0ca7578bdf7bc1fcbae621775a8197379a04c9f6e03cd5` | linux/amd64 and linux/arm64 rebuilt; Rust stays 1.99.1 |
| Debian snapshot for musl and socat | `20261027T000000Z` | `20261104T212339Z` | Read from the new image; package versions unchanged |
| composer:2.11.1 | `sha256:5190d9b445add2fa26090108273cb1ef1e1af98175201c737b85b3c4f90fbf7c` | `sha256:3d1eb9437516b0ba88c46a0bb05826451a1ab178d29adacb5ded26dad0899ae4` | linux/amd64 and linux/arm64 rebuilt; still PHP 8.5.12 and Composer 2.11.1 |
| com.google.cloud:google-cloud-storage | 2.76.0 | 2.77.0 | `sdk-tests/java`, `sdk-scorecard/java` |
| com.google.auth:google-auth-library-oauth2-http | 1.55.0 | 1.56.0 | `sdk-scorecard/java` |

`examples/java` is not tracked by the freshness check and stays on 2.76.0.
Evidence recorded for the earlier Java versions keeps those versions.

The same run failed Foundation checks on external hosts: musl.libc.org timed out
during a TLS handshake, and on a re-run snapshot.debian.org answered 504 after
all eight apt attempts. The build now installs the same pinned musl version from
deb.debian.org when the snapshot stays unavailable, and uses the musl source
archive committed under `distribution/notices/musl-1.2.5`, whose SHA-256
`a9a118bbe84d8764da0ea0d28b3ab3fae8477fc7e4085d90102b8596fc7c75e4` matched a
fresh download.

### Validation performed on 2026-11-06

- `cargo xtask dependencies check` at `2026-11-06T07:33:53Z` exited 0 with 118
  `CURRENT` inputs; fake-gcs-server v1.56.1 stayed `PINNED_REFERENCE`.
- With snapshot.debian.org mapped to an unreachable address, the install scripts
  installed musl 1.2.5-3.1~deb13u1 on linux/amd64 and linux/arm64 and socat
  1.8.0.3-1+deb13u1 on linux/amd64 from deb.debian.org. With deb.debian.org also
  unreachable, the musl script exited 100.
- `just check-containers` exited 0 on linux/amd64 and linux/arm64 with the new
  image pin and the committed musl archive.
- `scripts/sdk-go.sh --with-java --with-php`, run through
  `scripts/with-test-tmpdir.sh`, exited 0, including the Java HTTP scenario and
  the PHP strict HTTP cases.
- `mvn compile` in `sdk-scorecard/java` passed. The full scorecard, the Java
  Testcontainers scenario and the release archive workflow were not run locally.

### TCP keep-alive direct edge

`socket2` 0.6.5 is reused to enable TCP keep-alive on accepted sockets, which std
and Tokio do not expose. The [official registry index](https://index.crates.io/so/ck/socket2)
identifies it as the latest non-yanked stable and records Rust 1.70 as its minimum.
It declares `MIT OR Apache-2.0`, and its compressed archive is **60,031 bytes**, with
SHA-256 `c3d1e2c7f27f8d4cb10542a02c49005dbd6e93095799d6f3be745fae9f8fedd4`. The
[rust-lang repository](https://github.com/rust-lang/socket2) is not archived; its
last push was 2026-10-18.

The manifest and root lockfile add the application's direct edge to the package
Tokio already resolves. The resolved version and checksum do not change, and no
package is added.

A `cargo xtask dependencies check` run reported `socket2: CURRENT`. That run did not
finish: `proxy.golang.org` timed out with `CHECK_FAILED`, and it reported
`UPDATE_AVAILABLE` for flate2 1.1.11, adopted in the later refresh below.

### Later refresh on 2026-11-06

| Input | Before | After |
| --- | --- | --- |
| flate2 | 1.1.10 | 1.1.11, SHA-256 `626d4928326b646e70f65ed36fc7cf9f206bdb68a0019ee7c6b4709da4e084c4` |
| composer:2.11.1 | `sha256:3d1eb9437516b0ba88c46a0bb05826451a1ab178d29adacb5ded26dad0899ae4` | `sha256:8de2184c6de224f4689e8f80ed5f5edaf0030b6b8b7839a22a2bcb6722ac87d0` |

flate2 1.1.11 was published on 2026-11-06. The [official registry](https://crates.io/crates/flate2/1.1.11)
records `MIT OR Apache-2.0`, Rust 1.67 as its minimum and a compressed archive of **89,270 bytes**; the
[rust-lang repository](https://github.com/rust-lang/flate2-rs) is not archived. The lockfile changes only
its version and checksum, and no package is added.

The Composer index changed while its `linux/amd64` and `linux/arm64` manifests stayed at the recorded
digests, so the PHP suites run the same images.

`GITHUB_TOKEN=... cargo xtask dependencies check` at 2026-11-06T12:29:07Z exited 0 with 119 `CURRENT`
inputs; fake-gcs-server v1.56.1 stays `PINNED_REFERENCE`. The gzip tests passed with the new flate2.

### Refresh before the 0.12.0 release

| Input | Before | After |
| --- | --- | --- |
| com.google.auth:google-auth-library-oauth2-http | 1.56.0 | 1.57.0, jar of **395,167 bytes**, `sdk-scorecard/java` only |

This scorecard probe is a development-only input and ships in no release artifact. Its lockfile changes only that version.

`GITHUB_TOKEN=... cargo xtask dependencies check` at 2026-11-06T17:28:50Z exited 0 with 119 `CURRENT` inputs; fake-gcs-server v1.56.1 stays `PINNED_REFERENCE`. The Java suites run in the `just pre-push` recorded for the 0.12.0 release.

## Refresh: 2026-11-07

`cargo xtask dependencies check` reported `UPDATE_AVAILABLE` for the Java SDK in both Java manifests.

| Input | Previous | Adopted | Change |
| --- | --- | --- | --- |
| com.google.cloud:google-cloud-storage | 2.77.0 | 2.78.0, jar of **1,549,685 bytes** | `sdk-tests/java`, `sdk-scorecard/java` |

The [Maven Central metadata](https://repo1.maven.org/maven2/com/google/cloud/google-cloud-storage/maven-metadata.xml) names 2.78.0 as both the latest and the release version; its jar was published on 2026-11-06 and its POM declares Apache-2.0. Development continues in the [Google Cloud Java monorepo](https://github.com/googleapis/google-cloud-java/tree/main/java-storage), which is not archived; its last push was 2026-11-07.

Both lockfiles change only the Google libraries the SDK brings:

| Package | Previous | Adopted |
| --- | --- | --- |
| google-cloud-core, google-cloud-core-http, google-cloud-core-grpc | 2.79.0 | 2.80.0 |
| gax, gax-grpc, gax-httpjson | 2.89.0 | 2.90.0 |
| api-common | 2.72.0 | 2.73.0 |
| proto-google-iam-v1 | 1.75.0 | 1.76.0 |
| proto-google-common-protos | 2.80.0 | 2.81.0 |
| proto-, grpc- and gapic-google-cloud-storage-v2 | 2.77.0 | 2.78.0 |
| google-auth-library-credentials | 1.56.0 | 1.57.0 |
| google-auth-library-oauth2-http, `sdk-tests/java` only | 1.56.0 | 1.57.0 |

In `sdk-scorecard/java` the SDK now resolves google-auth-library-oauth2-http 1.57.0, the version the probe already pins for offline signing, so that pin stays. gRPC and protobuf are unchanged. `examples/java` is not tracked by the freshness check and stays on 2.76.0.

### musl allocator

`mimalloc` 0.1.52 and `libmimalloc-sys` 0.1.49 are new packages, resolved only for musl targets
through a `cfg(target_env = "musl")` dependency. The
[registry index](https://index.crates.io/mi/ma/mimalloc) identifies `mimalloc` 0.1.52 and the
[registry index](https://index.crates.io/li/bm/libmimalloc-sys) `libmimalloc-sys` 0.1.49 as the
latest non-yanked stable releases on 2026-11-07. Both declare MIT, and the
[repository](https://github.com/purpleprotocol/mimalloc_rust) is not archived; its last push was
2026-06-30.

| Package | Compressed archive bytes | SHA-256 |
| --- | --- | --- |
| `mimalloc` 0.1.52 | 5,382 | `2d4139bb28d14ad1facf21d5eb8825051b326e172d216b39f6d31df53cc97862` |
| `libmimalloc-sys` 0.1.49 | 500,217 | `6a45a52f43e1c16f667ccfe4dd8c85b7f7c204fd5e3bf46c5b0db9a5c3c0b8e9` |

The vendored allocator is Microsoft's, under its own MIT copyright, so `xtask release notices` also
copies `c_src/mimalloc/v3/LICENSE` and fails if a later version moves it. The default features
select allocator v3. Version 2 was tried and held about twice the idle memory at the same speed.

[Scratch runtime evidence](scratch-runtime.md#throughput) rejected an allocator dependency because
a trial build doubled resident memory. Two findings reverse that. The memory came mostly from
transparent huge pages, not the allocator: with them disabled for the process through
`prctl(PR_SET_THP_DISABLE)`, idle resident memory is about 8 MiB. And the catalog thread then capped
all reads, which hid most of what musl's allocator cost; see the
[read path measurement](read-path-performance.md).

### Validation performed on 2026-11-07

- `cargo xtask dependencies check` at `2026-11-07T11:40:05Z` exited 0 with 120 `CURRENT` inputs, `mimalloc` among them; fake-gcs-server v1.56.1 stayed `PINNED_REFERENCE`.
- `bash sdk-tests/java/run.sh --self-check` matched the lockfile, compiled and passed `java-origin-guard`. `bash sdk-scorecard/java/run.sh build` matched its lockfile and compiled.
- `scripts/sdk-go.sh --with-java`, run through `scripts/with-test-tmpdir.sh`, exited 0: 88 Go tests passed, the V2 signing test that needs the HTTPS profile was skipped, and the Java HTTP scenario passed.
- The Java Testcontainers scenario passed against an image built from the SDK update alone, before it joined the read path changes.
- The Java scorecard probe, run alone against this source's gcs-mimic with the scorecard's TLS front, gRPC listener and test CA, passed all 70 cases, the same cases as the recorded `2026-10-31-2` run. The full scorecard was not run, so no results directory is added.
- `cargo test -p xtask --locked` passed 222 tests, including the check of the actual Maven pins.

## Image digest refresh: 2026-11-09T03:43:55Z

`cargo xtask dependencies update-images` found these images rebuilt under an unchanged version tag. The checks of the pull request that adopts them are the validation; this record does not establish a result by itself.

| Input | Previous | Proposed | Changed manifests |
| --- | --- | --- | --- |
| rust:1.99.1-slim-trixie | `sha256:7eab7ce19273ce785e0ca7578bdf7bc1fcbae621775a8197379a04c9f6e03cd5` | `sha256:f6c4a35d21dbb3c89d7a8d056c193e94fc09037f2b58dd858b09c39a47dd02d7` | None on the declared platforms |
