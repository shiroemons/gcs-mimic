# SDK scorecard

<!-- markdownlint-configure-file {"MD013": false} -->

The scorecard measures workflows using the official Go, Ruby, Node.js, PHP and
Python Cloud Storage SDKs against local emulators. Within each SDK, it runs the
same probes, inputs and pass criteria against gcs-mimic, fake-gcs-server and
gcp-storage-emulator. Shared case IDs across SDKs do not establish identical
inputs, assertions or endpoint adaptations. Cross-SDK equivalence is not yet
verified, so aggregate scores are provisional.

It is a compatibility comparison, not a performance benchmark, and not an
implementation-status source: `compatibility/features.json` remains the only
status ledger. A passing probe means an ordinary SDK user observes the expected
result for that workflow; it does not mean the underlying API method is
`IMPLEMENTED`.

## Scoring

A product that passes every case in all five SDKs scores 100.

- Each SDK contributes 20 points.
- Within an SDK, every case has equal weight: `20 × passed / total`.
- Score = sum of the five SDK contributions, rounded to one decimal.
- Historical two-SDK runs keep their original 50-point weights when rescored.

Only the Go probe exercises gRPC, so it has more cases than the other probes.
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

| Area | Go | Ruby | Node.js | PHP | Python | Scope |
| --- | --- | --- | --- | --- | --- | --- |
| bucket | 20 | 20 | 20 | 20 | 20 | Bucket CRUD/listing, labels, preconditions, versioning, lifecycle, CORS, retention and lock, UBLA, PAP, IAM, soft-delete policy, autoclass, ACL/default ACL, notifications, HMAC keys, service account |
| object | 27 | 27 | 27 | 27 | 27 | Uploads (single, resumable, metadata, checksums), range reads, PATCH, delete, generation/metageneration conditions, listing (delimiter, paging, offsets, glob), copy, rewrite, compose, move, ACL, holds, object retention, custom time, storage class, gzip transcoding, CSEK, soft delete/restore |
| signed | 4 | 4 | 4 | 4 | 4 | V4 GET/PUT, V2 GET, V4 POST policy |
| grpc | 6 | – | – | – | – | `storage.NewGRPCClient`: bucket CRUD, write/read, list, update/delete, MultiRangeDownloader, appendable objects |

Case IDs are shared between the SDKs. The probes live in
[`go/main.go`](go/main.go), [`ruby/probe.rb`](ruby/probe.rb),
[`node/probe.mjs`](node/probe.mjs), [`php/probe.php`](php/probe.php) and
[`python/probe.py`](python/probe.py). The scorer rejects missing files,
incomplete/duplicate cases, invalid pass values, different SDK inputs or case
sets across products, and differing shared case IDs across SDKs.

## Pinned inputs

| Input | Version |
| --- | --- |
| Go SDK | `cloud.google.com/go/storage` v1.70.0 (`go/go.mod`, same graph as `sdk-tests/go`) |
| Ruby SDK | `google-cloud-storage` 1.62.1 (`ruby/Gemfile.lock`, same graph as `sdk-tests/ruby`) |
| Node.js SDK | `@google-cloud/storage` 8.4.0 (`node/package-lock.json`); measured on Node.js 26.13.0, requires Node.js 24+ |
| PHP SDK | `google/cloud-storage` 2.6.1 (`php/composer.lock`); official Composer 2.11.0 / PHP 8.5.12 image, digest pinned in `run.sh` |
| Python SDK | `google-cloud-storage` 3.17.1, `cryptography` 50.0.2 for offline RSA signing (`python/uv.lock`); Python 3.15.2 |
| gcs-mimic | Release build of the checked-out source; the commit is recorded in `*.meta.json` |
| fake-gcs-server | `fsouza/fake-gcs-server:1.56.1` |
| gcp-storage-emulator | `oittaa/gcp-storage-emulator:v2026.07.19` |

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
`cargo xtask dependencies check` includes these three ecosystems and the
Composer image in the existing daily update detection.

## Emulator configuration

Every product receives the same HTTPS loopback origin through a Go
standard-library TLS frontend and a temporary development CA. Each official
SDK and signed-request client verifies that CA and the certificate hostname.
The HTTP backends bind only loopback and use a fresh in-memory or temporary
data store in their most capable documented configuration:

- gcs-mimic: `serve --scheme http --grpc-listen ...` with the default
  permissive authentication profile.
- fake-gcs-server: `-scheme http -backend memory` (the memory backend is the
  only one that retains object versions); gRPC cases use the shared HTTP port.
- gcp-storage-emulator: `start --in-memory`; it has no gRPC listener, so the
  gRPC cases fail by timeout.

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
verification. `@google-cloud/storage` 8.4.0's V4 canonical host drops a nondefault port; strict
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
Docker, curl and jq. uv selects Python 3.15.2; PHP and Composer run in Docker.

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
- `<emulator>.<sdk>.jsonl` for `go`, `ruby`, `node`, `php`, `python`: one line per case with the
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

- Shared case IDs currently cover different checks in some SDKs. Go's
  `object.update_metadata` verifies merging and clearing the whole map;
  Ruby and the three added SDKs verify individual-key deletion. Go's resumable
  case explicitly compares returned CRC32C as well as size and bytes; the other
  probes do not make that same explicit assertion.
- Historical Go/Ruby checksum probes accepted any upload error followed by an
  absent object. Current probes in all five SDKs require a checksum-specific 400
  and no object. Python still uses a larger resumable input for its public
  supplied-checksum API; byte-for-byte cross-SDK equivalence is unverified.
- In the historical HTTP run, Go changed HTTPS to HTTP after V2 signing while
  PHP kept HTTPS and failed the origin guard. That failure measured an endpoint
  constraint. Current runs use common HTTPS and send both signed URLs unchanged;
  historical scores cannot be compared as equivalent transport evidence.
- Equal case weights do not reflect how often each feature is used.
- Historical scores measure permissive HTTP and default-region behavior. Current
  runs use a shared HTTPS frontend, not each emulator's native TLS listener.
  Strict authorization, other profiles/regions/transports, browser CORS, crash
  recovery and real-GCS parity remain separate verification scopes.
- A case is pass/fail; partial support inside a case is not credited.
- Full error-message text and exact HTTP headers are not compared; the new
  checksum cases do inspect the error message to classify checksum rejection.
