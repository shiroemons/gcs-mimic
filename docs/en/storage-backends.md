# Storage backend investigation

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

## Scope and recommendation

Reviewed on **2026-10-22**. This is a source investigation and engineering
assessment. No candidate benchmark, backend implementation or data migration was
executed. Current implementation statuses and recovery evidence are unchanged.

Use a transactional metadata catalog and opaque immutable object files as the
durable default. Keep SQLite for the catalog: indexed listing, atomic metadata
conditions and short transactions fit the required behavior without a separate
server. Stream object bodies outside the database transaction. This is the
recommended fit for this repository, not a universal storage or speed ranking.

Migration from fake-gcs-server is a product requirement. Reading its seed,
filesystem and live-export formats does not require adopting its internal
layout or replacing the metadata engine. The contract is in
`docs/en/fake-gcs-server-migration.md`; implementation remains pending.
Do not introduce a generic backend framework before a concrete second runtime
backend needs one.

Replacement also requires bidirectional synchronization with an inspectable
bucket/object directory. One-time import alone is insufficient. SQLite can
remain the index/catalog while a visible working tree feeds safe ingestion and
reflects API mutations. This synchronization is not implemented; conflict and
name-mapping decisions remain open. The
[filesystem investigation](fake-gcs-server-storage.md) records source behavior,
executed macOS edit/reseed cases and the required follow-up.

## Selection criteria

Evaluate every candidate against the same requirements:

- Bounded body streaming and indexed paging as the object catalog grows.
- Atomic generation/metageneration checks with the metadata mutation.
- Preservation of logical names, versions, metadata and permissions.
- Defined interruption, crash, backup and recovery behavior.
- Explicit platform/filesystem constraints and a disposable-test profile.
- Measured resource cost under equivalent durability and authentication.

Inspectable files are useful for fixtures and exports, but do not replace
indexes, conditional-update synchronization or recovery. A compact dependency
does not establish lower total complexity once these guarantees are included.

## Observed implementations

### gcs-mimic: SQLite metadata and separate files

`src/storage/catalog.rs` uses one operational SQLite connection, WAL,
synchronous FULL, a 2 MiB configured cache, mmap disabled and bounded admission
with one execution permit. Indexed queries and recovery pages avoid loading the
whole catalog. Schema v6 includes seed initialization and bucket IAM policy state.

`src/storage/objects.rs` streams into opaque files, publishes immutable blobs,
then checks conditions and changes references in a short SQL transaction.
SQLite is embedded and needs no separately managed database server. Current
application serialization means WAL's general reader/writer concurrency is not
a measured application benefit. WAL still has one writer, checkpoint work and
same-host filesystem constraints.

A blob rename and SQL commit are separate operations. The deletion journal and
startup reconciliation bridge that boundary. Database atomicity alone does not
make both stores one atomic transaction. Selected process-crash cases pass;
every fsync window, host power loss and all-platform behavior remain unverified.
Existing results remain in `docs/en/evidence/object-media.md`.

#### Resumable session storage contract

The [frozen resumable contract](slices/resumable-upload.md) extends this model
with an indexed persistent session table and opaque staging files on the same
data volume as immutable blobs. This is a required design, not executed backend
or migration evidence. The table must retain capability identity, bucket
incarnation and initiating authorization context, bounded metadata/conditions,
optional total, committed offset, fixed seven-day deadline, terminal state and
a bounded completion resource. Sessions are not ordinary listable objects.

