# Incremental implementation roadmap

Each milestone contains multiple small feature slices. Complete one requested slice per implementation turn; do not interpret a milestone as permission to build every listed feature at once. `phase` in the ledger records the recommended sequence, while `priority` ranks work within that sequence. Reconcile real dependencies before selecting a slice.

| Phase | Deliverable | Exit evidence |
| --- | --- | --- |
| P0 Foundation | Latest stable Rust/dependencies, reproducible pins, daily update checks, CLI/configuration, health/readiness, Docker amd64/arm64, deterministic ledger generator, source lock and clock interface | Container starts/stops correctly; dependency checks run; the English table and any requested companion views are generated from one ledger; no GCS API is falsely marked implemented |
| P1 Core data path | Bucket/object CRUD, streaming media and JSON multipart upload, metadata, list/pagination, basic ranges, checksums, generations and atomic preconditions, disk persistence | Go/Ruby core workflows; wrong checksum and failed preconditions rejected; restart and concurrent-create tests; object-size-independent buffering |
| P2 Access and URLs | Local strict identities, IAM/ACL/default ACL, UBLA/PAP, XML object paths, public URL helper, V4 signing, CORS and host/browser configuration | Public, private, expired, tampered and wrong-host cases; public URL generation does not grant access; Rails public and private delivery scenarios |
| P3 Transfer and HTTP breadth | Resumable sessions, retry/status/expiry, rewrite/copy/compose, HTTP batch, remaining XML operations and multipart, V2/HMAC interoperability, advanced conditional/range/transcoding | Interrupted transfer recovery; metadata and source inheritance; partial batch failures; bounded buffering; equivalent protocol outcomes |
| P4 Protection and lifecycle | Versioning, soft delete/restore/bulk restore, holds, bucket/object retention, lifecycle, Autoclass local transitions | Virtual-clock cases and cross-feature restrictions; local retention locking; no premature deletion; metadata-only coverage remains explicit |
| P5 Hierarchy and control | HNS folders/rename, managed folders/inherited access, storage layout, shared LRO behavior and relevant Control operations | Atomic subtree changes, list visibility, permission inheritance, cancellation limits and operation polling |
| P6 gRPC and Rapid | Storage v2 unary/streaming/bidirectional RPCs; field masks, wire errors, Rapid/appendable states and protocol restrictions | Pinned official SDK gRPC tests; single-writer handoff; read-during-append; rejected unsupported Rapid combinations |
| P7 Extended capabilities | Cache lifecycle, Insights, Intelligence, batch jobs, contexts/enrichments, MCP, notifications, encryption/infrastructure adapters and extended clients | Exact local wire contracts separated from simulations; reproducible jobs/events/fixtures; external integration boundaries and limitations published |

## First useful release

The first developer release should target P1 plus the essential P2 public/private and URL path. Publish the tested single-request upload subset until resumable P3 support exists; do not claim unrestricted SDK large-upload support early. Explicitly select Go JSON reads for P1, then verify default/XML reads in P2. Do not wait for analytics, caches or ML enrichment before making a useful emulator available. Conversely, do not market the first release as fully GCS-compatible. Publish the tested API/profile/SDK subset.

Version increments, immutable release identity and licensing follow the
[versioning and licensing policy](release-policy.md).

## Suggested first slices

P0 ledger tooling (DESIGN-091/092/093/097), bounded configuration/HTTP probes and the P1 bucket and object-media slices now exist, with Clock injection and Docker/Compose smoke checks. Local arm64 is native; amd64 is emulated. Native amd64 and remote CI remain unverified. Python dependency tooling was replaced by Rust xtask. Seeding and TLS remain unimplemented; strict/permissive profiles now exist. Current progress is recorded only in the ledger.

Bucket create/get/list/delete and streaming object media upload/metadata/download/delete use
an indexed transactional catalog and opaque immutable blobs. Checksums, atomic generation
conditions/create-if-absent, nonempty bucket protection and selected SIGKILL recovery points
pass, with pinned Go JSON Attrs/NewReader/Delete coverage. Publication and SQL commit are
separate; every instruction-level fsync/deletion window, power loss and performance remain
unverified. See the [integration evidence](evidence/object-media.md) and
[executed guide](evidence/object-media-guide.txt); full method rows remain PARTIAL.

