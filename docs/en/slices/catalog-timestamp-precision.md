# Catalog timestamp precision

<!-- markdownlint-configure-file {"MD013": false} -->

This Shared state slice owns schema 14 and the storage prerequisite for exact source microsecond timestamps. It does not add an imported-identity entry point or implement an importer. AUDIT-014 and DESIGN-073 retain their broader pending acceptance cases.

## Acceptance cases

| Case | Required behavior |
| --- | --- |
| catalog-timestamp-migration | Open a version 13 catalog containing pre-epoch, epoch, fractional-second and minimum/maximum representable millisecond values; multiply each selected value by 1000 and commit version 14. Preserve JSON resources and ETags byte for byte within the existing RFC 3339 range. |
| catalog-timestamp-rollback | Fail after bucket conversion but before object conversion; both tables and user_version remain unchanged and retry succeeds. Reject integer overflow before conversion without SQLite REAL promotion. |
| catalog-timestamp-version | Preserve rejection of unknown future versions; a reader limited to version 13 rejects version 14 before mutation. |
| catalog-timestamp-writes | Inject microsecond and nanosecond fractions; bucket/object metadata and retirement times persist floor-to-microsecond values, including pre-epoch values where already supported. Media/resumable/derived publication preserves microseconds for nonnegative clock values and retains its existing 503 rejection of pre-epoch clocks. JSON still floors to milliseconds. |
| catalog-timestamp-interactions | Preserve ETags and derived numeric fields, generation allocation, permission state and internal session deadlines. Conditional date headers and retention remain explicitly unsupported where they were unsupported before. |
| source-timestamp-contract | Remove the lossy ms mode; default Created/Updated preservation is exact through microseconds. Block only nonzero digits below microsecond precision, invalid times and unsupported source semantics; state the exact lossless subset. |

## Column inventory

| Table | Columns | Schema 14 unit |
| --- | --- | --- |
| buckets | time_created, updated | Signed Unix microseconds |
| objects | time_created, updated, nullable time_deleted | Signed Unix microseconds |
| resumable_sessions | created, deadline | Unchanged signed Unix milliseconds |
| derived_transfers | created, deadline | Unchanged signed Unix milliseconds |
| rewrite_sessions | deadline | Unchanged signed Unix milliseconds |
| object_generation / objects | last_generation / generation | Unchanged generation identities; never scale |

Cached resumable/rewrite JSON responses, conditions, hashes, metagenerations, policies, seed state and all other columns are unchanged. In-memory business times remain SystemTime; conversion occurs at the catalog boundary. Network/queue timeouts keep their existing units.

## Migration and failure handling

The timestamp migration is pinned to its own schema version 14 and does not run again when that catalog is reopened, even after the latest supported schema version advances.

After schema validation and before recovery/readiness, a single startup transaction checks that every selected non-null integer lies in -9223372036854775..=9223372036854775 milliseconds, updates both tables and sets user_version to 14. Null deletion times stay null. Any SQL, conversion or commit failure aborts startup; transaction rollback retains version 13 and its values, allowing a later retry. This is the only full-table timestamp rewrite; ordinary mutations retain short transactions. Older supported schemas first use their existing migrations to reach schema 13.

Range validation uses one EXISTS scan per table. Migration duration and WAL growth are unmeasured; the WAL can temporarily grow by roughly the objects table size, including rewritten index pages. Reserve disk space for the stopped-data backup and migration; disk-full failure must not be treated as permission to reset the catalog. No migration throughput or resource-budget result is claimed.

Back up the complete stopped data directory before upgrading. There is no automatic downgrade: an older executable rejects schema 14, and recovery requires the pre-upgrade backup plus its matching executable. Do not reset the catalog or divide timestamps in place to force a downgrade; post-upgrade writes may contain microseconds that cannot be represented by schema 13.

## Wire representation and import handoff

