# SDK scorecard: official C++ SDK probe

<!-- markdownlint-configure-file {"MD013": false} -->

Status: implemented on 2026-11-15. The probe passes all 51 shared cases
against gcs-mimic. Its first run exposed two emulator defects, fixed in
separate commits and described under "Verification". AUDIT-003 is PARTIAL.

Scope: AUDIT-003 (Official C++ SDK integration), first slice. The official
`google-cloud-cpp` Cloud Storage client runs the 51 shared scorecard cases
(bucket 20, object 27, signed 4) over its JSON/REST transport against every
compared emulator. This slice changes the optional comparison harness and its
dependency detection, not the emulator. No server behavior, configuration or
catalog schema changes; the two emulator fixes its first run called for are
their own commits.

Out of scope, each a later slice: the SDK's gRPC transport (`AsyncClient`,
`MakeGrpcClient`), C++-specific `sdk` cases, new shared cases, and a case
catalog. AUDIT-003 therefore moves to PARTIAL, not IMPLEMENTED.

## Pinned inputs

Official sources checked on 2026-11-15. `cargo xtask dependencies check` on
`23994c92` exited 0 with every existing input CURRENT before any change.

| Input | Version | Source and digest |
| --- | --- | --- |
| google-cloud-cpp | v3.13.0 (released 2026-11-03, latest release) | Generated tag archive `https://github.com/googleapis/google-cloud-cpp/archive/refs/tags/v3.13.0.tar.gz`, SHA-256 `3fefa2ca821ec8dd26f2d5ffc89611e74259c76a459fce3fd3cf46013785e423`. The release has no assets. |
| opentelemetry-cpp | v1.30.0 (latest release), API headers only | Generated tag archive `https://github.com/open-telemetry/opentelemetry-cpp/archive/refs/tags/v1.30.0.tar.gz`, SHA-256 `85aae7a42d7b71d17e510ddebc85db622d9078cc6d29cf9fb7373014b063859f`. |
| Abseil | 20260817.0 (latest release; SDK minimum 20250814 patch 1) | Release asset `abseil-cpp-20260817.0.tar.gz`, SHA-256 `f7e05179df39c45434cad433f5783840bb3788ef322976f9138bc6b72b3a107d` (matches the release digest). |
| nlohmann/json | v3.12.0 (latest release; SDK minimum 3.12.0) | Release asset `json.tar.xz`, SHA-256 `42f6e95cad6ec532fd372391373363b62a14af6d771056dbfc86160e6dfff7aa` (computed locally; the release publishes no digest). |
| CMake | 4.5.0 (latest release) | Release assets `cmake-4.5.0-linux-aarch64.tar.gz` `0c8447e7bba9701467b5ac6155d157aa9e309e71af4981f78bd4b9176107f553` and `cmake-4.5.0-linux-x86_64.tar.gz` `81f0d5b0960fb6adc8197030ffc827fba937640d51e73e39e0e9e40d88c66f6f` (both match the release digests). |
| Build and run image | `gcc:16.2.0-trixie` | Index `sha256:5c2f83335c97e233c80d2bc754bd5c46b2d9c32f236a765a4ac40a59f216c064`; linux/amd64 `sha256:531d94a9eb84cb69df20050ddde80d308bf8650c83f4cb0accf4ef39bb543ed4`; linux/arm64 `sha256:7f965c5da19c3e91a5a534f85919ea9ed2af4f0383445f75973ec3980efea563`. `16-trixie` is the moving tag. |
| libcurl, OpenSSL | 8.14.1-2+deb13u7, 3.5.7-1~deb13u3 | Shipped in the image (`libcurl4-openssl-dev`, `libssl-dev`); SDK minimums are 8.7.1 and 3.0.17. Pinned by the image digest. |
| socat | 1.8.0.3-1+deb13u1 | Existing `sdk-scorecard/install-socat.sh`; the image names the same Debian snapshot `20261104T212339Z` as the Rust image. |

The SDK and its dependencies are Apache-2.0 (google-cloud-cpp, Abseil,
opentelemetry-cpp) and MIT (nlohmann/json); CMake is BSD-3-Clause and only a
build tool. Nothing here ships in a release artifact.

The image is the official `gcc` image because it already carries libcurl and
OpenSSL development packages, so no apt package has to be pinned besides socat.
vcpkg, Bazel, a native macOS build and Debian's Abseil package were rejected:
they add an ecosystem to track, build far more from source, cannot be pinned,
or are older than the SDK minimum.

## Build

`sdk-scorecard/cpp/Dockerfile`, with its own build context under
`sdk-scorecard/` because the repository root `.dockerignore` denies everything
outside the release image's inputs.

- Sources enter through `ADD --checksum=sha256:...`; a changed archive fails
  the build instead of being used.
