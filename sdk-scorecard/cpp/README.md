# C++ SDK scorecard probe

<!-- markdownlint-configure-file {"MD013": false} -->

Probes the official `google-cloud-cpp` Cloud Storage client 3.13.0 over its
JSON/REST transport against an emulator. It emits one JSONL line per case: the
51 shared bucket, object and signed cases, with the same IDs, areas and order as
the Python probe. It has no SDK-specific (`sdk` area) cases and does not use the
SDK's gRPC transport, so `SCORECARD_GRPC_ENDPOINT` is ignored. Contract:
[the C++ slice](../../docs/en/slices/sdk-scorecard-cpp.md).

## Pins

Every input is a checksum-verified archive or a digest-pinned image in the
[Dockerfile](Dockerfile): google-cloud-cpp v3.13.0, Abseil 20260817.0,
nlohmann/json v3.12.0, opentelemetry-cpp v1.30.0 (API headers only, which the
SDK includes unconditionally), CMake 4.5.0 and the `gcc:16.2.0-trixie` image
(libcurl 8.14.1, OpenSSL 3.5.7). The libraries are static and built as C++17,
the SDK's documented standard; GCC 16 would default to C++20. socat comes from
`sdk-scorecard/install-socat.sh`, as for the Rust probe.

## Build and run

The probe builds from the scorecard directory so the context holds only what the
image reads:

```sh
docker build -f sdk-scorecard/cpp/Dockerfile -t gcs-mimic-scorecard-cpp sdk-scorecard
```

The library layers are cached, so editing the probe rebuilds only the last
stage. The binary is `/usr/local/bin/gcs-mimic-scorecard-cpp`; the build uses
`-Wall -Wextra -Wpedantic` and produces no warnings. It takes
`SCORECARD_ENDPOINT` (a loopback origin, required), `SCORECARD_CA_CERT`,
`SCORECARD_SIGNING_KEY` (default `/app/fixtures/signing/test-only-private.pem`),
`SCORECARD_PROJECT` (default `local-project`) and `SCORECARD_RUN` (a substring
filter on case IDs, for debugging). Run it in a container with the repository
mounted at `/app`, like the Rust probe. A container's loopback is not the
host's, so socat forwards the loopback port to the host gateway and URLs, Host
headers and signatures keep the loopback origin:

```sh
docker run --rm --add-host host.docker.internal:host-gateway \
  -v "$PWD:/app:ro" -v "$ca:/scorecard-ca.pem:ro" \
  -e SCORECARD_ENDPOINT="$endpoint" -e SCORECARD_CA_CERT=/scorecard-ca.pem \
  -e SCORECARD_SIGNING_KEY=/app/fixtures/signing/test-only-private.pem \
  -e HTTP_PORT="${endpoint##*:}" gcs-mimic-scorecard-cpp bash -c '
    set -euo pipefail
    socat "TCP-LISTEN:$HTTP_PORT,bind=127.0.0.1,fork,reuseaddr" "TCP:host.docker.internal:$HTTP_PORT" &
    until (echo >/dev/tcp/127.0.0.1/"$HTTP_PORT") 2>/dev/null; do sleep 0.1; done
    gcs-mimic-scorecard-cpp'
```

Stdout carries only JSONL; diagnostics go to stderr. The probe exits 0 after
running every selected case, even if some fail, and non-zero only for a process
or configuration failure. Each case is bounded to 60 seconds: a case that
exceeds it prints its failing line and ends the process with status 2.
`--transport-check` verifies CA, hostname and origin handling without an
emulator and exits 0 or non-zero.

## Transport

- The probe refuses to start unless `SCORECARD_ENDPOINT` is an origin on
  `127.0.0.1`, `localhost` or `::1` with no path, query or userinfo.
- Before creating a client it unsets `CLOUD_STORAGE_EMULATOR_ENDPOINT` and
  `CLOUD_STORAGE_TESTBENCH_ENDPOINT`, which would otherwise replace an explicit
  `RestEndpointOption` for requests and for signed URLs, and the credential,
  universe-domain, tracing, metadata and proxy variables.
