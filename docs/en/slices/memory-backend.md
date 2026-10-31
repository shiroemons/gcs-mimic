# Bounded process-local memory backend

<!-- markdownlint-configure-file {"MD013": false} -->

This implements the scoped RAM object paths of DESIGN-077: ordinary objects,
derived objects, JSON resumable and XML multipart uploads, soft-delete restore,
appendable storage and the existing gRPC object adapters. Inherited API exclusions
remain explicit. Disk remains the default backend.

## Contract

`serve --backend memory` opens SQLite with `Connection::open_in_memory` and keeps
immutable object bodies in a process-local chunk store. It creates no catalog,
staging files, temporary data directory or tmpfs mount, and never opens the
configured `data_dir`. Each process starts empty; stopping or killing it loses
its state. Existing disk data is neither reused nor deleted. `-backend memory`
and `FAKE_GCS_BACKEND=memory` select this same profile. Legacy `filesystem` maps
to native `disk` without adopting foreign-format storage.

Two separate limits use ordinary CLI > environment > JSON precedence:

| Setting | Default | Accounting |
| --- | --- | --- |
| `memory_max_bytes` | 67108864 | Retained allocation capacity plus 128 bytes per blob and per chunk for registry/descriptor overhead; staging, committed generations, history, pins, open readers and cloned response chunks share this budget. Chunk-table capacity beyond the initial 128-byte reservation is also charged, including replacement allocations during growth. |
| `memory_catalog_max_bytes` | 16777216 | Maximum SQLite database pages, rounded down to 4096-byte pages; applied separately to main and temporary databases. The connection also retains its bounded statement cache and 2 MiB page-cache target. |

These are storage limits, not a bound on process RSS, allocator overhead, runtime
threads, connection buffers or cgroup memory. Main and temporary SQLite page
limits are independent; they are not one combined allocation limit. No global
SQLite heap setting can affect another catalog. Ready reports the selected
backend and both configured limits; help lists defaults and ranges. Supplying
RAM limits to the disk backend fails instead of silently ignoring them.

The 64 MiB and 16 MiB values are configurable, provisional defaults for small
local tests. Neither Google nor SQLite prescribes these numbers. They do not
guarantee capacity for every thousand-object workload: payloads, metadata,
history, notification outbox records and simultaneous writes compete for space.
The ordinary `max_object_bytes` default of 1 GiB remains an independent per-upload
ceiling, not a claim that a 1 GiB body fits the default RAM aggregate budget.
A 40 MiB overwrite needs both old and new bodies to coexist before reclamation,
so even 80 MiB excludes descriptors, other objects and retained readers.

Input frames coalesce into blocks of at most 64 KiB, with smaller allocations
near capacity. Chunks copy input bytes, so a small slice cannot retain a large
input backing allocation. Each chunk owns its budget reservation through
`Bytes::from_owner`; the last clone releases it. Tiny input frames consume
descriptor overhead as well as allocation capacity. The final partial block is
compacted only when a second, fallibly allocated and separately charged buffer
fits; otherwise its original capacity stays charged. Allocator over-allocation
requires additional admission instead of assuming exact requested capacity.
Overwrite needs room for both the
old and staged new body. Deleting a generation releases its registry reference,
while readers and queued response chunks retain their charges until dropped.

Instance reset keeps its exclusive activity gate and atomic SQL state clear.
The RAM profile clears its registry only after confirming commit; it does not
grow a disk recovery journal before clearing a full catalog. Existing readers
still own their selected immutable bodies. The disk reset journal is unchanged.

RAM and disk use the same schema, migrations, catalog executor, authorization,
generation allocator, metadata validation, checksum stream and atomic
publication transaction. Incomplete input, checksum mismatch, capacity exhaustion
or a failed condition cannot replace committed bytes or identity. RAM has no
flush, fsync or power-loss recovery promise; disk publication and its existing
fault/recovery seams remain active independently.

## Supported operations and explicit exclusions