Regression tests additionally cover explicit upload EOF, commit waiting beyond body
inactivity, committed success with deferred reclamation, both storage race outcomes, early
rejection with eager bodies, advisory checks and OS metadata recovery. Object-free v1 catalogs
migrate through v3 to v4; unreleased v2 catalogs are rejected. See the
[verification summary](evidence/object-media.md)
for current logs and the remaining filesystem, platform and performance limits.

The **list and metadata** slice is complete for its scoped local subset; method rows
JSON-OBJECTS-008/010/015 remain PARTIAL. Prefix/delimiter/keyset paging and conditional
PATCH/PUT share v4 metadata with GET, including after restart. Pinned Go listing/Update
coverage exists. Remaining work includes ACL/IAM, historical/soft-deleted listing,
glob/context filters, partial response masks, token replay, gzip transcoding and
unverified legacy metadata input and list-shape edge cases. See the
[listing contract](slices/json-object-list.md) and [metadata contract](slices/json-object-metadata.md).

The **JSON multipart upload** subset now supports bounded metadata/media containers,
optional final checksums and the pinned Go `ChunkSize=0` Writer, including conditional
publication and rejected-overwrite preservation. MIME/error edge parity and token replay
remain open; see the [contract](slices/json-object-multipart.md) and
[integration evidence](evidence/multipart.md). Resumable sessions remain separate P3
work; Ruby/Rails is unverified.

Item 5 is split. The first part, **strict local authentication**, is done: explicit
permissive/strict profiles, a JSON local principal registry, project-level enforcement
of a pinned predefined `roles/storage.*` snapshot on the JSON routes (including the
overwrite `storage.objects.delete` rule) and the side-effect-free `gcs-mimic url public`
helper. DESIGN-001/003/008/046 are IMPLEMENTED; DESIGN-002/004 and the JSON method rows
remain PARTIAL, with SPEC-017 open. See the [contract](slices/strict-auth.md) and
[evidence](evidence/strict-auth.md). The next slice is **bucket IAM and public reads**:
bucket IAM policy get/set, `allUsers`/`allAuthenticatedUsers` grants and XML path-style
GET/HEAD for public object reads, so URLs built by the helper become servable when a
public grant exists.

1. **Bootstrap only:** initialize toolchain, server shell, config, health/readiness, ledger validator/table generator and a Docker smoke test. Decide persistence/recovery interfaces without implementing all GCS handlers.
2. **One bucket path:** create/get/list/delete a bucket, including name/project validation, duplicate and non-empty deletion behavior, metadata identity and persistence.
3. **One object path:** media upload/download/metadata/delete with streaming, checksum validation, generation and create-if-absent. Include failed and concurrent requests.
4. **List and metadata:** prefix/delimiter/page tokens, metadata patch vs replacement, conditional requests and official SDK integration.
5. **Public/private slice:** anonymous access, a minimal strict principal/role model, public grants, denied requests and public URL construction. The principal/role model, denied requests and URL construction are done; bucket IAM policies, public grants and XML public reads are next.
6. **Signed URL slice:** V4 download/upload using local signing keys, final external host, expiry/tampering and signer permission checks.

The full method row `objects.insert` remains `PARTIAL` with media and the scoped multipart subset. Track multipart and resumable acceptance cases separately. Similarly, `objects.list` is not complete merely because an unfiltered first page works.

## Release and source maintenance

Keep explicit compatibility profiles and a dated source lock. Refresh current Discovery/protobuf/release notes before starting each advanced group and before releases. Add new methods, parameters and preview features as new pending work without silently removing old rows. Retired upstream interfaces remain visible as legacy or not-applicable with an explanation.

Benchmark equivalent supported scenarios after P1 and after resource-sensitive changes. Build both CPU architectures, run the supported SDK matrix, regenerate English docs and requested companion views, and review every completed row's evidence before making a release claim. Publishing images/packages is a separate authorized action.