- The SDK client sets `RestEndpointOption`, `IamEndpointOption` (the origin
  plus `/iamapi`, never reached), `CARootsFilePathOption`,
  `MakeInsecureCredentials()`, `LimitedErrorCountRetryPolicy(0)` (one failed
  attempt is one failure) and stall and connect timeouts.
- Signing uses a second client holding service account credentials built in
  memory from the signing key as `test-signer@gcs-mimic.invalid`. The SDK signs
  locally and the client never sends a request. Signed URLs and POST policies go
  through the probe's own libcurl handle: exact scheme, host and port match with
  the origin before sending, no redirects, no proxy, only the origin's protocol,
  the scorecard CA. V4 URLs and POST policies default to `https` whatever the
  endpoint, so the probe passes the origin's scheme to the signer with
  `gcs::Scheme`; no URL is changed after signing.
- Failures are classified by `error_info().metadata()["http_status_code"]`,
  because the SDK maps 304, 308 and 412 to one `StatusCode`. The checksum case
  also requires a 400 whose message names a checksum or CRC32C.
- `--transport-check` passes only when an SDK call reaches HTTP through the
  fixture CA and the signed-request handle reaches the origin but refuses
  `https://storage.googleapis.com/` before connecting. Both run, and a wrong CA
  or hostname fails with libcurl's certificate text.

## SDK-specific choices

- Small writes use `InsertObject`, a single request; `WriteObject` is always
  resumable. `object.resumable_large` uses `WriteObject` with a 256 KiB upload
  buffer, because the 8 MiB default would send 3 MiB in one chunk.
- `object.checksum_rejected` uses `Crc32cChecksumValue` and `DisableMD5Hash`.
  They are deprecated in favor of `PrecomputedChecksumsOption`, but that option
  only reaches `WriteObject`; the deprecation warning is silenced for that one
  function.
- `object.md5_accepted` enables MD5 with `EnableMD5Hash()` and compares the
  stored hash with `ComputeMD5Hash`.
- `object.gzip_transcoding` reads once by default and once with
  `AcceptEncodingGzip()`. The SDK skips hash validation of a decompressed body
  only when the response carries `x-guploader-response-body-transformations:
  gunzipped`, so an emulator that omits it fails the case.
- `ReadRange` is right-open, so the closed range 2..5 is `ReadRange(2, 6)`.
- `bucket.list_prefix_paging` uses `ListBucketsExtended`, the only reader that
  exposes pages. No public reader exposes object pages, so
  `object.list_paging` runs `ListObjects` on a client with `rpc` logging and
  counts the items of each logged `ListObjects()` response from a log backend: it
  requires at least three pages of at most two objects whose counts add up to
  every listed object. This depends on the SDK's log text; a change to it fails
  the case instead of passing it.
- `object.copy_with_metadata` and `object.rewrite_storage_class` use
  `RewriteObjectBlocking` with destination metadata; setting cases use
  `PatchBucket`.

## Unsupported cases

None. Every shared case has a public API in 3.13.0.

## Known emulator incompatibilities

Against gcs-mimic two cases fail because of how the emulator treats what the
SDK sends, not because of the probe:

| Case | Observed |
| --- | --- |
| `object.soft_delete_restore` | `RestoreObject` sends `POST .../restore?generation=G` with a `{}` body and `content-type: application/json`. gcs-mimic answers 400 `Use an empty body, or form parameters identical to the URL query`. |
| `signed.post_policy_v4` | The SDK writes the policy expiration with nanoseconds (`2026-11-15T05:58:19.766459053Z`). gcs-mimic answers `InvalidPolicyDocument`: `The policy expiration must be an ISO 8601 or RFC 3339 UTC timestamp`. With the policy timestamp truncated to whole seconds the case passes. |
