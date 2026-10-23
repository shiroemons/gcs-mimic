# JSON object compose contract

<!-- markdownlint-configure-file {"MD013": false} -->

This slice extends the [frozen basic JSON profile](basic-json-contract.md) with
`JSON-OBJECTS-002`, `BEHAVIOR-OBJECT-025`, `BEHAVIOR-OBJECT-026` and
`BEHAVIOR-OBJECT-027`. Storage v2 `RPC-STORAGE-010` and XML `EXT-XML-025` are
outside this slice. Acceptance targets below are not passing evidence by
themselves; implementation status belongs only to `compatibility/features.json`.

## Source contract checked on 2026-10-23

| Official source | Authority and reconciliation |
| --- | --- |
| [objects.compose](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/compose) | JSON path, body, query, source conditions, permissions, owner/ACL omission and hard-delete flag |
| [Composite objects](https://docs.cloud.google.com/storage/docs/composite-objects) | Source order, same bucket/class, nested counts, MD5 absence, opaque ETag and CRC32C combination; supplied JSON destination MD5/CRC32C are ignored |
| [Compose objects](https://docs.cloud.google.com/storage/docs/composing-objects) | Source hard deletion bypasses Object Versioning and soft delete; CLI MIME inheritance is not a JSON API default |
| [Quotas and limits](https://docs.cloud.google.com/storage/quotas) | 32 sources per request; unlimited cumulative components, count saturation at 2,147,483,647 and maximum object size 5 TiB |
| [Object resource](https://docs.cloud.google.com/storage/docs/json_api/v1/objects) | Writable content/custom metadata, resource identity, storage class and componentCount representation |
| [Request preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions) | Source generation matching and destination atomic compare-and-swap, including create-if-absent zero |
| [Data validation](https://docs.cloud.google.com/storage/docs/data-validation), [status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes) | Integrity and JSON error envelopes; no inferred provider error precedence |
| [XML response headers](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers#xgoogcomponentcount) | XML GET/HEAD return x-goog-component-count only for composites; JSON's common-header reference does not document this header, so JSON exposes the resource field without adding the XML header |

Raw [Discovery storage v1](https://storage.googleapis.com/$discovery/rest?version=v1)
was re-fetched: revision `20261013`, SHA-256
`1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`.
The re-fetched [storage.proto](https://github.com/googleapis/googleapis/blob/d73dfb3c3be52245625b951b9c0bcf65163c549f/google/storage/v2/storage.proto)
at commit `d73dfb3c3be52245625b951b9c0bcf65163c549f` has SHA-256
`a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf`.
Both match the frozen basic-profile pins. JSON `ComposeRequest` includes
`deleteSourceObjects`; it is not a gRPC-only option. Proto has
`delete_source_objects` and a separate `object_checksums` field; its checksum
validation does not override the JSON guide's ignored destination checksum rule.
The pinned Go SDK's `Composer.SendCRC32C` comment describes validation, conflicting
with that JSON guide. Keep this provider-contract question explicit rather than
using SDK comments to silently change the JSON behavior.

The previously requested **1024 cumulative component limit is obsolete** under
the official quotas checked on 2026-10-23. This slice does not enforce it.
`componentCount` is the saturating sum of each source's component count, counting
ordinary objects as one. The same object listed twice contributes twice.

## Supported request and metadata subset

| Surface | Acceptance contract |
| --- | --- |
| Path and method | POST `/storage/v1/b/{destinationBucket}/o/{destinationObject}/compose`; decode each path component exactly once after routing; use existing bucket/profile and 1..1024-byte object-name rules; wrong method returns 405 and Allow: POST |
| Request body | JSON object capped at 64 KiB; complete EOF required before staging; malformed JSON 400 parseError, non-object/invalid typed fields 400; no object payload in this body |
| sourceObjects | Required array with 1..32 entries, each an object with a valid string name; order and duplicates preserved; no URL expansion, wildcard expansion, cross-bucket field or catalog-wide listing |
| Source generation | Optional nonnegative int64 decimal string or JSON integer; omitted selects live generation; explicit generation selects retained or live generation; resolve and pin all selected source generations in one request-start snapshot |
| objectPreconditions.ifGenerationMatch | Optional nonnegative int64; compare with the selected source generation, including when generation is explicit; mismatch, including a differing explicit generation, returns 412 without publishing |
| Source bucket/class | Every lookup is scoped to destinationBucket; source logical names can contain slashes, Unicode and URL-looking text; all source storage classes must agree, and the resulting class is the source class |
| Destination | Omitted or empty object creates with local default metadata; matching kind/name/bucket may be supplied; mismatching path identity returns local 400; other output-only/unknown fields fail with local 501 under the frozen unknown-input boundary |
| Fixed content fields | contentType, contentEncoding, contentDisposition, contentLanguage and cacheControl use the existing validated metadata parser; valid explicit values are preserved; null clears and omitted/empty contentType serves application/octet-stream; do not inherit the first source's contentType merely because gcloud does |
| Custom metadata | Fresh destination metadata map, no implicit source metadata inheritance; final key/value UTF-8 total at most 8192 bytes; existing empty-key, header-control and non-ASCII fixed-field restrictions apply |
| Holds | Explicit eventBasedHold supported; omission uses destination bucket defaultEventBasedHold; source holds do not block non-destructive reads; held live destinations cannot be overwritten |
| Supplied hashes | Ignore destination md5Hash and crc32c as the JSON composite-object guide directs; this exception does not weaken upload hash validation; RPC object_checksums remains out of scope |
| kind | Omitted or storage#composeRequest; other values return local 400 |
| deleteSourceObjects | Omitted/false leaves all source generations intact; true hard-deletes pinned source generations in the destination publication transaction under the local safety rules below; null/non-boolean returns 400 |
| ifGenerationMatch | Destination match condition, including zero for no live destination; authoritative check in the publication transaction; one winner under concurrent create-if-absent |
| ifMetagenerationMatch | Destination live metadata match; authoritative check in the same transaction; cannot silently ignore a stale revision |
| fields | Existing Object selector validates before mutation; filters successful Object only; errors remain complete; the pinned schema already permits componentCount |
| projection | noAcl/full both render the supported metadata subset, omitting owner/ACL as required by compose; full-projection additional permission parity stays pending on the broad method |
| Unsupported queries | destinationPredefinedAcl, kmsKeyName, userProject, dropContextGroups, unsupported condition variants and recognized unsupported common query semantics return explicit 501; malformed or duplicated supported parameters return 400 according to shared query rules |
| Unsupported destination features | ACL/owner, contexts, customTime, customerEncryption/KMS, temporaryHold, retention, soft-delete fields and any explicit storageClass input return 501; they never silently alter or grant permissions |
| Headers | Existing authentication, transport, telemetry and bounded invocation-token rules; customer encryption, billing, copy-source encryption, method override, write ETag/date conditions and unsupported X-Goog-* semantics fail closed; request Content-Type describes JSON and never becomes destination contentType |

The absent/null/empty destination defaults and legacy unknown/output-only handling
are local rules, with provider evidence still pending. Distinguish unknown source
fields from a documented cross-bucket selection mechanism: JSON sourceObjects has
no bucket field. The initial adapter rejects unsupported fields with 501 rather
than treating them as implemented semantics.

## Publication, permissions and response

Resolve at most 32 source metadata records and immutable blob handles under a
short catalog snapshot. No SQL transaction or blocking worker waits for streaming.
One admitted compose streams the pinned files in order with bounded 64 KiB chunks,
one source transfer active at a time, reusing the eight derived/upload permits.
CRC32C is combined from source CRC32Cs and lengths using the existing crc32c crate;
property tests compare it with direct calculation, including empty/binary inputs,
every split point, repeated sources and nested associativity. Never re-read all
source data merely to calculate the combined checksum.

Stage to a random opaque blob ID, sync and publish using the shared derived-object
foundation's filesystem/catalog recovery protocol. The final short transaction
checks the bucket incarnation, current effective grants, destination conditions
and active destination hold, then publishes one immutable destination generation
with metageneration 1. An old live destination becomes noncurrent when versioning
is enabled, or is journaled for physical deletion otherwise. Validation,
precondition, permission and hold failures before commit preserve the complete
previous destination and every source. Destination-as-source
uses its pinned earlier generation, allowing append without reading partial data.

Strict mode requires `storage.objects.get` for sources and
`storage.objects.create` for destination, plus `storage.objects.delete` when a
live destination would be replaced. Project and current bucket policy grants are
combined; revocation during transfer prevents publication. Anonymous public GET
permission alone does not authorize compose. No listing permission is required
for explicit source names. As a local safety rule, deleteSourceObjects=true also
requires storage.objects.delete even when the destination does not exist.
Retention/context permission checks are outside the
supported subset because those inputs return 501.

Success returns local 200 with an Object resource and no owner/acl fields, a fresh
generation, metageneration 1, injected-clock timestamps, exact concatenated size,
source storage class, accumulated componentCount and big-endian base64 CRC32C.
`md5Hash` is absent on composites, including one-source and empty composites.
The ETag is opaque, changes with generation/metadata, and is never presented as
the content's MD5. Metadata GET/list, selected-generation GET, whole/range media,
XML read headers, metadata updates, copy/rewrite and restart must preserve or
render composite identity consistently; XML compose itself remains out of scope.
Ordinary uploads retain MD5 and omit componentCount.

JSON and XML composite validators share the local quoted
`c-{generation}-{metageneration}` form. The wire tests verify equality and a
metadata-update change; the exact provider's opaque spelling is not claimed and
remains SPEC-COMPOSE-EDGES. Non-composite XML validators retain their earlier
declared MD5 form.

XML GET/HEAD additionally emit x-goog-component-count for composites only. JSON
metadata/list/fields expose componentCount; the XML-only header is not added to
JSON media responses. Custom metadata size is rejected by body validation before
source transfer. A changed destination bucket incarnation returns local 412;
cleanup errors are logged for deferred recovery without replacing the original
precondition/authorization error. With source deletion enabled, a pinned source
generation that disappears before publish returns 404 and never deletes a newer
generation. These status/precedence rules remain local under SPEC-COMPOSE-EDGES.

As in the frozen upload contract, a timeout or disconnected response after a
complete request does not prove that publication failed: the owned operation may
finish its atomic commit. Re-read the destination or retry with generation
conditions. Post-commit cleanup failure cannot undo a committed destination;
startup cleanup uses persisted transfer ownership and the deletion journal.

## Acceptance decomposition

| Case | Observable acceptance |
| --- | --- |
| COMPOSE-ORDER | One, two and 32 sources; binary/empty sources; repeated names and nested compositions produce the exact ordered bytes |
| COMPOSE-VALIDATION | Zero/33 sources, missing names, wrong JSON shapes, invalid generation values, malformed JSON and body cap fail without staging or destination mutation |
| COMPOSE-PATH | Encoded slash, percent, Unicode and compose-like suffix object names decode once; literal route suffixes remain unambiguous; invalid names and wrong method fail |
| COMPOSE-SOURCES | Missing bucket/live/selected source returns 404; source precondition mismatch and explicit generation mismatch return 412; all sources resolve before any destination publication |
| COMPOSE-PINNING | Replacing/deleting a source during transfer never substitutes a newer generation or combines mixed metadata/bytes |
| COMPOSE-METADATA | Explicit fixed/custom fields and event hold, omitted/default MIME and bucket default holds; unknown/advanced fields reject; failed requests preserve the entire previous resource |
| COMPOSE-CHECKSUMS | Combined CRC32C equals direct bytes; ignored JSON checksum inputs; absent MD5 on metadata/list/media/XML headers; opaque non-MD5 ETag |
| COMPOSE-COMPONENTS | Nested count accumulation and duplicates; greater than 1024 succeeds; saturation at int32 maximum without overflow; ordinary upload resets composite identity |
| COMPOSE-CLASS | Same-class sources succeed, mixed source classes reject; destination uses source class independently of a different bucket default |
| COMPOSE-CONDITIONS | Destination generation/metageneration matches and failures, create-if-absent, destination-as-source append and no live destination with retained versions |
| COMPOSE-RACE | Concurrent destination writes with the same match condition have exactly one winner; no partial destination, missing referenced blob or leaked admitted permit |
| COMPOSE-AUTH | Strict get+create allowed; get-only/create-only/missing credentials denied; overwrite additionally needs delete; current bucket grants/revocation enforced at publication |
| COMPOSE-VERSIONING | Retained destination history and selected source generations, active destination hold blocks overwrite, source hold permits read, default/explicit destination hold persists |
| COMPOSE-DELETE | Default/false preserves all sources and histories; true hard-deletes pinned live/noncurrent sources even with versioning; duplicates delete once; destination-as-source retains new composite; held source and all compose failures preserve everything |
| COMPOSE-FIELDS | Object field selection including componentCount; invalid selector does not mutate; errors are not filtered; list/read/update preserve composite metadata |
| COMPOSE-RECOVERY | Restart preserves composite metadata and bytes; abandoned staging and precommit orphan blobs are cleaned; postcommit catalog references remain valid |
| COMPOSE-RESOURCE | Object-byte cap checked with checked sums and enforced while streaming; bounded transfer/admission; disconnect, timeout and I/O errors do not publish partial data |
| COMPOSE-GO | Pinned storage v1.70.0 ObjectHandle.ComposerFrom with ordered sources, source generations/conditions, metadata, destination conditions and strict grants; no cloud/metadata-server access |

All cases require executable evidence before ledger promotion. Exhaustive provider
wire text, mixed-error precedence, every fsync/power-loss window, real-cloud
durability/billing and comparative memory/CPU performance are not claimed.

## Open source questions and explicit unsupported scope

`deleteSourceObjects=true` is documented for JSON and hard deletion bypasses
versioning/soft delete. Neither the checked JSON method nor the pinned proto
settles deletion authorization, held-source behavior, partial failure atomicity,
duplicate selected sources, concurrently removed sources, or destination-as-source
deletion. The method's permission list limits delete permission to destination
overwrite. The supported documented behavior is hard deletion after successful
composition and no source deletion on a failed composition. The following local
safety rules make the undocumented cases non-destructive and remain open source
questions, separately from that documented behavior scope:

- Strict mode additionally requires storage.objects.delete for source deletion.
- Any selected source under an event hold rejects the entire request with the
  existing objectUnderActiveHold error before any catalog mutation. Temporary
  holds and retention cannot be created in the current model and remain rejected
  as input rather than simulated as deletable states.
- Duplicate source entries contribute bytes/count in order but delete once.
- Sources with the destination name are excluded from hard deletion. Publish the
  new composite using normal destination overwrite rules: with versioning, its
  previous live generation becomes noncurrent; without versioning it is removed.
- Destination publication and deletion of all pinned source generations commit
  in one catalog transaction, with deferred blob reclaim. Recheck deletability
  under that write transaction; a failure rolls back the complete mutation.

No gRPC/XML implementation or billing simulation is implied. Record remaining
questions in compatibility/gaps.json during integration. Provider behavior for
concurrent source deletion or metadata changes during composition remains open;
the local transaction must never delete a different generation than the pinned one.

The checksum discrepancy between JSON guidance and the Go/RPC validation wording,
JSON MIME inheritance/defaults, unknown/output-only inputs and complete error
precedence likewise remain source questions. Keep the broad JSON method PARTIAL
for these questions and explicitly rejected advanced settings even after the
bounded core workflow passes. The obsolete 1024 limit is resolved by current
official quotas; it must not become a local compatibility limit.