The initial profile supports ordinary JSON media/multipart uploads, XML single
PUT, object metadata/list/ranged reads/deletes, version history, shared access and
conditions, administrative streaming fixtures and instance reset. Read-only seed
files stream into the same administrative publication path; bucket initialization
uses the same catalog. Seed sidecars, administrative file batches and atomic reseed
are owned by the separate source-staging slices.

Copy/rewrite/compose and XML object copy also use the shared derived engine; the
specific contract and resource accounting are described below.

Appendable storage uses the shared state machine described below.
The existing gRPC listener can use the same RAM catalog.
Directory synchronization continues to fail configuration validation.
Import targets remain disk catalogs. These operations require later RAM slices;
their disk implementations and existing compatibility claims are unchanged.

`ObjectReader::into_file().await` now returns `io::Result<File>`. RAM objects have
no filesystem descriptor, so conversion returns `Unsupported` without spilling
to disk or panicking. All disk callers propagate the result. Gzip decoding uses
an owned `Read` source so both storage profiles retain bounded decompression.

## Acceptance

- Wire tests cover permissions, failed conditions/checksums, exact bytes, selected
  history, ranges, metadata updates, JSON multipart and XML single-object access.
- Independent processes can use the same unusable data-dir path without touching
  its bytes; restart is empty and missing data directories stay absent.
- Capacity and incomplete-body failures preserve committed state; reset frees
  retained registry data through the existing administrative gate.
- Unit tests retain old/history/reader/wire leases across deletion, prove that
  tiny frames and sliced backing allocations remain bounded, and exercise
  concurrent generation conditions and descriptor-conversion failure.
- SQLite page exhaustion preserves existing buckets and does not constrain an
  independent catalog; seed tests verify exact bytes and unchanged source files.

No performance improvement, whole-process memory bound, complete RAM protocol
coverage or literal Go in-process execution is claimed. Heavy SDK/container
verification belongs to the final integrated source.

## Verification and sources

On 2026-10-30, `cargo check --tests --locked --offline` passed,
`cargo test --locked --offline --lib memory -- --nocapture` passed 12 tests,
and `cargo test --locked --offline --test memory_backend` passed 3 wire tests.
`cargo xtask lint` reported zero production violations. The later isolated
`ram_last_catalog_drop_wakes_and_releases_an_idle_notification_publisher`,
`ram_full_catalog_resets_many_objects_without_growing_a_deletion_journal`,
`ram_pragmas_keep_main_and_temp_page_limits_and_memory_journaling` and
`ram_restore_rejects_before_touching_deleted_metadata_bytes_or_capacity` tests
also passed, as did the 14 existing ordinary-object storage tests and all 6 disk
reset tests, including crash/power-loss boundaries. The bucket-deletion wake
regression passed with 128 multipart sessions and a hidden part. Configuration
input tests passed all 12 cases. Production clippy with `-D warnings`, the scoped
rustfmt check, Markdown lint and `git diff --check` passed.

One successful capacity run stored 1000 objects with 1024 body bytes each and a
128-byte custom metadata value; one object instead used an 8185-byte value plus
its 7-byte key, reaching the 8 KiB custom metadata bound. Main used 187 pages of
4096 bytes (765952 bytes), against a 4096-page limit. TEMP used 3 pages. These are
database-page observations for this workload, not total SQLite heap or RSS.
RSS and cgroup memory were not measured, and no comparative speed claim follows.

Contract references, checked 2026-10-30:

