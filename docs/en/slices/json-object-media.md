# JSON object media slice

Contract checked on **2026-10-21**. This is the integrated slice's acceptance decomposition,
not evidence that an API is implemented. `compatibility/features.json` remains the sole
status authority. Candidate rows: JSON-OBJECTS-004/005/007,
BEHAVIOR-OBJECT-002/005/007/008/015, BEHAVIOR-COMMON-002/011,
DESIGN-070/073/074/075/082 and the nonempty case of JSON-BUCKETS-001.

## Contract decisions

The supported behavior and local deviations are defined below. Source questions remain
open where real-service fidelity is unverified. Execution evidence and limits are in the
[verification summary](../evidence/object-media.md).

| Item | Integrated decision |
| --- | --- |
| 1 | Read failures retain `ObjectError::NotModified` and return an empty 304; mutations use 412. Missing selected resources take 404 precedence; failed match conditions take precedence over failed not-match conditions. These precedence/mutation choices remain local rules under SPEC-008. |
| 2 | Both media prefixes serve 200 directly. Ordinary-prefix 303 redirection is an explicit unimplemented deviation (SPEC-009). |
| 3 | The direct download route is supported from request-endpoints documentation, not a nonexistent Discovery field; non-media direct-download requests return 400 `notDownload`. |
| 4 | Generation selectors are get/delete-only and projection is insert/get-only; other misplaced parameters fail 400. Signed int64 values are accepted without a leading plus; selectors that do not identify the live positive generation yield 404. |
| 5 | `X-Goog-Hash` validates MD5/CRC32C before publication; `Content-MD5` and unknown algorithms fail 501, duplicate algorithms/noncanonical encodings fail 400. Duplicate handling and mismatch reason `invalid` are local choices (SPEC-010). |
| 6 | Missing Content-Type is accepted as a local convenience and stored as `application/octet-stream`; this does not claim Google's upload-header acceptance (SPEC-011). |
| 7 | Media includes ETag, identity stored-content-encoding, generation/metageneration, hashes and exact lengths. The rendered subset is tested, while complete production header parity remains unverified (SPEC-012). |
| 8 | `timeFinalized` equals creation time for finalized-only objects. Local ID/ETag/link encodings and omitted unsupported resource fields remain explicit (SPEC-012). |
| 9 | Unsupported ETag, Range/If-Range, encryption and custom metadata fail 501. Upload tokens fail 501 before body consumption. GET/DELETE accept tokens without deduplication as an explicit local SDK rule; retries re-execute (SPEC-013). |

### Official-source rationale

