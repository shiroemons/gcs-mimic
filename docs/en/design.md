# Google Cloud Storage emulator design

<!-- markdownlint-configure-file {"MD013": false} -->

## Product goal

Build a resource-efficient Cloud Storage emulator for local development and CI. Ship a Rust binary and Docker image with familiar fixture loading and endpoint configuration. Expand compatibility incrementally while keeping a precise, evidence-backed progress ledger.

P0 implements a bounded HTTP/1 health/readiness shell, CLI/configuration, Clock injection, Docker/Compose and Rust ledger/freshness tooling. P1 adds persistent JSON bucket create/get/list/delete and object media/multipart upload, metadata/list/update/download/delete with checksums and atomic generation conditions. P2 adds explicit permissive/strict authentication profiles, a local principal registry, additive project/bucket predefined-role enforcement, persistent scoped bucket IAM policies and a public URL helper. Path-style XML GET/HEAD and strict bucket allUsers/allAuthenticatedUsers grants are implemented for the documented read subset; object ACLs and signed URLs remain unimplemented. The JSON resumable engine below has wire, SDK, restart, guide and native resource [evidence](evidence/resumable-upload.md), with explicit remaining boundaries. No comparative performance result is claimed. Current status is authoritative only in `compatibility/features.json`. The project name, CLI binary and repository slug are **gcs-mimic**. Selected description: **A lightweight Google Cloud Storage emulator in Rust.**

The target is publicly observable protocol and behavior compatibility. A single local process cannot reproduce Google's physical regional redundancy, production durability/SLA, billing system, identity issuance, managed ML inference, or network placement. Represent those boundaries explicitly and provide useful local simulations or optional integrations. Do not count a simulated billing charge as a reproduced billing service.

The product also targets a complete behavioral superset of fake-gcs-server
after the documented environment-key rename and explicit data conversion.
This is a product goal, not an achieved replacement claim. The migration
contract is `docs/en/fake-gcs-server-migration.md`; it includes Go embedding,
legacy Storage v1 gRPC, TLS/defaults, helpers/events and both backend modes.
Google strict behavior and permissive source behavior need separate profiles
and evidence. The existing permissive auth mode is not a complete legacy profile.

P0-P7 group coverage; the ordered capabilities and prerequisites in
`delivery_plan` determine execution. Complete a bounded useful scope before
expanding it, retaining broader PARTIAL rows and all existing evidence.

## Compatibility surfaces

Cover core JSON REST, XML operations, Storage v2 gRPC, Storage Control v2, FeatureConfigs, Storage Intelligence, Storage Insights, Storage Batch Operations, and optional Cloud Storage MCP tools. Keep separate rows for different transports even when they share domain logic. Operation counts are inventory counts, not a count of independent product features.

Method names alone are insufficient. Compatibility includes required and optional parameters, field masks, defaults, errors, response headers, ACL/IAM permissions, pagination, state transitions, consistency, timing and interactions. The inventory is a dated public-contract baseline. Expand acceptance cases when a feature is selected, and refresh sources before implementing fast-moving or preview features.

