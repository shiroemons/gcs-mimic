# gcs-mimic

<!-- markdownlint-configure-file {"MD013": false} -->

A Google Cloud Storage emulator in Rust for local development and CI.

The current implementation provides persistent JSON bucket create/get/list/delete and scoped metadata PATCH/PUT, bucket IAM policy get/set and streaming object media/multipart/resumable upload, metadata get/patch/update, prefix/delimiter listing with keyset paging, whole-object and single-range download, delete and initial seed directories, with a bounded SQLite catalog, opaque disk blobs, health/readiness checks and graceful shutdown. JSON resumable sessions support chunk uploads, status/resume, cancellation, fixed seven-day expiry, restart recovery and client-reachable Location from the configured API endpoint. Path-style XML GET/HEAD serves public and private objects. The default permissive profile enforces no authentication; use an isolated local development environment. An optional strict profile checks local bearer tokens against project and bucket grants of predefined storage roles, including anonymous `allUsers` and registered-only `allAuthenticatedUsers` grants.

## Implementation status

| Capability | Status | Current scope |
| --- | --- | --- |
| Standalone binary | ✅ Available | Build and run from source; no Rust installation is needed at runtime. |
| Prebuilt binary distribution | 🚧 Partial | CI is configured for macOS/Windows ARM64 and x86-64, plus Linux GNU/musl on both CPUs. All-platform execution and public release are pending. |
| Docker and Compose | 🚧 Partial | Non-root runtime; native amd64/arm64 Foundation CI passed. GHCR publication is configured but has not run. |
| CLI, environment and JSON configuration | ✅ Available | Command-line options override environment variables, configuration files and defaults. `gcs-mimic url public` builds encoded public object URLs without reading configuration. |
| Health, readiness and graceful shutdown | 🚧 Partial | Catalog locking/recovery and configured initial seeding complete before readiness; SIGINT/SIGTERM drain works. Seeded Compose startup is verified on ARM64 and emulated AMD64. |
| HTTP and TLS | 🚧 Partial | HTTP/1 is available; TLS is not implemented. |
| JSON bucket APIs | 🚧 Partial | Create/get/list/delete with create-time versioning/default holds, scoped keyset paging and conditions; PATCH merges/removes labels and changes the supported default storage class, versioning and default holds with atomic metageneration conditions. PUT replaces that same subset and resets omitted modeled fields to declared local defaults. Response `fields` selection covers supported metadata/list/IAM resources; bucket GET supports `If-Match` / `If-None-Match`. Persistent IAM policy get/set with etags and scoped enforcement. ACLs, testIamPermissions and advanced settings remain unavailable. |
| JSON object APIs | 🚧 Partial | Media/multipart/resumable upload, metadata, MD5/CRC32C, range reads, keyset listing, patch/update and atomic conditions. Response `fields` selection and metadata/media GET `If-Match` / `If-None-Match` work, including selected generations and ranges. Persistent version history, selected generation reads/deletes, version paging and event holds work. Resumable cancel/expiry/recovery work; XML/signed resumable, replay, If-Range, transcoding, glob, ACL, temporary holds, retention and soft delete remain unavailable. |
| XML and gRPC APIs | 🚧 Partial | Path-style XML object GET/HEAD: selected live generation, single ranges, generation/metageneration match headers and XML errors. Virtual-host routing, XML writes/listing and gRPC are unavailable; recognized unsupported XML inputs return 501. |
| Persistence | 🚧 Partial | Transactional catalog, opaque blobs, exclusive directory lock, restart/crash recovery and initial seed directory import. Individual fsync crash windows and power loss remain unverified. |
| Authentication, IAM and ACLs | 🚧 Partial | Explicit permissive (default) and strict profiles. Strict mode resolves registered local bearer tokens and combines project grants with unconditional bucket policy bindings from a pinned `roles/storage.*` snapshot on JSON routes and XML reads. `allUsers` includes anonymous callers; `allAuthenticatedUsers` requires a registered identity. Object IAM, testIamPermissions, conditions, custom/basic roles, ACLs and UBLA/PAP settings are unavailable. |
| Public and signed URLs | 🚧 Partial | `gcs-mimic url public` builds encoded path-style URLs served by XML GET/HEAD without changing permissions. Strict reads enforce public/private grants and revocation; signed URLs and virtual-host routing are unavailable. |
| Browser CORS | ❌ Unavailable | JSON and XML CORS behavior is not implemented. |
| Official SDK workflows | 🚧 Partial | Go 1.27.3, storage v1.70.0 and API v0.308.0 cover bucket CRUD/paging, BucketHandle.Update (PATCH) with SetLabel/DeleteLabel, generated API Buckets.Update (PUT) replacement/defaults, class/settings and metageneration conditions, IAM Policy/SetPolicy, multipart and normal resumable Writer (default 16 MiB or 256 KiB chunks; `ChunkSize=0` is no longer required), Attrs/NewReader/NewRangeReader/Delete, iterators and conditional Update, including strict grants. Generated response fields, iterator attribute selection and conditional object/bucket/policy GETs with combined query conditions are verified. Scoped default/XML and JSON reads remain supported. Media/multipart retry tokens require generation-match; resumable initiation accepts them without it. No token response replay; Ruby and other SDKs remain pending. |

