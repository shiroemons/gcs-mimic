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

## Refresh: 2026-11-09 Go modules

The two module updates reported by the full check before the 0.16.0 release are adopted. Neither is a release input, and `cargo xtask dependencies check --scope release` passed without them.

| Input | Previous | Adopted | Manifest |
| --- | --- | --- | --- |
| golang.org/x/oauth2 | v0.37.0 | v0.38.0 (published 2026-11-06, revision `0f01e8215572819016fac21b005f5f32ba407798`) | `sdk-tests/go/go.mod` |
| golang.org/x/sys | v0.49.0 | v0.50.0 (published 2026-11-06, revision `00227f1d192842e243cba7763c1f9a1fcb1c4344`) | `go/sourceexport/go.mod` |

Both releases still require Go 1.26.0 and retract nothing, and their BSD-3-Clause LICENSE files are byte-identical to the previous releases. oauth2 v0.38.0 has one change, which prevents a `NewTicker` panic. x/sys v0.50.0 has ten changes: Windows system call and structure corrections, a `ppc64x` syscall fix, a `wasm32` CPU architecture, `IoctlGetUint64`, rejection of a short `SCM_CREDENTIALS` payload, Linux directory entries with zero inodes and a FreeBSD/386 `sendfile` count. `go get` and `go mod tidy` with Go 1.27.3 change no other requirement.

Indirect requirements and the `examples/go` and `sdk-scorecard/go` modules are not tracked by the freshness check, so they are unchanged here.

### Validation performed on 2026-11-09 for Go modules

- `cargo xtask dependencies check` at `2026-11-09T14:28:30Z` exited 0 with 120 `CURRENT` inputs; fake-gcs-server v1.56.1 stayed `PINNED_REFERENCE`.
- `go test -race ./...` in `go/sourceexport` passed.
- `just pre-push`, which runs the Go SDK suite, passed on the 0.16.0 release source.

## Refresh: 2026-11-10 SDK test inputs

The three updates reported by the full check before the 0.17.0 release are adopted. None is a release input, and `cargo xtask dependencies check --scope release` exited 0 before them.

| Input | Previous | Adopted | Manifest |
| --- | --- | --- | --- |
| org.codehaus.mojo:exec-maven-plugin | 3.6.4 | 3.7.0 | `sdk-tests/java/pom.xml`, `sdk-scorecard/java/pom.xml` |
| google-auth-library | 11.3.3 | 11.3.4 (published 2026-11-09, Apache-2.0, 785,192 bytes unpacked, Node.js >=22) | `sdk-tests/node/package.json` |
| google.golang.org/genproto | v0.0.0-20261104224025-d1e00e9f33ab | v0.0.0-20261110000943-09e21b237f2c (revision `09e21b237f2ce1c513f82bf67a7740af8d8f6e03`, Go 1.26.0) | `sdk-tests/go/go.mod` |

The genproto update also raises `cloud.google.com/go/iam` to v1.15.1 and `genproto/googleapis/api` and `rpc` to v0.0.0-20261104224025-d1e00e9f33ab as indirect requirements of the Go SDK suite. Inputs outside the freshness check, such as `examples/java`, are unchanged.

### Validation performed on 2026-11-10 for SDK test inputs

- `cargo xtask dependencies check` at `2026-11-10T05:01:50Z` exited 0 with 120 `CURRENT` inputs; fake-gcs-server v1.56.1 stayed `PINNED_REFERENCE`.
- `just check-sdk` passed all eight SDK suites.
- `bash sdk-scorecard/java/run.sh build` passed.
- `go mod verify` in `sdk-tests/go` reported all modules verified.

## Refresh: 2026-11-11

`just check-dependencies` exited 1 with five kinds of drift. Four are adopted; the .NET SDK is a new major release and is not.

| Input | Previous | Adopted | Change |
| --- | --- | --- | --- |
| prost, prost-types | 0.14.4 | 0.14.5 (published 2026-11-10, Apache-2.0, 34,590 and 50,257 compressed bytes, Rust 1.85) | `Cargo.toml`, `Cargo.lock`; prost-derive follows to 0.14.5 |
| Composer image | 2.11.1 `sha256:8de2184c…` | 2.11.2 `sha256:5bcb28b8d0edc4091125f8218f9d088f998bdfc843f8fbb9a4beb4dcd8625684` | `sdk-tests/php/run.sh`, `sdk-scorecard/run.sh`, `sdk-scorecard/test-tls.sh` |
| Go toolchain | go1.27.3 | go1.27.4 | `sdk-tests/go`, `go` and `go/sourceexport` modules |
| google.golang.org/api | v0.311.0 | v0.312.0 (published 2026-11-10, revision `be503e8963ac11ca328020a6a42316b00b3d8601`, Go 1.26.0) | `sdk-tests/go/go.mod`, `go/go.mod` |
| .NET SDK | 10.0.403 | Not adopted: 11.0.100 | `sdk-tests/dotnet/global.json`, `sdk-scorecard/dotnet/global.json` unchanged |

prost 0.14.5 is a patch release of the same official tokio-rs family; prost-build stays 0.14.4 because only tonic-prost-build requires it, and the freshness check covers direct dependencies. Composer 2.11.2 is a security release: its changelog fixes command execution and credential exposure through crafted lock files, writes outside the package directory through symlinked zip entries, and `Authorization` and `Cookie` headers resent on cross-origin redirects. The image still reports PHP 8.5.12, and its linux/amd64 and linux/arm64 manifests (`sha256:c4a7ecd0…`, `sha256:d76824a7…`) equal those behind `latest`, which differs only in its linux/riscv64 entry. API v0.312.0 lists regenerated Discovery clients only, and its BSD-3-Clause LICENSE is byte-identical to v0.311.0. It raises enterprise-certificate-proxy to v0.3.25 and golang.org/x/sync to v0.25.0 indirectly; in the `go` facade `go get` also raises golang.org/x/oauth2 to v0.38.0 and genproto/googleapis/rpc to v0.0.0-20261104224025-d1e00e9f33ab, the versions the SDK suite already uses.