- CMake's binary archive is selected by `TARGETARCH`.
- Abseil, nlohmann/json and the SDK are static, C++17, installed to
  `/usr/local`. GCC 16 defaults to C++20; the SDK's documented standard is
  stated explicitly.
- The SDK is configured with
  `-DGOOGLE_CLOUD_CPP_ENABLE=storage,-monitoring,-trace,-opentelemetry,-universe_domain`.
  `storage` alone implicitly enables those four features, which require gRPC
  and Protobuf (`cmake/GoogleCloudCppFeatures.cmake`).
- `google_cloud_cpp_common` 3.13.0 includes OpenTelemetry API headers
  unconditionally, so opentelemetry-cpp is installed with
  `-DOTELCPP_WITH_API_ONLY=ON` (headers only, no SDK or exporter).
- socat is installed by calling `/app/sdk-scorecard/install-socat.sh`; the
  Dockerfile contains no package installation of its own.
- `run.sh` and `test-tls.sh` build the image once and run it per emulator.

## Transport contract

- The probe refuses to start unless `SCORECARD_ENDPOINT` is an origin on
  `127.0.0.1`, `localhost` or `::1`, as the other probes do.
- It unsets `CLOUD_STORAGE_EMULATOR_ENDPOINT` and
  `CLOUD_STORAGE_TESTBENCH_ENDPOINT` before creating a client. When either is
  set the SDK replaces an explicit `RestEndpointOption`, for requests and for
  signed URLs (`google/cloud/storage/client.cc:568-571,663-667`). It also
  unsets proxy variables, `GOOGLE_APPLICATION_CREDENTIALS` and
  `GOOGLE_CLOUD_UNIVERSE_DOMAIN`.
- The SDK client uses `RestEndpointOption(origin)`,
  `IamEndpointOption(origin + "/iamapi")`, `CARootsFilePathOption` from
  `SCORECARD_CA_CERT`, `MakeInsecureCredentials()` and
  `LimitedErrorCountRetryPolicy(0)`, so one failed attempt is one failure.
- Signing uses a second client holding service account credentials built in
  memory from `SCORECARD_SIGNING_KEY` and the built-in test signer
  `test-signer@gcs-mimic.invalid`. The SDK signs locally first and only then
  falls back to the IAM `signBlob` API (`client.cc:372-397`); that endpoint
  also points at the local origin, so a fallback cannot reach Google. No
  emulator serves `/iamapi`; a fallback would fail the case.
- Signed URLs are sent unchanged. V4 and POST policy take scheme `https` and
  the endpoint authority including the port; V2 takes the endpoint as given.
  V4 signs only the `host` header.
- Signed requests go through the probe's own libcurl handle: exact-origin
  check, no redirects, no proxy, HTTPS only, the scorecard CA.
- Failures are classified by `error_info().metadata()["http_status_code"]`.
  The SDK maps 304, 308 and 412 to one `StatusCode`, so the code alone is not
  used. Status message text is not parsed, except for the checksum case, which
  must name a checksum as in every other probe.
- The container reaches the host through `host.docker.internal` with socat
  forwarding the loopback port, as the Rust probe does, so URLs, Host headers
  and signatures keep the loopback origin.
- `--transport-check` exits 0 only when the SDK client reaches the origin
  through the fixture CA and the signed-request client refuses an external
  origin; a wrong CA or SAN must fail with the SDK's certificate error.

Signed cases run in the permissive profile and establish routing, parameters
and bytes, not emulator-side signature verification.

## Cases

The 51 IDs, their order and their observable assertions are those of
`sdk-scorecard/python/probe.py`. Expected `unsupported` count: 0. Every case
has a public API in v3.13.0; a case is marked unsupported only if writing it
shows otherwise, with the reason recorded in `sdk-scorecard/cpp/README.md`.

SDK-specific choices that are part of the probe contract:

| Case | Choice |
| --- | --- |
| `object.write_read_small` and other small writes | `InsertObject`, a single request. `WriteObject` is resumable by default. |
| `object.resumable_large` | `WriteObject`, 3 MiB + 123 bytes; size and bytes compared. |
| `object.checksum_rejected` | `InsertObject` with a wrong `Crc32cChecksumValue` and `DisableMD5Hash(true)`; requires a checksum-specific 400 and no object. |
| `object.gzip_transcoding` | Default `ReadObject` for the decompressed bytes, `AcceptEncodingGzip()` for the stored bytes. The SDK skips hash validation only when the response carries `x-guploader-response-body-transformations: gunzipped`; an emulator that omits it fails the case. |
| `object.copy_with_metadata`, `object.rewrite_storage_class` | `RewriteObjectBlocking` with destination metadata. |
| `bucket.update_labels` and other setting cases | `PatchBucket`, matching Go and Python. |
| `signed.*` | The signing client above; requests sent by the guarded libcurl handle. |

