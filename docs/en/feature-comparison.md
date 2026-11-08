# Detailed emulator comparison

<!-- markdownlint-configure-file {"MD013": false} -->

Comparison baseline: **2026-10-23** (filesystem follow-up: **2026-10-24**; sync directory follow-up: **2026-10-28**): gcs-mimic, with its column updated for `0.8.0` (**2026-11-01**),
compared with fake-gcs-server `v1.56.1` and gcp-storage-emulator `v2026.07.19`.
The production-like access follow-up (**2026-11-08**) adds gcs-mimic capabilities
that need 0.14.0 or later; each such cell says so.
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
| Bucket labels/default `storageClass` PUT (`buckets.update`) | ✅ Native PUT replaces the resource (clears omitted labels/versioning/CORS/lifecycle/retention/default hold; keeps class, soft delete and access settings); legacy-emulator preserves omitted settings (SPEC-BUCKET-PUT) | ❌ Executed: all 32 PUT cases return JSON 404 on each pinned backend; metadata/bytes preserved | ❌ No bucket PUT route in pinned `server.py`; source review only |
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
| Object soft delete / restore (excludes bucket and bulk restore) | ✅ Opt-in policy retention (off by default and accepting any retention from one second, both unlike Cloud Storage), immutable `hardDeleteTime` and persistent generations; [scope](slices/sdk-soft-delete-restore.md); [SDK scorecard](../../sdk-scorecard/results/2026-10-31-2/summary.md) passes in all eight SDKs | ❌ [SDK scorecard](../../sdk-scorecard/results/2026-10-31-2/summary.md) fails in all eight SDKs | ✅ [SDK scorecard](../../sdk-scorecard/results/2026-10-31-2/summary.md) passes in seven of eight SDKs (Rust fails: its bucket setup needs gRPC); policy retention and `hardDeleteTime` in [storage](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/storage.py), source review |
| Single generation/metageneration mutation Match conditions, including failed-resource preservation | ✅ Object media/multipart/PATCH/PUT/delete and bucket PATCH/PUT/delete; zero create-if-absent and live-object rejection | 🚧 Upload generation conditions only; [upload](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/upload.go), [object](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/object.go) and [bucket](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/bucket.go), source review only | ❌ Conditions unused in [object](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/objects.py) and [bucket](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/buckets.py) handlers, source review only |
| MD5/CRC32C calculation (media/multipart; resumable CRC32C and single-data-request MD5) | ✅ Exact full-byte resource encodings in the declared subset | Source review only: MD5/CRC32C calculation in [upload handlers](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/upload.go); exact scoped behavior not executed | Source review only: MD5/CRC32C calculation in [object handlers](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/objects.py); exact scoped behavior not executed |
| Supplied MD5/CRC32C rejection with failed-overwrite preservation in documented JSON containers | ✅ Multipart metadata/final CRC32C, resumable CRC32C and single-data-request MD5; JSON media uploads ignore X-Goog-Hash/Content-MD5 as recorded from Cloud Storage | 🚧 Metadata checks before publication; media/final X-Goog-Hash absent in [upload handlers](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/upload.go), source review only | 🚧 Metadata checks precede create but mismatch raises 409 Conflict; media/final X-Goog-Hash absent in [object handlers](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/objects.py), source review only |
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
| Bucket PUT | Replace the resource: the native profile clears omitted labels, versioning, CORS, lifecycle, retention policy (as recorded from Cloud Storage) and default event-based hold (extrapolated from those recorded fields) and keeps the omitted storage class, soft delete policy and access settings; the legacy-emulator profile preserves every omitted setting as a local choice (SPEC-BUCKET-PUT). A supplied labels map replaces all keys, null/empty labels clear it, null/empty class is rejected. |
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
against Google. The broader method rows remain `PARTIAL`. Bucket PUT follows the
recorded Cloud Storage replacement in the native profile and the
omission-preserving local contract in the legacy-emulator profile
([contract](slices/bucket-metadata-put.md),
[historical integration evidence](evidence/bucket-metadata-put.md)).
Legacy omission preservation, null settings and
revision/error choices remain explicit local policy under SPEC-BUCKET-PUT.