The [preconditions guide](https://docs.cloud.google.com/storage/docs/request-preconditions)
assigns 304 to false not-match conditions; the JSON error catalog also describes failed
preconditions under 412. Mutation and combined-condition precedence remain local rules
under SPEC-008. The ordinary media endpoint returns direct 200 locally despite the error
catalog's 303 rule; the get page does not explain that redirect (SPEC-009). Discovery
revision `20261013` confirms media download support but has no `mediaDownload.path`; the
[request-endpoints reference](https://docs.cloud.google.com/storage/docs/request-endpoints)
independently establishes the optional download prefix.

The [data-validation guide](https://docs.cloud.google.com/storage/docs/data-validation)
explicitly supports JSON media `X-Goog-Hash`. JSON `Content-MD5`, duplicate algorithms and
exact mismatch reasons remain SPEC-010. Missing upload Content-Type acceptance is a local
convenience, distinct from the Object resource's default serving MIME type (SPEC-011).
Generation/hash header parity and exact opaque ID/ETag/link encodings remain SPEC-012;
`timeFinalized` is emitted for this finalized-only subset. Unsupported headers and
method-specific parameters fail explicitly; GET/DELETE retry-token acceptance without
deduplication remains SPEC-013. No IAM/ACL fidelity follows from ordinary success.

## Sources and provenance

All sources below were checked on 2026-10-21:

- [objects.insert](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/insert)
- [objects.get](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/get)
- [objects.delete](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/delete)
- [Object resource](https://docs.cloud.google.com/storage/docs/json_api/v1/objects)
- [Upload types](https://docs.cloud.google.com/storage/docs/uploads)
- [Single-request/media upload example](https://docs.cloud.google.com/storage/docs/uploading-objects#rest-upload-objects)
- [Request endpoints and URI encoding](https://docs.cloud.google.com/storage/docs/request-endpoints)
- [Object naming](https://docs.cloud.google.com/storage/docs/objects#naming)
- [Data validation, including JSON media X-Goog-Hash](https://docs.cloud.google.com/storage/docs/data-validation)
- [Hash syntax](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers#xgooghash)
- [Metadata and generation identity](https://docs.cloud.google.com/storage/docs/metadata#generation-number)
- [Preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions)
- [JSON headers and common parameters](https://docs.cloud.google.com/storage/docs/json_api/v1/parameters)
- [JSON status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes)
- [Bucket deletion interaction](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/delete)
- [Storage classes](https://docs.cloud.google.com/storage/docs/storage-classes)
- [Discovery v1](https://storage.googleapis.com/$discovery/rest?version=v1), revision
  `20261013`, SHA-256 `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`.
- [Storage v2 proto](https://github.com/googleapis/googleapis/blob/d73dfb3c3be52245625b951b9c0bcf65163c549f/google/storage/v2/storage.proto),
  latest file-changing commit `d73dfb3c3be52245625b951b9c0bcf65163c549f`, SHA-256
  `a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf`.

The proto independently confirms generation selectors, conditional writes/deletes and
full-object MD5/CRC32C state. JSON method documentation owns JSON HTTP status decisions;
RPC transport semantics are not substituted for unresolved JSON behavior.

The digest is of the fetched document bytes, before parsing or formatting. Selected raw
Discovery fields and reproduction commands are retained in
[source evidence](../evidence/object-media-sources.txt). Its insert `path` is `b/{bucket}/o`,
relative to `servicePath=storage/v1/`; that generic method path is not the media upload URL.
`mediaUpload.protocols.simple.path` is `/upload/storage/v1/b/{bucket}/o`;
`mediaUpload.protocols.resumable.path` is `/resumable/upload/storage/v1/b/{bucket}/o`.
Both protocols advertise `multipart=true`; this does not make multipart implemented locally.
Get/delete use `b/{bucket}/o/{object}`. Download flags are described in contract decision 3.

Existing SPEC-001/002/003/004/006/007 were reviewed; they do not authorize extra object
operations or Rapid semantics. SPEC-008 through SPEC-012 record questions affecting this
slice. No real-cloud bucket, credentials or metadata-server access is required or authorized.

## Explicit subset and deviations

One finalized live generation per `(bucket, name)` is stored on disk. The supported path is
single-request JSON `uploadType=media`, metadata get, whole-object media get and delete.
The application must stream bodies and checksums through bounded buffers to opaque blob IDs;
object names never become filesystem paths. An upload is visible only after complete body,
checksum validation, durable blob publication and an atomic catalog commit.

Multipart, resumable sessions, version retention, soft delete, ranges, transcoding, custom
metadata, ACL/IAM, authentication enforcement, encryption, retention/holds, notifications,
copy/compose/rewrite, listing and metadata patch/update remain outside this slice. Known
unsupported requests return 501 `notImplemented`, a local deviation rather than a Google
error claim. They must not receive success after their semantics were ignored. Unknown
routes continue to return 404; this document does not require success stubs for other APIs.

`projection=noAcl` is the default. As in the bucket slice, `full` may return the same modeled
fields without owner/ACL for SDK compatibility; that omission is a declared deviation.
No permissive/strict profile or production permission result is claimed. Production requires
`storage.objects.create` for upload, additionally `storage.objects.delete` to overwrite,
`storage.objects.get` for get, `storage.objects.getIamPolicy` for ACL projection and
`storage.objects.delete` for deletion. A locally constructed media URL grants no permission.

## Methods, parameters and request headers

Paths are relative to configured `api_base_url`. Ordinary media downloads deliberately omit
production redirection and return 200 directly.

| Operation | Method/path | Final success | Body |
| --- | --- | --- | --- |
| insert media | `POST /upload/storage/v1/b/{bucket}/o?uploadType=media&name={name}` | 200 Object JSON | Raw bytes, including zero bytes |
| metadata get | `GET /storage/v1/b/{bucket}/o/{object}` | 200 Object JSON | None |
| media get | `GET /storage/v1/b/{bucket}/o/{object}?alt=media` | 200 media (local direct-response deviation) | None |
| direct media get | `GET /download/storage/v1/b/{bucket}/o/{object}?alt=media` | 200 media | None |
| delete | `DELETE /storage/v1/b/{bucket}/o/{object}` | 204 empty | None |

Decode query components once as UTF-8 (`+` means space); split raw route structure before
decoding path components (`+` is literal in a path). Keep the existing bucket parser's
16 KiB raw-query cap and duplicate decoded-key rejection. Malformed escaping/UTF-8 and
unknown or method-inappropriate query parameters return 400 `invalidParameter` as local
validation rules, not an inferred upstream unknown-field policy. A raw slash is route
structure; callers encode embedded name slashes as `%2F`.

| Methods | Parameter | Decision |
| --- | --- | --- |
| insert | `uploadType` | Required; `media` supported; `multipart`/`resumable` rejected 501; other values 400 |
| insert | `name` | Required and nonempty because media has no metadata container |
| insert/get/delete | `ifGenerationMatch`, `ifGenerationNotMatch`, `ifMetagenerationMatch`, `ifMetagenerationNotMatch` | Signed int64 strings parsed without overflow; atomic comparisons; zero generation-match is create-if-absent |
| get/delete | `generation` | Optional exact live-generation selector; omitted selects live; absent selected generation 404, no historical fallback |
| insert/get | `projection` | `noAcl` default; `full` modeled subset only; other values 400 |
| insert | `contentEncoding`, `kmsKeyName`, `predefinedAcl` | Recognized and rejected 501, including empty values |
| insert/get/delete | `userProject` | Rejected 501; no billing model |
| get | `softDeleted` | Default/false selects live; true rejected 501; invalid boolean 400 |
| get | `restoreToken` | Rejected 501; no HNS/soft-deleted identity |

An insert `generation` or delete `projection` is not a method parameter. Preconditions do
not select old versions: `generation` selects, then conditions apply to that same record.
The special generation-match zero can only succeed when writing object data; GET/DELETE
must never turn it into a successful absent-object response. A not-match condition cannot
create an object when there is no live version. All supplied conditions must hold.
Malformed integer syntax, leading plus signs and overflow fail validation before streaming;
numeric signed values do not wrap into other generations. Exact missing-resource/error
precedence and conflicting conditions remain SPEC-008.

Common parameters: `alt=json` defaults for metadata/insert; `alt=media` only for media GET;
unknown `alt` values return 400 `invalidAltValue`. `fields`, `callback`, `key`, `oauth_token`,
`quotaUser` and `userIp` are recognized but unsupported (501). `prettyPrint=false` is
supported; true is unsupported (501), invalid booleans are 400. Compact JSON by default
differs from Discovery's pretty-print default. `uploadType` on get/delete is invalid for the
method. `/download` with a normal metadata request requires 400 `notDownload`, not a
metadata response disguised as a download; upload requests on the wrong endpoint must fail.

| Header | Decision |
| --- | --- |
| `Content-Type` on insert | Valid media type becomes stored `contentType`; missing is accepted locally as `application/octet-stream` (SPEC-011); no extension inference |
| `Content-Length` | Honor framing and cap; known oversize can fail before reading; no unchecked allocation based on the value |
| `Transfer-Encoding: chunked` | Supported bounded stream with unknown final length; full cap/checksum validation still applies |
| `X-Goog-Hash` on insert | MD5 (16 bytes), CRC32C (4 big-endian bytes), or both in base64; validate against the complete bytes before publication |
| `Content-MD5` on insert | No JSON support claim; reject 501 rather than silently treating it as validation (SPEC-010) |
| `Range`, `If-Range` on media get | Reject 501; never return an unannounced full body or claim 206 support |
| `If-Match`, `If-None-Match` | Reject 501 until ETag condition semantics exist; query generation conditions remain separate |
| `Content-Encoding`, custom metadata and encryption semantic headers | Unsupported behavior fails explicitly; no accepted-but-ignored compression, key or metadata request |
| `X-Goog-Gcs-Idempotency-Token` | Upload returns 501 before reading. Metadata/media GET and DELETE accept the header without deduplication: each retry executes again; repeated DELETE normally returns 404 after its first 204 (SPEC-013). |

Benign transport/client identification headers are not rejected simply for being unknown.
Authorization handling follows the existing local unauthenticated implementation and must
not be described as credential verification. Executed parser and wire coverage is recorded
in the verification summary; the header list alone does not establish compatibility.

## Resource and media response

Emitted Object fields in the modeled subset: `kind="storage#object"`, `id`,
`selfLink`, `mediaLink`, `name`, `bucket`, `generation`, `metageneration`, `contentType`,
`storageClass`, `size`, `md5Hash`, `crc32c`, `etag`, `timeCreated`, `updated` and
`timeStorageClassUpdated` and `timeFinalized` (equal to creation time). No JSON metadata
container is parsed on media upload: a body resembling Object JSON is simply object data.
Unsupported properties are omitted rather than synthesized to suggest their effects exist.

Generation/metageneration are decimal JSON strings; size is an unsigned decimal JSON string.
Hashes use standard base64, CRC32C in network byte order. `storageClass` inherits the bucket
default, never an ignored object override. New/replacement data has a distinct nonzero
generation and metageneration 1, even for identical bytes or a frozen clock; creation/update
times initially agree, and class-update time equals creation. Local millisecond RFC 3339 UTC
formatting follows bucket rendering. Google's generation ordering is not promised; tests
assert identity changes, not time-derived values or ordering across unrelated objects.

`id` contains bucket/name/generation; slash concatenation and ETag encoding are
opaque local identifiers. They must distinguish generations, remain stable for repeated
reads and not expose blob paths. Links use configured `api_base_url`, preserving an optional
base path; neither Host nor forwarded headers can affect them. Percent-encode object names
once in generated links. `selfLink` has no generation query and addresses the current
resource; `mediaLink` includes `generation` and selects the direct media endpoint with
`alt=media`, so following it cannot read replacement bytes (SPEC-012).

Successful whole-media responses must describe the exact opened generation:

| Header | Value/evidence boundary |
| --- | --- |
| `Content-Type` | Stored media type, otherwise `application/octet-stream` |
| `Content-Length` | Decimal exact byte count, including `0` |
| `ETag` | Quoted opaque entity tag; consistent with the response resource, local encoding |
| `x-goog-generation`, `x-goog-metageneration` | Decimal selected record values; explicit JSON evidence still required (SPEC-012) |
| `x-goog-hash` | Computed `crc32c=...` and `md5=...`; hash value order is not semantic |
| `x-goog-stored-content-length` | Exact stored byte count; no transcoding |
| `x-goog-stored-content-encoding` | `identity` for the supported unencoded subset |

No `Content-Range` or successful `Accept-Ranges: bytes` claim is made. JSON resources/errors
use `application/json; charset=UTF-8`; media must never inherit that JSON content type.
No provider trace IDs, production caching/authorization policy or complete header parity is
claimed. Response headers and media stream must be bound to one selected generation even
if it is overwritten/deleted while being downloaded.

## Errors and state transitions

Use the bucket slice's JSON envelope: `error.code`, `error.message` and a one-element
`error.errors` array with `domain="global"`, `reason` and matching `message`.
Never expose internal SQLite, filesystem or blob identifiers. A 304/204 has no JSON body.

| Case | Status | Reason / boundary |
| --- | --- | --- |
| Missing required uploadType/name | 400 | `required` |
| Invalid name/query/int64/hash syntax | 400 | `invalidParameter` (hash parser reason is a local choice) |
| Unsupported alt value | 400 | `invalidAltValue` |
| Checksum mismatch | 400 | `invalid`; exact Google reason unverified, SPEC-010 |
| Body stream aborted / nonempty body on get/delete | 400 | `badRequest` / `invalidArgument`, local mapping |
| Non-media request on direct download endpoint | 400 | `notDownload` |
| Unknown upload prefix / wrong verb on recognized object path | 404 / 405 | Local route rejection; production `notUpload`/`wrongUrlForUpload` bodies are not reproduced |
| Missing bucket/object/selected generation | 404 | `notFound`; combined-condition precedence is SPEC-008 |
| Bucket contains live objects | 409 | `conflict`, inherited local reason; bucket must remain intact |
| Failed match condition / create-if-absent loser | 412 | `conditionNotMet` |
| GET not-match condition false | 304 empty | `notModified` semantics, no error envelope |
| Mutation not-match condition false | 412 | `conditionNotMet`; explicit local mutation rule, SPEC-008; no mutation |
| Object exceeds configured cap | 413 | `uploadTooLarge`; Google's documented maximum is 5 TiB; a smaller local cap is a deviation |
| Recognized unsupported semantics | 501 | `notImplemented`, local deviation |
| Wrong verb on recognized route | 405 | `methodNotAllowed`, with `Allow` |
| Catalog/blob unavailable before response | 503 | `backendError`, local infrastructure mapping |

Ordinary insert overwrites instead of returning duplicate-name 409. Preconditions are
evaluated atomically against catalog state at publication; an advisory bucket/condition
check rejects already-doomed uploads before staging. Body transfer never holds a
database transaction. Two concurrent create-if-absent uploads have exactly one successful
commit and one 412; failures preserve the old object and publish no partial metadata/blob.
A successful delete removes the live record atomically with condition checks; repeated
delete is 404. Bucket deletion checks object existence and bucket conditions in the same
transaction as removal, including races with uploads.

After request/header validation and the known Content-Length cap check, upload admission
rejects excess uploads immediately with 503, before any catalog wait or advisory 404/412.
For admitted uploads, one advisory check finishes before any body streaming: a missing
bucket 404 or failed condition 412 takes precedence over streamed-size 413, body-error 400
and checksum-mismatch 400, regardless of
body timing. This is a local precedence choice under SPEC-008; the commit transaction still
rechecks bucket existence and conditions against the state at publication. A concurrent
mutation after the advisory check is observed only at publication, after body validation.

Durability is a separate project contract: stage/hash with bounded buffers, sync data, publish
the opaque immutable blob and sync its directory, then commit its catalog reference. Startup
recovery under the directory lock removes staging/orphans in bounded batches and refuses
readiness for missing referenced data. Well-known `.DS_Store` and `._*` OS metadata entries
are ignored in staging/blob scans; other invalid entries name their path in startup errors.
Once a mutation commits it returns success even if old-blob reclaim fails; the failure is
logged and its journal entry is retried at next startup. If reclaim fails again at startup,
readiness is blocked until the cause is fixed; the startup diagnostic names the blob ID.
Reader lifetime protects the exact opened blob from replacement/deletion cleanup.
These are required tests, not a claim that
SQLite and rename are one atomic transaction or that host power-loss durability has been measured.

Schema v3 stores a singleton generation high-water mark, advanced in the same transaction as
publication, so deletion, restart and a backward clock cannot reuse an identity allocated
by v3 from an empty object catalog. Intermediate unreleased v3 catalogs with a previously
seeded allocator are accepted without reconstructing their deleted history.
Startup migrates valid v1 catalogs, which contain no objects, directly to v3 in one
transaction so migration failure cannot leave a v2 marker. Unreleased v2 catalogs
are rejected before changing journal mode or recovering files: their deleted-generation
history cannot be recovered safely.
Read-only preflight compares the version's bounded schema inventory, including constraints,
collations and indexes, before changing journal mode or recovering files. Foreign/manual schema variants
are rejected; reformatted equivalent schemas are accepted only when whitespace normalization
matches the generated DDL. Under the directory lock, preflight uses an escaped immutable
read-only URI when no WAL exists, leaving no new WAL/SHM sidecars on rejection. With a hot
WAL it uses ordinary read-only mode to retain committed-frame visibility. Immutable
preflight does not consider a hot rollback `-journal`; gcs-mimic never creates one in WAL
mode, but a foreign DELETE-mode catalog could. Recovery validates the counter against
current live generations.

## Runtime bounds and timeouts

`max_object_bytes` defaults to **1 GiB (1,073,741,824 bytes)**; the supported setting range is
1 byte through 5 TiB. `--max-object-bytes`, `GCS_MIMIC_MAX_OBJECT_BYTES` and JSON
`max_object_bytes` follow existing CLI > environment > JSON > default precedence. There is no
unlimited mode. Content-Length above the cap is rejected before reading; the producer and
storage writer independently enforce the limit for unknown-length streams.

Uploads have a two-slot Tokio channel with copied chunks of at most 64 KiB; a queued slice
cannot retain an oversized incoming transport frame. Hyper parsing remains bounded by its
16 KiB connection buffer. Downloads use a one-slot http-body-util channel and a 64 KiB reader.
The disk writer holds no database transaction or blocking worker while waiting for a body.
Eight upload permits are available, with immediate excess rejection; ordinary catalog
admission is 32, and one execution permit is acquired before scheduling a short SQLite job.
The default two blocking workers therefore cannot both be occupied by uploads waiting for
body input or by catalog jobs waiting for each other. File jobs and the single SQLite job
share the bounded pool, so simultaneous large writes/fsyncs can delay catalog work;
throughput/fairness and RSS/cgroup memory are not benchmarked here.
Connection admission remains the configured `max_connections` (default 32).
Trickle uploads can hold all eight permits indefinitely by progressing before each
inactivity deadline; no total-upload deadline or minimum transfer rate is enforced.

`connection_timeout_ms` remains 5 seconds by default. It is an HTTP header-read deadline;
for recognized object routes it then bounds inactivity between upload/download chunks and
response startup. Each progressed chunk renews the deadline, so active transfers may last
longer than 5 seconds. The awaited upload advisory check suspends body inactivity and
restarts its deadline before streaming; catalog admission stays bounded at 32 calls and
SQLite lock waits retain their busy timeout. A fully sent upload queued behind another
commit therefore still receives its result beyond the inactivity deadline. After Hyper
confirms upload-body EOF, body inactivity no longer applies while storage syncs/publishes/commits;
the connection waits for the storage result.
After EOF, remaining queued writes, fsync, rename and directory sync, and shutdown's catalog
close wait on the filesystem without a wall-clock bound. SQLite lock waits retain their
five-second busy timeout. Backpressured downloads stop progressing and are closed on inactivity.
Bucket/probe/unknown routes retain the previous total connection deadline. Keep-alive stays
disabled; graceful drain still has the independent `shutdown_timeout_ms` network bound.
Before confirmed EOF, timeout/disconnect cancels the upload and triggers bounded cleanup
without publishing partial data; channel closure without an explicit EOF marker is an abort.
After confirmed EOF, the complete body may commit even if the client disconnects or shutdown
aborts the connection. Normal completion returns the committed result. Early rejections
send the response, half-close the socket and discard at most 256 KiB for at most 100 ms.

## Numbered acceptance cases

All cases below are pending execution in this document. Integrators link actual test IDs and
results in the feature ledger; source research and successful validation are not API tests.

| No. / ID | Observable assertion | Evidence owner |
| --- | --- | --- |
| 1 / OBJ-MEDIA-001 | Upload, metadata, media, direct download and delete paths/verbs; endpoint misuse and 405 Allow | Adapter + wire |
| 2 / OBJ-MEDIA-002 | Ordinary media GET has documented redirect or a declared direct-200 deviation; redirect retains encoded name, conditions and configured base | Integration; SPEC-009 |
| 3 / OBJ-MEDIA-003 | Media requires uploadType and name; query name is decoded once; no metadata-only insert succeeds | Adapter |
| 4 / OBJ-MEDIA-004 | All method/common parameter decisions above, defaults, unknown/misplaced names and encoded duplicate keys | Adapter |
| 5 / OBJ-MEDIA-005 | Signed int64 boundaries, overflow, invalid syntax, plus signs, duplicate conditions and query cap | Adapter |
| 6 / OBJ-MEDIA-006 | No Content-Type behavior is reconciled; supplied MIME type persists without extension inference | Adapter + wire; SPEC-011 |
| 7 / OBJ-MEDIA-007 | Zero-length and binary/non-UTF-8 body round trips exactly; Object-shaped JSON remains raw bytes | Storage + wire |
| 8 / OBJ-MEDIA-008 | Content-Length and chunked/unknown length produce identical bytes/metadata without payload collection | Storage + wire |
| 9 / OBJ-MEDIA-009 | Exact configured size cap succeeds; cap+1 fails 413 without publication; known oversize rejected early | Storage + wire |
| 10 / OBJ-MEDIA-010 | Midstream disconnect/error, deadline and cancellation preserve old data and leave no visible partial object | Storage + integration |
| 11 / OBJ-MEDIA-011 | MD5 and CRC32C match independent known vectors, including empty data and multiple chunks | Storage |
| 12 / OBJ-MEDIA-012 | Header MD5-only, CRC32C-only and both, including repeated header fields, validate complete body | Adapter + storage + wire |
| 13 / OBJ-MEDIA-013 | Base64 alphabet/padding and 16/4-byte lengths rejected when invalid; algorithm duplicates/unknown algorithms have explicit local policy | Adapter; SPEC-010 |
| 14 / OBJ-MEDIA-014 | Either checksum mismatch returns 400 and preserves prior generation/bytes; success is impossible when only one hash matches | Storage + wire |
| 15 / OBJ-MEDIA-015 | Content-MD5 alone/with X-Goog-Hash follows reconciled policy and never creates false validation success | Adapter + wire; SPEC-010 |
| 16 / OBJ-MEDIA-016 | Resource kind/identity/name/bucket/numeric strings, MIME, class, hashes, ETag, timestamps and links match selected record | Adapter + wire |
| 17 / OBJ-MEDIA-017 | noAcl defaults; full's ACL/owner omission is explicit; all unsupported fields remain absent; finalization-time decision recorded | Adapter; SPEC-012 |
| 18 / OBJ-MEDIA-018 | Overwrite identical/different data, including frozen-clock operations, changes generation and resets metageneration to 1 | Storage |
| 19 / OBJ-MEDIA-019 | timeCreated/updated/class-update time are initialized consistently per generation; rendering handles clock boundaries safely | Adapter + storage |
| 20 / OBJ-MEDIA-020 | Bucket storageClass default is inherited on creation/replacement; rejected encoding/encryption/ACL settings never appear applied | Storage + wire |
| 21 / OBJ-MEDIA-021 | Default metadata GET is JSON; media returns exact bytes/type/length, zero-byte media remains 200 | Wire |
| 22 / OBJ-MEDIA-022 | All declared media response headers refer to the same generation and computed checksums | Wire + pinned Go SDK; SPEC-012 |
| 23 / OBJ-MEDIA-023 | Exact generation selector succeeds for live generation; stale/deleted/nonexistent generation returns 404 | Storage + wire |
| 24 / OBJ-MEDIA-024 | Match success/failure for generation and metageneration; failure is 412 and leaves state intact | Storage + wire |
| 25 / OBJ-MEDIA-025 | GET false not-match returns empty 304 after match checks; mutation false not-match has reconciled status and no side effect | Storage + wire; SPEC-008 |
| 26 / OBJ-MEDIA-026 | Multiple conditions all apply to the selected generation; combined failure and missing-resource precedence is explicit | Storage + wire; SPEC-008 |
| 27 / OBJ-MEDIA-027 | ifGenerationMatch=0 creates only when absent; existing live object yields 412; GET/DELETE cannot succeed on absent data with zero | Storage + wire |
| 28 / OBJ-MEDIA-028 | Two simultaneous ifGenerationMatch=0 uploads have one commit/one 412 and complete winner bytes; no leaked visible loser | Storage + wire concurrency |
| 29 / OBJ-MEDIA-029 | ifGenerationNotMatch=0 succeeds only with a live version; absent version never becomes an unconditional create | Storage + wire |
| 30 / OBJ-MEDIA-030 | Conditional overwrite/delete races compare and mutate atomically; failure cannot remove or replace another generation | Storage + wire concurrency |
| 31 / OBJ-MEDIA-031 | GET/open pins record and blob together across overwrite/delete; streamed response never mixes generations | Storage + wire concurrency |
| 32 / OBJ-MEDIA-032 | Missing bucket/object, repeated delete and missing selected generation produce 404 envelope without internal diagnostics | Wire |
| 33 / OBJ-MEDIA-033 | Delete live object returns empty 204; recreated name gets distinct generation even with frozen clock or restart | Storage + wire |
| 34 / OBJ-MEDIA-034 | Nonempty bucket deletion returns 409 and preserves objects; after last object deletion bucket deletion succeeds | Catalog + wire |
| 35 / OBJ-MEDIA-035 | Concurrent bucket deletion/upload cannot commit a dangling object; in-flight staging is not a published live object | Catalog concurrency |
| 36 / OBJ-MEDIA-036 | Name boundaries 1/1024 UTF-8 bytes; empty/1025 bytes, CR/LF, exact dot/dot-dot and reserved ACME prefix fail | Adapter + wire |
| 37 / OBJ-MEDIA-037 | Unicode names round trip by bytes without normalization; multibyte boundary measured in bytes rather than scalar count | Adapter + wire |
| 38 / OBJ-MEDIA-038 | a%2Fb addresses a/b; a%252Fb addresses literal a%2Fb; plus/space/%/#/? encode distinctly; no double decoding | Adapter + wire |
| 39 / OBJ-MEDIA-039 | Malformed escapes/UTF-8 fail; nested dot components, leading/trailing/repeated slash and backslash remain logical names, never filesystem paths | Adapter + storage |
| 40 / OBJ-MEDIA-040 | Naming recommendations (#, wildcard/bracket/control compatibility concerns) are not invented blanket GCS prohibitions | Adapter |
| 41 / OBJ-MEDIA-041 | Host/forwarded headers cannot alter selfLink/mediaLink; configured base path survives traversal, and mediaLink retains generation | Wire |
| 42 / OBJ-MEDIA-042 | Multipart/resumable/Range/ETag conditions/fields/encoding/encryption/upload tokens and unsupported queries fail explicitly without body publication; GET/DELETE tokens follow SPEC-013 | Adapter + wire |
| 43 / OBJ-MEDIA-043 | softDeleted=false uses live data; true/restoreToken/versioning-dependent requests never claim historical support | Adapter + wire |
| 44 / OBJ-MEDIA-044 | Permission omissions are documented; authenticated-looking headers and URL generation do not grant or verify IAM/ACL rights | Scope review |
| 45 / OBJ-MEDIA-045 | Restart preserves successful object metadata and bytes/checksums/generations, with opaque names and bucket links intact | Storage recovery |
| 46 / OBJ-MEDIA-046 | Crash boundaries before/after blob publication and catalog commit reconcile staging/orphans; missing referenced blob prevents readiness | Storage recovery |
| 47 / OBJ-MEDIA-047 | Bounded chunks, channels, admission/workers and catalog queries hold as payload/catalog size grows; resource claim needs measurements | Storage + integration |
| 48 / OBJ-MEDIA-048 | Pinned Go JSON media upload/Attrs/NewReader/Delete, generation conditions and checksums use only supported transport; no cloud/metadata access | SDK harness |
| 49 / OBJ-MEDIA-049 | Error envelope/status/reason/content type, empty 204/304 and no body on get/delete cover all mappings above | Adapter + wire |
| 50 / OBJ-MEDIA-050 | Full objects.insert/get/delete rows remain PARTIAL wherever multipart, fields, ACL/permissions, ranges or other scoped cases are missing | Ledger review |

Object-name rules above are for flat namespace buckets: valid Unicode, 1–1024 UTF-8 bytes,
no CR/LF, not exactly `.`/`..`, and no `.well-known/acme-challenge/` prefix. Recommendations
about other special characters are not mandatory bans. HNS folder/base-name limits are not
inferred from the flat-namespace rule and remain out of scope.

## Verification and next slice

The [dependency evidence](../evidence/object-media-dependencies.txt) records Rust 1.99.1,
ten direct Rust crates, Go 1.27.3, storage v1.69.0, API v0.306.0 and the adopted Docker
digests as CURRENT on 2026-10-21. The [verification summary](../evidence/object-media.md)
records the Rust, SDK, container, concurrency and recovery results and their limits.
Implementation status and fidelity remain authoritative only in the ledger.

The next slice is object listing and metadata: prefix/delimiter/keyset pagination and
conditional metadata patch versus replacement. JSON multipart upload follows separately;
neither feature is implied by this media contract.
