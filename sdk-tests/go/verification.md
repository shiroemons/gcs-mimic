# Go SDK harness verification

<!-- markdownlint-configure-file {"MD013": false} -->

## Bucket metadata PUT contract tests: 2026-10-23

`TestBucketMetadataReplace` and `TestStrictAuthBucketMetadataReplace` exercise
`google.golang.org/api/storage/v1` `Service.Buckets.Update`. The pinned generated
client constructs HTTP PUT, whereas high-level `BucketHandle.Update` constructs
PATCH. The tests contrast replacement with merge, verify modeled omission
defaults, successful and failed match/not-match conditions, response/Attrs
consistency, revisions, existing object bytes/generation, strict viewer denial,
admin success and IAM policy etag preservation.

Pins remain Go 1.27.3, Storage v1.70.0, API v0.307.0 and OAuth2 v0.37.0.
The raw API client was already required; `go.mod` and `go.sum` are unchanged.
`go vet ./...` and `go test -run '^$' ./...` passed before server integration.
The existing guarded transport, explicit emulator origin, static local tokens
and redirect rejection apply to every raw client request; no ADC or Google
metadata-server discovery is used.

Against application source `ab7f410`,
`GCS_MIMIC_GO_TEST_RUN='^(TestBucketMetadataReplace|TestStrictAuthBucketMetadataReplace)$' bash scripts/sdk-go.sh`
passed vet and both top-level tests with zero failures or skips (package time
0.517s). The runner starts isolated native permissive and strict servers and
cleans up both. No change to `scripts/sdk-go.sh` was needed: its existing
test-name selector includes the new tests automatically.

