# SDK scorecard: more shared cases

<!-- markdownlint-configure-file {"MD013": false} -->

Status: implemented on 2026-11-15; see "Verification".

Scope: the optional comparison harness under `sdk-scorecard/`. Five cases
join the shared core (`bucket`, `object` and `signed` areas) in all nine
SDKs, and three .NET `sdk.*` cases that measure the same thing move to the
shared IDs. No configuration or ledger status changes; the one server change
fixes a defect the new cases exposed (see "Found during implementation"). The
scorecard stays a comparison, and `compatibility/features.json` remains the
only status ledger.

Out of scope, each a later slice: gRPC cases per SDK, the move of Java's
`sdk.grpc_*` cases to the `grpc` area, new SDK-specific cases, and any server
change a new case motivates.

## Choice of cases

The candidates were behaviors the ledger records as implemented or partial
that no scorecard case measured. The pinned SDKs' public APIs were read for
each; a candidate joins the shared core only when most SDKs can express it,
because a core case that most SDKs mark `n/a` measures little.

| Candidate | Outcome |
| --- | --- |
| CSEK rotation, compose preconditions, V4 signed resumable start, V4 response overrides | Shared core: every SDK has a public API. |
| Bucket soft delete and restore | Shared core: five SDKs have a public API; Go, Java, Rust and C++ mark it `n/a`, which tells users of those SDKs something true. |
| Resumable resume and cancel, PUT replacement, bounded rewrite continuation | Not shared: at most four SDKs expose each. They are candidates for the SDK-specific slice. |

## Cases

| ID | Area | Checks |
| --- | --- | --- |
| `bucket.soft_delete_restore` | bucket | A bucket created with a 604800-second soft-delete policy and then deleted returns 404 on a normal get; a soft-deleted lookup finds it with the generation it had before deletion; restoring that generation makes a normal get return it again. |
| `object.csek_rotation` | object | An object written with customer-supplied key A and rewritten onto itself with source key A and destination key B reads back its bytes with key B; reads with key A and without a key then fail with HTTP error responses, not transport failures. |
| `object.compose_preconditions` | object | In a versioned bucket, p1 written as `one` and overwritten as `ONE`, and p2 written as `two`: composing p1 pinned to its first generation and p2 pinned to its generation gives the bytes `onetwo`; repeating the compose with `ifGenerationMatch` set to the destination's generation plus one fails with 412 and leaves its generation and bytes unchanged; repeating it with `ifGenerationMatch` equal to the destination's generation succeeds with a new generation and the bytes `onetwo`. |
| `signed.v4_resumable_upload` | signed | A V4 URL signed for POST with the `x-goog-resumable: start` header, sent unchanged with that header, returns 201 and a session URI on the same origin; a PUT of all the bytes to that URI completes the upload, and the object reads back those bytes. |
| `signed.v4_response_overrides` | signed | A V4 signed GET URL whose signed query sets `response-content-disposition` to `attachment; filename="report.csv"` and `response-content-type` to `text/csv`, sent unchanged, returns 200 with the object's bytes and exactly those two header values; the object's stored `contentType` stays `text/plain`. |

A probe may check more, never less. The signed cases send the URL through the
probe's own guarded HTTP client, as the existing signed cases do: exact
origin, no redirects, the scorecard CA.

### Unsupported

| SDK | `bucket.soft_delete_restore` reason |
| --- | --- |
| Go | `cloud.google.com/go/storage v1.70.0 has no public API for soft-deleted buckets (BucketHandle has no SoftDeleted or Restore)` |
| Java | `google-cloud-storage 2.78.0 has no public API for restoring a soft-deleted bucket (Storage.restore takes only a BlobId)` |
| Rust | `google-cloud-storage 1.22.0 has no public API for restoring a soft-deleted bucket (StorageControl has restore_object only)` |
| C++ | `google-cloud-cpp 3.14.0 has no public API for restoring a soft-deleted bucket (Client has RestoreObject only)` |

Each reason was checked against the pinned release: `go doc` of
`BucketHandle`, `javap` of `Storage` and its option classes, and the sources
of the Rust crate and of google-cloud-cpp. Every other SDK and case has a
public API.

