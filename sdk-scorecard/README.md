# SDK scorecard

<!-- markdownlint-configure-file {"MD013": false} -->

The scorecard measures workflows using the official Go, Ruby, Node.js, PHP, Python, Java, .NET, Rust
and C++ Cloud Storage SDKs against local emulators. Within each SDK, it runs the
same probes, inputs and pass criteria against gcs-mimic, fake-gcs-server,
gcp-storage-emulator and floci-gcp. Shared case IDs across SDKs do not establish identical
inputs, assertions or endpoint adaptations. Cross-SDK equivalence is not yet
verified, so aggregate scores are provisional.

It is a compatibility comparison, not a performance benchmark, and not an
implementation-status source: `compatibility/features.json` remains the only
status ledger. A passing probe means an ordinary SDK user observes the expected
result for that workflow; it does not mean the underlying API method is
`IMPLEMENTED`.

## Scoring

A product that passes every scored case in all nine SDKs scores 100.

- Each SDK contributes 100/9 points, about 11.1 (100 divided by the number of
  SDKs in the run).
- Within an SDK, every scored case has equal weight: `(100/9) × passed / total`.
- Score = sum of the SDK contributions, rounded to one decimal.
- Historical runs keep their original weights when rescored: two SDKs 50
  points each, five SDKs 20 points each, eight SDKs 12.5 points each.

The `bucket`, `object` and `signed` areas are the shared core, with identical
IDs in every SDK. The `grpc` area (Go, Java, Python and Rust, each emitting
the gRPC cases its SDK has a public API for) and the `sdk` area (IDs starting
with `sdk.`, each emitted by the SDKs whose public API reaches the workflow)
hold each SDK's own transports and idioms, so
SDKs have different case counts. `summary.md` therefore reports two totals: `Score`
counts every case, `Core` counts only the shared core and is the figure to
compare across SDKs.

A case marked `unsupported` (`"unsupported": true` in the JSONL, with a
detail naming the reason) is an operation the SDK has no public API for. It is
shown as `n/a` and excluded from that SDK's denominator, so it neither lowers
nor raises any emulator's score. The marking comes from the probe, not the
emulator, and the scorer rejects runs where products disagree on it. At least
one scored case must remain per SDK.

## Case catalog

[`cases.json`](cases.json) defines every case: its area, the SDKs that emit
it, the observable `checks` every one of them must assert (a probe may check
more, never less), the SDKs that mark it unsupported with their exact reason,
and `variants`, where an SDK reaches the same checks by a different request.
`notes` records differences that cover a whole SDK, such as Rust's gRPC
metadata API. The operation a case tests uses the SDK's public API; a
non-public accessor may only read a result for an assertion when the SDK has
no public one, and its variant says so.

`run.sh` checks the catalog first, takes each SDK's case count from it and
copies it into the results directory. With that copy, `score.sh` also requires
each SDK's emitted IDs, areas and unsupported reasons to equal the catalog's.
Directories recorded before the catalog are scored as before. Adding or
changing a case starts in the catalog; `test-score.sh` fails when the Areas
table below does not count what the catalog defines. The contract is
[the catalog slice](../docs/en/slices/sdk-scorecard-catalog.md).

Equal weighting is a deliberate simplification: an ACL round trip counts as much
as resumable upload. Read the per-area table in each `summary.md` alongside the
total.

## Pass criteria

Each case verifies the observable effect, not just the absence of an error:

- settings are read back after being written (labels, lifecycle, CORS, UBLA,
  PAP, soft-delete policy, autoclass, ACL entries, IAM bindings);
- enforcement is exercised (retention and holds reject deletion with 403,
  failed preconditions return 412 and leave the object unchanged, a wrong
  CRC32C leaves no object, a customer-supplied-key object is unreadable
  without its key);
- bytes are compared after every upload, copy, rewrite, compose, move, range,
  signed and POST policy transfer;
- listing cases check exact names, prefixes and page sizes.

An emulator that accepts a setting but ignores it fails the case.
Emulator-specific features that the SDKs do not expose (strict local
authentication, seeding, sync directories, admin routes) are outside the
scorecard.

## Areas

