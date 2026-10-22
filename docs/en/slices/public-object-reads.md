# Public and private path-style object reads

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

Contract checked: **2026-10-22**. Scope: DESIGN-004, DESIGN-012, DESIGN-013,
DESIGN-047, EXT-XML-012, EXT-XML-017 and UX-005. This document defines acceptance
conditions; implementation status and executed evidence belong exclusively to
`compatibility/features.json`.

## Scope and exclusions

The `public-object-reads` deliverable adds path-style XML GET/HEAD for existing
live objects, anonymous bucket IAM `allUsers` grants and registered-local-identity
`allAuthenticatedUsers` grants. JSON object reads use the same authorization
decision. URL construction stays independent of permissions. Reads select
metadata, conditions, public classification and immutable bytes consistently.

Virtual-host routing, XML write/list breadth, full ACL/UBLA/PAP state machines
and signature authentication are deferred, as specified by `delivery_plan`.
Also excluded are version history, soft-deleted objects, decompressive
transcoding, requester billing, website responses, CORS, real OAuth verification,
Google policy propagation and a real cache/CDN simulation. A permissive read
does not prove anonymous IAM access.

The API rows EXT-XML-012 and EXT-XML-017 remain PARTIAL when only this subset is
verified: both official operations have inputs outside it. Completion of the
deliverable does not establish complete XML API fidelity or close SPEC-017.

## Sources