.NET 11.0.100 was released on 2026-11-10 as the first SDK of the 11.0 channel (STS). Moving the two `global.json` pins, which disable roll-forward, across a major SDK version is a migration of the .NET suite and scorecard, not a refresh, so it is left for a deliberate change; no exception is recorded, and the full check keeps failing until it lands. The 10.0 channel's own latest SDK is 10.0.404, released the same day; adopting it alone would not make the check pass.

### Validation performed on 2026-11-11

- `cargo check --workspace --all-targets --locked` passed and `cargo test --locked --lib grpc` passed 169 tests after the prost update.
- `cargo test -p xtask --locked` passed, including the check of the actual pins against the snapshot.
- In the Composer 2.11.2 image, `composer --version` reported Composer 2.11.2 with PHP 8.5.12, and `composer validate` and `composer check-platform-reqs --lock` passed for `sdk-tests/php` and `sdk-scorecard/php`. The PHP SDK suite itself was not run.
- `go mod tidy`, `go mod verify` and `go vet ./...` passed in all three Go modules with Go 1.27.4, `go vet -tags integration ./...` passed in `go`, and `go test -race ./...` passed in `go/sourceexport`. The Go SDK suite against a server was not run.
- `cargo xtask dependencies check` at `2026-11-11T00:59:44Z` exited 1 with 121 `CURRENT` inputs; only the .NET SDK reported `UPDATE_AVAILABLE` in both .NET manifests, and fake-gcs-server v1.56.1 stayed `PINNED_REFERENCE`.

### .NET SDK 11 migration

With the project owner's approval the .NET SDK 11.0.100 is adopted after all, in a separate change. `global.json` in `sdk-tests/dotnet`, `sdk-scorecard/dotnet` and `examples/dotnet` pins 11.0.100 with roll-forward still disabled, and CI keeps reading it through `actions/setup-dotnet`.

| Project | Target frameworks | Reason |
| --- | --- | --- |
| `sdk-tests/dotnet` | `net10.0` and `net11.0` | Keep proving the long-term-support runtime while also running on the newest one |
| `sdk-scorecard/dotnet`, `examples/dotnet` | `net11.0` | The runtime SDK 11.0.100 installs |

SDK 11 installs only the .NET 11 runtime, and `actions/setup-dotnet` installs SDKs only, so the .NET 10 runtime comes from the exact LTS SDK pinned in `sdk-tests/dotnet/.dotnet-lts-sdk-version`: 10.0.403, carrying runtime 10.0.14, the version already installed on the development host. The client-tools action installs it beside the `global.json` SDK. `sdk-tests/dotnet/run.sh` runs every mode, `--container` included, once per target framework, prints the runtime each run used, and fails if either run fails or runs on another major runtime; `scripts/test-dotnet-frameworks.sh` checks that with a stub `dotnet`. On `net11.0` the lockfiles omit Microsoft.Extensions.DependencyInjection.Abstractions and Microsoft.Extensions.Logging.Abstractions, which the .NET 11 shared framework supplies; direct pins are unchanged.

The freshness check now reads `TargetFrameworks`, checks the lock of every target framework, requires every target to run on a pinned runtime, and compares the LTS SDK pin with the latest SDK of its own supported LTS channel. That comparison reports 10.0.404, the 10.0 channel's 2026-11-10 security release with runtime 10.0.15. It is not adopted: installing it on the development host awaits the owner's approval, and no exception is recorded.

- `bash sdk-tests/dotnet/run.sh --self-check` printed `dotnet-runtime: .NET 10.0.14` and `.NET 11.0.0` with `dotnet-origin-guard: PASS` for each target.
- `scripts/sdk-go.sh --with-dotnet`, run through `scripts/with-test-tmpdir.sh`, exited 0: 90 Go tests passed and the V2 signing test that needs the HTTPS profile was skipped; the .NET suite printed `dotnet-http: resumable, metadata, generation, conditions, read, list PASS` on .NET 10.0.14 and .NET 11.0.0 and `PASS: .NET SDK suite on net10.0 and net11.0`.
- `bash sdk-scorecard/dotnet/run.sh build` built the net11.0 probe with 0 warnings and 0 errors. The scorecard itself was not run.
- `bash scripts/test-dotnet-frameworks.sh` and `cargo test -p xtask --locked` (221 tests) passed.
- `cargo xtask dependencies check` at `2026-11-11T01:44:36Z` exited 1 with 123 `CURRENT` lines, the .NET SDK `CURRENT` at 11.0.100 in both .NET manifests; only `.NET LTS SDK: UPDATE_AVAILABLE current=10.0.403 latest=10.0.404` remained, and fake-gcs-server v1.56.1 stayed `PINNED_REFERENCE`.

### LTS .NET SDK 10.0.404

With the owner's approval of the host install, the LTS SDK pin moves from 10.0.403 to 10.0.404, the 10.0 channel's 2026-11-10 security release, which carries runtime 10.0.15. SDK 10.0.403 stays installed on the development host beside it; `global.json` keeps SDK 11.0.100.

