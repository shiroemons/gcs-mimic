# Protected workspace transfer and exact native export capacity

<!-- markdownlint-configure-file {"MD013": false} -->

This storage slice supports DESIGN-072 and DESIGN-074. It connects protected write-back bodies to the existing registered conflict store and fixes repeated reservation of an already captured body during native export. The feature ledger remains authoritative; these native storage checks do not complete the broader synchronization requirements.

## Contract and boundaries

`Writeback::preserve_protected` scans pending operations in keyset pages of at most 256 entries. Each owned run retains the root/catalog leases and the existing capacity-one write-back execution permit. Conflict capture and transfer use their internal transitions within that permit; they never acquire it recursively. Namespace work and incremental hashing run on existing bounded file workers. Transactions use static cached SQL and never wait for file I/O. Bodies use 64 KiB buffers; neither payloads nor the entire catalog are collected in memory.

Transfer uses the protected operation's original owner, catalog, root, bucket incarnation, logical name and observed generation. Deleting the bucket or recreating its name in another project cannot retarget or erase those bytes. Every SQL job retains the root's management authority scope and its original-owner union. Retention does not require a current bucket; restoring a registered conflict still uses the existing current-incarnation, generation, permission and hold checks.

The existing capture path creates an opaque, read-only owned copy and makes its contents and parent directory durable before registration. Before removing the workspace entry, transfer verifies both the registered copy and the actual source with full device/inode/size/nanosecond observations and streamed hashes. It records a typed transfer proof in the existing intent snapshot, performs checked unlink and actual parent synchronization, then completes the source operation and releases only its workspace charge. Transfer does not publish an API object or change its metadata, reader, checksums, holds, generation or notifications.

An existing `source_op` copy is reused only after its original context, identity, read-only permissions and actual body are verified. A complete unregistered copy can finish registration after a lost response. An incomplete private capture may be cleaned up while its source remains protected. If the source body changes, the older retained copy remains intact and separately charged; a new actual body needs its own reservation. A missing or changed registered copy cannot authorize workspace removal.

## Capacity and persistent native proof

While workspace and registered copies coexist, both physical protected bodies consume their actual bytes and entries. A new copy is refused if there is no temporary capacity. Zero-byte copies still consume entries. Checked removal and durable completion reduce the two charges to the one remaining copy. Unexpected source growth is refreshed in full, including above the limit; older copies are never evicted to make room.

Native `LiveSource::Preserved` already has a registered exact-body copy. Moving the original public-tree inode into the workspace does not create another copy, so native preparation does not reserve that same body again. Its persisted snapshot contains the registered ID/revision and exact source/retained observations, and retains the proof beside the final fingerprint. Entry accounting discounts only an active intent whose typed proof still matches its registered owner, root and logical context. Pending intent counts remain separate. The proof is checked against the actual owned copy before preparation, installation, durable recording and acknowledgement; an arbitrary boolean cannot bypass capacity or hold checks.

Protected phase-two operations receive no entry discount. If a last check finds another displaced body, or cleanup fails after installation, the actual surviving workspace body is charged in full and further exports stop. Two surviving protected copies may therefore exceed a budget that previously fit one; the stop and explicit recovery remedy remain visible. A failed final parent synchronization with an absent workspace still retains an I/O stop and its intent. An accepted unlink can resume after restart only with the durable transfer proof and a verified registered copy; an unexplained absence never releases quota.

After a successful native export, the final binding carries its root control scope so the normal shared tracking transaction can clear obsolete findings. A successful removal clears only the matching root, incarnation and observed generation. An external-edit conflict does not clear its finding.

The fixed conflict limits remain 1 GiB and 1000 entries. Smaller internal write-back limits exercise exact boundaries with small real bodies. Oversized sparse workspaces retain the existing metadata-only inspection and explicitly confirmed discard remedy; transfer does not read an over-limit sparse body to completion. Physical workspace-to-store copying still needs space for both bodies, including when the final single-copy state would fit.

## Acceptance and evidence

The native tests cover binary and zero-byte transfer; temporary byte and entry limits; exact native bytes/one-entry capacity; verified reuse and changed-body preservation; missing proof/copy rejection; original ownership after bucket deletion and recreation; API body/record preservation; real same-directory restart at write, flush, file sync, directory sync, registration, accepted transfer, unlink, parent sync and completion boundaries; and keyset traversal beyond 256 pending operations. Native cleanup failures additionally verify full retained accounting, over-limit/I/O stops, rejection of new exports, preservation of later external bytes and persistent proof reuse after restart.

Paired Snapshot and PowerLoss tests capture the actual workspace tree and catalog at all 13 transfer/capture boundaries. They reopen the captured catalog, verify its API body and record, reactivate the original root, finish transfer and check the remaining registered body and exact quota. Snapshot copies allocate new host inodes: the test harness translates only copied device/inode/timestamp observations after verifying sizes and available hashes. It does not repair missing/truncated/different bodies, state, intent or permissions. Independent real-directory restarts preserve actual inode identities.

On 2026-10-31, the final combined storage run passed all 67 tests, including the existing 22 conflict and 32 write-back tests and 13 new transfer/native-capacity tests. No negative case is skipped or weakened.

| Check | Result |
| --- | --- |
| `cargo check --locked --offline --tests` | PASS |
| `cargo test --locked --offline --lib -- storage::catalog::sync_state::writeback storage::catalog::sync_state::conflicts --nocapture` | 67 passed, zero failed or ignored |
| `cargo xtask lint` | Zero production violations |
| Scoped `rustfmt --edition 2024 --config skip_children=true --check` and `git diff --check` | PASS |
| markdownlint-cli2 0.24.0, markdownlint 0.42.0 | One slice file linted; zero issues |

The additional test names are:

```text
protected_transfer_preserves_binary_and_empty_without_changing_api
protected_transfer_requires_temporary_capacity_for_both_actual_copies
protected_transfer_reuses_registered_copy_and_retains_changed_body_copy
preserved_export_uses_exact_byte_and_entry_budget_once
protected_transfer_restarts_each_copy_and_unlink_failure_without_losing_a_body
protected_transfer_retains_original_owner_incarnation_and_generation_after_recreation
protected_transfer_refuses_missing_proof_or_changed_owned_copy
protected_transfer_paired_crash_and_power_loss_reopen_catalog_api_and_finish_quota_transfer
native_cleanup_failure_charges_the_actual_displaced_body_and_stops_new_exports
preserved_native_proof_survives_restart_without_charging_another_retained_entry
preserved_native_export_never_removes_a_later_external_body_or_an_unproven_copy
protected_zero_byte_transfer_reserves_a_second_physical_entry
protected_transfer_crosses_keyset_pages_without_skipping_completed_rows
```

Final runtime HTTP/strict/observation integration is owned by its consumer slice. SDK, container and Linux distribution checks run once on the final integrated source. This slice adds no dependency, schema or version pin and claims no measured performance improvement. Native macOS results and the instrumented durability model do not prove Windows/Linux runtime equivalence, every-instruction process-kill coverage or physical host power-loss durability.

## Upstream boundary

Conflict transfer and its capacity accounting are emulator-local safety behavior. Google's [request preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions) and [object holds](https://docs.cloud.google.com/storage/docs/object-holds), checked on 2026-10-31, continue to govern shared publication. The existing Discovery/protobuf source pins in [directory synchronization](directory-synchronization.md#sources-and-verification-boundary) are unchanged. The comparison implementation remains fake-gcs-server v1.56.1, commit `896ece51e49eed0946089f3089f6343855c0d106`.
