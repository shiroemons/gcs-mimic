# Shared onboarding state contract

<!-- markdownlint-configure-file {"MD013": false} -->

This contract freezes the Shared state boundary for initialization, synchronization and import. DESIGN-062, DESIGN-072, DESIGN-074 and AUDIT-014 remain PARTIAL; UX-002 retains its implemented seed-before-ready scope. The [startup phase slice](startup-phases.md) supplies the runtime startup subset below; other runtime cases remain PENDING until their owning slices supply executable evidence.

## Acceptance and scope

The first three rows map to the three original `onboarding-state-contract` delivery conditions. The timestamp prerequisite subsequently added to that delivery entry is preserved separately, with its existing schema 14 evidence.

| Case | Feature | Required evidence / owner |
| --- | --- | --- |
| onboarding-state-authority | DESIGN-062 | PASS: validated identity/authority and bounded management operations, unchanged strict API permissions, native seed 13/Linux seed 14 cases and permissive/strict SDK/container regressions. [Evidence](../evidence/shared-admin-interface.md). Initializer startup remains separate. |
| onboarding-state-ownership | DESIGN-072 | Shared services own schema, conditions, hashes, generations and recovery; adapters use typed operations and transactional state hooks, never SQL/private blob paths; shared-admin-interface and shared-publication-modes. PENDING. |
| shared-change-journal | DESIGN-072 | PASS: origin-bearing transactional mutations, bounded replay and retention, stable fences, rescan after activation/restart/gap/PAP change. Catalog API only; no runtime consumer configuration. See change journal evidence below. |
| catalog-change-journal-migration | AUDIT-014 | PASS: atomic schema 14 to 15, partial-DDL rollback/retry, preserved objects/policies/active sessions, retained schema-14 catalog snapshot, complete bounded inventory and newer-format rejection. Full stopped-data restoration with older executables remains under catalog-upgrade-policy. |
| onboarding-state-create-missing | DESIGN-062 | Preserve existing bucket settings/objects on restart, changed project and removed entries; initializer runs before seed, independent of synchronization/public URL settings; shared-startup-phases and init-create-missing. PENDING. |
| onboarding-state-cancellation | DESIGN-074 | Interrupt every pre-bind phase; no ready event on failure, drain side effects before unlocking, remove definitely uncommitted files and retain possibly committed bodies; shared-startup-phases, shared-admin-interface and shared-publication-modes. PENDING. |
| administrative-create-missing | DESIGN-062 | PASS: typed names/generations/authority, ordered create-missing up to 1000 records, preserved stored projects/settings, atomic rollback, competing callers and strict seed/API separation. Initializer startup remains separate. [Native evidence](../evidence/shared-admin-interface.md). |
| administrative-bounded-operations | DESIGN-072 | PASS: independent management admission under full API slots, bounded queue, actual public 30-second timeout, shared metadata/condition/checksum/generation/versioning behavior and bounded reads. [Native evidence](../evidence/shared-admin-interface.md). |
| administrative-cancellation-and-errors | DESIGN-074 | PASS: accepted SQL/publication ownership survives cancellation/close, staging cleanup precedes unlock, renamed publication remains readable and streamed/SQLite disk full retains its type. Later publication handles remain PENDING. [Native evidence](../evidence/shared-admin-interface.md). |
| administrative-regression-benchmark | DESIGN-072 | PASS: six equivalent-profile workloads, five candidate repetitions each, zero errors; all 39 calibrated historical-baseline gates pass. Startup-10000 CPU remains PENDING for precision. Build-graph/background-load limits and every result: [evidence](../evidence/shared-admin-interface.md). |
| startup-phase-order | DESIGN-062 | PASS, owner shared-startup-phases: recovery → PAP → seed → bind → ready, typed phase results and serialized ReadyEvent; foundation/seed suites preserve fields, strict permissions and seed-before-ready. Future initializer/import/sync phases remain unimplemented. [Tests](startup-phases.md#acceptance-evidence). |
| startup-phase-cleanup | DESIGN-074 | PASS, owner shared-startup-phases: recovery/PAP barriers, real seed SIGTERM during partial staging and after rename before commit, PAP/bind/close failures and bind-wait TERM/INT verify no false readiness, drain before unlock, empty staging and no unreferenced blob before stopped, and retry with preserved committed seed bytes. [Tests](startup-phases.md#acceptance-evidence). Administrative/publication-handle cases remain PENDING. |
| startup-phase-benchmark | DESIGN-072 | PASS, owner shared-startup-phases: five exclusive native startup measurements at 10,000/100,000 objects, zero errors, valid readiness and clean stop, all seven calibrated CPU/readiness RSS/disk/wall gates pass; startup-10000 CPU remains PENDING. Source/build-graph limits and every sample: [evidence](../evidence/startup-phases.md#measurement-result). |
| onboarding-state-publication | DESIGN-074 | Native modes, SAVEPOINT file batches, count/time closure, DiskFull rollback and cancellation cleanup pass in [publication modes](shared-publication-modes.md). Broader adapter-state hooks and exhaustive batch/file crash evidence remain PENDING. |
| publication-metadata-modes | DESIGN-074 | PASS: transaction-local metadata inheritance, equal size/MD5/CRC32C no-op, condition/hold enforcement; [publication modes](shared-publication-modes.md). |
| publication-staged-batches | DESIGN-074 | PASS: catalog-bound staging handles, shared 256-handle budget, SAVEPOINT outcomes, timed prefixes, staging overlap and DiskFull rollback; [publication modes](shared-publication-modes.md). |
| publication-file-integrity | DESIGN-074 | PASS: dedicated workers, immutable snapshot hashing, one-pass fallback, source-drift/checksum/size rejection; [publication modes](shared-publication-modes.md). |
| publication-handle-cleanup | DESIGN-074 | PASS: discard/drop/cancellation, renamed-uncommitted cleanup and shutdown drain; exhaustive crash injection remains in the broader publication case. [Evidence](shared-publication-modes.md). |
| onboarding-state-import-holds | DESIGN-074 | Import bucket default hold without synthesizing holds on imported existing objects; later ordinary uploads inherit that default. shared-publication-modes with import-filesystem. PENDING. |
| onboarding-state-import-observations | DESIGN-072 | Bounded committed-element observations, atomic replacement after successful drift revalidation, repeated-resume and crash cases; shared-import-journal and import-filesystem. PENDING. |
| onboarding-state-benchmark | DESIGN-072 | PASS: five isolated same-profile measurements of existing publication, overwrite, 1 GiB transfer and startup at 10,000/100,000 objects, including correctness assertions, raw values, variability and per-metric calibration states/thresholds. Evidence: [native baseline](../evidence/onboarding-state-benchmark.md). |
| onboarding-state-sync-benchmark | DESIGN-072 | One full-rescan timing at about 1,000 entries, unchanged and then with one changed file, recorded as a fact without a regression gate. PENDING: synchronization is unimplemented. |
| catalog-upgrade-policy | AUDIT-014 | Each schema-owning slice tests migration/rollback/retry and backup restoration with generations, permissions and active sessions; final replacement-validation verifies the combined chain. PENDING for later schemas. Existing timestamp cases stay PASS in their original contract. |

## Identity, authority and create-missing

### Administrative interface acceptance

The shared-admin-interface slice preserves schema 14 and the existing publication/recovery protocol. Its executed acceptance cases cover validated bucket/object names and positive generations; create-missing batches (at most 1000 entries) preserve input order, stored projects, settings, policy and incarnation, and roll back together; Replace retains metadata, atomic conditions, holds, checksums and monotonically allocated generations; management reads use keyset pages of at most 256. API admission saturation does not block management admission, while management slot/execution waits time out after 30 seconds. Shutdown closes management admission and drains accepted publication and cancellation cleanup before releasing the directory lock. Filesystem StorageFull and SQLite SQLITE_FULL reach AdminError::DiskFull without string parsing; SQLite/stored-value corruption remains Corrupt. Seed's fourteen Linux cases, strict API authorization and existing schema migration regressions pass as recorded in the linked evidence. Origin is carried by the grant; persistent origin records belong to shared-change-journal.

Bucket identity is its validated global bucket name plus its stored owning project. Object identity is bucket/name/generation; project is not a second bucket namespace. `ensure_bucket(s)` returns `Created` or `Existing` with the stored record. It never patches existing settings or installs IAM grants. Callers decide whether an existing different project blocks their operation; the initializer must diagnose that mismatch without reassignment. Imported names/projects never create public permissions. Removing initialization entries never deletes resources.

Initialization, seed and offline import are local administrative operations on operator-controlled trusted input. Both permissive and strict profiles use an `AdminGrant` minted only inside `storage::admin`; API requests still use their configured authorization profile. The admin boundary removes `BucketAccess::Default`, separates `Permissive` from `Admin(AdminGrant)`, uses the planned Permission/name/generation types and limits unchecked Catalog methods to crate visibility. This contract does not retrofit those types throughout existing records.

## Startup and cancellation

The native administrative subset passes through `storage::admin::Admin`, minted explicitly by `Catalog::admin(Origin)`. Validated BucketDefinition values feed ensure_bucket(s); Publication requires an explicit publication mode, metadata/checksums, byte cap and an optional typed generation condition. Management delete requires a generation condition. Existing records retain their stored project; mismatch policy stays with the caller. Management reads have 32 slots and share a bounded pool of 32 simultaneous slot/execution waiters; overflow returns Busy. Close marks catalog-wide admission closed and wakes management waiters, then drains management semaphores before execution/lock release. Those semaphores stay drainable: closing them would prevent acquiring all permits to prove that drain. The API profile branch constructs Permissive explicitly; neither BucketAccess nor its enclosing upload/derived request types has an implicit authorization Default.

Fixed metadata validation is shared by storage and the JSON adapter: controls, non-ASCII fixed fields, overlong/empty contentLanguage and empty custom keys cannot create an unreadable generation through management publication. Request validation remains protocol-specific. Corrupt SQLite results and stored-value decoding retain a typed corruption failure. Backend I/O/corruption failures follow the existing resumable/derived reference-resolution path and preserve received bytes; only semantic rejection invalidates accepted work. Raw SQL callbacks, session creation/leases/staging paths and internal transfer records are crate-private, including the tests that intentionally inspect them.

The required order is config validation → TLS/registry → catalog open/recovery → unfinished-import guard → PAP → bucket initializer → seed → sync preflight → bind → ready → full sync reconciliation. Ready means API usability, not a caught-up working tree. Create-missing initialization precedes seed. Neither depends on a public hostname or completion of synchronization.

Every pre-bind phase races shutdown. Cancellation stops admission, lets owned side effects finish, closes the catalog and emits stopped only after lock release. Failure emits no ready event and exits nonzero after close. Side-effecting futures use detached owned work with permits; only side-effect-free waits may be dropped. Temporary-file cleanup must not do synchronous I/O or wait for SQLite on Tokio workers. Catalog close waits for management permits, cleanup and execution; is_healthy includes management admission state.

The baseline still performs object-count-proportional recovery before bind. Moving reclamation after readiness is permitted only after the protection protocol below exists. No startup improvement or bounded recovery duration is claimed here. shared-startup-phases preserves recovery internals. On shutdown during open/recovery, it awaits the pinned owned open result and closes any returned catalog; blocking recovery cannot be safely aborted. The private race requires an explicit CancelSafe marker for each individually reviewed PAP, seed and bind wait; it cannot take an unmarked compound phase. Existing catalog jobs/uploads own their execution/upload permits until completion and cleanup; close drains those permits before releasing the directory. Each safe cancellation point is documented in [startup phases](startup-phases.md#cancellation-failure-and-ownership). Configuration/TLS/registry preflight remains synchronous before runtime signal registration, as before; the shutdown race covers every asynchronous pre-bind phase. Initializer, unfinished-import and sync integration remain their owning slices' obligations and need their own cancellation contract.

## Bounded shared operations

| Operation | Required contract |
| --- | --- |
| ensure_bucket / ensure_buckets | Create missing resources only; batch existing-resource lookup, insertion and policy/incarnation creation use one transaction. Return Created/Existing in input order with actual stored project; existing records stay unchanged. Validate the complete bounded initializer input before mutation; any batch failure rolls back the entire initializer batch. Commit-time cancellation drains before close. This all-or-nothing initializer operation is distinct from per-element publication SAVEPOINTs. |
| publish_object | Replace, InheritFromReplaced, SkipIfSameHash and their combined policy; metadata and conditions checked in the publication transaction; dedicated bounded management admission, including advisory reads. |
| publish_objects | At most 256 input elements; durable immutable bodies precede SQL; each element gets a SAVEPOINT and typed result; commit independent successes and their adapter state together. |
| publish_from_file | Compose stage_from_file with publication; dedicated OS workers, immutable clone or one source-read/staging-write/hash pass, before/after source fingerprint check. Never use copy_file_range on mutable input. |
| stage_from_file | Return an opaque verified staged handle, with expected digests and source identity verified against the staged immutable bytes; no catalog publication or private blob path. Accepted by publish_objects. |
| delete_object | Atomic generation condition; preserve versioning/holds and journal side effects. |
| get/open/list | Bounded waiting management reads outside API admission; keyset pages, at most 256 records for adapter state. Open readers pin the selected immutable blob. |
| adapter state | Typed state updates participate in the same transaction as successful object changes. Adapters never receive a SQL connection or supply arbitrary SQL callbacks. |
| origin / changes | Typed api, seed, bucket-init, sync-ingest and import origins; ordered cursor reads with explicit gap/full-rescan signals. |

Management publication has **2 slots**, a **30-second admission deadline**, no unbounded queue and no automatic retry. Batch publication is single-flight per catalog, with at most one queued/staging next batch. Management reads also use bounded admission and the same 30-second deadline, independently of the API's immediate-failure 32-call/8-upload limits. CPU/body/walk work stays on bounded dedicated OS workers, not the two-worker shared blocking pool. A starting budget is two such workers and two page-channel slots, each page at most 256 records; slices must measure that budget rather than spawning per file.

The batch transaction budget is **20 ms** of monotonic elapsed execution: check before starting each next element and close/commit the successful prefix when reached, returning unattempted handles. A single SQL element or durable commit cannot be preempted safely and can exceed that budget; record that overrun, yield the serial execution lane before any next batch and test API interleaving. This explicitly qualifies the plan's transaction-time upper bound: a hard wall-clock abort during SQLite commit cannot guarantee rollback or durability. Body staging/hashing and reclamation never hold the transaction. Count/time closure does not roll back successful elements. DiskFull fails staging or the whole transaction once; Busy is admission timeout, not an element-specific retry.

`AdminError` carries Busy { retry_after }, DiskFull, Closed, Precondition, Hold, SourceChanged with changed-field evidence, Corrupt and typed I/O kind. Preserve StorageFull instead of parsing strings. No importer infers success from AlreadyExists or a blob filename.

### Handle budget and publication protection

The bound follows the plan's sum of route limits: 8 API uploads + 2 × 128 derived transfers + 128 resumable sessions + 2 management slots + 256 staged batch handles = **650 protected blob IDs per catalog**. These are bounds on IDs/handles, not an RSS or disk-byte result. Existing session byte/object caps and disk-full handling still apply.

The batch's active, staged-next, queued, unattempted and resolving handles share the **same 256-handle budget**, in addition to the two single-object management slots. A partial committing batch permits next-batch staging in the remaining capacity; a full 256-handle batch backpressures staging until outcomes resolve. A second 256-handle queue is forbidden. Unattempted work returned by count/time closure retains its charges, reducing the capacity for new staging. Budget admission waits with a deadline; cancellation does not release protection while side effects can still occur. Test full-budget backpressure, release after cleanup, partial-batch staging/commit overlap and shutdown under saturation. Benchmark full-batch backpressure versus partial-batch overlap when implemented; this slice makes no overlap speed claim.

Register each blob ID before rename and release only after resolved commit or completed cleanup. The shared publication function covers media, multipart, XML PUT, derived, resumable finalization and both management/file/batch paths. Reclamation holds the serial execution protection while confirming no reference and no in-flight owner. Excluded/unattempted/cancelled handles known to be uncommitted and unreferenced are reclaimed before successful command cleanup, with protection retained through reclamation. Possibly committed or ambiguous outcomes remain protected/recoverable; cancelling during SQLite commit and dropping a handle must never delete a committed readable body. Cleanup failures remain visible. No blocking filesystem/SQLite work occurs in Drop on a Tokio worker. Mechanisms are owned by shared-publication-modes, with explicit drain before command exit.

### Publication modes and imported identity

The delivered native publication subset is documented in [shared publication modes](shared-publication-modes.md). `stage_from_file` returns a verified catalog-bound `StagedObject`; `BatchPublication` combines that handle with validated destination names and a `Publication`. `publish_objects` returns individual results, charged unattempted handles and an explicit time-budget overrun flag. `InheritAndSkipIfSameHash` combines the two policies for Sync. Callers discard or submit all handles before closing the catalog; close drains their cleanup before releasing the directory lock.

Startup orphan scanning remains before bind. Pending-deletion reclamation only selects retired committed IDs, while new staged/renamed IDs stay uniquely owned by their handles and are never added to that queue. Therefore this slice retains ownership through cleanup without adding a separate in-flight-ID set: there is no post-ready orphan sweeper that could race a new ID. Introducing that sweeper still requires the full registration protocol described above. This is a representation choice preserving the protection requirement, not permission to scan unreferenced files concurrently.

Replace uses supplied metadata. InheritFromReplaced copies metadata from the selected replaced generation inside the same transaction as INSERT, preserving a concurrent metadata patch. SkipIfSameHash compares MD5, CRC32C and size atomically and creates no generation when equal. Per-element failures exclude only their element; A/C success with B rejected commits A/C plus a terminal import failure, never a resumable cursor over B. A resumable cursor represents only a contiguous committed prefix.

Imported generation/time/hash publication belongs to shared-imported-identity. Advance last_generation to at least the imported maximum and reject collisions. Schema **14** already stores bucket/object times as signed Unix microseconds. Preserve source precision exactly through microseconds; reject nonzero sub-microsecond digits. Do not introduce millisecond rounding. API rendering/ETags, internal millisecond deadlines and pre-epoch publication limitations remain as documented in [catalog timestamp precision](catalog-timestamp-precision.md); imported precision validation is still pending.

## Filesystem primitive shape

shared-publication-io owns these descriptor-based operations in `src/fsutil.rs`, extending the module if import-dry-run creates it first. This slice creates no runtime helper or dependency.

The delivered native subset and exact callable signatures are recorded in [shared publication I/O](shared-publication-io.md). `clone_into` also supports mutable-source staging by returning Unsupported without an output entry when cloning is unavailable; the later modes owner must then copy and hash in one pass. Unix exchange preserves displaced entries, Windows exchange returns Unsupported for Sync's checked-replace policy, and orphan scanning remains before bind. Publication and maintenance reclaim retired blobs in bounded pages outside SQLite execution while session-owned cleanup markers remain with their owner.

```rust
clone_or_copy_into(src_fd, dst_dir_fd, name) -> Result<CopyMethod, FsError>
durable_replace(dst_dir_fd, staged_name, destination_name, expected_identity)
    -> Result<ReplaceOutcome, FsError>
stat_identity(file_fd) -> Result<FileIdentity, FsError>
```

These are contract shapes, not compilable public declarations. Owned/borrowed descriptor types are platform-appropriate; names must be single validated path components. `CopyMethod` distinguishes Clone, CopyFileRange and ReadWrite for measurement. Only immutable open blobs may use CopyFileRange. Cloning creates a new destination entry, including macOS fclonefileat; no pre-created destination handle is required. Flush the resulting file, perform conditional rename and flush the destination directory. Fsync blobs/ only when creating a shard; reclaim old blobs outside the SQLite execution lane after the reference decision.

For absent destinations use no-replace rename; for existing destinations use exchange and validate the displaced identity under the sync conflict policy. If exchange reveals an unexpected concurrent writer, preserve its bytes and report a conflict, never unlink them as cleanup. Linux uses renameat2 flags; macOS uses supported exclusive/swap primitives or reports unsupported safely. Windows uses bounded read/write and MoveFileExW WRITE_THROUGH. Windows has a check-to-replace race without Unix exchange semantics; preserve/report detected changes and do not claim adversarial writer safety, directory-fsync equivalence or host-power-loss namespace guarantees. Unsupported primitives fail explicitly instead of silently using an overwriting rename. Nanosecond ctime/file identity lives in this same OS boundary. Sync owns conflict/name policy; this contract only requires preservation and typed outcomes.

Sync may select its documented `checked_replace` fallback on a platform/filesystem without exchange, provided that capability failure and the remaining check-to-replace race are explicit in status and diagnostics. This is never reported as exchange/no-loss safety. Preserve a settled old-target snapshot before replacement, recheck identities and installed output, and never exchange back over a later writer. Shared state supplies capability/outcome information; Sync chooses its conflict-preservation policy.

### Accepted synchronization properties

The directory synchronization contract requests durable catalog/root association and activating/completed baseline state, exclusive root ownership through all accepted file jobs, indexed paged fingerprints/generations and recoverable write-back intents. Accepted by shared-contract for shared-sync-state: root identity survives restart, stale bucket incarnations are rejected, activation cannot interpret unobserved files as deletions, and publication/deletion state updates are atomic with their mutations. File durability precedes state advancement; interrupted write-back/conflict intents remain recoverable. Representation/schema/permit choices belong to Shared state; Sync owns marker format, merge/deletion/conflict policy and control authorization. Required runtime evidence remains PENDING, including dual-catalog root contention, interrupted activation, missing/foreign marker handling, file/state crash windows and shutdown ownership. The same implementation-owner / independent-reviewer / coordinator receiving procedure below applies.

## Schema and change journal decisions

Schema changes ship in **separate monotonically increasing versions per schema-owning slice**, each with its own atomic migration. Do not reserve a speculative combined version or rerun schema 14 scaling. The next integrated schema owner selects the next actual version after checking main; parallel branches never assume the same number can be merged unchanged. shared-change-journal, shared-sync-state and shared-import-journal each own their migrations and AUDIT-014 cases. Raise/replace validate_schema's fixed inventory limit before adding tables beyond that bound.

Change-journal retention is at most **65,536 committed change records** per catalog. Write only when a configured consumer exists. Record every object publication, metadata update and deletion (including compose source deletion), bucket create/update/delete and IAM change in the mutation transaction. Retain a durable increasing sequence and oldest retained position; enabling a consumer later or reading before the retained position requires a full rescan. Trim in bounded pages, without an O(N) operation in one transaction. Changes larger than the cap produce a gap, never false complete delivery.

PAP's parent fixture is configuration loaded at startup, not a durable bucket mutation. Do not append one entry per affected object. On a changed parent-policy configuration or new consumer, invalidate that consumer's authorization view and require a full rescan before reporting caught-up. Bucket-local PAP updates remain ordinary bucket changes. Consumers must reconcile permissions from current policy, not replay old authorizations. This is a local fixture contract, not Google propagation simulation. shared-change-journal owns the durable detection/invalidation representation and its restart/gap tests.

### Change journal acceptance

`shared-change-journal` (DESIGN-072) covers atomic origin-bearing publication,
metadata/deletion (including compose source deletion), bucket and IAM records;
disabled logging, monotonic sequences, bounded retention/pages, stable fences,
late activation, restart/gap rescan and durable parent-PAP invalidation.
`catalog-change-journal-migration` (AUDIT-014) covers atomic schema 14 to 15,
rollback/retry, preserved existing data, complete schema inventory validation,
and rejection of newer formats. Broader feature rows remain PARTIAL.

Schema 15 adds `change_journal_state` and `change_journal` in one transaction;
it does not rescale schema-14 timestamps. The schema inventory reads at most
257 entries and rejects catalogs exceeding 256, instead of comparing truncated
32-entry inventories. Back up the entire stopped data directory before upgrading;
schema 15 is rejected by executables supporting only schema 14. Never downgrade
in place. The migration tests retain an unchanged catalog-only snapshot for
schema-14 validation; that snapshot alone is not a usable data-directory backup.

`Catalog::set_change_journal_enabled(true)` activates the aggregate consumer
requirement and returns a baseline `JournalFence`. The consumer owner must keep
logging enabled until its last reader stops. No CLI/environment/file setting is
added. Reopening an enabled catalog disables logging and advances the durable
revision; already disabled catalogs avoid an additional startup write. Restart
therefore conservatively requires a full scan even for unchanged PAP.
This avoids claiming continuity over an interval with no configured reader.
Enabling/disabling advances revision only on a transition; repeated enable does
not invalidate another reader. New readers pass `None` to `read_changes` and
must scan current state before using the returned fence as their cursor.

`read_changes(after, fence, limit)` accepts 1 through 256 records. Capture the
first page's fence and reuse it for later pages; `next` advances only through
that fence, excluding subsequent publications. Missing/invalid/future/reversed
cursors, changed revisions, disabled logging and retention gaps return an empty
page with `rescan_required=true` and a new baseline fence. A reader must never
interpret that empty page as caught-up. Scan and reconcile current state, then
consume changes after the captured baseline; if retention overtakes it, rescan
again. Multiple readers retain independent cursors without per-reader SQL state.

Records retain sequence, typed `Origin` and `ChangeKind`, bucket/project/incarnation,
optional object/generation and metageneration. Deletion captures identity before
the bucket is removed, so same-name recreation remains distinguishable. All
publication protocols use the common commit function; compose source deletion
shares the destination transaction. Rejected mutations roll back sequence and
records together. Retention removes at most 256 oldest positions per append
(normally zero or one), retaining at most 65,536 records even within a large
transaction. The sequence is not reset on disable or restart. The shared feed
can serve future EXT-BEH-032 consumers; provider event mapping, payload/filter
semantics and delivery guarantees remain that feature's unimplemented scope.

Parent PAP stores a canonical sorted/deduplicated project set and advances the
same durable revision on change, in the transaction replacing the temporary
effective fixture. Equal sets avoid a main-catalog write and do not invalidate readers. No object event is
fabricated for a parent-policy change. Readers must consult current policy.

### Change journal evidence

Executed on 2026-10-25 with the locked Rust 1.99.1 dependency graph:

| Test | Verified behavior |
| --- | --- |
| journal_records_atomic_origins_mutations_and_stable_pages | Disabled writes, all five origins, overwrite/metadata/delete, stable multi-page fence, injected journal failure rolls back bucket/sequence, page bounds. |
| journal_retention_activation_restart_and_pap_require_rescan | Actual 65,536-record cap and one-over gap, 256-record pages, disable/re-enable, restart, persisted PAP invalidation and equivalent-set stability. This is a correctness boundary test, not a benchmark. |
| journal_bucket_iam_compose_and_resumable_share_mutation_transactions | Bucket create/update/delete, IAM, compose destination/source deletion and resumable publication retain ordered identities/origins; additional schema entries are rejected. |
| journal_migration_is_atomic_and_inventory_cannot_hide_extra_tables | DDL collision after partial creation rolls back tables/version; retry succeeds; schema 15 cannot be accepted by a schema-14 version guard. |
| journal_schema_upgrade_preserves_objects_policies_and_active_sessions | Real catalog 14-to-15 reopen retains object identity, policy and active session; original stopped catalog snapshot still validates as schema 14. |

`cargo test --lib journal_ --locked --offline`: 7 passed, including the five
new cases and two existing deletion/recovery regressions. The repaired v1
migration regression and all 13 seed tests pass separately, including startup
blocked on an external SQLite writer and shutdown drain. `cargo clippy --lib --tests --locked
--offline -- -D warnings` passes. `cargo xtask dependencies check` reports all
compiler, crate, SDK and container inputs CURRENT; no dependencies were changed.
Final `just check` on source `49db033`: PASS, including formatting, workspace
clippy/tests/doctests, ledger/matrix consistency and shell/release checks.
SDK/container/guide
journeys and performance measurements are not run because HTTP behavior,
containers and guides are unchanged. Power-loss, older-executable full-directory
restore and runtime Sync/event consumers remain outside this slice.

## Accepted Import requests

This table is the **single receiving record** for the Shared state requests in [source storage Delivery boundaries](source-storage-contract.md#delivery-boundaries), accepted by shared-contract on 2026-10-24. Accepted means a delivery obligation, not implemented or verified. The receiver owns the runtime choices within these invariants. The implementation owner supplies named passing tests and durable evidence; the independent reviewer verifies them; the coordinator verifies prerequisite integration and asks the Import owner to reconcile DESIGN-073 before import-filesystem starts. This slice changes only its assigned feature rows and does not edit Import's ledger or historical contract.

| Request | Acceptance and receiver | Execution evidence |
| --- | --- | --- |
| staged handle and cleanup | Accepted: shared-publication-modes supplies stage_from_file/publish_objects, cancellation/ambiguous-commit body retention and command-time reclamation of definitely uncommitted unreferenced handles, typed failures and nonblocking cleanup. | PENDING: test cancellation during actual SQLite commit plus handle drop, committed GET, staged-only drop, exclusion, unattempted work, staging cancellation, cleanup failure and shutdown. |
| imported hold state | Accepted: shared-publication-modes preserves an imported object's explicit existing hold=false when its imported bucket defaultEventBasedHold=true; importing the default must not synthesize object holds. Later ordinary uploads still inherit the stored bucket default. | PENDING: import both values, verify existing object remains unheld/deletable and new ordinary upload is held; preserve explicit imported hold=true where supported and reject unsupported hold semantics. |
| handle budget and overlap | Accepted: shared-publication-modes enforces the 650-ID combined bound and shared 256-handle staging/queued budget above, allowing next-batch staging during commit without another unbounded queue. | PENDING: saturation, time/count closure, carry-over charges, overlap and bounded-memory measurements. |
| revalidation observations | Accepted: shared-import-journal persists per-element inode/file ID and nanosecond ctime, readable in pages of at most 256. After a drifted committed body verifies against recorded digests, atomically replace that element's observations through a bounded typed journal operation. import-filesystem supplies verified observations and never direct SQL. | PENDING: unchanged elements never reread; changed committed elements alone rehash; crash before update repeats safe revalidation, after update avoids repeated reads on unchanged subsequent resume. No full manifest/body retention in memory. |
| catalog-upgrade-policy | Accepted: every later schema owner adds AUDIT-014 migration/rollback/retry/backup/downgrade cases; shared-timestamp-precision already owns schema 14, and replacement-validation owns the combined final gate. | PENDING for later migrations/restoration/platforms; existing schema 14 tests remain PASS, never evidence for unimplemented journals. |

Revalidated observations become the current execution baseline only after digest verification and durable bounded update. An interrupted update leaves the previous baseline; replay can reread the affected element but never accepts changed bytes. Updating observations does not advance the import cursor, complete an import or change its stable export fingerprint. Inode/ctime-only changes within an execution stop retryably; across resume, only drifted committed elements are revalidated. Paths, selected metadata, size, mtime or digest changes remain terminal. Ordinary import bodies are read once while staging; this limited drift exception is already approved by the source contract. Repeated-resume bytes/time require new measurement when implemented.

An unfinished import blocks serve/readiness. Completion and startup_seed completion commit atomically only after fingerprint/count/digest gates and a complete successful prefix. Backup the entire stopped data directory, including catalog and referenced blobs, before upgrade. Restore only into a separate empty destination with its matching executable; no in-place downgrade/reset or marker-only removal. Newer schema rejection must preserve database/WAL bytes and release locks. Backup restoration must verify generations, permissions, active sessions, seed state and representative body digests. Source cutover remains Import's scope; memory export remains separate.

## Shared benchmark profile and regression gate

**Retired as a completion gate (2026-10-25).** gcs-mimic targets local-development scale, up to about a thousand objects per bucket, and delivery speed takes priority over benchmarking. Later slices do not rerun the five-repetition workloads below or compare against the frozen thresholds. A slice whose purpose is performance records one short run at local scale as a fact. The startup CPU calibration and cgroup benchmark cases were withdrawn with the gate; the measured baseline below remains historical evidence of source 60a4f03 only.

Baseline server source is main **60a4f03**. The dedicated harness changes no `src/`, manifests, lockfiles, compiler pin or runtime settings. Build release binaries with the locked graph and record compiler, source/lock digests, OS/architecture, CPU/memory, filesystem and cache policy. Keep TLS/auth/backend/durability/concurrency and payloads equal. The initial native profile uses HTTP/1 loopback, permissive/native defaults, 2 async and 2 blocking workers, SQLite WAL/FULL, disk blobs, concurrency 1 and 64 KiB client blocks. Generator CPU/RSS is excluded from server measurements. No cross-product comparison is claimed.

| Workload | Fixture / observation |
| --- | --- |
| small-publish | 10,000 sequential JSON media POSTs of 1,024 bytes into an empty bucket; validate every response, size, hashes and generation. |
| small-overwrite | Same 10,000 names with a different 1,024-byte body; require increasing generations and matching hashes. Report p50/p95/p99, elapsed time, throughput and zero errors. |
| transfer-1gib | Single JSON media upload and download of exactly 1,073,741,824 bytes; bounded 64 KiB buffers, exact returned bytes/hashes/size. Report both directions and CPU seconds/GiB. |
| startup-10000 / startup-100000 | Process spawn to parsed ready event, followed by a successful readiness probe; cleanly stopped catalogs with exact live-object counts prepared through current publication. Exclude fixture creation, report startup CPU/RSS separately. |
| sync-full-rescan | 10,000 and 100,000 unchanged working-tree entries, then 1% changed; full reconcile to caught-up, bounded pages, hash/racy counts. PENDING until sync exists. |
| api-with-sync | Same 10,000-entry fixture with one rescan/ingest worker and one sequential API client for 10,000 operations; compare API p50/p99 with equivalent API-only control. PENDING until sync exists. |
| import-overlap / import-resume | Full/partial 256-handle batches and repeated inode/ctime-drift resume, with streamed/read-byte counters and API latency under commit overlap. PENDING until the requested interfaces exist. |

Run each available workload **five times** in a coordinator-approved exclusive resource window. Fresh small-object catalogs per repetition; reuse the stopped 100,000-object fixture for startup. Warm OS caches are not evicted; describe sequential warm/mixed-cache behavior and retain all five values, including outliers. Sampling is observational, never a hard peak guarantee: native server RSS at 50 ms, separate server CPU deltas from ps before/after each workload (upload and download are different invocations/intervals), elapsed workload times, latency percentiles, error count and allocated disk use. The directly owned server PID is available before readiness; timeout/interruption cleanup is bounded and tested with a child holding a catalog but never becoming ready. Startup measures observed readiness with 10 ms polling plus process/command overhead, not a microsecond-accurate internal timestamp. Cgroup memory is a separate missing measurement, never inferred from RSS. Disk samples are not temporary-disk high-water marks. Native measurements do not prove the 128 MiB container target.

For each positive lower-is-better metric use baseline median M and observed relative spread S = (max − min) / M. Freeze the allowed regression fraction **max(0.10, 2 × S)** and upper bound M × (1 + fraction), computed from all five repetitions. For throughput use its reciprocal (seconds per unit) before applying the same rule. A later slice runs the same five repetitions and compares its median, with zero errors and identical correctness gates. Any zero sample, or any raw CPU repetition at or below 0.1 second (ten ps quantization ticks), prevents calibration; missing raw CPU also propagates PENDING to normalized seconds/GiB. Every applicable metric remains present, with a reason and missing round numbers when absent/nonnumeric; list inapplicable metrics separately. Use larger workloads/resolution before gating uncalibrated metrics. Broad noisy bounds are visible evidence of an unstable host, not proof of acceptable product performance; retain them and require a quieter rerun before tightening.

The [machine-readable baseline](../evidence/onboarding-state-baseline.json), [raw stdout](../evidence/onboarding-state-benchmark.txt) and [threshold table](../evidence/onboarding-state-benchmark.md#measured-results-and-frozen-thresholds) record the five executed repetitions. The startup-10000 CPU gate remains PENDING for measurement precision; all other recorded applicable metrics have explicit numeric bounds. Source, test and contract review preceded measurement; synchronization, cgroups, Windows, cold cache and host power loss remain outside this native baseline. Contract acceptance never promotes those cases to PASS. These are measurements of source 60a4f03, not the later integration target 96bcaa0.

## Source reconciliation

Checked official sources on 2026-10-24; SHA-256 values pin the fetched HTML bytes. SQLite's single-writer/commit/backup descriptions inform this local project contract; they do not make filesystem publication atomic with SQL. The database backup API alone does not copy external blob files. No Google API inventory or upstream semantics change in this slice.

| Official source | SHA-256 |
| --- | --- |
| [SQLite WAL](https://www.sqlite.org/wal.html) | f3467b530b883d4a00574fe1a898b3d121ed72764ae28cf66941080ac0badb9e |
| [SQLite backup](https://www.sqlite.org/backup.html) | 306b6cca5c7c2f93b648040ae139f14d7d3221c0d16d79b5749e69d808382e04 |
| [Microsoft MoveFileExW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw) | 36cc70da830043ee32e99f3789b8e5b78dfa0721cd9224b024fc59fa81f3f967 |

Reconciled `compatibility/gaps.json`: no open gap directly targets these feature rows. Existing API/authorization/hold gaps retain their scopes; administrative publication must preserve current validation and cannot claim provider parity from this design. The onboarding findings' schema-13 note is historical; the baseline is schema 14. Other historical ledger prose about object listing predates its later delivered slice; this contract neither removes its broad pending scaling case nor redefines listing coverage.
