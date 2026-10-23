# Bucket metadata PUT comparison and SDK verification

<!-- markdownlint-configure-file {"MD013": false} -->

Checked and executed on **2026-10-23** against final application, tests, SDK and
comparison source `c8d327a47446c0340796c87f06c6635059bc1db0`, from verification
checkout `ff084a91f8b26e67f46b6c36a040538c521bae86`.
The [final evidence](bucket-metadata-put.md) records environment and inputs;
the [complete raw comparison](bucket-metadata-put-comparison.txt) retains
requests, selected headers, exact response bodies and state probes.
The [PUT contract](../slices/bucket-metadata-put.md) defines local replacement
independently of PATCH and of other emulators.

## Pinned gcp-storage-emulator source review

Reviewed v2026.07.19, commit `8262eb4b95e219a89a24c93703225b727433a0e6`:

| Source | SHA-256 | Observed behavior |
| --- | --- | --- |
| [server.py](https://github.com/oittaa/gcp-storage-emulator/blob/8262eb4b95e219a89a24c93703225b727433a0e6/src/gcp_storage_emulator/server.py) | `b0227517548e0c34304b06029712351de277bd2a8bb1c7c45e71d0a27ec55837` | Bucket resource route registers GET, DELETE and PATCH; no PUT handler. PUT on the distinct IAM route is not buckets.update. |
| [handlers/buckets.py](https://github.com/oittaa/gcp-storage-emulator/blob/8262eb4b95e219a89a24c93703225b727433a0e6/src/gcp_storage_emulator/handlers/buckets.py) | `ce160c2cffdc84937d868a80ea91bed8141b5c2eae9cd623db228cf9fcfec01f` | `_patch_bucket` assigns supplied non-null fields; no bucket replacement handler. |

This supports the README's unavailable bucket PUT cell only. No execution
result or Google compatibility claim is made for this product.

## Pinned fake-gcs-server execution

Reference v1.56.1, commit `896ece51e49eed0946089f3089f6343855c0d106`,
source archive SHA-256
`13fa1ed9cff0216baedaedb162b3cf37575bba955fb55fd4b76acb9b818494dc`.
The harness covers filesystem and memory backends plus both local profiles.
`GCS_MIMIC_REFERENCE_REPO=https://github.com/fsouza/fake-gcs-server bash scripts/compare-reference-emulator.sh`
exited 0. All four targets executed 294 requests each: 1,176 raw records,
including 109 PUT-related requests per target (436 records), with 32 PUT
operations per target. Both PUT assertion groups and both inherited PATCH
assertion groups passed; all four normalized create/GET comparisons passed.

Every reference PUT returns the JSON 404 Not Found envelope, including malformed
bodies, missing buckets, unsupported queries and conditions. GET/list metadata
and existing object bytes remain unchanged. The post-upload baseline separates
filesystem upload changes to timestamps/default hold from PUT behavior.
There is no successful reference PUT from which to infer replacement semantics.

Both local profiles pass whole-map replacement, omitted/null/empty labels,
STANDARD/false omission defaults, native null-boolean rejection versus legacy
reset, null-class rejection, unsupported settings and queries, exact error reasons,
atomic conditions, one revision increment and PUT/GET/list equality. Identity,
placement, creation time, held-object metadata and bytes are preserved.
All expected failures preserve the preceding resource.

## Official Go transport boundary

Storage v1.70.0 `BucketHandle.Update` sends PATCH. PUT coverage uses API client
v0.307.0 `Buckets.Update`, with transport assertions in
`TestBucketMetadataReplace` and `TestStrictAuthBucketMetadataReplace`.
The full `just check-sdk` run exited 0: vet, 33 top-level tests and 73 subtests
passed, with no failures or skips (package time 3.203s). Both PUT tests passed
against isolated permissive and strict native servers; the comparison separately
exercised both compatibility profiles. The [SDK log](bucket-metadata-put-sdk-go.txt)
retains the complete run. Inherited cloud credential variables were cleared;
guarded local transports and fixture tokens prevent cloud/metadata-server use.

Local SDK success does not resolve SPEC-BUCKET-PUT defaults, nulls, revisions,
permissions, errors or propagation. The online dependency check detected API
v0.308.0 while this run retains the committed v0.307.0 graph; see the
[dependency log](bucket-metadata-put-dependencies.txt). No upstream emulator
test suite or real Google bucket was used.