- `scripts/sdk-go.sh --with-dotnet`, run through `scripts/with-test-tmpdir.sh`, exited 0: 90 Go tests passed and the HTTPS-only V2 signing test was skipped; the .NET suite printed `dotnet-runtime: .NET 10.0.15` and `dotnet-runtime: .NET 11.0.0`, each followed by `dotnet-http: resumable, metadata, generation, conditions, read, list PASS`, and `PASS: .NET SDK suite on net10.0 and net11.0`.
- `just check` exited 0.
- `cargo xtask dependencies check` at `2026-11-11T01:49:28Z` exited 0 with 124 `CURRENT` lines, among them `.NET SDK: CURRENT current=11.0.100 latest=11.0.100` in both .NET manifests and `.NET LTS SDK: CURRENT current=10.0.404 latest=10.0.404`; fake-gcs-server v1.56.1 stayed `PINNED_REFERENCE`.

### Go 1.27.4 in the remaining modules

The two Go modules outside daily detection also move to the go1.27.4 toolchain. `sdk-scorecard/go/go.mod` joins detection, like the .NET scorecard probe, so its requirements move to the versions the SDK suite already uses: IAM v1.15.1, OAuth2 v0.38.0, API v0.312.0 and genproto v0.0.0-20261110000943-09e21b237f2c. `go mod tidy` then marks OAuth2, genproto, gRPC and protobuf indirect, because no package of the module imports them, leaving IAM, Storage and API as its direct requirements, and raises oauth2adapt, enterprise-certificate-proxy, x/net, x/sync and genproto/googleapis/api and rpc. `examples/go` changes only its toolchain and stays outside detection, like `examples/dotnet`. CI keeps reading the Go version from `sdk-tests/go/go.mod`; no Dockerfile, script or workflow names a Go version directly.

- `go build` of the probe and of `./cmd/tls-front`, `go vet ./...` and `go test ./...` passed in `sdk-scorecard/go` with go1.27.4; `go build` and `go vet ./...` passed in `examples/go`, and `go mod verify` passed in both. Neither the scorecard nor the example was run against a server.
- `cargo test -p xtask --locked` passed every test binary (221, 11, 3 and 1 tests), including the declared-manifest checks that now require the scorecard module, its scope and its `go.sum` in the snapshot.
- `cargo xtask dependencies check` at `2026-11-11T02:20:01Z` exited 0 with 129 `CURRENT` lines, the Go toolchain `CURRENT` at 1.27.4 in all four checked Go modules; fake-gcs-server v1.56.1 stayed `PINNED_REFERENCE`.

### rusqlite 0.42.0

The 2026-11-11 refresh adopts rusqlite 0.42.0 and its required
libsqlite3-sys 0.40.0. Only those two package records change in Cargo.lock;
application code, SQL, catalog schema and feature statuses are unchanged.
There is no new direct dependency or additional enabled feature.

