# Frozen basic JSON local profile

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

Contract checked on **2026-10-23** against Google Cloud Storage JSON API v1.
This freezes completion conditions 1 and 4 of `basic-json-contract`, delivery
order 8. It is a contract and promotion proposal, not new execution evidence.
Only [the feature ledger](../../../compatibility/features.json) owns status.
Condition 3 needs the final integrated verification; condition 2 has
[recorded fields/ETag evidence](../evidence/basic-json-fields-etag.md).

## Profile and precedence of contracts

The baseline is `compat-profile=native`, flat-namespace live buckets and
finalized live objects, with disk blobs and the transactional local catalog.
Select `auth-mode=strict` to enforce registered local bearer principals and
project-plus-bucket unconditional predefined-role grants. The default
`permissive` mode bypasses authorization and never proves permission parity.
Strict local credentials do not reproduce Google identity issuance.

The `legacy-emulator` profile remains a separately tested compatibility option
under the [bucket core contract](bucket-core-parity.md). Its naming, creation
defaults, rendering and deletion responses are local differences, not Google
EXACT behavior. No existing profile or history/hold capability is removed.
The basic baseline uses live selections; retained-generation and event-hold
interactions remain governed by [versioning/holds](bucket-versioning-holds.md).

The tables below assemble the current contract. They supersede stale statements
in earlier slices that fields, read ETags, resumable uploads, strict grants or
retained generations are unavailable. Detailed validation, caps and acceptance
cases stay in the linked slices. This document does not expand their claims.
Copy, rewrite and compose are explicitly later **orders 9, 10 and 11**.

## Official sources re-fetched for this freeze

Every source in this section was fetched and checked on **2026-10-23**.
Documentation dates are check dates, not claimed upstream publication dates.

