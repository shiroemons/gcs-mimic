# Durable synchronization conflicts and recovery

<!-- markdownlint-configure-file {"MD013": false} -->

This storage slice supports DESIGN-072 and DESIGN-074. It implements owned conflict capture, bounded inspection, conditional restore, explicit discard, and the lower-level remedy for an unregistered displaced workspace body. Runtime mirroring, HTTP/CLI projection, strict startup, and combined migration evidence belong to their owning slices. The feature ledger remains authoritative; these storage tests alone do not complete the parent requirements.

## Contract and ownership

Schema 42 adds `sync_conflict_roots`, `sync_conflicts`, `sync_conflict_operations`, and `sync_recovery_workspaces`. Original owner project, bucket incarnation, logical object, root, and catalog identity survive bucket deletion and cross-project same-name recreation. Current bucket lookup never replaces a retained owner. Observation triggers invalidate management findings after every retained-state change. Startup changes unused listing and recovery revisions; accepted operations retain their exact payload and idempotency result.

`RootLease::capture_conflict` accepts an already opened regular file. `capture_path` keeps descriptor-relative, no-follow traversal and its descriptor handoff inside storage-owned work. Both retain the root/catalog lease and the existing write-back execution permit through bounded file jobs and short database transactions. Capture never modifies its source. A 64 KiB buffer computes incremental MD5 and CRC32C while copying; device, inode, size, mtime, and ctime must remain unchanged. The owned copy becomes read-only and is synchronized with its directory before registration. Logical names never select the retained filename: `data_dir/sync-conflicts/<opaque conflict_id>` owns those bytes.

`CapturedConflict::validate` checks the actual write-back root, catalog, incarnation, logical target, expected generation, and still-registered revision inside the caller's transaction. Its private proof contains the exact captured source fingerprint and device. Consumers must compare the current target against that proof before checked replacement; a successful capture alone does not authorize replacing a later external edit. Ordinary hold checks remain in force. Only the separately typed, exact preserved-body/read-only export path may use this proof.

## Capacity and incomplete copies

The conflict budget is fixed at 1 GiB and 1000 entries. Zero-byte bodies consume one entry. Reservations, registered copies, and existing protected workspace bodies participate in one capacity decision. While copying a workspace body into the conflict store, both physical copies consume bytes and entries. Only checked removal and durable completion release the workspace charge; the remaining conflict copy is then counted once. A full budget cannot be bypassed by tagging a capture with a workspace operation.

Unexpected protected workspace size is recorded in full with 128-bit aggregate accounting, even above the fixed budget. No quota overflow evicts older conflicts or removes the source. A failed capture can reclaim its own incomplete output while its source remains untouched. If registration or cleanup cannot complete, the reservation remains inspectable. A capturing record has an explicit discard remedy: accept an exact revision first, persist observations of any surviving partial copy, then check and remove only that owned entry. An absent unstarted copy is safe to finish only after that recorded acceptance.

## Registered resolution

Restore stages the retained immutable body through existing administrative publication with mandatory expected hashes, `InheritFromReplaced`, and an explicit generation condition. Native metadata inheritance, ACLs, holds, checksums, versioning, and notifications remain shared. The original bucket must still have its captured incarnation. Publication validates current management authority and the accepted operation in the same transaction as object mutation; its completion hook records `result_generation` in that transaction. A lost response after commit replays the recorded generation and finishes reclamation without another publication. Transaction failure preserves both the old API winner and retained bytes.

Discard durably accepts the exact operation before checked unlink, parent synchronization, and completion. It changes no API generation. Failed unlink, synchronization, or completion retains the charge and accepted intent. Retry may finish an absent entry only with that intent. Caller cancellation cannot drop the executing owned task or release its lease early. Failed earlier restore attempts become a recorded `target_resolved` failure when another explicit resolution finishes that target; replay never reports them as successful restores.

Every read and accepted mutation checks the shared management authority union. Unresolved original owners remain required after their last bucket disappears. Once all retained work is completed, that historical project does not permanently restrict unrelated status reads. An identical completed-operation retry still separately requires the recorded original owner's project grant.

## Unregistered workspace recovery