| Area | Go | Ruby | Node.js | PHP | Python | Java | .NET | Rust | C++ | Scope |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| bucket | 21 | 21 | 21 | 21 | 21 | 21 | 21 | 21 | 21 | Bucket CRUD/listing, labels, preconditions, versioning, lifecycle, CORS, retention and lock, UBLA, PAP, IAM, soft-delete policy, bucket soft delete/restore, autoclass, ACL/default ACL, notifications, HMAC keys, service account |
| object | 29 | 29 | 29 | 29 | 29 | 29 | 29 | 29 | 29 | Uploads (single, resumable, metadata, checksums), range reads, PATCH, delete, generation/metageneration conditions, listing (delimiter, paging, offsets, glob), copy, rewrite, compose and its preconditions, move, ACL, holds, object retention, custom time, storage class, gzip transcoding, CSEK and key rotation, soft delete/restore |
| signed | 6 | 6 | 6 | 6 | 6 | 6 | 6 | 6 | 6 | V4 GET/PUT, V4 resumable start, V4 response overrides, V2 GET, V4 POST policy |
| grpc | 8 | – | – | – | 2 | 10 | – | 1 | – | Each SDK's public gRPC API: bucket CRUD, chunked write and ranged read, list, update/delete, multi-range bidi reads, appendable uploads, checksum and precondition failures, plus Java's write sessions |
| sdk | 3 | 2 | 3 | 4 | 5 | 14 | 17 | 2 | 6 | The SDK's own idioms: resumable resume and cancel, PUT replacement, rewrite token loops, JSON batch, sliced downloads, XML multipart and parallel composite uploads, file helpers and streams, blob handles, channels and write sessions, progress, paging and signed-URL templates |

Case IDs in `bucket`, `object` and `signed` are shared between the SDKs. The
probes live in [`go/main.go`](go/main.go), [`ruby/probe.rb`](ruby/probe.rb),
[`node/probe.mjs`](node/probe.mjs), [`php/probe.php`](php/probe.php),
[`python/probe.py`](python/probe.py), [`java/`](java/README.md),
[`dotnet/`](dotnet/README.md), [`rust/`](rust/README.md) and
[`cpp/`](cpp/README.md); the last four list their unsupported cases, if any,
and the grounds in their READMEs. The scorer
rejects missing files, incomplete/duplicate cases, invalid pass values,
different SDK inputs, case sets or unsupported markers across products, and
differing shared case IDs across SDKs. The SDKs with unsupported cases are
Go, Java and C++ (`bucket.soft_delete_restore`), Ruby (`object.list_offsets`),
.NET (`object.gzip_transcoding`) and Rust (`bucket.soft_delete_restore`,
`bucket.notifications`, `project.hmac_keys`, `project.service_account`,
`signed.v2_get`).

## Pinned inputs

| Input | Version |
| --- | --- |
| Go SDK | `cloud.google.com/go/storage` v1.70.0 (`go/go.mod`, same graph as `sdk-tests/go`) |
| Ruby SDK | `google-cloud-storage` 1.62.2 (`ruby/Gemfile.lock`, same graph as `sdk-tests/ruby`) |
| Node.js SDK | `@google-cloud/storage` 9.0.0 and `google-auth-library` 11.3.4 for its anonymous `PassThroughClient` (`node/package-lock.json`); requires Node.js 24+. The probe signs with a separate client, because 9.x would exchange a signing key at Google OAuth before every request |
| PHP SDK | `google/cloud-storage` 2.6.1 (`php/composer.lock`); official Composer 2.11.1 / PHP 8.5.12 image, digest pinned in `run.sh` |
| Python SDK | `google-cloud-storage` 3.17.1, `cryptography` 50.0.2 for offline RSA signing (`python/uv.lock`); Python 3.15.2 |
| Java SDK | `com.google.cloud:google-cloud-storage` 2.78.0 and `google-auth-library-oauth2-http` 1.58.0 for offline signing (`java/pom.xml`, graph in `java/dependencies.lock`); JDK and Maven pinned in `java/.java-version` and `java/.maven-version` |
| .NET SDK | `Google.Cloud.Storage.V1` 5.0.0 (`dotnet/Probe.csproj`, hashes in `dotnet/packages.lock.json`); exact .NET SDK in `dotnet/global.json` |
| Rust SDK | `google-cloud-storage` 1.22.0 with `google-cloud-auth` 1.19.0 (`rust/Cargo.toml`, `rust/Cargo.lock`); built and run in the official `rust:1.100.0-slim-trixie` image, digest pinned in `run.sh`; socat for its loopback forwarder is installed by `install-socat.sh` from a fixed snapshot.debian.org archive (version and snapshot recorded in the dependency snapshot) |
| C++ SDK | `google-cloud-cpp` 3.13.0, storage library over JSON/REST, with Abseil 20260817.0, nlohmann/json 3.12.0, the opentelemetry-cpp 1.30.0 API headers and CMake 4.5.0, each a checksum-pinned archive in `cpp/Dockerfile`; built and run in the official `gcc:16.2.0-trixie` image (digest pinned there), whose libcurl and OpenSSL it uses; socat from `install-socat.sh` as for Rust |
| gcs-mimic | Release build of the checked-out source; the commit is recorded in `*.meta.json` |
| fake-gcs-server | `fsouza/fake-gcs-server:1.56.1` |
| gcp-storage-emulator | `oittaa/gcp-storage-emulator:v2026.07.19` |
| floci-gcp | `floci/floci-gcp:0.12.0` |

