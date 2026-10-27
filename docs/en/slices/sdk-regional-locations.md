# Regional and predefined dual-region bucket locations

<!-- markdownlint-configure-file {"MD013": false} -->

Feature: `JSON-BUCKETS-005` remains PARTIAL because broader bucket policies and advanced settings are pending. This slice supports location metadata only; it does not simulate physical placement, latency, pricing, replication or RPO.

## Acceptance contract

| Case | Required behavior |
| --- | --- |
| regional-create | Official regional names create buckets; insert, get and list return uppercase location and `region`. |
| case-insensitive | Lowercase and mixed-case names normalize to uppercase. |
| predefined-dual-region-create | Official predefined dual-region codes return `dual-region`. |
| multi-region-unchanged | `US`, `EU`, `ASIA` retain `multi-region`. |
| configurable-dual-region-unsupported | Nonempty `customPlacementConfig` uses the shared unsupported error; no bucket is published. |
| unknown-location-rejected | Unknown names fail with 400 `invalidArgument`, identifying location and valid examples `US`, `ASIA-NORTHEAST1`. |
| default-location-config | Configuration and bucket initialization share the same static names; omitted, null and empty location inputs use the default. |
| legacy-profile-unchanged | LegacyEmulator retains fixed `US-CENTRAL1` response behavior and existing input restrictions. |
| sdk-go | High-level `BucketHandle.Create` and `Attrs` support a regional and predefined dual-region bucket. |
| sdk-ruby | High-level `create_bucket` returns regional location and location_type. |

All ten scoped cases pass; the broad operation remains PARTIAL for its other pending cases.

## Sources and local choices

Checked on 2026-10-27: [Bucket locations](https://docs.cloud.google.com/storage/docs/locations), [Bucket resource](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets), [Storage v1 Discovery](https://storage.googleapis.com/$discovery/rest?version=v1) and the [official dual-region sample](https://docs.cloud.google.com/storage/docs/samples/storage-create-bucket-dual-region). The sample documents case-insensitive location inputs; the resource documents the default `US` and three supported location types. The static regional and predefined dual-region lists follow the locations page, rather than Discovery endpoint inventory.

Exact provider error wording, uppercase response normalization and null/empty normalization have no real-cloud differential evidence. These normalization/error choices are recorded under `SPEC-REGIONAL-LOCATIONS`; no cloud resources are created by this slice. In native insert, null/empty `customPlacementConfig` (`null`, `{}`, or `{"dataLocations":[]}`) is absent; every other placement configuration uses the shared unsupported error. Legacy input restrictions remain unchanged. PATCH/PUT relocation and configurable dual-regions remain unsupported; the XML adapter has no bucket-creation location parser to update.

The locations HTML snapshot has SHA-256 `1e121b586618b4aeac7b38c0e405bdaf6ba396f175320a1d1e61208621921c96`.
Storage v1 Discovery revision `20261020` has SHA-256 `eeb0831df501d131c9497534b2c3d130ea40f9fa215195115d011cb64e1a42bc`.

## Supported names

The shared static table contains 43 regions, six predefined dual-regions and three multi-regions. No table is fetched or built at startup.
Native administrative bucket definitions use this same table and require a canonical location/type pair, allowing initializer publication without weakening metadata validation. The catalog schema, transactions and startup order are unchanged.
Legacy `FAKE_GCS_LOCATION` remains an error, with migration advice selecting the corresponding uppercase native default (for example `US-CENTRAL1` or `ASIA-NORTHEAST1`). Unknown legacy values are redacted and receive valid examples; the legacy profile's fixed behavior is unchanged.

| Location type / area | Names |
| --- | --- |
| Region / North America | `NORTHAMERICA-NORTHEAST1`, `NORTHAMERICA-NORTHEAST2`, `NORTHAMERICA-SOUTH1`, `US-CENTRAL1`, `US-EAST1`, `US-EAST4`, `US-EAST5`, `US-SOUTH1`, `US-WEST1`, `US-WEST2`, `US-WEST3`, `US-WEST4` |
| Region / South America | `SOUTHAMERICA-EAST1`, `SOUTHAMERICA-WEST1` |
| Region / Europe | `EUROPE-CENTRAL2`, `EUROPE-NORTH1`, `EUROPE-NORTH2`, `EUROPE-SOUTHWEST1`, `EUROPE-WEST1`, `EUROPE-WEST2`, `EUROPE-WEST3`, `EUROPE-WEST4`, `EUROPE-WEST6`, `EUROPE-WEST8`, `EUROPE-WEST9`, `EUROPE-WEST10`, `EUROPE-WEST12` |
| Region / Asia | `ASIA-EAST1`, `ASIA-EAST2`, `ASIA-NORTHEAST1`, `ASIA-NORTHEAST2`, `ASIA-NORTHEAST3`, `ASIA-SOUTH1`, `ASIA-SOUTH2`, `ASIA-SOUTHEAST1`, `ASIA-SOUTHEAST2`, `ASIA-SOUTHEAST3` |
| Region / Middle East | `ME-CENTRAL1`, `ME-CENTRAL2`, `ME-WEST1` |
| Region / Australia | `AUSTRALIA-SOUTHEAST1`, `AUSTRALIA-SOUTHEAST2` |
| Region / Africa | `AFRICA-SOUTH1` |
| Predefined dual-region | `ASIA1`, `EUR4`, `EUR5`, `EUR7`, `EUR8`, `NAM4` |
| Multi-region | `US`, `EU`, `ASIA` |

New official names require a deliberate snapshot/table update. Geographic location codes used only for configurable dual-regions, such as `AU`, are not standalone supported locations.

## Verification

Implementation commits: `bce7bb3ef4cd51d9218c6d070c18dc17b0c377a4`, `a3074ab`, `cebd35b`.

- Rust toolchain: 1.99.1, edition 2024. No dependencies or lockfiles changed. `cargo xtask dependencies check`: PASS; all tracked Rust, Go, Ruby and container inputs CURRENT on 2026-10-27.
- `cargo check --workspace --all-targets --locked --offline`: PASS.
- `just check`: PASS on the final source, including formatting, host/cross-target clippy, all Rust tests, compatibility validation/generated views and offline script checks.
- `cargo test --locked --offline --test bucket_initialization`: PASS, 10 tests including regional and default predefined dual-region startup.
- `cargo test --locked --offline --lib bucket_definition_requires_matching_canonical_location_and_type`: PASS. The shared administrative boundary requires the canonical location/type pair and rejects unknown names, wrong types and noncanonical records.
- `cargo test --locked --offline --lib config_input::tests::legacy_location_advice_uses_effective_native_setting`: PASS; regional and predefined dual-region advice uses uppercase native settings and unknown values remain redacted.
- `cargo test --locked --offline --lib config_input::tests`: PASS, all 10 diagnostic tests, including mixed settings, unsafe-value redaction and property cases.
- `BUNDLE_FROZEN=true bundle check`: PASS using pinned Ruby 4.0.9 and Bundler 4.0.23.
- `just check-sdk` (HTTP): PASS. Go vet and the full Go suite pass, including regional, lowercase regional and NAM4 creation/Attrs; Ruby origin self-check, migration scenario and full permissive/strict journeys pass, including regional create/reload location and location_type.
- Markdown lint for the changed slice/configuration/migration documents: PASS, zero issues.

HTTPS SDK journeys, container builds, full guide journeys, benchmarks and real-cloud differential tests were not run: this slice changes location validation/metadata only and explicitly excludes those checks.