See the [detailed compatibility matrix](docs/en/feature-matrix.md) for supported scope and verification status.

Official Go workflows also cover generated bucket field selection, object iterator
attribute selection and conditional object/bucket/policy reads, including combined
generation/metageneration query conditions. Use the HTTP `ETag`
response header for conditional requests: the legacy-emulator bucket JSON `etag`
field can differ from that record-derived validator. Write/list validators and
standalone date validators remain unsupported.

### Delivered workflows and next capabilities

The workflows below name their final evidence and remaining boundaries.
These scoped workflows do not
promote their broader API rows to `IMPLEMENTED`: unresolved fields, permissions
and protocol differences remain in the same feature ledger.

| Workflow available in the documented scope | Main remaining boundary | Evidence |
| --- | --- | --- |
| Response field selection and JSON read ETag validators, including policy GET, combined query conditions, selected generations, ranges and restart | SPEC-FIELDS-EDGES, provider ETag/error/header parity, list/write/date validators and If-Range | [Fields and ETag verification](docs/en/evidence/basic-json-fields-etag.md) |
| Persistent JSON bucket/object round trip with checksums and atomic generation conditions | Advanced fields, full IAM/ACL and complete wire parity | [Media](docs/en/evidence/object-media.md) |
| Prefix/delimiter/keyset listing and metadata PATCH/PUT | History, glob/context filters, full projection and legacy-input parity | [Listing and metadata](docs/en/evidence/object-list-metadata.md) |
| Bounded JSON multipart upload with Go single-request Writer | MIME edge parity, response replay and advanced fields | [Multipart](docs/en/evidence/multipart.md) |
| JSON resumable sessions, chunk/status/retry, finalization, cancellation, seven-day expiry, restart recovery and configured host/peer Location; normal Go Writer | XML/signed initiation, response replay, provider edge parity and unrestricted resource/durability claims | [Final resumable verification](docs/en/evidence/resumable-upload.md) |
| Bucket create/get/delete with explicit compatibility profile, retained generations and default event holds | Broad methods remain PARTIAL; advanced settings and provider wire questions remain pending | [Final bucket verification](docs/en/evidence/bucket-core-parity.md) |
| Bucket PATCH label merge/removal, default class, versioning/default holds and atomic metageneration conditions | Whole method remains PARTIAL; advanced settings, full projection and provider edge parity remain pending | [Bucket PATCH verification](docs/en/evidence/bucket-metadata-patch.md) |
| Bucket PUT replacement of labels/default class and versioning/default holds, atomic conditions and resource preservation | Whole method remains PARTIAL; local defaults and provider edge parity remain explicit | [Final PUT verification](docs/en/evidence/bucket-metadata-put.md): Rust/Go, four-target comparison, both container architectures and native/Docker guide with restart equality pass; API client update verified separately |
| Strict local tokens and project-level predefined storage roles | Signatures and full strict authentication | [Authentication](docs/en/evidence/strict-auth.md) |
| Bucket IAM policy get/set and enforcement with project-plus-bucket grants | Policy methods remain PARTIAL; conditions, custom/basic roles, ACLs, testIamPermissions and unresolved wire parity | [Final IAM verification](docs/en/evidence/bucket-iam.md) |
| Public/private path-style XML GET/HEAD; anonymous and registered-only bucket grants | Virtual-host routing, XML writes/listing, ACL/UBLA/PAP state machines and signatures; broader XML methods and authentication wire parity remain PARTIAL | [Final public-read verification](docs/en/evidence/public-object-reads.md) |
| Single closed/open/suffix stored-byte ranges with pinned generation | If-Range, transcoding and exact fallback/header parity | [Range](docs/en/evidence/object-range.md) |
| Native initial seed import before readiness without replay over changes | Fixed octet-stream MIME; manifest metadata and legacy seed defaults remain separate work | [Seed](docs/en/evidence/seed-platform.md) |

Seed platform verification is complete: Linux rejects invalid-byte filenames;
Compose preserves user mutations and deletions with read-only fixtures, and
restores fixtures only after an explicit fresh-volume reset. UX-002 and UX-009
are IMPLEMENTED for their declared scope. ARM64 execution is native; AMD64 uses
emulation.

Public-read verification is complete for the declared strict local scope:
DESIGN-012, DESIGN-013, DESIGN-047 and UX-005 are IMPLEMENTED. DESIGN-004 and
EXT-XML-012/017 remain PARTIAL for their explicit remaining cases; see the
[final public-read evidence](docs/en/evidence/public-object-reads.md).