All sources below were fetched and read on **2026-10-22**. Storage reference pages
displayed a last-update date of 2026-10-21. Exact fetched-content SHA-256 values
and tagged source URLs are recorded in [Sources](../sources.md#public-object-read-source-pins).

| Source | Contract used |
| --- | --- |
| [XML GET Object](https://docs.cloud.google.com/storage/docs/xml-api/get-object-download) | Generation selection, ranges, match headers, download metadata and unsupported HTTP conditions |
| [XML HEAD Object](https://docs.cloud.google.com/storage/docs/xml-api/head-object) | Metadata-only read, Range distinction, empty success and error bodies |
| [XML headers](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers) | Header names, hashes, metadata, generations and cache directives |
| [XML status codes](https://docs.cloud.google.com/storage/docs/xml-api/reference-status) | XML Error/Code/Message, authentication, denial, existence and condition errors |
| [Request endpoints](https://docs.cloud.google.com/storage/docs/request-endpoints) | Path-style versus virtual-host URLs and encoded object names |
| [Storage IAM](https://docs.cloud.google.com/storage/docs/access-control/iam) | Inherited project and bucket permissions are additive |
| [IAM principals](https://docs.cloud.google.com/iam/docs/principals-overview) | `allUsers` includes anonymous callers; `allAuthenticatedUsers` excludes them |
| [Make data public](https://docs.cloud.google.com/storage/docs/access-control/making-data-public) | Bucket `allUsers` object-reader bindings; Object Viewer also grants listing |
| [Public access prevention](https://docs.cloud.google.com/storage/docs/public-access-prevention) | PAP overrides both public principal sets; signed URLs are a separate authorization mechanism |
| [Object metadata](https://docs.cloud.google.com/storage/docs/metadata#cache-control) | Stored cache directives, public caching/revocation consequences and XML ETag qualifications |
| [JSON headers](https://docs.cloud.google.com/storage/docs/json_api/v1/parameters#cachecontrol) | Public classification requires `allUsers`; private JSON reads always use `no-cache` |
| [Go HTTP reader, storage/v1.70.0](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/http_client.go) and [reader helpers](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/reader.go) | Exact SDK request construction and response parsing |
| [fake-gcs-server routes, v1.56.1](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/server.go) and [object handler](https://github.com/fsouza/fake-gcs-server/blob/v1.56.1/fakestorage/object.go) | Source-only comparison; never authority for Google behavior |

The slice-start [freshness evidence](../evidence/public-reads-dependencies.txt)
records one successful `just check-dependencies` run. Independent official Rust
stable-channel, crates.io sparse-index, Go download/module-proxy and Docker
registry checks also matched the pins. No dependency was upgraded by this
contract work.

## Request decomposition

### Path and decoding

The XML object address is `/<bucket>/<object>`. Split the raw bucket segment
from the entire remaining raw object path before percent decoding; decode each
exactly once. Literal and encoded slashes identify the same object bytes after
decoding. Preserve repeated slashes, leading/trailing object slashes, Unicode,
spaces, `+`, encoded `?`/`#`, literal percent signs and dot segments. Never use
an object name as a filesystem path. Malformed escapes, invalid UTF-8 and names
outside the object validator return an XML 400. An invalid raw bucket segment
does not select the XML adapter: it retains the JSON-style 404 unknown-route
response. This exception and encoded bucket segments remain a routing parity
question; see SPEC-XML-PATH-ERROR.

Only `/storage/v1`, `/upload/storage/v1`, `/download/storage/v1`,
`/batch/storage/v1` and `/_internal`, including each slash-delimited descendant,
take precedence over XML fallback. Other objects in buckets named `storage`,
`upload`, `download` or `batch` remain reachable. An unrecognized path inside
a reserved namespace must not become an
XML success. This creates a local addressing boundary for names colliding with
API prefixes; SPEC-XML-PREFIX tracks the exact cloud-routing question.
Virtual-host addresses are not interpreted as bucket names from `Host`.

The existing `url public` helper must round-trip its encoded output through this
route, including `%252F`, encoded dot segments and empty slash segments. It
rejects bucket/object combinations whose final path collides with a reserved
namespace rather than printing an unusable public URL. A base
URL path prefix requires a proxy that strips that prefix; the helper does not
configure server routing or trust forwarded headers.

### Query and header subset

| Input | Supported behavior or explicit boundary |
| --- | --- |
| `generation` | Optional positive decimal signed-64-bit selector; only the selected live generation exists locally. A different or removed generation yields authorized `NoSuchKey`, never current bytes |
| `Range` | One closed, open-ended or suffix byte range; inclusive offsets refer to stored bytes. GET returns 206 when satisfiable and 416 when a valid range cannot be satisfied |
| `x-goog-if-generation-match` | One nonnegative decimal signed-64-bit value; compare with the selected generation, including a failing zero against an existing object |
| `x-goog-if-metageneration-match` | One positive decimal signed-64-bit value; compare with the selected metadata revision; generation matching/selection is recommended when metadata consistency matters |
| `Authorization` | Strict local Bearer resolution below; no Google credential validation |
| `Accept-Encoding` | Existing stored-byte negotiation: gzip may be served when accepted or protected by stored `no-transform`; required decompression returns 501 |
| `Host` | Required HTTP authority; never changes permissions, advertised URLs or object names |
| `Cache-Control` request header | Does not override stored metadata or the response default |
| `Date`, `User-Agent`, `x-goog-api-client`, tracing headers | Informational; do not alter authorization or selected state |
| `x-goog-gcs-idempotency-token` | A single nonempty read retry token is accepted without response replay; empty or repeated values return 400 |
| `If-Match`, `If-None-Match`, `If-Modified-Since`, `If-Unmodified-Since`, `If-Range` | Recognized but outside this slice; 501, never unconditional success |
| CSEK/CMEK `x-goog-encryption-*`, `x-goog-user-project`, `userProject` | Recognized encryption/billing semantics are unsupported; 501 even for an empty value |
| `x-goog-allowed-resources` | Resource restrictions are unsupported; 501 rather than an unchecked restriction |
| `response-*` query overrides | Unsupported; 501 rather than ignored response metadata |
| XML subresources, including `acl`, `encryption`, `retention`, multipart/list selectors | 501 `NotImplemented`; no request-body consumption |
| Signed/authentication query parameters or signature Authorization schemes | Strict profile rejects with 400 `InvalidAuthentication`; no fallback to anonymous public grants |

Duplicate generation or match values, malformed numbers and malformed query
encoding return 400. Unknown XML query keys and unsupported `x-goog-*` headers
are locally rejected with 501; upstream unknown-input parity is not established
(SPEC-XML-INPUTS). Unrelated extension headers are
not rejected merely because their names are unknown. Recognized unsupported
semantics must fail explicitly.

Invalid/non-byte/multiple/repeated ranges inherit the existing single-range
fallback rules in [the range contract](json-object-range.md); those local rules
do not establish exact Google XML fallback parity. Authorization precedes
representation validation, conditions and existence disclosure. An authorized
missing object is resolved before testing its generation/match conditions.

### Official inputs outside each API row

| API row | Official or recognized inputs not implemented in this slice |
| --- | --- |
| EXT-XML-012 GET Object | HTTP ETag/date validators and If-Range; CSEK and encryption-key headers; requester-pays `x-goog-user-project`/`userProject`; signed URLs and signing headers/query; response metadata overrides; virtual-host endpoints; noncurrent versions; decompressive transcoding; resource restrictions such as `x-goog-allowed-resources` |
| EXT-XML-017 HEAD Object | HTTP ETag/date validators; CSEK-associated metadata/encryption semantics; requester-pays `x-goog-user-project`/`userProject`; signed URLs and signing headers/query; response overrides; virtual-host endpoints; noncurrent versions; resource restrictions such as `x-goog-allowed-resources` |

The GET/HEAD method pages directly list generation, match/date conditions, Range
and billing/encryption qualifications. The common references supply the broader
recognized input families; their presence here does not mean every input is
applicable to both operations in every Google configuration.

## Response decomposition

GET returns streamed object bytes with 200, or the selected byte interval with
206. Empty objects return 200 with length zero. HEAD never sends a body, including
on errors. For a valid HEAD Range, the method-specific reference says the range
only adds `Content-Range`: preserve 200 and the full `Content-Length`. Treat this
method-specific rule separately from GET; no partial body is sent. Exact HEAD
unsatisfiable-range parity remains unverified: the local response is 200 with
full `Content-Length`, `Content-Range: bytes */size` and no body. Malformed-range
parity remains open under SPEC-RANGE-FALLBACK.

| Header | Required local representation |
| --- | --- |
| `Content-Length` | Full size for 200 and HEAD; selected byte count for GET 206 |
| `Content-Type` | Stored type, otherwise `application/octet-stream` |
| `Content-Encoding`, `Content-Disposition`, `Content-Language` | Stored fixed metadata when present; no invented encoding |
| `Content-Range` | `bytes start-end/size` for GET 206 or a satisfiable HEAD range; `bytes */size` for a GET 416 or locally unsatisfiable HEAD 200 |
| `Accept-Ranges` | `bytes` on successful object responses |
| `x-goog-generation`, `x-goog-metageneration` | Decimal values from the same selected record as the bytes |
| `x-goog-hash` | Whole-object CRC32C and MD5 in base64, including on partial reads; these are not checksums of the selected interval |
| `x-goog-stored-content-length`, `x-goog-stored-content-encoding` | Full stored size and stored encoding, using `identity` when absent |
| `Last-Modified` | Selected record's update time as an HTTP date, including valid GMT formatting |
| `ETag` | Local quoted lowercase hexadecimal MD5; exact Google encoding and advanced-object qualifications remain SPEC-012 |
| `Cache-Control` | Protocol/public-state rules below |
| `x-goog-meta-*`, `x-goog-storage-class` | Selected custom metadata that can be represented safely in HTTP, and stored storage class |

JSON custom metadata need not be a legal HTTP field name/value. The XML adapter
must not inject headers, panic or silently claim a complete metadata round trip
for an unrepresentable record. Such metadata remains outside this XML subset;
reject the read with 501 rather than changing stored JSON metadata.
Case-colliding custom metadata also returns 501. Exact non-ASCII XML wire
encoding requires separate evidence (SPEC-012).

| Condition | XML status/code |
| --- | --- |
| Invalid object path/query/numeric value | 400 `InvalidArgument` |
| Repeated match header | 400 `ExcessHeaderValues` |
| Nonempty or transfer-encoded read body | 400 `UnexpectedContent` |
| Anonymous caller lacks a usable public grant | Local 401 `AuthenticationRequired`; exact anonymous 401/403 choice remains SPEC-017 |
| Invalid/unregistered/unsupported credentials in strict mode | 400 `InvalidAuthentication`; never anonymous fallback |
| Registered identity lacks `storage.objects.get` | 403 `AccessDenied` |
| Authorized missing bucket | 404 `NoSuchBucket` |
| Authorized missing object or selected generation | 404 `NoSuchKey` |
| Failed generation/metageneration match | 412 `PreconditionFailed` |
| Valid unsatisfiable GET range | 416 `InvalidRange` |
| Recognized unsupported XML operation or semantics | Local 501 `NotImplemented` |

Error GET responses use `Content-Type: application/xml` and an XML `Error`
containing `Code` and `Message`. Escape dynamic values. Do not return JSON error
documents on selected XML routes; invalid bucket segments retain the unknown
route exception above. HEAD preserves status and headers while suppressing
all error bytes. Exact local-token messages, request identifiers, optional XML
elements, error header inventory and 501 spelling are local choices; the XML
status reference is not an exhaustive wire template.

## Permission model

Strict mode resolves credentials before consulting a public grant. A missing
Authorization header represents an anonymous caller. A registered local bearer
resolves one identity. An unregistered bearer, malformed/duplicate header,
credential query or unsupported scheme returns XML 400 `InvalidAuthentication`,
including on an `allUsers`
object. These rules deliberately do not treat arbitrary strings as Google users.

| Caller | `allUsers` binding | `allAuthenticatedUsers` binding | Named identity/project grant |
| --- | --- | --- | --- |
| Anonymous | Matches | Does not match | Does not match |
| Registered local identity | Matches | Matches | Exact member/project membership applies |
| Invalid or unregistered credentials | Authentication fails first | Authentication fails first | Authentication fails first |

Bindings contribute permissions through the existing predefined-role snapshot,
not through a special-case role name. Public access means that `allUsers` grants
`storage.objects.get`; an `allUsers` role lacking that permission does not make
objects public. `allAuthenticatedUsers` alone is never anonymous public access.
Project grants and current owning-bucket bindings form a union; removing one
grant does not revoke another independent grant.

JSON anonymous operations now evaluate the exact granted permissions, including
object mutations and bucket/policy operations; a read-only public grant does
not enable writes or policy management. A denied
anonymous JSON request retains the existing 401 `required` body; invalid
credentials retain `authError`, and registered permission denials retain 403
`forbidden`. JSON and XML use different error adapters over the same permissions.

Only a caller with `storage.objects.get` may learn `NoSuchBucket`/`NoSuchKey`.
For a missing bucket, the existing local rule permits existence disclosure only
when a registered identity has that permission in at least one project; a
bucket-only grant cannot authorize a bucket that no longer exists. A newly
created/recreated bucket is checked against its actual owner and current policy.

Permission checks and public classification run against current policy in the
transaction that selects object metadata/bytes. Subsequent requests after a
successful revocation fail; a read already opened against an immutable generation
may finish. Grants persist through restart. Deleting/recreating a bucket removes
its old policy. No cache or Google propagation delay is simulated.

Linux/Android listeners set `SO_REUSEADDR` before binding for same-port restart.
Other Unix platforms retry bind for a bounded interval while TIME_WAIT drains,
and fail promptly when a live listener is detected. This does not permit
concurrent listeners or change persisted grants; Windows socket parity is not
established by Unix checks.

Permissive mode preserves the existing authentication bypass, including ignored
credential inputs, while keeping input validation and stored IAM policies.
This behavior is not evidence of a public grant. Unsupported XML operations,
encryption, billing and conditions still fail rather than receiving success.

## Interactions and local choices

### Cache-Control

Classify public state by effective `allUsers` `storage.objects.get`, independently
of whether this request supplied a valid identity or uses permissive mode.

| Response | Stored Cache-Control | Result |
| --- | --- | --- |
| JSON/XML, publicly readable | Present | Stored value |
| JSON/XML, publicly readable | Absent | `public, max-age=3600` |
| JSON, private or `allAuthenticatedUsers` only | Any | `no-cache`, per JSON-specific reference |
| XML, private or `allAuthenticatedUsers` only | Present | Stored value, per XML-specific reference |
| XML, private or `allAuthenticatedUsers` only | Absent | Local `private, max-age=0` fallback, consistent with the HEAD example |

The general metadata guide also conditions defaults on CMEK and VPC service
perimeters; neither is modeled. The private XML fallback is a declared local
choice, not proof of an exhaustive Google default. Request `Cache-Control`
does not change these results. GET 206 and HEAD use the same selected record's
cache classification as a whole GET. Revocation changes future emulator checks,
but cannot erase a response already stored in an external browser/proxy cache.

### Restrictions and unsupported operations

PAP, UBLA, ACLs and default ACLs are not modeled. Recognized bucket configuration
or ACL inputs that would imply these semantics must be rejected explicitly,
without installing a policy or exposing data. Reject PAP `enforced` and
`inherited` rather than accepting a no-op restriction; reject UBLA settings and
unsupported ACL/predefined ACL inputs. Do not label this a PAP/UBLA simulation.
Google PAP overrides public grants and does not invalidate otherwise authorized
signed access; the deferred signed-URL implementation must preserve that rule.

XML bucket listing, PUT, POST, DELETE and subresources return 501 without reading
request bodies. An authorization denial may win first in strict mode. These
failures must not create staging files, objects, policies or metadata changes.
Pure URL generation never reads or updates any permission. Header/condition
failures, denied reads and HEAD are also side-effect free.

The namespace precedence, signed-64-bit numeric bounds, unknown-query policy,
range fallbacks, anonymous XML denial selection, private XML cache fallback and
unrepresentable metadata rejection are explicit local rules. SPEC-017,
SPEC-012, SPEC-RANGE-FALLBACK and SPEC-XML-PREFIX retain their unresolved parity
questions. No real-cloud request is part of this contract research.

## Pinned Go SDK and comparison behavior

In `storage/v1.70.0`, default HTTP reads use XML unless JSON reads are explicitly
selected. The reader builds a path-style URL with `url.PathEscape(object)` and
uses HEAD for length zero. `generation` pins explicit or resumed reads. Range
requests cover closed, open and suffix forms. Options can add billing, CSEK,
gzip and generation/metageneration match headers; billing/CSEK stay unsupported.

The reader validates 206 `Content-Range`, pins `x-goog-generation` for reopen,
parses metageneration and HTTP `Last-Modified`, and exposes content type/encoding,
Cache-Control and custom metadata. CRC32C from `x-goog-hash` validates complete
untranscoded reads; MD5 is ignored. Stored encoding informs decompression logic.
`x-goog-stored-content-length` is part of the wire contract, but this reader does
not require or parse it. Missing optional metadata can silently yield zero/empty
SDK attributes, so a successful byte read alone is insufficient verification.

fake-gcs-server `v1.56.1` registers path-style and host-based GET/HEAD plus XML
listing and selected writes. Its download handler supports generation/range,
hash/metadata headers and optional gzip decompression, but does not evaluate
caller IAM grants. Source registration or accepted signed-looking requests do
not establish signature validation. This comparison is a source review; its
tests and cross-product performance were not executed.

## Acceptance cases

Case IDs identify behavior, not execution claims. Wire tests, catalog race tests,
the pinned default/XML Go harness and the host/peer guide must supply final
evidence. Do not infer passing cases from this table.

| Feature | Case ID | Required observation |
| --- | --- | --- |
| DESIGN-004 | DESIGN-004-CALLERS | Missing credentials can use `allUsers`; valid registry tokens resolve identities; invalid/duplicate/unregistered credentials fail XML 400 `InvalidAuthentication` or JSON 401 `authError` even on a public object |
| DESIGN-004 | DESIGN-004-ERRORS | Private anonymous JSON preserves exact 401 `required`; XML uses its own XML denial; HEAD errors contain no body |
| DESIGN-004 | DESIGN-004-EXISTENCE | Authorized missing bucket/object gets distinct 404 codes; unauthorized existing/missing resources receive the denial without existence disclosure |
| DESIGN-012 | DESIGN-012-ALLUSERS | A bucket binding granting `storage.objects.get` permits anonymous XML GET/HEAD and JSON metadata/media reads; unrelated permissions remain denied |
| DESIGN-012 | DESIGN-012-REVOKE | Remove the public grant, deny all subsequent anonymous reads, preserve independent project/bucket grants; restart preserves the final policy |
| DESIGN-012 | DESIGN-012-CACHE | Public default and stored overrides work for full/range/HEAD; private JSON always uses no-cache; private XML stored directives remain effective |
| DESIGN-013 | DESIGN-013-AUTHENTICATED | `allAuthenticatedUsers` accepts a registered identity with no project grant and denies anonymous and unregistered callers |
| DESIGN-013 | DESIGN-013-UNION | Named, public and project grants are additive; another bucket and a recreated bucket do not inherit revoked/deleted grants |
| DESIGN-047 | DESIGN-047-ENCODING | Raw/encoded slashes, `%252F`, Unicode, plus, spaces, escaped question/hash and dot/empty segments select the exact intended object |
| DESIGN-047 | DESIGN-047-PRECEDENCE | Existing JSON/upload/download/admin routes keep their adapter and negative behavior; reserved paths do not fall through to XML success |
| DESIGN-047 | DESIGN-047-URL | Helper URL retrieves the exact object and changes no IAM state; private objects remain denied; advertised host never grants permission |
| EXT-XML-012 | EXT-XML-012-WHOLE | GET returns exact bytes, zero-size objects, content/custom metadata and required whole-object integrity/generation headers |
| EXT-XML-012 | EXT-XML-012-RANGE | Closed/open/suffix/boundary ranges return correct 206 interval, full hashes, stored length and Content-Range; valid unsatisfiable ranges return XML 416 |
| EXT-XML-012 | EXT-XML-012-GENERATION | Explicit live generation succeeds, old/missing generation never falls back, overwrite races keep bytes and headers on one selected generation |
| EXT-XML-012 | EXT-XML-012-CONDITIONS | Each match succeeds/fails correctly, both conditions compose, failed conditions return XML 412 and change no state |
| EXT-XML-012 | EXT-XML-012-REJECTION | Encryption/billing/overrides/HTTP validators/unsupported subresources fail explicitly; malformed numbers/escapes/duplicates fail 400; signatures never grant strict access |
| EXT-XML-012 | EXT-XML-012-STREAM | Large/slow downloads stay bounded and cancellation releases work; no entire payload/catalog collection; rejection does not consume request bodies |
| EXT-XML-017 | EXT-XML-017-HEAD | Successful HEAD returns selected full size/metadata and no bytes; valid Range adds Content-Range without converting to a partial response |
| EXT-XML-017 | EXT-XML-017-ERRORS | Denial/missing/condition/invalid/unsupported HEAD cases have their XML status and no response body; metadata is not disclosed before permission |
| EXT-XML-017 | EXT-XML-017-REJECTION | HTTP validators, encryption, billing, response overrides, signatures and unsupported version/addressing modes remain outside the verified API subset |
| UX-005 | UX-005-GUIDE | Host and peer clients upload private data, observe denial, grant/read/revoke both public principal sets, restart and distinguish URLs from permissions |
| UX-005 | UX-005-GO | Pinned default/XML NewReader, NewRangeReader and length-zero HEAD verify attributes, conditions, generations, checksums and strict grant/revocation outcomes |
| UX-005 | UX-005-RESTRICTIONS | PAP/UBLA/ACL settings and XML writes/list/subresources fail closed without body consumption or state mutation; permissive success is not claimed as strict IAM proof |