### Known request differences

Recorded as `variants` in the catalog, or in `notes.rust`:

- Rust rewrites and composes through StorageControl gRPC; `notes.rust` now
  names both, and its CSEK rotation sends the keys as gRPC request fields.
- .NET composes through `client.Service.Objects.Compose`, as in
  `object.compose`, and starts the signed resumable upload with the SDK's
  `SignedUrlResumableUpload`, which sends the bytes in 256 KiB chunks.
- PHP signs through `bucketBoundHostname`, as in the other signed cases, and
  signs the resumable start through `signedUrl`, because `signedUploadUrl`
  drops that option.
- Node.js finds the deleted bucket through a soft-deleted listing: its
  `getMetadata` takes neither `softDeleted` nor `generation`.

## Moved .NET cases

| From | To |
| --- | --- |
| `sdk.csek_rotation` | `object.csek_rotation` |
| `sdk.signed_url_resumable_upload` | `signed.v4_resumable_upload` |
| `sdk.bucket_soft_delete_restore` | `bucket.soft_delete_restore` |

Each .NET implementation keeps the checks it already makes beyond the shared
ones, for example reading with a client whose default key is the new key, or
restoring the bucket's object after the bucket.

## Expected results

From the gcs-mimic sources, before running:

- gcs-mimic implements every new case over JSON and XML.
- gcs-mimic rejects customer-supplied keys on gRPC `RewriteObject`, as
  [the gRPC update and copy slice](grpc-v2-update-copy.md) documents, so the
  Rust probe's `object.csek_rotation` is expected to fail against it. The case
  stays failed and is reported; the server change is a separate slice.

Case counts become: Go 62, Ruby 56, Node.js 56, PHP 56, Python 56, Java 75,
.NET 73, Rust 56 and C++ 56. Every SDK's shared core grows from 51 to 56; the
.NET `sdk` area shrinks from 20 to 17.

## Acceptance

| Case | Required behavior |
| --- | --- |
| SC-CATALOG | `cases.json` defines the five cases with the checks above, the four unsupported reasons, and no `sdk.csek_rotation`, `sdk.signed_url_resumable_upload` or `sdk.bucket_soft_delete_restore`; `score.sh --check-catalog` passes. |
| SC-PROBES | Every probe emits each new case it supports through the SDK's public API and asserts every check; the four unsupported SDKs emit `bucket.soft_delete_restore` with their exact reason. |
| SC-DOTNET | .NET emits the three moved cases under the shared IDs and keeps its additional checks. |
| SC-DOCS | The Areas table and the unsupported-case list in `sdk-scorecard/README.md` match the catalog; `test-score.sh` passes. |
| SC-RESULTS | All four products are rerun into a new results directory. Each new case's result is reported per product, and every score change is explained by the new cases. A case gcs-mimic fails stays failed and is reported with its cause. |

## Decisions

1. `bucket.soft_delete_restore` joins the shared core with four `n/a` SDKs
   rather than staying a .NET-only case.
2. The compose case pins source generations instead of using per-source
   `ifGenerationMatch`, because Node.js and PHP expose only the generation.
   Its destination preconditions are non-zero generations: PHP's
   `Bucket::compose` (google/cloud-storage 2.6.1) passes its options through
   `array_filter`, which drops `ifGenerationMatch => 0`, so a
   create-only-if-absent compose cannot be sent from PHP. A matching
   generation must succeed as well as a stale one fail, so a server that
   answers 412 to any precondition fails the case.
3. The response-override case reads the stored `contentType` as well, so a
   server that rewrites the object's metadata to the override fails.
4. The Ruby bucket-restore defect is fixed in this slice, because it is a
   one-method inconsistency with object restore. Customer-supplied keys on
   gRPC `RewriteObject` are a feature the gRPC slice declared unsupported, so
   Rust's `object.csek_rotation` stays failed until a server slice adds them.

## Found during implementation

- **gcs-mimic defect, fixed.** google-cloud-storage 1.62.2 for Ruby sends
  `restore_bucket` with a form-encoded body that repeats the URL query.
  Bucket restore required an empty body and answered 400, while object
  restore and retention lock accept that body. Bucket restore now runs the
  same bodyless-POST check (`242076e9`); an identical form body passes, a
  changed or extra parameter and any JSON body, including `{}`, still return
  400. JSON-BUCKETS-010 records the rule.