The scoped checkmarks above use recorded execution evidence:
[final bucket core and history](evidence/bucket-core-parity.md),
[bucket CRUD](evidence/bucket-crud.md),
[object media](evidence/object-media.md),
[listing and metadata](evidence/object-list-metadata.md),
[multipart](evidence/multipart.md),
[range](evidence/object-range.md) and
[final integrated regressions](evidence/resumable-upload.md).
Advanced fields, ACL enforcement, response replay outside media/multipart/copy,
bucket and bulk restore, transcoding outside JSON media GET and XML GET/HEAD and provider
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
| gRPC Storage v2 with the official Go client over plaintext h2c: bucket CRUD/updates/IAM, object write/read/list/update/delete/restore, resumable start/query/cancel, ranged and bidirectional reads, rewrite/compose/move, appendable objects with Writer.Append, takeover and finalization (standard buckets; zonal Rapid buckets are not modeled) | ✅ Shared JSON/XML state and strict authorization; [SDK coverage](../../sdk-tests/go/README.md), [cross-protocol evidence](slices/cross-protocol-objects.md) | ❌ Storage v1 only; source review | ❌ |
| gRPC legacy Storage v1: the pinned reference's twelve RPCs (bucket insert/get/update/list/delete, object insert/get/update/patch/compose/list/delete), including multi-message InsertObject and project-free ListBuckets | ✅ Documented field subset; [legacy contract](slices/grpc-v1-legacy.md) | 🚧 InsertObject keeps only the first message's data; [pinned server](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/internal/grpc/server.go), source review | ❌ |
| TLS listener | ✅ HTTPS-only (default) with built-in development certificate or supplied pair; explicit HTTP-only, TLS 1.2/1.3 and bounded handshake; [verified scope](evidence/local-tls.md) | ✅ HTTPS (default), HTTP or both; supplied pair or Go test certificate; [pinned implementation](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/main.go#L27-L76), source review | ❌ HTTP only; source review |
| Default CORS for JSON API requests | ✅ | ✅ Global middleware | ❌ |
| Enforce bucket-configured XML GET/HEAD/PUT CORS | ✅ Buckets without rules follow `cors_fallback`: since 0.14.0 it defaults to `allow-all` in permissive mode, as fake-gcs-server answers every origin, and to `none` in strict mode; [contract](slices/cors-fallback.md) | ❌ | ❌ |
| ACL operations | 🚧 Bucket, default object and object ACL CRUD, atomic replacement through bucket/object PATCH and PUT, default ACL inheritance and UBLA rejection; no enforcement; [contract](slices/json-acl.md) | 🚧 Metadata operations; no enforcement | 🚧 Metadata/list operations; no enforcement |
| Project HMAC keys and service agent | 🚧 Persistent key lifecycle with one-time secrets; ACTIVE keys verify V4 (and scoped V2) HMAC signatures; [contract](slices/project-hmac-keys.md) | ❌ SDK scorecard | ❌ SDK scorecard |
| Bucket notification configurations | 🚧 Stored and listed; with an explicit local Pub/Sub emulator, events are delivered at least once through a durable outbox and insert checks the topic (LOCAL_SIMULATION); [contract](slices/bucket-notifications.md), [delivery](slices/pubsub-notification-delivery.md) | ✅ SDK scorecard round trip | ❌ SDK scorecard |
| Signed V4 POST policy form uploads | ✅ RSA signature, conditions and expiry; [scope](signed-urls.md#signed-post-policy-form-uploads) | ◐ SDK scorecard: passes in Ruby, Node.js, PHP, Python and Java; fails in Go, .NET and Rust | ❌ SDK scorecard |
| Object retention and customer-supplied keys | 🚧 Retention modes enforced on delete/overwrite; CSEK keys required to read data, which stays unencrypted on disk; [retention](slices/sdk-object-retention.md), [CSEK](slices/customer-supplied-keys.md) | ❌ SDK scorecard | ❌ SDK scorecard |
| Store/retrieve unconditional bucket IAM bindings | ✅ | ❌ | ✅ Stored only |
| Enforce predefined-role bucket IAM grants | ✅ Strict local identities | ❌ | ❌ |
| Enforce strict local bearer-token permissions on scoped JSON/XML routes | ✅ | ❌ | ❌ |
| Enforce anonymous `allUsers` versus registered-only `allAuthenticatedUsers` reads | ✅ Strict local bucket IAM | ❌ | ❌ |
| Public URL helper (encoded object address) | ✅ CLI builds encoded path-style or virtual-host URLs served by GET/HEAD; grants remain separate | ❌ `PublicURL()` returns a base origin only | ❌ |
| Upload/download bytes with V4 RSA signed GET/PUT URLs | ✅ | ✅ No signature validation | ✅ No signature validation |
| Signed-URL signature / expiry validation | ✅ V4 RSA and HMAC signatures (V2 RSA/HMAC subsets PARTIAL), final host/path/query/method/headers and injected-clock expiry/skew; exact provider errors remain PARTIAL; [scope and evidence](evidence/signed-urls.md). Since 0.14.0, permissive mode verifies them too with `signed_urls=verify`, and `signed_host_port=lenient` accepts the host without its port that Go storage signs, by default only in the enabled built-in test signer's own signatures | ❌ [Handlers](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/object.go) do not verify signing credentials; source review only | ❌ [Dispatch/handlers](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/server.py) do not verify signing credentials; source review only |
| Built-in signer whose signatures the emulator verifies, with no key to generate or register | ✅ `test-signer@gcs-mimic.invalid` with a published key: `gcs-mimic test-signer credentials` prints its service account key file and `url sign` signs with it by default; strict mode enables it when `test_signer=enabled` or the identities declare it. Needs 0.14.0 or later; [scope](signed-urls.md#built-in-test-signer) | ❌ Signatures are not verified, so no signer is needed | ❌ Signatures are not verified, so no signer is needed |
| Application servers without credentials while browsers stay anonymous | 🚧 Strict mode's trusted listener: an unpublished port where requests without credentials act as `trusted_principal`, by default the built-in `trusted-app@gcs-mimic.invalid` with `roles/storage.admin`, under the same IAM, ACL and public access prevention checks; it refuses browser requests and foreign Host names. A browser run against the recipe is pending (DESIGN-101 `PARTIAL`). Needs 0.14.0 or later; [recipe](../../examples/strict-local/README.md) | ❌ No access control: every request is allowed | ❌ No access control: every request is allowed |
| XML response Content-Type / Content-Disposition overrides | ✅ Authorized GET/HEAD, ranges and V4 query binding without metadata mutation; [scope](slices/xml-response-overrides.md) | ✅ Both response headers in [download handler](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/object.go#L1162-L1174); source review only | 🚧 Content-Disposition only in [download handler](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/objects.py#L687-L740); source review only |
| Virtual-hosted-style object endpoints | ✅ Explicit DNS suffix opt-in, GET/HEAD/PUT and strict signing; supplied matching SAN required for HTTPS; [scope](slices/xml-virtual-host.md) | ✅ Bucket-host GET/HEAD and upload routes in [router](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/server.go#L306-L343); source review only | ❌ Path-only [public routes](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/server.py#L133-L143); source review only |
| Public access prevention with preserved grants and valid signed access | ✅ Bucket enforcement and stored-grant reactivation; parent policy is LOCAL_SIMULATION; [scope](slices/public-access-prevention.md) | ❌ [Object handlers](https://github.com/fsouza/fake-gcs-server/blob/896ece51e49eed0946089f3089f6343855c0d106/fakestorage/object.go) do not enforce public grants/PAP; source review only | ❌ [Download handler](https://github.com/oittaa/gcp-storage-emulator/blob/v2026.07.19/src/gcp_storage_emulator/handlers/objects.py#L687-L740) does not enforce public grants/PAP; source review only |

Rows marked "SDK scorecard" cite the
[final 2026-10-31 eight-SDK run](../../sdk-scorecard/results/2026-10-31-2/summary.md);
a passing scorecard case shows an SDK round trip, not full API compatibility.

The optional plaintext `grpc_listen` serves the scoped Storage v2 subset in the
first gRPC row: bucket CRUD, updates, IAM and retention locking; object reads,
writes, listing, deletion and restore; resumable uploads with status queries and
cancellation, rewrite, compose, move and bidirectional ranged reads on standard
buckets. Appendable objects support creation, flush, reads while appending,
takeover and finalization over gRPC in any bucket; see the
[Rapid append contract](slices/grpc-v2-rapid-append.md) for limits. Unsupported
fields return actionable `UNIMPLEMENTED`. The same endpoint serves the reference emulator's
twelve legacy Storage v1 RPCs for migration, including multi-message uploads and
project-free bucket listing, with shared strict permissions and documented field
limits; the reference keeps only the first upload message's data. JSON, XML and
gRPC share object bytes, generations, metadata and authorization
([evidence](slices/cross-protocol-objects.md)). These rows describe workflows,
not Google GA/EXACT fidelity; individual RPC rows in the feature ledger remain
`PARTIAL`. See the [legacy contract](slices/grpc-v1-legacy.md),
[v2 gRPC contracts](slices/grpc-v2-foundation.md) and
[SDK journeys](../../sdk-tests/go/README.md) for verified subsets and limits.

An ACL value, stored IAM policy, accepted signed URL or CORS response does not
establish access control. Actual-browser execution is a separate evidence question:
gcs-mimic has [trusted-HTTPS signed-browser results](evidence/signed-browser.md);
this comparison did not run either other product in a browser.

The built-in signer, trusted listener and `cors_fallback` rows describe local
emulator extensions, not Cloud Storage behavior. The built-in signer's private
key is public, and the trusted listener grants its identity to anything that
reaches its port, so both serve only local development and tests. The
[strict local recipe](../../examples/strict-local/README.md) combines them; its
container smoke test runs the recipe with curl, not with a browser.

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
| API-created objects appear as bucket/object files | ✅ `--sync-dir` mirror beside opaque storage | ✅ Filesystem backend | ✅ Disk backend |
| Reconcile external edits with hashes and generation metadata | 🚧 Bounded live observation, create/edit ingestion and approved external deletion; complete conflict/recovery/platform acceptance pending | ❌ Direct reads can expose stale metadata | ❌ No reconciliation contract |
| Optional in-memory backend | 🚧 `--backend memory` with explicit storage budgets; [contract](slices/memory-backend.md) | ✅ | ✅ |
| Import a bucket/object seed directory | ✅ Every startup and opt-in atomic reseed; an exactly identical object keeps its generation | ✅ Startup and explicit reseed | ❌ Default bucket creation is separate |
| Create buckets with their settings (CORS, versioning, project, IAM policy) from a startup document | 🚧 A JSON document from `buckets_file`, or inline in `GCS_MIMIC_BUCKETS`; entries may set an initial `iamPolicy`, unapplied edits warn, and `buckets_file_mode=reconcile`, the default for the inline document, writes edited fields to existing buckets. Inline documents, `iamPolicy` and reconcile need 0.14.0 or later. Crash-boundary and review cases remain pending (DESIGN-062 `PARTIAL`); [contract](slices/bucket-initialization.md) | ❌ No startup bucket document; seed directories create buckets without settings | ❌ No startup bucket document |
| Published native binaries | ✅ macOS, Windows and Linux amd64+arm64 | ✅ macOS/Linux amd64+arm64; Windows amd64 | ❌ Python runtime required |
| Published Docker image | ✅ GHCR and Docker Hub linux/amd64+arm64 | ✅ Docker Hub | ✅ Docker Hub |
| Project license | MIT OR Apache-2.0 | BSD-2-Clause | BSD-3-Clause |

gcs-mimic applies its seed directory before readiness on
every startup, and the image reads `/data` without extra settings. As in
fake-gcs-server, an object that differs from its seed file in bytes or metadata,
including API metadata edits, becomes a new live generation. Only an object
exactly matching what seeding would write keeps its generation, where
fake-gcs-server writes a new one; fake-gcs-server also offers
`/_internal/reseed`, while gcs-mimic offers an opt-in atomic reseed through its
local administration route ([contract](slices/atomic-seed-reseed.md)); neither
has a seed watcher. Neither product deletes
objects whose seed file was removed.

gcs-mimic's sync directory is a mirror beside its catalog, not a second storage
backend: API writes appear shortly after each response, and external creates and
edits become new generations with recomputed hashes. Bounded live observation,
approved external deletion and the live `sync` CLI are available; complete
conflict, recovery and platform acceptance remains pending
([sync contract](slices/directory-synchronization.md)).
The follow-up ran the container smoke's sync stage on `linux/arm64` and
`linux/amd64`; see [sync directory evidence](evidence/sync-dir-containers.txt)
and [Docker usage](container-usage.md#mirror-objects-to-a-host-directory).
Disk persistence does not imply equivalent crash or power-loss guarantees.
CPU, memory, throughput and image size are not ranked across products in general. One equivalent
comparison with fake-gcs-server, limited to object reads at local scale, is recorded in the
[read path measurement](evidence/read-path-performance.md).

gcs-mimic stores metadata in SQLite and object bytes in opaque immutable files.
The storage investigation in `storage-backends.md` compares filesystem,
xattr/JSON, memory, SQLite BLOB and embedded key/value approaches. Transactional
metadata plus streamed opaque files is the recommended durable default; this is
a requirement-based decision, with no comparative benchmark claim.

The product target is a complete behavioral superset of the pinned fake-gcs-server
baseline, with documented native configuration mappings and explicit data conversion.
Matching flag names or environment-variable suffixes is not required.
Since 0.7.0, legacy flags and `FAKE_GCS_*` variables with a native equivalent
are translated with a warning, and since 0.8.0 the event settings map too; new
configuration should use native names. The migration contract in
`fake-gcs-server-migration.md` includes flags, TLS, endpoints, Go
embedding, Storage v1 gRPC, helper/event behavior and both source backends.
Lossless import and the remaining configuration roles are still planned. The existing
`legacy-emulator` profile covers a limited bucket contract. Existing comparison marks describe current behavior.
