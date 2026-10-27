# Detailed emulator comparison

<!-- markdownlint-configure-file {"MD013": false} -->

Comparison baseline: **2026-10-23** (filesystem follow-up: **2026-10-24**): gcs-mimic's pre-release development source (the first image is `0.1.1`)
compared with fake-gcs-server `v1.56.1` and gcp-storage-emulator `v2026.07.19`.
Planned features are not counted.

**Legend:** ✅ the named workflow is available in the stated scope;
🚧 part of that scope is missing or unverified; ❌ unavailable.
The row's scope applies to every product. Extra capabilities in a cell do not
expand that scope, and a checkmark does not mean full Google API compatibility.
In particular, a scoped workflow can be available while its broader API method
remains `PARTIAL` in the [feature ledger](../../compatibility/features.json).
These workflow summaries derive from the ledger's cases and evidence; they
are not a second whole-method implementation-status system.
The other products were reviewed through their documentation, source and tests.
The filesystem investigation additionally executes targeted macOS file-edit,
seed/reseed and CORS probes; see [storage behavior](fake-gcs-server-storage.md).
Every row applies the same behavior and exclusions to all products. Copy bytes,
rewrite continuation, CORS policy enforcement and signature validation are
separate checks; missing ACL projection does not fail a bytes-only copy row.
The pinned reference emulator's filesystem and memory backends also ran the
[bucket wire corpus](evidence/bucket-core-comparison.md) and
[bucket PATCH corpus](evidence/bucket-metadata-patch-comparison.md) and
[bucket PUT corpus](evidence/bucket-metadata-put-comparison.md); their upstream
full test suites were not run. The later
[configuration investigation](fake-gcs-server-configuration.md) ran the pinned
fake-gcs-server configuration package tests and focused environment probes.

## 🧩 JSON API operations