Each complete chunk must sync its appended bytes before committing its offset;
an interrupted request's uncommitted tail must be truncated. Finalization must
publish the immutable file before committing the Object and completion snapshot
together with atomic authorization/preconditions and the deletion journal.
Recovery must retain active unexpired staging, restore a precommit rename,
resolve ambiguous commits through references, and reject missing/truncated
acknowledged prefixes before readiness. A blanket staging purge would destroy
resumable progress and must be replaced with reference-aware bounded recovery.
See the [session engine design](design.md#resumable-session-engine-contract).

The planned 128 retained-session limit and existing object-size cap bound staging
growth, with in-progress API tails bounded by `max_connections` (default 32)
rather than a separate upload cap; they do not reserve free disk space.
Disk-full/write/sync failures must surface. Deadline-indexed maintenance must
reclaim expired staging and terminal records in bounded batches without busy
polling, and journal/retry failed unlinks. No whole-body buffering, new backend
dependency, comparative speed or additional durability result is implied.

### fake-gcs-server: files, xattrs or sidecars, and memory

The filesystem backend selects metadata xattrs where supported and otherwise
uses JSON sidecars. Object xattrs contain serialized `user.metadata`; bucket
attributes use a separate file. Content is written before metadata. The Unix
replacement helper uses renameio, which does not make a separate metadata
update part of the same transaction.

The reviewed `CreateObject` buffers the complete input while holding the
backend lock. Listing walks files and collects metadata. Disk persistence alone
therefore does not imply bounded upload memory or indexed paging. This
filesystem implementation rejects versioning.

Its separate memory backend retains active and archived objects in process.
It suits disposable fixtures but memory grows with retained bytes, and stopping
the process loses state. These observations describe the inspected source;
a filesystem implementation could instead add streaming and indexes.

### gcp-storage-emulator: files and one JSON catalog

Disk mode writes object files below the configured storage directory. The
`.meta` JSON catalog holds bucket, object, resumable, IAM and soft-delete maps.
Startup loads it into memory; mutations serialize the whole catalog again.

The file store reads and writes complete byte values. Resumable append reads
the accumulated content, concatenates a chunk and writes it back. These methods
do not define a shared body/catalog transaction or recovery journal.
Memory mode retains byte values and directory names in process collections.

This compact model is convenient for small fixtures. Metadata memory and
serialization grow with catalog size, and full-byte operations conflict with
this project's large-object streaming requirements. Stored IAM data alone
does not establish permission enforcement.

## Advantages and disadvantages

Source observations and requirement-based judgments are separate from measured
performance. No row below claims a benchmark win.

| Approach | Advantages | Disadvantages | Decision |
| --- | --- | --- | --- |
| SQLite metadata plus opaque files | Indexed queries and atomic metadata conditions; no DB server | Cross-store recovery and coherent backup; serialized writes and checkpoints | Recommended durable default |
| SQLite metadata plus BLOB bodies | Metadata/body can share a database transaction; one database backup domain | Large bodies need chunk design; long writes contend; database growth and limits require validation | Compare only if transactional body storage becomes a concrete requirement |
| Embedded transactional key/value store such as redb plus files | Rust-native ACID/MVCC catalog candidate | Secondary indexes, schema upgrades and query planning become application work; external-file atomicity remains | Candidate for a measured catalog bottleneck or dependency constraint |
| Files plus xattrs | Fewer visible sidecars; attributes follow the inode | Filesystem/copy-tool portability and multi-part consistency | Useful source format with explicit platform checks |
| Files plus per-object JSON | Inspectable metadata; no xattr requirement | File count, listing indexes and multi-file recovery | Useful fixture/export format |
| Files plus whole-catalog JSON | Small implementation; readable snapshot | Whole-catalog memory and rewrites; body/catalog recovery | Small fixtures under an explicitly weaker profile |
| In-memory collections | Simple reset; no persistent-file administration | Content-sized memory; no restart durability; parity work | Optional bounded disposable profile |

SQLite incremental BLOB I/O avoids loading a whole value into memory, but does
not remove value-size limits or write-transaction constraints. A single BLOB
does not cover the entire GCS object-size contract; chunking adds schema,
publication and recovery work. Historical SQLite internal/external BLOB
benchmarks do not establish a size cutoff for this application.

redb describes ACID transactions, MVCC and crash recovery. Those catalog
properties do not solve a separate blob-file publication boundary. Its upstream
benchmarks do not establish a win for this project's queries and workloads.

Keep raw logical object names separate from host paths. Slashes, backslashes
and file/directory collisions must not change stored identities. A readable
import/export format need not become the internal blob layout.

## Backup and migration requirements

A SQLite backup API snapshot covers the database, not external blob files.
Copying a live database file alone is also insufficient in WAL mode.
A coherent application backup must freeze mutations and reclamation, or pin
every immutable blob referenced by the chosen catalog snapshot until copying
and validation complete. Restore must verify the complete referenced set.
For a backup that includes resumable progress, the set also includes session
records and their acknowledged staging prefixes. Freeze session mutations while
capturing/copying that state: pinning immutable object blobs alone cannot protect
mutable staging. Resumable backup/restore execution is not established here.

For migration, read a stable source into a separate destination. Preserve
xattrs when the source uses them. Verify names, generations, timestamps,
metadata, lengths and hashes before endpoint cutover. Unsupported source
attributes block a lossless claim; do not silently drop them.

Keep an interruption journal and bounded cursors. Test restart, changed source,
disk full, conflicts and non-destructive retry. Reopening an old snapshot after
new writes is not a safe rollback unless those writes are replayed or the
verification window remained write-frozen.

## What would justify a different default

Reconsider the engine after an observed need: catalog admission/query or
checkpoint bottlenecks, unsupported deployment filesystems, distribution
constraints, or startup/reset cost for a disposable profile.

Compare equivalent operations with matching durability, checksums, permissions,
TLS, resource limits, architecture and cache state. Use 0 B through 1 GiB
bodies, growing metadata catalogs and defined concurrency. Include upload,
range read, listing, overwrite and restart.

Record process RSS separately from cgroup/cache, CPU time, throughput, latency
percentiles, errors, temporary disk and recovery duration. Also run naming,
atomic-precondition, generation, restart/crash and seed-replay cases.
Do not weaken guarantees for a favorable result. Report unequal profiles
separately. No new dependency or default change is authorized by this report.

## Sources consulted

The comparison versions are identified in the [detailed comparison](feature-comparison.md).

| Source | Material |
| --- | --- |
| gcs-mimic | `src/storage/catalog.rs`, `src/storage/objects.rs`, existing recovery evidence |
| fake-gcs-server | `internal/backend/fs.go`, `memory.go`, `metadata_file.go`, `metadata_xattr.go`, `writefile_unix.go` |
| gcp-storage-emulator | `src/gcp_storage_emulator/storage.py`, `settings.py` |
| SQLite official documentation | WAL, atomic commit, online backup, incremental BLOB I/O, limits, internal versus external BLOBs |
| redb project documentation | Transaction model, MVCC and crash-recovery description |

External product tests, imports, images and equivalent-workload benchmarks were
not run. No comparative CPU, memory, latency or power-loss result is claimed.