| Source | Contract authority |
| --- | --- |
| [Request preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions) | Comparison meaning, generation-match zero, match 412 and not-match 304; no inferred mixed-error order |
| [JSON status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes) | Error document structure, documented reasons, empty statuses and the media redirect question |
| [objects.insert](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/insert), [get](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/get), [delete](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/delete) | Upload/get/delete parameters, permissions, metadata versus media and condition comparisons |
| [objects.patch](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/patch), [update](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/update), [list](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/list) | Mutable metadata, replacement/partial update, listing filters, projection and paging |
| [buckets.insert](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/insert), [get](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/get), [list](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/list), [delete](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/delete) | Live bucket CRUD, project context, empty deletion, metageneration and paging |
| [buckets.patch](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/patch), [update](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/update) | Bucket metadata verbs; do not infer each omitted/null default from replacement intent |
| [Common parameters/headers](https://docs.cloud.google.com/storage/docs/json_api/v1/parameters) | fields, read validators, MIME, ranges, stored-content headers and upload headers |
| [Data validation](https://docs.cloud.google.com/storage/docs/data-validation) | Full-object MD5/CRC32C, media/final-resumable X-Goog-Hash, multipart checksum containers and mismatch rejection |
| [Request endpoints](https://docs.cloud.google.com/storage/docs/request-endpoints) | Upload/download prefixes and encoded names; configured local HTTP is not Google's HTTPS-only service |
| [Object resource](https://docs.cloud.google.com/storage/docs/json_api/v1/objects), [metadata](https://docs.cloud.google.com/storage/docs/metadata), [XML headers](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers) | Resource/hash meanings, MIME fallback and protocol-specific header distinctions |

[Discovery storage v1](https://storage.googleapis.com/$discovery/rest?version=v1)
was fetched as raw bytes: revision `20261013`, SHA-256
`1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`.
The latest file-changing commit returned for
[`google/storage/v2/storage.proto`](https://github.com/googleapis/googleapis/blob/d73dfb3c3be52245625b951b9c0bcf65163c549f/google/storage/v2/storage.proto)
was `d73dfb3c3be52245625b951b9c0bcf65163c549f`; fetched file SHA-256
`a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf`.
Both match the earlier source pins. Proto generation conditions, UpdateObject
and WriteObject are cross-checks; RPC statuses, update masks and transport
defaults cannot settle JSON status conflicts. Discovery has download support
flags but no `mediaDownload.path`; its resumable path discrepancy remains
SPEC-RSM-DISCOVERY-PATH. Schema presence alone establishes no launch stage.

## Shared request and response rules

Routes are relative to configured `api_base_url`. Split raw paths before
exactly-once UTF-8 percent decoding; encode embedded object slashes as `%2F`.
Path `+` is literal, query `+` is space. Duplicate decoded query keys, malformed
escapes, invalid scalar syntax and method-inappropriate parameters fail 400.
The raw query cap is 16 KiB. These local parser/error limits are not a claim
about undocumented provider limits.

| Shared input | Frozen supported subset and boundary |
| --- | --- |
| Authorization | Optional in permissive mode; strict Bearer registry and exact route permission below; project and matching bucket grants combine and mutation transactions recheck grants/owning project |
| `alt`, `prettyPrint` | JSON is default; `alt=media` only on object GET; compact output and `prettyPrint=false`; true pretty printing is 501, invalid values 400; compact default differs from Discovery |
| `fields` | Supported JSON resource/list responses per [fields contract](json-response-fields.md); DELETE ignores it; media combination and named selector edges are explicit 501 under SPEC-FIELDS-EDGES; session capability URLs cannot override the frozen selector |
| `projection` | `noAcl`/`full` accepted on the methods below; PUT defaults full, other basic methods noAcl locally; both render the same supported fields, with no ACL/owner; full projection's additional permissions/fields remain pending |
| Common unsupported queries | `userProject`, `key`, `oauth_token`, `quotaUser`, `userIp`, `callback` are 501 where recognized; no requester-pays, API-key/query-token, quota or JSONP semantics; method-inappropriate inputs remain 400 |
| JSON/framing headers | JSON bodies use Content-Type and bounded Content-Length/chunked framing; body cap 64 KiB; object streams obey max_object_bytes, inactivity/absolute/catalog deadlines and existing admission limits |
| Read HTTP validators | Bucket GET and object metadata/media/range GET support If-Match/If-None-Match under [read ETags](json-read-etag-validators.md); fields/range apply after the selected-record checks; local mixed-condition order remains SPEC-008/SPEC-JSON-ETAG-WIRE |
| Other validators | Write/list ETag conditions and supported-GET If-Range or standalone date validators are 501; RFC date precedence and unrelated-route behavior remain exactly as recorded in the read-ETag contract, without a blanket date-header rejection claim |
| Other semantic headers | Object encryption, copy-source encryption, X-Goog-User-Project and X-HTTP-Method-Override are explicitly unsupported; X-Goog-Meta-* is supported only in the resumable finalization subset; exact per-route handling remains in the linked contracts |
| Invocation tokens | One nonempty X-Goog-Gcs-Idempotency-Token; media/multipart require ifGenerationMatch, including zero; resumable initiation accepts it without replay; other accepted calls re-execute, not deduplicate; SPEC-013 |

Bucket resources expose `kind`, `id`, `name`, `selfLink`, `projectNumber`,
`metageneration`, `timeCreated`, `updated`, `etag`, `location`, `locationType`,
`storageClass`, nonempty `labels`, enabled `versioning.enabled` and true
`defaultEventBasedHold`. Native false settings may be omitted; profile rendering
differences are retained. ProjectNumber is local resolution, not provider identity.

Object resources expose `kind`, `id`, `selfLink`, `mediaLink`, `name`, `bucket`,
`generation`, `metageneration`, `storageClass`, `size`, `md5Hash`, `crc32c`,
`etag`, `timeCreated`, `updated`, `timeFinalized`, `timeStorageClassUpdated`,
present fixed content fields and nonempty custom `metadata`. Existing true
`eventBasedHold` and historical `timeDeleted` follow the separate hold/history
contract. Decimal int64/uint64 response fields are strings. Optional absent
fields stay absent after fields selection. IDs, ETags, URL spelling and timestamp
precision are local choices under SPEC-012 and the bucket provider gaps.

Successful media uses stored Content-Type or application/octet-stream,
Content-Length, Accept-Ranges, ETag, x-goog-generation/metageneration/hash,
x-goog-stored-content-length/encoding and present content encoding/disposition/
language. A 206 adds Content-Range and uses the selected byte length; hashes
remain full-object hashes. JSON nonpublic Cache-Control is no-cache; a supported
allUsers public grant selects stored directives or public, max-age=3600.
allAuthenticatedUsers alone is not public. Complete provider header presence,
opaque bytes and optional Last-Modified parity remain SPEC-012; no JSON
Last-Modified header is promised. JSON errors remain unfiltered by fields.

| Error / control response | Frozen local selection; provider parity boundary |
| --- | --- |
| 400 | invalidParameter for query/header/framing validation; invalidArgument for resource types/values; required for absent required input; parseError for malformed JSON; invalidAltValue for unknown alt; notDownload for a non-media direct-download request; invalid for checksum mismatch. Exact reason/order at parser/hash edges remains open |
| 401 / 403 | required for missing strict credentials where anonymous grants do not authorize, authError for invalid/unsupported credentials, forbidden for insufficient grants; SPEC-017 retains message/header/missing-resource selection questions |
| 404 / 405 / 409 | notFound for authorized missing resources; methodNotAllowed and route-specific Allow for wrong methods; conflict for existing bucket/nonempty bucket deletion; exact message/routing precedence is local |
| 412 / 413 / 416 | conditionNotMet for failed mutation comparisons or read Match; uploadTooLarge for configured object/JSON caps; requestedRangeNotSatisfiable for unsatisfiable single ranges; local caps and ambiguous precedence are not provider claims |
| 501 / 503 | notImplemented for recognized unsupported semantics; backendError for catalog/admission/deadline/storage unavailability; explicit emulator behavior, not implementation of the rejected feature |
| Empty/control bodies | Native successful DELETE 204, conditional GET 304; resumable initiation 200, progress/status 308, cancelled 499 and unavailable session 410/404 follow the resumable contract; transport disconnect/absolute transfer expiry may close/truncate instead of producing an error document |

Strict missing-bucket 403/404 selection and anonymous handling remain the local
SPEC-017 rules. Post-EOF timeout can leave the mutation outcome unknown because
publication may finish; reread state or retry with conditions. No error-envelope
claim promises a response after a failed connection.

## Frozen bucket methods

All bucket methods below use `/storage/v1/b` or `/storage/v1/b/{bucket}`.
GET/list/DELETE require empty bodies. Content-Type/framing, Authorization,
fields, common query rules and response resources are defined above.

| Method / ledger row | Supported parameters and body | Conditions / permission / success and errors | Detailed contract |
| --- | --- | --- | --- |
| insert / JSON-BUCKETS-005 | Required project; projection; JSON name, location (US/EU/ASIA), supported storageClass, labels, versioning.enabled and defaultEventBasedHold; exact empty lifecycle.rule accepted without rules | storage.buckets.create on project; 200 Bucket; invalid names/types/labels 400, duplicate name 409; native requires project and uses declared creation defaults | [CRUD](json-bucket-crud.md), [core/profile](bucket-core-parity.md) |
| get / JSON-BUCKETS-002 | Bucket path, projection, softDeleted=false, fields; read ETag headers | ifMetagenerationMatch/NotMatch against returned record; storage.buckets.get; 200 Bucket, match failure 412, read not-match 304, missing 404 | [CRUD](json-bucket-crud.md), [read ETags](json-read-etag-validators.md) |
| list / JSON-BUCKETS-006 | Required project; prefix, maxResults, pageToken, projection, softDeleted=false, returnPartialSuccess=false, fields | storage.buckets.list on project; 200 kind/items/nextPageToken; binary keyset order; query-bound token; default/cap 1000 noAcl or 200 full; zero uses default locally; invalid token/query 400 | [CRUD paging](json-bucket-crud.md) |
| delete / JSON-BUCKETS-001 | Bucket path; fields ignored; no projection | Atomic ifMetagenerationMatch/NotMatch; storage.buckets.delete; native empty 204, missing 404, any retained live/history object 409; failed mutation conditions 412 locally | [core/profile](bucket-core-parity.md), [media races](json-object-media.md) |
| patch / JSON-BUCKETS-008 | projection, fields; JSON labels key merge/null-key removal, storageClass, versioning.enabled, defaultEventBasedHold; preserve omitted settings | Atomic metageneration conditions; storage.buckets.update; 200 Bucket; invalid input 400, stale comparison 412, unsupported setting 501, missing 404; body-validation/error order is local | [PATCH](bucket-metadata-patch.md) |
| update / JSON-BUCKETS-013 | projection, fields; replace supported labels/default class/versioning/default hold; accept unchanged rendered location; omitted labels clear, class STANDARD, booleans false locally | Same atomic conditions/permission; 200 Bucket; invalid input 400, stale 412, unsupported 501, missing 404; omitted acl accepted and acl:[] ignored locally, without granting permissions | [PUT](bucket-metadata-put.md) |

Bucket PATCH labels:{} preserves keys, labels:null clears under the named local
interpretation. PUT labels omitted/null clears and per-key null fails. Both
verbs reject null/empty class; native null booleans fail, legacy null booleans
reset false. Both accept only exact empty lifecycle.rule without implementing
lifecycle; unchanged location is compared case-insensitively without relocation.
Unknown/output-only input handling, no-effect revision increments and omitted
PUT defaults remain SPEC-BUCKET-PATCH/PUT. Classes are STANDARD, NEARLINE,
COLDLINE, ARCHIVE, MULTI_REGIONAL and DURABLE_REDUCED_AVAILABILITY; REGIONAL/RAPID
are 501. Default class updates do not rewrite existing objects.

## Frozen object methods

Object names and metadata share [media](json-object-media.md),
[metadata](json-object-metadata.md) and [strict auth](strict-auth.md) validation.
The four query conditions are ifGenerationMatch, ifGenerationNotMatch,
ifMetagenerationMatch and ifMetagenerationNotMatch. They do not select a version.
Non-upload methods with generation select first and compare that same record.

| Method / ledger row | Supported parameters, headers and body | Conditions / permission / success and errors | Detailed contract |
| --- | --- | --- | --- |
| insert media / JSON-OBJECTS-007 | POST /upload/storage/v1/b/{bucket}/o; uploadType=media, required name, projection, fields, four conditions; Content-Type, Content-Length or chunked, X-Goog-Hash; raw zero/nonzero bytes | storage.objects.create, plus storage.objects.delete for overwrite; atomic publication and condition recheck after EOF/hash validation; 200 Object; missing bucket 404, failed conditions 412 locally, checksum mismatch 400, size 413, unsupported 501 | [Media](json-object-media.md) |
| insert multipart / JSON-OBJECTS-007 | Same POST, uploadType=multipart; optional query name overriding metadata name; multipart/related boundary, metadata JSON then binary media and optional checksum JSON; name, five fixed content fields, metadata, md5Hash/crc32c | Same permissions/conditions; bounded full-container validation before 200 Object; malformed/truncated MIME/JSON/checksums 400, caps 413, unsupported 501; failed overwrite preserves complete old resource | [Multipart](json-object-multipart.md) |
| insert JSON-resumable / JSON-OBJECTS-007 | Same POST, uploadType=resumable; name from query/metadata, projection, fields, four conditions, contentEncoding; empty/JSON metadata; X-Upload-Content-Type/Length, Origin; subsequent capability PUT or Go POST uses Content-Range/Length, final X-Goog-Hash and X-Goog-Meta-* | Initiation permission and final transaction recheck of initiator grants/overwrite conditions; initial 200 empty with absolute Location and no-store; chunk/status 308 and Range where committed, final 200 Object; cancellation 499 empty, expired/unknown 410/404; parser/checksum/size errors per session contract | [Resumable](resumable-upload.md) |
| get metadata / JSON-OBJECTS-005 | GET /storage/v1/b/{bucket}/o/{object}; alt=json/default, projection, fields, generation, softDeleted=false, four conditions, read ETag headers; no body | storage.objects.get; 200 Object, missing 404, failed match 412, false not-match empty 304; provider mixed-error order remains open | [Media](json-object-media.md), [read ETags](json-read-etag-validators.md) |
| get media/range / JSON-OBJECTS-005 | Same GET or /download/storage/v1/b/{bucket}/o/{object}; alt=media; generation, four conditions, projection subset; Range closed/open/suffix, Accept-Encoding, read ETag headers; no fields/body | Same permission and pinned selected bytes; direct 200 locally on both prefixes, valid nonempty single range 206, unsatisfiable 416 requestedRangeNotSatisfiable; conditions precede Range; malformed/multiple/empty range fallback is local; transcoding-required request 501 | [Range](json-object-range.md), [public cache](public-object-reads.md) |
| list / JSON-OBJECTS-008 | GET /storage/v1/b/{bucket}/o; prefix, delimiter, startOffset inclusive/endOffset exclusive, maxResults/pageToken, includeTrailingDelimiter, projection, fields; default/false versions, softDeleted and includeFoldersAsPrefixes; empty matchGlob/filter means no filter; no body | storage.objects.list; 200 kind/items/prefixes/nextPageToken with absent empty arrays; combined item/prefix cap 1000, zero means default locally; token binds query excluding page size/fields, no snapshot; missing bucket 404, invalid token 400, unsupported filters 501 | [Listing](json-object-list.md), [history extension](bucket-versioning-holds.md) |
| delete / JSON-OBJECTS-004 | DELETE object path; generation and four conditions; fields ignored; no body | storage.objects.delete; atomic selected-record checks and removal; 204 empty, missing 404, failed conditions 412 locally; holds/retention boundaries stay explicit | [Media](json-object-media.md), [versioning/holds](bucket-versioning-holds.md) |
| patch / JSON-OBJECTS-010 | PATCH object path; generation, four conditions, projection, fields; JSON contentType, contentEncoding, contentDisposition, contentLanguage, cacheControl and metadata; no media body | storage.objects.update; merge supplied fixed/custom values; null removes field/map/key; one revision and unchanged generation/bytes/hashes; 200 Object, invalid 400, stale 412 locally, unsupported 501, missing 404 | [Metadata](json-object-metadata.md) |
| update / JSON-OBJECTS-015 | PUT same path/queries and supported metadata; full default projection | Same permission/atomic checks; replace supported set, reset omitted fixed/custom fields; missing content type serves octet-stream; 200 Object and same failures; changed output-only/unknown input stays explicitly unsupported | [Metadata](json-object-metadata.md) |

The final custom metadata map is bounded to 8192 UTF-8 key/value bytes. Empty
custom values survive; empty keys/non-ASCII fixed header values return 501.
Metadata changes preserve immutable data identity and use the injected clock.
Unchanged server-managed metadata may be round-tripped inside the transaction;
changed/unknown/unmodeled output input returns 501 under SPEC-015. All metadata
empty/null/string distinctions remain in that contract, including contentType
empty PATCH no-op versus null clearing. Resumable validation is stricter about
condition signs than the original media parser, as its contract specifies.

## Source reconciliation without inferred answers

The check date for **every entry is 2026-10-23**. Classification applies to the
specific statement, not the whole gap. All four gaps remain OPEN.

| Gap / official statement | Frozen local behavior | Classification and remaining answer |
| --- | --- | --- |
| SPEC-008: [preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions) assigns not-match failure a “304 Not Modified”; [status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes) says “At least one of the pre-conditions” under 412 conditionNotMet | Single read not-match failure is empty 304; single match failure is 412; mutations use 412 for not-match too | Read/single-match comparison: **documented-and-matching**. Mutation not-match 412: **local deviation (kept, named)**. Which JSON mutation status controls: **source question still open** |
| SPEC-008: [insert](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/insert), [get](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/get), [delete](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/delete), PATCH/PUT and Discovery describe comparisons, not exhaustive ordering | Missing selected resource wins 404; failed Match tier wins before NotMatch; mixed HTTP/query reads share one snapshot. Upload header/known-size validation, admission, advisory check, streamed-body/checksum validation and publication recheck retain existing order; strict overwrite permission timing is unchanged | **Local deviation (kept, named)** for ordering; **source question still open**, including combined failures, missing resources and authorization. RPC comparisons do not settle JSON precedence |
| SPEC-009: [status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes) specifies mediaDownloadRedirect with path “prefixed by `/download`”; [get](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/get) describes alt=media at ordinary storage/v1, and [endpoints](https://docs.cloud.google.com/storage/docs/request-endpoints) says “Optionally, for JSON API object downloads” | Both prefixes return direct 200/206 without redirect; direct prefix requires alt=media; configured external endpoint constructs links | Direct-prefix media support: **documented-and-matching**. Ordinary-prefix direct success: **local deviation (kept, named)**. Reconciliation of 303 wording: **source question still open**; download flags do not invent a Discovery path |
| SPEC-011: [common headers](https://docs.cloud.google.com/storage/docs/json_api/v1/parameters) says “required for POST and PUT commands”; [insert](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/insert) lists Content-Type among generally required headers; [Object](https://docs.cloud.google.com/storage/docs/json_api/v1/objects) specifies default serving MIME | Missing raw-media request Content-Type is accepted and stores application/octet-stream; multipart/resumable MIME sources follow their own contracts | Serving fallback: **documented-and-matching**. Raw-media missing-header acceptance: **local deviation (kept, named)**. Whether Google accepts that request: **source question still open**; serving fallback is not upload acceptance evidence |
| SPEC-012: [Object](https://docs.cloud.google.com/storage/docs/json_api/v1/objects) defines id content, ETag, links, hashes and timeFinalized; [common headers](https://docs.cloud.google.com/storage/docs/json_api/v1/parameters) defines stored encoding/length; [validation](https://docs.cloud.google.com/storage/docs/data-validation) covers full-object checksums | Metadata exposes declared resource subset; full/partial media headers use the same record; range hashes cover whole bytes; timeFinalized equals initial finalization/creation time | Documented field/hash meanings: **documented-and-matching**. Exact local ID/ETag/link spelling: **local deviation (kept, named)**. Exhaustive 200/206/304 header presence and provider opaque bytes: **source question still open** |
| SPEC-012: [XML headers](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers) additionally defines generation/metageneration/hash; [metadata](https://docs.cloud.google.com/storage/docs/metadata) qualifies XML MD5 ETags | JSON emits tested generation/hash headers without claiming exhaustive parity; XML GET/HEAD keeps its independently documented selected-record/cache/custom-header behavior and local quoted hexadecimal MD5 ETag | Protocol distinction: **documented-and-matching**. Exact XML ETag/private-cache/custom-header representation: **local deviation (kept, named)**; remaining XML wire questions stay SPEC-XML-INPUTS, not inferred JSON authority |

## Gap map and preservation of the broad rows

The following is the complete routing rule for remaining basic-profile inputs,
outputs and errors: a supported case links to existing evidence below; every
other checked schema field/header/error goes to the named PENDING case or gap.
An explicit 501 proves fail-closed behavior, not support for that Google feature.
Preserve **all** existing acceptance cases, test IDs, verifications, SDK evidence
and known_deviations on every broad method row. Nothing in this proposal deletes
or changes a historical result. Older pending wording may describe a capability
now delivered; retain its unresolved remainder and its passing extension cases.

| Surface / complete remaining family | Existing verified anchor | Explicit pending destination / gap |
| --- | --- | --- |
| Bucket creation, name/project/location/default rendering and profile differences | BKT-GET-001, BUCKET-PROFILE-RESOURCE/NAMES/DELETE, PROFILE-NAME-AND-LIST-BOUNDS; [core evidence](../evidence/bucket-core-parity.md) | JSON-BUCKETS-005-PENDING for domain/project/organization policies; BEHAVIOR-COMMON-001-PENDING and BEHAVIOR-COMMON-014-PENDING for naming and real project/billing; bucket default/provider differences remain the core contract's named local scope |
| Bucket resource fields outside the emitted set: acl, autoclass, billing, cors, customPlacementConfig, defaultObjectAcl, encryption, hierarchicalNamespace, iamConfiguration, ipFilter, lifecycle, logging, objectRetention, retentionPolicy, rpo, satisfiesPZI/PZS, softDeletePolicy, website; generation/softDeleteTime/hardDeleteTime/owner | PATCH/PUT unsupported-input and resource-preservation tests; [PATCH](../evidence/bucket-metadata-patch.md), [PUT](../evidence/bucket-metadata-put.md) | PATCH-ADVANCED-FIELDS / PUT-ADVANCED-FIELDS; JSON-BUCKETS-001/002/005/006-PENDING for delete/get/insert/list consequences; CORS later orders 13/14; no silently accepted effect beyond exact empty lifecycle/acl exceptions |
| Bucket labels null/empty behavior, no-effect revisions, unknown/output-only inputs, immutable location, omission defaults, acl required/empty wording, timestamps/etag and propagation | PATCH-LABELS/ROUND-TRIP, PUT-REPLACEMENT/ROUND-TRIP; per-test tables in frozen PATCH/PUT contracts | PATCH-PROVIDER-PARITY / PUT-PROVIDER-PARITY; SPEC-BUCKET-PATCH / SPEC-BUCKET-PUT; conditions' ambiguous errors additionally SPEC-008 |
| ACL/default ACL and full projection, predefinedAcl/predefinedDefaultObjectAcl, extra IAM/ipFilter permissions | BUCKET-POLICY-GRANTS and strict method cases verify supported grants only | PATCH-ACL-BILLING-PROJECTION / PUT-ACL-BILLING-PROJECTION; JSON bucket/object method PENDING cases, BEHAVIOR-COMMON-005-PENDING; SPEC-017 / SPEC-BUCKET-PUT; ignored acl:[] grants nothing |
| Object output/input fields outside supported set: acl, owner, componentCount, contexts, customTime, customerEncryption, kmsKeyName, temporaryHold, retention, retentionExpirationTime, restoreToken, softDeleteTime/hardDeleteTime; storageClass mutation and cross-transport output behavior | JSON-OBJECTS-010/015-METADATA and METADATA-BOUNDARIES; [metadata evidence](../evidence/object-metadata.md) | JSON-OBJECTS-007/010/015-PENDING and JSON-OBJECTS-005-PENDING for read consequences; SPEC-015; componentCount belongs to order 11; class mutation to rewrite order 10; full IAM/ACL, retention, soft delete, contexts, encryption and custom time remain named deferred scopes |
| Version selection, event holds and versioning settings; remaining temporary holds, selected-delete conditions and propagation | VERSIONING-AND-EVENT-HOLD, BUCKET-HISTORY-NONEMPTY and [history/hold evidence](../evidence/bucket-core-parity.md) | JSON-OBJECTS-004/005/010/015-PENDING, PROVIDER-AND-SETTINGS-UPDATES, TEMPORARY-RETENTION-AND-WIRE; SPEC-VERSION-DELETE-CONDITIONS / SPEC-EVENT-HOLD-WIRE; preserve delivered history rather than reclassifying it absent |
| Upload body/framing, EOF, checksum failure preservation and bounded staging; raw missing MIME, MIME conflicts/caps, optional checksum JSON edge rules | BEHAVIOR-OBJECT-005-MEDIA/MULTIPART-CHECKSUMS, [media](../evidence/object-media.md), [multipart](../evidence/multipart.md), RSM-FINAL-003 | JSON-OBJECTS-007-PENDING; SPEC-010/011/016 and SPEC-RSM-EDGES for Content-MD5, algorithm/encoding/duplicate/conflict/unknown-key/error reasons and MIME edges; XML multipart/gRPC/composites are separate scopes |
| Single documented conditions versus mutation not-match, combined/selected-missing/permission/body/checksum/admission precedence | BKT-DEL-002, PATCH-CONDITIONS, PUT-CONDITIONS, BEHAVIOR-COMMON-011-MEDIA/METADATA, RSM-FINAL-004/005; tests below | Broad method PENDING on JSON-BUCKETS-001/002/008/013 and JSON-OBJECTS-004/005/007/010/015; SPEC-008, SPEC-017, SPEC-RSM-AUTH-CONDITIONS and SPEC-VERSION-DELETE-CONDITIONS; no local 412 rule removed |
| Object and bucket list paging, zero/default/clamp, marker item+prefix accounting, delimiter wording, empty arrays, live mutation consistency and local token format | OBJECT-LIST cases in [listing evidence](../evidence/object-list-metadata.md); bucket BKT-LST-001/002/003; FIELDS-009 | OBJECT-LIST-PENDING, JSON-BUCKETS-006-PENDING; SPEC-LIST-PAGING / SPEC-LIST-RESOURCE-SHAPE; corruption checks are not signatures or snapshots; remaining-resource paging stays BEHAVIOR-COMMON-013-PENDING |
| versions/softDeleted/includeFoldersAsPrefixes/returnPartialSuccess, nonempty matchGlob/filter, bucket generation and restoreToken queries | False/default rejections and retained-version extension cases in list/core evidence | OBJECT-LIST-PENDING and bucket/object method PENDING; soft-delete/folders/HNS/glob/context/regional failures remain explicitly deferred; preserve versions=true support outside live baseline |
| fields, read ETags, write/list/date validators, If-Range and opaque tag/incarnation collisions | FIELDS-001..016/018, FIELDS-INPUT-ORDER/ROUTE-REJECTIONS/RENDERER-SCHEMA, ETAG-READ cases; [final evidence](../evidence/basic-json-fields-etag.md) | FIELDS-017, ETAG-READ-PROVIDER-WIRE/COLLECTION-CONTRACT; SPEC-FIELDS-EDGES / SPEC-JSON-ETAG-WIRE / SPEC-JSON-ETAG-COLLECTIONS / SPEC-RANGE-VALIDATORS; malformed/local cap/empty-parent/session edges remain explicit |
| Range fallback/empty suffix, gzip negotiation/transcoding, custom response headers, redirect, IDs/URLs/ETags/timeFinalized and exhaustive metadata/media/partial/304 response headers | [Range](../evidence/object-range.md), ETAG-READ cases and [public reads](../evidence/public-object-reads.md); OBJ-MEDIA resource/header cases | JSON-OBJECTS-005-PENDING, BEHAVIOR-OBJECT-006/007/015-PENDING; SPEC-009/012/015, SPEC-RANGE-FALLBACK / SPEC-JSON-ETAG-WIRE; XML details remain SPEC-XML-INPUTS; arbitrary header presence/encoding is never inferred from SDK byte reads |
| Session query/header errors, replay, total/expiry/cancellation edges, capability authority and advertised Location/path aliases | RSM-INIT/CHUNK/STATUS/CANCEL/FINAL cases and [final resumable evidence](../evidence/resumable-upload.md) | JSON-OBJECTS-007-PENDING; SPEC-RSM-EDGES / SPEC-RSM-AUTH-CONDITIONS / SPEC-RSM-LOCATION / SPEC-RSM-DISCOVERY-PATH; fixed configured endpoint remains local; session completion replay is distinct from SPEC-013 invocation-token replay |
| Authentication, missing-resource visibility, 401 authError detail, 403 text, WWW-Authenticate, full projection and strict overwrite check ordering | Method STRICT-ROLES, BUCKET-POLICY-GRANTS, [IAM](../evidence/bucket-iam.md) / [strict auth](../evidence/strict-auth.md) / public-read cases | Each broad method PENDING; SPEC-017; policy normalization SPEC-IAM-WIRE; conditional/custom/basic/object IAM, ACL/UBLA/PAP and real credentials remain deferred |
| Common queries/headers not supported: billing/encryption/method override, API-key/query-token/quota/JSONP, retry tokens and pretty defaults | Query/header boundary cases, FIELDS-ROUTE-REJECTIONS, ETAG-READ-REJECTIONS; upload-token tests | Corresponding broad method PENDING plus SPEC-013/015/017; malformed token HTTP coverage on list/PATCH remains SPEC-013; no response replay, encryption or billing claimed |
| Errors outside the documented envelope/status subset: malformed parser/MIME/hash reasons, unknown-input policy, 405 Allow text, local 413 limits, 501 mappings, exact messages/locations/header sets and all precedence combinations | BKT-ERR-001, BEHAVIOR-COMMON-007-MEDIA/PUBLICATION/MEDIA-SAFETY/ADMISSION and fields error-preservation cases | Corresponding bucket/object method PENDING and SPEC-008/010/012/015/016/017, SPEC-BUCKET-PATCH/PUT, SPEC-RSM-EDGES, SPEC-FIELDS-EDGES; HTTP/XML/gRPC/batch errors beyond this JSON profile remain deferred |
| Publication/timeout outcomes, deferred reclaim, restart/recovery, resource/physical limits and infrastructure effects | Existing publication, race and resumable recovery evidence; named cases retained below | Existing DESIGN-070/073/074/075/082 and BEHAVIOR-COMMON-009 scope; post-EOF timeout may commit, reread/retry with conditions; fsync/power-loss and comparative RSS/cgroup/CPU claims remain outside this freeze |

## Promotion proposal and acceptance gates

The [implementation gate](../compatibility-policy.md#feature-implementation-gate)
requires every declared case to pass, EXACT actual/target fidelity, complete
verification links and **no deviations** on an IMPLEMENTED row. A documented-only
scope can qualify only after excluded cases and deviations have explicit homes
on the still-PARTIAL broad methods. A local passing test does not establish the
provider contract. This document changes no ledger result or fidelity.

### BEHAVIOR-COMMON-011: promotable after verification and relocation

Exact declared scope sentence:

> For authorized valid JSON v1 requests in the basic live-resource profile,
> generation/metageneration match comparisons, read not-match comparisons and
> generation-match-zero object creation use the documented selected-resource
> semantics and statuses, with mutation comparisons atomic with publication.

Mutation not-match **failure status**, mixed-condition/error precedence,
missing-resource precedence, parser sign/overflow choices, token handling,
permission timing and historical/XML/gRPC conditions are excluded. Successful
not-match comparisons need not change; their failure fidelity stays SPEC-008.
Move those assertions to the corresponding method PENDING cases in the gap map,
with SPEC-008/013/017, SPEC-VERSION-DELETE-CONDITIONS and
SPEC-RSM-AUTH-CONDITIONS. Read HTTP ETags stay BEHAVIOR-COMMON-012; provider
opaque-wire questions stay on broad GET rows. Target remains EXACT.

| Acceptance case | Required observable behavior | Existing ledger test IDs / needed additions |
| --- | --- | --- |
| BASIC-CONDITION-READ | Each single generation/metageneration Match and NotMatch returns 200 on pass, 412 or bodyless 304 on failure, using the returned record | `obj_http_all_preconditions_read_304_mutation_412_and_missing_precedence`; `bkt_http_crud_metadata_conditions_and_errors`; local precedence assertions in those tests are excluded from the promoted claim |
| BASIC-CONDITION-MUTATION | Existing valid resources: independently exercise generation/metageneration Match success/failure on media/multipart, metadata PATCH/PUT and DELETE; bucket metageneration Match on PATCH/PUT/DELETE; failure preserves entire resource and bytes | `all_four_preconditions_apply_to_reads_uploads_and_deletes`; `all_conditions_generation_selectors_and_conditioned_races`; `delete_preconditions_are_atomic_and_preserve_failed_records`; `bucket_patch_conditions_and_query_contract`; `bucket_put_conditions_and_query_contract`; **NEW** explicit HTTP matrix of each method's individual success/failure and preservation; existing combined/storage cases do not replace it |
| BASIC-CONDITION-ATOMIC | Generation-match zero creates only when no live object exists; two competitors have one winner; a condition changed during streaming cannot publish stale data | `obj_http_concurrent_create_if_absent_has_one_winner`; `obj_multipart_http_concurrent_create_if_absent_has_one_winner_and_no_leaks`; `conditional_overwrite_delete_and_bucket_delete_upload_races_are_atomic`; `bucket_patch_conditioned_race_has_one_winner`; `bucket_put_conditioned_race_has_one_winner` |
| BASIC-CONDITION-RESUMABLE | Persist supported match conditions through initiation/chunks and reject changed final state atomically; no relaxed chunk-query conditions | `all_four_initiation_conditions_are_rechecked_atomically_and_cannot_be_relaxed`; `two_create_if_absent_sessions_have_one_finalization_winner`; `TestObjectResumableConditionsAtFinalization`; `TestObjectResumableMetagenerationAtFinalization`; source-uncertain timing remains excluded |
| BASIC-CONDITION-SDK | SDK create-if-absent, conditional overwrite and metadata update demonstrate successful comparisons and stale rejection | `TestObjectMediaLifecycle`; `TestObjectMultipartWriter`; `TestObjectMetadataUpdate`; final current-version execution/evidence still required |

### BEHAVIOR-OBJECT-005: promotable after verification and relocation

Exact declared scope sentence:

> For ordinary finalized objects uploaded through the basic JSON media,
> multipart or resumable paths, compute full stored-byte MD5 and CRC32C, expose
> their documented resource encodings, and reject a validly encoded supplied
> checksum mismatch with HTTP 400 before publishing or replacing the object.

The [validation guide](https://docs.cloud.google.com/storage/docs/data-validation)
says a mismatched “write request is rejected”; it establishes 400, not every
reason string. Exclude Content-MD5, unsupported algorithms, noncanonical/
duplicate/conflicting encodings, third-part MD5/empty/unknown-key conventions,
exact mismatch reasons/precedence, exhaustive download-header presence,
transcoded/range-byte validation and composite/XML/gRPC/encrypted objects.
Move these to JSON-OBJECTS-007-PENDING (GET headers also
JSON-OBJECTS-005-PENDING), SPEC-010/012/016 and SPEC-RSM-EDGES. Composite
checksums remain order 11. Keep EOF/failed-overwrite safety cases, without
claiming undocumented admission/error order. Target remains EXACT.

| Acceptance case | Required observable behavior | Existing ledger test IDs / needed additions |
| --- | --- | --- |
| BASIC-HASH-VECTORS | Independent empty/binary/multichunk vectors give exact MD5, CRC32C and bytes; CRC32C is four-byte big-endian base64 and MD5 is 16-byte base64 | `empty_and_multi_chunk_bodies_have_exact_bytes_and_incremental_hashes`; `obj_http_round_trip_exact_metadata_media_headers_and_nonempty_bucket`; `sequential_binary_chunks_publish_exact_bytes_hashes_and_metadata_only_at_completion` |
| BASIC-HASH-MEDIA | Correct MD5-only, CRC32C-only and both X-Goog-Hash declarations succeed; each wrong valid checksum returns 400 with no new object or failed overwrite | `limits_bad_hashes_and_body_errors_leave_no_generation_or_blob`; `obj_http_bad_hashes_and_unsupported_requests_never_publish`; **NEW** isolated HTTP MD5-only/CRC32C-only/both success/failure matrix with full overwrite snapshots; existing media wire success uses both hashes together |
| BASIC-HASH-MULTIPART | Valid metadata hashes and documented final CRC32C checksum part validate exact media bytes; mismatch preserves old metadata, generation, hashes and bytes | `obj_multipart_http_checks_metadata_header_and_final_part_hashes_before_publication`; `obj_multipart_http_malformed_checksum_json_preserves_existing_generation`; `obj_multipart_http_truncated_checksum_part_preserves_existing_generation`; only documented-container assertions count toward EXACT |
| BASIC-HASH-RESUMABLE | Initiation metadata and final whole-object X-Goog-Hash validate complete bytes, including zero/multiple chunks; no chunk-local checksum is substituted | `checksum_and_final_metadata_failures_preserve_the_previous_full_resource`; `TestObjectResumableChecksumFailurePreservesObject`; `final_metadata_and_completion_snapshot_survive_catalog_restart` |
| BASIC-HASH-NO-PARTIAL-PUBLISH | Missing final EOF/truncated container never replaces the prior object | `producer_closure_without_eof_never_publishes_partial_bytes`; `obj_http_disconnected_overwrite_preserves_the_previous_generation`; multipart truncation tests above |
| BASIC-HASH-SDK | Official SDK upload checksums and metadata/read bytes agree; failed checksum leaves full prior resource | `TestObjectMediaLifecycle`; `TestObjectMultipartWriter`; `TestObjectResumableWriter`; `TestObjectResumableChecksumFailurePreservesObject`; final current-version execution/evidence still required |

### BEHAVIOR-COMMON-007: promotable for envelope structure only

Exact declared scope sentence:

> Basic JSON v1 application errors use an error object containing numeric code,
> message and errors entries with domain, reason and message, preserving the
> documented status/reason pairs for isolated notFound, conditionNotMet and
> requestedRangeNotSatisfiable failures and bodyless 204/304 responses.

This scope does not promise exact provider text, all optional location fields,
the number/order of error entries, diagnostic headers or every service status.
Exclude undocumented parser/hash/MIME reasons and error precedence; authError
detail, WWW-Authenticate and missing-resource authorization; emulator 501 and
configured-cap 413 choices; admission/deadline/commit/reclaim mapping; XML/gRPC/
batch errors. Keep them on the applicable broad method PENDING cases under
SPEC-008/010/012/015/016/017, SPEC-BUCKET-PATCH/PUT, SPEC-RSM-EDGES and
SPEC-FIELDS-EDGES. This separates structure from status selection; successful
error parsing alone does not prove reason/message parity. Target remains EXACT.

| Acceptance case | Required observable behavior | Existing ledger test IDs / needed additions |
| --- | --- | --- |
| BASIC-ERROR-SHAPE | JSON numeric code equals HTTP status; nonempty message/error entries carry domain/reason/message with valid escaping; fields does not truncate errors | `bkt_err_001_envelope_content_type_and_catalog_mappings`; `fields_http::fields_invalid_selectors_and_errors_never_filter_or_mutate`; **NEW** wire assertions for nonempty string messages/reasons and JSON content type across isolated bucket/object failures; existing wire helpers already assert code/domain/reason/message equality |
| BASIC-ERROR-PAIRS | Authorized isolated missing object/bucket 404 notFound, single Match failure 412 conditionNotMet, unsatisfiable single range 416 requestedRangeNotSatisfiable | `bkt_http_crud_metadata_conditions_and_errors`; `obj_http_all_preconditions_read_304_mutation_412_and_missing_precedence`; `obj_range_http_unsatisfiable_empty_and_ignored_ranges`; no missing/condition precedence claim |
| BASIC-ERROR-EMPTY | Successful native delete 204 and single read NotMatch 304 contain no body; invalid fields does not become filtered success | `bkt_http_crud_metadata_conditions_and_errors`; `obj_http_all_preconditions_read_304_mutation_412_and_missing_precedence`; `etag::json_etag_read_object_metadata_media_and_range`; `fields_http::fields_delete_is_ignored_media_is_explicitly_unsupported_and_xml_unchanged` |

### Other basic behavior rows

| Row | Proposal and concrete blocker or exclusion |
| --- | --- |
| BEHAVIOR-COMMON-004 / BEHAVIOR-COMMON-012 | Already IMPLEMENTED for declared documented fields/read-validator scopes; retain their evidence and broad-method edge gaps unchanged |
| BEHAVIOR-OBJECT-010..014 / BEHAVIOR-OBJECT-016 | Already IMPLEMENTED for declared resumable/single-range subsets; no promotion or scope expansion here |
| BEHAVIOR-COMMON-003 | A future string-encoding-only scope is source-supported, but the current row includes simulated projectNumber and parser edge semantics; keep PARTIAL in this proposal until those cases move explicitly and all selected integer fields have wire coverage |
| BEHAVIOR-COMMON-006 | Keep PARTIAL: current scope includes bucket PUT omitted/default/null rules and PATCH null/no-effect rules under SPEC-BUCKET-PUT/PATCH, plus SPEC-015 object input/empty/null edges. General replacement intent does not settle all those values |
| BEHAVIOR-COMMON-001/002 | Keep PARTIAL: provider domain ownership/ambiguous names and cross-protocol routing/canonicalization remain outside verified JSON naming; no new broad naming completion claim |
| BEHAVIOR-COMMON-005/014 | Keep PARTIAL: full ACL/owner permissions and genuine project/requester-pays resolution are absent; modeled fields/local project numbers are insufficient |
| BEHAVIOR-COMMON-009/013 / BEHAVIOR-OBJECT-018 | Keep PARTIAL: metadata/history/propagation coverage and other-resource paging remain incomplete; zero-size, marker accounting and empty-list shape are SPEC-LIST-PAGING/RESOURCE-SHAPE. Query-bound local tokens are tested but not provider encoding or snapshots |
| BEHAVIOR-OBJECT-001/002/003 | Keep PARTIAL: advanced metadata, complete version/soft-delete interactions and time/lifecycle behavior remain outside the frozen live subset; source-silent no-effect revisions and output-input treatment also remain |
| BEHAVIOR-OBJECT-006/007/008/009/015 | Keep PARTIAL: exact opaque ETags/links/headers, redirect, missing-MIME acceptance, multipart parser/error edges and replay remain SPEC-009..013/015/016 and SPEC-JSON-ETAG-WIRE; a successful transfer cannot remove those deviations |

### Preserve original acceptance content during integration

For the three candidate promotions, preserve the full original cases below on
the affected broad methods before replacing their behavior-row decomposition.
Keep existing PASS results with their original evidence; provider-uncertain
assertions also need explicit PENDING parity cases, without downgrading the
local execution result. The gap map supplies the destination for **every**
existing known_deviation, including stale statements about delivered features.

| Original cases retained in full | Destination when outside the declared promoted scope |
| --- | --- |
| BEHAVIOR-COMMON-011-BUCKET, -MEDIA, -METADATA, -METADATA-BOUNDARIES, PATCH-CONDITIONS, PUT-CONDITIONS | Respective bucket GET/DELETE/PATCH/PUT and object GET/DELETE/insert/PATCH/PUT rows; SPEC-008/015/017 and selected-version gaps retain local statuses and precedence |
| BEHAVIOR-COMMON-011-PUBLICATION, -MEDIA-SAFETY, -ADMISSION, -MULTIPART-PRECONDITIONS-TOKENS, -MULTIPART-CONCURRENCY, -MULTIPART-GO-SDK, -PENDING | JSON-OBJECTS-007 and affected delete/get rows; SPEC-008/013/016/017; retain EOF/advisory/race/permission/header assertions, whole SDK evidence and unsupported-history/transport remainder |
| BEHAVIOR-OBJECT-005-MEDIA, -PUBLICATION, -MEDIA-SAFETY, -ADMISSION, -MULTIPART-CHECKSUMS, -MULTIPART-GO-SDK, -PENDING | JSON-OBJECTS-007, header assertions on JSON-OBJECTS-005, and existing publication/design rows; SPEC-008/010/012/016, SPEC-RSM-EDGES and composite scope; preserve parser/conflict/reason assertions and all failed-overwrite checks |
| BKT-ERR-001, BEHAVIOR-COMMON-007-MEDIA, -PUBLICATION, -MEDIA-SAFETY, -ADMISSION, -PENDING | Respective bucket/object CRUD methods and existing recovery/design rows; preserve 400/405/409/412/413/501 mappings, early-body/commit/deferred-reclaim tests and pending auth/transports/precedence; SPEC-008/010/012/015/016/017 |

`NEW` means a needed test/evidence assertion, not an invented passing test ID.
The integration task must replace each such marker with actual tests or show
the existing exact assertion, record current commands/results and migrate
excluded cases before any promotion. Do not claim that this documentation
alone closes condition 3 or completes a whole API method. The next delivery
remains order 8 verification and ledger integration, followed by object copy.