The comparison source is fake-gcs-server v1.56.1, commit
`896ece51e49eed0946089f3089f6343855c0d106`, archive SHA-256
`13fa1ed9cff0216baedaedb162b3cf37575bba955fb55fd4b76acb9b818494dc`.
The pinned [router](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/server.go#L244-L253)
registers bucket PATCH and POST with PATCH override, but no bucket PUT.
The [bucket handler](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/bucket.go#L35-L68)
parses only versioning/default hold and is not reached by PUT.

`GCS_MIMIC_REFERENCE_REPO=https://github.com/fsouza/fake-gcs-server bash scripts/compare-reference-emulator.sh`
exited 0 on checkout `4bb930cbfe75d4a2c189088fc39ab269cbf712b4` with
application/test source `ab7f410`. Both reference backends and both local
profiles ran 292 requests each (1,168 raw records total), including 32 PUTs
per target. All four PATCH/PUT assertion groups passed. The reference-only
invocation with `GCS_MIMIC_REFERENCE_ONLY=1` also exited 0, with 584 records.
The script verifies the source archive digest before building the pinned
reference and uses the committed Rust 1.99.1 and Go 1.27.3 toolchains.

The local assertions check whole-map replacement; omitted/null/empty labels;
STANDARD/false omission defaults; native null-boolean 400 versus legacy reset;
null class 400; unsupported settings/ACL/billing 501; malformed body and invalid
projection/generation 400; missing bucket 404; condition failure 412 without
mutation; one revision increment per success; and PUT/GET/list agreement with
unchanged identity, placement, object list and bytes. Both local profiles pass
these assertions. The reference has no successful PUT and therefore provides
no omitted-field or null-scalar replacement contract.

Observed reference differences in both backends:

- All 32 PUT cases return HTTP 404 with the same JSON envelope: code 404,
  message `Not Found`, domain `global`, reason `notFound`. This includes an
  existing bucket, malformed JSON, null/empty metadata, unsupported settings,
  projection, billing/ACL queries and passing/failing metageneration conditions.
  No PUT replacement, input validation or condition enforcement is inferred.
- Every PUT preserves the post-upload GET/list bucket resource and object bytes.
  The reference retains fixed metageneration `1` and etag `RVRhZw==`; local
  successful replacements increment metageneration and change the etag.
- The fixture upload itself resets the filesystem backend bucket default hold
  from true to false and replaces its creation/updated timestamps. Memory
  preserves those fields. An explicit post-upload baseline isolates this
  preexisting backend behavior from PUT; local profiles preserve bucket metadata
  during object upload.

This comparison is not a Google-service differential test. Google omitted/null
defaults remain limited to the separately frozen local contract. No container
build, getting-started run, upstream test suite or real Google bucket is used
by these checks.
Markdownlint, `bash -n`, `git diff --check` and the changed-file local-path scan
passed. MD013 remains disabled by the existing document directive for long
commands and source links. The complete Go suite, container builds and
getting-started journeys remain separate final integration checks.

## Bucket metadata PATCH contract tests: 2026-10-23

`TestBucketMetadataUpdate` and `TestStrictAuthBucketMetadataUpdate` add official
SDK acceptance cases for label merge/null removal, default storage class,
versioning/default hold true and false, both bucket metageneration conditions,
read-after-write metadata consistency and strict read-only denial. The existing
guarded clients and local-only identity registry are reused.

The slice-start `cargo xtask dependencies check` exited 0: Go 1.27.3,
Storage v1.70.0, API v0.307.0 and OAuth2 v0.37.0 are CURRENT. No module or
lockfile changes were made. `GOTOOLCHAIN=auto go vet ./...` passed and the two
new tests compile and explicitly skip with emulator endpoints unset.
Against reviewed application source `2b4441cef21370f95cfb3506da1cbf1ebda09787`,
merged in verification checkout `3035d23f7b3a9c85a786441fc07c961d037c8eca`,
the final `env -u GCS_MIMIC_GO_TEST_RUN bash scripts/sdk-go.sh` exited 0:
vet, all 31 top-level tests and 73 subtests passed, with zero failures or skips
(package time 3.239s). The four-target pinned reference comparison also exited 0,
with 185 requests per target, 740 records total and both PATCH assertion groups
passing. The two bucket PATCH tests pass against isolated permissive/strict
native servers; the raw corpus independently verifies both compatibility profiles.
The [comparison evidence](../../docs/en/evidence/bucket-metadata-patch-comparison.md)
records exact serializer mappings, documented reference differences and the final
raw comparison plus complete SDK output. These checks involved no dependency
updates, real Google buckets, metadata server, container build or upstream
test-suite execution. Markdownlint passes; MD013 alone is disabled for existing
long tables and reproducible commands. The local-path scan and diff checks pass.

`updated` uses the injected clock and JSON millisecond precision. Consecutive
updates can share a timestamp; tests require it to be non-decreasing and not
before creation time. A successful mutation is established independently by
metageneration increasing exactly once, a changed ETag, the requested metadata
change and agreement with subsequent Attrs. Failed conditions and denied
permissions must preserve the entire checked metadata revision.

## Resumable Writer contract tests: 2026-10-22

The official Go download feed and module-proxy `@latest` endpoints confirm the
existing pins: Go 1.27.3, Storage v1.70.0 and API v0.307.0. No dependency files
changed. Storage resolves to upstream revision
`9a2913189e552da51b0fd60aa635027426f04d71`; API resolves to
`fbb4c3c103847f35323cf2261403643400b22062`.

Eight top-level acceptance tests now cover default and explicit chunk sizes,
unknown-length streaming, supplied-checksum rejection and old-generation
preservation, atomic conditions, context cancellation, one lost chunk response,
strict-admin upload, metadata-only condition races and strict creator overwrite
races. The obsolete resumable 501 assertion was removed from
the media lifecycle test. The new journeys are defined against official SDK
behavior; no emulator integration pass is claimed.

Harness-only checks passed with Go 1.27.3:

- `GOTOOLCHAIN=auto go vet ./...`.
- `GOTOOLCHAIN=auto go test -run XXX_none ./...` compiles all packages.
- With both emulator endpoint variables unset, the eight new top-level tests skip
  explicitly; endpoint validation passes without contacting a server.
- `git diff --check` and the repository local-path scan pass.

The guarded transport is retained for every request, including session URLs.
The interruption wrapper records at most 16 chunk requests and discards at most
64 KiB of one acknowledgement body. The largest payload is 16 MiB plus 37 bytes,
which exercises actual resumable requests with untouched default `ChunkSize`;
the remaining payloads stay below 1 MiB. These do not measure server resource
use. The retry journey checks actual POST ranges, same-session retries,
invocation token preservation and final automatic CRC32C.
Context cancellation is followed by a raw guarded status query confirming the
persisted prefix, then an explicit session DELETE expecting 499. This verifies
session retention independently of the Writer's own cancellation. No new test
sets `ChunkSize=0` or disables automatic checksums. Server execution, containers,
expiry, permit-release measurement and recovery remain pending.

## Public and default XML reads: 2026-10-22

After server retry-telemetry support in `ccecff8`, `just check-sdk` passed Go vet,
all 18 top-level tests and 58 subtests, with 0 failures and 0 skips, against
isolated native permissive and strict endpoints. The Go package reported 1.193s.
Pins remain Go 1.27.3, Storage v1.70.0, API v0.307.0 and OAuth2 v0.37.0;
`go.mod` and `go.sum` did not change.

The new coverage accounts for 5 top-level tests and 20 subtests:

| Test | Subtests | Verified behavior |
| --- | --- | --- |
| `TestXMLReadsStrict` | 5 | Default XML whole/range reads, generation query, matching conditions and HEAD; rejected generation/metageneration conditions additionally return 412 |
| `TestPublicReadsGrantAndRevoke` | 2 | XML and JSON anonymous denial, bucket `allUsers` viewer grant, exact bytes and public cache default, then denial after revocation |
| `TestPublicReadsAllAuthenticatedUsers` | 8 | XML/JSON protocol groups and registered no-project-role, anonymous and unregistered bearer cases; only the registered principal succeeds |
| `TestXMLReadsEncodedNames` | 5 | Spaces, plus, literal percent escapes, Unicode and slashes through actual XML request paths |
| `TestXMLReadsPermissive` | 0 | Default XML download without credentials |

Reader assertions compare bytes, full object size, generation, metageneration,
content type and cache control, including the XML private default
`private, max-age=0` and public default `public, max-age=3600`. The recorder checks
the real escaped request path, method, range, selected generation and condition
headers. It wraps the supplied transport before SDK construction because Storage
v1.70.0 clones the HTTP client. Read clients disable retries, preserving the
SDK's invocation/idempotency headers and exposing unsupported responses promptly.
Each public protocol journey uses its own bucket and cleans up with the admin.

An additional targeted run with both endpoint variables unset passed with all
5 new top-level tests explicitly skipped and no subtests executed. This verifies
opt-in harness behavior only, separate from the successful integration run.
`git diff --check` also passed for the harness changes.

The pinned upstream XML URL and response-header behavior is documented in
[README](README.md#public-and-default-xml-reads). No Google Cloud or metadata-server
request was made. This check did not build containers, run the getting-started
guide, verify cloud authentication error parity or exercise deferred XML writes,
listing, virtual-host routing, signatures, ACLs, UBLA or PAP state machines.

## Final bucket IAM verification: 2026-10-22

On merged source `7fadac2d5c15fd2534f915d90295f5d314486cb1`, `just check-sdk`
passed Go vet, all 13 top-level tests and 38 subtests with 0 failures or skips.
Pins: Go 1.27.3, Storage v1.70.0, API v0.307.0 and OAuth2 v0.37.0;
`go.mod` and `go.sum` remained unchanged. Both `TestStrictAuthBucketIAM` and
`TestBucketIAMPermissive` passed against isolated native endpoints.

The literal full getting-started guide also passed against its source-built
ARM64 Compose image. Its permissive endpoint command passes 8 top-level tests
and intentionally skips the 5 strict-only tests; the later
`go test -count=1 -v -run '^TestStrictAuth' ./...` against strict Compose passes
all 5 strict tests with no skips. The ready event reports 4 identities, and the
guide checks IAM grant/revoke and stale-etag preservation, public URL 404,
permission boundaries and unchanged policy etag after stop/start.

Durable outputs: [complete native Go run](../../docs/en/evidence/bucket-iam-sdk-go.txt)
and [full guide](../../docs/en/evidence/bucket-iam-guide.txt).
[Final integration evidence](../../docs/en/evidence/bucket-iam.md) records the
260 passing Rust tests, both Linux architectures, dependency freshness, source
and image digests, ledger decisions and remaining cloud-parity limits.
No real Google Cloud or metadata-server requests were made.

## Bucket IAM integration: 2026-10-22

The following records the earlier native integration before final verification.

The pinned Go 1.27.3 / Storage v1.70.0 / API v0.307.0 suite ran against isolated
permissive and strict native servers with the bucket IAM implementation merged.
No module requirements or checksums changed. The strict fixture adds a principal
with no project roles; both IAM tests retain the default convenience bindings.

| Check | Result |
| --- | --- |
| `env -u GCS_MIMIC_GO_TEST_RUN bash scripts/sdk-go.sh` | PASS: vet, 13 top-level tests and 38 subtests; 0 failed, 0 skipped |
| IAM-only Go runner after exact default-member assertions | PASS: vet and both IAM tests; 0 failed, 0 skipped |
| `TestStrictAuthBucketIAM` | PASS: default policy, bucket-only read/list, other-bucket denial, denied creation and policy management, stale etag and revocation |
| `TestBucketIAMPermissive` | PASS: unauthenticated get/set, fresh etag and 412 for a stale etag |
| Native getting-started IAM section | PASS: all 4 shell blocks, 75 shell lines, extracted directly from the guide and executed with `bash -euxo pipefail` |
| Policy etag after native restart | PASS: guide's policy GET and etag assertion with the same catalog on a new loopback port |
| `cargo fmt --all --check` | PASS |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS |
| `cargo test --workspace --locked --offline` | PASS: 256 tests; 0 failed, 0 ignored |
| `npx --yes markdownlint-cli2` on changed Markdown | PASS: 5 files; MD013 disabled for the existing long tables and shell commands; no repository lint config |

The native walkthrough used the guide's admin setup and the checked-in identity
registry. It checked exact downloaded bytes, listing, 403 boundaries, 412
`conditionNotMet` without losing the grant, a fresh revocation etag and 403 after
revocation. A native restart preserved the revoked policy etag; the Compose
restart commands were not executed for this check. Its objects, test buckets and
server were cleaned up. Raw SDK, server and command output stays
under `$TMPDIR` for integration evidence collection, outside the repository.
No container build or Google Cloud/metadata-server request was made.

The results below describe earlier harness revisions and remain historical.

The original harness-only results are retained below. Current pins are Go 1.27.3,
Storage v1.70.0 and API v0.307.0. The
[2026-10-22 SDK update evidence](../../docs/en/evidence/go-sdk-update.txt) records
the current full native suite. The
[multipart integration evidence](../../docs/en/evidence/multipart.md) records the
final full suite, including `TestObjectMultipartWriter` with `ChunkSize=0`,
metadata/checksums, conditional create/overwrite, Attrs and JSON NewReader.
Resumable Writer remains unsupported; upload invocation tokens require
generation-match and have no response replay. Results and versions below are
historical evidence rather than current coverage claims.

## Integrated execution

`bash scripts/sdk-go.sh` with the rustup proxy first in PATH passed vet and
both TestEmulatorEndpoint and TestBucketLifecycle, with no skipped tests.
The unchanged high-level storage client also passed against the read-only-root
Compose service from the getting-started flow. Requests stayed on the explicitly
validated emulator origin; no Google/metadata endpoint was contacted.

The native script selects a free loopback port first and passes that same origin
as `--api-base-url`. A competing bind makes startup fail instead of changing the
advertised endpoint. The pinned high-level `storage.BucketAttrs` does not expose
`selfLink`, so this Go workflow does not check that field; Rust wire tests check
the configured origin. The SDK type was checked on 2026-10-21 against
[bucket.go at v1.69.0](https://github.com/googleapis/google-cloud-go/blob/storage/v1.69.0/storage/bucket.go).

High-level Attrs/list request `projection=full`, so the emulator accepts it while
omitting unmodeled ACL/owner fields as an explicit compatibility deviation. The
SDK sends `pageToken=` initially, which selects the first page; malformed nonempty
tokens remain errors. These decisions and limitations are in the
[slice](../../docs/en/slices/json-bucket-crud.md) and
[integrated evidence](../../docs/en/evidence/bucket-crud.md).

## Commands and results

Rust commands below ran with `env -u RUSTUP_TOOLCHAIN` and the rustup proxy
directory first in `PATH`. The local mise shim otherwise re-injected 1.93.0 and
produced `error: rustc 1.93.0 is not supported ... requires rustc 1.99.1`.
`rustc -Vv` through the proxy reported 1.99.1 (58cf8f9c3 2026-10-13).

Go commands ran in `sdk-tests/go` with `GOTOOLCHAIN=auto`,
`GOMODCACHE=$TMPDIR/gcs-mimic-go-mod` and `GOCACHE=$TMPDIR/gcs-mimic-go-build` so the
checked toolchain and module downloads did not modify the user's Go cache.
The original Go 1.24.5 successfully downloaded Go 1.27.3 (darwin/arm64) after
network access was allowed. Official sources and pins are in [README](README.md)
and [the dependency snapshot](../../compatibility/dependency-status.json).

| Command | Result |
| --- | --- |
| `go version` | `go version go1.27.3 darwin/arm64` |
| `go vet ./...` | PASS |
| `go test -count=1 -v ./...` | PASS: endpoint guard; SKIP: bucket integration |
| `cargo test --locked -p xtask` | PASS: 18 tests |
| `cargo fmt --check` | PASS |
| `cargo clippy --locked -p xtask --all-targets -- -D warnings` | PASS |
| `cargo clippy --locked --all-targets -- -D warnings` | PASS |
| `cargo test --locked` | PASS: 8 foundation tests, with local TCP bind permitted |
| `cargo xtask dependencies check` | PASS: exit 0; full output below |
| `bash -n scripts/sdk-go.sh` | PASS |
| `shellcheck scripts/sdk-go.sh` | PASS |
| `git diff --check` | PASS |

Integration skip output:

```text
=== RUN   TestBucketLifecycle
GCS_MIMIC_ENDPOINT is unset; bucket integration requires an explicit local emulator
--- SKIP: TestBucketLifecycle (0.00s)
PASS
```

The first full foundation test invocation inside the sandbox failed seven TCP
tests with `Os { code: 1, kind: PermissionDenied, message: "Operation not permitted" }`
at loopback bind. The same unchanged tests passed outside that sandbox. No test
was skipped or weakened to address that environment restriction.

## Online dependency check output

Checked at Unix timestamp 1792575474, exit 0:

```text
rust: CURRENT current=1.99.1 latest=1.99.1
http-body-util: CURRENT current=0.1.5 latest=0.1.5
hyper: CURRENT current=1.12.0 latest=1.12.0
hyper-util: CURRENT current=0.1.21 latest=0.1.21
serde_json: CURRENT current=1.0.154 latest=1.0.154
tokio: CURRENT current=1.54.0 latest=1.54.0
actions/checkout: CURRENT current=3d3c42e5aac5ba805825da76410c181273ba90b1 latest=3d3c42e5aac5ba805825da76410c181273ba90b1
actions/setup-go: CURRENT current=b7ad1dad31e06c5925ef5d2fc7ad053ef454303e latest=b7ad1dad31e06c5925ef5d2fc7ad053ef454303e
docker library/rust:1.99.1-slim-trixie: CURRENT current=sha256:00544ccc9431a2fe93e65185d21381b0a039342f5cefcf84fbddac273c9e573d latest=sha256:00544ccc9431a2fe93e65185d21381b0a039342f5cefcf84fbddac273c9e573d
docker library/rust:slim-trixie: CURRENT current=sha256:00544ccc9431a2fe93e65185d21381b0a039342f5cefcf84fbddac273c9e573d latest=sha256:00544ccc9431a2fe93e65185d21381b0a039342f5cefcf84fbddac273c9e573d
Debian stable codename: CURRENT current=trixie latest=trixie
docker library/debian:trixie-slim: CURRENT current=sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad latest=sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
docker docker/dockerfile:1: CURRENT current=sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e latest=sha256:4edf897a3ffa55b89f906fc8cc78afdb3f1834cc9c7083565e611a8a7d5fe99e
Go language baseline: CURRENT current=1.27.0 latest=1.27.0
Go toolchain: CURRENT current=1.27.3 latest=1.27.3
cloud.google.com/go/storage: CURRENT current=v1.69.0 latest=v1.69.0
google.golang.org/api: CURRENT current=v0.306.0 latest=v0.306.0
```

## Decisions and remaining verification

Use `go 1.27.0` with `toolchain go1.27.3`: Go removes an identical patch-level
toolchain directive and then refuses ordinary tests until tidy is applied. The
checker compares the language series and actual stable patch separately.
setup-go's pinned source selects the toolchain directive when `GOTOOLCHAIN` is
unset/auto. No Rust dependency was added. The generated API client requires an
auth pseudo-version transitively; the snapshot records this upstream constraint.

At the original harness-only checkpoint, the bucket server and `--data-dir` support were unavailable, so
`scripts/sdk-go.sh` and `TestBucketLifecycle` have not run end to end. The
integration task must run the script after merging the bucket implementation,
record real SDK evidence and update JSON-BUCKETS-001/002/005/006 in the ledger.
Remote CI, scheduled workflow execution, XML, gRPC, objects and ACL/IAM were not
tested. No real Google Cloud API or metadata endpoint was contacted by tests.
