# Shared synchronization state

Scope: DESIGN-062, DESIGN-072, DESIGN-074 and AUDIT-014. Parent rows remain
PARTIAL; UX-002 seed-before-ready behavior is unchanged.

## Acceptance contract

- Persist a random opaque CatalogId across reopen, distinct for fresh catalogs.
- Store the last synchronized generation and fingerprint (size, signed nanosecond
  mtime/ctime, unsigned inode, MD5 and CRC32C, racy flag). Preserve integer extrema.
- Bind state to catalog and bucket incarnation. Reject foreign/stale bindings,
  invalid relative paths, duplicate paths and stale generation acknowledgments.
- Provide indexed path lookup and keyset pages capped at 256 entries. Never
  load the complete object catalog or perform file work inside a transaction.
- Attach typed state updates to staged publications. Commit state with object
  publication, including same-hash skips; failures roll back both. API writes
  preserve the old synchronized generation so later reconciliation sees changes.
- Delete with a generation condition and remove the matching synchronization
  record in the same transaction; holds and stale conditions change neither.
- Atomically migrate schema 16 to 17, preserving existing data. Failed migration
  rolls back DDL/version and permits retry; validate identity and schema on open.

## Decisions and boundaries

One configured working tree is represented by UTF-8 slash-separated relative
paths. Shared storage validates lexical safety; Sync owns representability and
object-file operations. The later [shared root slice](shared-sync-root.md) owns
root association, exclusive leases and marker validation. CatalogId is identity,
not authority.
The local administrative interface uses the existing trusted-operator boundary.
State acknowledgments require the current object generation and verified hashes.
MD5/CRC32C describe immutable object bytes, rather than a new hash algorithm.
Nanoseconds/inodes use fixed-width big-endian blobs to avoid SQLite integer loss.

Root association/ownership and explicit activation-baseline transitions are
provided by [shared-sync-root](shared-sync-root.md); they are not connected to
runtime startup. [Shared write-back APIs](shared-sync-writeback.md) now provide
durable pending phases, protected workspace accounting, private durability
receipts, atomic fingerprint/progress acknowledgments and opened ObjectReader
descriptor transfer. Target replacement, conflict registration/discard, watching,
initial merge, mirror orchestration and HTTP controls remain pending in the
directory synchronization contract.

Schema 17 introduced these records; schema 19 adds root associations and
schema 20 adds durable write-back state. All are forward-only. Back up the
entire stopped data directory before an
upgrade; restore the complete backup with its matching executable to roll back.
Executables supporting at most schema 19 reject schema 20; do not edit
`user_version` to downgrade.

## Verification

On 2026-10-25, `cargo test --locked --offline --lib sync_state` passed all four
tests initially and all five after the batch-isolation correction, on macOS ARM64
with Rust 1.99.1:

- `sync_state_identity_index_pages_and_exact_fingerprints_survive_reopen`
- `sync_state_publication_skip_rollback_and_conditional_delete_are_atomic`
- `sync_state_rejects_hash_path_identity_and_hold_conflicts_without_publication`
- `sync_state_batch_path_conflict_isolates_the_failed_element_and_cleans_its_blob`
- `sync_state_schema_migration_rolls_back_retries_and_preserves_objects`

The existing `v1_migration_is_transactional_and_current_schema_reopens_idempotently`
test also passed after updating old-schema fixtures. The dependency freshness
check passed with all pinned inputs CURRENT; no dependency was added or updated.
`RUST_TEST_THREADS=4 just check` passed on integrated main `fd43d63`, including
format, clippy, workspace/doc tests, compatibility and shell checks. The selected
HTTP migration tests and five sync-state tests also passed with current-schema
fixtures. Markdownlint passed and independent review found no release blockers.

No benchmark, SDK or container run is needed because this slice changes no HTTP
behavior, container input or user guide. No performance numbers are claimed.
