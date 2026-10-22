# Bucket metadata PATCH reference comparison

<!-- markdownlint-configure-file {"MD013": false} -->

Checked and executed on **2026-10-23**, macOS ARM64. Scope: the bucket PATCH
corpus for `JSON-BUCKETS-008`, separate from whole-method Google compatibility.
The [raw output](bucket-metadata-patch-comparison.txt) records the PATCH-related
requests, status, selected headers, exact response bodies and subsequent GET/list
probes. Only local temporary-path prefixes in reference filesystem error messages
are replaced with `<comparison-work>`; the file declares this replacement.

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
GCS_MIMIC_REFERENCE_ONLY=1 bash scripts/compare-reference-emulator.sh
```

Reference-only mode skips the local Rust build and local targets. Unset
`GCS_MIMIC_REFERENCE_ONLY` to build/run both gcs-mimic profiles as well.
The script verifies the archive digest, builds below `$TMPDIR`, binds loopback,
bypasses proxies, clears inherited server configuration and cleans up all children
and data. This run contacted upstream source/module registries, never real
Google buckets or the metadata server. Upstream emulator test suites were not run.

The complete corpus has 179 requests per target: the prior 92 CRUD probes plus
87 PATCH-related requests (27 PATCH operations, two create operations and
GET/list state probes). Reference-only execution completed with 358 raw records;
174 PATCH-related records are retained here. Reference assertions pass for
statuses, ignored label/class input, unchanged revisions, both backends' settings
and default hold toggles. Equality columns are descriptive: a documented
difference does not fail the run.

## Executed reference behavior

| Request/case | Filesystem backend | Memory backend |
| --- | --- | --- |
| Label add, merge, key:null removal, empty object and whole-map null | 200; labels absent in response and GET | Same |
| `storageClass:"NEARLINE"` | 200; class stays STANDARD | Same |
| Enable versioning and default hold together | 500 internalError; GET retains both false | 200; both true in response/GET |
| Disable both settings | 200; both false | Same |
| Default hold true, then false without versioning | 200; true then false in GET | Same |
| Metageneration match success/stale, not-match success/equal | All 200; conditions ignored | Same |
| Missing bucket PATCH | 500 internalError with filesystem stat diagnostic; subsequent GET 404 | 404 notFound; subsequent GET 404 |
| Truncated JSON `{"labels":` | 400 invalid, `unexpected EOF`; state unchanged | Same |
| Unknown field | 200; ignored | Same |
| `cors`, `lifecycle`, `retentionPolicy`, `iamConfiguration` set to null | 200; no modeled effect | Same |
| projection full/noAcl | 200; same modeled resource | Same |
| predefinedAcl, predefinedDefaultObjectAcl and userProject | 200; no modeled effect | Same |

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
settings in both profiles. The final corpus adds an empty PATCH after enablement
to expose this difference, plus an empty-string label value case.

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

## Local verification status

`GOTOOLCHAIN=auto go vet ./...` passes. The two new SDK tests compile and skip
explicitly when emulator endpoints are unset. Against application source
`406bd7185c86c27f74480d8d87ecde31a43fb6a9`, the narrow PATCH SDK runner passes
both new tests with no skips (package time 0.474s), including strict read-only
denial and admin success. Final complete Go execution and native/legacy-profile
comparison await the reviewed application source.

Local `updated` uses the injected clock and millisecond JSON precision. The
tests require non-decreasing time, no time before creation, metageneration +1,
a changed ETag and actual metadata changes. Consecutive successful updates can
share `updated`; timestamp inequality is not used as the mutation identifier.