The existing protected write-back operation owns `sync_dir/.gcs-mimic-sync-work/<recovery_id>`. Recovery inspection verifies the workspace directory identity and actual file observations, records an opaque target identity and revision, updates the full retained charge, and exposes an over-limit flag and remedy. Metadata-only inspection does not read a sparse body to completion; unverified hashes remain absent. This includes a 1099511627777-byte sparse file that cannot fit the conflict budget.

Recovery resolution uses the existing resolve operation. It requires one recovery ID, its exact inspected revision and target identity, a unique operation ID, `discard`, and exactly one explicit confirmation: `backup_verified` or `discard_without_backup`. There is no backup-path input, implicit confirmation, restore alias, or alternate administrative bypass. Stale identity, changed body, insufficient authority, or missing confirmation retains bytes, quota, and the stop. Admission failure occurs before unlink. Accepted retry after restart rechecks current authority and target; a fresh request after restart must use a newly inspected revision.

For manual preservation, stop external writers and the server, back up the catalog, conflict store, and working tree including its reserved workspace, and copy the selected recovery entry outside both roots. Verify that copy against the inspected size and available hashes. Restart, inspect again, and explicitly attest to the verified backup or permanent loss of that exact body. HTTP output contains logical targets and opaque IDs, never filesystem paths or object bodies. Removal of one entry leaves root recovery active until ordinary revalidation and resume succeed.

Conflict pages contain at most 256 items and stop before 240000 serialized item bytes, leaving room under the 256 KiB HTTP envelope. Recovery refresh pages contain at most 128 items with the same byte budget. Continue recovery refresh from the last returned ordinal until an empty page; byte-limited pages can contain fewer than the requested limit. Management projection must retain its fixed findings revision and invalidate stale cursors.

## Verification and limits

The native storage suite exercises binary/zero-byte retention, exact reservations and 1000-entry capacity, copied-workspace accounting, incomplete-capture disposal, stale generations, holds, original-owner authorization, bucket recreation, completed replay, and the oversized sparse recovery remedy. It tests write, flush, file sync, directory sync, registration, accepted intent, unlink, parent sync, completion, pre-publication-commit rollback, and lost response after committed publication. Faults must actually fire: each test checks the remaining fault count.

Snapshot and PowerLoss tests reopen the captured catalog, verify its API record/body/checksums, reactivate the root, and execute the admitted operation or a fresh inspected remedy through quota release. Workspace tests capture catalog and actual working tree at paired boundaries, then replay only the recorded discard before ordinary root resume. Independent same-directory restarts preserve real inode identities and test unused-revision invalidation, accepted retry, and repeated completion.

Copy-based snapshots allocate new host inodes. Their test harness translates only copied device/inode/timestamp observations after matching the retained size and hashes; it does not repair missing/truncated/different bytes or alter state, intent, or permission conditions. The production identity fence remains unchanged. The fault model now explicitly tracks new file entries and unlinks until parent sync, so omission of those namespace syncs is observable. This model is not proof of physical host power-loss behavior.

Run the focused suite with:

```sh
cargo check --locked --offline
cargo test --locked --offline --lib storage::catalog::sync_state::conflicts -- --nocapture
cargo xtask lint
```

On 2026-10-31 the focused suite passed all 19 tests with no failures or skips. `cargo check --locked --offline` passed, and `cargo xtask lint` reported zero production violations. Formatting and the slice's whitespace checks passed. These results cover the storage scope above; final runtime consumer and distribution gates remain separate.

Native macOS validation does not establish Linux/Windows runtime parity, every-instruction process-kill coverage, physical filesystem durability, or final SDK/container/HTTP behavior. No SDK, container build, cloud request, or performance benchmark is part of this storage verification.

## Upstream boundary

The conflict store and recovery controls are emulator-local safety behavior. Google's [request preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions) and [object holds](https://docs.cloud.google.com/storage/docs/object-holds), checked on 2026-10-31, remain the publication contract: a retained external edit does not bypass those protections. The existing Google Discovery/protobuf source pins remain those in [directory synchronization](directory-synchronization.md#sources-and-verification-boundary). The comparison implementation is fake-gcs-server v1.56.1, commit `896ece51e49eed0946089f3089f6343855c0d106`; it does not define Google's authorization or durability guarantees. No dependencies or version pins are added by this slice.