- [SQLite in-memory databases](https://www.sqlite.org/inmemorydb.html): connection-local volatile state and no backing disk file.
- [SQLite page limits](https://www.sqlite.org/pragma.html#pragma_max_page_count): database-page admission, distinct from heap use; in-memory pages cannot be evicted to disk and the page-cache size is a target.
- [SQLite heap limits](https://www.sqlite.org/c3ref/hard_heap_limit64.html): global engine limits affect all connections, so this backend does not use one.
- [Bytes ownership](https://docs.rs/bytes/1.12.1/bytes/struct.Bytes.html#method.from_owner): retained owner lifetimes underpin byte reservations.
- [Rust 1.99.1 allocation contracts](https://doc.rust-lang.org/1.99.1/std/vec/struct.Vec.html#method.try_reserve_exact): fallible reservations may yield excess capacity; nightly fallible shrink APIs are unnecessary here.
- [Google object metadata limits](https://docs.cloud.google.com/storage/quotas): custom metadata has a separate 8 KiB contract.
- [Pinned reference memory backend](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/internal/backend/memory.go) and [object helpers](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/object.go): this new aggregate RAM budget is a visible local safety difference.

Capacity exhaustion keeps the existing 503 `backendError` mapping, with fixed
advice naming RAM limits and disk space. Retrying alone does not create capacity;
release data/readers or change the relevant startup budget. Changing a RAM budget
requires a restart and loses that process's RAM state; it is not a live recovery
operation. Retry with generation conditions after capacity is available.
SQLITE_FULL rollback and subsequent delete/reset/create are tested, including
200 stored objects before reset without first deleting objects to free pages.

## Derived objects

RAM uses the same adapters and derived transfer, source-selection and destination
publication transactions as disk. There is no second copy/rewrite/compose
implementation and no spill-to-disk fallback. Source and destination buckets,
incarnations, selected generations, generation/metageneration conditions, IAM/ACL
grants, customer encryption keys, metadata, storage class, holds, expected hashes,
default ACLs and notification outbox behavior retain the disk subset and error
mapping. The field-by-field contracts and acknowledged upstream uncertainties
remain in [copy](object-copy.md), [rewrite](object-rewrite.md),
[compose](object-compose.md) and [XML copy](xml-object-copy.md).
No provider behavior is broadened by choosing RAM.

Source selection creates independent opaque aliases for immutable chunks in the
same catalog snapshot. Aliases keep selected bytes alive after source deletion
or replacement. Each alias reserves 128 bytes plus its actual chunk-vector
allocation; shared payload allocations keep their original single byte lease.
Transfer output is a separate mutable stage whose buffers use the same aggregate
budget as live objects, history, aliases and response readers. The existing
1..32 source limit and 128 continuable plus 128 ephemeral transfer limits remain.
Continuation lifetime is seven days; abandoned single-call transfers expire after
15 minutes through the existing scheduler, without a new polling task.

Each advance reads and writes at most 64 KiB at a time and updates the same
incremental MD5/CRC32C state. The SQL checkpoint job owns the checked-out RAM
stage until its offset and hash state have been stored. Dropping a caller cannot
race a second writer or return an unacknowledged tail as committed bytes. Capacity
or SQL checkpoint failures truncate to the previous acknowledged prefix. A
partially retained immutable block can continue charging its entire allocation
until that block's last reference drops; truncation never hides that charge.
Chunk tables retain their capacity lease after truncation, and growing a table
reserves both old and new allocations before replacing it. RAM advances yield
between blocks so a large transfer cannot starve shutdown or other connections.

Publication creates an immutable candidate by sharing staged chunks and charging
another 128 bytes plus the actual chunk-vector allocation. The shared transaction
checks the destination, allocates its generation, retires history, queues events
and binds protocol completion. Callback or transaction rollback preserves the
previous destination. A retry can reuse the candidate; ambiguous commit cleanup
checks SQL ownership before releasing pins or stages. Cancel/expiry discard owned
inputs and output, while published objects and readers retain their byte leases.
No disk flush, rename, fsync or power-loss guarantee applies to RAM; those paths
and recovery tests remain active for disk.

The byte pin does not bypass protocol validation. JSON rewrite continuations
still recheck source metadata, generation selection and grants; a source deletion
or an incompatible change can return 412 even while the byte pin remains valid.

Same-class, same-location rewrites may complete in one call even when a smaller
`maxBytesRewrittenPerCall` is supplied, as in the existing disk contract. A
storage-class-changing rewrite exercises continuation tokens, acknowledged byte
counts and response replay. RAM state and tokens disappear with the process;
there is no durable resume after restart.

The derived slice's targeted checks on 2026-10-30 passed:

- `cargo check --tests --locked --offline`.
- `cargo test --locked --offline --lib storage::memory:: -- --nocapture`: eight
  tests, including retained chunk-table capacity, rollback, aliases and final
  reader/chunk leases.
- `cargo test --locked --offline --lib ram_derived -- --nocapture`: four tests
  covering source deletion and incremental hashes, capacity/SQL checkpoint
  failure, callback source-deletion rollback, competing destination conditions,
  history, cancel, expiry, abandoned begin and closed-catalog rejection.
- `cargo test --locked --offline --test memory_backend`: four wire tests,
  including strict permissions, copy metadata and conditions, ordered compose
  bytes/composite hashes, atomic source disposal, multiblock rewrite token replay,
  XML copy and retained generation reads without creating `data_dir`.
- The broader `derived` library filter passed 31 tests in the sandbox. Its three
  existing HTTP fault tests initially failed at localhost bind with
  `PermissionDenied`; rerunning those three with local socket permission passed
  crash, power-loss and ambiguous-commit recovery tests. No negative test was
  removed or weakened.

The RAM profile itself has no restart recovery or power-loss claim.
Heavy SDK/container
verification and the project-wide compatibility ledger remain integration work.

## JSON resumable uploads

JSON resumable initiation, sequential chunks, overlap/discard, status, finalization,
completed-response replay, cancellation and seven-day expiry use the existing
[JSON resumable contract](resumable-upload.md). That contract owns accepted
metadata, Content-Range variants, optional totals, whole-object checksum headers,
conditions, capability delegation, permission rechecks, response projection,
308 override and explicitly unsupported fields. Its named provider uncertainties
and local choices, including multi-request MD5 behavior, are unchanged. Enabling
RAM adds no alternate upload-command protocol or new permissive authorization.

The shared storage I/O enum owns either a disk file or a RAM draft. The same
receive loop discards acknowledged overlaps, accepts new tail bytes only, and
requires complete transport EOF before checkpointing. RAM maintains an
incremental MD5/CRC32C state instead of reading its entire staged body again at
finalization. Each resumable stage also reserves `size_of::<Hasher>()` bytes for
that fixed-size state. Ordinary uploads and derived transfers retain their
existing checksum engines, without double hashing.

The SQL checkpoint job owns the output until its offset/total/options update
succeeds, then acknowledges the RAM offset and hash clone together. On failure,
draft cleanup restores both the previous byte prefix and its checksum state.
Body cancellation before EOF acknowledges no tail. After complete EOF, owned
work can finish a queued checkpoint or publication even if its caller disappears;
query status or replay the completed response before retrying. Catalog shutdown
wakes a stalled receiver and waits for owned cleanup before clearing RAM state.

Finalization shares the existing generation/metadata/authorization/publication
transaction. A RAM immutable candidate shares staged chunks and reserves its own
descriptor allocation. A retryable failure preserves the acknowledged stage and
previous live object; cleanup resolves an ambiguous completion before removing
anything referenced by committed SQL. A finalization error can occur after the
complete body was checkpointed: that complete offset stays available for status
and an empty final retry, while the old destination remains visible. Terminal
checksum/condition failures invalidate the session under the existing contract.

Status preparation goes through the catalog's same file/draft representation;
the adapter never opens a RAM staging path. Cancel, expiry and reset remove only
owned stages/candidates. A completed object's live or historical reference and
an open reader outlive session cleanup. The unchanged 128 retained-session limit
includes active, completed and cancelled records until their original deadline.
Session state consumes the same catalog-page budget; staged bytes compete with
live/history/pinned/reader bytes for the aggregate blob budget.

RAM capabilities and all their checkpoint state disappear at process exit.
Restarting with the same `data_dir` does not recover sessions or touch that path.
Disk still uses file flush/sync, reference-aware publication and startup recovery;
RAM does not add a flush, fsync or power-loss durability claim.

On 2026-10-30, `cargo check --tests --locked --offline` passed and
`cargo test --locked --offline --lib resumable -- --nocapture` passed all 121
tests, including seven RAM-specific cases, disk write/flush/sync and crash/power-loss
faults, callback/commit retry and capability/condition checks. The first broader
run passed 115 of 119 tests; four failed only because the exact StorageFull
message expectation still named disk-only advice. Updating that one golden
branch to the existing fixed RAM/disk message preserved all negative assertions
and made the full rerun pass. Native HTTP tests required local socket permission.
This is native test coverage, not an additional heavy SDK run.

`cargo test --locked --offline --test memory_backend` passed all five tests.
The new strict-mode resumable wire case verifies anonymous capability use,
256 KiB prefixes, status/no-308 override, differing duplicate bytes, gap rejection,
whole-object CRC32C, final metadata, completion replay, selected history,
checksum-failure preservation, cancel tombstones and process restart isolation
without creating the configured data directory. Eight chunk-store tests and
four RAM derived tests also passed after the I/O refactor. `cargo xtask lint`
reported zero production violations and one pre-existing reasoned suppression.

## XML multipart uploads

The same [XML multipart contract](xml-multipart-upload.md) applies to RAM:
initiation, part writes/replacement, part/upload listing, ordered completion and
abort reuse their adapters, authorization, CSEK checks, session incarnations,
leases and SQL transactions. Upload IDs remain identifiers requiring current
permissions, unlike JSON resumable capability URIs. Part numbers, minimum
nonfinal sizes, manifest grammar, retained-session/byte limits, local expiry,
unsupported fields and provider uncertainties are unchanged.

Parts use the ordinary `UploadSink` and bounded EOF/hash loop. A RAM part is sealed
under its opaque ID before its SQL checkpoint; a failed or ambiguous checkpoint
reclaims only candidates without a committed part reference. Incomplete bodies,
failed checksums, capacity exhaustion and failed SQL cannot replace a selected
part. Completed replacement retires the old part through the existing journal.
The shared body loop also wakes on catalog shutdown, without a new worker or
unbounded cancellation queue.

Completion uses the same 64 KiB assembly loop and rechecks each selected part's
MD5/CRC32C while computing the output hashes. The upload's exclusive lease keeps
part identities stable; immutable RAM input handles retain their selected bytes.
RAM assembly writes an ordinary draft, whose offset and hash state enter the
same ephemeral derived-transfer checkpoint used by existing publication.
The assembly loop adds no whole-payload buffer or catalog-wide collection.

Live/history/readers, retained hidden parts, replacement stages, assembled output
and immutable publication candidates all compete for the same byte budget.
Completion can therefore fail even when every part fitted separately. It preserves
the prior destination and committed parts for retry after releasing capacity.
The destination, outbox, session retirement and part-deletion journal share one
transaction. A failed publication rolls them back together; a committed object's
MD5 remains externally unavailable and its ETag stays opaque, as on disk.

RAM journal draining checks object/part/derived ownership before releasing an
immutable candidate or draft. It performs no filesystem unlink, rename or sync.
Abort, expiry, reset and process shutdown release their owned hidden bytes;
ordinary immutable readers retain their last-reference leases. Disk keeps its
existing publication, flush/sync, reclamation and recovery fault seams.

On 2026-10-30, unfiltered `cargo check --tests --locked --offline` passed.
`cargo test --locked --offline --lib multipart -- --nocapture` passed 26 tests,
including six new RAM cases and existing disk failed-write/flush/sync,
checkpoint/commit, crash and power-loss cases. The independent failed-final-write
regression also passed after adapting its existing caller to the shared sink.
`cargo test --locked --offline --test memory_backend` passed all six tests.
Its XML case covers a 5 MiB nonfinal part, failed checksum and successful
replacement, sparse completion order, part/upload listing, strict anonymous
denial, metadata/CRC-only output, history, completed retry, copy of a multipart
object and abort, without creating `data_dir`. `cargo xtask lint` reported zero
production violations and one pre-existing reasoned suppression.

The part checkpoint reads the clock inside its SQL transaction. A regression
holds the SQL worker until the upload expires and verifies that the queued
replacement is rejected without changing the old part identity, size, hashes
or live object.

## Soft-delete restore

RAM restore uses the same [scoped restore contract](sdk-soft-delete-restore.md),
including its explicit provider differences and unsupported ACL/HNS/Autoclass
options. The [official restore method](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/restore)
was checked on 2026-10-31. A restored generation shares immutable source chunks;
its own header and chunk-vector allocation count against the aggregate budget.
The selected retained generation remains independently readable by the existing
metadata API until expiry. Restoration neither extends its retention nor removes it.

Authorization, live-object conditions, generation allocation, default ACLs,
version-history retirement and notification outbox publication share the existing
transaction. The existing deletion journal owns failed aliases and rechecks
committed references before release. Capacity or SQL failure preserves the source
and destination. RAM performs no filesystem link, unlink, rename or sync here;
the disk publication and recovery paths are unchanged. Returned timestamps are
normalized to the stored object microsecond precision.

Unfiltered `cargo check --tests --locked --offline` passed. `cargo test --locked
--offline --lib ram_restore -- --nocapture` passed both RAM cases: a budget too
small for a payload copy still restores shared bytes, retained metadata and an
existing reader survive, alias capacity exhaustion and injected SQL failure leave
no pending candidates, retry succeeds, live conditions prevent replacement, and
a nanosecond clock returns the exact persisted microsecond record.
`cargo test --locked --offline --lib soft_delete -- --nocapture` passed nine
existing disk/configuration/concurrency cases. The seven tests in `cargo test
--locked --offline --test memory_backend` passed, including strict restore/create
permissions, unsupported fields, live/history bytes, conditions, retained source,
completed live-generation rejection, no `data_dir` creation and process isolation.

## Appendable storage

The RAM storage implementation retains the [scoped Rapid simulation](grpc-v2-rapid-append.md):
creation reserves one generation, reads expose the acknowledged prefix, flush
publishes its size and incremental CRC32C, takeover rotates the write handle and
discards the unacknowledged tail, and finalization uses ordinary publication with
the reserved generation. Existing conditions, metadata, default ACLs, errors,
unsupported options and visibility limitations are unchanged. This is neither
zonal-bucket fidelity nor RAM restart durability. The protobuf and official
[Rapid contract](https://docs.cloud.google.com/storage/docs/rapid/rapid-bucket)
were checked on 2026-10-31. Transport coverage is recorded separately below.

A mutable RAM draft holds received bytes. Flush creates a charged immutable
snapshot before the SQL checkpoint; a rollback guard restores the prior snapshot
unless the owned SQL job acknowledges it. Readers retain their selected snapshot
and last-reference leases. Draft chunks, old readers, candidate vectors, other
live/history/hidden data and finalization output share the same byte budget.
Capacity failure preserves the acknowledged prefix and finalization remains
retryable after releasing capacity. No payload-sized contiguous buffer is added.

RAM creation allocates its draft inside the owned SQL job, after validation.
Cancellation before that job creates no orphan; cancellation after enqueue lets
the transaction finish. A failed or ambiguous commit probes ownership and retains
possibly committed bytes. Finalization retires the old RAM draft and immutable ID
through the existing deletion journal in its publication transaction, so cleanup
does not depend on the finalizer's caller surviving. Disk does not receive that
RAM-only journal entry or any new filesystem step.

In-flight writes, syncs and truncations retain both the exclusive slot guard and
the registry's counted handle. Cancelling a takeover cannot let a later writer
use a different lock while its truncation is still running. RAM catalogs have
separate slot namespaces. A retired writer rejects even an overlapping no-op
append, as well as flush. Reset stays busy while a writer holds its activity lease.

`cargo test --locked --offline --lib appendable -- --nocapture` passed 21 tests,
including seven RAM-related cases: prefix/snapshot/catalog isolation, quota at
flush and finalization with reader-held capacity, SQL failure/outbox rollback,
queued flush cancellation, sixteen cancelled creations under a 600-byte budget,
an aborted finalizer whose SQL job commits and reclaims its original bytes, and
cancelled takeovers on disk and RAM. Existing disk write/sync faults, crash,
restart, reserved publication, ACLs and bucket recreation cases passed.
`cargo test --locked --offline --lib grpc::v2::append -- --nocapture` also passed
the six existing disk adapter cases. Heavy SDK tests are deferred to integration.

## gRPC transport

`--backend memory --grpc-listen 127.0.0.1:0` uses the existing HTTP/2 listener,
Storage v2 adapter and pinned legacy v1 migration subset with the same catalog as
JSON and XML. Listener address and advertised API/public endpoints retain their
existing meanings. UNIX-listener combination restrictions, strict authentication
and unsupported authentication fields are unchanged. Directory synchronization
remains explicitly unsupported on RAM.

The supported fields, masks, preconditions, response shapes, checksum behavior,
errors and exclusions are those of the existing [v2 object I/O](grpc-v2-object-io.md),
[update/copy](grpc-v2-update-copy.md), [multi-range reads](grpc-v2-bidi-read.md),
[Rapid simulation](grpc-v2-rapid-append.md) and [legacy v1](grpc-v1-legacy.md) slices.
The current [official RPC reference](https://docs.cloud.google.com/storage/docs/reference/rpc/google.storage.v2)
was checked on 2026-10-31; the generated protobuf pins and digests remain those in
the [foundation contract](grpc-v2-foundation.md). No second protocol engine or
generated-code change is introduced.

Ordinary, resumable, derived, restored and appendable writes use the RAM storage
representations described above. Capacity exhaustion preserves committed objects
and resumable acknowledged offsets, mapped through the existing gRPC error path.
The profile does not promise persistence after process restart. Existing 4 MiB
protobuf-message limits, two 64 KiB upload slots, 64 KiB output chunks, connection
and stream admission, silence bounds, cancellation and shutdown drain remain in
force. Decode buffers and transport queues are separately bounded; they are not
part of the retained-storage byte budget or a process-RSS limit.

The RAM-specific tests pass real gRPC-framed protobuf messages through the
generated services, including strict write/read permissions, checked chunks,
whole-object hash failure, retained generations, compose/rewrite/move, metadata
updates, multi-range reads, capacity rollback, resumable reconnect/status/cancel,
restore permissions/conditions, appendable takeover/finalization and legacy v1
insert/get. These service-framing tests do not exercise a network HTTP/2 client.
The separate native `memory_backend` listener test sends protobuf GetBucket over
HTTP/2 to the running CLI process, reads its terminal gRPC status and verifies
that JSON and gRPC share the bucket without creating `data_dir`.

Final scoped native checks on 2026-10-31 passed:

- Unfiltered `cargo check --tests --locked --offline` before target tests.
- `cargo test --locked --offline --lib grpc:: -- --nocapture`: 47 cases,
  including four RAM framing cases and the existing bounded HTTP/2 shutdown,
  inactivity, admission, disk adapter and legacy cases. The active-stream fixture
  distinguishes intentional client teardown from an unexpected body error.
- `cargo test --locked --offline --lib ram_ -- --nocapture`: 42 cross-cutting RAM
  cases, including the independently owned reset/reseed and capacity cases.
- `cargo test --locked --offline --test memory_backend -- --nocapture`: eight
  wire tests, including the running process's HTTP/2 protobuf response.
- `cargo test --locked --offline --lib appendable -- --nocapture`: 21 cases.
- `cargo test --locked --offline --lib resumable -- --nocapture`: 122 cases.
- `cargo test --locked --offline --lib multipart -- --nocapture`: 26 cases.
- `cargo test --locked --offline --lib transfer_io:: -- --nocapture`: the rollback
  flush/sync failure case. Rollback reuses `make_durable`, so Tokio's retained
  background-write error is observed before synchronization. Existing disk crash,
  power-loss, failed-write/flush/sync and ambiguous-commit cases remain passing.
- `cargo xtask lint`: zero production violations, with one pre-existing reasoned
  stringified-error suppression. Scoped rustfmt, Markdown lint and diff checks
  also passed.

The SDK/container integration run, full-repository final validation and central
compatibility ledger updates are separate integration work. No new dependency,
benchmark, real-cloud request or RAM power-loss/restart guarantee is introduced.
