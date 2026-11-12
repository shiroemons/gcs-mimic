# Bucket storage layout and folder prefixes for gcloud storage

<!-- markdownlint-configure-file {"MD013": false} -->

Features: `JSON-BUCKETS-004` (`buckets.getStorageLayout`) and the `includeFoldersAsPrefixes` parameter of `JSON-OBJECTS-008` (`objects.list`) and gRPC v2 `ListObjects`. Google Cloud SDK 591.0.0 sends both on its ordinary paths: `gcloud storage cp` and `cat` read the storage layout of every source and destination bucket before choosing a client, and `gcloud storage ls gs://BUCKET` lists with `delimiter=/&includeFoldersAsPrefixes=True`. Before this slice both answered 400 badRequest, so no `gcloud storage cp`, `cat` or bucket listing worked against gcs-mimic.

## Acceptance contract

| Case | Required behavior |
| --- | --- |
| layout-resource | `GET /storage/v1/b/BUCKET/storageLayout` returns `kind: storage#storageLayout`, `bucket`, `location` and `locationType`, equal to the bucket resource's location fields for the same profile. |
| layout-no-namespace-or-placement | `hierarchicalNamespace` and `customPlacementConfig` are absent: hierarchical namespace is unsupported and native insert stores no custom placement. Clients read an absent `hierarchicalNamespace` as disabled. |
| layout-parameters | `prefix` is accepted and does not change the response; `alt=json`, `prettyPrint` and `fields` (validated against `BucketStorageLayout`) apply; `projection`, unknown parameters and invalid fields return 400 `invalidParameter`; `userProject` and `alt=media` keep the shared 400 badRequest rejection. |
| layout-errors | A missing bucket returns 404 `notFound`; an invalid bucket name 400 `invalidParameter`; a request body 400 `invalidArgument`; `POST`, `PUT`, `PATCH` and `DELETE` 405 with `Allow: GET`. |
| layout-permission | Strict mode requires `storage.objects.list` on the bucket (the reference names no `storage.buckets.*` permission): `roles/storage.objectViewer` succeeds, `roles/storage.objectCreator` and an ungranted principal get 403, an anonymous caller 401, and a missing bucket answers as `buckets.get` does for the same caller. |
| layout-legacy-profile | The fake-gcs-server profile reports its fixed `US-CENTRAL1`/`region`, matching its bucket resource. |
| folders-as-prefixes-slash | `includeFoldersAsPrefixes=true` with `delimiter=/` returns the same items, prefixes and page tokens as the listing without it, alone and combined with `prefix` and `includeTrailingDelimiter`. |
| folders-as-prefixes-delimiter | `includeFoldersAsPrefixes=true` without a delimiter, with an empty one or with any delimiter other than `/` returns 400 `invalidParameter`. Omitted/false is unchanged. |
| folders-as-prefixes-grpc | gRPC v2 `ListObjects` applies the same rule through the same check: equal responses with delimiter `/`, `INVALID_ARGUMENT` otherwise. A nonempty `filter` stays `UNIMPLEMENTED`. |
| sdk-probes-kept | The SDK fail-fast probes that used `includeFoldersAsPrefixes` now send Requester Pays `userProject`, which stays a recognized unsupported parameter (`EXT-BEH-045` is NOT_STARTED); each still proves 400 badRequest without retries and a gcs-mimic diagnostic. |

## Sources and local choices

Checked on 2026-11-02: the [getStorageLayout reference](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/getStorageLayout) and the [objects.list reference](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/list), both last updated 2026-10-30, and the pinned Discovery `BucketStorageLayout` schema in `compatibility/upstream/json-schemas.json`.

- The reference requires `storage.objects.list` and says `prefix` makes that check apply to the given prefix. Local IAM rejects bindings with conditions, so every grant covers the whole bucket and the bucket-level check decides every prefix. The value is accepted and not stored.
- The reference describes `locationType` as "This property has a value of `region`." gcs-mimic reports the bucket's own location type instead (`multi-region` for the default `US` bucket, `region` for a regional one), matching the bucket resource. This conflict is a known deviation, unverified until a live recording exists.
- The reference response lists `kind`, `bucket`, `location`, `locationType` and `hierarchicalNamespace`; Discovery also defines `customPlacementConfig`. Omitting `hierarchicalNamespace` for a bucket without it is a local choice; no Cloud Storage response for a flat bucket was recorded.
- The objects.list reference says `includeFoldersAsPrefixes` adds empty folders and managed folders to `prefixes[]` and that `delimiter` must be `/` with it. It names no error code for other delimiters; 400 `invalidParameter` matches the local `matchGlob` delimiter rule. Local buckets hold neither folders nor managed folders, so the flag cannot add a prefix.
- No live Cloud Storage recording covers either method; the error codes above are local choices, and `JSON-BUCKETS-004` stays PARTIAL until a differential recording exists.

## Verification

On 2026-11-02 with Rust 1.99.1:

- `cargo check --all-targets`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` and `cargo xtask lint`: PASS (0 production violations).
- `cargo test --locked --offline --test bucket_storage_layout_http --test object_list_http --test unsupported_http`: PASS, 14 tests.
- `cargo test --locked --offline --lib`: PASS, 1077 tests, including the gRPC v2 folders-as-prefixes case and the pinned unsupported-route classification.
- `bash scripts/test-rust.sh`: the 71 other workspace test binaries and the doctests PASS.
- `cargo xtask differential run --reference compatibility/differential/reference/gcs.json`: 59 of 59 recorded cases match.
- `bash scripts/sdk-go.sh --with-ruby --with-node --with-python --with-php`: PASS. Go (Storage v1.70.0), Ruby (1.62.1), Node.js, Python (3.17.1) and PHP (v2.6.1) suites, permissive and strict, including each migrated fail-fast probe.
- `cargo xtask compatibility check`: PASS.
- gcloud 591.0.0 with an isolated configuration: `gcloud storage cat`, `gcloud storage cp gs://BUCKET/OBJECT FILE`, `gcloud storage ls gs://BUCKET` and `ls -l` now succeed; the storage layout request answers 200.