## Acceptance

| Case | Required behavior |
| --- | --- |
| CPP-PINS | The Dockerfile builds on linux/arm64 and linux/amd64 from the inputs above; a changed checksum fails the build. |
| CPP-TRANSPORT | `test-tls.sh` passes its four certificate combinations for `cpp`: 32 checks in total instead of 28. |
| CPP-ISOLATION | With `CLOUD_STORAGE_EMULATOR_ENDPOINT` set to another host in the container environment, requests and signed URLs still use `SCORECARD_ENDPOINT`. |
| CPP-CORE | `gcs-mimic.cpp.jsonl` holds the 51 shared IDs with the same areas as the Ruby core; `score.sh` accepts the nine-SDK run for all four products. |
| CPP-SCORER | `score.sh` labels `cpp` as C++; `test-score.sh` covers nine SDKs; rescoring `results/2026-10-29-5` still produces a byte-identical summary. |
| CPP-FRESHNESS | `cargo xtask dependencies check` reads the C++ pins, fails when the Dockerfile and the snapshot disagree, and reports a newer google-cloud-cpp, Abseil, nlohmann/json, opentelemetry-cpp, CMake or gcc image. |
| CPP-PENDING | gRPC transport, `sdk` cases, strict authentication, native TLS listeners and other profiles remain pending. |

## Harness and detection changes

- `sdk-scorecard/cpp/`: Dockerfile, CMake project, probe sources and a README
  with pins, build, transport notes and any unsupported cases.
- `run.sh`: build step, `cpp` in the SDK loop and metadata (`version`,
  `runtime`, `image`, `cases: 51`).
- `score.sh`: `cpp` with label `C++`. Nine SDKs give 100/9 points each; the
  printed weight is rounded without changing the output for historical runs.
- `test-score.sh` and `test-tls.sh`: nine SDKs.
- xtask: a source-archive ecosystem for the Dockerfile pins (offline
  comparison of URL and SHA-256 with the snapshot, online comparison with the
  latest GitHub release), the Dockerfile added to the files read for Docker
  pins, the gcc image as a development-only Docker input for linux/amd64 and
  linux/arm64, and the Dockerfile added to socat's callers. Tests cover a
  mismatch and an available update.
- Documentation: `sdk-scorecard/README.md`, `docs/en/dependency-policy.md`,
  the eight-SDK statements in `README.md`, `docs/en/roadmap.md`,
  `docs/en/feature-comparison.md` and their Japanese counterparts.
  Existing slice and evidence documents are history and stay unchanged.

## Ledger

AUDIT-003 moves from NOT_STARTED to PARTIAL with actual fidelity PROJECT:
acceptance cases for the scorecard scope (PASS only after the run) and for the
pending scopes, test ID `scorecard-cpp`, a verification record whose artifact
is the new `gcs-mimic.cpp.jsonl`, SDK evidence in the shape AUDIT-004 uses, a
known deviation naming the REST-only scope, and the pinned release in
`source_urls`. Both feature matrices are regenerated with
`cargo xtask compatibility render`. No other row changes status.

## Results

Adding an SDK changes every product's weights, so all four products are rerun
into a new `sdk-scorecard/results/<UTC date>/` directory. Its scores are not
comparable with the eight-SDK runs. A failing C++ case stays recorded as
`pass: false`; no case is removed to keep a score.

## Established before implementation

Observed on 2026-11-15, linux/arm64, in a scratch build outside the
repository:

- The inputs above build with GCC 16.2.0 without source changes: about 39 s
  for the SDK and 20 s for Abseil on 14 cores. A single cold end-to-end time
  was not measured.
- With the network disabled, `CreateV4SignedUrl`, `CreateV2SignedUrl` and
  `GenerateSignedPostPolicyV4` sign locally and return
  `https://127.0.0.1:<port>/<bucket>/<object>` URLs.
- `CLOUD_STORAGE_EMULATOR_ENDPOINT` overrides an explicit endpoint for
  requests and signed URLs.
- Against a loopback server, 400, 403, 404, 412, 500 and 503 each surfaced
  with the matching `http_status_code` after exactly one attempt.
- `install-socat.sh` installs the pinned socat in the image from the snapshot.

## Verification

Verified on 2026-11-15, linux/arm64 (Docker 29.9.2). The first run was at
`fec7476f`, before the branch was rebased onto main (the hash is not on main);
the final run is at `6652e6fa`. Both had no uncommitted change.

