# JSON resumable object uploads

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

Contract checked: **2026-10-22**. Delivery: `resumable-upload`, order 4.
Scope: BEHAVIOR-OBJECT-010 through BEHAVIOR-OBJECT-014 and DESIGN-057, with
contributions to JSON-OBJECTS-007 and DESIGN-082. This is an acceptance contract,
not execution evidence. Only `compatibility/features.json` owns implementation
status; the two broad parent rows remain PARTIAL.

## Scope and exclusions

Provide JSON initiation, bounded sequential chunk writes, status/resume,
finalization, cancellation, seven-day expiry and persistent-session recovery.
Preserve the existing single-request media/multipart paths and shared object
metadata, checksum, generation and authorization rules. Ordinary objects become
visible only at finalization; a failed overwrite preserves the old resource.

XML resumable/multipart, signed initiation (DESIGN-042), gRPC/Rapid appendable
objects, other SDK transports, full ACL/UBLA/PAP, encryption, billing and browser
CORS enforcement are deferred. Accepting `Origin` is required for initiation
interoperability, but does not establish browser CORS support. Invocation-token
response replay remains SPEC-013; session offsets and completed-session status
are a separate protocol state machine. No real-cloud request is authorized here.

## Sources

Fetched and read on **2026-10-22**. Exact fetched-byte SHA-256 values, the
Discovery revision and immutable upstream identities are in
[Sources](../sources.md#resumable-upload-source-pins).

| Source | Contract used |
| --- | --- |
| [Perform resumable uploads](https://docs.cloud.google.com/storage/docs/performing-resumable-uploads) | JSON POST initiation, PUT chunks, 308/Range, zero-body status, retry, final 200/201, DELETE cancellation 499 |
| [Resumable uploads](https://docs.cloud.google.com/storage/docs/resumable-uploads) | Bearer session URI, one-week lifetime, completion visibility, immutable persisted prefix, 410/404 invalid-session distinction |
| [Legacy JSON how-to](https://docs.cloud.google.com/storage/docs/json_api/v1/how-tos/resumable-upload) | Redirects to the performing guide; not a separate protocol |
| [Objects.insert](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/insert) and [Discovery](https://storage.googleapis.com/$discovery/rest?version=v1) | Initiation parameters, metadata Object, permissions, all four preconditions and response resource |
| [JSON headers](https://docs.cloud.google.com/storage/docs/json_api/v1/parameters) | Content-Range, X-Upload-Content-Type/Length and final X-Goog-Meta-* |
| [Data validation](https://docs.cloud.google.com/storage/docs/data-validation#json-resumable-upload) | Final X-Goog-Hash validates the whole object |
| [JSON status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes) and [retry strategy](https://docs.cloud.google.com/storage/docs/retry-strategy) | Incomplete upload versus redirect, errors, retryable 408/429/5xx and transport failures |
| [JSON IAM](https://docs.cloud.google.com/storage/docs/access-control/iam-json) | storage.objects.create and overwrite storage.objects.delete |
| [Storage v2 proto](https://github.com/googleapis/googleapis/blob/d73dfb3c3be52245625b951b9c0bcf65163c549f/google/storage/v2/storage.proto) | Cross-check only: gRPC WriteObject/QueryWriteStatus is a separate transport, not the JSON wire contract |
| [Go Writer](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/writer.go) and [HTTP client](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/http_client.go) | Writer chunk selection, metadata, conditions, automatic CRC32C and context behavior |
| [API v0.307.0 resumable transport](https://github.com/googleapis/google-api-go-client/blob/v0.307.0/internal/gensupport/resumable.go), [media helper](https://github.com/googleapis/google-api-go-client/blob/v0.307.0/internal/gensupport/media.go) and [generated storage API](https://github.com/googleapis/google-api-go-client/blob/v0.307.0/storage/v1/storage-gen.go) | POST chunks, unknown totals, empty final request, 308 override and retry headers |
| [fake-gcs-server v1.56.1 upload handler](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/upload.go) | Comparison only; no Google authority or executed comparison claim |

Discovery revision `20261013` lists a resumable protocol path under
`/resumable/upload/storage/v1`, whereas the method guide and pinned generated
Go client use `/upload/storage/v1` with `uploadType=resumable`. The guide/SDK
path is required; the Discovery alias is tracked by SPEC-RSM-DISCOVERY-PATH.
storage-testbench is comparison-only and was not executed or used to resolve
the contract. Slice-start dependency results are recorded in
[the freshness log](../evidence/resumable-upload-dependencies.txt).

## Request decomposition

### Initiation

`POST /upload/storage/v1/b/{bucket}/o?uploadType=resumable` creates a session,
not an Object. Return **200**, an empty body and an absolute `Location` with an
opaque unguessable `upload_id`. Set `Cache-Control: no-store`; never log the
capability URI or token. An `X-GUploader-UploadID` response may expose the same
identifier to the caller, but provider diagnostic headers are not exact parity.

| Input | Supported subset and validation |
| --- | --- |
| bucket and name | Existing JSON validation and single decoding; path bucket and query name override string/null metadata values as on multipart. Preserve encoded names; never turn names into file paths |
| body and Content-Type | Empty metadata body or a JSON Object, at most 64 KiB; nonempty metadata uses application/json. Reuse fixed/custom metadata and MD5/CRC32C parsing, including the 8 KiB custom-metadata cap |
| X-Upload-Content-Type | Optional object MIME type, distinct from metadata request Content-Type; nonempty metadata contentType wins, then this header, then application/octet-stream. Conflict precedence is an explicit local rule |
| X-Upload-Content-Length | Optional nonnegative total, including zero; omission means unknown. Reject malformed/duplicate/overflow values and known totals above max_object_bytes before staging |
| ifGenerationMatch/NotMatch | Persist signed-64-bit nonnegative conditions, including match zero for create-if-absent; recheck at finalization |
| ifMetagenerationMatch/NotMatch | Persist existing positive signed-64-bit conditions and combine atomically with generation conditions at finalization |
| projection and contentEncoding | Preserve the existing scoped Object projection and stored encoding behavior; full does not invent ACL/owner fields or transcoding |
| Authorization | Strict local principal resolution and permission check at initiation; permissive bypass remains explicit |
| Origin | Accept and retain the documented client origin where needed; do not reject it as an unknown upload command or treat it as authorization. CORS response/preflight behavior remains deferred |
| X-Goog-Gcs-Idempotency-Token | Accept one nonempty token without ifGenerationMatch; empty/duplicate is 400. Retried initiation creates another bounded session without mutating object state. No initiation response replay; media/multipart retain their generation-match requirement |
| x-goog-api-client, User-Agent and tracing | Accept telemetry without altering object state or permissions |
| ACL, retention, contexts, encryption, billing, resource restrictions and unsupported metadata | Explicit existing unsupported-semantics errors; never accept a setting and silently ignore its effect |

Do not reject every unrelated unknown extension header. `x-goog-upload-*`
command/protocol variants are not specified by the checked JSON references or
used by this pinned Go transport: recognized unsupported command semantics
return 501 rather than an invented alternate protocol. Legacy unknown/output
metadata handling remains SPEC-015. Conflicting duplicate request fields fail
validation before session creation. Persist initiation conditions and metadata;
query parameters on subsequent writes cannot retarget or relax them.

### Chunk writes and status

The guide uses **PUT** to the returned URI. Also accept **POST** on that same
session URI for the pinned Go transport; distinguish it from initiation by
`upload_id`. The session binds bucket, name and bucket incarnation. A malformed
or unknown token never selects another object. No request body or capability
is routed through the XML adapter.

| Request | Required behavior |
| --- | --- |
| Content-Range: bytes start-end/N | Inclusive nonnegative offsets; body length equals end-start+1; N is consistent with prior known total and end is less than N |
| Content-Range: bytes start-end/* | Unknown total; accepted prefix remains incomplete until a final request supplies the total |
| Nonfinal chunk | A multiple of 256 KiB; final chunk may be smaller. Recommended 8 MiB is guidance, not a server minimum |
| Single complete PUT without Content-Range | At offset zero, stream the entire object with declared Content-Length, including zero bytes; complete EOF finalizes. After a persisted prefix, require Content-Range. Do not mistake metadata request Content-Length for object size |
| Content-Range: bytes */N, Content-Length: 0 | Query persisted offset when below N; finalize if the complete persisted prefix equals N, supporting the Go empty final request and an empty object at N=0 |
| Content-Range: bytes */*, Content-Length: 0 | Query without declaring a total; never finalize an unknown-length prefix |
| X-Goog-Hash on final request | Optional MD5/CRC32C of the entire object, not the final chunk. Merge with initiation metadata hashes; equal declarations accepted, conflicts/malformed/mismatch fail before publication |
| X-Goog-Meta-* on final request | Add/replace custom metadata keys from initiation as documented; apply the same final 8 KiB bound and header safety validation. Repeat these headers after an interrupted final request |
| X-GUploader-No-308: yes | Return HTTP 200 plus X-HTTP-Status-Code-Override: 308 instead of actual 308 for incomplete writes/status; preserve the same Range semantics |
| X-Goog-Gcs-Idempotency-Token on session writes | Accept one nonempty SDK per-chunk retry token without requiring a new query precondition. It neither selects the offset nor implements token-keyed response replay |
| Authorization on session writes/status/cancel | Capability authorizes access without re-authenticating the uploader; optional SDK bearer headers do not replace or redirect the session |

Return **308 Resume Incomplete** without `Location` redirection and with
`Range: bytes=0-last` for a nonempty persisted prefix. Omit Range when no bytes
are persisted. The next offset is last+1, never the number of bytes the client
merely attempted. Status responses have empty bodies. Distinguish 200 with the
override header from terminal 200 with an Object resource and no override.

Persisted bytes are immutable. Repeated or overlapping chunks drain/discard the
already persisted prefix using bounded buffers, append only the unseen tail,
and never overwrite acknowledged bytes, even when resent bytes differ.
A future offset with a gap, malformed/duplicate ranges, reversed/overflowing
offsets, conflicting totals, wrong body length or an invalid nonfinal chunk
return local 400 without publication. Serializing writers per session gives
one ordered append; a competing writer returns retryable local 503 rather
than racing on a file offset. Exact malformed/concurrent error parity remains
SPEC-RSM-EDGES. Different sessions may progress concurrently.

An interrupted request may leave no newly acknowledged bytes. The local
contract checkpoints a chunk only after complete transport EOF, file sync and
the short offset transaction. Truncate any uncommitted tail to the durable
offset before another writer or status result can acknowledge it. Never mark
a partial chunk complete because a producer/channel disappeared. Clients can
resend a chunk in full or query the persisted prefix first. Timeout/disconnect
does not cancel the whole session. Existing absolute request, inactivity,
catalog-wait and shutdown bounds apply per request, not to seven-day lifetime.

Retryable transport failures and 408/429/5xx require status/resume or a safe
chunk resend with bounded client backoff. 4xx checksum/permission failures
require a new session. No test may remove required SDK retry headers merely
to get a success response.

### Finalization and permissions

Strict initiation requires `storage.objects.create` in the actual owning
project/bucket, plus `storage.objects.delete` when replacing an existing live
object. Unsupported retention/context settings are rejected; do not claim the
additional permissions or effects are implemented. Snapshot the initiating
local identity/capability context, never its bearer credential in logs.

The session URI itself is the upload authentication capability: an anonymous
delegate can write, query and cancel it. The performing guide and concept
page explicitly permit upload without further authentication. This does not
authorize unrelated bucket/object operations. At publication, re-evaluate the
initiating authorization context against current bucket grants and require
overwrite-delete if a live destination now exists, in the same transaction as
all four preconditions. Do not authenticate the delegate again. A revoked
grant or deleted/recreated bucket cannot publish through stale authority.
The performing guide warns that changed bucket permissions can cause a
nonretryable 4xx; exact recheck timing/order is SPEC-RSM-AUTH-CONDITIONS.

Finalization validates complete length and all whole-object checksums, then
atomically allocates generation, sets metageneration=1, publishes bytes and
metadata, marks the session complete and journals the replaced blob.
Creation/update times use completion time from the injected clock. GET/list
cannot observe session bytes or initiation metadata beforehand. Readers of an
old immutable generation may finish; subsequent reads select the committed
generation. Failed checksum, condition, cap, permission or storage work must
preserve the old bytes, generation, metadata and hashes.

Return local **200** with the existing JSON Object representation, including
decimal-string generation/metageneration/size, base64 MD5/CRC32C, fixed/custom
metadata and resource links. The guide permits 200 or 201; 200 matches the
pinned SDK path and existing insertion behavior. A completed session retains
a bounded final-resource snapshot until its original expiry, so status and
repeated final requests return the original completion without creating a new
generation, even after a later overwrite. This session completion record is
not SPEC-013 invocation-token replay. Validation/condition failure terminalizes
the local session and requires a new initiation; exact provider retention of
failed sessions is SPEC-RSM-EDGES.

## Cancellation, expiry and maintenance

`DELETE SESSION_URI` with an empty body cancels an incomplete session and
returns JSON-upload **499**. Do not borrow XML cancellation's 204. Further
status/write/cancel requests for a terminal invalid session return local
**410** before seven days from initiation and **404** at or after that deadline;
an unknown token returns 404. Cancellation changes no live object. Serialize
cancel/finalize races: exactly one terminal transition wins, and cancel cannot
delete a completed object. Completed-session DELETE returns local 410.

Lifetime is **604800 seconds from initiation**, never sliding on activity.
Use `Clock::now()` and a persisted absolute deadline. At now >= deadline the
session is unusable even if physical cleanup has not run. This exact-boundary
choice and cancellation/completion error bodies remain SPEC-RSM-EDGES.

Provide an explicit maintenance entry point driven by the injected clock and
one next-deadline scheduler. Wake on earlier inserted deadlines or shutdown;
sleep until the next due deadline, with no periodic busy polling and no timer
when there is no work. Tests advance the clock and invoke maintenance without
waiting a week. Expire in bounded indexed batches, remove staged bytes, and
reclaim terminal records at their original deadline. A failed unlink is
journaled/retried and surfaced, never silently marked physically reclaimed.
Committed cancellation still returns 499 when physical cleanup fails; later
requests return 410 before the original deadline while maintenance retries cleanup.
Likewise, a committed terminal validation failure preserves its original error
response if cleanup fails; diagnostics and the deletion journal retain that
cleanup failure for maintenance retry. Successfully cleaned cancelled sessions
retain their 410 tombstone without repeating physical cleanup before expiry.
Restart runs due cleanup before readiness and rearms the next deadline.

## Client-reachable Location

Use the validated, normalized **configured `api_base_url`**, followed by the
JSON upload path and encoded session identifier. `src/config.rs` and
`src/url.rs::normalize_base_url` currently accept absolute HTTP(S), reject
credentials/query/fragment and trim trailing slashes; preserve these semantics
and any configured proxy prefix. The proxy must strip that prefix before
forwarding. Never derive Location from the bind address, raw Host, Forwarded
or X-Forwarded-* headers. Public-object URL configuration grants no permission
and does not choose this capability endpoint.

| Initiating client | Required advertised configuration and observation |
| --- | --- |
| Host CLI / SDK | `api_base_url=http://127.0.0.1:PORT`; returned URI uploads from the host |
| Compose peer | `api_base_url=http://gcs:4443`; the peer follows the returned URI from its own network |
| Host browser | `api_base_url=http://localhost:PORT`; address is browser reachable, with CORS support separately pending |
| Reverse proxy | Explicit externally reachable HTTP(S) base/prefix; preserve it exactly and ignore spoofed forwarding headers |

The broader design/onboarding proposal mentions `validated_request_origin`
with an allowed-authority list. Neither mode nor allowlist exists in current
configuration. This slice freezes the available fixed override, rather than
trusting Host implicitly. One fixed origin cannot automatically serve both
host and peer address spaces: configure a common reachable DNS/proxy endpoint
or run the host/peer journeys with separate advertised configurations. The
remaining automatic multi-origin design is SPEC-RSM-LOCATION.

## Bounded resources and catalog recovery

Use the existing disk-backed catalog/blob mechanism, not a process-only map.
The planned local session cap is **128 retained sessions**, including active,
completed and invalid terminal records before their deadline. Admission is an
atomic count/insert with immediate **503 backendError** when full; rejected
initiation creates no files or object. The HTTP `max_connections` setting is
the only API upload concurrency limit; catalog work uses 32 admission slots
and one executing SQLite job. Upload ownership lasts through cleanup.
One session has at most one active writer, with no
unbounded per-session waiter queue.

Keep at most two copied 64 KiB channel chunks per admitted body, plus bounded
hash/parser state; never allocate a chunk-sized or object-sized byte vector.
Object cap is existing `max_object_bytes` (default 1 GiB, configurable through
5 TiB). Apply it to declared total, incoming offsets and actual streamed bytes,
including unknown-length uploads, using checked arithmetic. Rejected admission,
length and unsupported requests do not eagerly consume media. Disk usage is
bounded by retained sessions times object cap, plus in-progress tails bounded
by `max_connections` times that same cap and existing immutable live blobs.
This is not a global free-space reservation. Disk-full/write/sync errors fail
explicitly.

Add a transactional migration from the existing validated catalog schema to
an indexed session table: random capability ID, opaque staging/blob ID,
bucket incarnation/owner, logical name, initiating authorization context,
bounded metadata/checksum/condition fields, optional total, committed offset,
created/deadline timestamps, state and bounded completion resource. Index
deadline and opaque blob references. Keep session fields out of ordinary
object listing. Validate migrations and session invariants before readiness;
reject future/corrupt schemas read-only. No body transfer holds a transaction.

The SQL transaction and filesystem rename are **not one atomic operation**:

1. Allocate an opaque staging file on the data volume; insert its session
   after durable file creation. Unreferenced files from a crash are orphans.
2. For each complete request, append beyond the last committed offset, sync
   the file, then commit offset/total. A crash before offset commit leaves a
   removable tail; a crash after commit must preserve every acknowledged byte.
3. Finalization hashes by bounded disk reads if resumed state needs rebuilding,
   syncs the file, publishes it under its opaque immutable blob ID by the
   existing same-filesystem rename/directory-sync protocol, then commits Object
   and COMPLETE session snapshot together with conditions and deletion journal.
4. After a precommit crash, an ACTIVE session whose staging path is missing but
   whose recorded immutable blob exists is restored to staging before reuse.
   Its durable offset, metadata and total remain authoritative. An ambiguous
   SQL commit is resolved by session/object reference lookup before any cleanup.
5. After commit, the live reference and completed session must survive. Never
   move/delete a committed blob as abandoned staging. Reclaim replaced blobs
   through the existing deletion journal; failure does not undo publication.

Recovery must replace the current blanket staging purge with reference-aware
handling. Under the exclusive data-directory lock, page sessions and blobs;
retain active unexpired staging, truncate excess tails, recover precommit
renames, remove unreferenced/expired files and validate required file lengths.
A missing/truncated acknowledged prefix blocks readiness with a diagnostic;
do not reset its offset and report success. No object-catalog collection or
per-session whole-body read is permitted. Selected process-crash/restart
barriers are required; individual fsync windows, power loss and every platform
are separate unverified durability boundaries.

## Acceptance cases

All cases are planned/PENDING. Types: positive, negative, boundary, permission,
concurrency, restart and resource. IDs describe stable behavior, not review or
work-tracking numbers. Shared case references on parent rows are contributions,
not additional independent features.

| Feature | Case ID | Type | Required observation |
| --- | --- | --- | --- |
| BEHAVIOR-OBJECT-010 | RSM-INIT-001 | positive | POST metadata or empty initiation returns 200, empty body, no-store and opaque absolute Location; no Object yet |
| BEHAVIOR-OBJECT-010 | RSM-INIT-002 | positive/boundary | Query/path name precedence and encoded Unicode/slash/percent names; fixed/custom metadata and content type survive finalization |
| BEHAVIOR-OBJECT-010 | RSM-INIT-003 | negative/boundary | Malformed/duplicate fields, metadata/custom cap and known oversized/overflow total fail without session/file mutation |
| BEHAVIOR-OBJECT-010 | RSM-INIT-004 | permission | Strict anonymous/invalid/insufficient permission denial; creator can initiate new object but cannot overwrite; no body staging on rejection |
| BEHAVIOR-OBJECT-010 | RSM-INIT-005 | resource/concurrency | 128 retained session cap enforced atomically, next initiation 503; rejected initiation leaks no bytes/permits |
| BEHAVIOR-OBJECT-011 | RSM-CHUNK-001 | positive/boundary | PUT sequential 256 KiB-multiple chunks and smaller last chunk produce exact binary bytes and whole hashes |
| BEHAVIOR-OBJECT-011 | RSM-CHUNK-002 | positive/boundary | Unknown total uses bytes start-end/*, learns total only consistently and completes with numeric final total |
| BEHAVIOR-OBJECT-011 | RSM-CHUNK-003 | positive/boundary | Single no-range upload, empty object and exact-multiple empty final request finish correctly |
| BEHAVIOR-OBJECT-011 | RSM-CHUNK-004 | negative | Gaps, reversed/overflow/duplicate ranges, inconsistent totals and invalid nonfinal size are rejected without corrupting prefix |
| BEHAVIOR-OBJECT-011 | RSM-CHUNK-005 | positive/boundary | Complete duplicate and partial overlap ignore persisted bytes, append only tail and never change already acknowledged bytes |
| BEHAVIOR-OBJECT-011 | RSM-CHUNK-006 | negative/restart | Interrupted/wrong-length body never acknowledges uncommitted bytes; truncate tail and resend/status recover exact prefix |
| BEHAVIOR-OBJECT-011 | RSM-CHUNK-007 | concurrency | Two writers on one session serialize/reject safely; separate sessions progress; all permits released after timeout/disconnect |
| BEHAVIOR-OBJECT-011 | RSM-CHUNK-008 | positive | SDK POST with no-308 header gets HTTP 200 plus override 308 and correct Range; final response removes override |
| BEHAVIOR-OBJECT-011 | RSM-CHUNK-009 | resource/boundary | Exact object cap succeeds; cap+1 known/unknown/offset stream fails 413; large/slow upload uses bounded server buffers |
| BEHAVIOR-OBJECT-012 | RSM-STATUS-001 | positive/boundary | Empty PUT `bytes */N` (N>0) and `bytes */*` before data return 308 with no Range and no object; `bytes */0` completes the empty object |
| BEHAVIOR-OBJECT-012 | RSM-STATUS-002 | positive | Persisted prefix returns Range bytes=0-last, empty body and same offset across repeated queries/restart |
| BEHAVIOR-OBJECT-012 | RSM-STATUS-003 | positive | Completed status returns 200 Object snapshot even after later overwrite, never allocates a new generation |
| BEHAVIOR-OBJECT-012 | RSM-STATUS-004 | negative | Invalid token/total/range or nonempty status body cannot mutate destination or report false completion |
| BEHAVIOR-OBJECT-013 | RSM-CANCEL-001 | positive/permission | Anonymous delegate DELETE cancels active session with 499, removes staging and preserves old object |
| BEHAVIOR-OBJECT-013 | RSM-CANCEL-002 | negative/boundary | Cancelled URI gives 410 before original deadline, 404 at/after deadline; unknown URI is 404 |
| BEHAVIOR-OBJECT-013 | RSM-CANCEL-003 | concurrency | Cancel racing append/finalize has one terminal winner; completed-object bytes are never deleted |
| BEHAVIOR-OBJECT-013 | RSM-EXPIRY-001 | boundary | Injected clock at deadline-1, deadline, deadline+1 verifies fixed 604800-second lifetime without sliding renewal |
| BEHAVIOR-OBJECT-013 | RSM-EXPIRY-002 | resource/restart | Explicit due maintenance and startup cleanup release staged bytes/records; idle scheduler has no polling loop |
| BEHAVIOR-OBJECT-013 | RSM-EXPIRY-003 | negative/resource | Failed cleanup remains visible/retryable; no leaked slot or falsely reported physical reclamation |
| BEHAVIOR-OBJECT-014 | RSM-FINAL-001 | positive | Final 200 Object has exact bytes, hashes, metadata, size and new generation; timestamps reflect completion |
| BEHAVIOR-OBJECT-014 | RSM-FINAL-002 | positive/negative | Before completion GET/list show no new object and retain the old overwrite; final EOF changes visibility atomically |
| BEHAVIOR-OBJECT-014 | RSM-FINAL-003 | negative/boundary | Metadata/final X-Goog-Hash MD5/CRC32C success, malformed/conflicting/wrong whole hash and final metadata cap preserve old full resource |
| BEHAVIOR-OBJECT-014 | RSM-FINAL-004 | positive/negative | All four initiation conditions persist; changes during upload fail atomic finalization with 412; chunk query cannot relax them |
| BEHAVIOR-OBJECT-014 | RSM-FINAL-005 | concurrency | Two create-if-absent sessions have exactly one winner; overwrite/metadata/delete races obey selected final conditions |
| BEHAVIOR-OBJECT-014 | RSM-FINAL-006 | permission/concurrency | URI delegate needs no re-auth; revoked initiator grant, new overwrite without delete and bucket recreation cannot bypass final permission checks |
| BEHAVIOR-OBJECT-014 | RSM-FINAL-007 | positive/concurrency | Repeated final request after lost response returns same completed resource without second generation; independent of invocation-token replay |
| BEHAVIOR-OBJECT-014 | RSM-FINAL-008 | positive/boundary | Final X-Goog-Meta-* augments/overrides initiation custom metadata and survives restart with final byte/hash metadata |
| BEHAVIOR-OBJECT-014 | RSM-RECOVERY-001 | restart | Kill after file write/before offset commit: truncate tail; after offset commit: preserve acknowledged prefix and resume |
| BEHAVIOR-OBJECT-014 | RSM-RECOVERY-002 | restart | Kill after rename/before Object commit: restore active staging; kill after commit: preserve completed object/snapshot |
| BEHAVIOR-OBJECT-014 | RSM-RECOVERY-003 | negative/restart | Missing/truncated referenced prefix blocks readiness; orphan, expired, deferred-delete and multi-page recovery stay bounded |
| DESIGN-057 | RSM-LOC-001 | positive | Host CLI/SDK follows configured host Location and completes upload |
| DESIGN-057 | RSM-LOC-002 | positive | Compose peer follows peer-reachable advertised Location; browser-address case uses host-reachable endpoint without a CORS claim |
| DESIGN-057 | RSM-LOC-003 | negative/boundary | Spoofed Host/forwarding cannot change Location; trailing slash/proxy prefix preserved; invalid advertised URLs fail config |
| DESIGN-082 | RSM-SDK-GO-001 | positive | Default 16 MiB ChunkSize Writer with object larger than threshold performs real resumable POSTs and verifies Attrs/download/checksum |
| DESIGN-082 | RSM-SDK-GO-002 | positive/boundary | 256 KiB ChunkSize Writer spans several chunks including exact multiple, short final, binary and empty paths; metadata and explicit MD5/CRC32C verified |
| DESIGN-082 | RSM-SDK-GO-003 | negative/positive | Controlled transport interruption/lost chunk response plus configured safe retry exercises duplicate chunk; eventual exact bytes and one generation |
| DESIGN-082 | RSM-SDK-GO-004 | negative/resource | Cancel Writer context during transfer: Write/Close fails, no partial object, server body permit released; session remains until explicit cancel/expiry |
| DESIGN-082 | RSM-SDK-GO-005 | permission/concurrency | DoesNotExist, GenerationMatch and MetagenerationMatch success/failure plus strict creator overwrite race use normal nonzero ChunkSize |
| JSON-OBJECTS-007 | RSM-INSERT-001 | positive/negative | Initiation/chunk/status/cancel/finalization subset passes with existing media/multipart regression coverage; advanced fields and replay remain pending |

SDK cases must assert actual resumable requests, not merely a successful Writer
that silently chose multipart for a small object. Keep ordinary small/empty
Writer journeys too, since the pinned helper chooses multipart when the whole
body fits its first buffer. Client ChunkSize buffers are separate from server
memory. Pin Go 1.27.3, Storage v1.70.0 and API v0.307.0; use isolated endpoints,
fixture identities, cleared ADC and no metadata-server/cloud calls. Neither
ChunkSize=0 nor disabling automatic checksum is an acceptable workaround.

## Completion boundary and open questions

Record executed wire/catalog/SDK/resource/restart commands and artifacts before
promoting any row. CASE completion does not close broader JSON insert or Go
integration requirements. Source questions/local choices are recorded in
SPEC-RSM-DISCOVERY-PATH, SPEC-RSM-EDGES, SPEC-RSM-AUTH-CONDITIONS and
SPEC-RSM-LOCATION. SPEC-013 response replay remains separate and OPEN.
No conformance, performance, container build or real-cloud result is asserted
by this contract document.
