# JSON read ETag validators

<!-- markdownlint-configure-file {"MD013": false} -->

Contract and execution checked on **2026-10-23**.
This document records one supported subset; the
[feature ledger](../../../compatibility/features.json) remains the status authority.
MD013 is disabled so tables preserve complete test names and source digests.

## Scope

Implement `If-Match` and `If-None-Match` on JSON API v1 `objects.get` metadata,
whole media and single-range media reads, including the `/download/storage/v1`
alias, and `buckets.get`. Both native and legacy-emulator profiles use HTTP
entity-tag syntax. Strict local authentication and existing permissions remain
enforced. No dependency, schema, storage transaction or XML route changed.

The shared parser/comparison lives in `src/http_conditions.rs`; JSON maps its
outcomes to responses. It can be reused by a later XML slice without importing
JSON errors. `objects.list`, `buckets.list`, bucket IAM reads and mutations reject
these headers with 501 rather than accepting ignored conditions.

## Official sources

All sources below were checked on 2026-10-23. The Google references establish
read support; RFC 9110 supplies syntax and HTTP comparison rules.

| Source | Contract used |
| --- | --- |
| [Request preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions) | ETag conditions apply to retrieval; match failure is 412 and not-match failure is 304; an explicit generation selects a noncurrent version; date conditions are XML-only |
| [JSON headers](https://docs.cloud.google.com/storage/docs/json_api/v1/parameters#ifmatch) | Both ETag headers support GET for resources including buckets, objects and ACLs; writes are outside the documented support |
| [Object metadata](https://docs.cloud.google.com/storage/docs/metadata#etags) | ETags are opaque, change with data/metadata and can differ across APIs |
| [Requested hashes-etags URL](https://docs.cloud.google.com/storage/docs/hashes-etags) | Redirects to data-validation; it does not supply a separate ETag contract, so the metadata reference above is used |
| [objects.get](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/get) and [buckets.get](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/get) | Metadata/media selection, selected generations and existing query conditions |
| [objects.list](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/list) and [buckets.list](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/list) | Collection responses have no defined collection `etag`; this does not establish how Google evaluates collection validators |
| [RFC 9110 sections 13.1.1 and 13.1.2](https://www.rfc-editor.org/rfc/rfc9110.html#section-13.1.1) | Quoted entity-tag lists, wildcard, strong If-Match and weak If-None-Match comparison |
| [RFC 9110 section 13.1.3](https://www.rfc-editor.org/rfc/rfc9110.html#section-13.1.3) | Ignore If-Modified-Since when If-None-Match is present |
| [RFC 9110 section 13.1.4](https://www.rfc-editor.org/rfc/rfc9110.html#section-13.1.4) | Ignore If-Unmodified-Since when If-Match is present |
| [RFC 9110 section 13.2](https://www.rfc-editor.org/rfc/rfc9110.html#section-13.2) | Normal errors before conditions; If-Match before If-None-Match; conditions before Range |
| [RFC 9110 section 15.4.5](https://www.rfc-editor.org/rfc/rfc9110.html#section-15.4.5) | Bodyless 304 and applicable cache/validator response headers |
| [Discovery](https://storage.googleapis.com/$discovery/rest?version=v1) | Revision `20261013`; query condition definitions and Object/Bucket versus Objects/Buckets schemas; no HTTP-validator evaluation order |
| [Storage v2 protobuf](https://github.com/googleapis/googleapis/blob/14a161a98ef045be80c12ab69d924504a756f109/google/storage/v2/storage.proto) | Supplementary pinned schema; generation/metageneration request conditions do not define JSON HTTP ETag precedence |

Fetched source-byte SHA-256 values:

| Source | SHA-256 |
| --- | --- |
| Request preconditions HTML | `c10b7b82d9da56e9be4e925ba132e5ee999bc9aad1f0c017ba1e258729b01a3f` |
| JSON headers HTML | `e118c74126f1c0ba17c32de4902c0313fddc3a89675219f6018d7ca91e3da488` |
| Object metadata HTML | `f1cadc98079dc7b5244b474232644991814a921e75d61ebf6d7d626704deb51f` |
| Discovery revision `20261013` | `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0` |
| Pinned Storage v2 protobuf | `a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf` |

Moving documentation URLs are identified by their fetched bytes, not claimed
immutable. No real-cloud differential request was made.

## Supported subset and evaluation

`If-Match` succeeds when any strong quoted tag matches, or when the sole field
value is `*` and the requested resource exists. A weak tag never satisfies
If-Match. Failure returns the existing JSON 412 envelope with
`error.errors[0].reason=conditionNotMet`.

`If-None-Match` uses weak comparison. A matching strong/weak tag, or `*` on an
existing selected resource, returns 304. Otherwise GET proceeds normally.
Comma lists and repeated field instances form one logical list, preserving
quoted commas. RFC empty list elements are ignored; an empty list matches
nothing. Bare tags, lowercase `w/`, unclosed quotes, invalid opaque-tag bytes,
trailing garbage and wildcard/list mixtures return local 400 `invalidParameter`.
A repeated wildcard is invalid. Parsing does not stop at the first matching
member and thereby hide malformed later members.

For supported requests, the effective order is:

1. Existing credential, route/input (including fields syntax/schema) and permission checks.
2. Catalog authorization recheck and selection of the live or requested record;
   missing resources return 404 before evaluating even a malformed ETag value.
3. Existing media encoding capability checks; unsupported transcoding remains 501.
4. If-Match, then If-None-Match, then media Range handling.

Thus a stale If-Match beats a matching If-None-Match with 412. A matching
If-None-Match beats satisfiable, unsatisfiable and ignored Range values with
304, including a zero-byte object's otherwise unsatisfiable range. An authorized
missing object is never converted into a 304 or 412. Anonymous strict access
retains 401 `required`; insufficient local permission retains 403 `forbidden`.
These checks follow RFC normal-error ordering; exact Google authentication
messages and missing-bucket visibility retain the existing SPEC-017 boundary.
Malformed fields returns 400 `invalidParameter` before stale If-Match or matching
If-None-Match, following RFC 9110 section 13.2.1. This local ordering is tested for
both object and bucket metadata; exact provider error precedence remains
unverified. Valid fields changes only the final 200 JSON body, after comparison,
and never filters a 412 error envelope or adds content to a 304.

The four object generation/metageneration query preconditions, or either bucket
metageneration query precondition, combined with either HTTP ETag header return
501. Google permits multiple conditions but the checked sources do not order
HTTP validators relative to these query extensions when they imply different
responses. This slice does not invent that order. The unsupported combination
is rejected before catalog selection, as are other explicitly unsupported
headers. A `generation=` selector is not a precondition and works with both
ETag headers. Query conditions without ETag headers retain their existing rules.

### Representation identity, response headers and races

Object metadata `etag`, metadata GET's new `ETag` header, and whole/range media
`ETag` use the same selected generation/metageneration and existing opaque local
encoding. Both content overwrites and metadata changes invalidate the old tag.
This equality is the supported local JSON contract, not a claim about the exact
opaque bytes used by Google or XML. For native buckets the metadata field and
header are also equal.

The legacy bucket's metageneration-1 JSON field remains `RVRhZw==` to preserve its
source-profile representation. Its HTTP header and validator instead use the
native record-derived incarnation/revision tag in both profiles. Quoting the
legacy JSON field does not turn it into the HTTP validator; clients must use the
response header. A deleted and recreated bucket invalidates the original
validator: old If-None-Match returns 200 and old If-Match returns 412, including
when `fields=name` hides the JSON etag. Later legacy bucket revisions and native
buckets already render their record-derived tag in the JSON field.

304 carries ETag, Cache-Control and Hyper's Date, with no content or trailers,
Content-Type, Content-Length, Content-Encoding or Content-Range. Metadata uses
`no-cache`; media preserves the same private/public Cache-Control as its normal
200 response, including stored public cache directives after an allUsers grant.
Content-Location, Vary and Expires are absent on the corresponding normal
responses and are not invented for 304. Exact exhaustive Google header parity
remains SPEC-012.

Media uses `open_object_authorized` once. That call rechecks authorization,
selects one record and opens its immutable blob under the existing catalog
execution permit. The validator and range are checked against that returned
record; there is no metadata lookup followed by a separate object open.
After the response starts, metadata updates or overwrites cannot change the
record/header/body combination already selected. Metadata GET and bucket GET
similarly validate and serialize one returned record.

The deterministic race test pauses consumption after conditional 206 headers,
updates metadata, confirms a fresh request rejects the old tag, overwrites the
object, then verifies every remaining byte of the open 8 MiB response belongs
to the original generation and metageneration. Existing streaming chunks,
backpressure and request limits are unchanged; this slice adds no payload or
catalog buffering and makes no new RSS/throughput claim.

## Acceptance cases

All PASS entries below were executed locally. PENDING entries describe provider
questions or excluded implementation, not skipped tests counted as success.

| ID | Acceptance | Exact test IDs | Result |
| --- | --- | --- | --- |
| ETAG-READ-RESOURCES | Metadata, explicit alt=json, both media prefixes, exact header/resource identity, matching reads and nonmatching cache validators | `etag::json_etag_read_object_metadata_media_and_range`; `TestReadETagValidators` | PASS |
| ETAG-READ-COMPARISON | Strong If-Match, weak If-None-Match, wildcard, quoted comma, backslash, empty lists and repeated field instances | `http_conditions::tests::etag_comparison_uses_http_lists_and_strong_or_weak_matching`; `etag::json_etag_read_object_metadata_media_and_range`; `etag::json_etag_read_syntax_and_missing_resources` | PASS |
| ETAG-READ-SYNTAX | Malformed/unquoted tags, wildcard mixtures, repeated wildcard and invalid later members fail without false success | `http_conditions::tests::etag_comparison_rejects_invalid_tags_even_after_a_matching_member`; `etag::json_etag_read_syntax_and_missing_resources` | PASS |
| ETAG-READ-MATCH-FAILURE | Failed If-Match returns JSON 412 conditionNotMet on metadata and media | `etag::json_etag_read_object_metadata_media_and_range`; `TestReadETagValidators` | PASS |
| ETAG-READ-CACHE-RESPONSE | 304 ends at the headers, retains ETag/cache/Date, and omits content-specific headers | `etag::json_etag_read_object_metadata_media_and_range`; `etag::json_etag_read_selected_generation_zero_byte_and_public_cache`; `TestReadETagValidators` | PASS |
| ETAG-READ-ORDER | If-Match failure beats If-None-Match; both headers precede Range, including unsatisfiable ranges; successful match permits 206 | `etag::json_etag_read_object_metadata_media_and_range` | PASS |
| ETAG-READ-MISSING | Missing metadata/media objects and buckets return 404 before valid, wildcard or malformed ETag evaluation | `etag::json_etag_read_syntax_and_missing_resources` | PASS |
| ETAG-READ-BUCKETS | Native/legacy record-derived GET validators, legacy JSON/header divergence, metadata invalidation and delete/recreate invalidation with fields=name | `etag::json_etag_read_buckets_and_metadata_changes_in_both_profiles`; `TestReadETagValidators` | PASS |
| ETAG-READ-GENERATIONS | Selected noncurrent generation validates against its own tag and returns its own bytes | `etag::json_etag_read_selected_generation_zero_byte_and_public_cache`; `TestReadETagValidators` | PASS |
| ETAG-READ-EMPTY-PUBLIC | Empty media, empty unsatisfiable range overridden by 304, and stored public Cache-Control on 304 | `etag::json_etag_read_selected_generation_zero_byte_and_public_cache` | PASS |
| ETAG-READ-PERMISSIONS | Anonymous and creator-only strict requests fail before successful, failed or malformed ETag evaluation | `etag::json_etag_read_authorization_precedes_validators` | PASS |
| ETAG-READ-SNAPSHOT | Conditional open response retains original ETag, generation, metageneration and bytes through metadata update and overwrite | `etag::json_etag_read_metadata_update_and_overwrite_pin_open_reader` | PASS |
| ETAG-READ-REJECTIONS | ETag headers on query-conditioned reads, lists, IAM GET and writes return 501; standalone dates/If-Range fail on supported GETs; rejected writes preserve data | `etag::json_etag_read_unsupported_combinations_lists_dates_and_writes`; `obj_http_bad_hashes_and_unsupported_requests_never_publish`; `json::object_tests::obj_headers_003_range_is_accepted_and_unsupported_conditionals_are_rejected` | PASS |
| ETAG-READ-FIELDS | Bucket/object fields=name retains the complete-resource HTTP ETag; matching cache validator returns bodyless 304 and failed If-Match returns an unfiltered 412 error | `fields_http::fields_read_etag_validators_use_the_complete_resource` | PASS |
| ETAG-READ-FIELDS-ORDER | Malformed fields returns 400 before failing If-Match or matching If-None-Match; parameter validation precedes condition evaluation | `fields_http::fields_invalid_selection_precedes_read_etag_evaluation` | PASS |
| ETAG-READ-DATE-PRECEDENCE | If-None-Match overrides If-Modified-Since and If-Match overrides If-Unmodified-Since on supported reads; stray date/If-Range headers retain existing ignored behavior on writes and lists | `etag::json_etag_read_date_precedence_and_unrelated_routes_preserve_behavior`; `etag::json_etag_read_if_match_overrides_if_unmodified_since` | PASS |
| ETAG-READ-QUERY-ORDER | Establish HTTP versus generation/metageneration condition precedence before enabling mixed requests | None; explicit rejection is tested above | PENDING |
| ETAG-READ-COLLECTION-CONTRACT | Establish collection ETags and list validator behavior rather than deriving a tag from page items | None; explicit rejection is tested above | PENDING |
| ETAG-READ-PROVIDER-WIRE | Independently establish opaque Google tag bytes, malformed-input errors and exhaustive response headers | None; local wire behavior above is tested | PENDING |

## Explicit exclusions and gaps

- All write-side ETag conditions remain 501; GET-only Google header support does
  not authorize silently implementing an unsafe mutation condition.
- Collection validators remain 501 pending a collection contract. Discovery
  lacks collection etag fields; generated SDK list-call IfNoneMatch helpers
  alone do not establish actual Google behavior.
- Bucket IAM/ACL resource validators, XML, gRPC and HEAD are outside this slice.
  Existing JSON HEAD routing still rejects the method. XML validators retain
  their existing rejection and no XML route calls the new shared evaluator.
- If-Range remains excluded under SPEC-RANGE-VALIDATORS and returns 501 on
  supported GETs, including requests without Range. JSON date validators return
  501 on those reads, except If-Modified-Since is ignored when If-None-Match is
  present and If-Unmodified-Since is ignored when If-Match is present, as RFC 9110 requires.
  Non-GET and list routes preserve their existing ignored date/If-Range behavior.
- Two bucket incarnations created in the same millisecond with equal revisions
  can share the record-derived HTTP tag; this slice does not add an incarnation allocator.
- Decompressive transcoding remains unsupported before ETag evaluation. No new
  compressed/expanded representation equivalence is claimed.
- Field selection applies only after read ETag evaluation to the final 200
  metadata body. Combined fields/validator regressions are verified above.
- No container, complete SDK suite, guide journey, release or cloud-provider
  execution is claimed here. Those final integration gates run separately.

## Ledger reconciliation

These executed cases supply durable evidence for ledger reconciliation.
Preserve broader PENDING cases and existing evidence.

| Feature row | Acceptance to add or revise | Status boundary |
| --- | --- | --- |
| BEHAVIOR-COMMON-012 | Add ETAG-READ-RESOURCES through ETAG-READ-DATE-PRECEDENCE and their exact tests; retain the three PENDING cases above plus XML/date/IAM/HEAD breadth | Advance NOT_STARTED to PARTIAL; do not claim all HTTP conditions implemented |
| BEHAVIOR-OBJECT-006 | Add resource identity, strong/weak comparison, invalidation, selected-generation, permission and snapshot cases; revise the earlier MEDIA wording that only rejected headers | Keep PARTIAL for opaque production/composite/cross-API semantics and mixed/list cases |
| JSON-OBJECTS-005 | Add ETAG-READ-RESOURCES, COMPARISON, SYNTAX, MATCH-FAILURE, CACHE-RESPONSE, ORDER, MISSING, GENERATIONS, EMPTY-PUBLIC, PERMISSIONS, SNAPSHOT, REJECTIONS, FIELDS, FIELDS-ORDER and DATE-PRECEDENCE | Keep PARTIAL; replace blanket ETag-unsupported text with this scoped support and named exclusions |
| JSON-BUCKETS-002 | Add ETAG-READ-BUCKETS, COMPARISON, SYNTAX, CACHE-RESPONSE, MISSING, PERMISSIONS, REJECTIONS, FIELDS, FIELDS-ORDER and DATE-PRECEDENCE | Keep PARTIAL; distinguish the legacy metadata field from its record-derived HTTP validator |
| EXT-XML-* | No acceptance or fidelity upgrade from this JSON implementation; existing XML regressions pass in the workspace run | Untouched |

The complete new Rust test set is the two `http_conditions::tests::*` IDs and
eight `etag::json_etag_read_*` IDs and two `fields_http::*` IDs spelled out in the acceptance table. Existing
`obj_http_bad_hashes_and_unsupported_requests_never_publish` now asserts GET 400
for an unquoted tag and GET 304 for wildcard If-None-Match while preserving its
upload/delete/encryption/custom-header negative cases. The JSON header unit
test preserves upload 501 assertions. The Go SDK evidence is exclusively
`TestReadETagValidators`: official generated Objects.Get IfNoneMatch and
googleapi.IsNotModified, header-set If-Match with a selected-generation ranged
Download, stale match rejection, metadata invalidation and Buckets.Get
IfNoneMatch. SDK pins: Go 1.27.3, Storage v1.70.0, API client v0.308.0.

### Commands and results

All results below are from the final implementation and tests on 2026-10-23.
No test was skipped or ignored. HTTP tests used local loopback servers.

| Command | Result |
| --- | --- |
| `cargo +1.99.1 xtask dependencies check` | PASS; all 31 actual ecosystem/toolchain/container/Action inputs CURRENT, no dependency changes |
| `cargo +1.99.1 test --workspace --locked --offline` | PASS; 436 passed, 0 failed, 0 ignored across the workspace, including all 8 ETag HTTP tests, both comparison unit tests and all fields interaction cases |
| `cargo +1.99.1 fmt --all --check` | PASS |
| `cargo +1.99.1 clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS |
| `GCS_MIMIC_GO_TEST_RUN='TestPartialResponseFields\|TestReadETagValidators' GOPROXY=off GOFLAGS=-mod=readonly bash scripts/sdk-go.sh` | PASS; harness go vet plus the 2 named SDK tests; no new Go dependencies |
| `git diff --check` | PASS |
| `npx --yes markdownlint-cli2 docs/en/slices/json-read-etag-validators.md` | PASS; no issues, only MD013 disabled for complete table identifiers/digests |
| Tracked-file local-path scan | PASS; no matches before implementation commit |

### Gap reconciliation proposal

| Gap | Exact update needed |
| --- | --- |
| SPEC-012 | Retain opaque ETag and exhaustive header parity questions; add this document as local evidence for same-selected-record JSON field/header comparison, 304 ETag/cache headers and Go coverage. Remove blanket If-Match/If-None-Match 501 statements from affected feature deviations, replacing them with the supported GET subset and mixed/list/IAM/write/date exclusions. Add BEHAVIOR-OBJECT-006 and BEHAVIOR-COMMON-012 to affected rows where appropriate. |
| SPEC-008 | Add HTTP ETag plus generation/metageneration query precedence as a named unresolved subcase, rejected 501 before catalog selection. Keep existing standalone query-condition behavior and gaps unchanged. Add the bucket counterpart where appropriate. |
| SPEC-JSON-ETAG-COLLECTIONS (new, SOURCE_QUESTION) | Common JSON headers say GET resources support validators, but Objects/Buckets collection schemas have no ETag and the checked sources do not define collection validation. Both lists reject either header with 501, including wildcard. Establish the supported collection contract before enabling it. Affect JSON-OBJECTS-008, JSON-BUCKETS-006 and BEHAVIOR-COMMON-012. |
| SPEC-JSON-ETAG-WIRE (new, LOCAL_DEVIATION) | RFC syntax/comparison is locally verified; exact Google malformed/unquoted/repeated-header error envelopes, empty-list handling, exhaustive 304 headers, fields-error precedence and legacy JSON-versus-HTTP validator divergence lack service observations. Record local 400 invalidParameter, bodyless 304, record-derived HTTP validators and authorization/existence/input order without claiming production wire equality. Affect BEHAVIOR-COMMON-012, BEHAVIOR-OBJECT-006, JSON-OBJECTS-005 and JSON-BUCKETS-002. |
| SPEC-RANGE-VALIDATORS | Keep OPEN and If-Range 501; this implementation does not resolve its Google contract question. |
| SPEC-017 | Preserve existing authentication/error visibility boundaries; attach the local validator-before-disclosure permission regression without claiming independently verified service messages. |

No upstream version or source question is resolved merely by the local passing
SDK test. Fields selection and read validators are combined and locally verified.
Ledger/gap reconciliation and final container, complete SDK and guide checks
remain separate verification gates.