The official [Cargo index](https://index.crates.io/ru/sq/rusqlite) selected
0.42.0 as the highest numeric non-yanked stable release at
`2026-11-11T08:43:45Z`. The [publisher release](https://github.com/rusqlite/rusqlite/releases/tag/v0.42.0)
was published at `2026-11-11T06:47:37Z`; both archives record source revision
`812d63147ebea21c9c3de4db9e556f2e4e9f8161`. The publisher repository remains
unarchived, with its last push at `2026-11-11T06:47:36Z`.
Official downloaded archives match both index checksums and Cargo's cached archives.

| Package | Previous | Adopted | License | Compressed bytes | Declared Rust | SHA-256 |
| --- | --- | --- | --- | --- | --- | --- |
| rusqlite | 0.41.1 | 0.42.0 | MIT | 184,670 | 1.89.0 | `a4edff0039f148731aee2fd80f76dbfeb12d204ab3d1c544838d34e02151049b` |
| libsqlite3-sys | 0.39.1 | 0.40.0 | MIT | 2,759,453 | 1.88.0 | `b06e69070b7e12262b0cd90832f0a7396f142a2b330181255562decf9f0c6215` |

Sources: [rusqlite archive](https://static.crates.io/crates/rusqlite/rusqlite-0.42.0.crate),
[binding index](https://index.crates.io/li/bs/libsqlite3-sys),
[binding archive](https://static.crates.io/crates/libsqlite3-sys/libsqlite3-sys-0.40.0.crate).
Both distributed manifests retain edition 2024 and fit the pinned Rust 1.99.1.
Sizes describe source archives, not binary growth or runtime memory.

#### Source compatibility comparison

Comparing the previous and adopted distributed `Cargo.toml.orig` files shows
unchanged definitions for the three enabled features: `bundled` selects the
bundled C library and `modern_sqlite` (prebuilt bundled bindings), `cache`
selects hashlink, and `fallible_uint` enables checked unsigned integer
conversions without adding a dependency. `src/cache.rs`,
`src/types/to_sql.rs` and `src/types/from_sql.rs` are byte-identical between
rusqlite 0.41.1 and 0.42.0. Unsigned writes still reject values above
`i64::MAX`, and unsigned reads still reject negative integers.
`cargo tree --locked --offline -e features -i rusqlite` confirms the same
three explicit features, implied `modern_sqlite` and hashlink, with defaults disabled.

Between libsqlite3-sys 0.39.1 and 0.40.0, the SQLite amalgamation, header,
prebuilt bundled bindings and complete `build.rs` are byte-identical:

| File | SHA-256 in both releases |
| --- | --- |
| `sqlite3/sqlite3.c` | `0c657666d819f261cc4007526ad57e1669de9949df7a8ea847aa284a2bf0ee52` |
| `sqlite3/sqlite3.h` | `d6c84557261893439b341d2e6c35187ae3ea24fbfd843ad94616f72d89242b8c` |
| `sqlite3/bindgen_bundled_version.rs` | `a25057a06d8cb5053a61c8f0ff746cc3d81b3bd778fbd3199ba12c8a6ee2945d` |
| `build.rs` | `769b31370a41aa722e1d5a530012432c985bb142eab34d095481ffd1a3436e1b` |

SQLite remains **3.54.1**, source ID
`2026-10-16 14:45:20 555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`.
The unchanged build retains `SQLITE_THREADSAFE=1`, API armor and default foreign
keys, along with the same extension and platform flags. No synchronization or
durability setting changes. Existing catalog tests cover WAL, synchronous FULL,
foreign keys, bounded caches, disabled mmap, busy timeout and checked SQL values.

The release repairs callback ownership and allocation failure handling, extends
SQLCipher support and adds `RawValue`. Session, function, collation and SQLCipher
features remain disabled; the application does not adopt the new type. No API
migration is required. Both packaged MIT license texts remain byte-identical
(SHA-256 `c10c1f27337546471e5f7e4e97fdd398b35b9d4e126115dcd22de8d8e65abf6f`),
as does SQLite's public-domain notice. Release packaging discovers the resolved
crate versions and copies these notices automatically; there is no rusqlite
version-specific notice exception to update.

#### Freshness result

`cargo xtask dependencies check` at `2026-11-11T08:46:02Z` exited 0 with
129 `CURRENT` lines, zero updates and zero check errors. The following relevant
lines are verbatim; the pinned migration reference is intentionally not an
updatable dependency, and the Composer informational difference concerns only
an undeclared platform:

```text
rusqlite: CURRENT current=0.42.0 latest=0.42.0
docker library/composer:latest: CURRENT on declared platforms linux/amd64, linux/arm64
INFO: latest differs only on linux/riscv64
github.com/fsouza/fake-gcs-server: PINNED_REFERENCE current=v1.56.1 latest=v1.56.1; migration contract is revision 896ece51e49eed0946089f3089f6343855c0d106
```

#### Validation performed for rusqlite 0.42.0

| Command/check | Result |
| --- | --- |
| `cargo update -p rusqlite --precise 0.42.0 --offline` | PASS: only rusqlite and libsqlite3-sys change |
| `cargo check --locked --tests` | PASS |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS |
| `cargo xtask lint` | PASS: 0 new production violations; 0 ratcheted findings |
| `cargo test --locked --lib` | PASS with local socket/watch permissions: 2,042 passed, 0 failed, 0 ignored |
| `cargo test --locked --test it_import_seed` | PASS: 94 passed, 0 failed, 0 ignored |
| Native `cargo xtask release prepare aarch64-apple-darwin target/debug/gcs-mimic <output>` | PASS: generated notices include rusqlite 0.42.0 and libsqlite3-sys 0.40.0 MIT texts and SQLite's public-domain text |
| `npx --yes markdownlint-cli2 docs/en/dependency-policy.md docs/en/development.md docs/en/evidence/dependency-refresh.md` | PASS: 0 issues |
| `cargo xtask dependencies check` | PASS: exit 0, 129 CURRENT lines; verbatim relevant output above |

The import/seed target includes
`every_supported_release_has_a_fixture` and
`supported_release_data_directories_upgrade_without_changing_resources`.
It opens the fixtures from releases 0.8.0 through 0.17.1, compares metadata and
object bytes through HTTP, and reopens each upgraded directory a second time.
The native notice check packages a debug binary only; it is not a release build
or publication. `<output>` stands for an external temporary directory.

The initial sandbox library run exited 101 with
`test result: FAILED. 1824 passed; 218 failed; 0 ignored; 0 measured; 0 filtered out; finished in 233.97s`;
local socket creation reported
`Os { code: 1, kind: PermissionDenied, message: "Operation not permitted" }`
and native observation was unavailable. The same command was then rerun once
with those local permissions, producing the passing result above without source
or test changes. The initial network-limited freshness and Markdown checks also
failed on DNS resolution; their authorized network runs passed.

After rebasing onto `371c8e5a`, the type check, workspace Clippy, production lint
and all 94 import/seed integration tests passed again. The 2,042-test library
result precedes that rebase; it was not repeated because the integrated CLI
changes do not alter the adopted dependency graph or storage implementation.

The full test suite, SDK suites, container builds, Linux/Windows
runtime checks, benchmarks and remote CI were not run. Final combined validation
belongs to integration. No feature-ledger or generated-matrix change is required;
Japanese companion guides contain no corresponding version sentence to update.

## Refresh: 2026-11-12

The v0.19.0 tag publication ([run 43126574525](https://github.com/shiroemons/gcs-mimic/actions/runs/43126574525)) stopped in `validate`, before any job that publishes: the release-scoped check reported `docker moby/buildkit:latest: UPDATE_AVAILABLE on linux/amd64, linux/arm64`. BuildKit v0.35.0 had been released at `2026-11-11T20:12:10Z`, after the v0.19.0 dry run. BuildKit is a release input, so it is adopted before the release.

| Input | Previous | Adopted | Change |
| --- | --- | --- | --- |
| moby/buildkit | v0.34.2 `sha256:c5f8862f52499773af85bc07695b2379ef08e8002352b0df2c4864c91d78dabe` | v0.35.0 `sha256:aa632b921ef07411d2cb77292cd34213d4f7b384001d6fceef1e1f25bf9752bb` | Built-in Dockerfile frontend 1.29.0, retries for HTTP sources, refreshed registry credentials, and fixes for races in build progress, cache access, provenance and exporters |

BuildKit v0.35.0 platform digests are `sha256:a521fc1fd903a46c611c733dd692548512232fba2e0386d6055846a903d05a71` (linux/amd64) and `sha256:7dc61a9fab7c7f5a50b1cdbbefa4180829e68c6c0172206eb54ad934de097ddf` (linux/arm64), and `latest` resolves to the same index. The [release notes](https://github.com/moby/buildkit/releases/tag/v0.35.0) list no removed option and no change to the attestation formats. The release `Dockerfile` names its frontend by the pinned `docker/dockerfile:1` digest, so the built-in frontend update does not change how it is parsed. Dockerfile frontend 1.29.0 was released at the same time, but `docker/dockerfile:1` still resolved to the pinned digest in the checks below.

The same full check reports development-only updates, which are not adopted in this change:

| Input | Current | Latest | Where |
| --- | --- | --- | --- |
| google.golang.org/api | v0.312.0 | v0.313.0 | Three of the four checked Go modules |
| google-cloud-auth | 1.18.0 | 1.19.0 | `sdk-tests/rust`, `sdk-scorecard/rust` |
| google-cloud-storage (Rust) | 1.21.0 | 1.22.0 | `sdk-tests/rust`, `sdk-scorecard/rust` |
| google-cloud-wkt | 1.10.0 | 1.11.0 | `sdk-scorecard/rust` |
| node:24-trixie-slim | `sha256:0820eaa8…` | `sha256:462ec7f2…` on linux/amd64 | Moving tag; the pinned `node:24.22.2-trixie-slim` stays `CURRENT` |

Under the [release-scope rule](../dependency-policy.md#ci-lanes) they do not block publishing artifacts that do not contain them. They stay visible in the daily check until they are adopted or recorded as exceptions; no exception is recorded.

### Validation performed on 2026-11-12

- `cargo test -p xtask --locked` passed every test binary (221, 11, 3 and 1 tests), including the check of the actual pins against the snapshot.
- `cargo xtask dependencies check --scope release` at `2026-11-12T00:28:24Z` exited 0 with 41 `CURRENT` lines, among them `docker moby/buildkit:v0.35.0: CURRENT` and `docker moby/buildkit:latest: CURRENT`, both at `sha256:aa632b921ef07411d2cb77292cd34213d4f7b384001d6fceef1e1f25bf9752bb`.
- `cargo xtask dependencies check` at `2026-11-12T00:26:46Z` exited 1 with 120 `CURRENT` lines; every `UPDATE_AVAILABLE` line names one of the development-only inputs above, and fake-gcs-server v1.56.1 stayed `PINNED_REFERENCE`.
- On macOS (Apple Silicon), a `docker-container` Buildx builder started from the adopted pinned image reported `BuildKit version: v0.35.0`. With it, the release `Dockerfile` at the v0.19.0 source built for linux/arm64 with `--load`, and `bash scripts/smoke-containers.sh --image <image> linux/arm64` printed seven `PASS` lines, the strict local recipe and the full application example included. The same build exported as OCI with `--provenance=mode=max` and the pinned SBOM generator passed the checks `scripts/container-release.sh` applies to a pushed platform index: one linux/arm64 manifest plus its attestation manifest, an `SPDX-2.3` SBOM, and SLSA v1 provenance whose `buildType` is the BuildKit attestation definitions document. linux/amd64 was not built with v0.35.0 locally; the GHCR workflow builds it natively.

### Development-only updates adopted

The development-only updates listed above are adopted later the same day. While they were being verified, RubyGems published Ruby `google-cloud-storage` 1.62.2 at `2026-11-12T01:44:25Z`, and the next full check reported it in both Ruby manifests, so it is adopted too.

| Input | Previous | Adopted | Where |
| --- | --- | --- | --- |
| google.golang.org/api | v0.312.0 | v0.313.0 (published 2026-11-11, revision `bfc6507921ffcc3a1b3971105b621e441ecf9fa0`, Go 1.26.0, 52,749,023-byte module zip) | `go`, `sdk-scorecard/go`, `sdk-tests/go` |
| google-cloud-storage (Rust) | 1.21.0 | 1.22.0 (538,861 bytes, Rust 1.91.0) | `sdk-tests/rust`, `sdk-scorecard/rust`, `examples/rust` |
| google-cloud-auth | 1.18.0 | 1.19.0 (193,915 bytes) | the same three Rust manifests |
| google-cloud-wkt | 1.10.0 | 1.11.0 (113,973 bytes) | `sdk-scorecard/rust`; resolved transitively in the other two |
| node:24-trixie-slim selector | `node:24.22.2-trixie-slim` | `node:24.22.3-trixie-slim` `sha256:af02d57b57719b9657a45199a79264a9437940cd55b3c7b3fc99615654f124a0` | `scripts/strict-recipe-browser.sh` |
| google-cloud-storage (Ruby) | 1.62.1 | 1.62.2 (`sha256:4605fe7946575a0df5ce9809324b966a49c153cf305ce6e592e432219ff0bfa2`) | `sdk-tests/ruby`, `sdk-tests/ruby/active_storage`, `sdk-scorecard/ruby`, `examples/ruby` |

API v0.313.0 lists regenerated Discovery clients only, its BSD-3-Clause LICENSE is byte-identical to v0.312.0, and `go mod tidy` changes no other requirement. The Rust crates come from the 2026-11-11 google-cloud-rust release: Storage accepts `Into<Bytes>` in appendable writes, adds parallel-upload request types, rejects an excess persisted size and fails bidi reads on an unexpected server OK; auth and wkt carry fixes. Cargo also moves google-cloud-gax-internal to 0.7.22 and google-cloud-lro to 1.12.0 in each lockfile. Storage 1.22.0 still has no public API for bucket notifications, HMAC keys, the project service account or V2 signed URLs, and its client builder options match 1.21.0, so it still builds its own reqwest client; the scorecard reasons keep their wording with the new version. Node 24.22.3 is the 2026-11-11 security release (HTTP/2 and TLS fixes); its image names the same `20261104T212339Z` Debian snapshot, so the pinned Chromium install is unchanged. Ruby 1.62.2 differs from 1.62.1 only in `version.rb`, the changelog and one comment in `bucket.rb`, with the same gemspec dependencies; a conservative `bundle lock --update google-cloud-storage --conservative` changes no other gem in the four lockfiles.

#### Validation performed for the development-only updates

- `just check-sdk` exited 0 with the new Go and Rust inputs: the Go suite, `official-rust-http` and `arrow-object-store` passed, as did the Ruby, Node.js, Python, PHP, Java and .NET suites.
- After the Ruby update, `bash scripts/sdk-go.sh --with-ruby`, run through `scripts/with-test-tmpdir.sh`, exited 0 and printed `Ruby 4.0.9; google-cloud-storage 1.62.2`.
- `just check-active-storage-strict` exited 0 with `10 PASS, 0 FAIL`. `just check-active-storage` exited 1 with the documented default-authentication result, `1 PASS, 6 PASS_WITH_WORKAROUND, 1 FAIL`, and its storage.yml recipe printed `11 PASS, 0 FAIL` and a passing production case.
- `GCSM_IMAGE=ghcr.io/shiroemons/gcs-mimic:0.19.0 bash scripts/strict-recipe-browser.sh` printed `PASS: strict local recipe browser check completed` with Node 24.22.3.
- `cargo clippy --locked --all-targets -- -D warnings` passed in `sdk-scorecard/rust` and `examples/rust`; `go vet ./...` and `go build ./...` passed in `sdk-scorecard/go` and `examples/go`, and `go test ./...` passed in `sdk-scorecard/go`. The scorecard itself was not run.
- `cargo xtask dependencies check` at `2026-11-12T02:18:23Z` exited 0 with 129 `CURRENT` lines; fake-gcs-server v1.56.1 stayed `PINNED_REFERENCE`, and Composer `latest` differed only on linux/riscv64. The release-scoped check exited 0 with 41 `CURRENT` lines.

## Rust 1.100.0

Rust 1.100.0 was released on 2026-11-12 ([announcement](https://blog.rust-lang.org/2026/11/12/Rust-1.100.0/)). The official stable channel manifest, dated 2026-11-12, names `1.100.0 (0ef1bc386 2026-11-09)`. No new edition was released, so the crates stay on edition 2024.

| Input | Previous | Adopted |
| --- | --- | --- |
| Toolchain (`rust-toolchain.toml`) | 1.99.1 | 1.100.0, with rustfmt and clippy |
| `rust-version` of the application, `sdk-tests/rust`, `sdk-scorecard/rust` and `examples/rust` | 1.99.1 | 1.100.0 |
| Build image `rust:<version>-slim-trixie` (`Dockerfile`, `sdk-scorecard/run.sh`, `sdk-scorecard/test-tls.sh`) | 1.99.1, index `sha256:f6c4a35d21dbb3c89d7a8d056c193e94fc09037f2b58dd858b09c39a47dd02d7` | 1.100.0, index `sha256:e9d95b74296f7126a83ae3d041aeaba1ff4c478f13abc5cc034fcf7c131c3d4a`; linux/amd64 `sha256:4b68057dc8c103af4fa66b5cd29f21b18afec79fadfcd8fb91e295e939f0690c`, linux/arm64 `sha256:11acfd0f07018a7c60d33b5e95618275feb39e465c4fbc9a73b438b1ee1e43ce` |
| musl guard in `xtask/src/release.rs` | `rustc 1.99.1` | `rustc 1.100.0` |

### Inputs reviewed

- Both platform images of the new tag are also those of `slim-trixie`, report `rustc 1.100.0 (0ef1bc386 2026-11-09)` and `cargo 1.100.0 (3acc79c0c 2026-11-05)`, and record the Debian snapshot `20261104T212339Z`, so the musl and socat pins stay the same.
- `src/ci/docker/scripts/musl.sh` sets `MUSL=musl-1.2.5` at both the `1.99.1` and the `1.100.0` tags of rust-lang/rust. The bundled musl source archive and notices therefore stay the same, and only the toolchain guard advances.
- Clippy 1.100.0 found four clones in test code whose originals were not used again (`redundant_clone`, which the project already denies); they are removed. Production code needed no change, and the production lint gate reports no new or ratcheted finding.
- `sdk-tests/rust/run.sh` and `examples/rust/run.sh` built with whatever `RUSTUP_TOOLCHAIN` the caller set, unlike the other builds, which drop it so that `rust-toolchain.toml` decides. A global override of 1.99.1 made the Rust SDK suite fail against the new `rust-version`; both runners now drop the variable.

### Validation performed

| Command or check | Result |
| --- | --- |
| `cargo clippy --locked --workspace --all-targets` | PASS after the clone removals |
| `cargo xtask lint` | PASS: 0 new production violations, 0 ratcheted findings |
| `just pre-push`, Rust and shell checks | PASS in 11m58s: 3423 Rust tests in 16 binaries, 0 failed, 1 ignored; formatting, Clippy and the production lint gate on the host and on `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu` and `x86_64-unknown-linux-musl`; documentation and script checks |
| `just check-sdk` | PASS: Go, Ruby, Node.js, Python, PHP, Java, .NET (net10.0 and net11.0) and Rust; the Rust suite built with 1.100.0 and `official-rust-http` and `arrow-object-store` passed. Skipped by their own conditions: Go V2 RSA signing and two Node.js cases need HTTPS, and one Node.js case runs in the strict profile only |
| `just check-containers` | PASS: `linux/amd64` and `linux/arm64` images built from the new build image, and each passed all seven smoke checks |
| `cargo clippy --locked --all-targets -- -D warnings` in `sdk-tests/rust`, `sdk-scorecard/rust` and `examples/rust` | PASS |
| `cargo test --locked -p xtask` | PASS: 236 tests |
| `cargo xtask dependencies check` at `2026-11-13T00:24:39Z` | Exit 1 with 121 `CURRENT` lines. `rust` and both Rust image tags are `CURRENT`; `slim-trixie` differs from the pinned index only on linux/riscv64 |
| `cargo xtask dependencies check --scope release` at `2026-11-13T00:25:58Z` | Exit 1 with 39 `CURRENT` lines |

Both checks still report updates that this refresh does not adopt: the release inputs prost and prost-types 0.14.5 → 0.14.6, and the development-only google.golang.org/api v0.313.0 → v0.314.0 in three Go modules and @google-cloud/storage 8.4.0 → 9.0.0, a major release, in three npm manifests. No exception is recorded.

`just pre-push` first stopped in the Rust SDK suite, because the global toolchain override described above selected 1.99.1; after the runner fix, `just check-sdk` and `just check-containers` were rerun separately rather than as one `just pre-push`. The `examples/rust` program was not run against a server, and the release builds of the eight native archives run only on a release tag.

## prost 0.14.6

The release-scoped check after the Rust update still reported prost and prost-types 0.14.6, released on 2026-11-12, and a tag publication runs that check. [v0.14.6](https://github.com/tokio-rs/prost/releases/tag/v0.14.6) contains one change: decoding rejects a map field with a wire type other than length-delimited. gcs-mimic decodes Storage v2 and v1 requests with prost, so the fix applies to client input.

| Input | Previous | Adopted |
| --- | --- | --- |
| prost | 0.14.5 | 0.14.6 (`3ef0b569d1d907ab3a28596aee62cf1f61dacb0529a4bedcd8e7460e2d5228b4`, 34,593 bytes) |
| prost-types | 0.14.5 | 0.14.6 (`f29a7c4c7a84cef41a53d338a2d10d4f1b043ea5c3dde26db22ee2c9c1b1c75b`, 50,256 bytes) |
| prost-derive (transitive) | 0.14.5 | 0.14.6 (`1ad9c5423459fa2f729b8d9235fab810aa29628b171467744fc86d21460ad70d`) |

All three keep the Apache-2.0 license and declare Rust 1.85. `cargo update` changed no other package.

| Command or check | Result |
| --- | --- |
| `cargo clippy --locked --workspace --all-targets` | PASS |
| `bash scripts/test-grpc-codegen.sh` | PASS: the generated code still matches, and changed protos and stale output are rejected |
| `cargo test --locked --lib grpc` | PASS: 171 tests |
| `cargo xtask dependencies check --scope release` at `2026-11-13T01:10:47Z` | Exit 0 with 41 `CURRENT` lines |
| `cargo xtask dependencies check` at `2026-11-13T01:11:14Z` | Exit 1 with 123 `CURRENT` lines; only google.golang.org/api v0.314.0 in three Go modules and @google-cloud/storage 9.0.0 in three npm manifests remain, both development-only |

The complete gates for the release source are recorded with the 0.20.0 release.

## Node.js Storage 9.0.0

The full check after the 0.20.0 release still reported two development-only updates: google.golang.org/api v0.313.0 → v0.314.0 in three Go modules and `@google-cloud/storage` 8.4.0 → 9.0.0, a major release, in three npm manifests. Both are adopted. The Node.js SDK suite now runs every stage with 9.0.0 and with 8.4.0, so gcs-mimic keeps both major lines working.

| Input | Previous | Adopted | Where |
| --- | --- | --- | --- |
| google.golang.org/api | v0.313.0 | v0.314.0 (published 2026-11-12, revision `f66baba1fd76977b8c782563f8ee3e8d1a86fbb1`, Go 1.26.0, 52,755,419-byte module zip) | `go`, `sdk-scorecard/go`, `sdk-tests/go` |
| `@google-cloud/storage` | 8.4.0 | 9.0.0 (published 2026-11-12, `sha512-fjUzqzbxgkItIjEFjs/j/NrjQ9RZ3yWhMqmOdJB4AjdMu03vvpa88RvJEkZ5cjvSZAF+cBNCovw3fmERarOHFA==`, 1,633,490 bytes unpacked, Node.js >=22) | `sdk-tests/node`, `sdk-scorecard/node`, `sdk-tests/testcontainers/node` |
| `@google-cloud/storage` 8.x line | — | 8.4.0, in the new `sdk-tests/node/storage-8` manifest and lockfile | `GCSM_NODE_STORAGE=8` runs of the Node.js suite |
| `google-auth-library` (direct edge) | transitive | 11.3.4, already resolved | `sdk-scorecard/node`, `sdk-tests/testcontainers/node`, `examples/node` |

API v0.314.0 lists regenerated Discovery clients only, its BSD-3-Clause LICENSE is byte-identical to v0.313.0, and `go mod tidy` changes no other requirement.

[Storage 9.0.0](https://github.com/googleapis/google-cloud-node/releases/tag/storage-v9.0.0) replaces teeny-request with gaxios 8, returns WHATWG `Headers`, replaces `ApiError` with `GaxiosError` and moves low-level request options to gaxios names. Its lockfiles drop teeny-request, retry-request, `@google-cloud/projectify`, html-entities, abort-controller, event-target-shim, http-proxy-agent, stream-events, stubs and two nested gaxios 7 copies, and add no package. `examples/node` stays on 8.4.0 while it targets the published 0.20.0 image, which answers 400 to the parameters described below.

### Behavior changes found

| 9.0.0 behavior | Effect on gcs-mimic | Resolution |
| --- | --- | --- |
| Every JSON request goes through the auth client, even for a custom `apiEndpoint` without `useAuthWithCustomEndpoint` | An anonymous `Storage` fails with `Could not load the default credentials`; with `credentials`, the SDK first exchanges the key at `oauth2.googleapis.com`, which the scorecard's socket guard stopped | Anonymous clients pass `authClient: new PassThroughClient()` from google-auth-library, which also works on 8.x. The scorecard signs with a separate client that sends no request. The user guides show the same setup |
| `File#delete` sends `responseType=text` and the source of its `validateStatus` function as query parameters | objects.delete answered 400 `Unknown query parameter` | Recorded against Cloud Storage: case `obj-delete-undeclared-query` deleted the object with them and with `projection=noAcl`. objects.delete now ignores undeclared parameters in both profiles; fake-gcs-server v1.56.1 reads only `generation` there |
| `Bucket#getFiles` sends `autoPaginate` | objects.list answered 400 | Recorded: case `obj-list-undeclared-query` listed the objects with `autoPaginate=false` and `unknown=value`. objects.list now ignores undeclared parameters in both profiles |
| A `userProject` given to `storage.bucket()` is not sent on `getFiles` | The fail-fast case got 200 instead of the expected 400 | The case passes `userProject` per call |
| `getFiles` returns the HTTP response as its third value, with the listing under `data` | The scorecard read `prefixes` from the wrong object | The probe reads `data` when present |

Recognized but unsupported standard parameters such as `userProject` are still rejected on both methods, and the other object methods keep the 400 for an undeclared name (SPEC-JSON-UNKNOWN-QUERY-OBJECTS stays open for them).

### Cloud Storage runs

Both cases were recorded with `cargo xtask differential run --filter <case> --record` against a disposable `us-central1` STANDARD bucket with soft delete off, which the harness deleted after the case; 15 and 12 requests reached the live service, with no 429 retry, and `cargo xtask differential sweep` found no leftover bucket. The recordings were merged into `compatibility/differential/reference/gcs.json`.

A probe also ran the SDK itself against Cloud Storage with both lines, in its own disposable bucket, which it deleted:

| Step | 9.0.0 | 8.4.0 |
| --- | --- | --- |
| `file.delete()` | Succeeded; query `responseType=text&validateStatus=...`; object gone | Succeeded; no query |
| `storage.bucket(name, {userProject}).getFiles()` | Succeeded; no `userProject` sent | Succeeded; `userProject` sent |
| `bucket.getFiles({userProject})` | Succeeded; `userProject` sent | Succeeded; `userProject` sent |

### Validation performed on 2026-11-13

| Command or check | Result |
| --- | --- |
| `cargo xtask differential run --reference compatibility/differential/reference/gcs.json` | All 75 cases match, including the two new ones |
| `bash scripts/with-test-tmpdir.sh bash scripts/sdk-go.sh --with-node` | Go suite PASS with API v0.314.0; Node.js self-check 7, migration 2, permissive 22 (2 skipped) and strict 26 (1 skipped) PASS on 9.0.0 and on 8.4.0 |
| Node.js scorecard probe against a local permissive server | 51 of 51 PASS with 9.0.0 |
| `examples/node/run.sh` against a local server | PASS with 8.4.0 and `PassThroughClient` |
| `cargo test --locked --lib json::`; `cargo test --locked --test it_protocol wire_parity` | 139 and 17 PASS |
| `cargo test --locked -p xtask` | PASS, including the major-line selection |
| `go test -race -count=1 ./fakestorage` in `go`; `go vet`, `go build` and `go test` in `sdk-scorecard/go`; `go vet` in `sdk-tests/go` | PASS |
| `cargo xtask dependencies check` at `2026-11-13T03:18:27Z` | Exit 0 with 132 `CURRENT` lines; `npm:@google-cloud/storage (sdk-tests/node/storage-8/package.json, major 8): CURRENT current=8.4.0 latest=8.4.0` |
| `cargo xtask dependencies check --scope release` at `2026-11-13T03:19:15Z` | Exit 0 with 41 `CURRENT` lines |
| `just pre-push` on the final source | PASS in 16m14s: 16 Rust test binaries and doctests, Clippy and the production lint gate on the host and the three cross targets, documentation checks; all eight SDK suites, the Node.js suite on 9.0.0 and 8.4.0; both container architectures with seven smoke checks each |
| `GCSM_SDK_COMPAT_PROFILE=fake-gcs-server bash scripts/with-test-tmpdir.sh bash scripts/sdk-go.sh --with-node` | PASS: the Go suite and the same Node.js counts on both lines |
| `node scenario.mjs` in `sdk-tests/testcontainers/node` with the arm64 image `just pre-push` built | PASS with 9.0.0: start, health, mapped Location, resumable upload, bytes and cleanup |

Three integration tests asserted the old 400 for an undeclared name on objects.delete or objects.list. They now expect the recorded success for those methods and keep the 400 for object GET and PATCH; the unsupported-request case that deleted with `projection=noAcl` now deletes with `userProject`, which is still rejected with `badRequest` and leaves the object unchanged.
