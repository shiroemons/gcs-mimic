# Shared synchronization root

Scope: DESIGN-062, DESIGN-072, DESIGN-074 and AUDIT-014. Parent rows remain
PARTIAL. This adapter-facing storage API does not configure or start a mirror,
change HTTP/CLI behavior or perform the initial merge. UX-002 seed is unchanged.

## Acceptance contract

- Reuse the durable CatalogId; persist an opaque RootId, canonical root location,
  device/inode identity and activating/complete association in schema 19.
- Lock the root directory inode itself across catalogs; a replaceable lock file
  cannot bypass ownership. Reject root replacement and symlinks, including roots
  spelled with trailing separators/dots; recheck the originally selected path.
  A file job owns
  its lease and catalog drain permit through completion, including cancellation.
- Read no more than 4097 bytes of the version-1 marker. Reject unknown fields,
  versions, malformed IDs, duplicate fields, non-files, symlinks, oversize or
  foreign markers. Use descriptor-relative no-follow operations and revalidate
  the selected root and marker around file work.
- Activate a never-used pair and reopen the same association without accepting
  external deletions until an explicit complete-baseline transition. A known
  association with a missing marker fails closed, including interrupted activation.
- Explicit reinitialization requires the exact CatalogId, unchanged root identity,
  exclusive ownership and a missing marker. Never overwrite foreign/malformed
  markers, clear API/seed/conflict data or modify another root association.
- Test competing catalogs, same-catalog reopen, replacement, marker boundaries,
  activation interruptions, reinitialization, cancellation/drain and migration
  rollback/retry with existing data preserved.

## Commit and recovery protocol

Reserve the activating association in a short SQLite transaction, then exclusively
create the marker without replacement, write/flush it and flush the root directory.
No filesystem I/O runs inside SQLite transactions. SQLite and the filesystem are
separate durability domains: a reservation followed by a failed marker publication
requires explicit missing-marker reinitialization; a complete marker resumes the
same activating association. A malformed or partial marker is never repaired
automatically. A partial marker requires operator backup and removal before the
missing-marker repair can be requested. This deliberate fail-closed protocol
avoids an additional temporary workspace or an atomicity claim across domains.
Only the mirror's explicit baseline completion permits deletions.

Unix directory locks and no-follow descriptor operations are required. Other
platforms fail explicitly rather than use a weaker ownership fallback. All file
work uses the existing two dedicated OS workers and bounded queue. Callers must
release idle leases before closing the catalog; close drains running file jobs.

The canonical root location and device/inode pair must both match the durable
association. No-follow opens strip trailing separators/dot components without
resolving symlinks or parent components. Both the canonical location and original
absolute selection are rechecked against the pinned identity, so retargeting an
ancestor alias cannot silently redirect file work. A different fresh directory
can be associated separately; root
replacement at a known location is rejected. Reinitialization resets only that
association's baseline flag and preserves its RootId, other root associations,
existing synchronized records, API objects, seed state and workspace files.
The marker contains lowercase 32-digit hexadecimal IDs, not credentials.
The root and marker are revalidated around descriptor-relative file jobs;
callers must not retain descriptors or spawn independent work from the callback.
This is cooperative emulator ownership, not a sandbox against a malicious OS user.

`RootLease::status` returns a conservative activation snapshot. The explicit
`complete_baseline` result records Complete durably; the original snapshot need
not be promoted. A newly reopened lease reads that committed status. The API does
not assert that a merge occurred: its caller is responsible for finishing the
initial merge and replay before requesting completion.

Schema 19 is forward-only. Back up the entire stopped data directory before
upgrading; restore it with the matching executable to roll back. Do not edit
`user_version` to downgrade. Migration changes no existing resource or seed data.
The Google API gaps ledger was reviewed: this is an emulator-local identity and
ownership protocol and changes no Google method, Discovery field or open gap.

## Verification

On 2026-10-25, macOS ARM64, Rust 1.99.1, including selected-path protection in
`2ce4de17630de272e35fe1009d1dba3121b446a8`:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
RUST_TEST_THREADS=4 cargo test --locked --offline --lib -- \
  sync_root sync_state import_publication import_journal change_journal \
  storage::catalog_tests storage::object_tests::v1
```

Result: 58 passed, 0 failed. The root-specific tests are:

- `sync_root_activation_baseline_reopen_and_reinitialize_preserve_data`
- `sync_root_exclusive_inode_ownership_blocks_catalogs_and_lock_replacement`
- `sync_root_lock_child`
- `sync_root_replacement_symlinks_and_marker_changes_fail_closed`
- `sync_root_trailing_separators_and_dots_do_not_follow_symlinks`
- `sync_root_selected_parent_alias_retarget_stops_owned_jobs`
- `sync_root_marker_invalid_foreign_and_size_boundaries_are_preserved`
- `sync_root_interrupted_activation_requires_explicit_missing_marker_recovery`
- `sync_root_cancelled_file_job_keeps_both_ownerships_until_drain`
- `sync_root_schema_migration_rolls_back_retries_and_preserves_state`
- `sync_root_activation_failure_boundaries_preserve_reservation_and_refuse_partial_marker`
- `sync_root_sql_failures_never_publish_marker_or_complete_baseline`

Failure injection covers reservation commit, exclusive marker creation, marker
file sync and root-directory sync boundaries; restart requires explicit repair
for missing markers and rejects partial markers. SQL failures preserve absent
markers/incomplete baselines. Migration covers conflicting DDL, actual SQLite
disk-full rollback, rollback after DDL/version changes and successful Catalog-open
retry with catalog identity and bucket state retained. Existing sync-state and
import/journal migration tests also pass with preserved objects and state.
Cross-process root-lock rejection, catalog-lock file replacement, independent
root associations and cancellation/shutdown drain are tested. These are native
tests and injected failures, not a host power-loss guarantee.

`cargo xtask dependencies check` exited 0 with every input CURRENT, including
Rust 1.99.1 and the pinned upstream dependency/container/SDK metadata. No
dependency was added or updated.

The final command below exited 0 on the corrected source. Format, clippy with
warnings denied, workspace and doc tests, compatibility/generated-view validation
and shell checks passed;
all 15 native seed tests passed, preserving UX-002. English and Japanese feature
matrices were regenerated and remained byte-identical because parent statuses
and fidelity did not change. Markdownlint 0.24.0 reported no issues in the three
changed slice documents.

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
RUST_TEST_THREADS=4 just check
```

No benchmark, SDK, container or full guide execution was performed: HTTP, Docker
inputs and user guides are unchanged. Linux and Windows runtime verification,
host power loss, mirror merge and startup integration remain outside this slice.