- **PHP SDK limitation.** `Bucket::compose` drops `ifGenerationMatch => 0`
  (Decision 2), so the compose case uses non-zero generations in every SDK.
- **Probe idioms.** Java's `ComposeRequest.Builder` accumulates target
  options, so each compose gets a fresh builder; Go gained the `unsupported`
  marker the other probes already had.

## Verification

Verified on 2026-11-15 at `43d84560`, linux/arm64, with no uncommitted
change, after the branch was rebased onto main.

| Case | Result | Evidence |
| --- | --- | --- |
| SC-CATALOG | PASS | `bash sdk-scorecard/score.sh --check-catalog sdk-scorecard/cases.json` exits 0; the catalog has the five cases, the four reasons and none of the three moved `sdk.*` IDs. |
| SC-PROBES | PASS | Every probe emits exactly its catalog IDs and reasons; `score.sh` checked all 36 result files of [results/2026-11-15-4](../../../sdk-scorecard/results/2026-11-15-4/summary.md) against the catalog. The probes were read against every check; against gcs-mimic every new case passes except Rust's `object.csek_rotation`. |
| SC-DOTNET | PASS | .NET emits `bucket.soft_delete_restore`, `object.csek_rotation` and `signed.v4_resumable_upload` and no longer the three `sdk.*` IDs; its `sdk` area has 17 cases, and the moved cases keep the default-key client read, the object restore after the bucket and the chunk log. |
| SC-DOCS | PASS | `bash sdk-scorecard/test-score.sh` passes, including the Areas table check. |
| SC-RESULTS | PASS | All four products ran in 17 min 50 s, including the gcs-mimic release build. Every case shared with [results/2026-11-15-3](../../../sdk-scorecard/results/2026-11-15-3/summary.md) has the same result for all four products (2,016 cases then, 2,184 now: 180 new results and 12 removed `sdk.*` results). |

Results of the new cases, by SDK that passes. Go, Java, Rust and C++ report
`bucket.soft_delete_restore` as n/a for every product.

| Case | gcs-mimic | fake-gcs-server | gcp-storage-emulator | floci-gcp |
| --- | --- | --- | --- | --- |
| `bucket.soft_delete_restore` | 5 of 5 | 0 of 5 | 0 of 5 | 0 of 5 |
| `object.csek_rotation` | 8 of 9 (Rust fails: gRPC rejects the keys) | 0 of 9 | 0 of 9 | 0 of 9 |
| `object.compose_preconditions` | 9 of 9 | 0 of 9 | 0 of 9 | 1 of 9 (Rust, over gRPC; its REST compose ignores the pinned generation and returns `ONEtwo`) |
| `signed.v4_resumable_upload` | 9 of 9 | 0 of 9 | 0 of 9 | 0 of 9 |
| `signed.v4_response_overrides` | 9 of 9 | 7 of 9 (Ruby: its resumable upload stored `contentType` `application/json`, as in `object.metadata_on_write`; Rust: its gRPC metadata read fails, as in every Rust case) | 0 of 9 | 9 of 9 |

Scores, with the third 2026-11-15 comparison in parentheses: gcs-mimic 99.8
(100), floci-gcp 74.4 (78.3), fake-gcs-server 38.9 (40.5) and
gcp-storage-emulator 35.9 (38.7). The Core scores are 99.8, 74.6, 38.5 and
36.3.

Also passed: `cargo xtask lint`, `cargo clippy --all-targets` and the
bucket soft-delete integration tests for the server fix, whose new form-body
test fails without it; `go vet`, `ruby -c`, `node --check`, `php -l`, the Java,
.NET, Rust and C++ builds (`cargo clippy -D warnings` for the Rust probe,
`-Wall -Wextra -Wpedantic` for C++) and Markdown lint of the changed
documents. The C++ signature-verification run (CPP-SIGNED-VERIFY in
AUDIT-003) predates the two new signed cases and was not repeated.