Update the pins in `run.sh` and the module files deliberately; record the
change in the next results directory rather than rewriting old results.
The new SDK versions were checked against the official
[npm](https://www.npmjs.com/package/@google-cloud/storage),
[Packagist](https://packagist.org/packages/google/cloud-storage) and
[PyPI](https://pypi.org/project/google-cloud-storage/) registries on 2026-10-29.
They are maintained by Google and licensed Apache-2.0. `cryptography` is
maintained by PyCA under Apache-2.0 OR BSD-3-Clause. Exact artifact hashes,
source revisions and dependency versions are in the lockfiles; package sizes
and provenance are in `compatibility/dependency-status.json`.
`cargo xtask dependencies check` includes these three ecosystems, the Java,
.NET and Rust probe manifests and the Composer and Rust images in the existing
daily update detection.

## Emulator configuration

Every product receives the same HTTPS loopback origin through a Go
standard-library TLS frontend and a temporary development CA. Each official
SDK and signed-request client verifies that CA and the certificate hostname.
The HTTP backends bind only loopback and use a fresh in-memory or temporary
data store in their most capable documented configuration. The runner starts a
fresh emulator for every SDK probe (emulator x SDK), because probes consume
state such as resumable upload sessions and one SDK's leftovers must not
change another SDK's result:

- gcs-mimic: `serve --scheme http --grpc-listen ...` with the default
  permissive authentication profile.
- fake-gcs-server: `-scheme http -backend memory` (the memory backend is the
  only one that retains object versions); gRPC cases use the shared HTTP port.
- gcp-storage-emulator: `start --in-memory`; it has no gRPC listener, so the
  gRPC cases fail by timeout.
- floci-gcp: the default `memory` storage with `FLOCI_GCP_BASE_URL` set to the
  public origin and no Docker socket; gRPC cases use the shared port.

The frontend streams the original method, Host, escaped path, query, body and
integrity headers. Resumable Locations from the explicit backend origin are
normalized to the public HTTPS origin; external authorities are rejected.
Signed URLs use the public test key in `fixtures/signing` and are sent unchanged,
including V2 URLs generated with HTTPS. Clients are
anonymous and never contact Google Cloud or the metadata server.
The PHP container routes loopback connections through `host.docker.internal`
using cURL `CONNECT_TO`; request URLs, Host headers, advertised upload locations
and signatures stay unchanged. Every SDK transport is restricted to the selected
local origin. Signed HTTP requests cannot follow redirects. See the
[transport contract](../docs/en/slices/sdk-scorecard-tls.md).

SDK-specific public options are part of the probe contract:

- Node.js uses `apiEndpoint` and clears `STORAGE_EMULATOR_HOST`, which the SDK
  interprets as a JSON API base path instead of an origin. Its normal
  non-resumable uploads still send the SDK's idempotency token.
- Python uses scheme-inclusive `STORAGE_EMULATOR_HOST`, explicit anonymous
  credentials and a separate offline service-account signer. The 3 MiB upload
  uses `Blob.open("wb")` to force resumable transfer; the wrong-checksum probe
  uses an 8 MiB + 1 payload because the public upload method only sends the
  supplied CRC32C on the resumable path. Its copy-with-metadata workflow uses
  `Blob.rewrite`, which exposes destination metadata.
- PHP uses `apiEndpoint` and explicit anonymous credentials. Compose supplies
  `contentType`, which this SDK requires for extensionless destination names.
  V4 URLs use separate public scheme/host options and a signing object whose
  path includes the bucket; V2 uses the ordinary object with a bucket-bound URL.
  Both signatures are independently verified against the final request URL.
  Its V2 signer
  hardcodes HTTPS, which matches the shared TLS frontend. Neither PHP nor Go
  rewrites signed URL schemes after signing.

The common HTTP/1 frontend uses a two-second, 32 MiB bounded drain after early
errors, so clients that finish writing before reading retain the backend's
checksum-specific response. HTTP/2 uses its normal stream handling. Signed
cases run in permissive profiles and do not establish server-side signature
verification; the C++ probe's signatures were separately verified against
`--signed-urls verify --signed-host-port exact` (see
[its slice](../docs/en/slices/sdk-scorecard-cpp.md#signature-verification)). The V4 canonical host of `@google-cloud/storage` 8.4.0 and 9.0.0 drops a nondefault port; strict
use at this origin remains unsupported. See the transport contract for limits.

Ordinary SDK headers and query values are preserved. A failure during setup
(for example, upload rejected before a subsequent range read) fails that
workflow; it is not proof that its later API operation is broken. New checksum
rejection cases require a checksum-specific 400 response and an absent object;
an unrelated rejection does not earn credit. Lifecycle, CORS and other setting
probes check configuration round trips, not scheduled lifecycle execution or
browser behavior.

## Running

Requires Bash, Rust, Go, Ruby 4.0.9 with Bundler, Node.js 24+ with npm, uv,
the JDK and Maven pinned in `java/`, the .NET SDK pinned in `dotnet/global.json`,
Docker, curl and jq. uv selects Python 3.15.2; PHP and Composer run in Docker,
and so do the Rust and C++ probes (see Limitations).

```sh
bash sdk-scorecard/run.sh                       # all products, results/<UTC date>/
bash sdk-scorecard/run.sh gcs-mimic             # one product
bash sdk-scorecard/run.sh --out "$TMPDIR/sc"    # scratch run
bash sdk-scorecard/score.sh sdk-scorecard/results/2026-10-29   # rescore recorded results
bash sdk-scorecard/test-score.sh               # offline scoring checks
```

`SCORECARD_RUN=<substring>` restricts individual probes to matching case IDs when
debugging a single case against a running emulator:

```sh
cd sdk-scorecard/go
SCORECARD_ENDPOINT=http://127.0.0.1:4443 SCORECARD_RUN=object.compose go run .
```

## Results

The [sixth 2026-11-15 comparison](results/2026-11-15-6/summary.md) is the
latest. It gives Go, Ruby, Node.js, PHP, Python, Rust and C++ `sdk` cases for
the workflows their public APIs reach beyond the shared core, and moves the
seven Java and .NET cases that measure the same workflows to the shared IDs,
so it executes 2,324 cases on the 0.22.0 source at `1f5268a0`, with no
uncommitted change. Every result outside the `sdk` area, and every moved
case's result, is the same as in the fifth comparison for all four products,
and the Core scores are unchanged. gcs-mimic scores **99.1**: it passes every
new case except two. Python's `sdk.xml_multipart_upload` fails because a part
upload answers without the `x-goog-hash` the SDK validates. PHP's
`sdk.stream_wrapper` fails because the SDK declares `X-Upload-Content-Length:
0` before streaming chunks of unknown total, and gcs-mimic holds the session
to that 0; Cloud Storage's answer is unrecorded. floci-gcp scores **74.2**,
fake-gcs-server **39.2** and gcp-storage-emulator **35.7**. The contract and
per-case results are in
[the SDK-specific slice](../docs/en/slices/sdk-scorecard-sdk-cases.md).

The [fifth 2026-11-15 comparison](results/2026-11-15-5/summary.md) gives the
`grpc` area SDK-neutral IDs that Go, Java, Python and
Rust each emit where their SDK has a public gRPC API, and moves Java's
`sdk.grpc_*` cases to them, so it executes 2,224 cases on the 0.22.0 source
at `d208ae26`, with no uncommitted change. Every result outside the `grpc`
area is the same as in the fourth comparison for all four products, and the
Core scores are unchanged. gcs-mimic scores **99.5**: it passes every `grpc`
case except two. Java's `grpc.bucket_create_get_delete` fails because the
Java SDK's default bucket read mask names `billing`, which gcs-mimic does not
model. Python's `grpc.appendable_object` fails because the Python SDK waits
for a response to the appendable open that gcs-mimic sends only at a flush.
floci-gcp scores **74.0**: it passes the bucket, object, precondition and
write-session cases but implements neither bidi reads nor appendable objects,
and it rejects a checksum mismatch with DATA_LOSS instead of
INVALID_ARGUMENT. fake-gcs-server scores **38.3** and gcp-storage-emulator
**35.3**, failing every `grpc` case. The contract and per-case results are in
[the gRPC slice](../docs/en/slices/sdk-scorecard-grpc.md).

The [fourth 2026-11-15 comparison](results/2026-11-15-4/summary.md) adds
five shared cases (`bucket.soft_delete_restore`,
`object.csek_rotation`, `object.compose_preconditions`,
`signed.v4_resumable_upload` and `signed.v4_response_overrides`) and moves the
three .NET `sdk.*` cases that measured the same behaviors to those IDs, so it
executes 2,184 cases on the 0.22.0 source at `43d84560`, with no uncommitted
change. Every case it shares with the third comparison has the same result for
all four products, so each score change comes from the new cases. gcs-mimic
scores **99.8**: it passes every new case except Rust's
`object.csek_rotation`, because the Rust SDK rewrites through gRPC, where
gcs-mimic rejects customer-supplied keys as unsupported. floci-gcp scores
**74.4**, fake-gcs-server **38.9** and gcp-storage-emulator **35.9**. None of the
three restores a bucket, rotates a key or starts a signed resumable upload;
only floci-gcp's gRPC compose, which Rust uses, honors pinned source
generations; floci-gcp returns the response overrides in all nine SDKs and
fake-gcs-server in seven. The contract and per-case results are in
[the shared-case slice](../docs/en/slices/sdk-scorecard-shared-cases.md).

The [third 2026-11-15 comparison](results/2026-11-15-3/summary.md) is the
first with the case catalog, which `score.sh` checked every
result against. It executes the same 2,016 cases on the 0.22.0 source at
`814c6c43`, with no uncommitted change. The only case that changed for any
product is Ruby's `object.list_offsets`, now `n/a`, so Ruby scores 50 cases.
None of the stricter probe checks changed a result. gcs-mimic scores **100**,
floci-gcp **78.3**, fake-gcs-server **40.5** (it had passed that case) and
gcp-storage-emulator **38.7** (it had failed it).

The [second 2026-11-15 comparison](results/2026-11-15-2/summary.md) executes the same 2,016 cases on the 0.22.0 source at `6652e6fa`,
with no uncommitted change, after the branch was rebased onto main and the two
defects below were fixed. gcs-mimic scores **100**, with C++ **51/51**;
floci-gcp **78.3**, fake-gcs-server **40.6** and gcp-storage-emulator **38.6**.
Only those two gcs-mimic C++ cases changed from the first run.

The [first 2026-11-15 comparison](results/2026-11-15/summary.md) adds the C++
SDK (google-cloud-cpp 3.13.0 built with GCC 16.2.0) and executes 2,016 cases
(504 per product, 499 of them scored). It was measured at `fec7476f`, with no
uncommitted change, before that commit was rebased onto main, so the hash is
not on main. With nine SDKs each contributes 100/9 points, so the scores are
not comparable with the eight-SDK runs below. Every case of the other eight
SDKs has the same result as on 2026-11-12 for all four products. gcs-mimic
scored **99.6**, failing two C++ cases that were emulator defects:
`object.soft_delete_restore`, because `RestoreObject` sends a `{}` body that
gcs-mimic rejected with 400, and `signed.post_policy_v4`, because the SDK
writes the policy expiration with nanoseconds and gcs-mimic parsed it to the
microsecond only. See [the C++ probe](cpp/README.md).

The [2026-11-12 comparison](results/2026-11-12/summary.md) executes the same 1,812 cases on the 0.19.1 source at `590d9276`, with no
uncommitted change, and with the Ruby Storage SDK 1.62.2, the Rust SDK 1.22.0, .NET SDK
11.0.100 and Go 1.27.4. Every product scores the same as on 2026-11-09:
gcs-mimic **100**, floci-gcp **77.6**, fake-gcs-server **39.8** and
gcp-storage-emulator **37.7**.

The [2026-11-09 comparison](results/2026-11-09/summary.md) adds floci-gcp
0.12.0. It executes 1,812 cases (453 per product, 448 of them
scored) on the 0.16.0 source at `b0338de7`. gcs-mimic scores **100**, floci-gcp
**77.6**, fake-gcs-server **39.8** and gcp-storage-emulator **37.7**. Its
metadata records `source_dirty: true` because the only uncommitted change was
adding floci-gcp to `run.sh`.

The [2026-11-08 comparison](results/2026-11-08/summary.md) executes 1,359 cases (453 per product, 448 of them scored) from a clean checkout
of the `v0.14.0` tag at `ad9bca82`. gcs-mimic scores **100**, fake-gcs-server
**39.8** and gcp-storage-emulator **37.7**, the same as the 2026-10-31 run.

The [final 2026-10-31 eight-SDK comparison](results/2026-10-31-2/summary.md)
executes 1,359 cases (453 per product) from clean source `35de4e1`. gcs-mimic
scores **100**: Go **57/57**; Ruby, Node.js, PHP and Python each **51/51**; Java
and .NET each **70/70** (.NET 1 n/a); Rust **47/47** (4 n/a). fake-gcs-server
scores **39.8** and gcp-storage-emulator **37.7**; neither serves the gRPC API
that the Rust SDK uses for bucket operations, so both record 0/47 for Rust. The
weights differ from the five-SDK runs below, so compare scores only within a run.

The [first 2026-10-31 eight-SDK run](results/2026-10-31/summary.md) at
`b7975c3` remains historical evidence: gcs-mimic scored **99.6** with two
failures. Java `sdk.grpc_basic_object` (`StorageException: Illegal embedded
sign character`) came from a gRPC `Bucket.project` that held the project ID
instead of the project number, followed by rejected `checksums.*` read-mask
paths. Rust `object.acl` (`ACL missing`) came from the probe reading without the
`acl` read mask that the proto requires.

The [final 2026-10-29 shared-HTTPS comparison](results/2026-10-29-5/summary.md)
executes 783 cases from clean source `e7fb384`: gcs-mimic scores **100**, with
Go **57/57** and Ruby, Node.js, PHP and Python each **51/51**.
fake-gcs-server scores **44.9** and gcp-storage-emulator **41.5** using the same
SDK/runtime pins, CA and case lists. All three metadata files record
`source_dirty: false`. Raw cases, exact commit/container provenance, lockfile
hashes and the final native verification log are kept in that result directory.
The declared Node/PHP/Python scorecard acceptances pass; their broader ledger
rows remain `PARTIAL` for strict/native-TLS/other-profile/provider scopes.
See [integration scope and final checks](../docs/en/slices/sdk-scorecard-integration.md).

The [initial integrated HTTPS run](results/2026-10-29-4/summary.md) remains
immutable historical evidence: gcs-mimic 94.9, fake-gcs-server 44.1 and
gcp-storage-emulator 40.0. Its failures and subsequent scoped corrections are
explained in the integration contract.

The historical [2026-10-29 five-SDK HTTP run](results/2026-10-29-3/summary.md) records 783
case executions: 261 per product. Scores are gcs-mimic **63.9**,
fake-gcs-server **46.1** and gcp-storage-emulator **40.7**.
gcs-mimic passes Go 57/57, Ruby 51/51, Node.js 22/51, PHP 21/51 and Python
18/51. Its new-client failures chiefly begin at ordinary upload: Node.js/PHP
send an idempotency header without a generation condition, while Python sends
`x-upload-content-type`. Python bucket ACL resource updates and its capitalized
object-retention query boolean also fail. The raw results preserve these
failures. Those historical gaps are corrected for the final declared comparison
above; the broader integration rows remain `PARTIAL` for their pending scopes.

The former two-SDK score of 100 and the historical HTTP score of 63.9 use different SDK
sets and weights. Go/Ruby still pass all their cases in this run. No server
behavior was changed to obtain this measurement.

Validation for that historical harness-only change passed: `cargo check --workspace --all-targets
--locked --offline`, `cargo test -p xtask --locked --offline` (44 tests),
`cargo clippy -p xtask --all-targets --locked --offline -- -D warnings`,
`bash sdk-scorecard/test-score.sh`, ShellCheck for all three scorecard shell
scripts, `cargo xtask compatibility check`, Markdown lint and
`cargo xtask dependencies check` (all inputs current). The full application
test suite, strict/TLS SDK profiles and real-cloud differential tests were not
run for this harness-only change.

Each `results/<date>/` directory keeps the raw evidence:

- `<emulator>.meta.json`: product, version, measurement time, source dirty flag,
  SDK/runtime versions, PHP image digest and expected case counts;
- `<emulator>.<sdk>.jsonl` for `go`, `ruby`, `node`, `php`, `python` (and `java`,
  `dotnet`, `rust` in runs that include them): one line per case with the
  failure detail;
- `summary.md`: totals, per-area counts and the case matrix.

The final integration set also contains provenance.json, input-sha256.txt,
native-verification.log and runner.log. Log path prefixes are normalized with
a first-line notice; case files and metadata are copied unchanged.

Compare runs only when their case lists match; adding or removing a case changes
every product's denominator. When the probes change, rerun all products and
start a new results directory. A later run on the same UTC date goes in
`results/<date>-<n>/` (for example `2026-10-29-2`) so earlier results stay
unchanged.
The runner selects a fresh suffix automatically and refuses nonempty output
directories. Probe process failures retain raw output and stop score production;
ordinary compatibility failures remain recorded `pass: false` cases.

## Limitations

- A shared case ID asserts the catalog's checks in every SDK, but SDKs reach
  them through different requests, recorded as catalog `variants`. One
  difference remains in what is checked: Go's public API cannot delete a single
  metadata key, so Go's `object.update_metadata` verifies merging and clearing
  the whole map where the other eight SDKs verify individual-key deletion.
  Checks are reviewed against each probe by reading it; no tool proves a probe
  asserts what the catalog says.
- Historical Go/Ruby checksum probes accepted any upload error followed by an
  absent object. Current probes in all nine SDKs require a checksum-specific 400
  and no object. Python still uses a larger resumable input for its public
  supplied-checksum API; byte-for-byte cross-SDK equivalence is unverified.
- In the historical HTTP run, Go changed HTTPS to HTTP after V2 signing while
  PHP kept HTTPS and failed the origin guard. That failure measured an endpoint
  constraint. Current runs use common HTTPS and send both signed URLs unchanged;
  historical scores cannot be compared as equivalent transport evidence.
- The Rust SDK builds its own HTTP client and trusts only the operating system's
  roots: on macOS that is the Keychain, which ignores `SSL_CERT_FILE`, so the
  scorecard CA cannot be trusted natively. The Rust probe therefore always runs
  in the pinned Linux container, with the container's loopback ports forwarded to
  the host by `socat`, which `sdk-scorecard/install-socat.sh` installs at run
  time from a fixed snapshot.debian.org archive with a pinned version (network
  access is still required).
  `run.sh` and `test-tls.sh` require a working Docker for it; neither skips the
  Rust probe when Docker is unavailable.
- The C++ probe is built from source in the pinned `gcc` image and runs there
  with the same socat forwarding (installed into the image at build time), so
  the SDK's libcurl and OpenSSL are the image's Debian packages rather than the
  host's. Only its JSON/REST transport is measured; the SDK's gRPC client is a
  later scope.
- Each probe runs against its own freshly started emulator. This removes
  cross-SDK state leakage but means products are restarted nine times per run
  and per-SDK results are not from one continuous server state.
- The SDK-specific `sdk` area exists only for Java and .NET, so their `Score`
  includes cases other SDKs do not run; use `Core` to compare SDKs.
- Equal case weights do not reflect how often each feature is used.
- Historical scores measure permissive HTTP and default-region behavior. Current
  runs use a shared HTTPS frontend, not each emulator's native TLS listener.
  Strict authorization, other profiles/regions/transports, browser CORS, crash
  recovery and real-GCS parity remain separate verification scopes.
- A case is pass/fail; partial support inside a case is not credited.
- Full error-message text and exact HTTP headers are not compared; the new
  checksum cases do inspect the error message to classify checksum rejection.
