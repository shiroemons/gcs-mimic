# Shared onboarding state contract

<!-- markdownlint-configure-file {"MD013": false} -->

This contract freezes the Shared state boundary for initialization, synchronization and import. It changes no runtime behavior. DESIGN-062, DESIGN-072, DESIGN-074 and AUDIT-014 remain PARTIAL; UX-002 retains its implemented seed-before-ready scope. Runtime cases below remain PENDING until their owning slices supply executable evidence.

## Acceptance and scope

The first three rows map to the three original `onboarding-state-contract` delivery conditions. The timestamp prerequisite subsequently added to that delivery entry is preserved separately, with its existing schema 14 evidence.

| Case | Feature | Required evidence / owner |
| --- | --- | --- |
| onboarding-state-authority | DESIGN-062 | Validate bucket/project identity, AdminGrant in both profiles, unchanged strict API permissions and bounded mutation/read interfaces; shared-admin-interface and adapter tests. PENDING. |
| onboarding-state-ownership | DESIGN-072 | Shared services own schema, conditions, hashes, generations and recovery; adapters use typed operations and transactional state hooks, never SQL/private blob paths; shared-admin-interface and shared-publication-modes. PENDING. |
| onboarding-state-create-missing | DESIGN-062 | Preserve existing bucket settings/objects on restart, changed project and removed entries; initializer runs before seed, independent of synchronization/public URL settings; shared-startup-phases and init-create-missing. PENDING. |
| onboarding-state-cancellation | DESIGN-074 | Interrupt every pre-bind phase; no ready event on failure, drain side effects before unlocking, remove definitely uncommitted files and retain possibly committed bodies; shared-startup-phases, shared-admin-interface and shared-publication-modes. PENDING. |
| onboarding-state-publication | DESIGN-074 | Per-element SAVEPOINTs, atomic journal/object changes, count/time closure, disk-full stop, cleanup and concurrent reclamation for every publication route; shared-publication-modes. PENDING. |
| onboarding-state-import-holds | DESIGN-074 | Import bucket default hold without synthesizing holds on imported existing objects; later ordinary uploads inherit that default. shared-publication-modes with import-filesystem. PENDING. |
| onboarding-state-import-observations | DESIGN-072 | Bounded committed-element observations, atomic replacement after successful drift revalidation, repeated-resume and crash cases; shared-import-journal and import-filesystem. PENDING. |
| onboarding-state-benchmark | DESIGN-072 | PASS: five isolated same-profile measurements of existing publication, overwrite, 1 GiB transfer and startup at 10,000/100,000 objects, including correctness assertions, raw values, variability and per-metric calibration states/thresholds. Evidence: [native baseline](../evidence/onboarding-state-benchmark.md). |
| onboarding-state-startup-cpu-calibration | DESIGN-072 | PENDING: startup-10000 CPU observations include values at/below the 0.1-second precision boundary; a more precise equivalent-profile measurement is required before freezing a numeric CPU gate. |
| onboarding-state-sync-benchmark | DESIGN-072 | Full rescan and concurrent API/sync p50/p99 using the profile below. PENDING: synchronization is unimplemented; idle API requests are not a substitute. |
| onboarding-state-cgroup-benchmark | DESIGN-072 | Separate server RSS and cgroup memory including page cache under a pinned container profile. PENDING: the initial profile is native macOS, without cgroups. |
| catalog-upgrade-policy | AUDIT-014 | Each schema-owning slice tests migration/rollback/retry and backup restoration with generations, permissions and active sessions; final replacement-validation verifies the combined chain. PENDING for later schemas. Existing timestamp cases stay PASS in their original contract. |

## Identity, authority and create-missing

Bucket identity is its validated global bucket name plus its stored owning project. Object identity is bucket/name/generation; project is not a second bucket namespace. `ensure_bucket(s)` returns `Created` or `Existing` with the stored record. It never patches existing settings or installs IAM grants. Callers decide whether an existing different project blocks their operation; the initializer must diagnose that mismatch without reassignment. Imported names/projects never create public permissions. Removing initialization entries never deletes resources.

Initialization, seed and offline import are local administrative operations on operator-controlled trusted input. Both permissive and strict profiles use an `AdminGrant` minted only inside `storage::admin`; API requests still use their configured authorization profile. The admin boundary removes `BucketAccess::Default`, separates `Permissive` from `Admin(AdminGrant)`, uses the planned Permission/name/generation types and limits unchecked Catalog methods to crate visibility. This contract does not retrofit those types throughout existing records.

## Startup and cancellation

The required order is config validation → TLS/registry → catalog open/recovery → unfinished-import guard → PAP → bucket initializer → seed → sync preflight → bind → ready → full sync reconciliation. Ready means API usability, not a caught-up working tree. Create-missing initialization precedes seed. Neither depends on a public hostname or completion of synchronization.

Every pre-bind phase races shutdown. Cancellation stops admission, lets owned side effects finish, closes the catalog and emits stopped only after lock release. Failure emits no ready event and exits nonzero after close. Side-effecting futures use detached owned work with permits; only side-effect-free waits may be dropped. Temporary-file cleanup must not do synchronous I/O or wait for SQLite on Tokio workers. Catalog close waits for management permits, cleanup and execution; is_healthy includes management admission state.

The baseline still performs object-count-proportional recovery before bind. Moving reclamation after readiness is permitted only after the protection protocol below exists. No startup improvement or bounded recovery duration is claimed here. Startup's signal/PAP/bind cleanup limitations remain owned by shared-startup-phases.

## Bounded shared operations

| Operation | Required contract |
| --- | --- |
| ensure_bucket / ensure_buckets | Create missing resources only; batch existing-resource lookup, insertion and policy/incarnation creation use one transaction. Return Created/Existing in input order with actual stored project; existing records stay unchanged. Validate the complete bounded initializer input before mutation; any batch failure rolls back the entire initializer batch. Commit-time cancellation drains before close. This all-or-nothing initializer operation is distinct from per-element publication SAVEPOINTs. |
| publish_object | Replace mode initially; metadata and conditions checked in the publication transaction; dedicated bounded management admission, including advisory reads. |
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

Replace uses supplied metadata. InheritFromReplaced copies metadata from the selected replaced generation inside the same transaction as INSERT, preserving a concurrent metadata patch. SkipIfSameHash compares MD5, CRC32C and size atomically and creates no generation when equal. Per-element failures exclude only their element; A/C success with B rejected commits A/C plus a terminal import failure, never a resumable cursor over B. A resumable cursor represents only a contiguous committed prefix.

Imported generation/time/hash publication belongs to shared-imported-identity. Advance last_generation to at least the imported maximum and reject collisions. Schema **14** already stores bucket/object times as signed Unix microseconds. Preserve source precision exactly through microseconds; reject nonzero sub-microsecond digits. Do not introduce millisecond rounding. API rendering/ETags, internal millisecond deadlines and pre-epoch publication limitations remain as documented in [catalog timestamp precision](catalog-timestamp-precision.md); imported precision validation is still pending.

## Filesystem primitive shape

shared-publication-io owns these descriptor-based operations in `src/fsutil.rs`, extending the module if import-dry-run creates it first. This slice creates no runtime helper or dependency.

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
