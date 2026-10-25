# Imported object identity

This slice implements the `onboarding-state-contract` subset of `DESIGN-072`.
It adds trusted administrative publication with an explicit positive generation,
creation/update times and mandatory MD5/CRC32C verification, using the existing
bounded upload, authorization, precondition, retirement and recovery path.

## Acceptance

- Preserve generation, distinct Created/Updated timestamps through microseconds,
  verified MD5/CRC32C and bytes, including restart and generation-selected reads.
- Reject timestamps with nonzero sub-microsecond precision before staging.
  Signed, representable pre-epoch source times are accepted; ordinary uploads
  retain their existing pre-epoch clock restriction.
- Reject an existing bucket/name/generation before retiring the live version.
  Same generation values on different object names are valid independent identities.
- Advance the durable allocator to the maximum of its previous value and the
  imported generation; subsequent automatic publication never reuses that value,
  including backward clocks, restart and exhaustion at the signed integer limit.
- Reject mismatched source hashes without changing object state or the allocator.
  Preserve ordinary upload conditions, holds and versioning behavior.
- Reject non-Replace modes on imported publication before staging, so neither
  hash-based skipping nor metadata inheritance can discard explicit source identity.

The imported generation and times are trusted local input, not an HTTP option.
The caller uses `AdminGrant` via the administrative interface; no IAM grant or
public access is created. Importer orchestration, batch publication, file staging
and import journals remain separate slices. No schema change is required.

## Verification

Acceptance was recorded before implementation. Code and four focused tests are
committed as `c9d545d`, with shared administrative validation in `9b47372`.
Verification uses the pinned Rust 1.99.1 and edition 2024
on macOS ARM64, 2026-10-25. `cargo xtask dependencies check` passed with every
checked input CURRENT; no dependencies or pins changed.

`cargo test --locked --offline imported_identity --lib` passed all four tests.
They cover distinct exact microsecond times (including pre-epoch source data),
signed timestamp bounds and precision rejection, historical/live generation
collisions, concurrent duplicate publication, generation-selected bytes and
metadata across restart, stale conditions, lower-generation imports without
allocator regression, automatic allocation after a backward clock, allocator
exhaustion, both checksum mismatches, missing hashes and staging/blob cleanup.
`cargo test --locked --offline --lib storage::catalog::admin::` passed all
24 tests, including the four imported-identity cases and 20 existing management
tests. Both publication entry points use the same validation, admission,
precondition and mode-selection helper.

Before integration, `just check` passed on isolated source `e06a8b5`:
formatting, Clippy, all
workspace tests (including 363 library tests), compatibility validation and
offline shell/publication checks. `cargo xtask compatibility render` regenerated
the English and Japanese matrices; their displayed status/fidelity are unchanged,
so neither generated file has a diff. Matrix validation passed for 591 feature
rows, 401 parameter locations and 357 schema fields. The slice document also
passes `npx --yes markdownlint-cli2` with no issues.

SDK, container-build and guide suites are excluded because HTTP behavior,
Dockerfiles and guides are unchanged. No benchmark or performance claim is made.
Native Windows/Linux runtime, power-loss recovery and importer cutover remain
unverified here.

## Integration verification

Rebased onto `638d9af`, preserving publication modes, BlobId/hash changes and
all preceding ledger additions. The stream helper accepts mode and optional
identity together; ordinary stream and batch/file publication retain Automatic
generation assignment. Imported stream publication rejects non-Replace modes
before staging, preserving its declared source-identity contract.

The filtered library test command recorded in the ledger passed all 67 tests
with `RUST_TEST_THREADS=4`. It covers the five imported-identity tests, existing
management and publication tests, derived publication, BlobId, hashing and the
resumable commit/retry regression. Existing passing regressions include:

- `publication_streaming_modes_preserve_patches_and_require_all_hash_fields`
- `publication_inherits_metadata_patched_after_staging`
- `publication_equal_hash_keeps_generation_metadata_and_journal`
- `publication_batch_isolates_failed_elements_and_cleans_unused_blobs`
- `callback_source_deletion_rolls_back_with_destination_and_checks_holds`
- `retryable_publish_and_commit_failures_preserve_the_prefix_and_allocate_one_generation`
- `canonical_ids_preserve_names_shards_and_leading_zeroes`

`cargo fmt --all --check` and
`cargo clippy --workspace --all-targets --locked --offline -- -D warnings` pass.
Full `just check` is reserved for central verification after merging into main;
it was not repeated for integration preparation. SDK/container suites and
performance measurements were not run. Imported-identity connections to batch
and file publication remain pending, and `DESIGN-072` stays PARTIAL.

## Integration boundary

`storage::admin::ImportedIdentity::new` validates a typed positive generation
and representable, exact microsecond `SystemTime` values before upload starts.
`Admin::publish_imported_object` requires both source hashes in `Publication`;
the existing streaming upload verifies them against the immutable staged bytes.
The supported mode is Replace. The importer supplies supported metadata and
explicit hold values; default-hold exceptions belong to shared-publication-modes.
Metageneration starts at one and ETags retain existing generated behavior.

`objects::GenerationAssignment` adds only Automatic/Imported selection to the
shared commit function. Its existing automatic callers pass Automatic. The
shared INSERT stores the prepared record's creation and update times separately;
collision checking precedes retirement, and allocator advancement participates
in the same transaction. `last_generation = max(previous, imported)` allows
out-of-order imports without weakening subsequent automatic allocation.
Generation identity is scoped to bucket/name/generation, matching the existing
catalog key; equal generation numbers on different names are permitted.

## Contract sources

Checked official documentation on 2026-10-25; SHA-256 values identify the fetched
HTML content, not a promise of a stable upstream page revision:

- [Object metadata](https://docs.cloud.google.com/storage/docs/metadata):
  `46eac5ecf3e1c42442ede87f5e1f38faed136b35738c09508edb7d991575a16c`.
- [Objects resource](https://docs.cloud.google.com/storage/docs/json_api/v1/objects):
  `5ebff5f22abbf446804ab237d76f90038199e86b443eb1ab8e904b7d723cd1ba`.

Generation is ordinarily service-assigned and non-editable; this trusted local
import operation is an emulator extension. `compatibility/gaps.json` was reviewed:
SPEC-008's unresolved mutation-condition precedence remains unchanged, and these
local tests make no additional Google wire-parity claim. The existing pinned
Discovery/protobuf inventories are unaffected because no API field is added.

The parent `DESIGN-072` remains PARTIAL. The broader source-preserved-identity
importer case remains PENDING until its owner integrates publication and verifies
the source-to-destination workflow. No catalog schema or startup change is made.
