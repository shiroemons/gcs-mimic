# Object rewrite and continuation sessions

<!-- markdownlint-configure-file {"MD013": false} -->

Contract checked on **2026-10-23** for `JSON-OBJECTS-012` and
`BEHAVIOR-OBJECT-024`. This document defines acceptance, not completed evidence.
The [feature ledger](../../../compatibility/features.json) alone owns status.
The [frozen basic JSON profile](basic-json-contract.md) remains the baseline.
Storage v2 RPC, batch rewrite and enrichment rewrite are outside this slice;
`RPC-STORAGE-021`, `EXT-BEH-019` and `EXT-BEH-026` receive no implementation claim.

## Sources and pins

The following official sources were re-fetched on **2026-10-23**:

- [objects.rewrite](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/rewrite),
  [copy](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/copy) and
  [compose](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/compose).
- [Object resource](https://docs.cloud.google.com/storage/docs/json_api/v1/objects),
  [request preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions),
  [data validation](https://docs.cloud.google.com/storage/docs/data-validation),
  [composite objects](https://docs.cloud.google.com/storage/docs/composite-objects),
  [status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes)
  and [common parameters](https://docs.cloud.google.com/storage/docs/json_api/v1/parameters).
- [Discovery storage v1](https://storage.googleapis.com/$discovery/rest?version=v1):
  revision `20261013`, SHA-256
  `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`.
- [Storage v2 proto](https://github.com/googleapis/googleapis/blob/d73dfb3c3be52245625b951b9c0bcf65163c549f/google/storage/v2/storage.proto):
  commit `d73dfb3c3be52245625b951b9c0bcf65163c549f`, SHA-256
  `a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf`.

The raw Discovery/proto digests match the basic-profile pins. RPC declarations
are cross-checks, not authority for JSON errors or a claim of RPC implementation.
Comparison emulators do not override the official contract. No real-cloud
operation is authorized or required by this slice.

## Exact selected subset

The target is flat-namespace ordinary immutable objects, native and existing
legacy profiles, persisted catalog/blob storage, existing supported storage
classes, versioning/event holds and strict local grants. Supported metadata
and publication rules come from the derived-object copy foundation.
There is no emulated network transfer, regional durability, replication, KMS,
ACL enforcement, retention, soft deletion, appendable object or enrichment.
Recognized inputs needing those unavailable semantics fail 501; malformed
values fail 400. Broader API parity remains explicitly pending.

## Request acceptance

`POST /storage/v1/b/{sourceBucket}/o/{sourceObject}/rewriteTo/b/{destinationBucket}/o/{destinationObject}`
splits raw segments before decoding once, including encoded slashes, Unicode,
literal plus and percent signs. Bucket/object validation, 16 KiB query cap,
duplicate-query rejection, 64 KiB metadata cap and unsupported write HTTP
validators extend the frozen profile. Other methods return 405 with `Allow: POST`.

| Input | Acceptance cases and boundary |
| --- | --- |
| Four path names | Existing source and destination buckets required; destination path overrides metadata `name` and `bucket`. Wrong source/destination paths cannot reuse a token. Empty, overlong or malformed names fail without mutation. |
| `rewriteToken` | Initial request omits it. Every continuation uses a returned opaque token. Malformed and request-mismatched tokens fail 400. Unknown well-formed, altered and expired keys fail 410; this local unavailable-token rule permits bounded cleanup without permanent token tombstones. Continuation may omit optional fields/body; supplied values must equal the original request. |
| `maxBytesRewrittenPerCall` | Positive signed int64, minimum 1048576 and divisible by 1048576. Reject zero, negative, fractional, overflow and nonmultiple values with 400. The official text establishes MiB granularity; the positive minimum is the explicit local interpretation. Keep its value fixed throughout a session. Apply it only across different locations or storage classes. Same-location/class operations complete in one call even with a small specified maximum. |
| `sourceGeneration` | Optional positive generation selects a retained revision; omission selects the live revision initially. Persist the selected generation independently of request omission. Never switch to replacement bytes during continuation. Selected missing/deleted generations fail cleanly, without destination publication. |
| Source conditions | `ifSourceGenerationMatch`, `ifSourceGenerationNotMatch`, `ifSourceMetagenerationMatch`, `ifSourceMetagenerationNotMatch`; parse like the existing object conditions and compare with the selected source. Retain all four in the session. A failing mutation condition is locally 412 `conditionNotMet`; general NotMatch 304 wording remains the existing SPEC-008 gap. |
| Destination conditions | `ifGenerationMatch`, `ifGenerationNotMatch`, `ifMetagenerationMatch`, `ifMetagenerationNotMatch`; preserve the basic-profile combinations. Match zero is create-if-absent; NotMatch zero requires a live destination. The transaction publishing the sole destination generation rechecks all conditions. Failed and racing writes preserve the winning destination. |
| `destinationKmsKeyName` | Recognized but unavailable: 501; no fake encryption metadata success. |
| `destinationPredefinedAcl` | Recognize all six documented enum values; valid values fail 501 because ACLs are unavailable, invalid enum fails 400. No copied source ACL or new public grant. |
| `projection` | Default `noAcl`; accept `noAcl`/`full` under the frozen local profile, reject other values. Full owner/ACL fields and their extra permission parity remain pending. |
| `userProject` | Recognized requester-pays billing input fails 501. |
| `fields` | Validate against the rewrite response schema; nested `resource(...)` uses Object fields. Filter the final response only, without changing byte counts, publication or stored completion. Invalid selectors fail before side effects. Errors remain unfiltered. A continuation cannot replace an initially supplied selector with another value. |
| `dropContextGroups` | Recognized but unavailable: 501, including `custom`; no accepted-and-ignored context deletion. |
| Common queries | `alt=json`, compact output and `prettyPrint=false` extend existing parsing. Other common queries use existing 400/501 distinctions. No query bearer tokens, quota behavior or JSONP. |
| Headers | Strict authentication on every rewrite request. Reject unsupported source/destination CSEK headers, KMS/billing headers, write ETag/date validators and method override consistently with the existing profile. No media body or upload-checksum header reinterpretation. |

## Metadata acceptance

An absent body copies supported editable metadata from the source, excluding
source ACLs, holds and retention. Destination bucket defaults still apply.
A supplied Object body constructs destination metadata using the copy
foundation's declared omission/null rules. Cover both absent body and `{}`;
they must not be conflated. Fixed content fields, custom metadata, supported
storageClass and eventBasedHold have real effects. Destination generation,
metageneration and timestamps are fresh, while size/MD5/CRC32C describe the
streamed source bytes. Client-provided checksums, if supported by the shared
foundation, must be validated before publication, never echoed unverified.
Path identity overrides body identity. Unknown/output-only body handling follows
the copy contract and the basic-profile source question; no blanket new policy.

ACL/owner, contexts, retention, temporaryHold, customTime and encryption semantics
remain explicit unsupported cases. Composite object component-count changes
are owned by the compose slice; this slice must not invent a component count
for an ordinary object or claim composite rewriting before shared support exists.
An active source event hold permits reading; a protected destination cannot
be replaced. Destination versioning archives its old live generation exactly
once. Rewriting onto the same name must retain the original pinned bytes and
avoid deadlock or early source reclamation.

## Response and permissions acceptance

Every successful unfiltered response is HTTP 200 JSON with
`kind: storage#rewriteResponse`, string `totalBytesRewritten`, string `objectSize`
and boolean `done`. Byte counts are monotonic, bounded by objectSize and equal
at completion. While incomplete, include `rewriteToken` and omit `resource`;
when complete, omit `rewriteToken` and include the destination Object resource.
Empty and exact-chunk-boundary objects finish without a phantom extra step.
GET/list cannot observe a partial destination. A completed response corresponds
to exactly one durable destination generation.

Strict mode requires source `storage.objects.get` and destination
`storage.objects.create`; overwriting also needs destination
`storage.objects.delete`. Combine project and bucket grants using the existing
authorization helpers, then recheck permissions and bucket incarnations in
publication. A continuation is not a resumable-upload bearer capability: each
call authenticates. Token possession cannot bypass revoked grants or recreated
buckets. Unsupported retention/context inputs do not imply implemented
`setRetention`, `createContext` or `dropContexts` permissions. Cover permitted
cross-bucket grants, missing source read, missing destination create,
creator-only overwrite denial, anonymous denial, revocation and bucket recreation.

Errors use the existing JSON envelope: malformed/mismatched token or request
400, invalid credentials 401, missing grants/protected destination 403,
authorized missing source/bucket 404, unavailable/expired session 410, changed pinned source
or failed conditions 412, configured size cap 413, unsupported semantics 501,
storage/admission failure 503. Exact provider token errors, error ordering and
source-change status are open source questions; these are named local choices.

## Persisted state and bounded recovery design

The catalog stores one session per rewrite with: random session ID, selected
source bucket/object/generation/blob ID and bucket incarnation, destination path
and incarnation, normalized initial request parameters and raw metadata body,
effective destination metadata, staged opaque blob ID, acknowledged byte
offset, object size, partial checksum state/checkpoint, creation/deadline,
state and completion resource. Store no secret material in the token.

`rewrite_token.rs` uses a 256-bit random hex token as a database lookup key.
Only an exact persisted token authenticates a checkpoint; syntactic decoding
alone never authorizes a request. Bind each token to its complete saved request
and selected immutable generation. Altered random keys cannot select the
original session; no home-grown MAC or weak hash substitutes for randomness.
Optional continuation inputs are compared as a subset of the saved inputs,
without overwriting saved conditions. The stored request remains authoritative.

State transitions are `ACTIVE -> ACTIVE` after a durable prefix checkpoint,
`ACTIVE -> COMPLETE` atomically with destination publication, and
`ACTIVE -> FAILED` for a terminal source/condition failure. Sessions expire
after a fixed 604800 seconds from creation, including at deadline equality;
calls do not renew the deadline. Limit all retained sessions to 128 and use the
existing eight-stream admission budget and exclusive per-session writer guard.
Admission and cleanup errors stay visible and never drop existing data.

Each call opens the pinned source revision and copies at most the permitted
progress using a 64 KiB buffer. No object-size allocation or catalog transaction
spans file I/O. Flush/sync the staged prefix before committing offset/checksums
and the next token. Persist enough checksum state to continue, or rebuild it
from the acknowledged prefix with bounded reads after restart; never serialize
an undocumented Rust hasher memory layout. Check prefix hashes against saved
values before using a recovered stage. A crash after writing but before SQL
checkpoint truncates back to the acknowledged offset. Missing, truncated or
corrupt acknowledged prefixes cleanly invalidate the session without publishing
a destination. A source revision changed or removed between calls must never
silently switch to the new revision, including unversioned overwrite/deletion.

For completion, use the shared staged-blob sync/rename protocol before a short
immediate SQL transaction rechecks source identity/conditions, destination
conditions/holds and strict grants, publishes one reference and commits the
completion record. An ambiguous commit requires a reference/completion lookup
before cleanup. A rename-before-commit crash restores the ACTIVE stage; a
committed completion must never be reclaimed with the session.

Keep the previous input token and its saved output alongside the current token:
retrying that token returns the same progress/completion without another copy
or generation. Checkpoint and replay record commit together. The terminal
token replay returns its original completion even after a later destination
write, provided authorization remains valid. Initial requests without a token
are distinct operations; response replay is not inferred from invocation IDs.
Retry retention beyond the immediately previous checkpoint remains pending
unless separately implemented and tested; no unbounded token history.

Extend the existing injected-clock/next-deadline maintenance scheduler. Query
bounded pages, wake on session creation/checkpoint/expiry, and sleep only until
the next deadline; no periodic scan when idle. Journal failed physical cleanup
with pending blob deletions and retry on a bounded deadline. Recovery runs
before generic orphan scans, so active stages survive restart. Completed
destination blobs remain protected by catalog references.

## Required verification and unresolved questions

Wire/storage tests must cover one-call, empty, same-name and cross-bucket
rewrite, multi-call progress with a 1 MiB maximum across location/class,
exact boundaries, token-only continuation, changed optional inputs, every
token character altered, metadata and fields, all eight conditions, strict
allow/deny/revocation, holds/versioning and racing create-if-absent publication.
Restart after a partial checkpoint must complete with identical bytes/hashes;
source overwrite/delete must terminate or consistently select a retained
requested generation. Test staged suffix truncation, missing/corrupt prefixes,
expiry, bounded admission, duplicate/concurrent token retry and post-completion
retry without a new generation. Resource evidence must demonstrate bounded
buffering; source inspection alone proves no RSS/cgroup claim.

Pinned Go `storage` Copier must exercise ordinary one-call rewrite and
`ProgressFunc` across location/class with small `maxBytesRewrittenPerCall`,
plus destination conditions and metadata. Run targeted tests, fmt, clippy and
full Rust tests during implementation. Run final containers/full SDK/guide only
after the coordinator declares source final. Record only executed results in
one final evidence set, and leave broader rows PARTIAL where cases remain.

Open questions to reconcile in the ledger before promotion: exact minimum/zero
maxBytes behavior, token error reasons, historical-token replay/lifetime after
completion, source mutation/deletion with omitted versus explicit generation,
source permission/precondition recheck timing, body omission/null details and
mixed-error precedence. The frozen SPEC-008/012/013/017 boundaries remain intact.
