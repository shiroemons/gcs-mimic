# Google Cloud Storage emulator design

## Product goal

Build a resource-efficient Cloud Storage emulator for local development and CI. Ship a Rust binary and Docker image with familiar fixture loading and endpoint configuration. Expand compatibility incrementally while keeping a precise, evidence-backed progress ledger.

P0 implements a bounded HTTP/1 health/readiness shell, CLI/configuration, Clock injection, Docker/Compose and Rust ledger/freshness tooling. The first P1 slice adds persistent JSON bucket create/get/list/delete. Objects and permissions remain unimplemented; no performance result is claimed. Current status is authoritative only in `compatibility/features.json`. The project name, CLI binary and repository slug are **gcs-mimic**. Selected description: **A lightweight Google Cloud Storage emulator in Rust.**

The target is publicly observable protocol and behavior compatibility. A single local process cannot reproduce Google's physical regional redundancy, production durability/SLA, billing system, identity issuance, managed ML inference, or network placement. Represent those boundaries explicitly and provide useful local simulations or optional integrations. Do not count a simulated billing charge as a reproduced billing service.

## Compatibility surfaces

Cover core JSON REST, XML operations, Storage v2 gRPC, Storage Control v2, FeatureConfigs, Storage Intelligence, Storage Insights, Storage Batch Operations, and optional Cloud Storage MCP tools. Keep separate rows for different transports even when they share domain logic. Operation counts are inventory counts, not a count of independent product features.

Method names alone are insufficient. Compatibility includes required and optional parameters, field masks, defaults, errors, response headers, ACL/IAM permissions, pagination, state transitions, consistency, timing and interactions. The inventory is a dated public-contract baseline. Expand acceptance cases when a feature is selected, and refresh sources before implementing fast-moving or preview features.