Reference sources: [JSON API](https://docs.cloud.google.com/storage/docs/json_api/v1), [XML API](https://docs.cloud.google.com/storage/docs/xml-api/reference-methods), [Storage v2 RPC](https://docs.cloud.google.com/storage/docs/reference/rpc/google.storage.v2), [Storage Control v2](https://docs.cloud.google.com/storage/docs/reference/rpc/google.storage.control.v2).

## Implemented foundation and extension boundaries

One application crate contains `config`, `clock`, `auth`, `url`, `server`, `bucket`, `object`, `json`, `xml` and `storage`; `main` owns runtime creation, signals, identity-registry loading, catalog opening and listener setup, plus the side-effect-free `url public` command. JSON parsing/rendering remains separate from the transport-independent records/catalog. The server routes bucket CRUD and IAM get/set, object media/multipart/resumable upload, list/get/update/delete and internal probes; unknown routes return 404 without reading bodies. The XML adapter serves path-style object GET/HEAD with shared authorization and selected immutable bytes; gRPC is unavailable. `xtask` owns ledger validation/rendering and explicit online dependency checks. Existing `serde_json` supplies bounded configuration, bucket metadata and policy parsing.

| Boundary | Contract for subsequent slices |
| --- | --- |
| JSON adapter | Own JSON API parsing/errors/headers and media/upload routes; preserve raw URI until protocol decoding. |
| XML adapter | Own path/virtual-host canonicalization, XML errors, CORS and signed request parsing. |
| gRPC adapter | Own pinned protobuf, stream framing, status and metadata; no gRPC listener in P0. |
| Shared services | Own authorization, generations, atomic preconditions and state transitions; transport types do not enter persistence. |
| Metadata catalog | Own indexed transactions, short atomic condition checks and reference publication; no transaction across body transfer. |
| Blob storage | Own opaque blob IDs, bounded staging streams and durable immutable publication/deletion. Logical object names never become filesystem paths. |
| Clock | Inject business wall time through `Clock::now`; production uses `SystemClock`, tests supply deterministic time. Socket/drain deadlines use Tokio monotonic time independently. |

The bucket/object catalog is concrete; no generic persistence trait is frozen for future work. Recovery gates readiness. The transport-independent `auth` module resolves strict-mode callers and evaluates permissions from a pinned role snapshot; the JSON adapter maps routes to permissions and renders 401/403 errors. Catalog transactions recheck the observed bucket owner, effective project/bucket grants and overwrite-delete permission against the policy current in that transaction. Bucket insert/list remain project scoped. Whole bucket/object CRUD rows retain their existing limitations. Existing SPEC-001/002/003/004/006/007 gaps do not add bucket CRUD semantics. SPEC-008..013 record object condition precedence, direct-200 downloads, checksum/header/MIME choices and retry-token deviations; they remain open for their affected features.

The server caps connections (default 32), headers (64 and 16 KiB), shutdown drain
(5 seconds), async workers (2) and blocking workers (2). Connection admission stops
when full; the requested kernel backlog is 128. Every response closes its connection.
Bucket insert and IAM PUT JSON are capped at 64 KiB; probe and other bucket methods reject bodies.
Object media has a configurable `max_object_bytes` cap: 1 GiB by default, validated
from 1 byte through 5 TiB via CLI/environment/JSON configuration. Known Content-Length
overflow is rejected before reading; producer and storage also bound unknown-length streams.

JSON multipart removes MIME framing in the adapter before shared bounded staging
and atomic publication. Metadata and optional final checksum JSON each have a 64 KiB
cap; part header blocks and delimiter suffixes have 8 KiB caps, and preamble/epilogue
each have a 4 KiB cap. Publication waits for complete framing, checksums and transport
EOF. Media/multipart invocation tokens require generation-match and are not replayed.
The [multipart contract](slices/json-object-multipart.md) records precise accounting,
advisory ordering and local choices under SPEC-013/SPEC-016.

Eight uploads are admitted immediately; excess uploads fail with 503 before any advisory
catalog check. Header validation and the known Content-Length cap check precede admission.
Each upload uses two channel slots with copied chunks of at most 64 KiB, incremental
MD5/CRC32C and asynchronous short file jobs. Downloads use one channel slot and a 64 KiB
reader. Neither a database transaction nor a blocking worker is held waiting for body input.
Catalog admission remains 32 calls with one executing SQLite job. The five-second default
`connection_timeout_ms` bounds header reads, then object-route response startup and inactivity
between chunks; each progressing chunk renews it. Upload advisory checks suspend this
body-inactivity deadline while waiting for bounded catalog admission/execution; it restarts
before body streaming. Independently, `object_request_timeout_ms` defaults to 900000 ms
(validated 1–86400000) from object upload/media request dispatch and never renews on chunks.
Expiry closes an upload before EOF without an HTTP error response or publication, or
truncates a media download after its headers. Large/slow transfers can raise
`--object-request-timeout-ms` (for example, 3600000 ms for one hour).
Upload advisory and post-EOF waits each use `catalog_wait_timeout_ms`, default 30000 ms
(validated 1–60000), capped by the remaining absolute request budget; expiry returns
503 `backendError`. Both settings use CLI > environment > JSON > default precedence.
Bucket/probe/unknown routes retain their total connection deadline.
Shutdown has its independent drain deadline. Compose additionally
sets 128 MiB memory, one CPU, 64 PIDs and 1024 file descriptors. These are inspected settings
and functional bounds, not measured RSS/cgroup/CPU/latency or a verified 1 GiB transfer under
the container limit. Resumable expiry uses one next-deadline scheduler, with no
timer when idle; failed cleanup has a bounded retry delay.

Startup validates config and registers signals, then opens/recovers the catalog before bind/readiness. Lock, schema, corruption or path failures stop startup with a diagnostic and no ready event. Shutdown marks not-ready, drops the listener and drains; deadline expiry aborts connections and returns an error. Every server exit path then closes the catalog before returning, including after connection cancellation. Catalog close waits for running blocking work, so the network drain deadline does not bound filesystem/SQLite shutdown. The process reports stopped only after successful catalog release. Initial seed import completes before bind/readiness; resumable expiry uses the injected clock and one explicit deadline scheduler before readiness; other lifecycle scheduling remains future work.

## Implemented bucket catalog and recovery

`Catalog::open` creates the data directory and acquires an exclusive OS lock on
`catalog.lock`; the lock file is never unlinked. A read-only preflight connection checks
existing version, integrity and schema before the operational read-write connection opens.
When no WAL exists, the locked main file is opened through an escaped immutable URI to
avoid creating WAL/SHM sidecars on rejection; an existing WAL uses ordinary read-only mode
so committed frames remain visible. Immutable preflight does not consider a hot rollback
`-journal`: gcs-mimic never creates one in WAL mode, but a foreign DELETE-mode catalog could.
The preflight closes before a single operational
connection owns `catalog.sqlite3`, with WAL, synchronous FULL, a 2 MiB cache, mmap disabled,
file-backed temporary storage and a 256-page auto-checkpoint. Schema version 7
stores buckets, live objects, unique opaque blob references, pending blob deletions,
seed initialization, bucket IAM policies and persistent object/policy revision
high-water marks. It retains the bucket `(project, name)`
index; object keys use `(bucket, name)` with indexed blob lookup. Valid v1 catalogs
contain no objects and first migrate to v3 transactionally. A separate atomic v3-to-v4
migration adds content encoding/disposition/language, cache control and custom metadata;
both metadata reads and bounded object lists select these columns. PATCH merges and PUT
replaces the supported fields under atomic generation/metageneration conditions without
changing the blob or generation. The v4-to-v5 migration records seed initialization;
v5-to-v6 atomically adds one bounded default IAM policy per existing bucket and a
persistent policy revision allocator. The v6-to-v7 migration adds indexed
resumable sessions, durable offsets, fixed deadlines and completion snapshots.
Unreleased v2 catalogs
are rejected before journal-mode changes because deleted generation history is unavailable.
New/empty version 0 catalogs are initialized transactionally; future versions,
nonempty unversioned catalogs, failed quick_check or incompatible versioned DDL
(columns, constraints, collations and indexes) prevent readiness. Version, emptiness
and existing-schema checks precede any persistent journal-mode change, so rejected
foreign catalogs retain their bytes and mode. A poisoned connection permanently
revokes readiness and subsequent catalog operations return unavailable.

Each operation has immediate bounded admission (32 calls), then awaits one execution
permit before scheduling one blocking job. The runtime's configured blocking pool
remains bounded. Inserts enforce globally unique local names in SQL; delete checks
metageneration and removes the record in one immediate transaction. GET renders
the exact selected record after its checks. Listing uses indexed keyset bounds and
at most 1001 records (200 + lookahead for full projection), without loading the catalog.
Labels and time values are persisted; names never become filesystem paths.

SQLite performs committed-WAL recovery while the directory lock is held. Tests cover
process exit without drop after a committed write, immediate close/reopen, schema/corruption
rejection and a second server process failing to acquire the directory. The connection
is dropped before explicitly unlocking the lock file. Closing a file alone can leave
the lock held by an unrelated child between fork and exec. Blocking jobs explicitly
release captured ownership before publishing completion. `Catalog::close()` stops
admission across all clones, waits for blocking execution, closes SQLite and releases
the directory even when old handles remain alive. Repeated close/drop of old handles
cannot unlock a reopened catalog. Without explicit close, the last handle or running
job still owns the resources until its synchronous drop. See the
[lock-release evidence](evidence/bucket-lock-release.md). Object publication/recovery is
implemented as described below, with actual process-crash tests at selected boundaries.
Power-loss durability and every individual instruction window remain unverified.

`selfLink` uses fixed `api_base_url` (`--api-base-url` / `GCS_MIMIC_API_BASE_URL` /
JSON, default `http://127.0.0.1:4443`). Listener and advertised origin are independent;
Host and forwarded headers do not influence it. No public URL or permissions follow
from this setting. See the [slice contract](slices/json-bucket-crud.md) for local
projectNumber, projection and unsupported-semantics deviations.

### Bucket IAM policies

`bucket_policies` stores one row per bucket, with bindings bounded to 64 KiB and
a catalog-wide monotonically allocated revision encoded as an opaque base64 etag.
Creation commits the bucket and its default policy together. Deletion removes
both in one transaction; the allocator survives deletion, so recreation cannot
reuse an etag. Policy PUT checks the supplied etag and updates the bindings and
revision in one immediate transaction; a mismatch returns 412 without mutation.
Unconditional requests may omit the etag. Successful updates leave bucket
metageneration unchanged.

Strict bucket operations use the union of project grants and matching bucket
bindings from the pinned predefined-role snapshot. Policy grants are evaluated
inside the same transaction as metadata selection, listing, blob open, mutation
or upload publication. This prevents an earlier advisory check from authorizing
an operation after revocation. A reader whose immutable blob is already open
may finish. Bucket insert/list remain project scoped. Get/set IAM require their
own permissions, including when those permissions come from a bucket binding.

Defaults use the documented non-UBLA convenience bindings: project owners and
editors receive legacy bucket owner, and project viewers receive legacy bucket
reader. The local identity registry models no basic project roles, so convenience
sets have no local members. Public allUsers and registered-only allAuthenticatedUsers principals are supported.
Conditions, ACLs and hierarchy above the local project remain excluded. The [bucket IAM contract](slices/bucket-iam.md)
defines fields, errors, limits, acceptance functions and verification.

## Planned storage architecture

| Component | Proposed responsibility |
| --- | --- |
| CLI/configuration | Listener, persistence, advertised URLs, auth profile, resource caps, fixture import |
| JSON adapter | JSON/media/upload/batch paths, request parsing, response envelopes |
| XML adapter | Path-style/virtual-host routes, headers, XML errors, signed requests, multipart sessions |
| gRPC adapter | Version-pinned protobuf, stream framing, metadata and status details |
| Shared service layer | Bucket/object generations, atomic preconditions, authorization, visibility, lifecycle |
| Metadata catalog | Indexed resource state, upload sessions, IAM/ACLs, operations and scheduling state |
| Blob store | Streaming temporary and immutable blob files with bounded buffers |
| Optional adapters | Local Pub/Sub, inventory exports, caches, analytics/ML fixtures and MCP |

Start with one application crate and clear modules. Split crates only where actual boundaries make it useful. Proposed libraries are Tokio, Axum/Hyper, Serde, Rustls and SQLite; add tonic/prost for gRPC. Verify and adopt the latest stable Rust and latest non-yanked stable releases of chosen dependencies at bootstrap. Track updates daily, including intentional major-version migrations, and record exact toolchain/lockfile/image inputs for reproducibility. Follow the [dependency policy](dependency-policy.md). These library choices are design proposals, not benchmark conclusions.

Store object data on disk by default. Keep a bounded in-memory backend only for small tests, with an explicit byte limit. Catalog entries should be indexed by bucket/name/generation, with keyset pagination instead of loading and sorting every object. Use bounded caches and queues so catalog growth does not imply unbounded resident memory.

Tokio's file operations may use blocking workers; merely using async functions does not cap thread growth. Bound CPU and blocking work explicitly. SQLite WAL allows readers alongside a writer, but still has one writer at a time. Keep writes short and avoid holding a transaction during a transfer. Use one process per data directory and an exclusive process lock. [Tokio](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html), [SQLite WAL](https://www.sqlite.org/wal.html).

### Storage selection and foreign data

The recommended durable model is SQLite metadata plus opaque immutable files.
Indexed paging and atomic metadata conditions fit a transactional catalog;
bounded body streams avoid database transactions held for an entire transfer.
SQLite BLOBs, embedded transactional key/value stores, filesystem metadata and
memory each have tradeoffs documented in `docs/en/storage-backends.md`.
No equivalent-workload benchmark or alternative backend has been implemented.

Keep foreign-format readers outside catalog internals and protocol handlers.
Seed trees, persistent xattr/sidecar roots and live memory exports are different
inputs. They should yield typed records and bounded streams into the shared
import path, preserving source names, identities and metadata. Do not require
the runtime to adopt a foreign directory layout.

Import into a separate destination, report unsupported state before cutover,
and retain the original source. A coherent backup must include the catalog
snapshot and its referenced blobs, protected from mutation/reclamation while
copying. SQLite's backup API alone does not back up external bodies.

## Persistence and concurrency

Use generated, opaque blob identifiers. Object names such as Unicode strings, repeated slashes, encoded `%2F`, dot segments and trailing delimiters are logical keys, not native filesystem paths.

The implemented ordinary-object write stages under a random 128-bit opaque ID on the
data filesystem. It streams and hashes the entire body within the configured cap,
validates declared hashes and syncs the file. It creates/syncs the blob shard's parent,
renames the staged file into that shard and syncs the shard. Only then does a short
immediate SQL transaction check bucket existence and all conditions, advance the
persistent generation high-water mark, replace the live reference and journal any
old blob for deletion. File publication and SQL commit are separate operations.
An ambiguous commit error is followed by a reference check before cleanup; committed
live data must not be removed as though the write had failed.

Delete atomically removes the live record and journals its blob. After successful commit,
overwrite/delete return success even if reclamation fails; the diagnostic is logged and the
journal retains deferred work for startup. Reclamation checks
for live references, unlinks/syncs the directory and removes the journal entry. A failed
startup retry blocks readiness until its cause is fixed and names the blob ID.
An already opened file descriptor continues to serve its selected immutable generation
after overwrite/delete on the tested Unix platforms. The Windows distribution
profile uses file flushes and same-volume write-through publication, without a
Unix directory-fsync equivalent or a power-loss namespace guarantee; see the
[native platform contract](binary-distribution.md#persistence-and-shutdown).
Windows unlink behavior remains outside the verified native resumable scope;
the resumable evidence records its tested host and recovery boundaries.

Single-request recovery holds the exclusive directory lock before bind/readiness. It removes abandoned staging,
replays deletion entries and validates live references in pages of 128. Directory
iterators plus indexed single-ID checks remove orphan blobs without collecting the
object catalog. Missing/truncated/non-file referenced data prevents readiness. Actual
SIGKILL tests cover incomplete staging, a renamed blob blocked before catalog commit,
and restart after successful commit. Fixtures cover pending deletion, orphan cleanup,
multi-page recovery and missing/truncated live data. Individual file/directory fsync,
deletion-journal insertion, unlink and journal-removal windows have not each been
barrier-killed; host power loss is untested. See the
[integration evidence](evidence/object-media.md). This is a local recovery
protocol, not an atomic SQLite/filesystem transaction or a production durability claim.
Persistent resumable sessions now use reference-aware staging recovery before
orphan scans, as specified in the session contract below.

The [regression evidence](evidence/object-media.md)
covers explicit body-completion markers, commit waiting beyond inactivity, both storage
race outcomes and advisory bucket/condition checks before staging. The authoritative checks
remain inside the publication transaction. After header and known-size validation, upload
admission is immediate (excess requests return 503), followed by one advisory bucket/condition
check (404/412), body validation (413/400), and authoritative publication checks. Concurrent
state changes after the advisory check can change the final 404/412 outcome. Channel closure
without confirmed EOF aborts
the upload. The advisory check suspends body inactivity, which restarts before streaming.
After EOF, body inactivity is suspended while storage completes; complete bodies
may still commit after disconnect or shutdown. SQLite lock waiting keeps its five-second
busy timeout. The post-EOF response wait is bounded by the catalog timeout and remaining
request budget, including queued writes, fsync, rename, directory sync and commit/reclaim.
After a post-EOF timeout, the mutation outcome is unknown: the commit may still complete.
Clients should re-read state or retry with generation/metageneration preconditions; success
is returned only after the storage result confirms the commit. Background filesystem work
and shutdown catalog close still have no execution bound; timing out the response does not
cancel a running filesystem/SQLite operation or release its ownership early. Early responses use
a write half-close followed by at most 256 KiB of discarded input for at most 100 ms.
Recovery ignores `.DS_Store` and `._*` OS metadata; other invalid entries fail startup with
their path in the diagnostic. These checks do not establish power-loss durability or
cross-platform socket behavior.

Make generation/metageneration compare-and-swap part of the committing mutation. Two concurrent `ifGenerationMatch=0` creates must not both succeed. Ordinary uploads must not expose partial objects. Rapid appendable objects have different visibility and single-writer handoff rules, so model them separately. [Preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions), [Rapid Bucket](https://docs.cloud.google.com/storage/docs/rapid/rapid-bucket).

Inject a deterministic clock from the beginning. Lifecycle evaluation, retention expiry, soft deletion, upload expiry, signed URL validity, UBLA locking and optional propagation delays should be testable without sleeping. Use explicit maintenance ticks or scheduled deadlines, not busy polling.

## Resumable session engine contract

This section describes the implemented [JSON resumable contract](slices/resumable-upload.md).
Its acceptance cases and pinned official sources define the boundary;
[final evidence](evidence/resumable-upload.md) records executed checks and limits,
and feature status remains in the ledger. XML uploads, signed initiation, gRPC, browser CORS
and invocation-token response replay are outside this slice.

The JSON adapter must parse initiation metadata and ranges, render the session
headers and distinguish POST initiation from PUT/SDK POST writes by `upload_id`.
The shared session engine must own offsets, serialization, authorization context
and terminal transitions, using the existing bounded blob/catalog mechanisms.
Sessions bind one bucket incarnation and logical object name; later request
queries cannot retarget them or relax persisted preconditions. Their opaque,
unguessable capability URI authorizes writes, status and cancellation without
re-authenticating the delegate. Never log the URI or token.

One session may have at most one active writer; competing writes return local
retryable 503 without an unbounded waiter queue. Repeated/overlapping writes
must discard the acknowledged prefix with bounded buffers and append only the
unseen tail. Persisted bytes remain immutable. A chunk checkpoint requires
complete transport EOF, file sync and a short offset transaction; interruption
or a disappearing producer must not acknowledge a partial request. Status must
report the durable offset after truncating any uncommitted tail.

Finalization must validate complete length and whole-object hashes before
publication. Its transaction must re-evaluate the initiating identity's current
bucket/project grants, require overwrite-delete permission for a live destination,
check bucket incarnation and all four generation/metageneration conditions,
allocate one generation, set metageneration to 1, publish metadata, record the
completed-session resource and journal a replaced blob. Creation/update times
come from completion-time `Clock::now()`. GET/list must expose no partial new
object; failures preserve the old object. Completed status/repeated final
requests must return the original bounded resource snapshot until the original
deadline, without allocating another generation even after a later overwrite.
This session state is separate from invocation-token response replay.

### Catalog, admission and staging

The contract requires a transactional migration from the validated catalog to
an indexed persistent session table. Store the random capability ID, opaque
staging/blob ID, bucket incarnation/owner, logical name, initiating authorization
context, bounded metadata/checksums/conditions, optional total, committed offset,
creation/deadline timestamps, state and bounded completion resource. Index
deadlines and blob references; exclude session fields from ordinary object lists.
Validate schema and session invariants before readiness, rejecting future or
corrupt schemas read-only. No body transfer may hold a SQL transaction.

The admission cap is 128 retained sessions, including active, completed
and invalid terminal records until their deadline. Count/insert must be atomic;
a full table returns immediate 503 `backendError` without files or object changes.
Reuse eight body-transfer slots, releasing them between session requests,
32 catalog admission slots and one executing SQLite job. At most two copied
64 KiB channel chunks per admitted body plus bounded hash/parser state are
allowed; neither chunks nor whole objects may be collected in a byte vector.
Apply `max_object_bytes` to declared total, offsets and streamed bytes using
checked arithmetic, including unknown-length requests. Retained staging can use
session-count times the object cap plus at most eight in-progress tails and live
blobs; this is a disk bound, not a free-space reservation. Disk-full/write/sync
failures must remain explicit.

### Checkpoints and crash recovery

SQLite commit and filesystem rename remain separate operations. Under the
exclusive data-directory lock, the required protocol is:

1. Create and durably record an opaque staging file on the data volume before
   inserting the session. A crash leaving an unreferenced file creates an orphan.
2. Append beyond the committed offset, sync after complete request EOF, then
   commit the offset/total. Before that commit, a crash may leave a tail to
   truncate; afterward, all acknowledged bytes must survive restart.
3. Rebuild hashes by bounded disk reads when resumed state requires it. Sync
   and publish the file under its recorded immutable blob ID using the existing
   same-filesystem rename/directory-sync protocol, then commit Object state and
   the COMPLETE resource snapshot together with conditions and deletion journal.
4. Before Object commit, an ACTIVE session with missing staging but its recorded
   immutable blob present must recover that blob to staging. The durable offset,
   metadata and total remain authoritative. Resolve an ambiguous SQL commit by
   session/object reference lookup before moving or deleting bytes.
5. After commit, preserve live references and the completion snapshot. Never
   reclaim committed data as abandoned staging; replaced blobs use the existing
   deletion journal, and failed reclamation cannot undo publication.

Startup must page session/blob references, retain active unexpired staging,
truncate excess tails, restore precommit renames and reclaim unreferenced or
expired files. Missing/truncated acknowledged prefixes must block readiness
with a diagnostic instead of resetting offsets. Recovery must avoid collecting
the catalog or reading whole bodies. Selected restart/process-crash barriers
are acceptance requirements; individual fsync windows, host power loss and
all-platform durability remain separate unverified boundaries.

### Cancellation and expiry scheduling

DELETE of an incomplete session must serialize with append/finalize so exactly
one terminal transition wins. JSON cancellation returns 499 and changes no live
object; deleting a completed session must not delete its object. Invalid terminal
sessions return local 410 before their original deadline, then 404 at or after
it; unknown sessions return 404.

The persisted deadline is initiation time plus 604800 seconds, never renewed by
activity. Requests must reject a session at `Clock::now() >= deadline` even if
physical cleanup is pending. An explicit maintenance entry point and one
next-deadline scheduler must expire indexed bounded batches, reclaim staging
and terminal records, and retry journaled unlink failures visibly. Wake when an
earlier deadline is inserted or shutdown begins; sleep until due work, with no
periodic polling and no timer when no work exists. Startup must run due cleanup
before readiness and rearm the next deadline. Tests advance the injected clock
and invoke maintenance without waiting a week.

## Access and URL contract

| Mechanism | Expected local behavior |
| --- | --- |
| Public URL helper | Construct a correctly encoded address; never change permissions |
| Anonymous object access | Evaluate effective `allUsers` grants and public access prevention |
| Authenticated access | Resolve local principals, role permissions, ACLs and applicable conditions |
| `allAuthenticatedUsers` | Require an authenticated principal; it is not anonymous public access |
| Fine-grained ACLs | Evaluate bucket/object/default ACL behavior and predefined ACL options |
| Uniform bucket-level access | Disable ACL paths and enforce transition/locking rules |
| Public access prevention | Override public grants while preserving otherwise valid signed access |
| Signed URLs | Validate V4 RSA/HMAC, expiration, method, raw canonical resource, query and signed headers; add legacy V2 later |
| POST policies | Validate policy conditions, length range, form fields and signature separately from signed URLs |
| Resumable session URI | Treat as a bearer capability with its own state and expiry; do not log it in full |

Public URLs normally address an XML-compatible path such as `http://localhost:4443/bucket/key`. JSON `mediaLink` and `selfLink` are resource links, not permission grants. A signing helper is a local tool or SDK example; Cloud Storage has no general storage-server `generateSignedUrl` endpoint. Remote IAM Credentials `signBlob` belongs to a separate service. [Public data](https://docs.cloud.google.com/storage/docs/access-public-data), [Signed URLs](https://docs.cloud.google.com/storage/docs/access-control/signed-urls).

V4 includes the host in the signed request. Sign the actual browser-facing emulator hostname from the start. Replacing the hostname after signing is incompatible with strict validation. Separate binding (`0.0.0.0:4443`), peer connectivity (`http://gcs:4443` inside Compose), API advertised URLs, and the browser/public endpoint (`http://localhost:4443`). [Canonical requests](https://docs.cloud.google.com/storage/docs/authentication/canonical-requests).

The resumable contract requires absolute `Location` values from configured
`api_base_url` (`--api-base-url` / `GCS_MIMIC_API_BASE_URL` / JSON), preserving a
normalized proxy prefix. The proxy must strip that prefix before forwarding.
Never derive it from bind, Host, Forwarded or X-Forwarded-*; public-object URL
configuration does not choose this capability endpoint or grant permissions.
Use `http://127.0.0.1:PORT` for a host, `http://gcs:4443` for a Compose peer,
and a host-reachable address such as `http://localhost:PORT` for a host browser.
An address check does not establish CORS support. One fixed base cannot
automatically cover both address spaces: use separate configurations or a common
reachable DNS/proxy endpoint. The validated-request-origin/allowed-authority
proposal in the [onboarding contract](getting-started-spec.md) remains deferred;
this slice freezes the existing fixed override. The
[guide](getting-started.md#resume-and-finalize-an-upload-from-the-host) supplies
the host/peer/address acceptance commands.

Use an explicit `permissive` profile for easy smoke tests and `strict` for permission/signature conformance. Strict mode can use local credentials and a local principal map; it must not silently contact Google to mint tokens. Passing permissive tests does not establish strict compatibility.

Implemented so far: `auth_mode` selects `permissive` (default) or `strict`; strict mode loads a bounded JSON identity registry once at startup, resolves `Authorization: Bearer` tokens, rejects invalid or unsupported credentials with JSON 401 `authError` or XML 400 `InvalidAuthentication`, and authorizes JSON routes and path-style XML reads through effective project and scoped bucket bindings from a pinned predefined-role snapshot. Authentication precedes route syntax validation; advisory authorization precedes body reads, conditions and existence checks, and the catalog transaction repeats authorization against the current policy. Permissive mode applies the same policy validation, persistence and etag rules without permission checks. `allUsers` includes anonymous callers; `allAuthenticatedUsers` requires a registered local identity. Anonymous denials remain JSON 401 `required` or XML 401 `AuthenticationRequired`; registered permission denials are 403. `gcs-mimic url public` is a pure URL builder for the served XML endpoint and rejects reserved namespace collisions. See the [strict authentication contract](slices/strict-auth.md) and [bucket IAM contract](slices/bucket-iam.md).

JSON CORS follows its documented defaults; XML CORS follows bucket configuration. A CORS response never authorizes an object read. Website index/not-found behavior is endpoint-dependent and must not be added to every object-download route. Authenticated browser download UX, DNS, TLS certificates, load balancers and CDN are distinct integration boundaries. [CORS](https://docs.cloud.google.com/storage/docs/cross-origin), [Request endpoints](https://docs.cloud.google.com/storage/docs/request-endpoints), [Website metadata](https://docs.cloud.google.com/storage/docs/xml-api/put-bucket-metadata).

## Developer experience

Aim for one command or one Compose service, a default development port of
4443, HTTP/optional HTTPS, environment variables plus CLI flags, fixture import,
persisted volumes, health/readiness, structured logs, graceful shutdown and
clear error messages.

Provide migration examples for fake-gcs-server's data-directory and endpoint
patterns. Replace only the environment prefix with `GCS_MIMIC_`, preserving
each setting's meaning; never accept `FAKE_GCS_*` aliases or fallback values.
Reject contradictory native/renamed settings at the same precedence, retaining
CLI precedence. Distinguish seed `DATA`, durable `DATA_DIR` and Compose
host-port publication. These migration settings are planned, not currently
accepted equivalents. The source README's permissive signed URL behavior
belongs to the legacy profile, separate from strict validation.

Docker targets are `linux/amd64` and `linux/arm64`, supporting both standard CI hosts and Apple Silicon development. Use a multi-stage build, a non-root runtime, explicit writable data/temp mounts, and bounded optional tmpfs. A read-only root must not break SQLite WAL or temporary uploads. Keep seeding input distinct from durable storage so reboots do not silently reimport or overwrite data. Provide native binary installation after the core workflow works.

The implementation prompts and conversation with the project owner are Japanese. Code, comments, commits, issues, PRs, canonical docs and CLI are English. Japanese planning notes are separate companion material outside the project template; optional translated views retain identical feature IDs and status.

## Verification strategy

Build a contract corpus at the wire level, not only unit tests of the internal service. Test correct operations and errors, omitted vs null vs empty fields, string-encoded 64-bit JSON values, path encoding, checksums, partial responses, cross-feature restrictions and retry behavior. Keep tests aligned with referenced contracts rather than with whatever the implementation currently does.

Prioritize Go and Ruby/Rails Active Storage. Verify Ruby SDK usage and browser direct uploads, public and private delivery, signed upload/download, and object deletion; include Node.js, Python and official Rust next. Add Java, C++, .NET, PHP, gcloud and FUSE according to the scope and transport. Record exact versions and endpoint settings. Do not assume one `STORAGE_EMULATOR_HOST` value configures all SDKs or their URL signers.

Use differential tests against an explicitly authorized disposable GCS environment for unresolved behavior. Offline CI remains the default and blocks unintended outbound network access. fake-gcs-server and storage-testbench help find differences, but neither replaces Google documentation or real-service evidence, especially for IAM/ACLs.

## Performance plan

Rust is a means to control resource use, not proof of lower CPU or memory consumption. Start with bounded-memory invariants, then measure equivalent scenarios against a pinned fake-gcs-server image.

Provisional engineering targets, not results: idle process RSS at or below 64 MiB for the disk-backed core; peak process RSS at or below 256 MiB for 16 concurrent 1 GiB streamed transfers in a defined environment; an idle CPU average below 1% of one core over 60 seconds. Calibrate these after the first working slice and report full profiles separately. The workload generator's memory and CPU must not be included in server measurements.

Measure 0 B, 1 KiB, 1 MiB, 64 MiB and 1 GiB objects; concurrency 1/8/16; 1k/100k metadata entries; upload/download/list/range/resumable/rewrite/compose and signature verification. Capture process RSS, cgroup memory including page cache, CPU-seconds/GiB, requests/sec, throughput, p50/p95/p99, errors, temporary disk high-water mark and recovery time. Maximum-size contract tests need not allocate the maximum production object size in routine CI.

Keep backend, TLS, authorization, checksums, durability, CPU/memory limits, architecture and cache state equal in comparisons. Never advertise a latency number without its workload and environment. Optimize demonstrated bottlenecks, not language stereotypes.
