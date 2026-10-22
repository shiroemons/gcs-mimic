# Bucket metadata PUT comparison boundaries

<!-- markdownlint-configure-file {"MD013": false} -->

Source review checked on **2026-10-23**. Final-source execution evidence remains
pending; this document records the pinned inputs and source boundary.
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

## Pinned fake-gcs-server execution gate

Reference v1.56.1, commit `896ece51e49eed0946089f3089f6343855c0d106`,
source archive SHA-256
`13fa1ed9cff0216baedaedb162b3cf37575bba955fb55fd4b76acb9b818494dc`.
The harness covers filesystem and memory backends plus both local profiles.
Final integration will record absent-route JSON 404 and preservation probes,
raw output and assertion results.

## Official Go transport boundary

Storage v1.70.0 `BucketHandle.Update` sends PATCH. PUT coverage uses API client
v0.307.0 `Buckets.Update`, with transport assertions in
`TestBucketMetadataReplace` and `TestStrictAuthBucketMetadataReplace`.
Final integration must record their complete SDK run. Local SDK success does not
resolve SPEC-BUCKET-PUT defaults, nulls, revisions, errors or propagation.