Reference sources: [JSON API](https://docs.cloud.google.com/storage/docs/json_api/v1), [XML API](https://docs.cloud.google.com/storage/docs/xml-api/reference-methods), [Storage v2 RPC](https://docs.cloud.google.com/storage/docs/reference/rpc/google.storage.v2), [Storage Control v2](https://docs.cloud.google.com/storage/docs/reference/rpc/google.storage.control.v2).

## Implemented foundation and extension boundaries

One application crate contains `config`, `clock`, `server`, `bucket`, `json` and `storage`; `main` owns runtime creation, signals, catalog opening and listener setup. JSON parsing/rendering remains separate from the transport-independent record/catalog. The server routes four bucket operations and internal probes; unknown routes return 404 without reading bodies. No XML/gRPC success stubs are registered. `xtask` owns ledger validation/rendering and explicit online dependency checks. Existing `serde_json` supplies bounded configuration and bucket metadata parsing.

| Boundary | Contract for subsequent slices |
| --- | --- |
| JSON adapter | Own JSON API parsing/errors/headers and media/upload routes; preserve raw URI until protocol decoding. |
| XML adapter | Own path/virtual-host canonicalization, XML errors, CORS and signed request parsing. |
| gRPC adapter | Own pinned protobuf, stream framing, status and metadata; no gRPC listener in P0. |
| Shared services | Own authorization, generations, atomic preconditions and state transitions; transport types do not enter persistence. |
| Metadata catalog | Own indexed transactions, short atomic condition checks and reference publication; no transaction across body transfer. |
| Blob storage | Own opaque blob IDs, bounded staging streams and durable immutable publication/deletion. Logical object names never become filesystem paths. |
| Clock | Inject business wall time through `Clock::now`; production uses `SystemClock`, tests supply deterministic time. Socket/drain deadlines use Tokio monotonic time independently. |

The bucket catalog is concrete; no generic persistence trait is frozen for future object work. Recovery gates readiness. Authorization remains unimplemented and bucket API rows remain PARTIAL. Existing SPEC-001/002/003/004/006/007 gaps were reviewed; none adds bucket CRUD semantics. They remain open for their affected features.

The server caps connections (default 32), headers (64 and 16 KiB), connection lifetime (5 seconds), shutdown drain (5 seconds), async workers (2) and blocking workers (2). Connection admission stops when full; the requested kernel backlog is 128. Every response closes its connection. Only insert reads a body, capped at 64 KiB. Probe and other bucket methods reject bodies from headers. Compose additionally sets 128 MiB memory, one CPU, 64 PIDs and 1024 file descriptors. These are settings, not benchmarks. Catalog admission and cache limits are described below; no maintenance polling exists.

Startup validates config and registers signals, then opens/recovers the catalog before bind/readiness. Lock, schema, corruption or path failures stop startup with a diagnostic and no ready event. Shutdown marks not-ready, drops the listener and drains; deadline expiry aborts connections and returns an error. An already running blocking catalog call can finish after connection cancellation, retaining ownership until it finishes. Fixture seeding, clock-backed expiration and an explicit maintenance scheduler remain future work.

## Implemented bucket catalog and recovery

`Catalog::open` creates the data directory and acquires an exclusive OS lock on
`catalog.lock`; the lock file is never unlinked. A single SQLite connection owns
`catalog.sqlite3`, with WAL, synchronous FULL, a 2 MiB cache, mmap disabled,
file-backed temporary storage and a 256-page auto-checkpoint. Schema version 1
stores bucket metadata and a `(project, name)` index. New/empty version 0 catalogs
are initialized transactionally; future versions, nonempty unversioned catalogs,
failed quick_check or incompatible columns prevent readiness.

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
is dropped before the lock; blocking jobs explicitly release captured ownership before
publishing completion. No object blobs, filesystem rename protocol, uncommitted blob
reconciliation or power-loss durability measurement is implemented. The object publication
contract below remains planned and must be verified before exposing object writes.

`selfLink` uses fixed `api_base_url` (`--api-base-url` / `GCS_MIMIC_API_BASE_URL` /
JSON, default `http://127.0.0.1:4443`). Listener and advertised origin are independent;
Host and forwarded headers do not influence it. No public URL or permissions follow
from this setting. See the [slice contract](slices/json-bucket-crud.md) for local
projectNumber, projection and unsupported-semantics deviations.

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

## Persistence and concurrency

Use generated, opaque blob identifiers. Object names such as Unicode strings, repeated slashes, encoded `%2F`, dot segments and trailing delimiters are logical keys, not native filesystem paths.

A suggested ordinary-object write path is: allocate upload state, stream to a temporary blob while checking length/checksums, make the blob durable according to the selected profile, publish its immutable file, and commit the catalog reference with atomic precondition checks. File publication and SQL commit are separate operations. A recovery journal or an explicitly proven reconciliation protocol must cover every crash boundary, orphan blob and dangling reference. Garbage collection must not remove a generation still being read or needed by a resumable operation.

Before exposing writes, validate this publication protocol: stage and fsync a temporary blob; rename to an opaque immutable ID on the same filesystem and fsync its directory; atomically check preconditions and commit the catalog reference. A crash before publication leaves staging data; before catalog commit it leaves an orphan; after commit the referenced blob must be durable. Recovery takes the exclusive directory lock before serving and reconciles staging/orphans against indexed references in bounded pages. Missing referenced blobs are corruption that prevents readiness. Failed preconditions expose no generation, and GC must respect readers/sessions. This is a planned crash-boundary contract, not verified durability or an atomic SQLite/filesystem transaction.

Make generation/metageneration compare-and-swap part of the committing mutation. Two concurrent `ifGenerationMatch=0` creates must not both succeed. Ordinary uploads must not expose partial objects. Rapid appendable objects have different visibility and single-writer handoff rules, so model them separately. [Preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions), [Rapid Bucket](https://docs.cloud.google.com/storage/docs/rapid/rapid-bucket).

Inject a deterministic clock from the beginning. Lifecycle evaluation, retention expiry, soft deletion, upload expiry, signed URL validity, UBLA locking and optional propagation delays should be testable without sleeping. Use explicit maintenance ticks or scheduled deadlines, not busy polling.

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

V4 includes the host in the signed request. Sign the actual browser-facing emulator hostname from the start. Replacing the hostname after signing is incompatible with strict validation. Separate binding (`0.0.0.0:4443`), peer connectivity (`http://gcs:4443` inside Compose), API advertised URLs, and the browser/public endpoint (`http://localhost:4443`). Resumable `Location` responses must be reachable by the initiating client, which may use a different network than the browser. Use the listener scheme and an explicitly allowed request authority for the default development mapping, with a fixed advertised-base override for proxies. Do not trust arbitrary forwarded headers. See the [onboarding contract](getting-started-spec.md). [Canonical requests](https://docs.cloud.google.com/storage/docs/authentication/canonical-requests).

Use an explicit `permissive` profile for easy smoke tests and `strict` for permission/signature conformance. Strict mode can use local credentials and a local principal map; it must not silently contact Google to mint tokens. Passing permissive tests does not establish strict compatibility.

JSON CORS follows its documented defaults; XML CORS follows bucket configuration. A CORS response never authorizes an object read. Website index/not-found behavior is endpoint-dependent and must not be added to every object-download route. Authenticated browser download UX, DNS, TLS certificates, load balancers and CDN are distinct integration boundaries. [CORS](https://docs.cloud.google.com/storage/docs/cross-origin), [Request endpoints](https://docs.cloud.google.com/storage/docs/request-endpoints), [Website metadata](https://docs.cloud.google.com/storage/docs/xml-api/put-bucket-metadata).

## Developer experience

Aim for one command or one Compose service, a default development port of 4443, HTTP/optional HTTPS, environment variables plus CLI flags, fixture import, persisted volumes, health/readiness, structured logs, graceful shutdown and clear error messages. Provide migration examples for fake-gcs-server's data-directory and endpoint patterns. Do not claim drop-in flag compatibility until each alias is tested. fake-gcs-server's README explicitly documents permissive signed URL behavior; this is an ergonomics reference, not the strict security contract. [fake-gcs-server](https://github.com/fsouza/fake-gcs-server/blob/main/README.md).

Docker targets are `linux/amd64` and `linux/arm64`, supporting both standard CI hosts and Apple Silicon development. Use a multi-stage build, a non-root runtime, explicit writable data/temp mounts, and bounded optional tmpfs. A read-only root must not break SQLite WAL or temporary uploads. Keep seeding input distinct from durable storage so reboots do not silently reimport or overwrite data. Provide native binary installation after the core workflow works.

The implementation prompts and conversation with the project owner are Japanese. Code, comments, commits, issues, PRs, canonical docs and CLI are English. Japanese planning notes are separate companion material outside the project template; optional translated views retain identical feature IDs and status.

## Verification strategy

Build a contract corpus at the wire level, not only unit tests of the internal service. Test correct operations and errors, omitted vs null vs empty fields, string-encoded 64-bit JSON values, path encoding, checksums, partial responses, cross-feature restrictions and retry behavior. Keep tests aligned with referenced contracts rather than with whatever the implementation currently does.

Prioritize Go and Ruby/Rails Active Storage. Verify Ruby SDK usage and browser direct uploads, public and private delivery, signed upload/download, and object deletion; include Node.js, Python and official Rust next. Add Java, C++, .NET, PHP, gcloud and FUSE according to the scope and transport. Record exact versions and endpoint settings. Do not assume one `STORAGE_EMULATOR_HOST` value configures all SDKs or their URL signers.

Use differential tests against an explicitly authorized disposable GCS environment for unresolved behavior. Offline CI remains the default and blocks unintended outbound network access. fake-gcs-server and storage-testbench help find differences, but neither replaces Google documentation or real-service evidence, especially for IAM/ACLs. [storage-testbench](https://github.com/googleapis/storage-testbench).

## Performance plan

Rust is a means to control resource use, not proof of lower CPU or memory consumption. Start with bounded-memory invariants, then measure equivalent scenarios against a pinned fake-gcs-server image.

Provisional engineering targets, not results: idle process RSS at or below 64 MiB for the disk-backed core; peak process RSS at or below 256 MiB for 16 concurrent 1 GiB streamed transfers in a defined environment; an idle CPU average below 1% of one core over 60 seconds. Calibrate these after the first working slice and report full profiles separately. The workload generator's memory and CPU must not be included in server measurements.

Measure 0 B, 1 KiB, 1 MiB, 64 MiB and 1 GiB objects; concurrency 1/8/16; 1k/100k metadata entries; upload/download/list/range/resumable/rewrite/compose and signature verification. Capture process RSS, cgroup memory including page cache, CPU-seconds/GiB, requests/sec, throughput, p50/p95/p99, errors, temporary disk high-water mark and recovery time. Maximum-size contract tests need not allocate the maximum production object size in routine CI.

Keep backend, TLS, authorization, checksums, durability, CPU/memory limits, architecture and cache state equal in comparisons. Never advertise a latency number without its workload and environment. Optimize demonstrated bottlenecks, not language stereotypes.