The four PATCH/PUT rows use the shared
[metadata update criteria](#metadata-update-comparison-criteria) below.

For bucket create/get/delete, the completion target is the pinned reference
capability set, with additional gcs-mimic capabilities above it.
The executed [bucket baseline](evidence/bucket-core-parity.md) includes
source-supported versioning and defaultEventBasedHold settings and their object
effects. Select `--compat-profile legacy-emulator` for source naming, omitted
project, resource defaults and deletion responses. Native behavior and strict
authorization retain their separate contracts. The three named workflows pass;
their broader Google method rows remain `PARTIAL`, with documented differences.

| Operation and comparison scope | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | --- | --- | --- |
| Bucket create (`buckets.insert`) | ✅ Create by name, versioning/default holds; source behavior via compatibility profile | ✅ Create by name | ✅ Create by name |
| Bucket metadata GET (`buckets.get`) | ✅ Basic resource; source defaults via compatibility profile | ✅ Basic resource | ✅ Basic resource |
| Partial response `fields` selection | ✅ Supported bucket/object/list/IAM JSON responses; bounded nested selectors and terminal wildcards; named edges return 400 | ❌ Executed on both backends: returns full bucket/object/list resources; [shared JSON handler](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/json_response.go) | ❌ Full resources; [response/router](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/server.py), source review only |
| JSON read ETag validators (`If-Match` / `If-None-Match`) | ✅ Bucket/policy GET and object metadata/media/range GET; combined query conditions, strong/weak comparison, 412/304 and permissions | ❌ Executed on both backends: wildcard If-None-Match and stale If-Match return 200 on scoped reads; [object GET](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/object.go), [bucket GET](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/bucket.go) | ❌ No evaluation in [object handlers](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/objects.py) or [bucket handlers](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/buckets.py), source review only |
| List live buckets with prefix and pagination (`buckets.list`) | ✅ Project/prefix filters and keyset paging | ✅ Prefix and paging | 🚧 All buckets; no prefix/paging |
| Bucket delete (`buckets.delete`) | ✅ Empty-bucket deletion; source response via compatibility profile | ✅ Empty-bucket deletion | ✅ Empty-bucket deletion |
| Bucket labels/default `storageClass` PATCH (merge/remove; `buckets.patch`) | ✅ Merge/key-null removal, whole-map null clearing (local interpretation), empty-map preservation and supported default class | ❌ Executed: labels/class ignored; other PATCH settings exist | 🚧 Source review: assigns fields; no label-key merge/null removal |
| Bucket labels/default `storageClass` PUT (`buckets.update`) | ✅ HTTP/Go/Ruby preserve omitted settings; supplied labels replace the map, null clears (local SDK choice; SPEC-BUCKET-PUT). Historical native/Docker runs used omission resets | ❌ Executed: all 32 PUT cases return JSON 404 on each pinned backend; metadata/bytes preserved | ❌ No bucket PUT route in pinned `server.py`; source review only |
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
| Copy live-object bytes (`objects.copy`; excludes ACL/owner) | ✅ | ✅ | ✅ |
| One-call rewrite of live-object bytes (`objects.rewrite`; excludes ACL/owner) | ✅ | ✅ | ✅ |
| Multi-call rewrite with continuation tokens | ✅ | ❌ Immediate completion only | ❌ Immediate completion only |
| Object compose (`objects.compose`) | ✅ Scoped ordered 1..32 pinned sources, nested component/CRC32C metadata without MD5, atomic conditions and source hard deletion | ✅ | ✅ |
| Retained object version history | ✅ Persistent generations, selected reads/deletes and version paging | 🚧 Memory backend only | ❌ Versioning metadata is not history |
| Soft delete / restore | ❌ | ❌ | 🚧 Local deleted-object listing and restore |
| Single generation/metageneration mutation Match conditions, including failed-resource preservation | ✅ Object media/multipart/PATCH/PUT/delete and bucket PATCH/PUT/delete; zero create-if-absent and live-object rejection | 🚧 Upload generation conditions only; [upload](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/upload.go), [object](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/object.go) and [bucket](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/bucket.go), source review only | ❌ Conditions unused in [object](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/objects.py) and [bucket](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/buckets.py) handlers, source review only |
| MD5/CRC32C calculation (media/multipart; resumable CRC32C and single-data-request MD5) | ✅ Exact full-byte resource encodings in the declared subset | Source review only: MD5/CRC32C calculation in [upload handlers](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/upload.go); exact scoped behavior not executed | Source review only: MD5/CRC32C calculation in [object handlers](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/objects.py); exact scoped behavior not executed |
| Supplied MD5/CRC32C rejection with failed-overwrite preservation in documented JSON containers | ✅ Media X-Goog-Hash, multipart metadata/final CRC32C, resumable CRC32C and single-data-request MD5; no Content-MD5 claim | 🚧 Metadata checks before publication; media/final X-Goog-Hash absent in [upload handlers](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/upload.go), source review only | 🚧 Metadata checks precede create but mismatch raises 409 Conflict; media/final X-Goog-Hash absent in [object handlers](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/objects.py), source review only |
| JSON error envelopes, isolated 404/412/416 reasons and empty DELETE/304 bodies | ✅ Numeric code, message and errors entries; fields preserves errors; exact text/headers and local DELETE status excluded | 🚧 Envelope exists, but the [shared reason mapper](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/json_response.go) has no requestedRangeNotSatisfiable case, source review only | 🚧 Some full error objects; other [object failures](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/objects.py) return only error.message, source review only |

The [basic JSON evidence](evidence/basic-json-contract.md) records the
local condition/checksum/error verification and pinned source-file digests.
The other-product cells for mutation conditions, checksum calculation/rejection
and error envelopes use source review only; the
existing executed reference corpus covers bucket metadata, fields and read ETags.

### Metadata update comparison criteria

The same field set and update semantics apply to every product in these four
rows. Bucket fields are `labels` and the supported default `storageClass`
metadata; physical placement is outside this comparison. Object fields are
`contentType`, `contentEncoding`, `contentDisposition`, `contentLanguage`,
`cacheControl` and custom `metadata` on the live object. ACL/owner, holds,
retention, lifecycle and other advanced fields are outside these rows.

| Row | Requirements for ✅ within this field set |
| --- | --- |
| Bucket PATCH | Merge labels, remove selected label keys, update the supported default class and preserve omitted fields; empty labels merges no keys, labels:null clears the map; null/empty class is rejected. These null/empty rules are declared local choices pending SPEC-BUCKET-PATCH, not service-verified. |
| Bucket PUT | Preserve omitted settings; replace a supplied labels map, clear null/empty labels, reject null/empty class. This local choice supports Ruby update blocks and remains unverified against GCS under SPEC-BUCKET-PUT. |
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
[metadata verification](evidence/object-metadata.md) and
[integrated listing/metadata verification](evidence/object-list-metadata.md),
including merge/removal, replacement/defaults, unchanged object bytes and GET
consistency. Bucket PATCH uses the [frozen contract](slices/bucket-metadata-patch.md),
[reference comparison](evidence/bucket-metadata-patch-comparison.md) and
[integration evidence](evidence/bucket-metadata-patch.md). Whole-map null
clearing interprets the general PATCH deletion rule; scalar null/default,
unknown/output-only input and no-effect revision behavior remain unverified
against Google. The broader method rows remain `PARTIAL`. Bucket PUT uses its independently
[omission-preserving local contract](slices/bucket-metadata-put.md) and
[historical integration evidence](evidence/bucket-metadata-put.md).
Omitted-field preservation, null settings and
revision/error choices remain explicit local policy under SPEC-BUCKET-PUT.

The scoped checkmarks above use recorded execution evidence:
[final bucket core and history](evidence/bucket-core-parity.md),
[bucket CRUD](evidence/bucket-crud.md),
[object media](evidence/object-media.md),
[listing and metadata](evidence/object-list-metadata.md),
[multipart](evidence/multipart.md),
[range](evidence/object-range.md) and
[final integrated regressions](evidence/resumable-upload.md).
Advanced fields, IAM/ACL, response replay, soft delete, transcoding and provider
wire-edge gaps remain recorded in the ledger. Copy has scoped
[local verification](evidence/object-copy.md). The
[compose verification](evidence/object-compose.md) records native and
targeted Go coverage, explicit 400 boundaries and local deletion safety rules.
The [shared final gate](evidence/derived-objects.md) passes all Rust/Go checks,
both container architectures and the affected native/Docker guide journeys.
Rewrite supports the declared
[local contract](slices/object-rewrite.md), including bounded continuation,
request-bound tokens, immediate retry, strict permissions and restart recovery.
Its method row remains PARTIAL for full ACL projection; the local session row
is IMPLEMENTED/EXACT in its declared scope.
Once rewrite mutation starts, it awaits its durable result across the configured
request deadline so a timeout cannot hide a later publication.

The pinned comparison source review covers
[fake-gcs-server bucket handlers](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/bucket.go),
[object handlers](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/object.go),
[shared metadata mutation](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/internal/backend/object.go),
[filesystem updates](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/internal/backend/fs.go),
[memory updates](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/internal/backend/memory.go),
[gcp-storage-emulator bucket handlers](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/buckets.py)
and [object handlers](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/objects.py).
Their implementations were inspected; full upstream test suites were not executed.

## 🔐 Protocols and access

| Capability | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | --- | --- | --- |
| Path-style object GET/HEAD/PUT (bytes only; excludes listing) | ✅ | ✅ Public-host routing | 🚧 GET/PUT; no HEAD |
| gRPC API | 🚧 Scoped Storage **v2** bucket/object operations and bidirectional reads; [SDK coverage](../../sdk-tests/go/README.md). Storage v1 is a separate slice in progress. | 🚧 Storage **v1** subset; not Storage v2 | ❌ |
| TLS listener | ✅ HTTPS-only (default) with built-in development certificate or supplied pair; explicit HTTP-only, TLS 1.2/1.3 and bounded handshake; [verified scope](evidence/local-tls.md) | ✅ HTTPS (default), HTTP or both; supplied pair or Go test certificate; [pinned implementation](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/main.go#L27-L76), source review | ❌ HTTP only; source review |
| Default CORS for JSON API requests | ✅ | ✅ Global middleware | ❌ |
| Enforce bucket-configured XML GET/HEAD/PUT CORS | ✅ | ❌ | ❌ |
| ACL operations | ❌ | 🚧 Metadata operations; no enforcement | 🚧 Metadata/list operations; no enforcement |
| Store/retrieve unconditional bucket IAM bindings | ✅ | ❌ | ✅ Stored only |
| Enforce predefined-role bucket IAM grants | ✅ Strict local identities | ❌ | ❌ |
| Enforce strict local bearer-token permissions on scoped JSON/XML routes | ✅ | ❌ | ❌ |
| Enforce anonymous `allUsers` versus registered-only `allAuthenticatedUsers` reads | ✅ Strict local bucket IAM | ❌ | ❌ |
| Public URL helper (encoded object address) | ✅ CLI builds encoded path-style or virtual-host URLs served by GET/HEAD; grants remain separate | ❌ `PublicURL()` returns a base origin only | ❌ |
| Upload/download bytes with V4 RSA signed GET/PUT URLs | ✅ | ✅ No signature validation | ✅ No signature validation |
| Signed-URL signature / expiry validation | ✅ V4 RSA signature, final host/path/query/method/headers and injected-clock expiry/skew; exact provider errors remain PARTIAL; [scope and evidence](evidence/signed-urls.md) | ❌ [Handlers](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/object.go) do not verify signing credentials; source review only | ❌ [Dispatch/handlers](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/server.py) do not verify signing credentials; source review only |
| XML response Content-Type / Content-Disposition overrides | ✅ Authorized GET/HEAD, ranges and V4 query binding without metadata mutation; [scope](slices/xml-response-overrides.md) | ✅ Both response headers in [download handler](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/object.go#L1162-L1174); source review only | 🚧 Content-Disposition only in [download handler](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/objects.py#L687-L740); source review only |
| Virtual-hosted-style object endpoints | ✅ Explicit DNS suffix opt-in, GET/HEAD/PUT and strict signing; supplied matching SAN required for HTTPS; [scope](slices/xml-virtual-host.md) | ✅ Bucket-host GET/HEAD and upload routes in [router](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/server.go#L306-L343); source review only | ❌ Path-only [public routes](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/server.py#L133-L143); source review only |
| Public access prevention with preserved grants and valid signed access | ✅ Bucket enforcement and stored-grant reactivation; parent policy is LOCAL_SIMULATION; [scope](slices/public-access-prevention.md) | ❌ [Object handlers](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/object.go) do not enforce public grants/PAP; source review only | ❌ [Download handler](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/objects.py#L687-L740) does not enforce public grants/PAP; source review only |

An ACL value, stored IAM policy, accepted signed URL or CORS response does not
establish access control. Actual-browser execution is a separate evidence question:
gcs-mimic has [trusted-HTTPS signed-browser results](evidence/signed-browser.md);
this comparison did not run either other product in a browser.

Bucket IAM storage and grant enforcement are separate rows for every product.
Conditional policies, ACLs and UBLA transitions are outside these scoped rows. In fake-gcs-server
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

## 🛠️ Local development and distribution

| Capability | gcs-mimic | fake-gcs-server | gcp-storage-emulator |
| --- | --- | --- | --- |
| Runtime / embedded use | Rust binary | Go binary or Go test library | Python CLI or Python test server |
| Preserve API-created bytes across normal restart | ✅ SQLite catalog and opaque files | ✅ Filesystem backend | ✅ Files and JSON metadata |
| API-created objects appear as bucket/object files | ❌ Opaque internal IDs | ✅ Filesystem backend | ✅ Disk backend |
| Reconcile external edits with hashes and generation metadata | ❌ | ❌ Direct reads can expose stale metadata | ❌ No reconciliation contract |
| Optional in-memory backend | ❌ | ✅ | ✅ |
| Import a bucket/object seed directory | ✅ One-time native import | ✅ Startup and explicit reseed | ❌ Default bucket creation is separate |
| Published native binaries | ✅ macOS, Windows and Linux amd64+arm64 | ✅ macOS/Linux amd64+arm64; Windows amd64 | ❌ Python runtime required |
| Published Docker image | ✅ GHCR linux/amd64+arm64 | ✅ Docker Hub | ✅ Docker Hub |
| Project license | MIT OR Apache-2.0 | BSD-2-Clause | BSD-3-Clause |

Disk persistence does not imply equivalent crash or power-loss guarantees.
CPU, memory, throughput and image size are not ranked: no equivalent cross-product
benchmark was run.

gcs-mimic stores metadata in SQLite and object bytes in opaque immutable files.
The storage investigation in `storage-backends.md` compares filesystem,
xattr/JSON, memory, SQLite BLOB and embedded key/value approaches. Transactional
metadata plus streamed opaque files is the recommended durable default; this is
a requirement-based decision, with no comparative benchmark claim.

The product target is a complete behavioral superset of the pinned fake-gcs-server
baseline, with documented native configuration mappings and explicit data conversion.
Matching flag names or environment-variable suffixes is not required.
Legacy keys will not be aliases or fallback inputs. The migration contract in
`fake-gcs-server-migration.md` includes flags, TLS, endpoints, Go
embedding, Storage v1 gRPC, helper/event behavior and both source backends.
Lossless import and the remaining configuration roles are still planned. The existing
`legacy-emulator` profile covers a limited bucket contract. Existing comparison marks describe current behavior.