Bucket create/get/delete and their declared versioning/hold effects are delivered;
see the [final verification](docs/en/evidence/bucket-core-parity.md).
Bucket metadata PATCH is available in its [documented subset](docs/en/slices/bucket-metadata-patch.md).
Bucket metadata PUT is delivered in its [documented replacement subset](docs/en/slices/bucket-metadata-put.md),
with [final verification](docs/en/evidence/bucket-metadata-put.md).
Next close the basic CRUD/list/metadata/download contract
gaps, then implement copy, bounded continuation rewrite and compose. Verify
the complete declared basic JSON scope with HTTP tests, pinned Go/Ruby SDKs
and native/container journeys. Then implement local HTTPS/TLS, JSON CORS defaults
and bucket-configured XML-read CORS, with actual-browser verification, before
starting signed URLs.

Signed GET is delivery order 16 and explicitly depends on the basic API, CORS
and local HTTPS verification gate. Signed PUT, signed-browser integration and
full Rails journeys follow; initial CORS/TLS implementation comes before signing.
The [basic API milestone](docs/en/roadmap.md#basic-json-api-milestone-before-signed-urls)
defines the six bucket and nine object methods, CORS/TLS acceptance conditions
and exclusions. HTTPS uses supplied development certificates and verified local
client trust; JSON CORS defaults and XML bucket-configured CORS have independent
rules. Public certificate issuance and DNS provisioning remain separate work.
Additional history interactions, retention/lifecycle and advanced IAM/ACL beyond the selected
source baseline, plus full Google method compatibility, remain separate work.
Source-required behavior stays required even when its historical phase is later.
These priorities have no
calendar delivery dates. Distribution verification has separate release gates
and publication authorization.

[The delivery roadmap](docs/en/roadmap.md) and the ledger's `delivery_plan` define
the ordered capabilities, prerequisites and completion conditions. P0-P7 remain
coverage groups; finishing a scope does not require every later extension in its
broad API row. Planning metadata adds no new implementation claim, and PARTIAL
is not assigned an arbitrary percentage.

## Feature comparison

Snapshot reviewed on **2026-10-23**: gcs-mimic's unreleased `0.1.0`
compared with fake-gcs-server `v1.56.1` and gcp-storage-emulator `v2026.07.19`.
Planned features are not counted.

**Legend:** ✅ the named workflow is available in the stated scope;
🚧 part of that scope is missing or unverified; ❌ unavailable.
The row's scope applies to every product. Extra capabilities in a cell do not
expand that scope, and a checkmark does not mean full Google API compatibility.
In particular, a scoped workflow can be available while its broader API method
remains `PARTIAL` in the [feature ledger](compatibility/features.json).
These workflow summaries derive from the ledger's cases and evidence; they
are not a second whole-method implementation-status system.
The other products were reviewed through their documentation, source and tests.
The pinned reference emulator's filesystem and memory backends also ran the
[bucket wire corpus](docs/en/evidence/bucket-core-comparison.md) and
[bucket PATCH corpus](docs/en/evidence/bucket-metadata-patch-comparison.md) and
[bucket PUT corpus](docs/en/evidence/bucket-metadata-put-comparison.md); their upstream
test suites were not run.

### 🧩 JSON API operations

The four PATCH/PUT rows use the shared
[metadata update criteria](#metadata-update-comparison-criteria) below.

For bucket create/get/delete, the completion target is the pinned reference
capability set, with additional gcs-mimic capabilities above it.
The executed [bucket baseline](docs/en/evidence/bucket-core-parity.md) includes
source-supported versioning and defaultEventBasedHold settings and their object
effects. Select `--compat-profile legacy-emulator` for source naming, omitted
project, resource defaults and deletion responses. Native behavior and strict
authorization retain their separate contracts. The three named workflows pass;
their broader Google method rows remain `PARTIAL`, with documented differences.

| Operation and comparison scope | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | --- | --- | --- |
| Bucket create (`buckets.insert`) | ✅ Create by name, versioning/default holds; source behavior via compatibility profile | ✅ Create by name | ✅ Create by name |
| Bucket metadata GET (`buckets.get`) | ✅ Basic resource; source defaults via compatibility profile | ✅ Basic resource | ✅ Basic resource |
| Partial response `fields` selection | ✅ Supported bucket/object/list/IAM JSON responses; bounded nested selectors and terminal wildcards; named edges return 501 | ❌ Executed on both backends: returns full bucket/object/list resources; [shared JSON handler](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/json_response.go) | ❌ Full resources; [response/router](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/server.py), source review only |
| JSON read ETag validators (`If-Match` / `If-None-Match`) | ✅ Bucket/policy GET and object metadata/media/range GET; combined query conditions, strong/weak comparison, 412/304 and permissions | ❌ Executed on both backends: wildcard If-None-Match and stale If-Match return 200 on scoped reads; [object GET](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/object.go), [bucket GET](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/bucket.go) | ❌ No evaluation in [object handlers](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/objects.py) or [bucket handlers](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/buckets.py), source review only |
| List live buckets with prefix and pagination (`buckets.list`) | ✅ Project/prefix filters and keyset paging | ✅ Prefix and paging | 🚧 All buckets; no prefix/paging |
| Bucket delete (`buckets.delete`) | ✅ Empty-bucket deletion; source response via compatibility profile | ✅ Empty-bucket deletion | ✅ Empty-bucket deletion |
| Bucket labels/default `storageClass` PATCH (merge/remove; `buckets.patch`) | ✅ Merge/key-null removal, whole-map null clearing (local interpretation), empty-map preservation and supported default class | ❌ Executed: labels/class ignored; other PATCH settings exist | 🚧 Source review: assigns fields; no label-key merge/null removal |
| Bucket labels/default `storageClass` PUT (replacement/defaults; `buckets.update`) | ✅ Executed HTTP/Go/native/Docker replacement and GET consistency; omitted/null labels clear, omitted class STANDARD (local defaults; SPEC-BUCKET-PUT) | ❌ Executed: all 32 PUT cases return JSON 404 on each pinned backend; metadata/bytes preserved | ❌ No bucket PUT route in pinned `server.py`; source review only |
| Single-request raw media upload | ✅ Bounded streamed bytes | ✅ | ✅ |
| Single-request JSON multipart metadata + media upload | ✅ Bounded metadata/media; optional checksum part also supported | ✅ | ✅ |
| JSON resumable initiation, chunks, status and finalization | ✅ Also scoped cancellation, seven-day expiry and restart recovery | ✅ Sessions and chunk uploads | ✅ Initiation, chunks, status and finalization |
| Read basic live-object metadata (`objects.get`; excludes ACL/owner) | ✅ Current live generation | ✅ | ✅ |
| Whole live-object media download (stored bytes) | ✅ Whole object; direct HTTP 200 | ✅ | ✅ |
| Single byte-range download (closed/open/suffix; excludes If-Range/transcoding) | ✅ Stored-byte ranges | ✅ | 🚧 Single range; explicit start required |
| List live objects with prefix/delimiter/start/end offsets (`objects.list`) | ✅ Prefix/delimiter/offsets | ✅ Prefix/delimiter/offsets | 🚧 Prefix/delimiter/glob filters; no start/end offsets |
| Live-object list pagination (`maxResults` / `pageToken`) | ✅ Query-bound tokens; bounded keyset seeks | ✅ `maxResults` / `pageToken` | ❌ |
| Object content/custom metadata PATCH (merge/null clearing; `objects.patch`) | ✅ All scoped fields; merge and null clearing | 🚧 Merges metadata; no null clearing | 🚧 Replaces metadata map; null fields ignored |
| Object content/custom metadata PUT (replacement/omission reset; `objects.update`) | ✅ All scoped fields; omitted fields reset | 🚧 No `contentEncoding` input; omitted fixed fields retained | ❌ |
| Remove a live object from active reads/listing (`objects.delete`) | ✅ Live generation; atomic conditions | ✅ Current or selected generation | ✅ Local soft delete by default |
| Object copy (`objects.copy`) | ❌ | ✅ | ✅ |
| Object rewrite (`objects.rewrite`) | ❌ | 🚧 Immediate copy; no continuation | 🚧 Immediate completion; no continuation |
| Object compose (`objects.compose`) | ❌ | ✅ | ✅ |
| Retained object version history | ✅ Persistent generations, selected reads/deletes and version paging | 🚧 Memory backend only | ❌ Versioning metadata is not history |
| Soft delete / restore | ❌ | ❌ | 🚧 Local deleted-object listing and restore |
| Generation/metageneration preconditions | 🚧 Atomic object upload/delete/metadata checks; scoped bucket checks | 🚧 Upload generation checks only | ❌ Not evaluated |
| MD5 and CRC32C calculation for uploaded object bytes | ✅ Media/multipart/resumable metadata and download headers | ✅ | ✅ |
| Supplied MD5/CRC32C validation | 🚧 Media `X-Goog-Hash`; multipart metadata/final JSON; resumable metadata/final whole-object `X-Goog-Hash`; no `Content-MD5` | 🚧 Multipart/resumable metadata checks | 🚧 Multipart/resumable metadata checks |

#### Metadata update comparison criteria

The same field set and update semantics apply to every product in these four
rows. Bucket fields are `labels` and the supported default `storageClass`
metadata; physical placement is outside this comparison. Object fields are
`contentType`, `contentEncoding`, `contentDisposition`, `contentLanguage`,
`cacheControl` and custom `metadata` on the live object. ACL/owner, holds,
retention, lifecycle and other advanced fields are outside these rows.

| Row | Requirements for ✅ within this field set |
| --- | --- |
| Bucket PATCH | Merge labels, remove selected label keys, update the supported default class and preserve omitted fields; empty labels merges no keys, labels:null clears the map; null/empty class is rejected. These null/empty rules are declared local choices pending SPEC-BUCKET-PATCH, not service-verified. |
| Bucket PUT | Replace all labels and the supported default class; omitted/null labels clear, omitted class becomes STANDARD, null/empty class fails. Freeze PUT independently of PATCH; these defaults are local choices pending SPEC-BUCKET-PUT. |
| Object PATCH | Change only supplied fixed fields; merge custom keys; null clears a fixed field or the map and removes individual custom keys; preserve other keys and empty-string custom values. |
| Object PUT | Replace all scoped fixed/custom metadata; reset omitted fixed fields and custom metadata, including clearing `contentEncoding`; absent content type serves as `application/octet-stream`. |

A checkmark requires the whole listed field set and semantics, with the
returned resource consistent with a subsequent GET. Updates must preserve
unrelated bucket settings and objects, or the object's bytes, generation,
checksums and creation time. For bucket updates, freeze the source-confirmed
clear/default rules before implementation, then record passing cases for all
of them. Accepting a field without applying it is insufficient. Permissions,
preconditions and broader durability guarantees retain their separate scopes.

gcs-mimic's object PATCH/PUT checkmarks use the executed
[metadata verification](docs/en/evidence/object-metadata.md) and
[integrated listing/metadata verification](docs/en/evidence/object-list-metadata.md),
including merge/removal, replacement/defaults, unchanged object bytes and GET
consistency. Bucket PATCH uses the [frozen contract](docs/en/slices/bucket-metadata-patch.md),
[reference comparison](docs/en/evidence/bucket-metadata-patch-comparison.md) and
[integration evidence](docs/en/evidence/bucket-metadata-patch.md). Whole-map null
clearing interprets the general PATCH deletion rule; scalar null/default,
unknown/output-only input and no-effect revision behavior remain unverified
against Google. The broader method rows remain `PARTIAL`. Bucket PUT uses its independently
[frozen replacement contract](docs/en/slices/bucket-metadata-put.md) and
[final integration evidence](docs/en/evidence/bucket-metadata-put.md).
Omitted-field defaults, null settings and
revision/error choices remain explicit local policy under SPEC-BUCKET-PUT.

The scoped checkmarks above use recorded execution evidence:
[final bucket core and history](docs/en/evidence/bucket-core-parity.md),
[bucket CRUD](docs/en/evidence/bucket-crud.md),
[object media](docs/en/evidence/object-media.md),
[listing and metadata](docs/en/evidence/object-list-metadata.md),
[multipart](docs/en/evidence/multipart.md),
[range](docs/en/evidence/object-range.md) and
[final integrated regressions](docs/en/evidence/resumable-upload.md).
Advanced fields, IAM/ACL, response replay, soft delete, transcoding and provider
wire-edge gaps remain recorded in the ledger. Copy, rewrite
and compose remain unavailable until implemented and verified; the new roadmap
does not turn them into completed features.

The pinned comparison source review covers
[fake-gcs-server bucket handlers](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/bucket.go),
[object handlers](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/object.go),
[shared metadata mutation](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/internal/backend/object.go),
[filesystem updates](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/internal/backend/fs.go),
[memory updates](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/internal/backend/memory.go),
[gcp-storage-emulator bucket handlers](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/buckets.py)
and [object handlers](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/objects.py).
Their implementations were inspected; their test suites were not executed.

### 🔐 Protocols and access

| Capability | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | --- | --- | --- |
| XML-style object endpoints | 🚧 Path-style object GET/HEAD only; generation, single ranges and match conditions | 🚧 Public-host path-style and virtual-host GET/HEAD; selected listing/upload routes | 🚧 Path-style GET/PUT only; no HEAD or virtual-host routing |
| gRPC API | ❌ | 🚧 Storage **v1** subset; not Storage v2 | ❌ |
| TLS listener | ❌ HTTP only | ✅ HTTPS, HTTP or both | ❌ HTTP only |
| Browser CORS | ❌ | 🚧 Wildcard CORS middleware | ❌ Stored CORS metadata only |
| ACL operations | ❌ | 🚧 Metadata operations; no enforcement | 🚧 Metadata/list operations; no enforcement |
| Bucket IAM policy get/set and enforcement | 🚧 Persistent unconditional predefined-role bindings, strict project-plus-bucket grants and atomic etags; policy methods remain PARTIAL for unsupported semantics and wire parity | ❌ No bucket IAM get/set routes or enforcement | 🚧 Stored policies; supplied etags kept without checking; no enforcement |
| Authentication / permission enforcement | 🚧 Strict profile: registered local tokens and project-plus-bucket predefined storage roles on JSON routes and XML reads | ❌ | ❌ |
| Public grants: enforce anonymous `allUsers` versus registered-only `allAuthenticatedUsers` reads | 🚧 Strict bucket IAM predefined roles; denial and revocation; no ACL/UBLA/PAP state machines | ❌ Reads do not evaluate IAM grants | ❌ Stored IAM/ACL metadata does not restrict reads |
| Public URL helper (encoded object address) | ✅ CLI builds encoded path-style URLs served by GET/HEAD; grants remain separate | ❌ `PublicURL()` returns a base origin only | ❌ |
| Signed-URL GET/PUT requests | ❌ | 🚧 Accepted without signature/expiry checks | 🚧 Accepted without signature/expiry checks |
| Signed-URL signature / expiry validation | ❌ | ❌ | ❌ |

An ACL value, stored IAM policy, accepted signed URL or CORS response does not
establish access control.

The bucket IAM comparison uses the same get/set-and-enforcement scope for every
product. In fake-gcs-server
[`v1.56.1` route registration](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/server.go#L253-L346),
bucket ACL routes exist but `/b/{bucket}/iam` GET/PUT routes do not. Its
[object handlers](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/object.go)
access the backend without evaluating caller IAM grants. gcp-storage-emulator
[`v2026.07.19` routes](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/server.py)
register policy get/set and `testPermissions`. Its
[bucket handlers](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/buckets.py)
store policies, preserve a nonempty supplied etag without comparing it, and do
not enforce grants. The `testPermissions` stub returns all requested permissions
for an existing bucket. These are source reviews, not differential execution
results.

The public-grant row uses the same anonymous-versus-authenticated read
enforcement scope for every product. Serving a path without checking a grant
does not satisfy that scope. In fake-gcs-server's
[`v1.56.1` public routes](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/server.go#L283-L315),
GET/HEAD supports `/<bucket>/<object>` on the configured public host
(`storage.googleapis.com` by default) and `/<object>` on bucket virtual hosts.
Its [`PublicURL()` helper](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/server.go)
returns the public origin rather than an encoded object address.
gcp-storage-emulator's
[`v2026.07.19` public routes and request handler](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/server.py)
register path-style GET/PUT, with no HEAD handler or bucket-from-host routing;
its [download handler](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/objects.py)
reads storage without evaluating IAM/ACL grants. These route capabilities do
not imply complete XML semantics or authenticated-only public access.

### 🛠️ Local development and distribution

| Capability | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | --- | --- | --- |
| Runtime / embedded use | Rust binary | Go binary or Go test library | Python CLI or Python test server |
| Disk persistence | 🚧 SQLite catalog and opaque blobs; documented recovery limits | ✅ Filesystem backend | ✅ Files and JSON metadata |
| Optional in-memory backend | ❌ | ✅ | ✅ |
| Fixture import | ✅ Native read-only initial seed before readiness; no replay; Linux and Compose lifecycle verified | ✅ Seed directory / initial objects | 🚧 Default bucket creation only |
| Published native binaries | 🚧 Eight-target CI configured; no release yet | ✅ macOS/Linux amd64+arm64; Windows amd64 | ❌ Python runtime required |
| Published Docker image | 🚧 Source build; GHCR workflow configured | ✅ Docker Hub | ✅ Docker Hub |
| Project license | MIT OR Apache-2.0 | BSD-2-Clause | BSD-3-Clause |

Disk persistence does not imply equivalent crash or power-loss guarantees.
CPU, memory, throughput and image size are not ranked: no equivalent cross-product
benchmark was run.

gcs-mimic stores metadata in SQLite and object bytes in opaque immutable files.
The storage investigation in `docs/en/storage-backends.md` compares filesystem,
xattr/JSON, memory, SQLite BLOB and embedded key/value approaches. Transactional
metadata plus streamed opaque files is the recommended durable default; this is
a requirement-based decision, with no comparative benchmark claim.

The product target is a complete behavioral superset of fake-gcs-server after
renaming `FAKE_GCS_*` to `GCS_MIMIC_*` and explicitly converting existing data.
Legacy keys will not be aliases or fallback inputs. The migration contract in
`docs/en/fake-gcs-server-migration.md` includes flags, TLS, endpoints, Go
embedding, Storage v1 gRPC, helper/event behavior and both source backends.
Lossless import, equivalent renamed settings and the legacy compatibility
profile remain planned. Existing comparison marks describe current behavior.

## Run with Docker

Requires Git, Docker Engine/Desktop with Buildx, and curl. Linux amd64 and arm64 are supported. Build from source; no container image is published.

```sh
git clone https://github.com/shiroemons/gcs-mimic.git
cd gcs-mimic
docker build -t gcs-mimic:dev .
COMPOSE_PROJECT_NAME=gcs-mimic-bucket-demo GCS_MIMIC_PORT=0 docker compose up -d --wait
COMPOSE_PROJECT_NAME=gcs-mimic-bucket-demo docker compose exec -T --interactive=false gcs \
  gcs-mimic healthcheck --url http://127.0.0.1:4443/_internal/ready
```

The service is exposed only on host loopback. Follow the [runnable guide](docs/en/getting-started.md) to discover the allocated port, set the advertised endpoint and exercise buckets and object media from host and peer containers.

```sh
COMPOSE_PROJECT_NAME=gcs-mimic-bucket-demo docker compose stop
```

To require authentication, add `compose.strict.yaml`. It selects
`auth_mode=strict` and mounts the local fixture registry `config/identities.json`,
whose admin, object-viewer, object-creator and bucket-reader tokens are public emulator test
values, not Google credentials. Send them as `Authorization: Bearer <token>`;
anonymous requests without a matching public bucket grant return 401 and
missing permissions return 403. Only
project and bucket grants of predefined storage roles are enforced. The
bucket-reader starts without project roles; follow the
[bucket-only grant walkthrough](docs/en/getting-started.md#grant-and-revoke-bucket-only-access)
to grant and revoke object reads on one bucket.

```sh
COMPOSE_PROJECT_NAME=gcs-mimic-bucket-demo GCS_MIMIC_PORT=0 \
  docker compose -f compose.yaml -f compose.strict.yaml up -d --wait
```

`gcs-mimic url public --base-url URL --bucket NAME --object NAME` prints an encoded
path-style object URL and changes no permission. XML GET/HEAD serves that URL;
strict mode requires a matching project or bucket grant. Follow the
[public/private walkthrough](docs/en/getting-started.md#public-and-private-object-reads)
to grant `allUsers`, verify restart persistence, restrict access to
`allAuthenticatedUsers` and revoke it. The base URL must be reachable by the
consumer: host browsers use the published host port, while Compose peers use
`http://gcs:4443`. Browser cross-origin fetch still needs the planned CORS work,
which is scheduled with local HTTPS/TLS before signed URLs.

The container runs as a non-root user on a read-only root, with a writable named data volume. Compose sets limits of 128 MiB memory, one CPU and 64 processes. These are configured limits, not measured resource requirements. Ordinary stop/down preserves the catalog; deliberate volume removal resets it.

The runtime uses digest-pinned Distroless Debian 13 with glibc/libgcc and a stripped
binary. It has no shell or package manager; use the built-in `healthcheck` command
and container logs for diagnostics. UID/GID `10001:10001` remain compatible with
existing data volumes. Project and third-party notices are included in the image,
along with the OS notices retained by Distroless. See the
[GHCR distribution procedure](docs/en/container-distribution.md) for future
version/minor/latest tags, digest pinning and first-publication setup.

A prior build comparison measured complete-image compressed layers decreasing from 30.57 to 12.14 MiB on
amd64 and from 30.92 to 12.38 MiB on arm64, about 60% in both cases. See the
[measurement and verification details](docs/en/evidence/distroless.md). These are
image-size results; runtime memory and CPU improvements have not been measured.

## Run the binary

Requires rustup to build; the resulting binary needs no Rust installation or Python to run.

The [native distribution foundation](docs/en/binary-distribution.md) defines eight
future release targets, archive contents, checksums and the manual/tag workflows.
No prebuilt release is available yet. Windows uses a separate persistence profile:
normal restart and process termination recovery are tested by its configured CI;
OS-crash and power-loss namespace durability are not guaranteed. All-platform CI
execution remains pending.

```sh
rustup toolchain install 1.99.1 --profile minimal
cargo +1.99.1 build --release --locked --bin gcs-mimic
./target/release/gcs-mimic serve --config config/development.json
```

Check readiness from another terminal, then stop with Ctrl-C:

```sh
./target/release/gcs-mimic healthcheck --url http://127.0.0.1:4443/_internal/ready
```

Configuration precedence is command-line options, `GCS_MIMIC_` environment variables, JSON configuration, then defaults. Use `--help` for options and limits. See the [build and run guide](docs/en/getting-started.md) for configuration, container connectivity and troubleshooting.

## Current limitations

Object APIs support the documented [media](docs/en/slices/json-object-media.md), [multipart](docs/en/slices/json-object-multipart.md), [resumable](docs/en/slices/resumable-upload.md), [range](docs/en/slices/json-object-range.md), [listing](docs/en/slices/json-object-list.md), [metadata](docs/en/slices/json-object-metadata.md) and [public/private XML reads](docs/en/slices/public-object-reads.md) subsets. JSON resumable initiation returns an opaque capability Location from configured `api_base_url`; sequential PUT/SDK POST chunks, durable status/resume, cancellation, fixed seven-day expiry and restart recovery are supported. Objects become visible only after final checksum/metadata/permission/condition validation; completed-session status returns the original resource without another generation. Retention is bounded to 128 sessions, including completed/invalid records until their original deadline. One configured API base must be reachable by its client; use separate host/peer configurations or common DNS/proxy. XML/signed resumable, invocation-token response replay, If-Range, transcoding, version history, object IAM and bucket/object ACLs remain unavailable. Single untranscoded closed/open/suffix ranges return 206; unsatisfiable GET ranges return 416. XML HEAD returns 200 with Content-Range, including the local bytes */size response for unsatisfiable ranges. Malformed, multiple and repeated ranges use a documented local 200 fallback. Listing rejects nonempty `matchGlob`/`filter`, `versions=true`, soft-deleted and folder-prefix options with 501. Metadata patch/update covers content headers and custom metadata; unsupported holds/retention/ACL settings return 501. Without stored Cache-Control, public media uses `public, max-age=3600`, private XML uses `private, max-age=0` and nonpublic JSON media uses `no-cache`. Stored Cache-Control takes precedence for XML and public JSON media. Go SDK multipart and normal nonzero-ChunkSize resumable Writer, and default/XML or explicit JSON `NewRangeReader`, work for the scoped subset. Ordinary media GET returns a direct 200; accepted GET/DELETE/update retry tokens are not deduplicated. Resumable initiation accepts a token without generation-match and creates another session on retry; media/multipart tokens still require generation-match. SPEC-013 and the resumable/listing/range gaps remain open. Objects default to a configurable 1 GiB cap; [native resumable measurements](docs/en/evidence/resumable-upload.md#bounded-resources-and-measured-results) cover large, slow and concurrent transfers, without a cgroup or 1 GiB constrained-container claim.

[Regression evidence](docs/en/evidence/object-media.md) covers upload admission, advisory checks, explicit upload EOF, committed outcomes and recovery. Transfers have a configurable 15-minute absolute ceiling in addition to body inactivity; catalog waits default to 30 seconds and return 503 on expiry. Resumable finalization rereads the whole object for hashing under the absolute request budget; resolution SQL after a prepublication failure can also wait for that budget rather than the catalog timeout. After a publication timeout the mutation outcome is unknown: query session status or re-read state, because the commit may still complete. Filesystem work and shutdown catalog close have no separate execution bound. Unreleased schema-v2 catalogs are rejected; object-free v1 catalogs and supported v3–v6 catalogs migrate to v7, preserving object metadata, seed initialization and bucket IAM while adding indexed resumable sessions. Migrated catalogs do not import fixtures.

Initial [seed directory import](docs/en/slices/seed.md) runs before readiness and does not replay fixtures over existing data. Bucket IAM supports unconditional predefined-role bindings, `allUsers` / `allAuthenticatedUsers` and etag concurrency; object IAM, testIamPermissions, ACLs, conditions, custom/basic roles, UBLA/PAP settings, signed URLs, CORS and TLS are unavailable. Strict mode combines project and bucket grants, and an invalid token never falls back to anonymous access. Revocation protects new reads but cannot recall cached bytes or abort an already-open read. See the [strict authentication contract](docs/en/slices/strict-auth.md) and SPEC-017 for local error and missing-bucket rules. Bucket APIs support only the [documented subset](docs/en/slices/json-bucket-crud.md); `full` projection omits ACL/owner, project IDs are locally simulated, and recognized unsupported settings return 501. XML virtual-host routing, writes and listing remain unavailable. Native data defaults to cwd-relative `data`; `--data-dir`, `--api-base-url` and `--max-object-bytes` are explicit overrides. Host power-loss durability and Ruby/Rails remain unverified.

For contributors, install [just](https://github.com/casey/just#installation) and
run `just check` while editing, then `just pre-push` before pushing to run the
Rust, Go SDK and both Linux container architecture checks. See the
[development guide](docs/en/development.md) for prerequisites and CI limitations.

## Versioning and releases

The current `0.1.0` is an unreleased development version. During `0.x`, compatible
fixes increment patch; new functionality and breaking changes increment minor.
Breaking changes include documented CLI, configuration, API and persistent-data
compatibility changes and require release notes and migration guidance. Published
versions are immutable. See the [versioning and licensing policy](docs/en/release-policy.md)
for the full contract and first-release requirements.

The **Release binaries** workflow prepares verification artifacts on manual
dispatch. Future `vX.Y.Z` tags require a matching Cargo version, committed release
notes, freshness checks, the existing Foundation CI and all eight native suites
before publishing archives with SHA-256 checksums. See the
[release procedure and platform limits](docs/en/binary-distribution.md).

## License

Copyright (c) 2026 shiroemons

Original project code and documentation are dual-licensed under the
[MIT License](LICENSE-MIT) or the [Apache License, Version 2.0](LICENSE-APACHE),
at your option (`MIT OR Apache-2.0`). Third-party material retains its own license.

This is an independent project and is not an official Google product.