| Case | Result | Evidence |
| --- | --- | --- |
| CPP-PINS | PASS on linux/arm64; linux/amd64 not built | `docker build -f sdk-scorecard/cpp/Dockerfile ... sdk-scorecard` succeeds without probe warnings. Both CMake archives are checksum-verified on every build; the linux/x86_64 one was never unpacked. |
| CPP-TRANSPORT | PASS | `bash sdk-scorecard/test-tls.sh`: 32/32, including `cpp` with the fixture CA (passes), an unrelated CA and a wrong SAN (both fail with libcurl's certificate error). |
| CPP-ISOLATION | PASS | With `CLOUD_STORAGE_EMULATOR_ENDPOINT`, `CLOUD_STORAGE_TESTBENCH_ENDPOINT`, proxy variables and `GOOGLE_CLOUD_UNIVERSE_DOMAIN` set to other hosts, every case result over HTTP and HTTPS was identical. A run without the scrub was not made. |
| CPP-CORE | PASS | `bash sdk-scorecard/run.sh` wrote [results/2026-11-15](../../../sdk-scorecard/results/2026-11-15/summary.md) in 830 s and, after the fixes, [results/2026-11-15-2](../../../sdk-scorecard/results/2026-11-15-2/summary.md) in 988 s, each for all four products, and `score.sh` accepted both. The 51 IDs and areas equal the Ruby core; no case is unsupported. gcs-mimic passed 49 in the first run (below) and 51 in the final one, so the ledger's all-pass case CPP-SCORECARD passes. In both runs the other eight SDKs' case results equal the 2026-11-12 run for every product. |
| CPP-SCORER | PASS | `bash sdk-scorecard/test-score.sh` passes with nine SDKs; every recorded summary from 2026-10-29 to 2026-11-12 rescores byte for byte. |
| CPP-FRESHNESS | PASS | `cargo xtask dependencies check` exits 0 with the seven new inputs CURRENT. A changed SHA-256 fails it (exit 2) and an older google-cloud-cpp tag reports UPDATE_AVAILABLE (exit 1), both shown on a scratch copy. `cargo test -p xtask`, `cargo clippy -p xtask --all-targets -- -D warnings` and `cargo xtask lint` pass. |
| CPP-PENDING | PENDING | As listed in the contract. |

Final scores with nine SDKs: gcs-mimic 100, floci-gcp 78.3, fake-gcs-server
40.6, gcp-storage-emulator 38.6. For C++: gcs-mimic 51/51, floci-gcp 43/51,
fake-gcs-server 24/51, gcp-storage-emulator 23/51. The first run differed only
in gcs-mimic's two C++ failures (score 99.6).

Those two failures were emulator defects. The probe kept the SDK defaults
rather than hide them:

- `object.soft_delete_restore`: `RestoreObject` sends
  `POST .../restore?generation=G` with the body `{}` and
  `content-type: application/json`. The bodyless-POST rule, written for Ruby's
  form-encoded repetition of the query, answers 400 `Use an empty body, or form
  parameters identical to the URL query`. No Cloud Storage recording shows that
  body being rejected. Fixed by `fix(json): accept the empty JSON object the
  C++ SDK sends to object restore`: restore alone accepts `{}` declared as
  `application/json`; retention lock and HMAC key creation, where no pinned
  client sends one, still refuse it. Recorded in JSON-OBJECTS-011.
- `signed.post_policy_v4`: the SDK writes the policy expiration with
  nanoseconds, for example `2026-11-15T05:58:19.766459053Z`, and the POST policy
  parser kept microseconds, answering `InvalidPolicyDocument`. Fixed by
  `fix(xml): accept POST policy expirations with nanoseconds`: up to nine
  digits are accepted and floored to the microsecond, which can only end a
  policy earlier. Recorded in DESIGN-041.

Both fixes passed `just check` (618 s) before the final run.

Deviations from the contract, kept deliberately:

- `object.checksum_rejected` uses `Crc32cChecksumValue` and `DisableMD5Hash`,
  which 3.13.0 deprecates; the replacement `PrecomputedChecksumsOption` reaches
  only `WriteObject`. The deprecation warning is silenced for that function.
- No public reader exposes object list pages, so `object.list_paging` counts
  pages from the SDK's own `rpc` log. The counted items must add up to the
  listing, so a change in the log text fails the case instead of passing it.
- `bucket.list_prefix_paging` uses `ListBucketsExtended`, which exposes pages.

The probe depends on these SDK behaviors, observed but not separately
specified: error responses carry `http_status_code` in `ErrorInfo` metadata,
and a gunzipped download carries
`x-guploader-response-body-transformations: gunzipped`.

Not run: a linux/amd64 build and strict-profile signature verification.
`just check` ran once, on the final source with both fixes. The first build's
time was not captured; with cached library layers a rebuild after a probe
change takes seconds.
