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
paths. Shared storage validates lexical safety only; Sync owns representability,
marker contents and filesystem operations. CatalogId is identity, not authority.
The local administrative interface uses the existing trusted-operator boundary.
State acknowledgments require the current object generation and verified hashes.
MD5/CRC32C describe immutable object bytes, rather than a new hash algorithm.
Nanoseconds/inodes use fixed-width big-endian blobs to avoid SQLite integer loss.

Root association/ownership, activation baselines, write-back intents, conflict
preservation, watching, mirror orchestration and HTTP controls remain pending in
the directory synchronization contract. This slice supplies state primitives,
not a complete mirror or marker recovery implementation. File and parent
durability must precede acknowledgment by the adapter.

Schema 17 is forward-only. Back up the entire stopped data directory before an
upgrade; restore the complete backup with its matching executable to roll back.
Older executables reject schema 17; do not edit user_version to downgrade.

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
`RUST_TEST_THREADS=4 just check` ran once: format/clippy, 399 library tests and
9 binary tests passed; auth_http stopped at 101 passed / 1 failed because an
old-schema test still expected version 16 (`left: 17`, `right: 16`). The IAM
expectation and matching bucket fixture were corrected; the two selected HTTP
migration tests, five sync-state tests, format and clippy then passed. The
coordinator will run the remaining full gate on integrated main; it was not
repeated here. Markdownlint passed. Independent review checks the focused fix.

No benchmark, SDK or container run is needed because this slice changes no HTTP
behavior, container input or user guide. No performance numbers are claimed.
