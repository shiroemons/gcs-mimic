# Bucket metadata PATCH comparison and SDK verification

<!-- markdownlint-configure-file {"MD013": false} -->

Checked and executed on **2026-10-23**, macOS ARM64, against reviewed application
source `2b4441cef21370f95cfb3506da1cbf1ebda09787`, merged into verification checkout
`3035d23f7b3a9c85a786441fc07c961d037c8eca`. Scope: the bucket PATCH
corpus for `JSON-BUCKETS-008`, separate from whole-method Google compatibility.
The [raw output](bucket-metadata-patch-comparison.txt) records the PATCH-related
requests, status, selected headers, exact response bodies and subsequent GET/list
probes, followed by the complete final Go SDK output. Local temporary-path prefixes
in reference filesystem error messages are replaced with `<comparison-work>` and
the checkout path in SDK build output with `<checkout>`; the file declares these
replacements. Subsequent changes only update verification prose and raw evidence.

## Inputs and reproduction

| Input | Pin |
| --- | --- |
| Reference emulator | v1.56.1; commit `896ece51e49eed0946089f3089f6343855c0d106` |
| Source archive SHA-256 | `13fa1ed9cff0216baedaedb162b3cf37575bba955fb55fd4b76acb9b818494dc` |
| Go toolchain | go1.27.3 |
| Official Storage SDK | v1.70.0; revision `9a2913189e552da51b0fd60aa635027426f04d71` |
| Official API SDK | v0.307.0 |
| Discovery revision | `20261013` |
| Discovery SHA-256 | `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0` |