The owner selected microsecond persistence and retained the existing API representation. The [Google object resource](https://docs.cloud.google.com/storage/docs/json_api/v1/objects) and [bucket resource](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets) define RFC 3339 datetime fields; these references were checked on 2026-10-24. The emulator continues its three-fractional-digit rendering with floor toward negative infinity, including pre-epoch instants. This is an emulator compatibility choice, not a claim that RFC 3339 requires milliseconds. XML HTTP-date rendering and its existing supported range remain unchanged. Bucket ETags and other time-derived values still use Unix milliseconds; object ETags still use generation/metageneration.

shared-imported-identity must pass validated source Created/Updated instants without prior millisecond truncation. Import must reject a value with nonzero sub-microsecond digits instead of silently truncating it. Supported API clock values are floored to microseconds, including pre-epoch nanoseconds on bucket/metadata paths. Source timestamp parsing and imported identity publication remain owned by their later slices.

The catalog deliberately floors timestamps on write; it cannot establish source precision-losslessness. The future import entry point must check for a nonzero remainder below one microsecond before calling the catalog. This slice adds neither that entry point nor an exact-conversion helper.

Generation allocation retains its existing requirement that a publication clock be at or after the Unix epoch: put, resumable finalization and derived publication reject pre-epoch clocks with 503 before allocating a generation. Signed timestamp conversion remains available for existing migrated values and the bucket/metadata/retirement paths that already supported them. XML keeps its existing pre-epoch updated-time rejection and HTTP-date rendering. Neither generation allocation nor XML's supported range expands in this slice.

Milliseconds and microseconds still use signed i64 values internally; no newtypes are introduced. Unit-specific conversion names (`system_time_millis`, `system_time_micros`, `unix_millis`, `unix_micros`) and shared fixture conversion prevent the identified mix-ups, but the compiler does not enforce units.

## Verification

Verified on 2026-10-24 with Rust 1.99.1, edition 2024. Acceptance and pending scope are recorded in compatibility/features.json.

| Command or test | Result |
| --- | --- |
| `just check` | PASS on the final source: pinned format, Clippy, full workspace tests, compatibility and offline shell/container-publication checks |
| `cargo test --locked --offline timestamp --lib --test object_versioning_http` | PASS: timestamp conversion, migration, pre-epoch rejection and HTTP resource/header comparison cases |
| `cargo test --workspace --locked --offline --test catalog --test object_storage --lib` | PASS: catalog, object storage and both workspace library suites |
| `cargo test --locked --offline --test object_versioning_http` | Existing generation migration and allocator preservation plus versioned JSON GET/XML HEAD timestamp migration |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS |
| `cargo xtask compatibility render` / `cargo xtask compatibility check` | PASS: 591 feature rows, 401 parameter locations and 357 schema fields; both language views regenerated |
| `npx --offline --yes markdownlint-cli2` on the changed slice and planning documents | PASS with existing per-file rules |
| Markdown lint on generated English/Japanese matrices | PASS with MD013 disabled for existing long table rows and MD024 restricted to sibling headings for repeated categories under different parents |
| `bash scripts/verify-source-storage-contract.sh` | PASS: 14 source Git blobs, YAML and Go MIME sources, pinned mailcap source mirror and APK checksums |
| `cargo xtask dependencies check` | Exit 1: all Rust/compiler inputs CURRENT; existing distroless runtime image reports UPDATE_AVAILABLE, outside this timestamp slice |

No importer, official SDK suite, Docker build, real-cloud differential test, Windows execution or performance measurement was run for this slice. Exact source-time parsing/publication remains pending in import-dry-run and shared-imported-identity; schema storage alone does not establish a lossless import.

Rollback coverage includes a full version 13 catalog file with policies, objects and an active resumable session. A TEMP trigger on its migration connection aborts the objects UPDATE after bucket conversion; a second connection checks unchanged on-disk version and values. Removing the trigger and reopening through Catalog::open completes migration and preserves resources, policy and session state. A persistent trigger would be rejected by schema-inventory validation before migration, so no production injection point or relaxed schema validation is added.