Discovery was fetched on the check date from the [official endpoint](https://storage.googleapis.com/$discovery/rest?version=v1).
The slice-start dependency checker exited 0 and reported every tracked input
CURRENT, including the existing Go/SDK pins. No dependency files changed.

```sh
: "${GCS_MIMIC_REFERENCE_REPO:?Set the pinned reference repository from README comparison sources}"
bash scripts/compare-reference-emulator.sh
env -u GCS_MIMIC_GO_TEST_RUN bash scripts/sdk-go.sh
```

Optional `GCS_MIMIC_REFERENCE_ONLY=1` skips the local Rust build and local targets;
the final run above used all four targets with that option unset.
The script verifies the archive digest, builds below `$TMPDIR`, binds loopback,
bypasses proxies, clears inherited server configuration and cleans up all children
and data. This run contacted upstream source/module registries, never real
Google buckets or the metadata server. Upstream emulator test suites were not run.

The complete corpus has 185 requests per target: the prior 92 CRUD probes plus
93 PATCH-related requests (29 PATCH operations, two create operations and
GET/list state probes). Final execution completed with 740 raw records;
372 PATCH-related records are retained here. Both reference and local assertions
pass. All four create/get normalized-body comparisons also pass. Equality columns
are descriptive: documented cross-product differences do not fail the run.

## Executed behavior

| Request/case | Reference filesystem | Reference memory | gcs-mimic native and legacy-emulator |
| --- | --- | --- | --- |
| Label add, merge and key:null removal | 200; labels absent in response and GET | Same | 200; merge retains other keys; null removes only the selected key |
| Empty label map, whole-map null and empty string value | 200; labels absent | Same | Empty map preserves; null clears; empty string persists |
| `storageClass:"NEARLINE"` | 200; class stays STANDARD | Same | 200; NEARLINE persists through subsequent updates |
| Enable versioning and default hold together | 500 internalError; GET retains both false | 200; both true in response/GET | 200; both true in response/GET |
| Empty PATCH after enabling settings | 200; both false | 200; resets both false | 200; preserves both true |
| Disable both settings | 200; both false | Same | 200; both false |
| Default hold true, then false without versioning | 200; true then false in GET | Same | 200; true then false in GET |
| Metageneration match success/stale, not-match success/equal | All 200; conditions ignored | Same | Successes 200; stale match and equal not-match 412, unchanged state |
| Missing bucket PATCH | 500 internalError with filesystem stat diagnostic; subsequent GET 404 | 404 notFound; subsequent GET 404 | 404; subsequent GET 404 |
| Truncated JSON `{"labels":` | 400 invalid, `unexpected EOF`; state unchanged | Same | Native 400 parseError; legacy 400 invalid; state unchanged |
| Unknown field | 200; ignored | Same | 200; ignored, stored metadata preserved and revision advances |
| `cors`, `lifecycle`, `retentionPolicy`, `iamConfiguration` set to null | 200; no modeled effect | Same | 501; unchanged state |
| projection full/noAcl | 200; same modeled resource | Same | 200; same modeled resource |
| predefinedAcl, predefinedDefaultObjectAcl and userProject | 200; no modeled effect | Same | 501; unchanged state |

For every local successful PATCH, the script asserts response/GET/list equality,
exactly one metageneration increment, changed ETag, non-decreasing update time,
stable identity/creation time/location/project, and preservation of omitted class,
labels and boolean settings. Every failed local mutation is followed by GET/list
and compared with the complete previous resource. The missing bucket stays absent.
The error statuses above are expected observations, not harness failures.

Successful reference PATCH leaves metageneration `"1"`, ETag `"RVRhZw=="`
and `updated == timeCreated`, including real settings changes. Label/class inputs
cannot establish Google merge/null semantics because the reference ignores them.
The filesystem versioning limitation and missing-bucket 500 are recorded
comparison behavior, not required gcs-mimic errors. The memory settings capability
is the minimum baseline to preserve with durable local storage.

The pinned [reference bucket handler](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/bucket.go)
has SHA-256 `73ac2fded764470f13b451d281c61089704f48e79a0234cfdf164657d443cf96`.
`getBucketAttrsToUpdate` models only the two settings as non-pointer booleans:
omission resets them to false. The local contract deliberately preserves omitted
settings in both profiles. The executed empty PATCH after enablement exposes this
difference; the empty-string label value is independently preserved locally.

## Official Go SDK wire mapping

The pinned module origin metadata resolves tag `storage/v1.70.0` to the revision
above. Source inspected: [bucket.go](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/bucket.go)
(`BucketHandle.Update`, `BucketAttrsToUpdate.toRawBucket`, `SetLabel`, `DeleteLabel`)
and [http_client.go](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/http_client.go)
(`httpStorageClient.UpdateBucket`). Their file SHA-256 values are respectively
`cfaeeafa408ac664e88038a74eea89a2c639b46fe535621630418d5ac4fa84eb`
and `a74c0157e327096d641d4a8ca8b544ec6b2e4f8c9e64c463f5d8155e1cec50d0`.

All high-level updates use `PATCH /storage/v1/b/{bucket}?projection=full`.
The generated client's standard `alt=json`/`prettyPrint=false` parameters may
also appear. Conditions add `ifMetagenerationMatch` or
`ifMetagenerationNotMatch`; predefined ACLs and userProject are forwarded when
requested. The following JSON bodies spell the serialized fields exactly;
object key ordering and whitespace do not carry semantics.

| SDK operation | JSON body |
| --- | --- |
| SetLabel keep/one and remove/two | `{"labels":{"keep":"one","remove":"two"}}` |
| SetLabel add/three with DeleteLabel remove | `{"labels":{"add":"three","remove":null}}` |
| DeleteLabel only | `{"labels":{"keep":null}}` |
| StorageClass NEARLINE | `{"storageClass":"NEARLINE"}` |
| VersioningEnabled true/false | `{"versioning":{"enabled":true}}` / `{"versioning":{"enabled":false}}` |
| DefaultEventBasedHold true/false | `{"defaultEventBasedHold":true}` / `{"defaultEventBasedHold":false}` |
| Empty BucketAttrsToUpdate | `{}` |

DeleteLabel adds `Labels.<key>` to raw NullFields; delete-only updates also
force-send Labels, so removal is JSON null rather than an empty string.
SetLabel sends only changed keys, requiring merge preservation of other keys.
Explicit false booleans are force-sent; omitted optional booleans and empty
StorageClass are not sent. The high-level SDK has no whole-label-map-null setter;
that case belongs to the raw comparison corpus.

The [official PATCH reference](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/patch)
defines update permission and conditions; the [PATCH semantics](https://docs.cloud.google.com/storage/docs/json_api#patch)
and [bucket label guide](https://docs.cloud.google.com/storage/docs/using-bucket-labels)
define merge/key removal. [Bucket resource fields](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets),
[storage classes](https://docs.cloud.google.com/storage/docs/storage-classes) and
[status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes)
remain authoritative over emulator observations. Unknown/output-only/null/query
edge behavior must follow the separately frozen slice contract and recorded gaps;
no real-service differential parity is inferred from SDK serialization.

## Final local verification

The final `env -u GCS_MIMIC_GO_TEST_RUN bash scripts/sdk-go.sh` exits 0: Go vet,
31 top-level tests and 73 subtests pass, with zero failures or skips (package time
3.239s). This includes `TestBucketMetadataUpdate` and
`TestStrictAuthBucketMetadataUpdate`: high-level label additions/merge/removal,
delete-only/absent-key updates, class changes, explicit true/false settings,
both condition successes and 412 failures, subsequent Attrs consistency,
read-only bucket principal 403 with unchanged revision, admin success and
unchanged object bytes. With endpoints unset, the two tests compile and skip
explicitly; that harness-only check is separate from the successful server run.

`bash scripts/compare-reference-emulator.sh` exits 0 with all four targets and
both PATCH assertion groups passing. `bash -n`, `git diff --check` and the
local-path scan pass. Markdownlint passes on the three changed Markdown files;
MD013 alone is disabled for existing long tables and reproduction commands.

Local `updated` uses the injected clock and millisecond JSON precision. The
tests require non-decreasing time, no time before creation, metageneration +1,
a changed ETag and actual metadata changes. Consecutive successful updates can
share `updated`; timestamp inequality is not used as the mutation identifier.

The [slice contract](../slices/bucket-metadata-patch.md) retains unverified
Google-service behavior and profile carry-over limits. The corpus covers the
listed fields/queries; it does not claim whole-method parity, cloud IAM error
parity, physical storage placement, restart coverage from this comparison,
container validation or upstream test-suite execution. Rust wire/restart tests,
ledger updates and final container/guide evidence belong to the integrated slice.
